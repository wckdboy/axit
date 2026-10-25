use anyhow::{Context, Result, bail, ensure};
use std::{
    io::Read,
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct QueryChild(Child);
impl Drop for QueryChild {
    fn drop(&mut self) {
        if self.0.try_wait().is_ok_and(|status| status.is_none()) {
            // Only the process group created for this read-only metadata query.
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
            let _ = self.0.wait();
        }
    }
}

fn nonblocking(stream: &impl AsRawFd) -> Result<()> {
    let fd = stream.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "Cannot read package-manager metadata safely"
    );
    Ok(())
}
fn drain(stream: &mut impl Read, output: &mut Vec<u8>, limit: usize) -> Result<()> {
    let mut bytes = [0; 8192];
    loop {
        match stream.read(&mut bytes) {
            Ok(0) => break,
            Ok(n) => {
                ensure!(
                    output.len() + n <= limit,
                    "Package metadata exceeds the size limit"
                );
                output.extend_from_slice(&bytes[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Bounded metadata capture; never invokes a shell or waits indefinitely.
pub(super) fn query(program: &Path, args: &[&str], timeout: Duration) -> Result<Vec<u8>> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("HOMEBREW_NO_AUTO_UPDATE", "1")
        .env("HOMEBREW_NO_ANALYTICS", "1")
        .env("LC_ALL", "C")
        .process_group(0);
    let mut child = QueryChild(
        command
            .spawn()
            .with_context(|| format!("Cannot query {}", crate::text(program)))?,
    );
    let mut stdout = child
        .0
        .stdout
        .take()
        .context("Missing package metadata output")?;
    let mut stderr = child
        .0
        .stderr
        .take()
        .context("Missing package error output")?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let (mut output, mut errors) = (vec![], vec![]);
    let started = Instant::now();
    loop {
        drain(&mut stdout, &mut output, 8 * 1024 * 1024)?;
        drain(&mut stderr, &mut errors, 128 * 1024)?;
        if let Some(status) = child.0.try_wait()? {
            drain(&mut stdout, &mut output, 8 * 1024 * 1024)?;
            drain(&mut stderr, &mut errors, 128 * 1024)?;
            ensure!(
                status.success(),
                "Package query failed: {}",
                crate::clean(&String::from_utf8_lossy(&errors))
                    .chars()
                    .take(400)
                    .collect::<String>()
            );
            return Ok(output);
        }
        if started.elapsed() >= timeout {
            bail!("Package-manager query timed out");
        }
        thread::sleep(Duration::from_millis(10));
    }
}
