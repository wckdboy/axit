//! Application identity, inventory and reviewed related-file discovery.
mod catalog;
mod command;
mod history;
mod managed;
mod related;
#[cfg(test)]
mod tests;

pub use catalog::{Catalog, Context, inventory, read_bundle};
pub use history::{leftovers, remember};
pub use managed::{ManagedPlan, ManagedTools, Provider, install_plan, uninstall_plan};
pub use related::{Confidence, RelatedFile, RelatedKind, related_files};

/// Shared identifiers can belong to several installed copies; keep their data.
pub fn review_files(context: &Context, catalog: &Catalog, app: &Application) -> Vec<RelatedFile> {
    let mut files = related_files(context, app);
    if catalog
        .apps
        .iter()
        .filter(|other| other.id.eq_ignore_ascii_case(&app.id))
        .count()
        > 1
    {
        for file in &mut files {
            if file.kind != RelatedKind::Application {
                file.selectable = false;
                file.reason = "Shared identifier with another installed copy; data kept".into();
            }
        }
    }
    files
}

pub fn default_file(app: &Application, file: &RelatedFile, uninstall: bool) -> bool {
    file.selectable
        && ((uninstall && file.kind == RelatedKind::Application)
            || (file.kind == RelatedKind::Cache
                && file.confidence == Confidence::Exact
                && !file.contains_user_data
                && (!matches!(app.source, Source::Leftover) || app.remembered)))
}

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    Bundle,
    Desktop,
    Brew { token: String },
    Flatpak { reference: String, user: bool },
    Leftover,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Application {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub path: Option<PathBuf>,
    pub source: Source,
    pub protected: bool,
    pub related_hints: Vec<PathBuf>,
    #[serde(default)]
    pub remembered: bool,
}

impl Application {
    pub fn key(&self) -> String {
        match &self.source {
            Source::Brew { token } => self
                .path
                .as_deref()
                .map(|p| format!("brew:{token}:{}", crate::text(p)))
                .unwrap_or_else(|| format!("brew:{token}")),
            Source::Flatpak { reference, user } => format!(
                "flatpak:{}:{reference}",
                if *user { "user" } else { "system" }
            ),
            Source::Leftover => format!("leftover:{}", self.id),
            _ => self
                .path
                .as_deref()
                .map(crate::text)
                .unwrap_or_else(|| self.id.clone()),
        }
    }
    pub fn source_label(&self) -> &'static str {
        match self.source {
            Source::Bundle => {
                if self.protected {
                    "Protected app"
                } else {
                    "App bundle"
                }
            }
            Source::Desktop => "Desktop launcher",
            Source::Brew { .. } => "Homebrew",
            Source::Flatpak { user: true, .. } => "Flatpak · user",
            Source::Flatpak { user: false, .. } => "Flatpak · system",
            Source::Leftover => {
                if self.remembered {
                    "Known app data"
                } else {
                    "Unmatched data"
                }
            }
        }
    }
}

pub fn resolve<'a>(catalog: &'a Catalog, query: &str) -> anyhow::Result<&'a Application> {
    let mut matches = catalog.apps.iter().filter(|app| {
        app.key() == query
            || app.id.eq_ignore_ascii_case(query)
            || app.name.eq_ignore_ascii_case(query)
            || matches!(&app.source,Source::Brew{token} if query==format!("brew:{token}"))
    });
    let first = matches
        .next()
        .ok_or_else(|| anyhow::anyhow!("No application matches {query:?}"))?;
    anyhow::ensure!(
        matches.next().is_none(),
        "More than one app matches; use its full key from apps list"
    );
    Ok(first)
}

pub fn ensure_stopped(app: &Application) -> anyhow::Result<()> {
    let open = crate::system::OpenFiles::capture()?;
    ensure_stopped_with(app, &open)
}
pub(crate) fn ensure_stopped_with(
    app: &Application,
    open: &crate::system::OpenFiles,
) -> anyhow::Result<()> {
    if let Some(path) = &app.path
        && matches!(app.source, Source::Bundle | Source::Brew { .. })
    {
        let current = read_bundle(path)?;
        anyhow::ensure!(
            current.id.eq_ignore_ascii_case(&app.id),
            "Application identity changed since review"
        );
        anyhow::ensure!(
            !open.in_use(path, None),
            "Quit {} before removing its files",
            app.name
        );
    }
    if let Source::Flatpak { .. } = &app.source {
        let tools = ManagedTools::system();
        let program = tools.flatpak.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Flatpak is unavailable; cannot inspect running apps")
        })?;
        let output = command::query(
            program,
            &["ps", "--columns=application"],
            std::time::Duration::from_secs(5),
        )?;
        anyhow::ensure!(
            !String::from_utf8_lossy(&output)
                .lines()
                .any(|line| line.trim() == app.id),
            "Quit {} before removing its files",
            app.name
        );
    }
    Ok(())
}

pub fn valid_app_id(id: &str) -> bool {
    (3..=255).contains(&id.len())
        && id.split('.').count() >= 2
        && id.split('.').all(|part| {
            !part.is_empty()
                && part.as_bytes()[0].is_ascii_alphanumeric()
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        })
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !matches!(value, "." | "..")
        && !value
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\'))
}
