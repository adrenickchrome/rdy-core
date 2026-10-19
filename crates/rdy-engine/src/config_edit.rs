//! One config file, locked for an edit: the bytes read under a blocking lock, and a durable
//! replacement while the lock is held.
//!
//! A [`LockedConfig`] holds the exclusive lock of the file's stable sibling `<file>.lock` from
//! [`LockedConfig::open`] until it is dropped, so two processes that edit one file take turns.
//! The lock file is created private and is never unlinked, so every process locks the same
//! file. The type knows bytes only: the caller selects the config home and the file below it,
//! parses the bytes and keeps the values of other layers.
//!
//! The caller's config home is trusted as given, links included. Below it, every directory,
//! the file and its lock are opened one entry at a time without following links, and must be
//! this user's: a symbolic link, a foreign owner and a target that is not a regular file are
//! refused. These directories, and the home when it holds the file itself, also have a write
//! rule. For a secret file (`private`), group and others must not be able to write them, so no
//! other user can swap a temp file or the target. For a settings file, the group may write them:
//! under a umask of 002, `rdy init` makes `.rdy/` group-writable, and a group that can write it
//! can already change the store. All users may write such a directory only when its sticky bit
//! is set, so no other user can remove or replace this user's entries. A secret file must also
//! be mode 0o600, and a lock must give group and others no access; a settings file keeps its mode
//! through a replacement. Nothing here changes the mode of any directory, or of an existing
//! file. mkdir returns no handle, so no call proves that the directory opened after it is the one
//! it made: a directory is made only when the umask keeps 0o700 whole, and what then stands at
//! its name is used only if it is a 0o700 directory of this user, as that mkdir makes it.
//!
//! The lock, the read and the replacement all go through the handle of the directory checked
//! at open, so a path changed later cannot redirect them, and a replacement first checks that
//! the path still leads to that directory.

use crate::error::{E_IO, E_LIMIT, RdyError};
use rdy_store::fileid::FileId;
use rdy_store::{fsutil, pio};
use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A config file's bytes, read under its sibling lock, which is held until drop.
pub struct LockedConfig {
    home: PathBuf,
    rel: PathBuf,
    private: bool,
    limit: usize,
    bytes: Vec<u8>,
    /// The directory that holds the file, as checked at open.
    dir: File,
    dir_id: FileId,
    /// `dir`'s path at open, for the sync recorder and test probe only.
    dir_path: PathBuf,
    name: OsString,
    lock: File,
}

impl LockedConfig {
    /// Locks the file `rel` below the config home `home` through `<file>.lock` (waiting for
    /// another holder), then reads the file. `home` is the directory the caller selected (the
    /// user config directory, or a project root); it must exist and is trusted as given.
    /// `rel` is a plain relative path (no `.`, `..`, root or empty component), such as
    /// `rdy/credentials.yaml`.
    ///
    /// A missing file reads as empty bytes and is not created. Missing directories below
    /// `home` are created 0o700, durably; a umask that takes one of the owner's bits from 0o700
    /// is a failed save, with nothing made. `private` is true for a secret file, which must be
    /// mode 0o600. A file over `limit` bytes is `E_LIMIT`.
    pub fn open(
        home: &Path,
        rel: &Path,
        private: bool,
        limit: usize,
    ) -> Result<LockedConfig, RdyError> {
        let (dirs, name) = split_rel(rel)?;
        let (dir, dir_path) = walk(home, &dirs, true, private)?;
        let dir_id = FileId::of_file(&dir).map_err(|_| save_failed())?;
        let lock = open_lock(&dir, &lock_name(name))?;
        lock.lock().map_err(|_| save_failed())?;
        // the lock is held from here; dropping `held` releases it on every error below
        let mut held = LockedConfig {
            home: home.to_path_buf(),
            rel: rel.to_path_buf(),
            private,
            limit,
            bytes: vec![],
            dir,
            dir_id,
            dir_path,
            name: name.to_os_string(),
            lock,
        };
        // the wait for the lock may be long: the path must still lead to the same directory
        held.check_bound()?;
        held.bytes = read_target(&held.dir, &held.name, private, limit)?;
        Ok(held)
    }

    /// The file's bytes as read under the lock, or as last replaced.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Replaces the file's bytes durably while the lock is held: the old bytes stay until the
    /// complete new bytes are renamed into place. A secret file ends 0o600; a settings file keeps
    /// its mode, and a new one is 0o600. Bytes over the limit are `E_LIMIT`, refused before any
    /// temp file is made.
    ///
    /// The path must still lead to the directory checked at open, with the same checks, and a
    /// link or other non-regular file at the target is refused, before any temp file is made.
    /// The write goes through that directory's handle. If the path no longer leads to it after
    /// the write, that is a privacy error, with the new bytes in the directory checked at open.
    /// A failure of the directory fsync after the rename is an error, with the new bytes
    /// possibly in place.
    pub fn replace(&mut self, bytes: &[u8]) -> Result<(), RdyError> {
        if bytes.len() > self.limit {
            return Err(too_large(self.limit));
        }
        self.check_bound()?;
        let keep = match pio::open_read_at(&self.dir, &self.name) {
            Ok(f) => {
                let m = f.metadata().map_err(|_| save_failed())?;
                check_target(&m, self.private)?;
                (!self.private).then(|| m.permissions())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(open_failed(&e)),
        };
        fsutil::replace_in_dir(&self.dir, &self.dir_path, &self.name, bytes, keep.as_ref())
            .map_err(|_| save_failed())?;
        self.check_bound()?;
        self.bytes = bytes.to_vec();
        Ok(())
    }

    /// Whether the path from the home still leads to the directory checked at open, with every
    /// check of [`walk`] passing again; nothing is created.
    fn check_bound(&self) -> Result<(), RdyError> {
        let (dirs, _) = split_rel(&self.rel)?;
        let (dir, _) = walk(&self.home, &dirs, false, self.private)?;
        match FileId::of_file(&dir) {
            Ok(id) if id == self.dir_id => Ok(()),
            Ok(_) => Err(not_private()),
            Err(_) => Err(save_failed()),
        }
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

/// The directories of `rel` and its file name. `rel` must be valid UTF-8 and plain: relative,
/// not empty, with no `.`, `..` or empty component (a programming error guard: callers pass
/// constants).
fn split_rel(rel: &Path) -> Result<(Vec<&OsStr>, &OsStr), RdyError> {
    let text = rel.to_str().ok_or_else(not_private)?;
    let mut parts: Vec<&OsStr> = vec![];
    for part in text.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(not_private());
        }
        parts.push(OsStr::new(part));
    }
    let name = parts.pop().ok_or_else(not_private)?;
    Ok((parts, name))
}

/// `<name>.lock`.
fn lock_name(name: &OsStr) -> OsString {
    let mut lock = name.to_os_string();
    lock.push(".lock");
    lock
}

/// Opens `home` (links followed: the caller trusts it), then each of `dirs` below it, one entry
/// at a time and never through a link. Each must be a directory this user owns that passes the
/// write rule of [`check_dir`] for `private`; so must the last one, which holds the file, even
/// when it is the home. With `create`, a missing directory is made 0o700 and synced with its
/// parent; without it, a missing directory means the path changed. Returns the last directory
/// and its path.
fn walk(
    home: &Path,
    dirs: &[&OsStr],
    create: bool,
    private: bool,
) -> Result<(File, PathBuf), RdyError> {
    let mut cur = pio::open_dir(home).map_err(|_| not_private())?;
    let mut cur_path = home.to_path_buf();
    for &part in dirs {
        let next = match pio::open_dir_at(&cur, part) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound && create => {
                make_dir(&cur, &cur_path, part)?
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(not_private()),
            Err(e) => return Err(open_failed(&e)),
        };
        check_dir(&next, private)?;
        cur = next;
        cur_path.push(part);
    }
    if dirs.is_empty() {
        check_dir(&cur, private)?;
    }
    Ok((cur, cur_path))
}

/// Makes the directory `part` in `parent` (at `parent_path`) and opens it. One that another
/// process made first is opened and judged as an existing one.
///
/// No directory is ever chmodded here: mkdir returns no handle, so nothing proves that the
/// directory opened at `part` afterwards is the one it made. So the umask is checked first, with
/// a probe file (see [`probe_name`]): a umask that takes an owner bit from 0o700 is a failed
/// save, and nothing is made. After the
/// mkdir, what stands at `part` is used only if it is what that mkdir made under this umask: a
/// 0o700 directory (not a link) that this user owns. Anything else is refused and keeps its mode.
/// Then the directory and its parent are synced.
fn make_dir(parent: &File, parent_path: &Path, part: &OsStr) -> Result<File, RdyError> {
    let probe = probe_name(part);
    match pio::remove_file_at(parent, &probe) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(save_failed()),
        _ => {}
    }
    if !pio::umask_keeps_0700_at(parent, &probe).map_err(|_| save_failed())? {
        return Err(save_failed());
    }
    match pio::create_private_dir_at(parent, part) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return match pio::open_dir_at(parent, part) {
                Ok(f) => Ok(f),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Err(not_private()),
                Err(e) => Err(open_failed(&e)),
            };
        }
        Err(_) => return Err(save_failed()),
    }
    fsutil::probe_dir_made(&parent_path.join(part)).map_err(|_| save_failed())?;
    let dir = match pio::open_dir_at(parent, part) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(not_private()),
        Err(e) => return Err(open_failed(&e)),
    };
    let m = dir.metadata().map_err(|_| save_failed())?;
    if !m.is_dir() || pio::owner(&m) != pio::own_uid() || !pio::is_private_dir(&m) {
        return Err(not_private());
    }
    dir.sync_all().map_err(|_| save_failed())?;
    parent.sync_all().map_err(|_| save_failed())?;
    fsutil::record(parent_path);
    Ok(dir)
}

/// `.<name>.<pid>-<n>.tmp`, the umask probe file's name beside a new directory `name`: one name
/// per process and call, so two opens that make one directory at once never remove or block each
/// other's probe. Whatever stands at it (left by a crash) is removed first, as at a temp name.
fn probe_name(name: &OsStr) -> OsString {
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let n = CALLS.fetch_add(1, Ordering::Relaxed);
    let mut tmp = OsString::from(".");
    tmp.push(name);
    tmp.push(format!(".{}-{n}.tmp", std::process::id()));
    tmp
}

/// A directory of the config path: this user's, and safe to write the file in. For a secret
/// file (`private`), group and others cannot write it. For a settings file, the group may write
/// it, and all users may write it only when the sticky bit is set.
fn check_dir(dir: &File, private: bool) -> Result<(), RdyError> {
    let m = dir.metadata().map_err(|_| save_failed())?;
    let shared = if private {
        pio::is_shared_writable(&m)
    } else {
        pio::is_world_writable_unsticky(&m)
    };
    if m.is_dir() && pio::owner(&m) == pio::own_uid() && !shared {
        Ok(())
    } else {
        Err(not_private())
    }
}

/// The target, judged from its open handle: a regular file this user owns, and 0o600 when
/// `private`.
fn check_target(m: &Metadata, private: bool) -> Result<(), RdyError> {
    let ours = m.is_file() && pio::owner(m) == pio::own_uid();
    if ours && (!private || pio::is_private_file(m)) {
        Ok(())
    } else {
        Err(not_private())
    }
}

/// The error of an open in a directory that failed for a reason other than a missing entry: a
/// link (or a non-directory where a directory is needed) is refused as not private; anything
/// else is a failed save.
fn open_failed(e: &io::Error) -> RdyError {
    if pio::is_link_refusal(e) {
        not_private()
    } else {
        save_failed()
    }
}

/// Opens the lock file `name` in `dir`, never through a link. A new one is made 0o600 whatever
/// the umask. An existing one must be a regular file this user owns that gives group and others
/// no access; it is opened for reading, which is all the lock needs, and never chmodded.
fn open_lock(dir: &File, name: &OsStr) -> Result<File, RdyError> {
    let file = match pio::create_private_at(dir, name) {
        Ok(f) => {
            pio::make_private_file(&f).map_err(|_| save_failed())?;
            f
        }
        // whatever stands there, a link included, is opened without following it, and judged
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            pio::open_read_at(dir, name).map_err(|e| open_failed(&e))?
        }
        Err(e) => return Err(open_failed(&e)),
    };
    let m = file.metadata().map_err(|_| save_failed())?;
    if !m.is_file() || pio::owner(&m) != pio::own_uid() || !pio::is_owner_only(&m) {
        return Err(not_private());
    }
    Ok(file)
}

/// Reads the target `name` in `dir` under the lock: empty bytes when it is missing; at most
/// `limit` bytes. It is opened without following a link and without waiting on a FIFO, then
/// judged from the handle.
fn read_target(dir: &File, name: &OsStr, private: bool, limit: usize) -> Result<Vec<u8>, RdyError> {
    let file = match pio::open_read_at(dir, name) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(open_failed(&e)),
    };
    let m = file.metadata().map_err(|_| save_failed())?;
    check_target(&m, private)?;
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
