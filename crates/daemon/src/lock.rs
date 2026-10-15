//! The single-instance lock a daemon holds on `<state>/daemon.lock` for its lifetime.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::path::Path;

use anyhow::Context;

/// Holds the lock and keeps the holder's pid in the file; the file is emptied on drop.
pub struct InstanceLock {
    file: File,
}

impl InstanceLock {
    pub fn acquire(path: &Path) -> anyhow::Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let mut holder = String::new();
                let _ = file.read_to_string(&mut holder);
                match holder.trim().parse::<u32>() {
                    Ok(pid) => anyhow::bail!("a daemon is already running (pid {pid})"),
                    Err(_) => anyhow::bail!("a daemon is already running"),
                }
            }
            Err(TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("locking {}", path.display()));
            }
        }
        file.set_len(0)
            .and_then(|()| write!(file, "{}", std::process::id()))
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(Self { file })
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = self.file.set_len(0);
    }
}
