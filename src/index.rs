use anyhow::{Context, Result, ensure};
use memmap2::{Mmap, MmapOptions};
use serde::{Deserialize, Serialize};
use std::os::unix::{
    ffi::{OsStrExt, OsStringExt},
    fs::{MetadataExt, PermissionsExt},
};
use std::{
    collections::HashSet,
    fs::{self, File, Metadata},
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

mod delta;
mod refresh;
mod walk;
pub use refresh::{RefreshReport, refresh, refresh_paths, refresh_scoped};

const HEADER: usize = 64;
const RECORD: usize = 64;
const MAGIC: &[u8; 8] = b"SCRYIDX1";
pub const DIR: u32 = 1;
pub const LINK: u32 = 2;
pub const ALIAS: u32 = 4;
pub const RESTRICTED: u32 = 8;
const NO_PARENT: u32 = u32::MAX;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Progress {
    pub entries: u64,
    pub errors: u64,
    pub allocated: u64,
    pub done: bool,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: u32,
    pub parent: u32,
    pub flags: u32,
    pub name_offset: u64,
    pub name_len: u32,
    pub uid: u32,
    pub logical: u64,
    pub allocated: u64,
    pub modified: i64,
    pub device: u64,
    pub inode: u64,
}

impl Entry {
    pub fn is_dir(self) -> bool {
        self.flags & DIR != 0
    }
    pub fn is_link(self) -> bool {
        self.flags & LINK != 0
    }
    fn decode(id: u32, b: &[u8]) -> Self {
        Self {
            id,
            parent: u32_at(b, 0),
            flags: u32_at(b, 4),
            name_offset: u64_at(b, 8),
            name_len: u32_at(b, 16),
            uid: u32_at(b, 20),
            logical: u64_at(b, 24),
            allocated: u64_at(b, 32),
            modified: u64_at(b, 40) as i64,
            device: u64_at(b, 48),
            inode: u64_at(b, 56),
        }
    }
    fn encode(self) -> [u8; RECORD] {
        let mut b = [0; RECORD];
        b[0..4].copy_from_slice(&self.parent.to_le_bytes());
        b[4..8].copy_from_slice(&self.flags.to_le_bytes());
        b[8..16].copy_from_slice(&self.name_offset.to_le_bytes());
        b[16..20].copy_from_slice(&self.name_len.to_le_bytes());
        b[20..24].copy_from_slice(&self.uid.to_le_bytes());
        b[24..32].copy_from_slice(&self.logical.to_le_bytes());
        b[32..40].copy_from_slice(&self.allocated.to_le_bytes());
        b[40..48].copy_from_slice(&self.modified.to_le_bytes());
        b[48..56].copy_from_slice(&self.device.to_le_bytes());
        b[56..64].copy_from_slice(&self.inode.to_le_bytes());
        b
    }
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}
fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes([
        b[i],
        b[i + 1],
        b[i + 2],
        b[i + 3],
        b[i + 4],
        b[i + 5],
        b[i + 6],
        b[i + 7],
    ])
}

pub struct Index {
    map: Mmap,
    /// Exclusive entry-ID bound. Deleted incremental records leave holes.
    pub count: u32,
    pub started: i64,
    pub finished: i64,
    pub errors: u64,
    stamp: [u64; 5],
    names_start: usize,
    base_count: u32,
    live_count: u32,
    delta: Option<delta::Delta>,
    pub(crate) cache_candidates: OnceLock<Vec<u32>>,
}

impl Index {
    /// Recover the established scan scope even if its incremental sidecar is damaged.
    pub fn base_root(path: &Path) -> Result<PathBuf> {
        Self::open_base(path)?.path(0)
    }

    pub fn open(path: &Path) -> Result<Self> {
        // A full scan may replace the base while we open its sidecar. Retry
        // rather than combining metadata from two different publications.
        for _ in 0..4 {
            let base = Self::open_base(path)?;
            let stamp = base.stamp;
            let opened = delta::attach(base, &path.with_extension("delta"));
            let current = fs::metadata(path)?;
            if delta::file_stamp(&current) == stamp {
                return opened;
            }
        }
        anyhow::bail!("Index changed while opening; retry the request")
    }

    fn open_base(path: &Path) -> Result<Self> {
        let file = File::open(path)
            .context("No index yet. Run axit scan, or start the TUI to create one")?;
        // AXIT writes immutable generations and publishes them with an atomic rename.
        // It never truncates a published file. The private state directory is 0700.
        let map = unsafe { MmapOptions::new().map(&file)? };
        let metadata = file.metadata()?;
        ensure!(
            (HEADER..=1024 * 1024 * 1024 + HEADER).contains(&map.len()) && &map[..8] == MAGIC,
            "Invalid AXIT index header"
        );
        ensure!(
            u32_at(&map, 8) == 1 && u32_at(&map, 12) == RECORD as u32,
            "Unsupported index version"
        );
        let count = u64_at(&map, 16);
        ensure!(count > 0 && count < u32::MAX as u64, "Invalid entry count");
        let start = HEADER
            .checked_add(
                (count as usize)
                    .checked_mul(RECORD)
                    .context("Index overflow")?,
            )
            .context("Index overflow")?;
        ensure!(
            start.checked_add(u64_at(&map, 24) as usize) == Some(map.len()),
            "Truncated or malformed index"
        );
        let index = Self {
            cache_candidates: OnceLock::new(),
            count: count as u32,
            base_count: count as u32,
            live_count: count as u32,
            delta: None,
            started: u64_at(&map, 32) as i64,
            finished: u64_at(&map, 40) as i64,
            errors: u64_at(&map, 48),
            stamp: [
                metadata.dev(),
                metadata.ino(),
                metadata.mtime() as u64,
                metadata.mtime_nsec() as u64,
                metadata.len(),
            ],
            names_start: start,
            map,
        };
        // Validate all offsets once; search can then stay allocation-free.
        for id in 0..index.count {
            let e = index.entry(id).context("Missing entry")?;
            ensure!(
                (id == 0 && e.parent == NO_PARENT) || (id > 0 && e.parent < id),
                "Invalid parent chain"
            );
            let end = e
                .name_offset
                .checked_add(e.name_len as u64)
                .context("Name overflow")?;
            ensure!(
                end <= (index.map.len() - start) as u64 && e.name_len > 0,
                "Invalid name range"
            );
            let name = index.name(e);
            ensure!(!name.contains(&0), "Invalid filename");
            if id > 0 {
                ensure!(
                    name != b"." && name != b".." && !name.contains(&b'/'),
                    "Invalid child name"
                );
            }
        }
        ensure!(index.path(0)?.is_absolute(), "Index root must be absolute");
        Ok(index)
    }

    pub fn disk_bytes(&self) -> u64 {
        self.map.len() as u64 + self.delta.as_ref().map_or(0, |d| d.disk_bytes())
    }

    /// Number of visible records, excluding incremental tombstones.
    pub fn live_count(&self) -> u32 {
        self.live_count
    }

    pub fn last_refresh(&self) -> Option<i64> {
        self.delta.as_ref().map(|d| d.finished)
    }

    pub fn stamp(&self) -> [u64; 5] {
        self.delta
            .as_ref()
            .map_or(self.stamp, |d| d.generation(self.stamp))
    }

    pub fn entry(&self, id: u32) -> Option<Entry> {
        if let Some(delta) = &self.delta
            && delta.changed(id)
        {
            return delta.entry(id);
        }
        self.base_entry(id)
    }

    fn base_entry(&self, id: u32) -> Option<Entry> {
        if id >= self.base_count {
            return None;
        }
        let start = HEADER + id as usize * RECORD;
        let b = &self.map[start..start + RECORD];
        Some(Entry::decode(id, b))
    }

    pub fn name(&self, entry: Entry) -> &[u8] {
        if let Some(delta) = &self.delta
            && entry.name_offset >= self.base_names_len()
        {
            return delta.name(entry, self.base_names_len());
        }
        self.base_name(entry)
    }

    fn base_names_len(&self) -> u64 {
        (self.map.len() - self.names_start) as u64
    }

    fn base_name(&self, entry: Entry) -> &[u8] {
        let p = self.names_start + entry.name_offset as usize;
        &self.map[p..p + entry.name_len as usize]
    }

    pub fn display_name(&self, e: Entry) -> String {
        crate::clean(&String::from_utf8_lossy(self.name(e)))
    }

    pub fn path(&self, id: u32) -> Result<PathBuf> {
        let mut current = id;
        let mut parts = Vec::new();
        loop {
            let e = self.entry(current).context("Unknown entry")?;
            parts.push(std::ffi::OsString::from_vec(self.name(e).to_vec()));
            if e.parent == NO_PARENT {
                break;
            }
            ensure!(parts.len() <= 1024, "Path is too deep");
            current = e.parent;
        }
        let mut path = PathBuf::new();
        for p in parts.into_iter().rev() {
            path.push(p);
        }
        Ok(path)
    }

    pub fn children(&self, parent: u32, limit: usize) -> Vec<Entry> {
        let hidden = HashSet::new();
        crate::page::first(
            self,
            self.children_spec(parent, crate::query::Sort::Size, &hidden),
            limit,
            1..self.count,
            |e, _| e.parent == parent,
            None,
        )
        .ids
        .into_iter()
        .filter_map(|id| self.entry(id))
        .collect()
    }

    pub(crate) fn children_spec(
        &self,
        parent: u32,
        sort: crate::query::Sort,
        hidden: &HashSet<u32>,
    ) -> crate::page::Spec {
        let mut key = b"children".to_vec();
        key.extend_from_slice(&parent.to_le_bytes());
        crate::page::Spec {
            sort,
            binding: crate::page::binding(&key, sort, hidden),
            summarize: true,
        }
    }
    pub fn children_page(
        &self,
        parent: u32,
        sort: crate::query::Sort,
        request: crate::page::Request<'_>,
        hidden: &HashSet<u32>,
        generation: Option<(&std::sync::atomic::AtomicU64, u64)>,
    ) -> Result<crate::page::Page> {
        ensure!(
            self.entry(parent).is_some_and(|e| e.is_dir()),
            "Choose a directory"
        );
        crate::page::select(
            self,
            self.children_spec(parent, sort, hidden),
            request,
            1..self.count,
            |e, _| e.parent == parent && !self.is_hidden(e.id, hidden),
            generation,
        )
    }
    pub fn children_cursor_at(
        &self,
        parent: u32,
        sort: crate::query::Sort,
        hidden: &HashSet<u32>,
        id: u32,
    ) -> String {
        crate::page::at(self, self.children_spec(parent, sort, hidden), id)
    }

    pub fn is_hidden(&self, mut id: u32, hidden: &HashSet<u32>) -> bool {
        if hidden.is_empty() {
            return false;
        }
        loop {
            if hidden.contains(&id) {
                return true;
            }
            let Some(e) = self.entry(id) else {
                return false;
            };
            if e.parent == NO_PARENT {
                return false;
            }
            id = e.parent;
        }
    }

    pub fn find_path(&self, path: &Path) -> Option<u32> {
        let root = self.path(0).ok()?;
        // Resolve aliases in the parent only. Resolving the final component would
        // turn a symlink lookup into a lookup of its target (or follow a cycle).
        let normalized = path
            .parent()
            .and_then(|p| p.canonicalize().ok())
            .and_then(|p| path.file_name().map(|name| p.join(name)));
        let scoped = crate::scoped_alias(normalized.as_deref().unwrap_or(path), &root).ok()?;
        let path = scoped.as_path();
        if path == root {
            return Some(0);
        }
        let relative = path.strip_prefix(&root).ok()?;
        let mut parent = 0;
        let mut next = 1;
        for component in relative.components() {
            let name = component.as_os_str().as_bytes();
            parent = (next..self.count).find(|id| {
                self.entry(*id)
                    .is_some_and(|e| e.parent == parent && self.name(e) == name)
            })?;
            next = parent + 1;
        }
        Some(parent)
    }

    pub fn is_descendant(&self, mut id: u32, parent: u32) -> bool {
        loop {
            if id == parent {
                return true;
            }
            if id < parent {
                return false;
            }
            let Some(e) = self.entry(id) else {
                return false;
            };
            if e.parent == NO_PARENT {
                return false;
            }
            id = e.parent;
        }
    }
}

struct Scanner<'a> {
    records: BufWriter<File>,
    names: BufWriter<File>,
    names_bytes: u64,
    count: u32,
    seen_links: HashSet<(u64, u64)>,
    patches: Vec<(u32, u64, u32)>,
    errors: Vec<String>,
    error_count: u64,
    root_device: u64,
    excluded: &'a Path,
    cancel: &'a AtomicBool,
    progress: &'a Arc<Mutex<Progress>>,
}

impl Scanner<'_> {
    fn error(&mut self, path: &Path, error: impl std::fmt::Display) {
        self.error_count += 1;
        if self.errors.len() < 1500 {
            self.errors.push(format!("{}: {error}", crate::text(path)));
        }
    }

    fn visit(
        &mut self,
        path: &Path,
        name: &[u8],
        parent: u32,
        metadata: Metadata,
        depth: u32,
    ) -> Result<u64> {
        ensure!(
            !self.cancel.load(Ordering::Relaxed),
            "Scan canceled; previous index preserved"
        );
        ensure!(self.count < u32::MAX - 1, "Too many files for this index");
        ensure!(
            self.names_bytes + name.len() as u64 + (self.count as u64 + 1) * RECORD as u64
                <= 1024 * 1024 * 1024,
            "Index reached its 1 GiB size limit. Scan a narrower folder; previous index preserved"
        );
        let id = self.count;
        self.count += 1;
        let is_dir = metadata.is_dir();
        let alias = metadata.is_file()
            && metadata.nlink() > 1
            && !self.seen_links.insert((metadata.dev(), metadata.ino()));
        let mut flags = if is_dir { DIR } else { 0 }
            | if metadata.file_type().is_symlink() {
                LINK
            } else {
                0
            }
            | if alias { ALIAS } else { 0 };
        let mut total = if alias {
            0
        } else {
            metadata.blocks().saturating_mul(512)
        };
        let e = Entry {
            id,
            parent,
            flags,
            name_offset: self.names_bytes,
            name_len: name.len() as u32,
            uid: metadata.uid(),
            logical: if metadata.is_file() {
                metadata.len()
            } else {
                0
            },
            allocated: total,
            modified: metadata.mtime(),
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        self.records.write_all(&e.encode())?;
        self.names.write_all(name)?;
        self.names_bytes += name.len() as u64;
        if self.count.is_multiple_of(8192)
            && let Ok(mut p) = self.progress.lock()
        {
            p.entries = self.count as u64;
            p.errors = self.error_count;
            p.message = crate::text(path);
        }
        if is_dir {
            if depth > 256 {
                self.error(path, "Depth limit reached");
                flags |= RESTRICTED;
            } else {
                match fs::read_dir(path) {
                    Err(error) => {
                        self.error(path, error);
                        flags |= RESTRICTED;
                    }
                    Ok(children) => {
                        for child in children {
                            let child = match child {
                                Ok(child) => child,
                                Err(error) => {
                                    self.error(path, error);
                                    continue;
                                }
                            };
                            let child_path = child.path();
                            if child_path == self.excluded {
                                continue;
                            }
                            // DirEntry metadata uses the open parent directory on Unix,
                            // avoiding repeated full-path resolution for deep trees.
                            // Like symlink_metadata, it does not follow symlinks.
                            let md = match child.metadata() {
                                Ok(md) => md,
                                Err(error) => {
                                    self.error(&child_path, error);
                                    continue;
                                }
                            };
                            if md.dev() != self.root_device {
                                continue;
                            }
                            total = total.saturating_add(self.visit(
                                &child_path,
                                child.file_name().as_bytes(),
                                id,
                                md,
                                depth + 1,
                            )?);
                        }
                    }
                }
            }
            self.patches.push((id, total, flags));
        }
        Ok(total)
    }
}

pub fn scan(
    root: &Path,
    state: &Path,
    progress: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
) -> Result<PathBuf> {
    let root = root.canonicalize().context("Scan root is unavailable")?;
    ensure!(root.is_dir(), "Choose a directory to scan");
    fs::create_dir_all(state)?;
    fs::set_permissions(state, fs::Permissions::from_mode(0o700))?;
    let state = state.canonicalize()?;
    let excluded = crate::scoped_alias(&state, &root).unwrap_or_else(|_| state.clone());
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state.join("scan.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock).context("Another AXIT scan is already running")?;
    let temporary = tempfile::tempdir_in(&state)?;
    let started = crate::unix_now();
    let md = fs::symlink_metadata(&root)?;
    let mut scanner = Scanner {
        records: BufWriter::with_capacity(
            256 * 1024,
            File::create(temporary.path().join("records"))?,
        ),
        names: BufWriter::with_capacity(256 * 1024, File::create(temporary.path().join("names"))?),
        names_bytes: 0,
        count: 0,
        seen_links: HashSet::new(),
        patches: Vec::new(),
        errors: Vec::new(),
        error_count: 0,
        root_device: md.dev(),
        excluded: &excluded,
        cancel: &cancel,
        progress: &progress,
    };
    let total = scanner.visit(&root, root.as_os_str().as_bytes(), NO_PARENT, md, 0)?;
    scanner.records.flush()?;
    scanner.names.flush()?;
    let mut header = [0u8; HEADER];
    header[..8].copy_from_slice(MAGIC);
    header[8..12].copy_from_slice(&1u32.to_le_bytes());
    header[12..16].copy_from_slice(&(RECORD as u32).to_le_bytes());
    header[16..24].copy_from_slice(&(scanner.count as u64).to_le_bytes());
    header[24..32].copy_from_slice(&scanner.names_bytes.to_le_bytes());
    header[32..40].copy_from_slice(&started.to_le_bytes());
    header[40..48].copy_from_slice(&crate::unix_now().to_le_bytes());
    header[48..56].copy_from_slice(&scanner.error_count.to_le_bytes());
    let output = temporary.path().join("complete.idx");
    let mut destination = BufWriter::with_capacity(256 * 1024, File::create(&output)?);
    destination.write_all(&header)?;
    let mut records = File::open(temporary.path().join("records"))?;
    scanner.patches.sort_unstable_by_key(|p| p.0);
    let mut patches = scanner.patches.iter().peekable();
    let mut id = 0u32;
    let mut block = vec![0u8; RECORD * 4096];
    loop {
        let mut read = 0;
        while read < block.len() {
            let n = records.read(&mut block[read..])?;
            if n == 0 {
                break;
            }
            read += n;
        }
        if read == 0 {
            break;
        }
        ensure!(read.is_multiple_of(RECORD), "Incomplete index spool");
        for row in block[..read].as_chunks_mut::<RECORD>().0 {
            if let Some(&&(patch_id, total, flags)) = patches.peek()
                && patch_id == id
            {
                row[32..40].copy_from_slice(&total.to_le_bytes());
                row[4..8].copy_from_slice(&flags.to_le_bytes());
                patches.next();
            }
            id += 1;
        }
        destination.write_all(&block[..read])?;
    }
    std::io::copy(
        &mut File::open(temporary.path().join("names"))?,
        &mut destination,
    )?;
    destination.flush()?;
    destination.get_ref().sync_all()?;
    fs::set_permissions(&output, fs::Permissions::from_mode(0o600))?;
    Index::open(&output).context("New index validation failed; previous index preserved")?;
    ensure!(
        !cancel.load(Ordering::Relaxed),
        "Scan canceled; previous index preserved"
    );
    let target = crate::index_path(&state);
    let directory = File::open(&state)?;
    directory.sync_all()?;
    ensure!(
        !cancel.load(Ordering::Relaxed),
        "Scan canceled; previous index preserved"
    );
    fs::rename(&output, &target)?;
    // The old delta remains useful to readers of the old base. Retire its
    // directory entry only after the new base rename is durable; otherwise a
    // crash could resurrect the old base without its latest updates.
    let mut warnings = vec![];
    match directory.sync_all() {
        Ok(()) => {
            if let Err(error) = fs::remove_file(target.with_extension("delta"))
                && error.kind() != std::io::ErrorKind::NotFound
            {
                warnings.push(format!(
                    "Could not retire the old incremental index: {error}"
                ));
            }
            if let Err(error) = directory.sync_all() {
                warnings.push(format!(
                    "Could not confirm incremental-index retirement: {error}"
                ));
            }
        }
        Err(error) => warnings.push(format!(
            "Index published, but directory durability was not confirmed: {error}"
        )),
    }
    let error_report = serde_json::json!({"started":started,"finished":crate::unix_now(),"entries":scanner.count,"allocated_bytes":total,
        "restricted_or_missing":scanner.error_count,"errors":scanner.errors,"omitted_error_details":scanner.error_count.saturating_sub(scanner.errors.len() as u64),
        "notes":["Symlinks are not followed","Other mounted filesystems are excluded","Hardlinks counted once","APFS clones may share blocks; allocated sizes are not guaranteed reclaimable bytes"]});
    if let Err(error) = fs::write(
        state.join("latest-scan.json"),
        serde_json::to_vec_pretty(&error_report)?,
    ) {
        warnings.push(format!(
            "Index published, but the coverage report could not be saved: {error}"
        ));
    }
    if let Ok(mut p) = progress.lock() {
        p.entries = scanner.count as u64;
        p.errors = scanner.error_count;
        p.allocated = total;
        p.done = true;
        p.message = if warnings.is_empty() {
            "Scan complete".into()
        } else {
            format!("Scan complete · {}", warnings.join("; "))
        };
    }
    Ok(target)
}
