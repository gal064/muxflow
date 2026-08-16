use std::{
    fs::{self, OpenOptions},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::Path,
};

use anyhow::Context;

/// Process-shared transaction guard for one runtime's hook mailbox.
///
/// Writers hold it across prune + rename + directory sync, and the daemon
/// holds it across enumerate + apply/delete. Keeping the lock file in place is
/// intentional: unlinking an advisory-lock inode lets a racing process lock a
/// different replacement inode and destroys mutual exclusion.
pub(crate) struct HookMailboxLock {
    file: fs::File,
}

impl HookMailboxLock {
    pub(crate) fn acquire(runtime: &Path) -> anyhow::Result<Self> {
        crate::paths::prepare_runtime_dir(runtime)?;
        let path = runtime.join(".hook-fallback-mailbox.lock");
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .context("open hook mailbox lock")?;
        // SAFETY: flock receives this live descriptor and a valid operation.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error()).context("lock hook mailbox");
        }
        Ok(Self { file })
    }
}

impl Drop for HookMailboxLock {
    fn drop(&mut self) {
        // SAFETY: self owns the descriptor until after Drop returns.
        unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}
