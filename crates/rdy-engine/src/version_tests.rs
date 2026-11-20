//! The rebase test of a chain of landings (D-2QX9W, inputs of D-N0NVF): whether the chain, oldest
//! first, still merges onto the state of one landing, with the merge rules of a landing.
//!
//! Each landing's own changes merge onto the running state, which starts as `onto`'s snapshot
//! (the empty state for none) and becomes each clean merge's state. Its deletes are those of the
//! landings merged so far, else those of `onto`'s own chain
//! ([`crate::landing_check::ChainDeletes`]): never a later landing of the chain, a landing
//! outside `onto`'s chain or current canon's landed rows. The test stops at the first landing
//! with any conflict and names every conflicting block of that landing and its kind
//! (`StoreReader::merge_conflicts`). It reads records only and writes nothing. The caller checks
//! the inputs.

use crate::ReadState;
use crate::error::RdyError;
use crate::landing_check::ChainDeletes;
use rdy_model::Hash;
use rdy_store::MergeConflict;
use serde::Serialize;

/// The result of a rebase test.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RebaseTest {
    /// Every landing of the chain merges, in order.
    Clean,
    /// The first landing that does not merge, with every conflicting block and its kind.
    Conflict {
        landing: Hash,
        conflicts: Vec<MergeConflict>,
    },
}

/// Merges each landing of `chain`, oldest first, onto the state of landing `onto` (see the
/// module docs). Every hash of `chain` names a stored landing.
pub fn rebase_test(
    st: &ReadState,
    chain: &[Hash],
    onto: Option<Hash>,
) -> Result<RebaseTest, RdyError> {
    let reader = st.reader();
    let mut state = reader.state_of(onto)?;
    let mut deletes = ChainDeletes::new(onto);
    for &landing in chain {
        let record = reader.landing(&landing)?;
        let changes = record
            .changes
            .iter()
            .map(|c| reader.change(c))
            .collect::<Result<Vec<_>, _>>()?;
        let given = deletes.of_changes(st, &state, &changes)?;
        let (merged, conflicts) = reader.merge_conflicts_given(&state, &changes, &given)?;
        if !conflicts.is_empty() {
            return Ok(RebaseTest::Conflict { landing, conflicts });
        }
        deletes = deletes.after(&state, &merged.state, &changes);
        state = merged.state;
    }
    Ok(RebaseTest::Clean)
}
