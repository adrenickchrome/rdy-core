//! A read-only follower of a source store (D-AA4FD), for a host. It holds the store's one
//! exclusive lock while it lives. `maintenance::seed_follower` makes one. No write reaches it, and
//! no engine handle escapes.
use crate::store::{Snapshot, snapshot_of};
use rdy_engine::follower::{FollowReport, FollowerCore};
use rdy_engine::{Location, RdyError};
use std::path::Path;

/// One open follower. Dropping it closes the store, which writes no batch.
pub struct Follower {
    core: FollowerCore,
}

impl Follower {
    /// Opens the follower at `dir`. A store with no valid mark is refused (`E_DUMP_CHAIN`), and
    /// a store open elsewhere is `E_LOCKED`.
    pub fn open(dir: &Path) -> Result<Self, RdyError> {
        Ok(Follower {
            core: FollowerCore::open(dir)?,
        })
    }

    /// The end of the follower's log.
    pub fn end(&self) -> Result<Location, RdyError> {
        self.core.end()
    }

    /// Follows the one increment at `increment`: its batches at their source places, its removals
    /// and project files, then a new read state and the new mark. A repeat of an increment already
    /// written appends nothing. A failed follow keeps the source batches it completed, and
    /// `snapshot` refuses until the same increment is retried. A store that cannot reopen after
    /// such a stop is seeded again from a new full dump (`E_REDACTION_CORRUPT`).
    pub fn follow(&mut self, increment: &Path) -> Result<FollowReport, RdyError> {
        self.core.follow(increment)
    }

    /// A read snapshot, as `StoreHandle::snapshot` takes one. A follower with an unfinished
    /// increment refuses it (`E_DUMP_CHAIN`).
    pub fn snapshot(&self) -> Result<Snapshot, RdyError> {
        snapshot_of(self.core.read()?)
    }
}
