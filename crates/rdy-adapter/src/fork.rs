//! A hub fork from a snapshot (D-99CWC, D-DZDB7, D-HX6BX): a new holder whose canon ends at a
//! captured tag's landing. Core makes it (`rdy_engine::fork`); this crate opens it.
use crate::store::{Snapshot, StoreHandle};
use rdy_engine::RdyError;
use rdy_engine::fork::{ForkHook, fork_holder_at, fork_holder_at_with};
use std::path::Path;

impl Snapshot {
    /// Forks this holder snapshot at its captured tag `tag` into `dir`, and returns the new
    /// holder, open. The fork has a new repo id, and its identity's origin names this store's
    /// repo, the tag and the tag's landing (`fork: true`). Its canon is that landing. It holds
    /// only the tags on that landing's chain, and no view, held chain or replaced mark.
    ///
    /// The export reads only what this snapshot captured. Every removal stays applied, and later
    /// proof records are kept, but no read reaches them. `dir` must be absent or an empty
    /// directory. A refusal or failure leaves this store and `dir` as they were. A project
    /// snapshot is refused (`this store is a project`), a tag the snapshot lacks is `no version
    /// <name>`, and a removal since the snapshot is `E_REDACTED_STATE`.
    pub fn fork_at(&self, tag: &str, dir: &Path) -> Result<StoreHandle, RdyError> {
        let rdy = fork_holder_at(self.read_state(), tag, dir)?;
        Ok(StoreHandle::from_rdy(rdy))
    }

    /// Test hook: [`Snapshot::fork_at`] with `hook` at each step of the fork. No product path
    /// reaches it.
    #[doc(hidden)]
    pub fn fork_at_with_hook_for_tests(
        &self,
        tag: &str,
        dir: &Path,
        hook: ForkHook<'_>,
    ) -> Result<StoreHandle, RdyError> {
        let rdy = fork_holder_at_with(self.read_state(), tag, dir, hook)?;
        Ok(StoreHandle::from_rdy(rdy))
    }
}
