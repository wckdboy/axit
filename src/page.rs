//! Bounded, bidirectional pages over one immutable metadata generation.
use crate::{
    index::{Entry, Index},
    query::Sort,
};
use anyhow::{Result, ensure};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    cmp::{Ordering, Reverse},
    collections::{BinaryHeap, HashSet},
    sync::atomic::{AtomicU64, Ordering as AtomicOrdering},
    time::Instant,
};

pub const MAX_SIZE: usize = 1000;
const TOKEN_BYTES: usize = 89;
#[derive(Clone, Debug, Default, Serialize)]
pub struct Navigation {
    pub offset: u64,
    pub next_cursor: Option<String>,
    pub previous_cursor: Option<String>,
    pub first_cursor: Option<String>,
    pub last_cursor: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Page {
    pub ids: Vec<u32>,
    pub matched: u64,
    pub total_bytes: u64,
    pub elapsed_ms: f64,
    pub canceled: bool,
    #[serde(flatten)]
    pub paging: Navigation,
    #[serde(skip)]
    pub largest: Vec<u32>,
    #[serde(skip)]
    pub cursor: Option<String>,
}
#[derive(Clone, Copy)]
pub struct Request<'a> {
    pub limit: usize,
    pub cursor: Option<&'a str>,
}
#[derive(Clone, Copy)]
pub struct Spec {
    pub sort: Sort,
    pub binding: [u8; 32],
    pub summarize: bool,
}

pub fn binding(namespace: &[u8], sort: Sort, hidden: &HashSet<u32>) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(namespace);
    hash.update([sort as u8]);
    let mut ids: Vec<_> = hidden.iter().copied().collect();
    ids.sort_unstable();
    hash.update((ids.len() as u64).to_le_bytes());
    for id in ids {
        hash.update(id.to_le_bytes());
    }
    hash.finalize().into()
}

#[derive(Clone, Copy)]
enum Position {
    First,
    Last,
    After(u32),
    Before(u32),
    From(u32),
}
impl Position {
    fn parts(self) -> (u8, u32) {
        match self {
            Self::First => (0, 0),
            Self::Last => (1, 0),
            Self::After(id) => (2, id),
            Self::Before(id) => (3, id),
            Self::From(id) => (4, id),
        }
    }
}
struct Cursor {
    position: Position,
    as_of: i64,
}
struct Bound<'a> {
    cursor: Cursor,
    anchor: Option<Rank<'a>>,
}
fn encode(index: &Index, spec: Spec, position: Position, as_of: i64) -> String {
    let mut bytes = Vec::with_capacity(TOKEN_BYTES);
    bytes.extend_from_slice(b"AXP1");
    for value in index.stamp() {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&spec.binding);
    bytes.extend_from_slice(&as_of.to_le_bytes());
    let (kind, id) = position.parts();
    bytes.push(kind);
    bytes.extend_from_slice(&id.to_le_bytes());
    const HEX: &[u8; 16] = b"0123456789abcdef";
    bytes
        .into_iter()
        .flat_map(|b| {
            [
                HEX[(b >> 4) as usize] as char,
                HEX[(b & 15) as usize] as char,
            ]
        })
        .collect()
}
fn decode(index: &Index, spec: Spec, token: &str) -> Result<Cursor> {
    ensure!(
        token.len() == TOKEN_BYTES * 2,
        "Invalid page cursor; start from the first page"
    );
    let bytes = token
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |b: u8| match b {
                b'0'..=b'9' => Some(b - b'0'),
                b'a'..=b'f' => Some(b - b'a' + 10),
                _ => None,
            };
            digit(pair[0])
                .zip(digit(pair[1]))
                .map(|(a, b)| (a << 4) | b)
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| anyhow::anyhow!("Invalid page cursor; start from the first page"))?;
    ensure!(&bytes[..4] == b"AXP1", "Unsupported page cursor");
    for (i, value) in index.stamp().iter().enumerate() {
        ensure!(
            bytes[4 + i * 8..12 + i * 8] == value.to_le_bytes(),
            "Index changed; start from the first page"
        );
    }
    ensure!(
        bytes[44..76] == spec.binding,
        "Cursor belongs to a different query, scope or sort"
    );
    let as_of = i64::from_le_bytes(bytes[76..84].try_into()?);
    ensure!(
        (0..=crate::unix_now().saturating_add(300)).contains(&as_of),
        "Invalid cursor time"
    );
    let id = u32::from_le_bytes(bytes[85..89].try_into()?);
    let position = match bytes[84] {
        0 if id == 0 => Position::First,
        1 if id == 0 => Position::Last,
        2 => Position::After(id),
        3 => Position::Before(id),
        4 => Position::From(id),
        _ => anyhow::bail!("Invalid cursor position"),
    };
    Ok(Cursor { position, as_of })
}

#[derive(Clone, Copy)]
struct Rank<'a> {
    entry: Entry,
    name: &'a [u8],
    sort: Sort,
}
impl Rank<'_> {
    fn new(index: &Index, entry: Entry, sort: Sort) -> Rank<'_> {
        Rank {
            entry,
            name: index.name(entry),
            sort,
        }
    }
}
impl PartialEq for Rank<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Rank<'_> {}
impl PartialOrd for Rank<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Rank<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        let primary = match self.sort {
            Sort::Name => Ordering::Equal,
            Sort::Size => other.entry.allocated.cmp(&self.entry.allocated),
            Sort::Modified => other.entry.modified.cmp(&self.entry.modified),
        };
        primary
            .then_with(|| self.name.cmp(other.name))
            .then(self.entry.id.cmp(&other.entry.id))
    }
}
pub fn compare(index: &Index, sort: Sort, a: Entry, b: Entry) -> Ordering {
    Rank::new(index, a, sort).cmp(&Rank::new(index, b, sort))
}
enum Top<'a> {
    First(BinaryHeap<Rank<'a>>),
    Last(BinaryHeap<Reverse<Rank<'a>>>),
}
impl<'a> Top<'a> {
    fn add(&mut self, value: Rank<'a>, limit: usize) {
        match self {
            Self::First(heap) => {
                if heap.len() < limit {
                    heap.push(value);
                } else if heap.peek().is_some_and(|old| value < *old) {
                    heap.pop();
                    heap.push(value);
                }
            }
            Self::Last(heap) => {
                if heap.len() < limit {
                    heap.push(Reverse(value));
                } else if heap.peek().is_some_and(|old| value > old.0) {
                    heap.pop();
                    heap.push(Reverse(value));
                }
            }
        }
    }
    fn ordered(self) -> Vec<Rank<'a>> {
        let mut values: Vec<_> = match self {
            Self::First(h) => h.into_vec(),
            Self::Last(h) => h.into_iter().map(|Reverse(r)| r).collect(),
        };
        values.sort_unstable();
        values
    }
}

pub fn first(
    index: &Index,
    spec: Spec,
    limit: usize,
    candidates: impl Iterator<Item = u32>,
    matches: impl Fn(Entry, i64) -> bool,
    generation: Option<(&AtomicU64, u64)>,
) -> Page {
    scan(
        index,
        spec,
        limit,
        Bound {
            cursor: Cursor {
                position: Position::First,
                as_of: crate::unix_now(),
            },
            anchor: None,
        },
        candidates,
        matches,
        generation,
    )
}
pub fn select(
    index: &Index,
    spec: Spec,
    request: Request<'_>,
    candidates: impl Iterator<Item = u32>,
    matches: impl Fn(Entry, i64) -> bool,
    generation: Option<(&AtomicU64, u64)>,
) -> Result<Page> {
    let cursor = match request.cursor {
        Some(token) => decode(index, spec, token)?,
        None => Cursor {
            position: Position::First,
            as_of: crate::unix_now(),
        },
    };
    let anchor = match cursor.position {
        Position::After(id) | Position::Before(id) | Position::From(id) => {
            let entry = index
                .entry(id)
                .filter(|e| matches(*e, cursor.as_of))
                .ok_or_else(|| {
                    anyhow::anyhow!("Cursor item no longer matches; start from the first page")
                })?;
            Some(Rank::new(index, entry, spec.sort))
        }
        _ => None,
    };
    let mut page = scan(
        index,
        spec,
        request.limit,
        Bound { cursor, anchor },
        candidates,
        matches,
        generation,
    );
    page.cursor = request.cursor.map(str::to_string);
    Ok(page)
}
pub fn at(index: &Index, spec: Spec, id: u32) -> String {
    encode(index, spec, Position::From(id), crate::unix_now())
}

fn scan<'a>(
    index: &'a Index,
    spec: Spec,
    limit: usize,
    bound: Bound<'a>,
    candidates: impl Iterator<Item = u32>,
    matches: impl Fn(Entry, i64) -> bool,
    generation: Option<(&AtomicU64, u64)>,
) -> Page {
    let Bound { cursor, anchor } = bound;
    let started = Instant::now();
    let limit = limit.clamp(1, MAX_SIZE);
    let backwards = matches!(cursor.position, Position::Before(_) | Position::Last);
    let mut best = if backwards {
        Top::Last(BinaryHeap::new())
    } else {
        Top::First(BinaryHeap::new())
    };
    let mut largest = Top::First(BinaryHeap::new());
    let mut eligible = 0u64;
    let mut page = Page::default();
    for (visited, id) in candidates.enumerate() {
        if visited % 1024 == 0
            && generation.is_some_and(|(g, n)| g.load(AtomicOrdering::Relaxed) != n)
        {
            page.canceled = true;
            break;
        }
        let Some(entry) = index.entry(id) else {
            continue;
        };
        if !matches(entry, cursor.as_of) {
            continue;
        }
        page.matched += 1;
        page.total_bytes = page.total_bytes.saturating_add(entry.allocated);
        if spec.summarize {
            largest.add(Rank::new(index, entry, Sort::Size), 8);
        }
        let rank = Rank::new(index, entry, spec.sort);
        let accepted = match cursor.position {
            Position::After(_) => anchor.is_some_and(|a| rank > a),
            Position::Before(_) => anchor.is_some_and(|a| rank < a),
            Position::From(_) => anchor.is_some_and(|a| rank >= a),
            _ => true,
        };
        if accepted {
            eligible += 1;
            best.add(rank, limit);
        }
    }
    if generation.is_some_and(|(g, n)| g.load(AtomicOrdering::Relaxed) != n) {
        page.canceled = true;
    }
    if !page.canceled {
        page.ids = best.ordered().into_iter().map(|r| r.entry.id).collect();
        page.largest = largest.ordered().into_iter().map(|r| r.entry.id).collect();
        let offset = if backwards {
            eligible.saturating_sub(page.ids.len() as u64)
        } else {
            page.matched - eligible
        };
        page.paging.offset = offset;
        if offset > 0 {
            page.paging.previous_cursor = Some(encode(
                index,
                spec,
                page.ids
                    .first()
                    .map_or(Position::Last, |id| Position::Before(*id)),
                cursor.as_of,
            ));
            page.paging.first_cursor = Some(encode(index, spec, Position::First, cursor.as_of));
        }
        if offset + (page.ids.len() as u64) < page.matched {
            page.paging.next_cursor = Some(encode(
                index,
                spec,
                page.ids
                    .last()
                    .map_or(Position::First, |id| Position::After(*id)),
                cursor.as_of,
            ));
            page.paging.last_cursor = Some(encode(index, spec, Position::Last, cursor.as_of));
        }
    }
    page.elapsed_ms = started.elapsed().as_secs_f64() * 1000.;
    page
}
