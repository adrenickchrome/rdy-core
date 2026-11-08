//! What a test reads of a store's files on every platform (FW-XP1 phase 4).

use std::path::Path;

/// The bytes of the regular file `path`, whose metadata `meta` was read without following a
/// link, for a test's record of a tree. An empty file is empty without a read. On Windows a file
/// that another handle locks (a held store lock, a live index) cannot be read, so it is recorded
/// by its length, its identity and its last write time instead of its bytes: a change to any of
/// them still changes the record.
pub fn contents(path: &Path, meta: &std::fs::Metadata) -> Vec<u8> {
    if meta.len() == 0 {
        return vec![];
    }
    match std::fs::read(path) {
        Ok(bytes) => bytes,
        // ERROR_LOCK_VIOLATION: another handle holds a lock on the bytes
        #[cfg(windows)]
        Err(e) if e.raw_os_error() == Some(33) => format!(
            "locked: {} bytes, {}, written {:?}",
            meta.len(),
            rdy_store::fileid::FileId::lstat(path).unwrap(),
            meta.modified().unwrap()
        )
        .into_bytes(),
        Err(e) => panic!("{}: {e}", path.display()),
    }
}
