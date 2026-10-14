//! The project's store layout: the store lives in `.rdy/store/`. Anything at that path that is
//! not a directory (an M2.0 project's stub file, for one) is a store of another rdy build and is
//! refused before anything is locked or written.
use crate::error::StoreError;
use crate::store::{Store, StoreLock};
use std::path::Path;

/// The store directory inside `.rdy`.
pub const STORE_DIR: &str = "store";

/// Opens the store of the project whose `.rdy` directory is `rdy_dir`. A missing store is
/// refused, never created: only `create_project_store` (rdy init) creates one.
pub fn open_project_store(rdy_dir: &Path) -> Result<Store, StoreError> {
    Store::open(&rdy_dir.join(STORE_DIR))
}

/// Opens the store of the project whose `.rdy` directory is `rdy_dir` under `lock`, which the
/// caller took on that store and keeps (see `Store::open_locked`).
pub fn open_project_store_locked(rdy_dir: &Path, lock: &StoreLock) -> Result<Store, StoreError> {
    Store::open_locked(&rdy_dir.join(STORE_DIR), lock)
}

/// Creates the store of a new project (rdy init), durably (dur1); see `Store::create`.
pub fn create_project_store(rdy_dir: &Path) -> Result<Store, StoreError> {
    Store::create(&rdy_dir.join(STORE_DIR))
}
