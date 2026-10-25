use super::*;
use std::{collections::HashSet, fs};

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Exact,
    Review,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RelatedKind {
    Application,
    Cache,
    Preference,
    Support,
    SavedState,
    Container,
    Launcher,
}
#[derive(Clone, Debug, Serialize)]
pub struct RelatedFile {
    pub path: PathBuf,
    pub kind: RelatedKind,
    pub confidence: Confidence,
    pub contains_user_data: bool,
    pub selectable: bool,
    pub reason: String,
}

/// Match known locations by complete identity; never sweep vendor prefixes or shared groups.
pub fn related_files(context: &Context, app: &Application) -> Vec<RelatedFile> {
    let mut result = vec![];
    let mut seen = HashSet::new();
    let mut add = |path: PathBuf, kind, confidence, data, reason: &str| {
        if !context.allows(&path) || !seen.insert(path.clone()) {
            return;
        }
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            result.push(RelatedFile {
                path,
                kind,
                confidence,
                contains_user_data: data,
                selectable: !metadata.file_type().is_symlink()
                    && (metadata.is_dir() || metadata.is_file()),
                reason: reason.into(),
            });
        }
    };
    if let Some(path) = &app.path {
        match app.source {
            Source::Bundle => add(
                path.clone(),
                RelatedKind::Application,
                Confidence::Exact,
                false,
                "Application bundle; reversible Trash",
            ),
            Source::Desktop => add(
                path.clone(),
                RelatedKind::Launcher,
                Confidence::Exact,
                false,
                "Launcher only; software may require its package manager",
            ),
            _ => {}
        }
    }
    if !safe_component(&app.id) {
        return result;
    }
    let confidence = if valid_app_id(&app.id) {
        Confidence::Exact
    } else {
        Confidence::Review
    };
    let home = &context.home;
    for (folder, suffix, kind, data) in [
        ("Library/Caches", "", RelatedKind::Cache, false),
        (
            "Library/Preferences",
            ".plist",
            RelatedKind::Preference,
            true,
        ),
        (
            "Library/Saved Application State",
            ".savedState",
            RelatedKind::SavedState,
            true,
        ),
        (
            "Library/Application Support",
            "",
            RelatedKind::Support,
            true,
        ),
        ("Library/Containers", "", RelatedKind::Container, true),
        (
            "Library/Application Scripts",
            "",
            RelatedKind::Support,
            true,
        ),
        ("Library/WebKit", "", RelatedKind::Support, true),
        ("Library/HTTPStorages", "", RelatedKind::Cache, true),
        (
            "Library/HTTPStorages",
            ".binarycookies",
            RelatedKind::Preference,
            true,
        ),
        ("Library/Logs", "", RelatedKind::Cache, false),
        (".cache", "", RelatedKind::Cache, false),
        (".config", "", RelatedKind::Preference, true),
        (".local/share", "", RelatedKind::Support, true),
        (".local/state", "", RelatedKind::Support, true),
        (".var/app", "", RelatedKind::Container, true),
    ] {
        add(
            home.join(folder).join(format!("{}{suffix}", app.id)),
            kind,
            confidence,
            data,
            "Complete application identifier match",
        );
    }
    if safe_component(&app.name) {
        for folder in [
            "Library/Application Support",
            "Library/Caches",
            "Library/Logs",
        ] {
            let path = home.join(folder).join(&app.name);
            if path.file_name().is_some_and(|x| x != app.id.as_str()) {
                add(
                    path,
                    RelatedKind::Support,
                    Confidence::Review,
                    true,
                    "Name match only; may be shared. Review contents before selecting.",
                );
            }
        }
    }
    let by_host = home.join("Library/Preferences/ByHost");
    if context.allows(&by_host)
        && let Ok(entries) = fs::read_dir(by_host)
    {
        for entry in entries.take(10000).flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(host) = name
                .strip_prefix(&format!("{}.", app.id))
                .and_then(|n| n.strip_suffix(".plist"))
            else {
                continue;
            };
            if host.len() == 36 && host.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-') {
                add(
                    entry.path(),
                    RelatedKind::Preference,
                    Confidence::Exact,
                    true,
                    "Application identifier and host UUID match",
                );
            }
        }
    }
    for path in app.related_hints.iter().take(64) {
        if path.starts_with(&context.home)
            && path != &context.home
            && path
                .components()
                .all(|c| !matches!(c, std::path::Component::ParentDir))
        {
            add(
                path.clone(),
                RelatedKind::Support,
                Confidence::Review,
                true,
                "Package recipe lists this path; it may be shared. Review before selecting.",
            );
        }
    }
    result.sort_by(|a, b| {
        (a.kind != RelatedKind::Application, &a.path)
            .cmp(&(b.kind != RelatedKind::Application, &b.path))
    });
    result.truncate(128);
    if app.protected {
        for file in &mut result {
            if app.source == Source::Bundle
                || file.kind == RelatedKind::Application
                || file.kind == RelatedKind::Launcher
            {
                file.selectable = false;
                file.reason = "System or package-owned application; use its native manager".into();
            }
        }
    }
    for file in &mut result {
        if file
            .path
            .starts_with(context.home.join("Library/Group Containers"))
            || file
                .path
                .starts_with(context.home.join("Library/Keychains"))
            || file.path.starts_with(context.home.join(".ssh"))
            || file.path.starts_with(context.home.join(".gnupg"))
        {
            file.selectable = false;
            file.reason = "Shared groups and credential stores are protected".into();
        }
    }
    result
}
