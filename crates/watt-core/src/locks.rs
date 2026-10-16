//! Advisory file locks (`flock`), held for the life of a guard. They are released by the kernel
//! when the holder dies, so a killed op never leaves an instance locked.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

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

    /// Whether someone holds the lock right now (without keeping it).
    pub fn is_held(path: &Path) -> bool {
        path.exists() && matches!(Self::try_acquire(path), Ok(None))
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
