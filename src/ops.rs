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
    pub note: String,
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

fn cache_project(path: &Path) -> Result<Option<PathBuf>> {
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
        git_generated(path, project)?;
        Ok(Some(project.to_path_buf()))
    } else if label.starts_with("Android") || label.contains("Gradle") {
        let project = path
            .ancestors()
            .find(|p| {
                p.join("settings.gradle.kts").is_file() || p.join("settings.gradle").is_file()
            })
            .context("Gradle project marker missing")?;
        git_generated(path, project)?;
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
    mut action: Action,
    state: &Path,
    min_age: i64,
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
    for (id, path) in paths {
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
                let project = cache_project(&path)?;
                ensure!(
                    !open.in_use(&path, project.as_deref()),
                    "In use by a running process or build"
                );
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
                plan.total_bytes = plan.total_bytes.saturating_add(e.bytes);
                plan.entries.push(e);
            }
            Err(error) => plan.blocked.push(Blocked {
                path: crate::text(&path),
                reason: error.to_string(),
            }),
        }
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
        note: if plan.action == Action::Trash {
            "Moved to Trash. This does not free disk space until Trash is emptied. Undo is available.".into()
        } else if plan.action == Action::Clean {
            "Verified disposable caches removed permanently. Allocated sizes may differ from space recovered.".into()
        } else {
            "Moved without overwriting existing files. Undo is available.".into()
        },
    };
    for entry in &plan.entries {
        let execute = || -> Result<u64> {
            let path = decode(&entry.native)?;
            let metadata = identity_path(&path, &root)?;
            ensure!(
                entry.identity.matches(&metadata),
                "File changed since preview"
            );
            let open = OpenFiles::capture()?;
            let project = if plan.action == Action::Clean {
                cache_project(&path)?
            } else {
                None
            };
            ensure!(
                !open.in_use(&path, project.as_deref()),
                "File or project is in use"
            );
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
                let check = || -> Result<()> {
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
                    ensure!(
                        !OpenFiles::capture()?.in_use(&claimed, project.as_deref()),
                        "Cache became active"
                    );
                    Ok(())
                };
                if let Err(error) = check() {
                    let restored = rename_exclusive(&claimed, &path);
                    return Err(error.context(if restored.is_ok() {
                        "Cache restored; nothing deleted"
                    } else {
                        "Cache retained under .axit-clean name; restore original path manually"
                    }));
                }
                if let Err(error) = journal(
                    state,
                    serde_json::json!({"time":crate::unix_now(),"plan":plan.id,"action":"clean_started","source":entry.native,"claimed":encode(&claimed)}),
                ) {
                    let restored = rename_exclusive(&claimed, &path);
                    return Err(error.context(if restored.is_ok() {
                        "Audit could not be saved; cache restored"
                    } else {
                        "Cache retained under .axit-clean name; nothing deleted"
                    }));
                }
                fs::remove_dir_all(&claimed)?;
                journal(
                    state,
                    serde_json::json!({"time":crate::unix_now(),"plan":plan.id,"action":"clean","source":entry.native,"bytes":entry.bytes}),
                )?;
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
                journal(
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
                journal(
                    state,
                    serde_json::json!({"time":crate::unix_now(),"plan":plan.id,"action":"move","trash":plan.action==Action::Trash,"source":entry.native,"destination":encode(&destination),"inode":entry.identity.inode,"device":entry.identity.device}),
                )?;
                Ok(0)
            }
        };
        match execute() {
            Ok(bytes) => {
                result.completed.push(entry.path.clone());
                result.allocated_removed = result.allocated_removed.saturating_add(bytes);
            }
            Err(error) => result.blocked.push(Blocked {
                path: entry.path.clone(),
                reason: format!("{error:#}"),
            }),
        }
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
    for root in &policy.roots {
        let root = Path::new(root).canonicalize()?;
        let normalized = root
            .strip_prefix("/System/Volumes/Data")
            .map(|p| Path::new("/").join(p))
            .unwrap_or(root.clone());
        ensure!(
            normalized != crate::home()?.canonicalize()? && normalized.components().count() > 3,
            "Choose a project/cache folder, not an entire home or disk"
        );
    }
    let mut temporary = tempfile::NamedTempFile::new_in(state)?;
    temporary.write_all(&serde_json::to_vec_pretty(policy)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(state.join("auto.json"))?;
    Ok(())
}

pub fn auto_run(state: &Path, index: &Index) -> Result<Option<Applied>> {
    let mut policy = auto_policy(state)?;
    if !policy.enabled || crate::unix_now() - policy.last_run < policy.cooldown_secs {
        return Ok(None);
    }
    if crate::system::volume(&index.path(0)?)?.available >= policy.below_free_bytes {
        return Ok(None);
    }
    let roots: Vec<_> = policy
        .roots
        .iter()
        .map(|p| Path::new(p).canonicalize())
        .collect::<std::io::Result<_>>()?;
    let mut ids = Vec::new();
    let mut estimate = 0;
    for id in 1..index.count {
        let e = index.entry(id).context("Missing entry")?;
        if !e.is_dir()
            || e.allocated == 0
            || e.allocated > policy.max_bytes.saturating_sub(estimate)
        {
            continue;
        }
        if !cache_candidate(index, e) {
            continue;
        }
        let path = index.path(id)?;
        if roots.iter().any(|root| path.starts_with(root))
            && !ids.iter().any(|parent| index.is_descendant(id, *parent))
        {
            ids.push(id);
            estimate += e.allocated;
        }
        if ids.len() >= 16 {
            break;
        }
    }
    policy.last_run = crate::unix_now();
    save_auto(state, &policy)?;
    if ids.is_empty() {
        return Ok(None);
    }
    let plan = plan(
        index,
        &ids,
        Action::Clean,
        state,
        policy.minimum_age_days as i64 * 86400,
    )?;
    if plan.entries.is_empty() || plan.total_bytes > policy.max_bytes {
        return Ok(None);
    }
    Ok(Some(apply(state, &plan, "CLEAN")?))
}

#[cfg(test)]
mod tests {
    use super::*;

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
