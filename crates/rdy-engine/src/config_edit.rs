//! One config file, locked for an edit: the bytes read under a blocking lock, and a durable
//! replacement while the lock is held.
//!
//! A [`LockedConfig`] holds the exclusive lock of the file's stable sibling `<file>.lock` from
//! [`LockedConfig::open`] until it is dropped, so two processes that edit one file take turns.
//! The lock file is created private and is never unlinked, so every process locks the same
//! file. The type knows bytes only: the caller selects the path, parses the bytes and keeps the
//! values of other layers.
//!
//! The file, its lock and its parent directory must be this user's: a symbolic link at any of
//! them, a target that is not a regular file and a foreign owner are refused. A secret file
//! (`private`) must also be mode 0o600; a settings file keeps its mode through a replacement.
//! Nothing here changes the mode of an existing file or directory.

use crate::error::{E_IO, E_LIMIT, RdyError};
use rdy_store::{fsutil, pio};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// A config file's bytes, read under its sibling lock, which is held until drop.
pub struct LockedConfig {
    path: PathBuf,
    private: bool,
    limit: usize,
    bytes: Vec<u8>,
    lock: File,
}

impl LockedConfig {
    /// Locks `path` through `<path>.lock` (waiting for another holder), then reads the file.
    /// A missing file reads as empty bytes and is not created. Missing parent directories are
    /// created private, durably. `private` is true for a secret file, which must be mode 0o600.
    /// A file over `limit` bytes is `E_LIMIT`.
    pub fn open(path: &Path, private: bool, limit: usize) -> Result<LockedConfig, RdyError> {
        let lock = lock_path(path)?;
        ensure_parent(parent_of(path))?;
        let lock = open_lock(&lock)?;
        lock.lock().map_err(|_| save_failed())?;
        // the lock is held from here; dropping `held` releases it on every error below
        let mut held = LockedConfig {
            path: path.to_path_buf(),
            private,
            limit,
            bytes: vec![],
            lock,
        };
        held.bytes = read_target(path, private, limit)?;
        Ok(held)
    }

    /// The file's bytes as read under the lock, or as last replaced.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Replaces the file's bytes durably while the lock is held: the old bytes stay until the
    /// complete new bytes are renamed into place. A secret file ends 0o600; a settings file keeps
    /// its mode, and a new one is 0o600. Bytes over the limit are `E_LIMIT`, refused before any
    /// temp file is made. A failure of the directory fsync after the rename is an error, with the
    /// new bytes possibly in place.
    pub fn replace(&mut self, bytes: &[u8]) -> Result<(), RdyError> {
        if bytes.len() > self.limit {
            return Err(too_large(self.limit));
        }
        let written = if self.private {
            fsutil::write_private_durable(&self.path, bytes)
        } else {
            fsutil::write_settings_durable(&self.path, bytes)
        };
        written.map_err(|_| save_failed())?;
        self.bytes = bytes.to_vec();
        Ok(())
    }
}

impl Drop for LockedConfig {
    fn drop(&mut self) {
        // explicit, as the store's lock does: a child spawned by another thread shares the open
        // file description until it execs
        let _ = self.lock.unlock();
    }
}

/// E_IO: a link, a foreign owner, a file that is not regular, or a secret file that is not 0o600.
fn not_private() -> RdyError {
    RdyError::new(
        E_IO,
        "the config path is not private to this user",
        "use owner-only permissions and remove symbolic links from the config path",
    )
}

/// E_IO: a read, write, sync or rename failed.
fn save_failed() -> RdyError {
    RdyError::new(
        E_IO,
        "the config file could not be saved durably",
        "check the config path and available disk space, then try again",
    )
}

/// E_LIMIT: the file or its replacement is over `limit` bytes.
fn too_large(limit: usize) -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("config input exceeds {limit} bytes"),
        "reduce the config file to the stated limit",
    )
}

fn parent_of(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// `<path>.lock`.
fn lock_path(path: &Path) -> Result<PathBuf, RdyError> {
    let Some(name) = path.file_name() else {
        return Err(save_failed());
    };
    let mut lock = name.to_os_string();
    lock.push(".lock");
    Ok(path.with_file_name(lock))
}

/// Whether `m` is a real directory (not a link to one) that this user owns, in any mode.
fn own_dir(m: &fs::Metadata) -> bool {
    m.is_dir() && pio::owner(m) == pio::own_uid()
}

/// Checks the file's parent directory: an existing one is a real directory this user owns
/// (any mode, never changed); a missing one is created, with its missing ancestors, private
/// and durably.
fn ensure_parent(parent: &Path) -> Result<(), RdyError> {
    match fs::symlink_metadata(parent) {
        Ok(m) if own_dir(&m) => Ok(()),
        Ok(_) => Err(not_private()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => create_private_parents(parent),
        Err(_) => Err(save_failed()),
    }
}

/// Creates `dir` and its missing ancestors (mode 0o700), each followed by an fsync of its
/// parent, nearest the root first. The existing ancestor they are made in is the config home
/// or above it, which the caller selects; it is not judged.
fn create_private_parents(dir: &Path) -> Result<(), RdyError> {
    let mut missing = vec![];
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if d.as_os_str().is_empty() {
            break;
        }
        match fs::metadata(d) {
            Ok(m) if m.is_dir() => break,
            Ok(_) => return Err(save_failed()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => missing.push(d),
            Err(_) => return Err(save_failed()),
        }
        cur = d.parent();
    }
    for d in missing.into_iter().rev() {
        match pio::create_private_dir(d) {
            Ok(()) => {}
            // another process made it first; it is judged below as an existing parent
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(save_failed()),
        }
        fsutil::sync_dir(parent_of(d)).map_err(|_| save_failed())?;
    }
    match fs::symlink_metadata(dir) {
        Ok(m) if own_dir(&m) => Ok(()),
        Ok(_) => Err(not_private()),
        Err(_) => Err(save_failed()),
    }
}

/// Opens the lock file, never through a link; a new one is made 0o600 whatever the umask.
fn open_lock(lock: &Path) -> Result<File, RdyError> {
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create_new(true);
    let (file, created) = match pio::open_no_follow(pio::private_file(&mut opts), lock) {
        Ok(f) => (f, true),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let mut opts = OpenOptions::new();
            opts.read(true).write(true);
            match pio::open_no_follow(&mut opts, lock) {
                Ok(f) => (f, false),
                Err(_) => return Err(refused_or_failed(lock)),
            }
        }
        Err(_) => return Err(refused_or_failed(lock)),
    };
    if created {
        pio::make_private_file(&file).map_err(|_| save_failed())?;
    }
    let m = file.metadata().map_err(|_| save_failed())?;
    if !m.is_file() || pio::owner(&m) != pio::own_uid() {
        return Err(not_private());
    }
    Ok(file)
}

/// The error of an open of `path` that failed: a link (or anything else that is not a regular
/// file) at `path` is refused as not private; anything else is a failed save.
fn refused_or_failed(path: &Path) -> RdyError {
    match fs::symlink_metadata(path) {
        Ok(m) if !m.is_file() => not_private(),
        _ => save_failed(),
    }
}

/// Reads the target under the lock: empty bytes when it is missing; at most `limit` bytes.
fn read_target(path: &Path, private: bool, limit: usize) -> Result<Vec<u8>, RdyError> {
    // judged before the open, so a FIFO or a device is never opened
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => {}
        Ok(_) => return Err(not_private()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(_) => return Err(save_failed()),
    }
    let mut opts = OpenOptions::new();
    opts.read(true);
    let file = match pio::open_no_follow(&mut opts, path) {
        Ok(f) => f,
        Err(_) => return Err(refused_or_failed(path)),
    };
    let m = file.metadata().map_err(|_| save_failed())?;
    let ours = m.is_file() && pio::owner(&m) == pio::own_uid();
    if !ours || (private && !pio::is_private_file(&m)) {
        return Err(not_private());
    }
    let cap = u64::try_from(limit)
        .ok()
        .and_then(|l| l.checked_add(1))
        .ok_or_else(|| too_large(limit))?;
    let mut bytes = vec![];
    file.take(cap)
        .read_to_end(&mut bytes)
        .map_err(|_| save_failed())?;
    if bytes.len() > limit {
        return Err(too_large(limit));
    }
    Ok(bytes)
}
