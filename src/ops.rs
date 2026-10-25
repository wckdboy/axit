use crate::{index::Index, system::OpenFiles};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::{self, Metadata, OpenOptions},
    io::Write,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    Trash,
    Move { destination: String },
    Clean,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Identity {
    pub device: u64,
    pub inode: u64,
    pub modified: i64,
    pub nanos: i64,
    pub changed: i64,
    pub changed_nanos: i64,
    pub size: u64,
}

impl Identity {
    fn of(m: &Metadata) -> Self {
        Self {
            device: m.dev(),
            inode: m.ino(),
            modified: m.mtime(),
            nanos: m.mtime_nsec(),
            changed: m.ctime(),
            changed_nanos: m.ctime_nsec(),
            size: m.len(),
        }
    }
    fn matches(&self, m: &Metadata) -> bool {
        self.device == m.dev()
            && self.inode == m.ino()
            && self.modified == m.mtime()
            && self.nanos == m.mtime_nsec()
            && self.changed == m.ctime()
            && self.changed_nanos == m.ctime_nsec()
            && self.size == m.len()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlannedEntry {
    pub id: u32,
    pub path: String,
    pub native: String,
    pub identity: Identity,
    pub bytes: u64,
    pub reason: String,
    pub fingerprint: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Blocked {
    pub path: String,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub created: i64,
    pub root: String,
    pub action: Action,
    #[serde(default)]
    pub destination_identity: Option<Identity>,
    pub minimum_age_secs: i64,
    pub entries: Vec<PlannedEntry>,
    pub blocked: Vec<Blocked>,
    pub total_bytes: u64,
}
#[derive(Debug, Serialize)]
pub struct Applied {
    pub plan: String,
    pub completed: Vec<String>,
    pub blocked: Vec<Blocked>,
    pub allocated_removed: u64,
    /// A failed entry changed paths; refresh the index before retrying it.
    pub needs_reconcile: bool,
    pub note: String,
}

struct CleanChecks<C> {
    capture: C,
    recent: Option<(Instant, OpenFiles)>,
}

impl<C: FnMut() -> Result<OpenFiles>> CleanChecks<C> {
    fn new(capture: C) -> Self {
        Self {
            capture,
            recent: None,
        }
    }

    // Reuse only the advisory precheck. The destructive boundary below is always fresh.
    fn before(&mut self, path: &Path, project: Option<&Path>) -> Result<()> {
        if let Some((time, open)) = &self.recent
            && time.elapsed() <= Duration::from_millis(100)
        {
            ensure!(!open.in_use(path, project), "File or project is in use");
        } else {
            ensure!(
                !(self.capture)()?.in_use(path, project),
                "File or project is in use"
            );
        }
        Ok(())
    }

    fn after_claim(&mut self, path: &Path, project: Option<&Path>) -> Result<()> {
        let open = (self.capture)()?;
        let active = open.in_use(path, project);
        self.recent = Some((Instant::now(), open));
        ensure!(!active, "Cache became active");
        Ok(())
    }
}

pub fn encode(path: &Path) -> String {
    path.as_os_str()
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub fn decode(value: &str) -> Result<PathBuf> {
    ensure!(
        value.is_ascii()
            && value.bytes().all(|b| b.is_ascii_hexdigit())
            && value.len().is_multiple_of(2)
            && value.len() < 131072,
        "Invalid native path"
    );
    let b = (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(!b.contains(&0), "Invalid native path");
    Ok(PathBuf::from(std::ffi::OsString::from_vec(b)))
}

fn private_dir(path: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == unsafe { libc::geteuid() },
        "Private state directory must be an owned directory, not a symlink"
    );
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn identity_path(path: &Path, root: &Path) -> Result<Metadata> {
    ensure!(
        path.is_absolute() && path.starts_with(root) && path != root,
        "Outside the selected scope, or scope root itself"
    );
    ensure!(
        path.to_str().is_some()
            && !path
                .as_os_str()
                .as_bytes()
                .iter()
                .any(|b| *b < 32 || *b == 127),
        "Unusual filename: search is supported, but mutate this path in your system file manager"
    );
    ensure!(
        path.components()
            .all(|x| !matches!(x, std::path::Component::ParentDir)),
        "Parent traversal is forbidden"
    );
    for parent in path.ancestors() {
        ensure!(
            !fs::symlink_metadata(parent)?.file_type().is_symlink(),
            "Symlink paths cannot be mutated"
        );
    }
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.uid() == unsafe { libc::geteuid() },
        "Not owned by the current user"
    );
    ensure!(m.is_file() || m.is_dir(), "Special files cannot be mutated");
    let home = crate::home()?.canonicalize()?;
    let policy_path = path
        .strip_prefix("/System/Volumes/Data")
        .map(|p| Path::new("/").join(p))
        .unwrap_or_else(|_| path.to_path_buf());
    ensure!(
        !home.starts_with(&policy_path),
        "Home and its parent directories are protected"
    );
    let names: Vec<_> = path
        .components()
        .map(|x| x.as_os_str().as_bytes())
        .collect();
    ensure!(
        !names.iter().any(|n| [
            b".git".as_slice(),
            b".ssh",
            b".gnupg",
            b"Keychains",
            b".Trash"
        ]
        .contains(n)),
        "Repository metadata, keys and Trash are protected"
    );
    let protected = [
        "/System",
        "/bin",
        "/sbin",
        "/usr",
        "/Library",
        "/Applications",
        "/private/var/db",
        "/private/var/vm",
        "/System/Volumes/Data/System",
        "/System/Volumes/Data/Library",
        "/System/Volumes/Data/Applications",
        "/System/Volumes/Data/private/var/db",
    ];
    ensure!(
        !protected
            .iter()
            .any(|p| policy_path.starts_with(p) || Path::new(p).starts_with(&policy_path)),
        "System and installed application folders are protected"
    );
    Ok(m)
}

pub fn cache_label(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.as_bytes();
    let parent = path.parent()?.file_name().unwrap_or_default().as_bytes();
    cache_kind(name, parent)
}

pub fn cache_candidate(index: &Index, entry: crate::index::Entry) -> bool {
    let parent = index
        .entry(entry.parent)
        .map(|e| index.name(e))
        .unwrap_or_default();
    cache_kind(index.name(entry), parent).is_some()
}

/// Metadata-only hints, built once per immutable index. Execution always revalidates live paths.
pub fn cache_candidates(index: &Index) -> &[u32] {
    index.cache_candidates.get_or_init(|| {
        (1..index.count)
            .filter_map(|id| index.entry(id))
            .filter(|e| e.is_dir() && e.allocated > 0 && cache_candidate(index, *e))
            .map(|e| e.id)
            .collect()
    })
}

fn cache_kind(name: &[u8], parent: &[u8]) -> Option<&'static str> {
    if name == b"incremental"
        || (name == b"debug" && parent == b"target")
        || parent == b"incremental"
    {
        Some("Rust compiler cache")
    } else if name == b".cxx" {
        Some("Android native build cache")
    } else if name == b".gradle" {
        Some("Project Gradle cache")
    } else if name == b"intermediates" && parent == b"build" {
        Some("Android build intermediates")
    } else if [
        b"Intermediates.noindex".as_slice(),
        b"Index.noindex",
        b"ModuleCache.noindex",
        b"SDKStatCaches.noindex",
        b"SDKExplicitPrecompiledModules",
    ]
    .contains(&name)
    {
        Some("Xcode compiler/index cache")
    } else {
        None
    }
}

fn git_generated(path: &Path, project: &Path) -> Result<()> {
    let git = || {
        let mut c = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.as_bytes().starts_with(b"GIT_") {
                c.env_remove(key);
            }
        }
        c.args(["-c", "core.fsmonitor=false"])
            .current_dir(project)
            .env("GIT_OPTIONAL_LOCKS", "0");
        c
    };
    let relative = path
        .strip_prefix(project)
        .context("Cache is outside its project")?;
    let identity = git()
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(project)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()?;
    ensure!(
        identity.status.success(),
        "No Git working copy found; generated status cannot be proven"
    );
    let tracked = git()
        .args(["--literal-pathspecs", "ls-files", "-z", "--"])
        .arg(relative)
        .current_dir(project)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()?;
    ensure!(
        tracked.status.success() && tracked.stdout.is_empty(),
        "Contains tracked source files"
    );
    let ignored = git()
        .args(["check-ignore", "-q", "--"])
        .arg(relative)
        .current_dir(project)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .status()?;
    ensure!(ignored.success(), "Not an ignored build directory");
    Ok(())
}

fn cache_scope(path: &Path) -> Result<Option<PathBuf>> {
    let label = cache_label(path).context("This location has no verified disposable-cache rule")?;
    if label.starts_with("Rust") {
        let target = path
            .ancestors()
            .find(|p| p.file_name().is_some_and(|s| s == "target"))
            .context("Rust target directory is missing")?;
        let tag =
            fs::read_to_string(target.join("CACHEDIR.TAG")).context("Rust cache marker missing")?;
        ensure!(
            tag.contains("8a477f597d28d172789f06886806bc55"),
            "Rust cache marker is invalid"
        );
        let project = target.parent().context("Missing project")?;
        ensure!(
            project.join("Cargo.toml").is_file(),
            "Shared external target: review manually"
        );
        Ok(Some(project.to_path_buf()))
    } else if label.starts_with("Android") || label.contains("Gradle") {
        let project = path
            .ancestors()
            .find(|p| {
                p.join("settings.gradle.kts").is_file() || p.join("settings.gradle").is_file()
            })
            .context("Gradle project marker missing")?;
        Ok(Some(project.to_path_buf()))
    } else {
        let allowed = crate::home()?
            .join("Library/Developer/Xcode/DerivedData")
            .canonicalize()
            .context("Xcode cache location is missing")?;
        ensure!(
            path.starts_with(allowed),
            "Outside Xcode's DerivedData directory"
        );
        Ok(None)
    }
}

fn cache_fingerprint(root: &Path, min_age: i64) -> Result<(String, u64)> {
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut count = 0u64;
    fn visit(
        path: &Path,
        root: &Path,
        min_age: i64,
        hash: &mut Sha256,
        bytes: &mut u64,
        count: &mut u64,
    ) -> Result<()> {
        *count += 1;
        ensure!(
            *count <= 2_000_000,
            "Cache is too large to verify in one operation"
        );
        let m = fs::symlink_metadata(path)?;
        ensure!(
            !m.file_type().is_symlink(),
            "Contains symlinks; review this cache manually"
        );
        ensure!(
            m.uid() == unsafe { libc::geteuid() },
            "Contains files owned by another user"
        );
        ensure!(
            m.mtime() <= crate::unix_now() - min_age,
            "Contains recently written files"
        );
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        let reserved = [
            ".git",
            ".ssh",
            ".gnupg",
            ".env",
            "outputs",
            "release",
            "releases",
            "archives",
            "models",
            "credentials",
            "id_rsa",
            "id_ed25519",
            "cargo.toml",
            "package.json",
        ];
        let suffixes = [
            ".apk",
            ".aab",
            ".ipa",
            ".dmg",
            ".xcarchive",
            ".zip",
            ".tar.gz",
            ".tar",
            ".tar.xz",
            ".tar.zst",
            ".7z",
            ".rar",
            ".app",
            ".pkg",
            ".deb",
            ".rpm",
            ".appimage",
            ".msi",
            ".msix",
            ".snap",
            ".flatpak",
            ".whl",
            ".gem",
            ".jar",
            ".tgz",
            ".jks",
            ".keystore",
            ".p12",
            ".pem",
            ".key",
            ".gguf",
            ".safetensors",
            ".onnx",
        ];
        ensure!(
            !reserved.contains(&name.as_str())
                && !name.starts_with(".env.")
                && !suffixes.iter().any(|s| name.ends_with(s)),
            "Contains source markers, packages, keys or model data"
        );
        hash.update(path.strip_prefix(root)?.as_os_str().as_bytes());
        for value in [
            m.dev(),
            m.ino(),
            m.mtime() as u64,
            m.mtime_nsec() as u64,
            m.len(),
            m.mode() as u64,
        ] {
            hash.update(value.to_le_bytes());
        }
        if m.is_dir() {
            let mut entries = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_unstable_by_key(|e| e.file_name());
            for child in entries {
                visit(&child.path(), root, min_age, hash, bytes, count)?;
            }
        } else {
            ensure!(m.is_file(), "Contains a special filesystem entry");
            *bytes = bytes.saturating_add(m.blocks().saturating_mul(512));
        }
        Ok(())
    }
    visit(root, root, min_age, &mut hash, &mut bytes, &mut count)?;
    Ok((format!("{:x}", hash.finalize()), bytes))
}

pub fn plan(
    index: &Index,
    ids: &[u32],
    action: Action,
    state: &Path,
    min_age: i64,
) -> Result<Plan> {
    plan_with_budget(index, ids, action, state, min_age, None)
}

#[derive(Clone, Copy)]
struct PlanBudget {
    bytes: u64,
    entries: usize,
}

fn plan_with_budget(
    index: &Index,
    ids: &[u32],
    mut action: Action,
    state: &Path,
    min_age: i64,
    budget: Option<PlanBudget>,
) -> Result<Plan> {
    ensure!(
        !ids.is_empty() && ids.len() <= 128,
        "Choose between 1 and 128 entries"
    );
    ensure!(min_age >= 0, "Invalid age rule");
    let root = index.path(0)?;
    let destination_identity = if let Action::Move { destination } = &mut action {
        let path = PathBuf::from(&*destination);
        ensure!(path.is_absolute(), "Destination must be absolute");
        let path = crate::scoped_alias(&path.canonicalize()?, &root)?;
        ensure!(
            path.is_dir() && path.starts_with(&root),
            "Destination must be inside the selected scope"
        );
        let md = if path == root {
            fs::symlink_metadata(&path)?
        } else {
            identity_path(&path, &root)?
        };
        *destination = path
            .to_str()
            .context("Destination must be UTF-8")?
            .to_string();
        Some(Identity::of(&md))
    } else {
        None
    };
    let open = OpenFiles::capture()?;
    let nonce = tempfile::NamedTempFile::new_in(state)?;
    let mut h = Sha256::new();
    h.update(nonce.path().as_os_str().as_bytes());
    h.update(crate::unix_now().to_le_bytes());
    let mut plan = Plan {
        id: format!("{:x}", h.finalize())[..20].to_string(),
        created: crate::unix_now(),
        root: encode(&root),
        action,
        destination_identity,
        minimum_age_secs: min_age,
        entries: Vec::new(),
        blocked: Vec::new(),
        total_bytes: 0,
    };
    let mut paths: Vec<_> = ids
        .iter()
        .map(|id| Ok((*id, index.path(*id)?)))
        .collect::<Result<_>>()?;
    paths.sort_unstable_by_key(|(_, p)| p.components().count());
    if budget.is_some() {
        paths.sort_unstable_by_key(|(id, p)| {
            (
                p.components().count(),
                std::cmp::Reverse(index.entry(*id).map_or(0, |e| e.allocated)),
                *id,
            )
        });
    }
    for (id, path) in paths {
        if budget.is_some_and(|b| plan.entries.len() >= b.entries || plan.total_bytes >= b.bytes) {
            break;
        }
        if plan
            .entries
            .iter()
            .any(|e| decode(&e.native).is_ok_and(|parent| path.starts_with(parent)))
        {
            continue;
        }
        let inspect = || -> Result<PlannedEntry> {
            let m = identity_path(&path, &root)?;
            let record = index.entry(id).context("Unknown index entry")?;
            ensure!(
                record.inode == m.ino() && record.device == m.dev(),
                "Index is stale: this path now identifies a different file"
            );
            let (fingerprint, bytes, reason) = if plan.action == Action::Clean {
                ensure!(
                    m.is_dir(),
                    "Permanent cleanup applies only to verified cache directories"
                );
                let project = cache_scope(&path)?;
                ensure!(
                    !open.in_use(&path, project.as_deref()),
                    "In use by a running process or build"
                );
                if let Some(project) = &project {
                    git_generated(&path, project)?;
                }
                let (fingerprint, bytes) = cache_fingerprint(&path, min_age)?;
                (
                    Some(fingerprint),
                    bytes,
                    cache_label(&path).unwrap_or("Cache").to_string(),
                )
            } else {
                ensure!(!open.in_use(&path, None), "In use by a running process");
                if let Action::Move { destination } = &plan.action {
                    let destination =
                        crate::scoped_alias(&PathBuf::from(destination).canonicalize()?, &root)?;
                    ensure!(
                        destination.is_dir() && destination.starts_with(&root),
                        "Destination must be an existing folder inside the scan scope"
                    );
                    ensure!(
                        !destination.starts_with(&path),
                        "Cannot move a folder into itself"
                    );
                    ensure!(
                        !destination
                            .join(path.file_name().context("Missing filename")?)
                            .exists(),
                        "A file with that name already exists at the destination"
                    );
                }
                (
                    None,
                    record.allocated,
                    if m.is_dir() {
                        "Includes this directory and all its contents".into()
                    } else {
                        "User-selected file".into()
                    },
                )
            };
            Ok(PlannedEntry {
                id,
                path: crate::text(&path),
                native: encode(&path),
                identity: Identity::of(&m),
                bytes,
                reason,
                fingerprint,
            })
        };
        match inspect() {
            Ok(e) => {
                if budget.is_some_and(|b| e.bytes > b.bytes.saturating_sub(plan.total_bytes)) {
                    plan.blocked.push(Blocked {
                        path: e.path,
                        reason: "Exceeds the remaining automatic-cleanup budget".into(),
                    });
                    continue;
                }
                plan.total_bytes = plan.total_bytes.saturating_add(e.bytes);
                plan.entries.push(e);
            }
            Err(error) => plan.blocked.push(Blocked {
                path: crate::text(&path),
                reason: error.to_string(),
            }),
        }
    }
    // Watchers can retry blocked candidates indefinitely without accumulating plans.
    if budget.is_some() && plan.entries.is_empty() {
        return Ok(plan);
    }
    private_dir(&state.join("plans"))?;
    let path = state.join("plans").join(format!("{}.json", plan.id));
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(&serde_json::to_vec_pretty(&plan)?)?;
    file.sync_all()?;
    Ok(plan)
}

pub fn load_plan(state: &Path, id: &str) -> Result<Plan> {
    ensure!(
        id.len() == 20 && id.chars().all(|c| c.is_ascii_hexdigit()),
        "Invalid plan id"
    );
    Ok(serde_json::from_slice(&fs::read(
        state.join("plans").join(format!("{id}.json")),
    )?)?)
}

pub fn rename_exclusive(source: &Path, destination: &Path) -> Result<()> {
    let from = CString::new(source.as_os_str().as_bytes())?;
    let to = CString::new(destination.as_os_str().as_bytes())?;
    // Both platform calls atomically refuse to replace an existing destination.
    #[cfg(target_os = "macos")]
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(target_os = "linux")]
    let result = unsafe {
        // Call the kernel directly: Rust's bundled musl does not export the
        // renameat2 wrapper on every supported target. No overwrite fallback.
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn trash_base() -> Result<PathBuf> {
    if let Some(path) =
        std::env::var_os("AXIT_TRASH_DIR").or_else(|| std::env::var_os("SCRY_TRASH_DIR"))
    {
        let path = PathBuf::from(path);
        ensure!(path.is_absolute(), "AXIT_TRASH_DIR must be absolute");
        recovery_dir(&path)?;
        return Ok(path.canonicalize()?.join("files"));
    }
    Ok(if cfg!(target_os = "macos") {
        crate::home()?.join(".Trash")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or(crate::home()?.join(".local/share"))
            .join("Trash/files")
    })
}

fn trash_destination(path: &Path, id: &str) -> Result<PathBuf> {
    let base = trash_base()?;
    recovery_dir(&base)?;
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .context("Unsupported filename")?;
    let mut end = filename.len().min(170);
    while !filename.is_char_boundary(end) {
        end -= 1;
    }
    let name = format!(
        "axit-{id}-{}-{}",
        fs::symlink_metadata(path)?.ino(),
        &filename[..end]
    );
    Ok(base.join(name))
}

fn recovery_dir(base: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(base)?;
    let metadata = fs::symlink_metadata(base)?;
    ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "Trash directory is not a private, owned directory"
    );
    Ok(())
}

#[cfg(any(target_os = "linux", test))]
fn trash_info(source: &Path, destination: &Path) -> Result<PathBuf> {
    let info = destination
        .parent()
        .and_then(Path::parent)
        .context("Invalid Trash path")?
        .join("info");
    recovery_dir(&info)?;
    let mut name = destination
        .file_name()
        .context("Missing trash name")?
        .to_os_string();
    name.push(".trashinfo");
    let path = info.join(name);
    let mut encoded = String::new();
    for &b in source.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    let seconds = crate::unix_now();
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    ensure!(
        !unsafe { libc::localtime_r(&seconds, tm.as_mut_ptr()) }.is_null(),
        "Cannot create Trash timestamp"
    );
    let tm = unsafe { tm.assume_init() };
    let mut date = [0u8; 64];
    let fmt = CString::new("%Y-%m-%dT%H:%M:%S")?;
    let n = unsafe { libc::strftime(date.as_mut_ptr().cast(), date.len(), fmt.as_ptr(), &tm) };
    ensure!(n > 0, "Cannot format Trash timestamp");
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    write!(
        file,
        "[Trash Info]\nPath={encoded}\nDeletionDate={}\n",
        std::str::from_utf8(&date[..n])?
    )?;
    file.sync_all()?;
    Ok(path)
}

fn remove_trash_info(destination: &Path) {
    #[cfg(target_os = "linux")]
    if let Some(root) = destination.parent().and_then(Path::parent)
        && let Some(name) = destination.file_name()
    {
        let mut name = name.to_os_string();
        name.push(".trashinfo");
        let _ = fs::remove_file(root.join("info").join(name));
    }
    #[cfg(target_os = "macos")]
    let _ = destination;
}

fn journal(state: &Path, value: serde_json::Value) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(state.join("operations.jsonl"))?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer(&mut file, &value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

pub fn apply(state: &Path, plan: &Plan, confirmation: &str) -> Result<Applied> {
    apply_with_journal(state, plan, confirmation, journal)
}

fn apply_with_journal(
    state: &Path,
    plan: &Plan,
    confirmation: &str,
    mut write_journal: impl FnMut(&Path, serde_json::Value) -> Result<()>,
) -> Result<Applied> {
    let phrase = if plan.action == Action::Clean {
        "CLEAN"
    } else if plan.action == Action::Trash {
        "TRASH"
    } else {
        "MOVE"
    };
    ensure!(
        confirmation == phrase,
        "Review the plan, then confirm with {phrase}"
    );
    ensure!(
        (0..=600).contains(&(crate::unix_now() - plan.created)),
        "Plan expired. Create a fresh preview"
    );
    ensure!(!plan.entries.is_empty(), "No eligible files in this plan");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state.join("operations.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock).context("Another operation is running")?;
    ensure!(
        !state
            .join("plans")
            .join(format!("{}.done", plan.id))
            .exists(),
        "This plan has already been applied"
    );
    let root = decode(&plan.root)?;
    let mut result = Applied {
        plan: plan.id.clone(),
        completed: Vec::new(),
        blocked: Vec::new(),
        allocated_removed: 0,
        needs_reconcile: false,
        note: if plan.action == Action::Trash {
            "Moved to Trash. This does not free disk space until Trash is emptied. Undo is available.".into()
        } else if plan.action == Action::Clean {
            "Verified disposable caches removed permanently. Allocated sizes may differ from space recovered.".into()
        } else {
            "Moved without overwriting existing files. Undo is available.".into()
        },
    };
    let mut process_checks = CleanChecks::new(OpenFiles::capture);
    for entry in &plan.entries {
        let mut changed = false;
        let mut execute = || -> Result<u64> {
            let path = decode(&entry.native)?;
            let metadata = identity_path(&path, &root)?;
            ensure!(
                entry.identity.matches(&metadata),
                "File changed since preview"
            );
            let project = if plan.action == Action::Clean {
                cache_scope(&path)?
            } else {
                None
            };
            if plan.action == Action::Clean {
                process_checks.before(&path, project.as_deref())?;
                if let Some(project) = &project {
                    git_generated(&path, project)?;
                }
            } else {
                ensure!(
                    !OpenFiles::capture()?.in_use(&path, None),
                    "File or project is in use"
                );
            }
            if plan.action == Action::Clean {
                let (fingerprint, _) = cache_fingerprint(&path, plan.minimum_age_secs)?;
                ensure!(
                    entry.fingerprint.as_ref() == Some(&fingerprint),
                    "Cache contents changed since preview"
                );
                let claimed = path
                    .parent()
                    .context("No parent")?
                    .join(format!(".axit-clean-{}-{}", plan.id, entry.id));
                rename_exclusive(&path, &claimed)?;
                changed = true;
                let mut check = || -> Result<()> {
                    let m = fs::symlink_metadata(&claimed)?;
                    ensure!(
                        m.ino() == entry.identity.inode && m.dev() == entry.identity.device,
                        "Directory identity changed"
                    );
                    let (fresh, _) = cache_fingerprint(&claimed, plan.minimum_age_secs)?;
                    ensure!(
                        fresh == fingerprint,
                        "Cache contents changed during operation"
                    );
                    process_checks.after_claim(&claimed, project.as_deref())?;
                    Ok(())
                };
                if let Err(error) = check() {
                    let restored = rename_exclusive(&claimed, &path);
                    changed = restored.is_err();
                    return Err(error.context(if restored.is_ok() {
                        "Cache restored; nothing deleted"
                    } else {
                        "Cache retained under .axit-clean name; restore original path manually"
                    }));
                }
                if let Err(error) = write_journal(
                    state,
                    serde_json::json!({"time":crate::unix_now(),"plan":plan.id,"action":"clean_started","source":entry.native,"claimed":encode(&claimed)}),
                ) {
                    let restored = rename_exclusive(&claimed, &path);
                    changed = restored.is_err();
                    return Err(error.context(if restored.is_ok() {
                        "Audit could not be saved; cache restored"
                    } else {
                        "Cache retained under .axit-clean name; nothing deleted"
                    }));
                }
                fs::remove_dir_all(&claimed)
                    .with_context(|| format!("Cleanup incomplete at {}", crate::text(&claimed)))?;
                write_journal(
                    state,
                    serde_json::json!({"time":crate::unix_now(),"plan":plan.id,"action":"clean","source":entry.native,"bytes":entry.bytes}),
                )
                .context("Cache removed, but completion audit could not be saved")?;
                Ok(entry.bytes)
            } else {
                let destination = match &plan.action {
                    Action::Trash => trash_destination(&path, &plan.id)?,
                    Action::Move { destination } => {
                        let folder = PathBuf::from(destination);
                        ensure!(folder.canonicalize()? == folder, "Destination path changed");
                        let m = if folder == root {
                            fs::symlink_metadata(&folder)?
                        } else {
                            identity_path(&folder, &root)?
                        };
                        let identity = plan
                            .destination_identity
                            .as_ref()
                            .context("Destination preview is missing; create a new plan")?;
                        ensure!(
                            m.ino() == identity.inode && m.dev() == identity.device,
                            "Destination folder changed since preview"
                        );
                        folder.join(path.file_name().context("Missing filename")?)
                    }
                    Action::Clean => bail!("Invalid operation state"),
                };
                if matches!(plan.action, Action::Move { .. }) {
                    ensure!(destination.starts_with(&root), "Destination scope changed");
                }
                // Persist recovery information before changing a user's path.
                write_journal(
                    state,
                    serde_json::json!({"time":crate::unix_now(),"plan":plan.id,"action":"move_pending","trash":plan.action==Action::Trash,"source":entry.native,"destination":encode(&destination),"inode":entry.identity.inode,"device":entry.identity.device}),
                )?;
                #[cfg(target_os = "linux")]
                if plan.action == Action::Trash {
                    trash_info(&path, &destination)?;
                }
                if let Err(error) = rename_exclusive(&path, &destination) {
                    if plan.action == Action::Trash {
                        remove_trash_info(&destination);
                    }
                    return Err(error);
                }
                changed = true;
                write_journal(
                    state,
                    serde_json::json!({"time":crate::unix_now(),"plan":plan.id,"action":"move","trash":plan.action==Action::Trash,"source":entry.native,"destination":encode(&destination),"inode":entry.identity.inode,"device":entry.identity.device}),
                )
                .with_context(|| {
                    format!(
                        "Moved to {}, but completion journal could not be saved; pending recovery record retained",
                        crate::text(&destination)
                    )
                })?;
                Ok(0)
            }
        };
        match execute() {
            Ok(bytes) => {
                result.completed.push(entry.path.clone());
                result.allocated_removed = result.allocated_removed.saturating_add(bytes);
            }
            Err(error) => {
                result.needs_reconcile |= changed;
                result.blocked.push(Blocked {
                    path: entry.path.clone(),
                    reason: format!("{error:#}"),
                });
            }
        }
    }
    if result.needs_reconcile {
        result.note = "Some paths changed before an error. Refresh the index and inspect the operation details before retrying.".into();
    }
    fs::write(
        state.join("plans").join(format!("{}.done", plan.id)),
        serde_json::to_vec(&result)?,
    )?;
    Ok(result)
}

pub fn undo(state: &Path) -> Result<serde_json::Value> {
    undo_in(state, None)
}

pub fn undo_in(state: &Path, scope: Option<&Path>) -> Result<serde_json::Value> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state.join("operations.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock)?;
    let data =
        fs::read_to_string(state.join("operations.jsonl")).context("No previous move to undo")?;
    let events: Vec<serde_json::Value> = data
        .lines()
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect();
    let undone: std::collections::HashSet<_> = events
        .iter()
        .filter(|v| v["action"] == "undo")
        .filter_map(|v| v["destination"].as_str())
        .collect();
    for event in events
        .iter()
        .rev()
        .filter(|v| v["action"] == "move" || v["action"] == "move_pending")
    {
        let destination = event["destination"].as_str().context("Bad journal")?;
        if undone.contains(destination) {
            continue;
        }
        let source = decode(event["source"].as_str().context("Bad journal")?)?;
        let current = decode(destination)?;
        if let Some(scope) = scope {
            ensure!(
                source.starts_with(scope)
                    && (current.starts_with(scope)
                        || (event["trash"] == true
                            && current.parent() == Some(trash_base()?.as_path()))),
                "Last move belongs to a different scope; undo it in the TUI or CLI"
            );
        }
        let m = match fs::symlink_metadata(&current) {
            Ok(m) => m,
            Err(_) if event["action"] == "move_pending" => continue,
            Err(error) => return Err(error.into()),
        };
        if event["action"] == "move_pending"
            && (m.ino() != event["inode"].as_u64().unwrap_or(0)
                || m.dev() != event["device"].as_u64().unwrap_or(0))
        {
            continue;
        }
        ensure!(
            m.ino() == event["inode"].as_u64().unwrap_or(0)
                && m.dev() == event["device"].as_u64().unwrap_or(0),
            "Moved item changed; undo refused"
        );
        for p in source.parent().context("Missing parent")?.ancestors() {
            ensure!(
                !fs::symlink_metadata(p)?.file_type().is_symlink(),
                "Restore path changed"
            );
        }
        rename_exclusive(&current, &source)?;
        if event["trash"] == true {
            remove_trash_info(&current);
        }
        journal(
            state,
            serde_json::json!({"action":"undo","time":crate::unix_now(),"destination":destination,"source":event["source"]}),
        )?;
        return Ok(serde_json::json!({"restored":crate::text(&source)}));
    }
    bail!("No reversible move remains; permanent cache cleanup cannot be undone")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoPolicy {
    pub enabled: bool,
    pub roots: Vec<String>,
    pub minimum_age_days: u32,
    pub max_bytes: u64,
    pub below_free_bytes: u64,
    pub cooldown_secs: i64,
    pub last_run: i64,
    pub last_checked: i64,
}
impl Default for AutoPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            roots: Vec::new(),
            minimum_age_days: 7,
            max_bytes: 2 * 1024 * 1024 * 1024,
            below_free_bytes: 20 * 1024 * 1024 * 1024,
            cooldown_secs: 86400,
            last_run: 0,
            last_checked: 0,
        }
    }
}

pub fn auto_policy(state: &Path) -> Result<AutoPolicy> {
    match fs::read(state.join("auto.json")) {
        Ok(b) => Ok(serde_json::from_slice(&b)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AutoPolicy::default()),
        Err(e) => Err(e.into()),
    }
}

pub fn save_auto(state: &Path, policy: &AutoPolicy) -> Result<()> {
    validate_auto_limits(policy)?;
    for root in &policy.roots {
        auto_root(root)?;
    }
    persist_auto(state, policy)
}

fn auto_root(root: &str) -> Result<PathBuf> {
    let root = Path::new(root).canonicalize()?;
    let normalized = root
        .strip_prefix("/System/Volumes/Data")
        .map(|p| Path::new("/").join(p))
        .unwrap_or(root.clone());
    ensure!(
        normalized != crate::home()?.canonicalize()? && normalized.components().count() > 3,
        "Choose a project/cache folder, not an entire home or disk"
    );
    Ok(root)
}

fn validate_auto_limits(policy: &AutoPolicy) -> Result<()> {
    ensure!(
        policy.minimum_age_days >= 1
            && policy.max_bytes <= 20 * 1024 * 1024 * 1024
            && policy.cooldown_secs >= 3600,
        "Automatic cleanup limits are too broad"
    );
    ensure!(
        !policy.enabled || !policy.roots.is_empty(),
        "Choose explicit project/cache roots before enabling automatic cleanup"
    );
    Ok(())
}

// Persist runtime timestamps without dropping configured roots that went offline.
fn persist_auto(state: &Path, policy: &AutoPolicy) -> Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(state)?;
    temporary.write_all(&serde_json::to_vec_pretty(policy)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(state.join("auto.json"))?;
    Ok(())
}

pub fn auto_run(state: &Path, index: &Index) -> Result<Option<Applied>> {
    // Serialize the policy check and reservation across CLI/TUI/watch processes.
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state.join("auto.lock"))?;
    match fs2::FileExt::try_lock_exclusive(&lock) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let mut policy = auto_policy(state)?;
    validate_auto_limits(&policy)?;
    let now = crate::unix_now();
    if !policy.enabled
        || now.saturating_sub(policy.last_run) < policy.cooldown_secs
        || now.saturating_sub(policy.last_checked) < 300
    {
        return Ok(None);
    }
    let scope = index.path(0)?;
    if crate::system::volume(&scope)?.available >= policy.below_free_bytes {
        return Ok(None);
    }
    let roots: Vec<_> = policy
        .roots
        .iter()
        .filter_map(|p| {
            let path = auto_root(p).ok()?;
            crate::scoped_alias(&path, &scope).ok()
        })
        .collect();
    if roots.is_empty() {
        return Ok(None);
    }
    let mut candidates: Vec<_> = cache_candidates(index)
        .iter()
        .copied()
        .filter(|id| {
            index.entry(*id).is_some_and(|e| {
                e.allocated <= policy.max_bytes
                    && now.saturating_sub(e.modified) >= i64::from(policy.minimum_age_days) * 86400
            })
        })
        .collect();
    candidates.sort_unstable_by_key(|id| {
        (
            std::cmp::Reverse(index.entry(*id).map_or(0, |e| e.allocated)),
            *id,
        )
    });
    let mut ids = Vec::new();
    for id in candidates {
        let path = index.path(id)?;
        if roots.iter().any(|root| path.starts_with(root)) {
            ids.push(id);
        }
        if ids.len() >= 64 {
            break;
        }
    }
    policy.last_checked = now;
    persist_auto(state, &policy)?;
    if ids.is_empty() {
        return Ok(None);
    }
    let plan = plan_with_budget(
        index,
        &ids,
        Action::Clean,
        state,
        policy.minimum_age_days as i64 * 86400,
        Some(PlanBudget {
            bytes: policy.max_bytes,
            entries: 16,
        }),
    )?;
    if plan.entries.is_empty() {
        return Ok(Some(Applied {
            plan: plan.id,
            completed: vec![],
            blocked: plan.blocked,
            allocated_removed: 0,
            needs_reconcile: false,
            note: "No eligible caches passed the current checks".into(),
        }));
    }
    ensure!(
        plan.total_bytes <= policy.max_bytes,
        "Automatic cleanup budget exceeded"
    );
    // Reserve the daily allowance durably before any mutation; errors retain it.
    let previous_run = policy.last_run;
    policy.last_run = now;
    persist_auto(state, &policy)?;
    let result = apply(state, &plan, "CLEAN")?;
    if result.completed.is_empty() && !result.needs_reconcile {
        policy.last_run = previous_run;
        persist_auto(state, &policy)?;
    }
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_destructive_claim_gets_a_fresh_process_check() {
        use std::{cell::Cell, rc::Rc};
        let calls = Rc::new(Cell::new(0));
        let count = calls.clone();
        let mut checks = CleanChecks::new(move || {
            count.set(count.get() + 1);
            let mut open = OpenFiles::default();
            open.paths.insert(PathBuf::from(if count.get() == 3 {
                "/fixture/claimed/live"
            } else {
                "/unrelated"
            }));
            Ok(open)
        });
        checks.before(Path::new("/fixture/cache"), None).unwrap();
        checks
            .after_claim(Path::new("/fixture/claimed"), None)
            .unwrap();
        checks.before(Path::new("/fixture/cache"), None).unwrap();
        assert_eq!(
            calls.get(),
            2,
            "An immediate advisory precheck should reuse the prior snapshot"
        );
        assert!(
            checks
                .after_claim(Path::new("/fixture/claimed"), None)
                .is_err()
        );
        assert_eq!(
            calls.get(),
            3,
            "The final check must capture new active files"
        );
        checks.recent.as_mut().unwrap().0 = Instant::now() - Duration::from_secs(1);
        checks.before(Path::new("/fixture/cache"), None).unwrap();
        assert_eq!(calls.get(), 4, "Expired snapshots must not be reused");
    }

    #[test]
    fn budgeted_plans_skip_large_entries_and_keep_the_fitting_ones() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("files");
        let state = root.with_file_name("state");
        fs::create_dir_all(root.join("destination")).unwrap();
        private_dir(&state).unwrap();
        for (name, bytes) in [("large", 65536), ("small-a", 4096), ("small-b", 4096)] {
            fs::write(root.join(name), vec![0; bytes]).unwrap();
        }
        let path = crate::index::scan(
            &root,
            &state,
            std::sync::Arc::new(std::sync::Mutex::new(crate::index::Progress::default())),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap();
        let index = Index::open(&path).unwrap();
        let ids: Vec<_> = ["large", "small-a", "small-b"]
            .iter()
            .map(|name| index.find_path(&root.join(name)).unwrap())
            .collect();
        let budget = ids[1..]
            .iter()
            .map(|id| index.entry(*id).unwrap().allocated)
            .sum();
        let result = plan_with_budget(
            &index,
            &ids,
            Action::Move {
                destination: root.join("destination").to_string_lossy().into(),
            },
            &state,
            0,
            Some(PlanBudget {
                bytes: budget,
                entries: 2,
            }),
        )
        .unwrap();
        assert_eq!(result.entries.len(), 2);
        assert_eq!(result.total_bytes, budget);
        assert!(result.entries.iter().all(|entry| entry.id != ids[0]));
        assert!(
            result
                .blocked
                .iter()
                .any(|entry| entry.reason.contains("budget"))
        );
        assert!(root.join("large").exists() && root.join("small-a").exists());
    }

    #[test]
    fn journal_failures_distinguish_unchanged_rejection_from_recoverable_move() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let root = base.join("files");
        let state = base.join("state");
        fs::create_dir_all(root.join("destination")).unwrap();
        private_dir(&state).unwrap();
        let source = root.join("source.txt");
        let destination = root.join("destination/source.txt");
        fs::write(&source, b"keep").unwrap();
        let path = crate::index::scan(
            &root,
            &state,
            std::sync::Arc::new(std::sync::Mutex::new(crate::index::Progress::default())),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap();
        let index = Index::open(&path).unwrap();
        let id = index.find_path(&source).unwrap();
        let action = Action::Move {
            destination: root.join("destination").to_string_lossy().into(),
        };
        let preview = plan(&index, &[id], action.clone(), &state, 0).unwrap();
        let rejected = apply_with_journal(&state, &preview, "MOVE", |_, _| {
            bail!("Injected pre-move journal failure")
        })
        .unwrap();
        assert!(!rejected.needs_reconcile);
        assert!(source.exists() && !destination.exists());

        let preview = plan(&index, &[id], action, &state, 0).unwrap();
        let uncertain = apply_with_journal(&state, &preview, "MOVE", |state, value| {
            if value["action"] == "move" {
                bail!("Injected completion journal failure");
            }
            journal(state, value)
        })
        .unwrap();
        assert!(uncertain.completed.is_empty());
        assert_eq!(uncertain.blocked.len(), 1);
        assert!(uncertain.needs_reconcile);
        assert!(uncertain.blocked[0].reason.contains("Moved to"));
        assert!(!source.exists() && destination.exists());
        assert!(
            fs::read_to_string(state.join("plans").join(format!("{}.done", preview.id)))
                .unwrap()
                .contains("\"needs_reconcile\":true")
        );
        undo(&state).unwrap();
        assert_eq!(fs::read(source).unwrap(), b"keep");
        assert!(!destination.exists());
    }

    #[test]
    fn freedesktop_metadata_stays_inside_the_private_recovery_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("recovery");
        recovery_dir(&root.join("files")).unwrap();
        let source = temp.path().join("source #.txt");
        fs::write(&source, b"keep").unwrap();
        let outside = temp.path().join("info");
        fs::create_dir(&outside).unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).unwrap();
        let info = trash_info(&source, &root.join("files/axit-example")).unwrap();
        assert_eq!(info, root.join("info/axit-example.trashinfo"));
        let text = fs::read_to_string(&info).unwrap();
        assert!(text.starts_with("[Trash Info]\nPath="));
        assert!(text.contains("source%20%23.txt\nDeletionDate="));
        assert_eq!(fs::metadata(info).unwrap().mode() & 0o077, 0);
        assert_eq!(fs::metadata(outside).unwrap().mode() & 0o777, 0o755);
        assert_eq!(fs::read(source).unwrap(), b"keep");
    }

    #[test]
    fn trash_metadata_rejects_shared_or_symlinked_folders_without_chmod() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("recovery");
        recovery_dir(&root.join("files")).unwrap();
        let source = temp.path().join("source.txt");
        fs::write(&source, b"keep").unwrap();
        let info = root.join("info");
        fs::create_dir(&info).unwrap();
        fs::set_permissions(&info, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(trash_info(&source, &root.join("files/axit-example")).is_err());
        assert_eq!(fs::metadata(&info).unwrap().mode() & 0o777, 0o755);
        fs::remove_dir(&info).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&outside, &info).unwrap();
        assert!(trash_info(&source, &root.join("files/axit-example")).is_err());
        assert_eq!(fs::metadata(outside).unwrap().mode() & 0o777, 0o755);
        assert_eq!(fs::read(source).unwrap(), b"keep");
    }
}
