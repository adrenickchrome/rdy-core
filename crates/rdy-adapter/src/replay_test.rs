//! The replay test of a chain of landings onto one landing's state, with each replayed
//! landing's gate result (D-2QX9W Replay test and Gate result, D-N0NVF, D-0WWYR).
//!
//! The test is core's (`rdy_engine::replay_test`): it runs a pull replay's judging steps on each
//! landing, oldest first, and stops where the pull would stop, for the same cause. This module
//! checks the inputs, resolves `onto` with the adapter's one scope resolver, pins the answer to
//! the snapshot's captured refs and returns core's result. It writes nothing.
//!
//! The pack and gate settings are checked first, as [`Snapshot::check_landing`] checks them
//! (`E_BAD_INPUT`). The chain and `onto` are then checked as the rebase test checks them
//! ([`crate::version_tests`]): at most 100,000 hashes, each once, `onto` the state of one
//! landing, and each hash a landing that D-Y32MN's reach finds. An empty chain replays nothing.

use crate::store::Snapshot;
use rdy_engine::RdyError;
use rdy_engine::landing_check::CheckInputs;
use rdy_model::Hash;

pub use rdy_engine::landing_check::LandingCheck;
pub use rdy_engine::replay_test::{ReplayStop, ReplayStopCause, ReplayTest, ReplayedCheck};

impl Snapshot {
    /// Where a pull replay of `chain`, oldest first, onto the state at `onto` would stop, with
    /// the gate result of each landing it replays under the pack and gate settings `inputs`
    /// (D-2QX9W). The inputs are checked first (see the module docs). A removal since the
    /// snapshot is `E_REDACTED_STATE`.
    pub fn replay_test(
        &self,
        chain: &[Hash],
        onto: &str,
        inputs: &CheckInputs,
    ) -> Result<ReplayTest, RdyError> {
        inputs.check()?;
        let onto = self.version_test_inputs(chain, onto)?;
        let read = self.read_state();
        let out = rdy_engine::replay_test::replay_test(read, chain, onto, inputs)?;
        read.ensure_current_epoch()?;
        Ok(out)
    }
}
