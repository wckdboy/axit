use super::*;
use anyhow::{Context as _, Result, bail, ensure};
use std::{
    fs,
    io::{self, IsTerminal},
    os::unix::fs::MetadataExt,
    path::Path,
    process::Command,
    time::Duration,
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Brew,
    Flatpak,
}
impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Self::Brew => "Homebrew",
            Self::Flatpak => "Flatpak",
        }
    }
}
impl std::str::FromStr for Provider {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "brew" => Ok(Self::Brew),
            "flatpak" => Ok(Self::Flatpak),
            _ => Err("Choose brew or flatpak".into()),
        }
    }
}

#[derive(Clone, Default)]
pub struct ManagedTools {
    pub brew: Option<PathBuf>,
    pub flatpak: Option<PathBuf>,
}
impl ManagedTools {
    pub fn system() -> Self {
        let executable = |paths: &[&str]| {
            paths.iter().find_map(|p| {
                let path = Path::new(p);
                let m = fs::metadata(path).ok()?;
                (m.is_file() && m.mode() & 0o111 != 0)
                    .then(|| path.canonicalize().ok())
                    .flatten()
            })
        };
        Self {
            brew: if cfg!(target_os = "macos") {
                executable(&["/opt/homebrew/bin/brew", "/usr/local/bin/brew"])
            } else {
                None
            },
            flatpak: if cfg!(target_os = "linux") {
                executable(&["/usr/bin/flatpak", "/usr/local/bin/flatpak"])
            } else {
                None
            },
        }
    }
    pub fn available(&self) -> Vec<Provider> {
        [
            (Provider::Brew, &self.brew),
            (Provider::Flatpak, &self.flatpak),
        ]
        .into_iter()
        .filter_map(|(p, path)| path.as_ref().map(|_| p))
        .collect()
    }
    fn path(&self, provider: Provider) -> Result<&Path> {
        match provider {
            Provider::Brew => self.brew.as_deref(),
            Provider::Flatpak => self.flatpak.as_deref(),
        }
        .with_context(|| {
            format!(
                "{} is not installed; install its native package manager first",
                provider.label()
            )
        })
    }
}

fn valid_token(value: &str) -> bool {
    (1..=255).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'+' | b'@'))
}
fn valid_flatpak_ref(reference: &str) -> bool {
    let parts: Vec<_> = reference.split('/').collect();
    parts.len() == 4
        && parts[0] == "app"
        && valid_app_id(parts[1])
        && parts[1].split('.').count() >= 3
        && valid_token(parts[2])
        && valid_token(parts[3])
}

pub(super) fn enrich(context: &Context, catalog: &mut Catalog) {
    if let Some(program) = &context.tools.brew {
        let result = super::command::query(
            program,
            &["info", "--json=v2", "--installed", "--cask"],
            Duration::from_secs(15),
        )
        .and_then(|data| Ok(serde_json::from_slice::<serde_json::Value>(&data)?))
        .and_then(|json| parse_brew(context, &json));
        match result {
            Ok(apps) => {
                for app in apps {
                    if let Some(existing) = catalog
                        .apps
                        .iter_mut()
                        .find(|old| same_path(old.path.as_deref(), app.path.as_deref()))
                    {
                        existing.source = app.source;
                        existing.related_hints = app.related_hints;
                    } else if catalog.apps.len() < 4096 {
                        catalog.apps.push(app);
                    }
                }
            }
            Err(e) => {
                catalog.warnings.push(format!("Homebrew inventory unavailable; bundle removal is disabled until ownership can be checked: {e}"));
                for app in &mut catalog.apps {
                    if app.source == Source::Bundle {
                        app.protected = true;
                    }
                }
            }
        }
    }
    if let Some(program) = &context.tools.flatpak {
        match super::command::query(
            program,
            &["list", "--app", "--columns=ref,name,installation"],
            Duration::from_secs(10),
        ) {
            Ok(data) => {
                for app in parse_flatpak(&String::from_utf8_lossy(&data)) {
                    if let Some(existing) = catalog
                        .apps
                        .iter_mut()
                        .find(|old| old.id == app.id && old.source == Source::Desktop)
                    {
                        existing.source = app.source;
                        existing.protected = false;
                    } else if catalog.apps.len() < 4096 {
                        catalog.apps.push(app);
                    }
                }
            }
            Err(e) => catalog.warnings.push(format!("Flatpak: {e}")),
        }
    }
}
fn same_path(a: Option<&Path>, b: Option<&Path>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            if a == b {
                return true;
            }
            fs::metadata(a)
                .ok()
                .zip(fs::metadata(b).ok())
                .is_some_and(|(a, b)| a.dev() == b.dev() && a.ino() == b.ino())
        }
        _ => false,
    }
}
pub(super) fn parse_brew(context: &Context, json: &serde_json::Value) -> Result<Vec<Application>> {
    let casks = json["casks"]
        .as_array()
        .context("Invalid Homebrew metadata")?;
    let mut apps = vec![];
    for cask in casks.iter().take(4096) {
        let Some(token) = cask["token"].as_str().filter(|s| valid_token(s)) else {
            continue;
        };
        let mut hints = vec![];
        let artifacts = cask["artifacts"]
            .as_array()
            .map(|a| &a[..a.len().min(256)])
            .unwrap_or(&[]);
        for artifact in artifacts {
            if let Some(zaps) = artifact["zap"].as_array() {
                for zap in zaps {
                    if let Some(paths) = zap["trash"].as_array() {
                        for path in paths.iter().filter_map(|v| v.as_str()).take(128) {
                            if let Some(relative) = path.strip_prefix("~/") {
                                let p = Path::new(relative);
                                if hints.len() < 64
                                    && relative.len() <= 4096
                                    && !relative.contains(['*', '?', '['])
                                    && p.components()
                                        .all(|c| matches!(c, std::path::Component::Normal(_)))
                                {
                                    hints.push(context.home.join(p));
                                }
                            }
                        }
                    }
                }
            }
        }
        let mut found = false;
        for artifact in artifacts {
            if apps.len() >= 4096 {
                return Ok(apps);
            }
            let Some(appname) = artifact["app"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|v| v.as_str())
            else {
                continue;
            };
            let target = artifact["app"]
                .as_array()
                .and_then(|a| a.get(1))
                .and_then(|options| options["target"].as_str())
                .or_else(|| artifact["target"].as_str())
                .map(PathBuf::from)
                .map(|p| {
                    if p.is_absolute() {
                        p
                    } else {
                        Path::new("/Applications").join(p)
                    }
                })
                .unwrap_or_else(|| Path::new("/Applications").join(appname));
            if let Ok(mut app) = read_bundle(&target) {
                app.source = Source::Brew {
                    token: token.into(),
                };
                app.related_hints = hints.clone();
                apps.push(app);
                found = true;
            }
        }
        if !found {
            if apps.len() >= 4096 {
                return Ok(apps);
            }
            apps.push(Application {
                id: format!("brew:{token}"),
                name: cask["name"]
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str())
                    .map(|s| crate::clean(&s.chars().take(255).collect::<String>()))
                    .unwrap_or(token.into()),
                version: cask["installed"]
                    .as_str()
                    .map(|s| crate::clean(&s.chars().take(128).collect::<String>())),
                path: None,
                source: Source::Brew {
                    token: token.into(),
                },
                protected: false,
                related_hints: hints,
                remembered: false,
            });
        }
    }
    Ok(apps)
}
pub(super) fn parse_flatpak(text: &str) -> Vec<Application> {
    text.lines()
        .take(4096)
        .filter_map(|line| {
            let parts: Vec<_> = line.split('\t').collect();
            // `list --columns=ref` emits ID/ARCH/BRANCH, without the app/ kind.
            let reference = if parts.first().is_some_and(|p| p.starts_with("app/")) {
                parts[0].to_string()
            } else {
                format!("app/{}", parts.first()?)
            };
            if parts.len() < 3
                || !valid_flatpak_ref(&reference)
                || !matches!(parts[2], "user" | "system")
            {
                return None;
            }
            Some(Application {
                id: reference.split('/').nth(1)?.into(),
                name: crate::clean(&parts[1].chars().take(255).collect::<String>()),
                version: None,
                path: None,
                source: Source::Flatpak {
                    reference,
                    user: parts[2] == "user",
                },
                protected: false,
                related_hints: vec![],
                remembered: false,
            })
        })
        .collect()
}

#[derive(Clone, Debug, Serialize)]
struct ToolStamp {
    path: PathBuf,
    device: u64,
    inode: u64,
    modified: i64,
    nanos: i64,
    changed: i64,
    changed_nanos: i64,
    size: u64,
}
impl ToolStamp {
    fn read(path: &Path) -> Result<Self> {
        let path = path.canonicalize()?;
        let m = fs::metadata(&path)?;
        ensure!(
            m.is_file() && m.mode() & 0o111 != 0,
            "Package manager is not executable"
        );
        Ok(Self {
            path,
            device: m.dev(),
            inode: m.ino(),
            modified: m.mtime(),
            nanos: m.mtime_nsec(),
            changed: m.ctime(),
            changed_nanos: m.ctime_nsec(),
            size: m.len(),
        })
    }
    fn validate(&self) -> Result<()> {
        let m = fs::metadata(&self.path)?;
        ensure!(
            m.dev() == self.device
                && m.ino() == self.inode
                && m.mtime() == self.modified
                && m.mtime_nsec() == self.nanos
                && m.ctime() == self.changed
                && m.ctime_nsec() == self.changed_nanos
                && m.len() == self.size,
            "Package manager changed since review; create a new preview"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct ManagedPlan {
    pub provider: Provider,
    pub target: String,
    pub uninstall: bool,
    pub command: String,
    pub note: String,
    created: i64,
    tool: ToolStamp,
    args: Vec<String>,
    active_bundle: Option<PathBuf>,
    subject: Option<Application>,
}
impl ManagedPlan {
    pub fn phrase(&self) -> &'static str {
        if self.uninstall {
            "UNINSTALL"
        } else {
            "INSTALL"
        }
    }
    fn new(
        tools: &ManagedTools,
        provider: Provider,
        target: &str,
        uninstall: bool,
        user: bool,
    ) -> Result<Self> {
        ensure!(
            (provider == Provider::Flatpak && uninstall && valid_flatpak_ref(target))
                || (!uninstall && valid_token(target))
                || (provider == Provider::Brew && valid_token(target)),
            "Invalid package identifier"
        );
        let tool = ToolStamp::read(tools.path(provider)?)?;
        let args: Vec<String> = match provider {
            Provider::Brew => vec![
                if uninstall { "uninstall" } else { "install" }.into(),
                "--cask".into(),
                "--".into(),
                target.into(),
            ],
            Provider::Flatpak => vec![
                if user { "--user" } else { "--system" }.into(),
                if uninstall { "uninstall" } else { "install" }.into(),
                "--".into(),
                target.into(),
            ],
        };
        let command = format!("{} {}", crate::text(&tool.path), args.join(" "));
        Ok(Self{provider,target:target.into(),uninstall,command,
            note:if uninstall{"Native package manager removes this package and its components. User data is kept; inspect leftovers separately. Axit Undo applies only to files moved to Trash."}else{"Native package manager verifies and installs the selected package. Its own prompts remain enabled."}.into(),
            created:crate::unix_now(),tool,args,active_bundle:None,subject:None})
    }
    fn validate(&self, confirmation: &str) -> Result<()> {
        ensure!(
            confirmation == self.phrase(),
            "Review the native command, then confirm with {}",
            self.phrase()
        );
        ensure!(
            (0..=600).contains(&(crate::unix_now() - self.created)),
            "Package preview expired"
        );
        self.tool.validate()
    }
    pub fn run(&self, state: &Path, confirmation: &str) -> Result<()> {
        self.validate(confirmation)?;
        ensure!(
            io::stdin().is_terminal() && io::stdout().is_terminal(),
            "Package operations require an interactive terminal for native prompts"
        );
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(state.join("operations.lock"))?;
        fs2::FileExt::try_lock_exclusive(&lock).context("Another Axit operation is running")?;
        if let Some(app) = &self.subject {
            remember(state, app)?;
        }
        if self.uninstall {
            if let Some(app) = &self.subject
                && self.provider == Provider::Brew
            {
                let open = crate::system::OpenFiles::capture()?;
                crate::apps::ensure_stopped_with(app, &open)?;
                // A cask can install several bundles. Check every bundle that the native command removes.
                let context = Context::system()?;
                let bytes = super::command::query(
                    &self.tool.path,
                    &["info", "--json=v2", "--installed", "--cask"],
                    Duration::from_secs(15),
                )?;
                let current = parse_brew(&context, &serde_json::from_slice(&bytes)?)?;
                ensure!(
                    current
                        .iter()
                        .any(|a| matches!(&a.source,Source::Brew{token} if token==&self.target)),
                    "Package ownership changed; refresh Apps before uninstalling"
                );
                for other in current
                    .iter()
                    .filter(|a| matches!(&a.source,Source::Brew{token} if token==&self.target))
                {
                    crate::apps::ensure_stopped_with(other, &open)?;
                }
            }
            if let Some(bundle) = &self.active_bundle {
                ensure!(
                    !crate::system::OpenFiles::capture()?.in_use(bundle, None),
                    "Quit the application before uninstalling it"
                );
            }
            if self.provider == Provider::Flatpak {
                let output = super::command::query(
                    &self.tool.path,
                    &["ps", "--columns=application"],
                    Duration::from_secs(5),
                )?;
                let id = self
                    .target
                    .split('/')
                    .nth(1)
                    .context("Invalid Flatpak reference")?;
                ensure!(
                    !String::from_utf8_lossy(&output)
                        .lines()
                        .any(|line| line.trim() == id),
                    "Quit the Flatpak application before uninstalling it"
                );
            }
        }
        let status = self
            .process()
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()?;
        ensure!(
            status.success(),
            "Native manager exited with {status}. It may have made partial changes; review its output before retrying."
        );
        Ok(())
    }
    fn process(&self) -> Command {
        let mut command = Command::new(&self.tool.path);
        command
            .args(&self.args)
            .env("HOMEBREW_NO_AUTO_UPDATE", "1")
            .env("HOMEBREW_NO_ANALYTICS", "1")
            .env("HOMEBREW_NO_INSTALL_CLEANUP", "1")
            .env("HOMEBREW_NO_AUTOREMOVE", "1");
        command
    }
}
pub fn install_plan(tools: &ManagedTools, provider: Provider, target: &str) -> Result<ManagedPlan> {
    ManagedPlan::new(tools, provider, target, false, true)
}
pub fn uninstall_plan(tools: &ManagedTools, app: &Application) -> Result<ManagedPlan> {
    let mut plan = match &app.source {
        Source::Brew { token } => ManagedPlan::new(tools, Provider::Brew, token, true, true),
        Source::Flatpak { reference, user } => {
            ManagedPlan::new(tools, Provider::Flatpak, reference, true, *user)
        }
        _ => bail!(
            "This app is not managed by Homebrew or Flatpak; review its standalone files instead"
        ),
    }?;
    plan.active_bundle = if matches!(app.source, Source::Brew { .. }) {
        app.path.clone()
    } else {
        None
    };
    plan.subject = Some(app.clone());
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn managed_plans_reject_injection_expiry_and_replaced_tools() {
        let temp = tempfile::tempdir().unwrap();
        let tool = temp.path().join("brew");
        fs::write(&tool, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();
        let tools = ManagedTools {
            brew: Some(tool.clone()),
            flatpak: None,
        };
        for value in [
            "--zap",
            "../escape",
            "alpha;touch marker",
            "https://bad/app.rb",
            "alpha\n--force",
        ] {
            assert!(install_plan(&tools, Provider::Brew, value).is_err());
        }
        let mut plan = install_plan(&tools, Provider::Brew, "firefox").unwrap();
        let out = plan.process().output().unwrap();
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            "install\n--cask\n--\nfirefox\n"
        );
        assert!(plan.validate("WRONG").is_err());
        assert!(plan.validate("INSTALL").is_ok());
        plan.created = crate::unix_now() - 601;
        assert!(plan.validate("INSTALL").is_err());
        plan.created = crate::unix_now();
        fs::write(&tool, "#!/bin/sh\nexit 1\n").unwrap();
        assert!(plan.validate("INSTALL").is_err());
    }
}
