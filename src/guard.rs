//! Duplicate guard for credit-burning and self-mutating operations.
//!
//! The kernel owns the lock for as long as the acquired file handle remains
//! open. The lock file is never removed: unlinking it would allow another
//! process to open a new inode and bypass a live owner's lock. File contents
//! are diagnostic metadata only and never determine ownership.

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::errors::CliError;

pub struct DuplicateGuard {
    lock_path: PathBuf,
    operation: String,
    file: Option<File>,
}

impl DuplicateGuard {
    pub fn new(data_dir: &Path, operation: &str) -> Self {
        Self {
            lock_path: data_dir.join("locks").join(format!("{operation}.lock")),
            operation: operation.to_string(),
            file: None,
        }
    }

    /// Acquire this operation's kernel lock without waiting.
    ///
    /// `force` deliberately bypasses duplicate protection and does not open,
    /// modify, unlock, or remove another caller's lock file.
    pub fn acquire(&mut self, force: bool) -> Result<(), CliError> {
        if force || self.file.is_some() {
            return Ok(());
        }

        let lock_dir = self.lock_path.parent().expect("lock path has a parent");
        std::fs::create_dir_all(lock_dir)?;

        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        let mut file = options.open(&self.lock_path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(CliError::InvalidInput(format!(
                    "Operation '{}' is already running. Use --force to override.",
                    self.operation
                )));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }

        let metadata = serde_json::json!({
            "pid": std::process::id(),
            "started_at": chrono::Utc::now().to_rfc3339(),
            "operation": self.operation,
        });
        let contents = serde_json::to_vec(&metadata)?;
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&contents)?;
        file.sync_data()?;

        self.file = Some(file);
        Ok(())
    }

    /// Release only the lock held by this guard. The stable lock file remains.
    pub fn release(&mut self) {
        self.file = None;
    }
}

impl Drop for DuplicateGuard {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejected_and_forced_guards_cannot_release_owner() {
        // Windows locks also exclude reads through separately opened handles.
        // Read diagnostic metadata through the handle that owns the lock.
        fn snapshot(guard: &mut DuplicateGuard) -> Vec<u8> {
            use std::io::Read;
            let file = guard.file.as_mut().unwrap();
            file.seek(SeekFrom::Start(0)).unwrap();
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            bytes
        }
        let tmp = tempfile::tempdir().unwrap();
        let mut owner = DuplicateGuard::new(tmp.path(), "op");
        owner.acquire(false).unwrap();
        let metadata = snapshot(&mut owner);

        {
            let mut rejected = DuplicateGuard::new(tmp.path(), "op");
            let error = rejected.acquire(false).unwrap_err();
            assert!(matches!(error, CliError::InvalidInput(_)));
            rejected.release();
        }
        {
            let mut forced = DuplicateGuard::new(tmp.path(), "op");
            forced.acquire(true).unwrap();
            forced.release();
        }

        assert_eq!(snapshot(&mut owner), metadata);
        let mut contender = DuplicateGuard::new(tmp.path(), "op");
        assert_eq!(contender.acquire(false).unwrap_err().exit_code(), 3);
    }

    #[test]
    fn stale_or_corrupt_metadata_does_not_claim_ownership() {
        for contents in [
            b"invalid metadata".as_slice(),
            br#"{"pid":999999,"started_at":"1999-01-01T00:00:00Z","operation":"op"}"#,
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let mut guard = DuplicateGuard::new(tmp.path(), "op");
            std::fs::create_dir_all(guard.lock_path.parent().unwrap()).unwrap();
            std::fs::write(&guard.lock_path, contents).unwrap();

            guard.acquire(false).unwrap();
            assert!(guard.file.is_some());
        }
    }

    #[test]
    fn simultaneous_callers_have_exactly_one_owner() {
        const CALLERS: usize = 8;
        let tmp = tempfile::tempdir().unwrap();
        let barrier = std::sync::Barrier::new(CALLERS);

        let outcomes = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..CALLERS)
                .map(|_| {
                    scope.spawn(|| {
                        let mut guard = DuplicateGuard::new(tmp.path(), "generate");
                        barrier.wait();
                        let result = guard.acquire(false);
                        barrier.wait();
                        result
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });

        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert!(
            outcomes
                .iter()
                .filter_map(|result| result.as_ref().err())
                .all(|error| matches!(error, CliError::InvalidInput(_)))
        );
    }

    #[test]
    fn release_keeps_inode_and_allows_next_owner() {
        let tmp = tempfile::tempdir().unwrap();
        let mut first = DuplicateGuard::new(tmp.path(), "op");
        first.acquire(false).unwrap();
        let path = first.lock_path.clone();

        #[cfg(unix)]
        let inode_before = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&path).unwrap().ino()
        };

        first.release();
        assert!(path.exists());

        let mut second = DuplicateGuard::new(tmp.path(), "op");
        second.acquire(false).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode_before);
        }
    }

    #[test]
    fn different_operations_do_not_block_each_other() {
        let tmp = tempfile::tempdir().unwrap();
        let mut generate = DuplicateGuard::new(tmp.path(), "generate");
        let mut update = DuplicateGuard::new(tmp.path(), "update");
        generate.acquire(false).unwrap();
        update.acquire(false).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn metadata_permissions_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let mut guard = DuplicateGuard::new(tmp.path(), "op");
        guard.acquire(false).unwrap();

        let mode = std::fs::metadata(&guard.lock_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
