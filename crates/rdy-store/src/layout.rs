//! The project's store layout (plan M2.0, fmt1 and upg1): the store lives in `.rdy/store2/`, and
//! `.rdy/store` is a stub file that M1 binaries fail on before they lock or write anything.
//!
//! An M1 project has a store directory at `.rdy/store`. The first M2 open moves it to
//! `.rdy/store2` (under the old store's lock, so a running M1 process blocks the move), leaves
//! the stub in its place and then upgrades the log inside `store2`.
use crate::error::StoreError;
use crate::fsutil;
use crate::store::{Store, lock_error};
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The store directory inside `.rdy`.
pub const STORE_DIR: &str = "store2";
/// The M1 store directory, now the name of the upgrade stub.
pub const LEGACY_DIR: &str = "store";
/// The exact content of the stub file `.rdy/store`.
pub const STUB_TEXT: &str = "this store was upgraded by a newer rdy (M2 or later); upgrade rdy\n";

fn format_error(message: String, fix: &str) -> StoreError {
    StoreError::Format {
        message,
        fix: fix.to_string(),
    }
}

/// What `.rdy/store` is.
enum Old {
    Dir,
    File,
    Missing,
    /// A symbolic link (an M1 store kept elsewhere); never followed.
    Link,
    Other,
}

fn classify(old: &Path) -> Result<Old, StoreError> {
    match std::fs::symlink_metadata(old) {
        Ok(m) if m.file_type().is_dir() => Ok(Old::Dir),
        Ok(m) if m.file_type().is_file() => Ok(Old::File),
        Ok(m) if m.file_type().is_symlink() => Ok(Old::Link),
        Ok(_) => Ok(Old::Other),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Old::Missing),
        Err(e) => Err(e.into()),
    }
}

/// The log of the store at `dir`: the bytes in all its `segments/seg-*.log` files, and the mtime
/// of the highest-numbered one (the epoch if there is none).
fn log_summary(dir: &Path) -> (u64, SystemTime) {
    let mut bytes = 0;
    let mut best: Option<(u32, PathBuf)> = None;
    if let Ok(entries) = std::fs::read_dir(dir.join("segments")) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let id = name
                .to_str()
                .and_then(|n| n.strip_prefix("seg-"))
                .and_then(|n| n.strip_suffix(".log"))
                .filter(|n| n.len() >= 6 && n.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|n| n.parse::<u32>().ok());
            let Some(id) = id else { continue };
            bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
            if best.as_ref().is_none_or(|(b, _)| id > *b) {
                best = Some((id, entry.path()));
            }
        }
    }
    let mtime = best
        .and_then(|(_, p)| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    (bytes, mtime)
}

/// Both `.rdy/store` and `.rdy/store2` are store directories. The one named as holding the newer
/// log is the non-empty one if only one holds frames (an older rdy that ran after the stub was
/// lost recreates `.rdy/store` with an empty log), else the one whose last segment is newer,
/// `.rdy/store2` on a tie.
fn both_are_stores(old: &Path, new: &Path) -> StoreError {
    let ((old_bytes, old_time), (new_bytes, new_time)) = (log_summary(old), log_summary(new));
    let newer = match (old_bytes == 0, new_bytes == 0) {
        (true, true) => "; both are empty".to_string(),
        (true, false) => format!(
            "; {} holds the newer log ({} is empty)",
            new.display(),
            old.display()
        ),
        (false, true) => format!(
            "; {} holds the newer log ({} is empty)",
            old.display(),
            new.display()
        ),
        (false, false) => format!(
            "; {} holds the newer log",
            if old_time > new_time { old } else { new }.display()
        ),
    };
    format_error(
        format!(
            "both {} and {} are store directories: an older rdy wrote to {} after this store was \
             upgraded{newer}",
            old.display(),
            new.display(),
            old.display(),
        ),
        "upgrade every rdy that opens this project, then keep the store with the newer log as \
         .rdy/store2 and replace .rdy/store with the upgrade stub; move the other directory \
         aside, don't delete it",
    )
}

/// Moves the M1 store directory `old` to `new` under `old`'s lock. Returns `Ok(false)` if `old`
/// vanished before it could be locked (another process moved it), so the caller starts over.
fn move_store(rdy: &Path, old: &Path, new: &Path) -> Result<bool, StoreError> {
    let lock = match OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(old.join("lock"))
    {
        Ok(f) => f,
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Ok(false);
        }
        Err(e) => return Err(e.into()),
    };
    // an M1 process holding the store gives E_LOCKED and nothing moves
    lock.try_lock().map_err(|e| lock_error(old, e))?;
    std::fs::rename(old, new)?;
    fsutil::sync_dir(rdy)?;
    fsutil::write_durable(old, STUB_TEXT.as_bytes())?;
    drop(lock);
    Ok(true)
}

/// Opens the store of the project whose `.rdy` directory is `rdy_dir`, moving an M1 store to
/// `.rdy/store2` and writing the upgrade stub as needed. See the module docs.
pub fn open_project_store(rdy_dir: &Path) -> Result<Store, StoreError> {
    let old = rdy_dir.join(LEGACY_DIR);
    let new = rdy_dir.join(STORE_DIR);
    let mut retried = false;
    loop {
        let new_is_dir = new.is_dir();
        match (classify(&old)?, new_is_dir) {
            (Old::Dir, true) => return Err(both_are_stores(&old, &new)),
            (Old::Dir, false) => {
                if move_store(rdy_dir, &old, &new)? {
                    return Store::open(&new);
                }
                if retried {
                    return Err(StoreError::Io(std::io::Error::new(
                        ErrorKind::NotFound,
                        format!("{} vanished while it was being upgraded", old.display()),
                    )));
                }
                retried = true;
            }
            (Old::File, true) => return Store::open(&new),
            (Old::File, false) => {
                return Err(format_error(
                    format!(
                        "{} marks a store upgraded by a newer rdy, but {} is missing",
                        old.display(),
                        new.display()
                    ),
                    "restore .rdy/store2 from backup",
                ));
            }
            (Old::Missing, true) => {
                // finishes a crash between the move's rename and its stub
                fsutil::write_durable(&old, STUB_TEXT.as_bytes())?;
                return Store::open(&new);
            }
            (Old::Missing, false) => {
                fsutil::create_dir_durable(&new)?;
                fsutil::write_durable(&old, STUB_TEXT.as_bytes())?;
                return Store::open(&new);
            }
            (Old::Link, _) => {
                return Err(format_error(
                    format!(
                        "{} is a symbolic link, neither the store directory nor the upgrade stub",
                        old.display()
                    ),
                    "replace the link with the directory it points to (or move that directory to \
                     .rdy/store), then reopen",
                ));
            }
            (Old::Other, _) => {
                return Err(format_error(
                    format!(
                        "{} is neither the store directory nor the upgrade stub",
                        old.display()
                    ),
                    "move it aside, then reopen",
                ));
            }
        }
    }
}
