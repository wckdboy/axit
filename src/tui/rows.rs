use super::*;
use std::{
    cmp::Reverse,
    collections::{BinaryHeap, VecDeque},
};

const CACHE_CAPACITY: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Key {
    stamp: [u64; 5],
    view: View,
    folder: u32,
    sort: Sort,
    revision: u64,
}

#[derive(Clone, Default)]
pub(super) struct Data {
    pub ids: Vec<u32>,
    pub count: u64,
    pub bytes: u64,
}

#[derive(Default)]
pub(super) struct Cache(VecDeque<(Key, Data)>);

impl Cache {
    fn get(&mut self, key: Key) -> Option<Data> {
        let position = self.0.iter().position(|(k, _)| *k == key)?;
        let entry = self.0.remove(position)?;
        let data = entry.1.clone();
        self.0.push_front(entry);
        Some(data)
    }
    fn insert(&mut self, key: Key, data: Data) {
        self.0.retain(|(k, _)| *k != key);
        self.0.push_front((key, data));
        self.0.truncate(CACHE_CAPACITY);
    }
}

pub(super) fn compute(
    index: &Index,
    view: View,
    folder: u32,
    sort: Sort,
    hidden: &HashSet<u32>,
    generation: &AtomicU64,
    ticket: u64,
) -> Option<Data> {
    let canceled = || generation.load(Ordering::Relaxed) != ticket;
    let mut data = Data::default();
    match view {
        View::Space => {
            let mut largest = BinaryHeap::new();
            for id in 1..index.count {
                if id % 1024 == 0 && canceled() {
                    return None;
                }
                let e = index.entry(id)?;
                if e.parent != folder || (!hidden.is_empty() && index.is_hidden(id, hidden)) {
                    continue;
                }
                data.count += 1;
                data.bytes = data.bytes.saturating_add(e.allocated);
                let value = Reverse((e.allocated, id));
                if largest.len() < 1000 {
                    largest.push(value);
                } else if largest.peek().is_some_and(|old| value.0 > old.0) {
                    largest.pop();
                    largest.push(value);
                }
            }
            data.ids = largest.into_iter().map(|Reverse((_, id))| id).collect();
        }
        View::Clean => {
            for &id in ops::cache_candidates(index) {
                if canceled() {
                    return None;
                }
                if !hidden.is_empty() && index.is_hidden(id, hidden) {
                    continue;
                }
                data.ids.push(id);
            }
            data.count = data.ids.len() as u64;
            if data.ids.len() > 1000 {
                data.ids.select_nth_unstable_by(1000, |a, b| {
                    let (a, b) = (index.entry(*a).unwrap(), index.entry(*b).unwrap());
                    b.allocated.cmp(&a.allocated).then(a.id.cmp(&b.id))
                });
                data.ids.truncate(1000);
            }
        }
        _ => return Some(data),
    }
    if canceled() {
        return None;
    }
    data.ids.sort_unstable_by(|a, b| {
        let (a, b) = (index.entry(*a).unwrap(), index.entry(*b).unwrap());
        let names = || index.name(a).cmp(index.name(b)).then(a.id.cmp(&b.id));
        match sort {
            Sort::Name => names(),
            Sort::Size => b.allocated.cmp(&a.allocated).then_with(names),
            Sort::Modified => b.modified.cmp(&a.modified).then_with(names),
        }
    });
    data.ids.truncate(1000);
    (!canceled()).then_some(data)
}

impl App {
    pub(super) fn cache_listing(&self) {
        if let Some(index) = &self.index
            && matches!(self.view, View::Space | View::Clean)
            && let Ok(mut cache) = self.rows_cache.lock()
        {
            cache.insert(
                self.rows_key(index),
                Data {
                    ids: self.ids.clone(),
                    count: self.folder_count,
                    bytes: self.folder_bytes,
                },
            );
        }
    }
    fn rows_key(&self, index: &Index) -> Key {
        Key {
            stamp: index.stamp(),
            view: self.view,
            folder: self.folder,
            sort: self.sort,
            revision: self.rows_revision,
        }
    }

    pub(super) fn invalidate_rows(&mut self) {
        self.rows_revision = self.rows_revision.wrapping_add(1);
        if let Ok(mut cache) = self.rows_cache.lock() {
            cache.0.clear();
        }
    }

    pub(super) fn apply_listing(&mut self, data: Data, focused: Option<u32>, offset: usize) {
        self.ids = data.ids;
        self.folder_count = data.count;
        self.folder_bytes = data.bytes;
        let row = focused
            .and_then(|id| self.ids.iter().position(|item| *item == id))
            .unwrap_or(0);
        self.select_row(row);
        *self.table.offset_mut() = offset.min(self.ids.len().saturating_sub(1));
        self.listing = false;
    }

    pub(super) fn listing(&mut self, focused: Option<u32>, offset: usize) {
        let Some(index) = self.index.clone() else {
            return;
        };
        if !matches!(self.view, View::Space | View::Clean) {
            return;
        }
        let key = self.rows_key(&index);
        let cached = self.rows_cache.lock().ok().and_then(|mut c| c.get(key));
        if let Some(data) = cached {
            self.apply_listing(data, focused, offset);
            return;
        }
        if index.count <= 4096 {
            if let Some(data) = compute(
                &index,
                self.view,
                self.folder,
                self.sort,
                &self.hidden,
                &self.generation,
                self.generation.load(Ordering::Relaxed),
            ) {
                if let Ok(mut cache) = self.rows_cache.lock() {
                    cache.insert(key, data.clone());
                }
                self.apply_listing(data, focused, offset);
            }
            return;
        }
        self.listing = true;
        self.pending_listing = Some((focused, offset));
        self.start_listing();
    }

    /// Keep at most one large listing worker; a newer request replaces the pending one.
    pub(super) fn start_listing(&mut self) {
        if self.list_inflight || self.pending_listing.is_none() {
            return;
        }
        let Some(index) = self.index.clone() else {
            return;
        };
        let (focused, offset) = self.pending_listing.take().unwrap();
        let key = self.rows_key(&index);
        let cached = self.rows_cache.lock().ok().and_then(|mut c| c.get(key));
        if let Some(data) = cached {
            self.apply_listing(data, focused, offset);
            return;
        }
        self.list_inflight = true;
        let (view, folder, sort, hidden, generation, tx, cache) = (
            self.view,
            self.folder,
            self.sort,
            self.hidden.clone(),
            self.generation.clone(),
            self.tx.clone(),
            self.rows_cache.clone(),
        );
        let ticket = generation.load(Ordering::Relaxed);
        thread::spawn(move || {
            let result = compute(&index, view, folder, sort, &hidden, &generation, ticket);
            if let Some(data) = &result
                && let Ok(mut cache) = cache.lock()
            {
                cache.insert(key, data.clone());
            }
            let _ = tx.send(Message::Listing(ticket, result, focused, offset));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn row_cache_is_bounded_and_recent_visits_survive_eviction() {
        let key = |folder| Key {
            stamp: [1; 5],
            view: View::Space,
            folder,
            sort: Sort::Size,
            revision: 0,
        };
        let mut cache = Cache::default();
        for folder in 0..CACHE_CAPACITY as u32 {
            cache.insert(key(folder), Data::default());
        }
        assert!(cache.get(key(0)).is_some());
        cache.insert(key(100), Data::default());
        assert_eq!(cache.0.len(), CACHE_CAPACITY);
        assert!(cache.get(key(0)).is_some());
        assert!(cache.get(key(1)).is_none());
    }
}
