use super::*;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::Path,
};
const MAX_BYTES: usize = 1024 * 1024;
#[derive(Serialize, Deserialize)]
struct Record {
    app: Application,
    time: i64,
}
#[derive(Default)]
struct History {
    records: Vec<Record>,
    rejected: Option<Vec<u8>>,
}

fn load(state: &Path) -> Result<History> {
    let file = match fs::File::open(state.join("apps-history.json")) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(History::default()),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = vec![];
    file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_BYTES,
        "App history exceeds its 1 MiB limit"
    );
    let mut records = vec![];
    let mut rejected = false;
    match serde_json::from_slice::<Vec<serde_json::Value>>(&bytes) {
        Ok(values) => {
            for value in values.into_iter().take(128) {
                match serde_json::from_value(value) {
                    Ok(record) => records.push(record),
                    Err(_) => rejected = true,
                }
            }
        }
        Err(_) => rejected = true,
    }
    Ok(History {
        records,
        rejected: rejected.then_some(bytes),
    })
}

/// Keep bounded identity/recipe metadata so related data can be found after an uninstall.
pub fn remember(state: &Path, app: &Application) -> Result<()> {
    if matches!(app.source, Source::Leftover | Source::Desktop) {
        return Ok(());
    }
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state.join("apps-history.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock)?;
    let History {
        mut records,
        rejected,
    } = load(state)?;
    if let Some(bytes) = rejected {
        // Keep one private, bounded recovery copy before replacing advisory metadata.
        let mut backup = tempfile::NamedTempFile::new_in(state)?;
        backup.write_all(&bytes)?;
        backup.as_file().sync_all()?;
        backup.persist(state.join("apps-history.unreadable.json"))?;
    }
    records.retain(|record| record.app.key() != app.key());
    records.insert(
        0,
        Record {
            app: app.clone(),
            time: crate::unix_now(),
        },
    );
    records.truncate(128);
    let bytes = loop {
        let bytes = serde_json::to_vec(&records)?;
        if bytes.len() <= MAX_BYTES {
            break bytes;
        }
        records.pop();
        ensure!(
            !records.is_empty(),
            "Application metadata is too large to remember safely"
        );
    };
    let mut file = tempfile::NamedTempFile::new_in(state)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist(state.join("apps-history.json"))?;
    Ok(())
}

/// Unmatched data is a review candidate, never proof that a background service is unused.
pub fn leftovers(context: &Context, catalog: &Catalog, state: &Path) -> Result<Vec<Application>> {
    let installed: HashSet<_> = catalog.apps.iter().map(|a| a.id.to_lowercase()).collect();
    let mut seen = HashSet::new();
    let mut result = vec![];
    for record in load(state)?.records {
        let mut app = record.app;
        if installed.contains(&app.id.to_lowercase()) || !seen.insert(app.id.to_lowercase()) {
            continue;
        }
        app.source = Source::Leftover;
        app.path = None;
        app.protected = false;
        app.remembered = catalog.warnings.is_empty();
        if related_files(context, &app).iter().any(|f| f.selectable) {
            result.push(app);
        }
    }
    for (folder, suffix) in [
        ("Library/Caches", ""),
        ("Library/Preferences", ".plist"),
        ("Library/Application Support", ""),
        ("Library/Containers", ""),
        (".cache", ""),
        (".config", ""),
        (".local/share", ""),
        (".var/app", ""),
    ] {
        let folder = context.home.join(folder);
        if !context.allows(&folder) {
            continue;
        }
        let Ok(entries) = fs::read_dir(folder) else {
            continue;
        };
        for entry in entries.take(5000).flatten() {
            if result.len() >= 512 {
                break;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(id) = name.strip_suffix(suffix) else {
                continue;
            };
            if !valid_app_id(id)
                || id.split('.').count() < 3
                || id.starts_with("com.apple.")
                || installed.contains(&id.to_lowercase())
                || !seen.insert(id.to_lowercase())
            {
                continue;
            }
            result.push(Application {
                id: id.into(),
                name: id.into(),
                version: None,
                path: None,
                source: Source::Leftover,
                protected: false,
                related_hints: vec![],
                remembered: false,
            });
        }
    }
    result.sort_by_cached_key(|a| (a.name.to_lowercase(), a.id.to_lowercase()));
    Ok(result)
}
