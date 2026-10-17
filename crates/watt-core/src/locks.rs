//! Advisory file locks (`flock`), held for the life of a guard. They are released by the kernel
//! when the holder dies, so a killed op never leaves an instance locked.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

/// What [`FileLock::state`] found. An error from that function is not a state: the caller cannot
/// tell, and must treat the lock as busy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockState {
    /// The file is missing, or the lock could be taken (and was released again).
    Free,
    /// Someone else holds it.
    Held,
}

/// A held lock.
#[derive(Debug)]
pub struct FileLock {
    _file: File,
    path: PathBuf,
}

impl FileLock {
    /// Wait for the lock.
    pub fn acquire(path: &Path) -> Result<Self> {
        let file = open(path)?;
        file.lock()
            .with_context(|| format!("cannot lock {}", path.display()))?;
        Ok(Self {
            _file: file,
            path: path.to_path_buf(),
        })
    }

    /// Take the lock if it is free; `Ok(None)` when someone else holds it.
    pub fn try_acquire(path: &Path) -> Result<Option<Self>> {
        let file = open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self {
                _file: file,
                path: path.to_path_buf(),
            })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => {
                Err(e).with_context(|| format!("cannot lock {}", path.display()))
            }
        }
    }

    /// Whether the lock is free or held, without keeping it.
    ///
    /// A missing file is [`LockState::Free`] and is not created. `try_acquire` succeeding is
    /// free (the probe lock is dropped before returning). `Ok(None)` is held. An error is
    /// returned as-is: opening the file can fail with a sharing violation even when the lock
    /// itself would only say "busy", and that must not look like "free".
    pub fn state(path: &Path) -> Result<LockState> {
        if !path.exists() {
            return Ok(LockState::Free);
        }
        match Self::try_acquire(path)? {
            Some(held) => {
                drop(held);
                Ok(LockState::Free)
            }
            None => Ok(LockState::Held),
        }
    }

    /// Whether someone holds the lock right now (without keeping it).
    ///
    /// Fail closed: a missing file is free, a held lock is held, and any error (the lock could
    /// not be probed) counts as held so callers refuse to start a second game or delete an
    /// instance they cannot prove is idle. The normal answers are unchanged.
    pub fn is_held(path: &Path) -> bool {
        !matches!(Self::state(path), Ok(LockState::Free))
    }

    /// The lock file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn open(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_holder_is_refused_until_release() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a/b.lock");
        let first = FileLock::try_acquire(&path).unwrap().expect("free");
        assert!(FileLock::try_acquire(&path).unwrap().is_none());
        assert!(FileLock::is_held(&path));
        assert_eq!(FileLock::state(&path).unwrap(), LockState::Held);
        drop(first);
        // Another test thread forking a child can hold a copy of the descriptor until the child
        // execs (flock locks follow the open file description), so allow a moment.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while FileLock::is_held(&path) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(FileLock::try_acquire(&path).unwrap().is_some());
    }
}
