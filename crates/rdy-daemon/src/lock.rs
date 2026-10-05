//! An exclusive lock on a file, released explicitly.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// An exclusively flock-ed file. Dropping it unlocks explicitly before the close: the lock
/// belongs to the open file description, which a child spawned by another thread shares until
/// it execs, so leaving the release to the close would keep the lock held after we are done.
pub struct FileLock(File);

impl FileLock {
    /// Opens `path` (create, read+write, mode 0600; std opens with O_CLOEXEC) and tries the
    /// lock once. `Ok(None)` when another holder has it. The file is never deleted.
    pub fn try_take(path: &Path) -> io::Result<Option<FileLock>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(e)) => return Err(e),
        }
        // guarded first, so an error below still unlocks explicitly
        let lock = FileLock(file);
        // a file left by an older run keeps its mode through open; set it exactly
        lock.0.set_permissions(fs::Permissions::from_mode(0o600))?;
        Ok(Some(lock))
    }

    /// Test hook: a duplicate of the descriptor, as a spawned child holds one until exec.
    #[doc(hidden)]
    pub fn share_descriptor_for_tests(&self) -> io::Result<File> {
        self.0.try_clone()
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
