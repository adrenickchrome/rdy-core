//! The explicit landing check and the gate inputs of a push through the adapter (D-0WWYR,
//! D-ME2HD as refined, D-WDKWQ).
//!
//! The check is core's (`rdy_engine::landing_check`): a landing is judged as a fresh propose of
//! its approved version on the landing's own `prev` would judge it, with the pack and gate
//! settings the caller passes ([`CheckInputs`]) and every other input read from records. It reads
//! no view, no project file and no user config, judges no freshness and writes nothing. This
//! module checks the inputs, pins the landing to the snapshot's captured histories and returns
//! core's result. Who approved and the signatures are the hub's to judge, not this check's.
//!
//! [`Snapshot::check_landing`] checks a landing the holder stores.
//! [`Snapshot::check_pushed_landing`] checks a landing of a checked quarantine before the hub's
//! move, the first push included: the holder stores nothing until the move.
//!
//! [`Snapshot::capture_gate_inputs`] reads a project's pack and gate settings once, for a push:
//! the hub keeps them, and a holder cannot rebuild them from a digest.

use crate::store::Snapshot;
use crate::sync::CheckedQuarantine;
use rdy_engine::history_facts;
use rdy_engine::{E_BAD_INPUT, RdyError};
use rdy_model::Hash;
use rdy_store::holder::StoreMode;

pub use rdy_engine::landing_check::{CheckInputs, LandingCheck};
pub use rdy_engine::{CoverageGate, ProposeCadence};
pub use rdy_model::assessment::AssessedPolicy;
pub use rdy_schema::{Issue, Pack};

impl Snapshot {
    /// The check of landing `landing` with `inputs` (see the module docs). The inputs are
    /// checked first (`E_BAD_INPUT`). The landing must lie in a history the snapshot captured:
    /// canon's, a tag's or a `held/` chain's (`E_NOT_FOUND` otherwise). A removal since the
    /// snapshot is `E_REDACTED_STATE`.
    pub fn check_landing(
        &self,
        landing: Hash,
        inputs: &CheckInputs,
    ) -> Result<LandingCheck, RdyError> {
        inputs.check()?;
        let read = self.read_state();
        let heads: Vec<Hash> = self
            .canon()
            .into_iter()
            .chain(self.captured_tags().values().map(|(_, tag)| tag.landing))
            .chain(self.captured_held().iter().copied())
            .collect();
        if !history_facts::reachable(read, &heads, landing)? {
            return Err(history_facts::not_reachable(landing));
        }
        let record = read.reader().landing(&landing)?;
        let out = rdy_engine::landing_check::check_landing(read, &record, inputs)?;
        read.ensure_current_epoch()?;
        Ok(out)
    }

    /// The check of landing `landing` of the push `checked`, before the hub's move (D-0WWYR:
    /// the landing and its `prev` may be in the quarantine). `checked` must have been checked on
    /// this snapshot (`E_BAD_INPUT` otherwise), and the inputs are checked first. A landing the
    /// push's records do not hold is [`Snapshot::check_landing`]'s. A pushed landing is checked
    /// by core's evaluator in a scratch holder: a private temporary holder loaded from a dump of
    /// this snapshot's published state, so it holds every record the holder held (canon, tags,
    /// `held/` chains and views), and then the push's checked records, by the same move. The
    /// batch files are not read again. The holder itself stores nothing, and its canon does not
    /// move: the hub's [`StoreHandle::move_canon`](crate::StoreHandle::move_canon) still
    /// compares the expected tip. The scratch is made at the first check of `checked` and kept
    /// with it. A removal since the snapshot is `E_REDACTED_STATE`.
    pub fn check_pushed_landing(
        &self,
        checked: &CheckedQuarantine,
        landing: Hash,
        inputs: &CheckInputs,
    ) -> Result<LandingCheck, RdyError> {
        inputs.check()?;
        checked.ensure_bound(self)?;
        let read = self.read_state();
        read.ensure_current_epoch()?;
        if !checked.pushes(&landing) {
            return self.check_landing(landing, inputs);
        }
        let scratch = checked.scratch(self)?;
        let out = scratch.snapshot().check_landing(landing, inputs)?;
        read.ensure_current_epoch()?;
        Ok(out)
    }

    /// The gate inputs of a push from this project: its effective pack in full and the gate
    /// settings its config gave when the snapshot's state was published. A holder has none of
    /// its own and is refused (`E_BAD_INPUT`).
    pub fn capture_gate_inputs(&self) -> Result<CheckInputs, RdyError> {
        let read = self.read_state();
        if read.reader().mode()? == StoreMode::Holder {
            return Err(project_only("gate inputs"));
        }
        read.ensure_current_epoch()?;
        Ok(CheckInputs::of_state(read))
    }
}

/// `E_BAD_INPUT`: inputs that only a project holds, asked of a holder.
pub(crate) fn project_only(what: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("a holder has no {what}"),
        format!("capture the {what} in the project that pushes"),
    )
}
