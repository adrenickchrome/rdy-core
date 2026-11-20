//! The replay test of a chain of landings (D-2QX9W Replay test and Gate result, inputs of
//! D-N0NVF): where a pull replay of the chain onto the state of one landing would stop, and the
//! gate result of each landing it replays.
//!
//! The test runs a pull's judging steps ([`crate::sync_replay`]'s, shared, not copied) on each
//! landing L of the chain, oldest first: the removal stop; the new changes (`links.replayed_from`);
//! their merge onto the running state ([`rdy_store::StoreReader::merge_conflicts`], which names
//! every conflicting block and its kind); the gate at the base
//! ([`crate::landing_check::check_landing_over`], with `prev` that state and L's source changes,
//! as a pull checks a candidate); and the accepting policy of a policy landing. The running state
//! starts as `onto`'s snapshot (the empty state for none) and becomes each replayed landing's
//! merged state. It is built in memory: no landing is stored for it, so the merge and the gate
//! read its deletes from [`ChainDeletes`]: those of the landings replayed so far, else those of
//! `onto`'s own chain. A later landing of the chain, a landing outside `onto`'s chain and current
//! canon's landed rows never count, as they never count for the state a pull replays onto. With
//! `require_up_to_date` set, the test stops at the first landing before any step, as a pull
//! under `reviews.require_up_to_date` does.
//!
//! It reads records only and writes nothing: no change, landing, assessment or ref. A stop on
//! removed content names the removal requests and carries no text. The caller checks the inputs.

use crate::ReadState;
use crate::error::RdyError;
use crate::landing_check::{ChainDeletes, CheckInputs, LandingCheck, PrevState};
use crate::sync_replay::{Judged, judge};
use rdy_model::{Change, Hash};
use rdy_store::MergeConflict;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

/// Why a replay test stopped: the causes of a pull's stop, and `up_to_date` for
/// `require_up_to_date`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayStopCause {
    Conflict,
    Gate,
    Policy,
    Removal,
    UpToDate,
}

/// A landing the test replays, with the gate result of its new landing at its base.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReplayedCheck {
    pub source: Hash,
    pub check: LandingCheck,
}

/// The landing a replay test stopped at, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReplayStop {
    pub landing: Hash,
    pub cause: ReplayStopCause,
    /// `conflict` only: every conflicting block and its kind.
    pub conflicts: Vec<MergeConflict>,
    /// `gate` and `policy` only: the gate result at the base.
    pub check: Option<LandingCheck>,
    /// `removal` only: the removal requests, never the removed text.
    pub removals: Vec<Hash>,
}

impl ReplayStop {
    fn new(landing: Hash, cause: ReplayStopCause) -> ReplayStop {
        ReplayStop {
            landing,
            cause,
            conflicts: vec![],
            check: None,
            removals: vec![],
        }
    }
}

/// What a replay test found: the landings it replays, oldest first, and where it stops (none
/// when every landing replays).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReplayTest {
    pub replayed: Vec<ReplayedCheck>,
    pub stop: Option<ReplayStop>,
}

/// Replays `chain`, oldest first, onto the state of landing `onto` as a pull would, with the
/// pack and gate settings of `inputs` (see the module docs). Every hash of `chain` names a
/// stored landing.
pub fn replay_test(
    st: &ReadState,
    chain: &[Hash],
    onto: Option<Hash>,
    inputs: &CheckInputs,
) -> Result<ReplayTest, RdyError> {
    let mut out = ReplayTest {
        replayed: vec![],
        stop: None,
    };
    if inputs.require_up_to_date {
        out.stop = chain
            .first()
            .map(|l| ReplayStop::new(*l, ReplayStopCause::UpToDate));
        return Ok(out);
    }
    let reader = st.reader();
    let mut deletes = Arc::new(ChainDeletes::new(onto));
    let mut prev = PrevState {
        deletes: Some(Arc::clone(&deletes)),
        ..PrevState::stored(st, onto)?
    };
    let mut new_of: HashMap<Hash, Hash> = HashMap::new();
    for source in chain {
        let l = reader.landing(source)?;
        let merge = |changes: &[Change]| {
            let given = deletes.of_changes(st, &prev.state, changes)?;
            let (merged, conflicts) = reader.merge_conflicts_given(&prev.state, changes, &given)?;
            Ok(if conflicts.is_empty() {
                Ok(merged.state)
            } else {
                Err(conflicts)
            })
        };
        let stop = |cause| ReplayStop::new(*source, cause);
        let judged = match judge(st, inputs, source, &l, &new_of, &prev, merge)? {
            Judged::Replays(j) => *j,
            Judged::Removal(removals) => {
                out.stop = Some(ReplayStop {
                    removals,
                    ..stop(ReplayStopCause::Removal)
                });
                return Ok(out);
            }
            Judged::Conflict(conflicts) => {
                out.stop = Some(ReplayStop {
                    conflicts,
                    ..stop(ReplayStopCause::Conflict)
                });
                return Ok(out);
            }
            Judged::Gate(check) => {
                out.stop = Some(ReplayStop {
                    check: Some(check),
                    ..stop(ReplayStopCause::Gate)
                });
                return Ok(out);
            }
            Judged::Policy(check) => {
                out.stop = Some(ReplayStop {
                    check: Some(check),
                    ..stop(ReplayStopCause::Policy)
                });
                return Ok(out);
            }
        };
        new_of.extend(
            l.changes
                .iter()
                .copied()
                .zip(judged.changes.iter().map(Change::id)),
        );
        out.replayed.push(ReplayedCheck {
            source: *source,
            check: judged.check,
        });
        deletes = Arc::new(deletes.after(&prev.state, &judged.state, &judged.changes));
        prev = PrevState {
            at: onto,
            state: Arc::new(judged.state),
            deletes: Some(Arc::clone(&deletes)),
        };
    }
    Ok(out)
}
