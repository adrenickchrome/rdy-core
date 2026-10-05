//! The project's store layout: the store lives in `.rdy/store/`. Anything at that path that is
//! not a directory (an M2.0 project's stub file, for one) is a store of another rdy build and is
//! refused before anything is locked or written.
use crate::error::StoreError;
use crate::format;
use crate::fsutil;
use crate::store::Store;
use std::io::ErrorKind;
use std::path::Path;

/// The store directory inside `.rdy`.
pub const STORE_DIR: &str = "store";

/// Opens the store of the project whose `.rdy` directory is `rdy_dir`, creating the store
/// directory durably if it is missing (dur1).
pub fn open_project_store(rdy_dir: &Path) -> Result<Store, StoreError> {
    let dir = rdy_dir.join(STORE_DIR);
    match std::fs::symlink_metadata(&dir) {
        Ok(m) if m.file_type().is_dir() => {}
        Ok(_) => return Err(format::other_build(&dir, "is not a directory")),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            fsutil::create_dir_durable(&dir)?;
        }
        Err(e) => return Err(e.into()),
    }
    Store::open(&dir)
}
