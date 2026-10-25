use super::*;
use anyhow::{Context as _, Result, ensure};
use std::{
    collections::HashSet,
    fs,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, Instant},
};

const MAX_METADATA: u64 = 2 * 1024 * 1024;
const MAX_APPS: usize = 4096;

#[derive(Clone)]
pub struct Context {
    pub home: PathBuf,
    pub app_roots: Vec<PathBuf>,
    pub desktop_roots: Vec<PathBuf>,
    pub tools: ManagedTools,
    /// Optional fixed filesystem scope, used by protocol clients.
    pub scope: Option<PathBuf>,
}

impl Context {
    pub fn system() -> Result<Self> {
        let home = crate::home()?.canonicalize()?;
        let mut desktop_roots = vec![];
        if cfg!(target_os = "linux") {
            let data = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(".local/share"));
            desktop_roots.push(data.join("applications"));
            let dirs = std::env::var_os("XDG_DATA_DIRS")
                .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
            desktop_roots.extend(
                std::env::split_paths(&dirs)
                    .filter(|p| p.is_absolute())
                    .map(|p| p.join("applications")),
            );
            desktop_roots.push(home.join(".local/share/flatpak/exports/share/applications"));
            desktop_roots.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
        }
        desktop_roots.truncate(128);
        Ok(Self {
            app_roots: if cfg!(target_os = "macos") {
                vec![
                    PathBuf::from("/Applications"),
                    home.join("Applications"),
                    PathBuf::from("/System/Applications"),
                ]
            } else {
                vec![]
            },
            home,
            desktop_roots,
            tools: ManagedTools::system(),
            scope: None,
        })
    }

    pub fn constrained(mut self, root: &Path) -> Result<Self> {
        let root = root.canonicalize()?;
        let restrict = |roots: Vec<PathBuf>| {
            roots
                .into_iter()
                .filter_map(|p| {
                    let p = p.canonicalize().ok()?;
                    if p.starts_with(&root) {
                        Some(p)
                    } else if root.starts_with(&p) {
                        Some(root.clone())
                    } else {
                        None
                    }
                })
                .collect()
        };
        self.app_roots = restrict(self.app_roots);
        self.desktop_roots = restrict(self.desktop_roots);
        if !self.app_roots.contains(&root) {
            self.app_roots.push(root.clone());
        }
        if !self.desktop_roots.contains(&root) {
            self.desktop_roots.push(root.clone());
        }
        // Native package inventories are global. Scoped servers inspect only local metadata.
        self.tools = ManagedTools::default();
        self.scope = Some(root);
        Ok(self)
    }

    pub(crate) fn allows(&self, path: &Path) -> bool {
        path.to_str().is_some()
            && self.scope.as_ref().is_none_or(|root| {
                path.starts_with(root) && path.canonicalize().is_ok_and(|p| p.starts_with(root))
            })
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Catalog {
    pub apps: Vec<Application>,
    pub warnings: Vec<String>,
}

fn bounded_metadata(path: &Path) -> Result<Vec<u8>> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "Application metadata is not a regular file"
    );
    let mut bytes = vec![];
    file.take(MAX_METADATA + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_METADATA,
        "Application metadata exceeds 2 MiB"
    );
    Ok(bytes)
}

/// Read XML or binary Info.plist without running the app or invoking a shell.
pub fn read_bundle(path: &Path) -> Result<Application> {
    ensure!(path.to_str().is_some(), "Application path is not UTF-8");
    ensure!(
        path.extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("app")),
        "Choose an .app bundle"
    );
    ensure!(
        fs::metadata(path)?.is_dir(),
        "Application bundle is not a directory"
    );
    let bytes = bounded_metadata(&path.join("Contents/Info.plist"))?;
    let plist = plist::Value::from_reader(std::io::Cursor::new(bytes))?;
    let dictionary = plist
        .as_dictionary()
        .context("Invalid application property list")?;
    let value = |key: &str| dictionary.get(key).and_then(plist::Value::as_string);
    let id = value("CFBundleIdentifier").context("Application has no bundle identifier")?;
    ensure!(valid_app_id(id), "Invalid application identifier");
    let fallback = path.file_stem().and_then(|x| x.to_str()).unwrap_or(id);
    let name = value("CFBundleDisplayName")
        .or_else(|| value("CFBundleName"))
        .filter(|x| safe_component(x))
        .unwrap_or(fallback);
    Ok(Application {
        id: id.into(),
        name: crate::clean(name),
        version: value("CFBundleShortVersionString")
            .or_else(|| value("CFBundleVersion"))
            .map(|x| crate::clean(&x.chars().take(128).collect::<String>())),
        path: Some(path.to_path_buf()),
        source: Source::Bundle,
        protected: path
            .strip_prefix("/System/Volumes/Data")
            .map(|p| Path::new("/").join(p))
            .unwrap_or_else(|_| path.to_path_buf())
            .starts_with("/System"),
        related_hints: vec![],
        remembered: false,
    })
}

fn desktop(path: &Path) -> Result<Application> {
    let bytes = bounded_metadata(path)?;
    let content = std::str::from_utf8(&bytes)?;
    let mut active = false;
    let mut fields = std::collections::HashMap::new();
    for line in content.lines().map(str::trim) {
        if line.starts_with('[') {
            active = line == "[Desktop Entry]";
            continue;
        }
        if active
            && !line.starts_with('#')
            && let Some((key, value)) = line.split_once('=')
        {
            fields.insert(key.trim(), value.trim());
        }
    }
    ensure!(
        fields.get("Type") == Some(&"Application")
            && fields.get("Hidden") != Some(&"true")
            && fields.get("NoDisplay") != Some(&"true"),
        "Not a visible application launcher"
    );
    let stem = path
        .file_stem()
        .and_then(|x| x.to_str())
        .context("Invalid launcher name")?;
    let id = fields.get("X-Flatpak").copied().unwrap_or(stem);
    ensure!(safe_component(id), "Invalid launcher identifier");
    Ok(Application {
        id: id.into(),
        name: crate::clean(
            &fields
                .get("Name")
                .copied()
                .unwrap_or(stem)
                .chars()
                .take(255)
                .collect::<String>(),
        ),
        version: None,
        path: Some(path.to_path_buf()),
        source: Source::Desktop,
        protected: true,
        related_hints: vec![],
        remembered: false,
    })
}

/// Shallow platform inventory: bundle contents and the rest of the disk are not traversed.
pub fn inventory(context: &Context) -> Catalog {
    let mut catalog = Catalog::default();
    let started = Instant::now();
    let mut remaining = 16384usize;
    let mut seen = HashSet::new();
    for root in &context.app_roots {
        let mut dirs = vec![(root.clone(), 0)];
        while let Some((dir, depth)) = dirs.pop() {
            if remaining == 0
                || started.elapsed() > Duration::from_secs(10)
                || catalog.apps.len() >= MAX_APPS
            {
                if catalog.warnings.len() < 64 {
                    catalog
                        .warnings
                        .push("Application inventory limit reached; some apps are omitted".into());
                }
                break;
            }
            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    if catalog.warnings.len() < 64 {
                        catalog
                            .warnings
                            .push(format!("{}: {error}", crate::text(&dir)));
                    }
                    continue;
                }
            };
            for entry in entries.take(remaining).flatten() {
                remaining = remaining.saturating_sub(1);
                if catalog.apps.len() >= MAX_APPS {
                    break;
                }
                let path = entry.path();
                if !context.allows(&path) {
                    continue;
                }
                if path
                    .extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("app"))
                {
                    if !context.allows(&path.join("Contents/Info.plist")) {
                        continue;
                    }
                    match read_bundle(&path) {
                        Ok(app) if seen.insert(path.clone()) => catalog.apps.push(app),
                        Ok(_) => {}
                        Err(error) if catalog.warnings.len() < 64 => catalog
                            .warnings
                            .push(format!("{}: {error}", crate::text(&path))),
                        Err(_) => {}
                    }
                } else if depth < 1 && entry.file_type().is_ok_and(|x| x.is_dir()) {
                    dirs.push((path, depth + 1));
                }
            }
        }
    }
    let mut desktop_ids = HashSet::new();
    for root in &context.desktop_roots {
        if remaining == 0
            || started.elapsed() > Duration::from_secs(10)
            || catalog.apps.len() >= MAX_APPS
        {
            if catalog.warnings.len() < 64 {
                catalog
                    .warnings
                    .push("Application inventory limit reached; some launchers are omitted".into());
            }
            break;
        }
        if !context.allows(root) {
            continue;
        }
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.take(remaining.min(MAX_APPS)).flatten() {
            remaining = remaining.saturating_sub(1);
            if catalog.apps.len() >= MAX_APPS {
                break;
            }
            let path = entry.path();
            if !context.allows(&path) {
                continue;
            }
            if path.extension().is_some_and(|x| x == "desktop")
                && let Ok(mut app) = desktop(&path)
                && desktop_ids.insert(app.id.to_ascii_lowercase())
            {
                app.protected = !path.starts_with(&context.home);
                catalog.apps.push(app);
            }
        }
    }
    super::managed::enrich(context, &mut catalog);
    catalog
        .apps
        .sort_by_cached_key(|app| (app.name.to_lowercase(), app.id.to_lowercase()));
    catalog.warnings.truncate(64);
    catalog
}
