//! The project's store layout: the store lives in `.rdy/store/`. Anything at that path that is
//! not a directory (an M2.0 project's stub file, for one) is a store of another rdy build and is
//! refused before anything is locked or written.
use crate::error::StoreError;
use crate::pio;
use crate::start_lock::StartGuard;
use crate::store::{OpenPolicy, Store, StoreLock};
use std::path::Path;

/// The store directory inside `.rdy`.
pub const STORE_DIR: &str = "store";
/// The store directory a load writes inside `.rdy` before it installs it as [`STORE_DIR`].
pub const LOADING_DIR: &str = "store.loading";
/// The run directory inside `.rdy`.
pub const RUN_DIR: &str = "run";
/// The project's start lock in the run directory (see `start_lock`).
pub const START_LOCK_FILE: &str = "start.lock";

/// Renames the finished `.rdy/store.loading` of the project whose `.rdy` directory is `rdy_dir` to
/// `.rdy/store`, the one installation of every load. `lock` is that store's lock, which this call
/// closes first: no process can rename a directory on Windows while a file below it is open, so
/// the caller has closed every other handle below it. From here until the caller opens the
/// installed store under a new lock, only `start` keeps the store to the caller. `before_rename`
/// runs between the close and the rename; its error fails the call. A guard of another project,
/// or a lock that has another holder, is `Locked` with nothing renamed. The caller flushes
/// `.rdy`.
pub fn install_loading_store(
    rdy_dir: &Path,
    start: &StartGuard,
    lock: StoreLock,
    before_rename: impl FnOnce() -> std::io::Result<()>,
) -> Result<(), StoreError> {
    let loading = rdy_dir.join(LOADING_DIR);
    if !start.guards(rdy_dir) || lock.close().is_err() {
        return Err(StoreError::Locked(loading));
    }
    before_rename()?;
    pio::rename_durable(&loading, &rdy_dir.join(STORE_DIR))?;
    Ok(())
}

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

/// [`open_project_store`] with upgrade policy `policy` (see `Store::open_with_policy`).
pub fn open_project_store_with_policy(
    rdy_dir: &Path,
    policy: OpenPolicy,
) -> Result<Store, StoreError> {
    Store::open_with_policy(&rdy_dir.join(STORE_DIR), policy)
}

/// [`open_project_store_locked`] with upgrade policy `policy` (see
/// `Store::open_locked_with_policy`).
pub fn open_project_store_locked_with_policy(
    rdy_dir: &Path,
    lock: &StoreLock,
    policy: OpenPolicy,
) -> Result<Store, StoreError> {
    Store::open_locked_with_policy(&rdy_dir.join(STORE_DIR), lock, policy)
}

/// Opens the follower store whose `.rdy` directory is `rdy_dir` (see `Store::open_follower`).
pub fn open_follower_store(rdy_dir: &Path) -> Result<Store, StoreError> {
    Store::open_follower(&rdy_dir.join(STORE_DIR))
}

/// Creates the store of a new project (rdy init), durably (dur1); see `Store::create`.
pub fn create_project_store(rdy_dir: &Path) -> Result<Store, StoreError> {
    Store::create(&rdy_dir.join(STORE_DIR))
}
