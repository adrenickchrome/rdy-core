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
//! (`ReadState::candidate_state_over`); no rule is copied here.
//!
//! [`check_landing_over`] runs the same check on a `prev` state built in memory, with no stored
//! landing for it (D-2QX9W Gate result: a pull's replay, or a replay test). [`check_landing`] is
//! that check on the landing's own stored `prev`.
//!
//! Who approved, and the signatures, are out of scope.

use crate::ReadState;
use crate::config::{CoverageGate, ProposeCadence};
use crate::error::{E_BAD_INPUT, RdyError};
use crate::readiness::assessment::{GateConfig, config_digest, pack_digest};
use crate::readiness::{Candidate, assess};
use rdy_model::assessment::AssessedPolicy;
use rdy_model::{BlockId, Change, Hash, LandingRecord, Op, Proposal, StateSnapshot};
use rdy_schema::{Issue, Pack, RuleTestSpec};
use serde::Serialize;
use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, OnceLock};

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
    /// a warning; an explicit input is refused instead. Then each module rule of the pack, in the
    /// pack's order, before any rule is copied or compiled and whatever blocks it selects: its
    /// module, `args` and `depth` within the limits a pack file's rule meets
    /// ([`rdy_schema::check_module_test`]), refused as `invalid pack: <detail>` with the words a
    /// pack file's rule gets.
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
        for rule in &self.pack.rules {
            if let RuleTestSpec::Module(m) = &rule.test {
                rdy_schema::check_module_test(&rule.name, m).map_err(|detail| {
                    RdyError::new(
                        E_BAD_INPUT,
                        format!("invalid pack: {detail}"),
                        "pass a pack whose module rules are within the limits",
                    )
                })?;
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
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

/// The `prev` a landing is checked on: its state, and `at`, the last stored landing under it.
/// For a stored `prev`, `state` is that landing's own state, and its deletes read as of `at`.
/// A state built in memory (a replay or rebase test's merge onto a stored landing, D-2QX9W)
/// keeps the stored landing it was built on, and reads its deletes from [`ChainDeletes`].
#[derive(Clone, Debug)]
pub struct PrevState {
    pub at: Option<Hash>,
    pub state: Arc<StateSnapshot>,
    /// The deletes of a state built in memory; none for a stored landing's state.
    pub(crate) deletes: Option<Arc<ChainDeletes>>,
}

impl PrevState {
    /// The state of stored landing `at` (the empty state for none).
    pub fn stored(st: &ReadState, at: Option<Hash>) -> Result<PrevState, RdyError> {
        Ok(PrevState {
            at,
            state: Arc::new(st.reader().state_of(at)?),
            deletes: None,
        })
    }
}

/// The chain of a stored landing `onto`, each landing with its distance from `onto`, read once.
#[derive(Debug)]
pub(crate) struct OntoChain {
    onto: Option<Hash>,
    positions: OnceLock<Result<Arc<HashMap<Hash, usize>>, RdyError>>,
}

impl OntoChain {
    pub(crate) fn new(onto: Option<Hash>) -> Arc<OntoChain> {
        Arc::new(OntoChain {
            onto,
            positions: OnceLock::new(),
        })
    }

    fn positions(&self, st: &ReadState) -> Result<Arc<HashMap<Hash, usize>>, RdyError> {
        self.positions
            .get_or_init(|| {
                let chain = match self.onto {
                    Some(h) => crate::history_facts::walk(st, h)?,
                    None => vec![],
                };
                Ok(Arc::new(chain.into_iter().zip(0..).collect()))
            })
            .clone()
    }
}

/// Which change removed a block, as a state built in memory on the stored landing `onto` reads
/// it (D-2QX9W, D-N0NVF): a landing of the tested chain merged so far that removed it, the
/// latest first; else a landing of `onto`'s own chain (`StoreReader::deleters_on_chain`). A
/// landing outside `onto`'s chain, a later landing of the tested chain and current canon's
/// landed rows never count, so a test reads the deletes that its simulated replay has.
#[derive(Clone, Debug)]
pub(crate) struct ChainDeletes {
    onto: Arc<OntoChain>,
    /// The change of the tested chain that removed each block, latest landing first.
    chain: HashMap<BlockId, Hash>,
}

impl ChainDeletes {
    /// The deletes of `onto`'s state, before any landing of the tested chain.
    pub(crate) fn new(onto: Option<Hash>) -> ChainDeletes {
        ChainDeletes {
            onto: OntoChain::new(onto),
            chain: HashMap::new(),
        }
    }

    /// These deletes after one landing of the tested chain merged `changes` from state `before`
    /// to state `after`: each block `before` held and `after` lacks was removed by the smallest
    /// of `changes` that deletes it, as a landing's deletes are read.
    pub(crate) fn after(
        &self,
        before: &StateSnapshot,
        after: &StateSnapshot,
        changes: &[Change],
    ) -> ChainDeletes {
        let mut next = self.clone();
        let mut removed: HashMap<BlockId, Hash> = HashMap::new();
        for c in changes {
            let id = c.id();
            for op in &c.ops {
                let b = op.block_id();
                if matches!(op, Op::Delete { .. })
                    && before.blocks.contains_key(b)
                    && !after.blocks.contains_key(b)
                {
                    let e = removed.entry(b.clone()).or_insert(id);
                    *e = (*e).min(id);
                }
            }
        }
        next.chain.extend(removed);
        next
    }

    /// The change that removed each of `blocks` that `base` lacks (see the type's docs).
    pub(crate) fn deleters<'a>(
        &self,
        st: &ReadState,
        base: &StateSnapshot,
        blocks: impl IntoIterator<Item = &'a BlockId>,
    ) -> Result<HashMap<BlockId, Hash>, RdyError> {
        let mut out = HashMap::new();
        let mut rest: BTreeSet<&BlockId> = BTreeSet::new();
        for b in blocks {
            if base.blocks.contains_key(b) {
                continue;
            }
            match self.chain.get(b) {
                Some(c) => {
                    out.insert(b.clone(), *c);
                }
                None => {
                    rest.insert(b);
                }
            }
        }
        let reader = st.reader();
        if rest.is_empty() || !reader.ever_deleted(rest.iter().copied())? {
            return Ok(out);
        }
        let positions = self.onto.positions(st)?;
        out.extend(reader.deleters_on_chain(rest.iter().copied(), &positions)?);
        Ok(out)
    }

    /// The deletes `deleters` gives for the blocks `changes` write.
    pub(crate) fn of_changes(
        &self,
        st: &ReadState,
        base: &StateSnapshot,
        changes: &[Change],
    ) -> Result<HashMap<BlockId, Hash>, RdyError> {
        self.deleters(
            st,
            base,
            changes.iter().flat_map(|c| c.ops.iter().map(Op::block_id)),
        )
    }
}

/// The check of `landing` on `st` (see the module docs), on the landing's own stored `prev`.
/// `st`'s reader holds the landing's changes, its approved version and every record readiness
/// reads.
pub fn check_landing(
    st: &ReadState,
    landing: &LandingRecord,
    inputs: &CheckInputs,
) -> Result<LandingCheck, RdyError> {
    let prev = PrevState::stored(st, landing.prev)?;
    check_landing_over(st, &prev, landing, inputs)
}

/// The check of `landing` on the `prev` state `prev`, which may be built in memory (D-2QX9W
/// Gate result): the candidate is `prev.state` plus the landing's changes, as one view
/// (`ReadState::candidate_state_over`). The landing's own `prev` is not read. Everything else is
/// [`check_landing`]'s.
pub fn check_landing_over(
    st: &ReadState,
    prev: &PrevState,
    landing: &LandingRecord,
    inputs: &CheckInputs,
) -> Result<LandingCheck, RdyError> {
    LANDING_CHECKS.with(|n| n.set(n.get() + 1));
    let version = landing.approved_version.unwrap_or(landing.proposal);
    let pref = crate::gate::version_ref(st, &version)?;
    let stored = st.reader().proposal(&version)?;
    let candidate = Proposal {
        canon_at: prev.at,
        changes: landing.changes.clone(),
        ..stored
    };
    let cst = st.candidate_state_over(prev, &candidate.view, &landing.changes, inputs)?;
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
    /// This thread's runs of [`check_landing_over`] (every [`check_landing`] runs it once).
    static LANDING_CHECKS: Cell<u64> = const { Cell::new(0) };
}

/// Tests only (as `readiness_calls_for_tests`): this thread's runs of [`check_landing_over`].
#[doc(hidden)]
pub fn landing_checks_for_tests() -> u64 {
    LANDING_CHECKS.with(Cell::get)
}
