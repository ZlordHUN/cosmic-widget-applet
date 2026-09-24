// SPDX-License-Identifier: MPL-2.0

//! Per-user single-instance guard for the standalone layer-shell widget.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// Keeps the advisory lock alive for the lifetime of the widget process.
pub struct InstanceGuard {
    _file: File,
}

/// Try to acquire the standalone widget's per-user lock.
///
/// A lock is preferable to process-name checks here because panel restarts can
/// race with delayed autostart tasks. The kernel releases it automatically if
/// the widget exits or crashes.
pub fn try_acquire() -> io::Result<Option<InstanceGuard>> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))?;

    try_acquire_at(&runtime_dir.join("cosmic-widget.lock"))
}

fn try_acquire_at(path: &Path) -> io::Result<Option<InstanceGuard>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .open(path)?;

    // SAFETY: flock only reads the valid file descriptor owned by `file`.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(Some(InstanceGuard { _file: file }));
    }

    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::WouldBlock {
        Ok(None)
    } else {
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::try_acquire_at;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_LOCK: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn prevents_a_second_instance_until_the_first_exits() {
        let suffix = NEXT_LOCK.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "cosmic-widget-instance-test-{}-{suffix}.lock",
            std::process::id()
        ));

        let first = try_acquire_at(&path).unwrap().unwrap();
        assert!(try_acquire_at(&path).unwrap().is_none());

        drop(first);
        assert!(try_acquire_at(&path).unwrap().is_some());

        let _ = std::fs::remove_file(path);
    }
}
