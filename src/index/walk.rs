//! Descriptor-relative, metadata-only traversal for targeted reconciliation.
use anyhow::{Context, Result, ensure};
use std::{
    ffi::{CStr, CString},
    fs::{File, Metadata},
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
};

#[derive(Clone, Copy, Debug)]
pub(super) struct Facts {
    pub device: u64,
    pub inode: u64,
    pub uid: u32,
    pub logical: u64,
    pub allocated: u64,
    pub modified: i64,
    pub modified_ns: i64,
    pub changed: i64,
    pub changed_ns: i64,
    pub links: u64,
    pub mode: u32,
}
// mode_t and stat field widths differ between Darwin and Linux.
#[allow(clippy::unnecessary_cast)]
impl Facts {
    pub fn same_read(self, other: Self) -> bool {
        (
            self.device,
            self.inode,
            self.modified,
            self.modified_ns,
            self.changed,
            self.changed_ns,
            self.allocated,
        ) == (
            other.device,
            other.inode,
            other.modified,
            other.modified_ns,
            other.changed,
            other.changed_ns,
            other.allocated,
        )
    }
    pub fn directory(self) -> bool {
        self.mode & libc::S_IFMT as u32 == libc::S_IFDIR as u32
    }
    pub fn symlink(self) -> bool {
        self.mode & libc::S_IFMT as u32 == libc::S_IFLNK as u32
    }
    pub fn regular(self) -> bool {
        self.mode & libc::S_IFMT as u32 == libc::S_IFREG as u32
    }
    fn from_metadata(md: &Metadata) -> Self {
        Self {
            device: md.dev(),
            inode: md.ino(),
            uid: md.uid(),
            logical: md.len(),
            allocated: md.blocks().saturating_mul(512),
            modified: md.mtime(),
            modified_ns: md.mtime_nsec(),
            changed: md.ctime(),
            changed_ns: md.ctime_nsec(),
            links: md.nlink(),
            mode: md.mode(),
        }
    }
    // libc field widths differ between Darwin and Linux; retain one checked
    // representation for the four supported 64-bit release targets.
    fn from_stat(md: libc::stat) -> Self {
        Self {
            device: md.st_dev as u64,
            inode: md.st_ino as u64,
            uid: md.st_uid,
            logical: md.st_size.max(0) as u64,
            allocated: (md.st_blocks.max(0) as u64).saturating_mul(512),
            modified: md.st_mtime as i64,
            modified_ns: md.st_mtime_nsec as i64,
            changed: md.st_ctime as i64,
            changed_ns: md.st_ctime_nsec as i64,
            links: md.st_nlink as u64,
            mode: md.st_mode as u32,
        }
    }
}
fn name(value: &[u8]) -> Result<CString> {
    ensure!(
        !value.is_empty() && value != b"." && value != b".." && !value.contains(&b'/'),
        "Invalid directory entry name"
    );
    CString::new(value).context("Filename contains NUL")
}
const OPEN_DIR: i32 = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

pub(super) struct Directory(File);
impl Directory {
    pub fn root(path: &Path, identity: (u64, u64)) -> Result<Self> {
        ensure!(path.is_absolute(), "Refresh root must be absolute");
        let root = CString::new("/")?;
        // The literal path and flags are valid; ownership of successful fds is
        // immediately transferred into File, including all early error paths.
        let fd = unsafe { libc::open(root.as_ptr(), OPEN_DIR) };
        ensure!(
            fd >= 0,
            "Cannot open refresh root: {}",
            io::Error::last_os_error()
        );
        let mut current = Self(unsafe { File::from_raw_fd(fd) });
        for part in path.components() {
            match part {
                Component::RootDir => {}
                Component::Normal(value) => current = current.open(value.as_bytes())?,
                _ => anyhow::bail!("Refresh path contains a non-normal component"),
            }
        }
        let actual = current.facts()?;
        ensure!(
            (actual.device, actual.inode) == identity,
            "Indexed root was replaced; run a full scan"
        );
        Ok(current)
    }
    fn open(&self, value: &[u8]) -> Result<Self> {
        let name = name(value)?;
        let fd = unsafe { libc::openat(self.0.as_raw_fd(), name.as_ptr(), OPEN_DIR) };
        if fd < 0 {
            return Err(io::Error::last_os_error())
                .context("Cannot open refresh directory without following symlinks");
        }
        Ok(Self(unsafe { File::from_raw_fd(fd) }))
    }
    pub fn child(&self, value: &[u8], expected: Facts) -> Result<Self> {
        let child = self.open(value)?;
        let actual = child.facts()?;
        ensure!(
            (actual.device, actual.inode) == (expected.device, expected.inode),
            "Directory changed during refresh; retry"
        );
        Ok(child)
    }
    pub fn duplicate(&self) -> Result<Self> {
        Ok(Self(self.0.try_clone()?))
    }
    pub fn facts(&self) -> Result<Facts> {
        Ok(Facts::from_metadata(&self.0.metadata()?))
    }
    pub fn stat(&self, value: &[u8]) -> Result<Facts> {
        let name = name(value)?;
        let mut md = std::mem::MaybeUninit::<libc::stat>::uninit();
        // stat is metadata-only and never follows the final symlink. Parent
        // components have already been anchored by no-follow directory fds.
        let result = unsafe {
            libc::fstatat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                md.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Facts::from_stat(unsafe { md.assume_init() }))
    }
    pub fn entries(&self) -> Result<Entries> {
        // A fresh file description avoids sharing a directory iteration offset
        // with this anchor or another iterator on it.
        let dot = CString::new(".")?;
        let fd = unsafe { libc::openat(self.0.as_raw_fd(), dot.as_ptr(), OPEN_DIR) };
        if fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let pointer = unsafe { libc::fdopendir(fd) };
        if pointer.is_null() {
            let error = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(error.into());
        }
        Ok(Entries {
            pointer,
            finished: false,
        })
    }
}

pub(super) struct Entries {
    pointer: *mut libc::DIR,
    finished: bool,
}
impl Iterator for Entries {
    type Item = io::Result<Vec<u8>>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        loop {
            #[cfg(target_os = "macos")]
            unsafe {
                *libc::__error() = 0;
            }
            #[cfg(target_os = "linux")]
            unsafe {
                *libc::__errno_location() = 0;
            }
            let entry = unsafe { libc::readdir(self.pointer) };
            if entry.is_null() {
                self.finished = true;
                let error = io::Error::last_os_error();
                return (error.raw_os_error() != Some(0)).then_some(Err(error));
            }
            // POSIX readdir owns this NUL-terminated name until its next call.
            // Copy bytes now, retaining non-UTF8 names without interpreting them.
            let value = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if value != b"." && value != b".." {
                return Some(Ok(value.to_vec()));
            }
        }
    }
}
impl Drop for Entries {
    fn drop(&mut self) {
        unsafe { libc::closedir(self.pointer) };
    }
}
