//! A bounded immutable sidecar. The large v1 base is never rewritten by refresh.
use super::*;
use sha2::{Digest, Sha256};
use std::os::unix::fs::OpenOptionsExt;

pub(super) const HEADER: usize = 128;
pub(super) const RECORD: usize = 72;
pub(super) const MAGIC: &[u8; 8] = b"AXITDLT1";
pub(super) const MAX_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const MAX_CHANGES: u32 = 131_072;

pub(super) fn file_stamp(md: &Metadata) -> [u64; 5] {
    [
        md.dev(),
        md.ino(),
        md.mtime() as u64,
        md.mtime_nsec() as u64,
        md.len(),
    ]
}

pub(super) struct Delta {
    map: Mmap,
    stamp: [u64; 5],
    pub count: u32,
    pub live: u32,
    pub finished: i64,
    pub errors: u64,
    records: u32,
    names_start: usize,
    changed: Vec<u8>,
}

impl Delta {
    pub(super) fn read(path: &Path, base: &Index) -> Result<Option<Self>> {
        let file = match File::options()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).context("Cannot open incremental index"),
        };
        let md = file.metadata()?;
        ensure!(
            md.is_file() && (HEADER as u64..=MAX_BYTES).contains(&md.len()),
            "Invalid incremental index size"
        );
        // Sidecars, like base indexes, are immutable after atomic publication.
        let map = unsafe { MmapOptions::new().map(&file)? };
        ensure!(
            &map[..8] == MAGIC && u32_at(&map, 8) == 1 && u32_at(&map, 12) == RECORD as u32,
            "Invalid incremental index header"
        );
        let bound: [u64; 5] = std::array::from_fn(|i| u64_at(&map, 16 + i * 8));
        // A completed full scan retires the old overlay. During its atomic
        // base rename the prior sidecar can briefly remain at this path.
        if bound != base.stamp {
            return Ok(None);
        }
        ensure!(
            u32_at(&map, 56) == base.base_count,
            "Incremental index base count differs"
        );
        let count = u32_at(&map, 60);
        let live = u32_at(&map, 64);
        let records = u32_at(&map, 68);
        ensure!(
            u64_at(&map, 88) == base.errors,
            "Incremental coverage differs from its full scan"
        );
        ensure!(
            count >= base.base_count
                && count - base.base_count <= MAX_CHANGES
                && records <= MAX_CHANGES
                && live > 0
                && live <= count,
            "Invalid incremental entry counts"
        );
        let names_start = HEADER + records as usize * RECORD;
        ensure!(
            names_start.checked_add(u64_at(&map, 72) as usize) == Some(map.len()),
            "Truncated incremental index"
        );
        let mut result = Self {
            stamp: file_stamp(&md),
            count,
            live,
            records,
            names_start,
            finished: u64_at(&map, 80) as i64,
            errors: u64_at(&map, 88),
            changed: vec![0; (count as usize).div_ceil(8)],
            map,
        };
        let mut previous = None;
        let mut next_added = base.base_count;
        for row in 0..records {
            let bytes = result.row(row);
            let id = u32_at(bytes, 0);
            ensure!(
                id < count && previous.is_none_or(|last| id > last),
                "Unordered incremental records"
            );
            ensure!(u32_at(bytes, 4) <= 1, "Invalid incremental record kind");
            if id >= base.base_count {
                ensure!(id == next_added, "Missing incremental record");
                next_added += 1;
            }
            previous = Some(id);
            result.changed[id as usize / 8] |= 1 << (id % 8);
        }
        ensure!(next_added == count, "Missing added entries");
        result.validate(base)?;
        Ok(Some(result))
    }
    fn row(&self, position: u32) -> &[u8] {
        let offset = HEADER + position as usize * RECORD;
        &self.map[offset..offset + RECORD]
    }
    pub(super) fn changed(&self, id: u32) -> bool {
        self.changed
            .get(id as usize / 8)
            .is_some_and(|byte| byte & (1 << (id % 8)) != 0)
    }
    pub(super) fn entry(&self, id: u32) -> Option<Entry> {
        let (mut start, mut end) = (0, self.records);
        while start < end {
            let middle = start + (end - start) / 2;
            let row = self.row(middle);
            match u32_at(row, 0).cmp(&id) {
                std::cmp::Ordering::Less => start = middle + 1,
                std::cmp::Ordering::Greater => end = middle,
                std::cmp::Ordering::Equal => {
                    return (u32_at(row, 4) == 0).then(|| Entry::decode(id, &row[8..]));
                }
            }
        }
        None
    }
    pub(super) fn name(&self, e: Entry, base_names: u64) -> &[u8] {
        let offset = self.names_start + (e.name_offset - base_names) as usize;
        &self.map[offset..offset + e.name_len as usize]
    }
    fn validate(&self, base: &Index) -> Result<()> {
        let entry = |id| {
            if self.changed(id) {
                self.entry(id)
            } else {
                base.base_entry(id)
            }
        };
        let name = |e: Entry| -> Result<&[u8]> {
            let end = e
                .name_offset
                .checked_add(e.name_len as u64)
                .context("Incremental name overflow")?;
            ensure!(e.name_len > 0, "Empty incremental name");
            if e.name_offset < base.base_names_len() {
                ensure!(
                    end <= base.base_names_len(),
                    "Incremental name crosses an arena"
                );
                Ok(base.base_name(e))
            } else {
                ensure!(
                    end - base.base_names_len() <= (self.map.len() - self.names_start) as u64,
                    "Invalid incremental name range"
                );
                Ok(self.name(e, base.base_names_len()))
            }
        };
        let root = entry(0).context("Incremental index removed its root")?;
        let original = base.base_entry(0).context("Missing base root")?;
        ensure!(
            root.parent == NO_PARENT
                && root.is_dir()
                && root.device == original.device
                && root.inode == original.inode
                && name(root)? == base.base_name(original),
            "Incremental root identity changed"
        );
        let mut live = 0u32;
        for id in 0..self.count {
            let Some(e) = entry(id) else { continue };
            live += 1;
            if id > 0 {
                ensure!(
                    e.parent < id && entry(e.parent).is_some_and(|p| p.is_dir()),
                    "Invalid incremental parent chain"
                );
            }
            if self.changed(id) {
                ensure!(
                    e.flags & !(DIR | LINK | ALIAS | RESTRICTED) == 0,
                    "Unknown incremental entry flags"
                );
                ensure!(
                    !(e.is_dir() && e.is_link()),
                    "Invalid incremental entry type"
                );
                let value = name(e)?;
                ensure!(
                    !value.contains(&0)
                        && (id == 0 || value != b"." && value != b".." && !value.contains(&b'/')),
                    "Invalid incremental filename"
                );
            }
        }
        ensure!(live == self.live, "Incremental live count differs");
        Ok(())
    }
    pub(super) fn disk_bytes(&self) -> u64 {
        self.map.len() as u64
    }
    pub(super) fn generation(&self, base: [u64; 5]) -> [u64; 5] {
        let mut hash = Sha256::new();
        for value in base.into_iter().chain(self.stamp) {
            hash.update(value.to_le_bytes());
        }
        let bytes = hash.finalize();
        [
            u64_at(&bytes, 0),
            u64_at(&bytes, 8),
            u64_at(&bytes, 16),
            u64_at(&bytes, 24),
            base[4] + self.disk_bytes(),
        ]
    }
}

pub(super) fn attach(mut base: Index, path: &Path) -> Result<Index> {
    if let Some(delta) = Delta::read(path, &base)? {
        base.count = delta.count;
        base.live_count = delta.live;
        base.errors = delta.errors;
        base.delta = Some(delta);
    }
    Ok(base)
}

pub(super) struct Change {
    pub entry: Entry,
    pub name: Vec<u8>,
}

/// Construct and validate a candidate before its caller atomically publishes it.
/// The base mapping stays shared; no full base-index copy is constructed.
pub(super) fn write_candidate(
    base: &Index,
    changes: &std::collections::BTreeMap<u32, Option<Change>>,
    count: u32,
    live: u32,
    path: &Path,
) -> Result<Delta> {
    ensure!(
        changes.len() <= MAX_CHANGES as usize,
        "Incremental change budget reached; run a full scan"
    );
    ensure!(
        count >= base.base_count && count - base.base_count <= MAX_CHANGES,
        "Incremental ID budget reached; run a full scan"
    );
    let mut names = Vec::new();
    let mut records = Vec::with_capacity(changes.len() * RECORD);
    for (&id, value) in changes {
        let mut bytes = [0u8; RECORD];
        bytes[..4].copy_from_slice(&id.to_le_bytes());
        if let Some(change) = value {
            ensure!(change.entry.id == id, "Incremental entry ID differs");
            let mut e = change.entry;
            e.name_len = u32::try_from(change.name.len())?;
            if let Some(original) = base
                .base_entry(id)
                .filter(|old| base.base_name(*old) == change.name)
            {
                e.name_offset = original.name_offset;
            } else {
                ensure!(
                    HEADER + changes.len() * RECORD + names.len() + change.name.len()
                        <= MAX_BYTES as usize,
                    "Incremental name budget reached; run a full scan"
                );
                e.name_offset = base.base_names_len() + names.len() as u64;
                names.extend_from_slice(&change.name);
            }
            bytes[8..].copy_from_slice(&e.encode());
        } else {
            bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
        }
        records.extend_from_slice(&bytes);
    }
    let mut header = [0u8; HEADER];
    header[..8].copy_from_slice(MAGIC);
    header[8..12].copy_from_slice(&1u32.to_le_bytes());
    header[12..16].copy_from_slice(&(RECORD as u32).to_le_bytes());
    for (i, value) in base.stamp.iter().enumerate() {
        header[16 + i * 8..24 + i * 8].copy_from_slice(&value.to_le_bytes());
    }
    header[56..60].copy_from_slice(&base.base_count.to_le_bytes());
    header[60..64].copy_from_slice(&count.to_le_bytes());
    header[64..68].copy_from_slice(&live.to_le_bytes());
    header[68..72].copy_from_slice(&(changes.len() as u32).to_le_bytes());
    header[72..80].copy_from_slice(&(names.len() as u64).to_le_bytes());
    header[80..88].copy_from_slice(&crate::unix_now().to_le_bytes());
    header[88..96].copy_from_slice(&base.errors.to_le_bytes());
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&header)?;
    file.write_all(&records)?;
    file.write_all(&names)?;
    file.sync_all()?;
    Delta::read(path, base)?.context("Incremental candidate belongs to another base")
}
