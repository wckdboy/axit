use anyhow::{Context, Result};
use serde::Serialize;
use std::{
    collections::HashSet,
    ffi::CString,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Serialize)]
pub struct Volume {
    pub total: u64,
    pub free: u64,
    pub available: u64,
}

pub fn volume(path: &Path) -> Result<Volume> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    let mut value = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // Both pointers are valid for the duration of the libc call.
    if unsafe { libc::statvfs(path.as_ptr(), value.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let s = unsafe { value.assume_init() };
    #[cfg(target_os = "macos")]
    let (total, free, available) = (
        u64::from(s.f_blocks),
        u64::from(s.f_bfree),
        u64::from(s.f_bavail),
    );
    #[cfg(target_os = "linux")]
    let (total, free, available) = (s.f_blocks, s.f_bfree, s.f_bavail);
    Ok(Volume {
        total: total.saturating_mul(s.f_frsize),
        free: free.saturating_mul(s.f_frsize),
        available: available.saturating_mul(s.f_frsize),
    })
}

#[derive(Debug, Default)]
pub struct OpenFiles {
    pub paths: HashSet<PathBuf>,
    pub build_directories: Vec<PathBuf>,
}

impl OpenFiles {
    pub fn capture() -> Result<Self> {
        let tool = if cfg!(target_os = "macos") {
            Path::new("/usr/sbin/lsof")
        } else {
            Path::new("/usr/bin/lsof")
        };
        let uid = unsafe { libc::geteuid() }.to_string();
        let result=Command::new(tool).args(["-nP","-u",&uid,"-Fpcfn"]).output()
            .context("Cannot inspect active files. Install lsof; cleanup stays blocked until process checks work")?;
        anyhow::ensure!(
            result.status.success(),
            "Active-file inspection failed; no cleanup performed"
        );
        let mut open = Self::default();
        let mut command = String::new();
        let mut fd = String::new();
        for line in result.stdout.split(|c| *c == b'\n') {
            let Some((&tag, value)) = line.split_first() else {
                continue;
            };
            match tag {
                b'c' => command = String::from_utf8_lossy(value).into_owned(),
                b'f' => fd = String::from_utf8_lossy(value).into_owned(),
                b'n' if value.starts_with(b"/") => {
                    use std::os::unix::ffi::OsStringExt;
                    let path = PathBuf::from(std::ffi::OsString::from_vec(value.to_vec()));
                    if fd == "cwd"
                        && [
                            "cargo",
                            "rustc",
                            "clippy-driver",
                            "java",
                            "gradle",
                            "xcodebuild",
                            "swiftc",
                            "clang",
                            "ninja",
                            "cmake",
                            "make",
                        ]
                        .contains(&command.as_str())
                    {
                        open.build_directories.push(path.clone());
                    }
                    open.paths.insert(path);
                }
                _ => {}
            }
        }
        anyhow::ensure!(
            !open.paths.is_empty(),
            "Active-file inspection returned no data; cleanup blocked"
        );
        Ok(open)
    }

    pub fn in_use(&self, path: &Path, project: Option<&Path>) -> bool {
        self.paths.iter().any(|p| p == path || p.starts_with(path))
            || project
                .is_some_and(|root| self.build_directories.iter().any(|p| p.starts_with(root)))
    }
}

pub fn disk_layout() -> Result<serde_json::Value> {
    if cfg!(target_os = "macos") {
        let result = Command::new("/usr/sbin/diskutil")
            .args(["apfs", "list"])
            .output()?;
        anyhow::ensure!(result.status.success(), "diskutil failed");
        Ok(
            serde_json::json!({"format":"diskutil apfs list","text":String::from_utf8_lossy(&result.stdout)}),
        )
    } else {
        let result = Command::new("lsblk")
            .args(["-b", "-J", "-o", "NAME,SIZE,TYPE,MOUNTPOINTS,FSTYPE"])
            .output()?;
        anyhow::ensure!(result.status.success(), "lsblk failed");
        Ok(serde_json::from_slice(&result.stdout)?)
    }
}
