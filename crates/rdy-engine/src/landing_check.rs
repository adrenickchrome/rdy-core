//! The explicit candidate evaluator (D-0WWYR): the readiness and policy verdict of one landing,
//! judged as a fresh propose of its approved version on the landing's `prev` would judge it.
//!
//! The candidate is the landing's `prev` plus the landing's changes, as one view. The caller
//! passes the effective pack in full and the gate settings ([`CheckInputs`]). Every other input
//! is a record: the proposal and its approved version, the changes, the bound plan, the briefing
//! records, the live policy block at the base, the tensions and the check verdicts. The check
//! reads no other view, no view's plan, no project file and no user config, so tensions with work
//! in flight elsewhere do not apply. Freshness does not apply either: the check computes
//! readiness on the landing's own `prev`. It reuses no stored assessment, and it writes nothing.
//!
//! The landing may be one the store holds, or a captured record whose changes and proposal the
//! state's reader holds (a store's reader, or a reader over a checked quarantine). It runs the
//! one readiness implementation ([`crate::readiness::assess`]) on the candidate's state
//! (`ReadState::candidate_state`); no rule is copied here.
//!
//! Who approved, and the signatures, are out of scope.

use crate::ReadState;
use crate::config::{CoverageGate, ProposeCadence};
use crate::error::{E_BAD_INPUT, RdyError};
use crate::readiness::assessment::{GateConfig, config_digest, pack_digest};
use crate::readiness::{Candidate, assess};
use rdy_model::assessment::AssessedPolicy;
use rdy_model::{Hash, LandingRecord, Proposal};
use rdy_schema::{Issue, Pack};
use std::cell::Cell;

/// What the caller passes (D-0WWYR): the effective pack in full and the gate settings readiness
/// reads. `require_up_to_date` enters only the config digest that `assessment_matches` compares:
/// the check never judges freshness.
#[derive(Clone, Debug)]
pub struct CheckInputs {
    pub pack: Pack,
    pub coverage_gate: CoverageGate,
    pub coverage_target: u32,
    pub decisions_home: Option<String>,
    pub propose_cadence: ProposeCadence,
    pub propose_changes: u32,
    pub require_up_to_date: bool,
}

impl CheckInputs {
    /// The inputs a published state holds: its pack and its gate settings, as the config read
    /// when it was published.
    pub fn of_state(st: &ReadState) -> CheckInputs {
        CheckInputs {
            pack: st.pack().clone(),
            coverage_gate: st.coverage_gate(),
            coverage_target: st.coverage_target(),
            decisions_home: st.decisions_home().map(str::to_string),
            propose_cadence: st.propose_cadence(),
            propose_changes: st.propose_changes(),
            require_up_to_date: st.require_up_to_date(),
        }
    }

    /// Checks the inputs a caller passes (`E_BAD_INPUT`), in the ranges the config keeps: a
    /// coverage target from 0 to 100, a propose changes count from 1 to 1000, and a decisions
    /// home that is not empty and has no spaces at its ends. The config drops such a value with
    /// a warning; an explicit input is refused instead.
    pub fn check(&self) -> Result<(), RdyError> {
        if self.coverage_target > 100 {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!(
                    "coverage target {} is outside 0 to 100",
                    self.coverage_target
                ),
                "pass a coverage target from 0 to 100",
            ));
        }
        if !(1..=1000).contains(&self.propose_changes) {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!(
                    "propose changes {} is outside 1 to 1000",
                    self.propose_changes
                ),
                "pass a propose changes count from 1 to 1000",
            ));
        }
        if let Some(home) = &self.decisions_home {
            if home.trim().is_empty() {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    "decisions home is empty",
                    "pass none for no decisions home",
                ));
            }
            if home.trim() != home {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    "decisions home has spaces at its ends",
                    "pass the decisions home without them",
                ));
            }
        }
        Ok(())
    }

    /// The gate settings the config digest follows (O18).
    pub fn gate_config(&self) -> GateConfig {
        GateConfig {
            require_up_to_date: self.require_up_to_date,
            coverage_gate: self.coverage_gate,
        }
    }
}

/// What [`check_landing`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LandingCheck {
    /// True exactly when `rows` is empty.
    pub pass: bool,
    /// Each readiness row, with its code, in readiness's closed order.
    pub rows: Vec<Issue>,
    /// The authority policy's verdict under the live policy at the landing's `prev`, when the
    /// candidate is ready; none when it is not.
    pub policy: Option<AssessedPolicy>,
    /// Whether the stored assessment of the approved version names the pack digest and the
    /// config digest of the inputs. False when the store holds no assessment of it.
    pub assessment_matches: bool,
}

/// The check of `landing` on `st` (see the module docs). `st`'s reader holds the landing's
/// changes, its approved version and every record readiness reads.
pub fn check_landing(
    st: &ReadState,
    landing: &LandingRecord,
    inputs: &CheckInputs,
) -> Result<LandingCheck, RdyError> {
    LANDING_CHECKS.with(|n| n.set(n.get() + 1));
    let version = landing.approved_version.unwrap_or(landing.proposal);
    let pref = crate::gate::version_ref(st, &version)?;
    let stored = st.reader().proposal(&version)?;
    let candidate = Proposal {
        canon_at: landing.prev,
        changes: landing.changes.clone(),
        ..stored
    };
    let cst = st.candidate_state(landing.prev, &candidate.view, &landing.changes, inputs);
    let gate = inputs.gate_config();
    let c = Candidate::stored(&cst, pref, candidate)?;
    let readiness = assess(&cst, &c, &gate)?.readiness;
    let assessment_matches = match stored_assessment(st, landing, &version)? {
        Some(a) => {
            let a = st.reader().assessment(&a)?;
            a.pack.digest == pack_digest(&inputs.pack) && a.config == config_digest(&gate)
        }
        None => false,
    };
    Ok(LandingCheck {
        pass: readiness.issues.is_empty(),
        rows: readiness.issues,
        policy: readiness.policy,
        assessment_matches,
    })
}

/// The assessment the landing names, else the store's latest of version `version`.
fn stored_assessment(
    st: &ReadState,
    landing: &LandingRecord,
    version: &Hash,
) -> Result<Option<Hash>, RdyError> {
    if let Some(a) = landing.assessment {
        return Ok(Some(a));
    }
    Ok(st.reader().assessment_of_proposal(version, st.seq())?)
}

thread_local! {
    /// This thread's runs of [`check_landing`].
    static LANDING_CHECKS: Cell<u64> = const { Cell::new(0) };
}

/// Tests only (as `readiness_calls_for_tests`): this thread's runs of [`check_landing`].
#[doc(hidden)]
pub fn landing_checks_for_tests() -> u64 {
    LANDING_CHECKS.with(Cell::get)
}
