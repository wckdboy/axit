pub mod apps;
pub mod index;
pub mod ops;
pub mod page;
pub mod protocol;
pub mod query;
pub mod system;
pub mod tui;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("Cannot locate your home directory")
}

pub fn state_dir() -> Result<PathBuf> {
    let p = if let Some(p) =
        std::env::var_os("AXIT_STATE_DIR").or_else(|| std::env::var_os("SCRY_STATE_DIR"))
    {
        PathBuf::from(p)
    } else if cfg!(target_os = "macos") {
        let base = home()?.join("Library/Application Support");
        preferred_state(base.join("ai.wckd.axit"), base.join("ai.wckd.scry"))
    } else {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or(home()?.join(".local/state"));
        preferred_state(base.join("axit"), base.join("scry"))
    };
    std::fs::create_dir_all(&p)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(p)
}

fn preferred_state(primary: PathBuf, legacy: PathBuf) -> PathBuf {
    // Reuse the old private store in place, including its large metadata index.
    // An explicit AXIT_STATE_DIR or an existing Axit store always takes priority.
    if !primary.exists() && legacy.is_dir() {
        legacy
    } else {
        primary
    }
}

pub fn index_path(state: &Path) -> PathBuf {
    state.join("disk.idx")
}

pub fn text(path: &Path) -> String {
    clean(&path.to_string_lossy())
}

pub fn clean(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < 4 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Resolve only genuine macOS Data-volume aliases, never an arbitrary path prefix.
pub fn scoped_alias(path: &Path, root: &Path) -> Result<PathBuf> {
    if path.starts_with(root) {
        return Ok(path.to_path_buf());
    }
    #[cfg(target_os = "macos")]
    if root.starts_with("/System/Volumes/Data") && path.is_absolute() {
        use std::os::unix::fs::MetadataExt;
        let candidate = Path::new("/System/Volumes/Data").join(path.strip_prefix("/")?);
        let original = std::fs::symlink_metadata(path)?;
        let alias = std::fs::symlink_metadata(&candidate)?;
        if original.dev() == alias.dev()
            && original.ino() == alias.ino()
            && candidate.starts_with(root)
        {
            return Ok(candidate);
        }
    }
    anyhow::bail!("Path is outside the selected scope")
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    #[test]
    fn existing_scry_data_is_reused_without_copying_or_overwriting_a_new_store() {
        let temp = tempfile::tempdir().unwrap();
        let legacy = temp.path().join("scry");
        let primary = temp.path().join("axit");
        std::fs::create_dir(&legacy).unwrap();
        std::fs::write(legacy.join("disk.idx"), b"keep index").unwrap();
        assert_eq!(preferred_state(primary.clone(), legacy.clone()), legacy);
        std::fs::create_dir(&primary).unwrap();
        assert_eq!(preferred_state(primary.clone(), legacy.clone()), primary);
        assert_eq!(
            std::fs::read(legacy.join("disk.idx")).unwrap(),
            b"keep index"
        );
    }
}
