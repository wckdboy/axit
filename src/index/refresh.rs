//! Reconcile selected filesystem objects into one bounded atomic sidecar.
use super::{
    delta::{self, Change},
    walk::{Directory, Facts},
    *,
};
use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsStr,
    os::unix::fs::OpenOptionsExt,
    time::Instant,
};

const MAX_PATHS: usize = 64;
const MAX_VISITED: usize = delta::MAX_CHANGES as usize;
#[derive(Clone, Debug, Serialize)]
pub struct RefreshReport {
    pub scopes: Vec<String>,
    #[serde(skip)]
    pub(crate) paths: Vec<PathBuf>,
    pub visited: u64,
    pub added: u32,
    pub removed: u32,
    pub updated: u32,
    pub entries: u32,
    pub delta_bytes: u64,
    pub published: bool,
    pub elapsed_ms: f64,
    pub warning: Option<String>,
    pub note: &'static str,
}
struct Target {
    path: PathBuf,
    parent: u32,
    old: Option<u32>,
    name: Vec<u8>,
}
struct Observation {
    facts: Facts,
    directory: Option<Directory>,
}
fn same(a: Entry, b: Entry) -> bool {
    Entry {
        name_offset: 0,
        ..a
    } == Entry {
        name_offset: 0,
        ..b
    }
}
fn absent(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}
fn permission_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
}
fn check(cancel: &AtomicBool) -> Result<()> {
    ensure!(
        !cancel.load(Ordering::Relaxed),
        "Refresh canceled; previous generation preserved"
    );
    Ok(())
}
fn bits(mask: &[u8], id: u32) -> bool {
    mask.get(id as usize / 8)
        .is_some_and(|b| b & (1 << (id % 8)) != 0)
}

fn scoped_path(path: &Path, root: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    ensure!(
        path.components().all(|p| matches!(
            p,
            std::path::Component::RootDir | std::path::Component::Normal(_)
        )),
        "Refresh paths cannot contain parent traversal"
    );
    let path: PathBuf = path.components().collect();
    if path.starts_with(root) {
        return Ok(path);
    }
    // Resolve genuine Data-volume aliases even when the final name was deleted.
    for ancestor in path.ancestors() {
        if let Ok(alias) = crate::scoped_alias(ancestor, root) {
            return Ok(alias.join(path.strip_prefix(ancestor)?));
        }
    }
    anyhow::bail!("Refresh path is outside the indexed root")
}
/// Validate paths without following their final names, including deleted names.
/// Relative paths are interpreted inside `scope`.
pub fn refresh_paths(scope: &Path, paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    ensure!(scope.is_absolute(), "Refresh scope must be absolute");
    ensure!(
        !paths.is_empty() && paths.len() <= MAX_PATHS,
        "Choose 1–64 refresh paths"
    );
    ensure!(
        paths.iter().map(|p| p.as_os_str().len()).sum::<usize>() <= 64 * 1024,
        "Refresh path budget exceeded"
    );
    paths
        .iter()
        .map(|p| {
            ensure!(
                !p.as_os_str().is_empty() && !p.as_os_str().as_bytes().contains(&0),
                "Refresh paths cannot be empty or contain NUL"
            );
            scoped_path(p, scope)
        })
        .collect()
}

fn target(index: &Index, path: PathBuf, root: &Path) -> Result<Target> {
    let parts: Vec<_> = path
        .strip_prefix(root)?
        .components()
        .map(|c| c.as_os_str())
        .collect();
    if parts.is_empty() {
        return Ok(Target {
            path,
            parent: NO_PARENT,
            old: Some(0),
            name: index.name(index.entry(0).context("Missing root")?).to_vec(),
        });
    }
    let mut parent = 0;
    let mut built = root.to_path_buf();
    for (position, name) in parts.iter().enumerate() {
        built.push(name);
        let child = (parent + 1..index.count).find(|id| {
            index
                .entry(*id)
                .is_some_and(|e| e.parent == parent && index.name(e) == name.as_bytes())
        });
        let can_descend = child
            .and_then(|id| index.entry(id))
            .is_some_and(|e| e.is_dir() && !e.is_link());
        if position + 1 == parts.len() || !can_descend {
            return Ok(Target {
                path: built,
                parent,
                old: child,
                name: name.as_bytes().to_vec(),
            });
        }
        parent = child.context("Missing parent")?;
    }
    unreachable!()
}

struct Reconciler<'a> {
    index: &'a Index,
    root: Directory,
    root_path: PathBuf,
    excluded: PathBuf,
    cancel: &'a AtomicBool,
    progress: &'a Arc<Mutex<Progress>>,
    changes: BTreeMap<u32, Option<Change>>,
    next: u32,
    by_name: HashMap<(u32, &'a [u8]), u32>,
    selected: Vec<u32>,
    seen: HashSet<u32>,
    ancestors: HashSet<u32>,
    child_sums: HashMap<u32, u64>,
    facts: HashMap<(u64, u64), Facts>,
    affected: HashSet<(u64, u64)>,
    visited: u64,
    touched: HashSet<u32>,
    observed: BTreeMap<(u32, u32), Facts>,
    parents_with_children: HashSet<u32>,
}
impl<'a> Reconciler<'a> {
    fn can_keep_restricted(&self, old: Option<u32>) -> bool {
        old.is_some_and(|id| {
            self.index
                .entry(id)
                .is_some_and(|e| e.is_dir() && e.flags & RESTRICTED != 0)
                && !self.parents_with_children.contains(&id)
        })
    }
    fn anchor(
        &self,
        parent: &Directory,
        name: &[u8],
        facts: Facts,
        old: Option<u32>,
    ) -> Result<Option<Directory>> {
        if !facts.directory() {
            return Ok(None);
        }
        match parent.child(name, facts) {
            Ok(directory) => Ok(Some(directory)),
            Err(error) if permission_error(&error) && self.can_keep_restricted(old) => Ok(None),
            Err(error) => Err(error),
        }
    }
    fn observe(&mut self, entry: Entry, facts: Facts) -> Result<()> {
        self.observed.insert((entry.parent, entry.id), facts);
        ensure!(
            self.observed.len() <= MAX_VISITED,
            "Refresh verification budget reached; choose a narrower scope or run a full scan"
        );
        Ok(())
    }

    fn verify_observed(&self) -> Result<()> {
        let mut parent_id = None;
        let mut parent = None;
        for ((id_parent, id), expected) in &self.observed {
            check(self.cancel)?;
            let actual = if *id == 0 {
                self.root.facts()?
            } else {
                if parent_id != Some(*id_parent) {
                    parent = Some(self.directory(*id_parent)?);
                    parent_id = Some(*id_parent);
                }
                let entry = self.entry(*id).context("Observed entry disappeared")?;
                parent
                    .as_ref()
                    .context("Missing verification parent")?
                    .stat(self.name(entry))
                    .context("Filesystem changed during refresh; previous generation preserved")?
            };
            ensure!(
                expected.same_read(actual),
                "Filesystem metadata changed during refresh; retry; previous generation preserved"
            );
        }
        Ok(())
    }

    fn entry(&self, id: u32) -> Option<Entry> {
        match self.changes.get(&id) {
            Some(v) => v.as_ref().map(|v| v.entry),
            None => self.index.base_entry(id),
        }
    }
    fn name(&self, e: Entry) -> &[u8] {
        match self.changes.get(&e.id) {
            Some(Some(v)) => &v.name,
            _ => self.index.base_name(e),
        }
    }
    fn stage(&mut self, e: Entry, name: &[u8]) -> Result<()> {
        self.touched.insert(e.id);
        if self
            .index
            .base_entry(e.id)
            .is_some_and(|old| same(old, e) && self.index.base_name(old) == name)
        {
            self.changes.remove(&e.id);
        } else {
            self.changes.insert(
                e.id,
                Some(Change {
                    entry: e,
                    name: name.to_vec(),
                }),
            );
        }
        ensure!(
            self.changes.len() <= MAX_VISITED,
            "Incremental record budget reached; run a full scan or choose a narrower scope"
        );
        Ok(())
    }
    fn directory(&self, id: u32) -> Result<Directory> {
        let mut chain = vec![];
        let mut at = id;
        while at != 0 {
            let e = self
                .entry(at)
                .context("Refresh parent is no longer indexed")?;
            ensure!(
                e.is_dir() && e.parent < e.id && chain.len() < 256,
                "Invalid refresh parent chain"
            );
            chain.push(e);
            at = e.parent;
        }
        let mut current = self.root.duplicate()?;
        for e in chain.into_iter().rev() {
            let value = self.name(e);
            let facts = current.stat(value)?;
            ensure!(
                (facts.device, facts.inode) == (e.device, e.inode),
                "A parent directory was replaced; refresh that parent or run a full scan"
            );
            current = current.child(value, facts)?;
        }
        Ok(current)
    }
    fn metadata(e: Entry, facts: Facts) -> Entry {
        Entry {
            uid: facts.uid,
            logical: if facts.regular() { facts.logical } else { 0 },
            allocated: facts.allocated,
            modified: facts.modified,
            device: facts.device,
            inode: facts.inode,
            flags: if facts.directory() {
                DIR
            } else if facts.symlink() {
                LINK
            } else {
                0
            },
            ..e
        }
    }
    fn visit(
        &mut self,
        parent: u32,
        old: Option<u32>,
        name: &[u8],
        observed: Observation,
        path: &Path,
        depth: usize,
    ) -> Result<(u32, u64)> {
        let Observation { facts, directory } = observed;
        check(self.cancel)?;
        self.visited += 1;
        ensure!(
            self.visited <= MAX_VISITED as u64 && depth <= 256,
            "Refresh traversal budget reached; choose a narrower scope or run a full scan"
        );
        let id = if let Some(id) = old {
            self.seen.insert(id);
            id
        } else {
            let id = self.next;
            self.next = self.next.checked_add(1).context("Index ID overflow")?;
            ensure!(
                self.next - self.index.base_count <= delta::MAX_CHANGES,
                "Incremental ID budget reached; run a full scan"
            );
            id
        };
        let mut e = Self::metadata(
            Entry {
                id,
                parent,
                flags: 0,
                name_offset: 0,
                name_len: u32::try_from(name.len())?,
                uid: 0,
                logical: 0,
                allocated: 0,
                modified: 0,
                device: 0,
                inode: 0,
            },
            facts,
        );
        if facts.directory() && directory.is_none() {
            ensure!(self.can_keep_restricted(old), "Missing directory anchor");
            e.flags |= RESTRICTED;
        }
        self.stage(e, name)?;
        self.observe(e, facts)?;
        if facts.regular() {
            self.facts.insert((facts.device, facts.inode), facts);
            self.affected.insert((facts.device, facts.inode));
        }
        let mut allocated = facts.allocated;
        if facts.directory() {
            let Some(directory) = directory else {
                return Ok((id, allocated));
            };
            let children = match directory.entries() {
                Ok(children) => children,
                Err(error) if permission_error(&error) && self.can_keep_restricted(old) => {
                    self.stage(
                        Entry {
                            flags: e.flags | RESTRICTED,
                            ..e
                        },
                        name,
                    )?;
                    return Ok((id, allocated));
                }
                Err(error) => return Err(error),
            };
            for child in children {
                check(self.cancel)?;
                let child = child?;
                let child_path = path.join(OsStr::from_bytes(&child));
                if child_path == self.excluded {
                    continue;
                }
                let md = directory.stat(&child).with_context(|| {
                    format!(
                        "Directory changed or became inaccessible: {}",
                        crate::text(&child_path)
                    )
                })?;
                if md.device != self.index.entry(0).context("Missing root")?.device {
                    continue;
                }
                let old = self.by_name.get(&(id, child.as_slice())).copied();
                let anchor = self.anchor(&directory, &child, md, old)?;
                let (_, bytes) = self.visit(
                    id,
                    old,
                    &child,
                    Observation {
                        facts: md,
                        directory: anchor,
                    },
                    &child_path,
                    depth + 1,
                )?;
                allocated = allocated
                    .checked_add(bytes)
                    .context("Directory allocation overflow; run a full scan")?;
            }
            ensure!(
                facts.same_read(directory.facts()?),
                "Directory changed while refreshing; retry"
            );
            self.stage(Entry { allocated, ..e }, name)?;
        }
        if self.visited.is_multiple_of(1024)
            && let Ok(mut progress) = self.progress.lock()
        {
            progress.entries = self.visited;
            progress.message = crate::text(path);
        }
        Ok((id, allocated))
    }
    fn adjust(&mut self, mut parent: u32, difference: i128) -> Result<()> {
        while parent != NO_PARENT {
            let mut e = self.entry(parent).context("Missing refresh ancestor")?;
            let name = self.name(e).to_vec();
            e.allocated = u64::try_from(e.allocated as i128 + difference)
                .context("Incremental directory total is out of range; run a full scan")?;
            self.stage(e, &name)?;
            parent = e.parent;
        }
        Ok(())
    }
    fn parent_metadata(&mut self) -> Result<()> {
        for id in self.ancestors.clone() {
            let old = self.index.entry(id).context("Missing ancestor")?;
            let current = self.entry(id).context("Missing effective ancestor")?;
            let directory = self.directory(id)?;
            let facts = directory.facts()?;
            self.observe(current, facts)?;
            let own = old
                .allocated
                .checked_sub(*self.child_sums.get(&id).unwrap_or(&0))
                .context("Inconsistent directory total; run a full scan")?;
            let mut e = Self::metadata(current, facts);
            e.flags = old.flags;
            e.allocated = current.allocated;
            let name = self.name(old).to_vec();
            self.stage(e, &name)?;
            self.adjust(id, facts.allocated as i128 - own as i128)?;
        }
        Ok(())
    }
    fn hardlinks(&mut self) -> Result<()> {
        let mut groups: HashMap<(u64, u64), Vec<u32>> = HashMap::new();
        let mut grouped = 0usize;
        for id in 0..self.next {
            if id.is_multiple_of(1024) {
                check(self.cancel)?;
            }
            let Some(e) = self.entry(id) else { continue };
            if !e.is_dir() && !e.is_link() && self.affected.contains(&(e.device, e.inode)) {
                grouped += 1;
                ensure!(
                    grouped <= MAX_VISITED,
                    "Related hardlink budget reached; run a full scan"
                );
                groups.entry((e.device, e.inode)).or_default().push(id);
            }
        }
        for (identity, ids) in groups {
            let first = self.entry(ids[0]).context("Missing hardlink")?;
            let facts = if let Some(facts) = self.facts.get(&identity) {
                *facts
            } else {
                let parent = self.directory(first.parent)?;
                let facts = parent.stat(self.name(first))?;
                ensure!(
                    (facts.device, facts.inode) == identity,
                    "A related hardlink changed; refresh its folder or run a full scan"
                );
                facts
            };
            ensure!(
                !facts.regular() || ids.len() as u64 <= facts.links,
                "Related hardlinks changed outside these scopes; refresh their folders or run a full scan"
            );
            for (position, id) in ids.into_iter().enumerate() {
                let old = self.entry(id).context("Missing linked entry")?;
                let mut fresh = Self::metadata(old, facts);
                if facts.regular() && position > 0 {
                    fresh.flags |= ALIAS;
                    fresh.allocated = 0;
                }
                let difference = fresh.allocated as i128 - old.allocated as i128;
                let name = self.name(old).to_vec();
                self.stage(fresh, &name)?;
                self.observe(fresh, facts)?;
                self.adjust(old.parent, difference)?;
            }
        }
        Ok(())
    }
}

pub fn refresh(
    root: &Path,
    state: &Path,
    paths: &[PathBuf],
    progress: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
) -> Result<RefreshReport> {
    refresh_scoped(root, state, paths, root, progress, cancel)
}

/// Reject expansion to an unindexed ancestor outside `scope`, under the same
/// publication lock as reconciliation and snapshot validation.
pub fn refresh_scoped(
    root: &Path,
    state: &Path,
    paths: &[PathBuf],
    scope: &Path,
    progress: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
) -> Result<RefreshReport> {
    reconcile(root, state, paths, scope, progress, cancel, || {})
}

fn reconcile(
    root: &Path,
    state: &Path,
    paths: &[PathBuf],
    scope: &Path,
    progress: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
    before_validation: impl FnOnce(),
) -> Result<RefreshReport> {
    let started = Instant::now();
    let paths = refresh_paths(scope, paths)?;
    check(&cancel)?;
    let state = state.canonicalize()?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(state.join("scan.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock)
        .context("Another scan or refresh is already running")?;
    let index = Index::open(&crate::index_path(&state))?;
    let indexed_root = index.path(0)?;
    ensure!(
        root.canonicalize()? == indexed_root,
        "Refresh root differs from the index; run a full scan first"
    );
    let scope = scoped_path(scope, &indexed_root)?;
    let root_entry = index.entry(0).context("Missing index root")?;
    let anchor = Directory::root(&indexed_root, (root_entry.device, root_entry.inode))?;
    let excluded = crate::scoped_alias(&state, &indexed_root).unwrap_or_else(|_| state.clone());
    let mut targets = vec![];
    for path in &paths {
        let path = scoped_path(path, &indexed_root)?;
        ensure!(
            !path.starts_with(&excluded),
            "Axit's private index directory is excluded from refresh"
        );
        let target = target(&index, path, &indexed_root)?;
        ensure!(
            target.path.starts_with(&scope),
            "Refresh needs an unindexed ancestor outside this scope; index that ancestor first"
        );
        targets.push(target);
    }
    targets.sort_by_key(|t| (t.path.components().count(), t.path.clone()));
    let mut scopes: Vec<Target> = vec![];
    for target in targets {
        if !scopes.iter().any(|p| target.path.starts_with(&p.path)) {
            scopes.push(target);
        }
    }
    let mut reconciler = Reconciler {
        index: &index,
        root: anchor,
        root_path: indexed_root.clone(),
        excluded,
        cancel: &cancel,
        progress: &progress,
        changes: BTreeMap::new(),
        next: index.count,
        by_name: HashMap::new(),
        selected: vec![],
        seen: HashSet::new(),
        ancestors: HashSet::new(),
        child_sums: HashMap::new(),
        facts: HashMap::new(),
        affected: HashSet::new(),
        visited: 0,
        touched: HashSet::new(),
        observed: BTreeMap::new(),
        parents_with_children: HashSet::new(),
    };
    if let Some(delta) = &index.delta {
        for id in 0..index.count {
            if id.is_multiple_of(1024) {
                check(&cancel)?;
            }
            if delta.changed(id) {
                reconciler.changes.insert(
                    id,
                    index.entry(id).map(|entry| Change {
                        name: index.name(entry).to_vec(),
                        entry,
                    }),
                );
            }
        }
    }
    for scope in &scopes {
        let mut parent = scope.parent;
        while parent != NO_PARENT {
            reconciler.ancestors.insert(parent);
            parent = index
                .entry(parent)
                .context("Missing scope ancestor")?
                .parent;
        }
    }
    let roots: HashSet<_> = scopes.iter().filter_map(|t| t.old).collect();
    let mut selected = vec![0u8; (index.count as usize).div_ceil(8)];
    for id in 0..index.count {
        if id.is_multiple_of(1024) {
            check(&cancel)?;
        }
        let Some(e) = index.entry(id) else { continue };
        if reconciler.ancestors.contains(&e.parent) {
            let total = reconciler.child_sums.entry(e.parent).or_default();
            *total = total
                .checked_add(e.allocated)
                .context("Directory total overflow")?;
        }
        if roots.contains(&id) || (e.parent != NO_PARENT && bits(&selected, e.parent)) {
            selected[id as usize / 8] |= 1 << (id % 8);
            reconciler.selected.push(id);
            ensure!(
                reconciler.selected.len() <= MAX_VISITED,
                "Selected subtree exceeds the incremental budget; choose a narrower scope or run a full scan"
            );
            reconciler.by_name.insert((e.parent, index.name(e)), id);
            reconciler.parents_with_children.insert(e.parent);
            if !e.is_dir() && !e.is_link() {
                reconciler.affected.insert((e.device, e.inode));
            }
        }
    }
    reconciler.parent_metadata()?;
    let mut root_differences = vec![];
    for scope in &scopes {
        check(&cancel)?;
        let result = if scope.old == Some(0) {
            let facts = reconciler.root.facts()?;
            let directory = reconciler.root.duplicate()?;
            Some(reconciler.visit(
                NO_PARENT,
                Some(0),
                &scope.name,
                Observation {
                    facts,
                    directory: Some(directory),
                },
                &scope.path,
                0,
            )?)
        } else {
            let parent = reconciler.directory(scope.parent)?;
            match parent.stat(&scope.name) {
                Err(e) if absent(&e) => None,
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("Cannot refresh {}", crate::text(&scope.path)));
                }
                Ok(facts) if facts.device != root_entry.device => None,
                Ok(facts) => {
                    let directory = reconciler.anchor(&parent, &scope.name, facts, scope.old)?;
                    let result = reconciler.visit(
                        scope.parent,
                        scope.old,
                        &scope.name,
                        Observation { facts, directory },
                        &scope.path,
                        0,
                    )?;
                    let after = parent.stat(&scope.name)?;
                    ensure!(
                        (after.device, after.inode) == (facts.device, facts.inode),
                        "Refresh target changed; retry"
                    );
                    Some(result)
                }
            }
        };
        let previous = scope
            .old
            .and_then(|id| index.entry(id))
            .map_or(0, |e| e.allocated);
        root_differences.push((
            scope.parent,
            result.map_or(0, |(_, n)| n) as i128 - previous as i128,
        ));
    }
    for id in &reconciler.selected {
        if !reconciler.seen.contains(id) {
            reconciler.changes.insert(*id, None);
        }
    }
    for (parent, difference) in root_differences {
        reconciler.adjust(parent, difference)?;
    }
    reconciler.hardlinks()?;
    let (mut added, mut removed, mut updated) = (0u32, 0u32, 0u32);
    reconciler
        .touched
        .extend(reconciler.changes.keys().copied());
    for &id in &reconciler.touched {
        let old = index.entry(id);
        let new = reconciler.entry(id);
        match (old, new) {
            (None, Some(_)) => added += 1,
            (Some(_), None) => removed += 1,
            (Some(old), Some(new))
                if !same(old, new) || index.name(old) != reconciler.name(new) =>
            {
                updated += 1
            }
            _ => {}
        }
    }

    let entries = index
        .live_count
        .checked_add(added)
        .and_then(|n| n.checked_sub(removed))
        .context("Invalid refreshed live count")?;
    let mut report = RefreshReport {
        scopes: scopes.iter().map(|t| crate::text(&t.path)).collect(),
        paths: scopes.iter().map(|t| t.path.clone()).collect(),
        visited: reconciler.visited,
        added,
        removed,
        updated,
        entries,
        delta_bytes: index.delta.as_ref().map_or(0, |d| d.disk_bytes()),
        published: false,
        elapsed_ms: 0.,
        warning: None,
        note: "Only these scopes and related hardlink metadata were reconciled. Full-scan coverage and permission gaps remain unchanged.",
    };
    if added != 0 || removed != 0 || updated != 0 {
        reconciler
            .changes
            .retain(|id, change| match (index.base_entry(*id), change) {
                (Some(old), Some(new)) => !same(old, new.entry) || index.base_name(old) != new.name,
                _ => true,
            });
        let temporary = tempfile::tempdir_in(&state)?;
        let path = temporary.path().join("refresh.delta");
        let candidate =
            delta::write_candidate(&index, &reconciler.changes, reconciler.next, entries, &path)?;
        check(&cancel)?;
        Directory::root(&reconciler.root_path, (root_entry.device, root_entry.inode))?;
        let parent = File::open(&state)?;
        parent
            .sync_all()
            .context("Cannot sync the private state directory; previous generation preserved")?;
        // The private hook lets tests force a race after candidate construction.
        // The public path instantiates an empty closure, optimized away.
        before_validation();
        reconciler.verify_observed()?;
        Directory::root(&reconciler.root_path, (root_entry.device, root_entry.inode))?;
        check(&cancel)?;
        fs::rename(&path, crate::index_path(&state).with_extension("delta"))?;
        report.published = true;
        report.delta_bytes = candidate.disk_bytes();
        if let Err(error) = parent.sync_all() {
            report.warning = Some(format!(
                "Index published, but directory durability was not confirmed: {error}"
            ));
        }
    } else {
        before_validation();
        reconciler.verify_observed()?;
        Directory::root(&reconciler.root_path, (root_entry.device, root_entry.inode))?;
        check(&cancel)?;
    }
    report.elapsed_ms = started.elapsed().as_secs_f64() * 1000.;
    if let Ok(mut p) = progress.lock() {
        p.entries = report.visited;
        p.done = true;
        p.message = if report.published {
            "Incremental refresh complete"
        } else {
            "No indexed metadata changes"
        }
        .into();
    }
    Ok(report)
}

#[cfg(test)]
mod race_tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("files");
        let state = temp.path().join("state");
        fs::create_dir_all(root.join("nested/deep")).unwrap();
        fs::create_dir_all(root.join("other")).unwrap();
        fs::create_dir(&state).unwrap();
        fs::write(root.join("nested/file"), vec![1; 8192]).unwrap();
        let root = root.canonicalize().unwrap();
        let state = state.canonicalize().unwrap();
        super::super::scan(
            &root,
            &state,
            Arc::new(Mutex::new(Progress::default())),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        (temp, root, state)
    }
    fn run(
        root: &Path,
        state: &Path,
        scope: &str,
        cancel: Arc<AtomicBool>,
        change: impl FnOnce(),
    ) -> Result<RefreshReport> {
        reconcile(
            root,
            state,
            &[PathBuf::from(scope)],
            root,
            Arc::new(Mutex::new(Progress::default())),
            cancel,
            change,
        )
    }
    #[test]
    fn leaf_write_during_candidate_construction_preserves_the_previous_sidecar() {
        let (_temp, root, state) = fixture();
        fs::write(root.join("nested/file"), vec![2; 16384]).unwrap();
        run(
            &root,
            &state,
            "nested",
            Arc::new(AtomicBool::new(false)),
            || {},
        )
        .unwrap();
        let delta = crate::index_path(&state).with_extension("delta");
        let previous = fs::read(&delta).unwrap();
        let old = Index::open(&crate::index_path(&state)).unwrap();
        fs::write(root.join("nested/new"), b"new").unwrap();
        let parent = Directory::root(&root.join("nested"), {
            let md = fs::metadata(root.join("nested")).unwrap();
            (md.dev(), md.ino())
        })
        .unwrap();
        let before = parent.facts().unwrap();
        let error = run(
            &root,
            &state,
            "nested",
            Arc::new(AtomicBool::new(false)),
            || {
                fs::write(root.join("nested/file"), vec![3; 65536]).unwrap();
                assert!(
                    before.same_read(parent.facts().unwrap()),
                    "Leaf writes must leave directory facts unchanged in this fixture"
                );
            },
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("metadata changed during refresh")
        );
        assert_eq!(fs::read(&delta).unwrap(), previous);
        assert_eq!(
            Index::open(&crate::index_path(&state)).unwrap().stamp(),
            old.stamp()
        );
    }
    #[test]
    fn a_noop_refresh_rechecks_leaves_instead_of_reporting_an_obsolete_snapshot_fresh() {
        let (_temp, root, state) = fixture();
        let base = crate::index_path(&state);
        let old = fs::read(&base).unwrap();
        let error = run(
            &root,
            &state,
            "nested",
            Arc::new(AtomicBool::new(false)),
            || {
                fs::write(root.join("nested/file"), vec![4; 32768]).unwrap();
            },
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("metadata changed during refresh")
        );
        assert_eq!(fs::read(&base).unwrap(), old);
        assert!(!base.with_extension("delta").exists());
    }
    #[test]
    fn final_verification_detects_new_entries_in_already_visited_nested_directories() {
        let (_temp, root, state) = fixture();
        let base = crate::index_path(&state);
        let old = fs::read(&base).unwrap();
        let error = run(
            &root,
            &state,
            "nested",
            Arc::new(AtomicBool::new(false)),
            || {
                fs::write(root.join("nested/deep/late"), b"late").unwrap();
            },
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("metadata changed during refresh")
        );
        assert_eq!(fs::read(&base).unwrap(), old);
        assert!(!base.with_extension("delta").exists());
    }
    #[test]
    fn related_hardlink_replacement_outside_the_scope_is_checked_before_publication() {
        let (_temp, root, state) = fixture();
        fs::hard_link(root.join("nested/file"), root.join("other/alias")).unwrap();
        super::super::scan(
            &root,
            &state,
            Arc::new(Mutex::new(Progress::default())),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        fs::write(root.join("nested/file"), vec![5; 16384]).unwrap();
        let result = run(
            &root,
            &state,
            "nested/file",
            Arc::new(AtomicBool::new(false)),
            || {
                fs::remove_file(root.join("other/alias")).unwrap();
                fs::write(root.join("other/alias"), b"replacement").unwrap();
            },
        );
        assert!(result.is_err());
        assert!(!crate::index_path(&state).with_extension("delta").exists());
    }
    #[test]
    fn cancellation_at_the_commit_boundary_preserves_the_previous_generation() {
        let (_temp, root, state) = fixture();
        let base = crate::index_path(&state);
        let old = fs::read(&base).unwrap();
        fs::write(root.join("nested/new"), b"new").unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let error = run(&root, &state, "nested", cancel.clone(), || {
            cancel.store(true, Ordering::Relaxed)
        })
        .unwrap_err();
        assert!(error.to_string().contains("canceled"));
        assert_eq!(fs::read(&base).unwrap(), old);
        assert!(!base.with_extension("delta").exists());
    }
}
