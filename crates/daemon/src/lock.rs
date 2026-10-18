//! The single-instance lock a daemon holds on `<state>/daemon.lock` for its lifetime.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context;
use protocol::lockfile;

/// Holds the lock and keeps the holder's pid and start time in the file; the file is
/// emptied on drop.
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
        match try_lock_for(&file, PROBE_WAIT) {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let mut holder = String::new();
                let _ = file.read_to_string(&mut holder);
                match lockfile::parse(&holder) {
                    Some(holder) => {
                        anyhow::bail!("a daemon is already running (pid {})", holder.pid)
                    }
                    None => anyhow::bail!("a daemon is already running"),
                }
            }
            Err(TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("locking {}", path.display()));
            }
        }
        let pid = std::process::id();
        let holder = lockfile::format(pid, lockfile::process_start(pid).as_deref());
        file.set_len(0)
            .and_then(|()| file.write_all(holder.as_bytes()))
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(Self { file })
    }
}

/// A client checking whether a daemon runs takes the lock for a moment; this long covers it.
const PROBE_WAIT: Duration = Duration::from_millis(200);

fn try_lock_for(file: &File, wait: Duration) -> Result<(), TryLockError> {
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            result => return result,
        }
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = self.file.set_len(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(path: &Path) -> File {
        let file = File::open(path).expect("open");
        file.try_lock().expect("probe lock");
        file
    }

    #[test]
    fn a_lock_probed_meanwhile_is_still_acquired() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("daemon.lock");
        std::fs::write(&path, "").expect("create");
        let probe = probe(&path);
        let released = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            drop(probe);
        });
        InstanceLock::acquire(&path).expect("acquired after the probe");
        released.join().expect("join");
    }

    #[test]
    fn a_lock_held_by_another_daemon_is_refused_soon() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("daemon.lock");
        let _held = InstanceLock::acquire(&path).expect("first");
        let started = Instant::now();
        let err = InstanceLock::acquire(&path).err().expect("second refused");
        assert!(format!("{err:#}").contains("already running"), "{err:#}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }
}
