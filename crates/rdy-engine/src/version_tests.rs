//! The rebase test of a chain of landings (D-2QX9W, inputs of D-N0NVF): whether the chain, oldest
//! first, still merges onto the state of one landing, with the merge rules of a landing. The
//! version pair of two request versions (D-G8T2W) is at the end: [`version_pair`].
//!
//! Each landing's own changes merge onto the running state, which starts as `onto`'s snapshot
//! (the empty state for none) and becomes each clean merge's state. Deletions read as of `onto`
//! (`merge_onto`'s `at`). The test stops at the first landing with any conflict and names every
//! conflicting block of that landing and its kind (`StoreReader::merge_conflicts`). It reads
//! records only and writes nothing. The caller checks the inputs.

use crate::ReadState;
use crate::error::RdyError;
use crate::history_facts::{self, MAX_LANDINGS, walk_heads};
use rdy_model::{BlockId, Hash};
use rdy_store::{MergeConflict, kinds};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

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
    for &landing in chain {
        let record = reader.landing(&landing)?;
        let changes = record
            .changes
            .iter()
            .map(|c| reader.change(c))
            .collect::<Result<Vec<_>, _>>()?;
        let (merged, conflicts) = reader.merge_conflicts(onto, &state, &changes)?;
        if !conflicts.is_empty() {
            return Ok(RebaseTest::Conflict { landing, conflicts });
        }
        state = merged.state;
    }
    Ok(RebaseTest::Clean)
}

/// One version of a request: its chain of landings, oldest first, and the landing it sits on
/// (None for the empty state, as on an empty hub canon).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionChain {
    pub base: Option<Hash>,
    pub chain: Vec<Hash>,
}

/// The two answers of the carry-over rule for a pair of request versions (D-G8T2W).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct VersionTests {
    /// The new chain replays the old one landing for landing.
    pub replays_one_for_one: bool,
    /// A landing between the bases writes a block that the request writes, or the old base is
    /// not in the new base's chain.
    pub base_moves_write_request_blocks: bool,
}

/// The version pair of D-G8T2W, over stored landings. The caller checks the inputs.
///
/// One for one: both chains have the same length, and at each position the new landing is the
/// old one, or a walk of `rebased_from` from it reaches the old one. Each step of the walk has
/// exactly one entry (a landing with two or more replays none one for one, D-VXZ0C), and the
/// walk ends at a landing the store lacks or after [`MAX_LANDINGS`] steps.
///
/// Between the bases: the landings of the new base's chain after the old base, up to and
/// including the new base; none for equal bases. A null base comes before the first landing.
/// When the old base is not in the new base's chain, the base moves. The request's blocks are the
/// block ids of every op of every landing of both chains. It reads records only.
pub fn version_pair(
    st: &ReadState,
    old: &VersionChain,
    new: &VersionChain,
) -> Result<VersionTests, RdyError> {
    Ok(VersionTests {
        replays_one_for_one: replays_one_for_one(st, &old.chain, &new.chain)?,
        base_moves_write_request_blocks: base_moves_write(st, old, new)?,
    })
}

fn replays_one_for_one(st: &ReadState, old: &[Hash], new: &[Hash]) -> Result<bool, RdyError> {
    if old.len() != new.len() {
        return Ok(false);
    }
    for (o, n) in old.iter().zip(new) {
        if !replays(st, *o, *n)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether `new` is `old`, or a walk of single `rebased_from` entries from `new` reaches it.
fn replays(st: &ReadState, old: Hash, new: Hash) -> Result<bool, RdyError> {
    let reader = st.reader();
    let mut at = new;
    for _ in 0..MAX_LANDINGS {
        if at == old {
            return Ok(true);
        }
        if !reader.has_kind(kinds::LANDING, &at)? {
            return Ok(false);
        }
        let &[source] = reader.landing(&at)?.rebased_from.as_slice() else {
            return Ok(false);
        };
        at = source;
    }
    Ok(at == old)
}

fn base_moves_write(
    st: &ReadState,
    old: &VersionChain,
    new: &VersionChain,
) -> Result<bool, RdyError> {
    let Some(between) = between_bases(st, old.base, new.base)? else {
        return Ok(true);
    };
    if between.is_empty() {
        return Ok(false);
    }
    let mut writes: HashMap<Hash, HashSet<BlockId>> = HashMap::new();
    let mut blocks_of = |h: Hash| -> Result<HashSet<BlockId>, RdyError> {
        if let Some(ids) = writes.get(&h) {
            return Ok(ids.clone());
        }
        let ids: HashSet<BlockId> = history_facts::landing(st, h)?
            .blocks
            .into_iter()
            .map(|b| b.id)
            .collect();
        writes.insert(h, ids.clone());
        Ok(ids)
    };
    let mut request: HashSet<BlockId> = HashSet::new();
    for &h in old.chain.iter().chain(&new.chain) {
        request.extend(blocks_of(h)?);
    }
    for h in between {
        if !blocks_of(h)?.is_disjoint(&request) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The landings of `new`'s chain after `old`, newest first, up to and including `new`; None when
/// `old` is not in that chain. A walk of more than [`MAX_LANDINGS`] landings is `E_LIMIT`.
fn between_bases(
    st: &ReadState,
    old: Option<Hash>,
    new: Option<Hash>,
) -> Result<Option<Vec<Hash>>, RdyError> {
    if old == new {
        return Ok(Some(Vec::new()));
    }
    let Some(new) = new else {
        return Ok(None);
    };
    let reader = st.reader();
    let mut out = Vec::new();
    let found = walk_heads(
        &[new],
        |h| Ok(reader.landing(h)?.prev),
        |h| {
            if Some(h) == old {
                return true;
            }
            out.push(h);
            false
        },
    )?;
    // a null base comes before the first landing, so the whole chain is between
    Ok((found || old.is_none()).then_some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Rdy;
    use rdy_model::{Assurance, LandingRecord, Provenance, RepoId, Role};

    /// A landing record with no changes, distinct by `n`, that replays `rebased_from`.
    fn replay_of(n: u8, rebased_from: Vec<Hash>) -> LandingRecord {
        LandingRecord {
            proposal: Hash([n; 32]),
            prev: None,
            state: Hash([2; 32]),
            approved_by: Provenance {
                actor: "human:founder".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1,
            },
            changes: vec![],
            chosen: None,
            checks: vec![],
            assurance: Assurance::Local,
            relayed_by: None,
            relay_evidence: None,
            approved_version: None,
            rebased_from,
            evidence: None,
            assessment: None,
            policy: None,
            read_identity: None,
        }
    }

    fn pair(old: &[Hash], new: &[Hash]) -> (VersionChain, VersionChain) {
        let chain = |c: &[Hash]| VersionChain {
            base: None,
            chain: c.to_vec(),
        };
        (chain(old), chain(new))
    }

    /// No pull writes a landing with two `rebased_from` entries (a replay names its one source),
    /// so the records are written directly: `o1` and `o2` are old landings, `fold` replays both,
    /// `r1` replays `o1`, `r2` replays `r1`, and `lost` replays a landing the store lacks.
    #[test]
    fn crb6_a_landing_that_replays_two_does_not_replay_one_for_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = Rdy::create_holder(dir.path(), RepoId([1; 16])).unwrap();
        let mut put = |rec: LandingRecord| h.store_mut().put(kinds::LANDING, &rec).unwrap();
        let o1 = put(replay_of(1, vec![]));
        let o2 = put(replay_of(2, vec![]));
        let fold = put(replay_of(3, vec![o1, o2]));
        let r1 = put(replay_of(4, vec![o1]));
        let r2 = put(replay_of(5, vec![r1]));
        let lost = put(replay_of(6, vec![Hash([9; 32])]));
        h.seal_open().unwrap();
        h.close().unwrap();
        let h = Rdy::open_holder(dir.path()).unwrap();
        let read = h.read();
        let one_for_one = |old: &[Hash], new: &[Hash]| {
            let (old, new) = pair(old, new);
            let t = version_pair(&read, &old, &new).unwrap();
            assert!(!t.base_moves_write_request_blocks, "equal null bases");
            t.replays_one_for_one
        };
        // a fold replays neither of its sources one for one
        assert!(!one_for_one(&[o1], &[fold]));
        assert!(!one_for_one(&[o2], &[fold]));
        assert!(!one_for_one(&[o1, o2], &[fold]));
        // a walk of single entries, to any depth
        assert!(one_for_one(&[o1], &[r1]));
        assert!(one_for_one(&[o1], &[r2]));
        assert!(one_for_one(&[r1], &[r2]));
        assert!(!one_for_one(&[o2], &[r2]));
        // a walk that reaches a landing the store lacks ends there
        assert!(!one_for_one(&[o1], &[lost]));
        // the same landing replays itself; the order of the chain counts
        assert!(one_for_one(&[o1, o2], &[o1, o2]));
        assert!(!one_for_one(&[o1, o2], &[o2, o1]));
        assert!(one_for_one(&[], &[]));
    }
}
