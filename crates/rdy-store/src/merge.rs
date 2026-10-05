//! The conflict-aware merge (merge1). Pure: it reads no store and does no I/O.
//!
//! Changes apply in "deps first, then change id" order. Each block is either clean (a version
//! and the change that produced it) or conflicted (the failing op's base and the competing
//! sides). A conflict is data, not an error; a change that depends on every side of a conflict
//! resolves it.
use rdy_model::{BlockId, Hash, Op, StateSnapshot};
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BinaryHeap, HashMap};

/// One change as the merge sees it.
#[derive(Clone, Copy, Debug)]
pub struct MergeChange<'a> {
    pub id: Hash,
    pub deps: &'a [Hash],
    pub ops: &'a [Op],
}

/// One competing version of a conflicted block.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Side {
    /// The change whose op produced this side; `None` when the side comes from the base snapshot
    /// and its version has no known producer.
    pub change: Option<Hash>,
    /// `None` means deleted.
    pub version: Option<Hash>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    pub id: Hash,
    pub block: BlockId,
    pub base: Option<Hash>,
    pub sides: Vec<Side>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Merged {
    /// The base with every touched block set to its clean version, or to its conflict's base.
    pub state: StateSnapshot,
    /// One per conflicted block, in block-id order, sides sorted ascending.
    pub conflicts: Vec<Conflict>,
    /// The order the changes were applied in.
    pub order: Vec<Hash>,
}

/// A conflict's id: the hash of the postcard encoding of `(block, base, sides)`, with `sides`
/// already sorted.
pub fn conflict_id(block: &BlockId, base: Option<Hash>, sides: &[Side]) -> Hash {
    Hash::of(&postcard::to_allocvec(&(block, base, sides)).expect("encode conflict"))
}

enum Slot {
    Clean {
        version: Option<Hash>,
        owner: Option<Hash>,
    },
    Conflicted {
        base: Option<Hash>,
        sides: Vec<Side>,
    },
}

/// Deps first, then change id: Kahn's algorithm over the distinct input changes, its ready set
/// ordered by id ascending; any nodes a cycle leaves unplaced follow in ascending id order.
/// Returns each applied change's position in `changes` (its first, when listed twice).
fn order(changes: &[MergeChange<'_>]) -> Vec<usize> {
    let mut index: HashMap<Hash, usize> = HashMap::with_capacity(changes.len());
    // node -> its position in `changes`
    let mut nodes: Vec<usize> = Vec::with_capacity(changes.len());
    for (at, c) in changes.iter().enumerate() {
        if let Entry::Vacant(e) = index.entry(c.id) {
            e.insert(nodes.len());
            nodes.push(at);
        }
    }
    let id = |i: usize| changes[nodes[i]].id;
    let mut indegree = vec![0usize; nodes.len()];
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (i, &at) in nodes.iter().enumerate() {
        for d in changes[at].deps {
            if let Some(&j) = index.get(d)
                && j != i
            {
                succ[j].push(i);
                indegree[i] += 1;
            }
        }
    }
    let mut ready: BinaryHeap<Reverse<(Hash, usize)>> = indegree
        .iter()
        .enumerate()
        .filter(|(_, n)| **n == 0)
        .map(|(i, _)| Reverse((id(i), i)))
        .collect();
    let mut placed = vec![false; nodes.len()];
    let mut out = Vec::with_capacity(nodes.len());
    while let Some(Reverse((_, i))) = ready.pop() {
        placed[i] = true;
        out.push(nodes[i]);
        for &s in &succ[i] {
            indegree[s] -= 1;
            if indegree[s] == 0 {
                ready.push(Reverse((id(s), s)));
            }
        }
    }
    if out.len() < nodes.len() {
        let mut rest: Vec<usize> = (0..nodes.len()).filter(|&i| !placed[i]).collect();
        rest.sort_by_key(|&i| id(i));
        out.extend(rest.into_iter().map(|i| nodes[i]));
    }
    out
}

/// Merges `changes` onto `base`. `producer` names the change that produced a base version, so
/// that a side from the base can be depended on.
pub fn merge(
    base: &StateSnapshot,
    changes: &[MergeChange<'_>],
    producer: &dyn Fn(&Hash) -> Option<Hash>,
) -> Merged {
    merge_indexed(base, changes, producer).0
}

/// `merge`, with each entry of `Merged::order` also given as its position in `changes` (its
/// first, when a change is listed twice), so a caller can find the applied changes without
/// hashing them again.
pub fn merge_indexed(
    base: &StateSnapshot,
    changes: &[MergeChange<'_>],
    producer: &dyn Fn(&Hash) -> Option<Hash>,
) -> (Merged, Vec<usize>) {
    let ordered = order(changes);
    let mut slots: BTreeMap<BlockId, Slot> = BTreeMap::new();
    for c in ordered.iter().map(|&at| &changes[at]) {
        let mut deps = c.deps.to_vec();
        deps.sort_unstable();
        let covered = |s: &Side| s.change.is_some_and(|x| deps.binary_search(&x).is_ok());
        for op in c.ops {
            let block = op.block_id();
            let slot = slots.entry(block.clone()).or_insert_with(|| {
                let version = base.blocks.get(block).copied();
                Slot::Clean {
                    version,
                    owner: version.and_then(|v| producer(&v)),
                }
            });
            let expected = op.base();
            let new = match op {
                Op::Create { block } | Op::Update { block, .. } => Some(block.version_hash()),
                Op::Delete { .. } => None,
            };
            let mine = Side {
                change: Some(c.id),
                version: new,
            };
            match slot {
                Slot::Clean { version, owner } if *version == expected => {
                    *version = new;
                    *owner = Some(c.id);
                }
                Slot::Clean { version, .. } if *version == new => {}
                Slot::Clean { version, owner } => {
                    let theirs = Side {
                        change: *owner,
                        version: *version,
                    };
                    *slot = Slot::Conflicted {
                        base: expected,
                        sides: vec![theirs, mine],
                    };
                }
                Slot::Conflicted { sides, .. }
                    if sides.iter().all(|s| s.change.is_none() || covered(s)) =>
                {
                    *slot = Slot::Clean {
                        version: new,
                        owner: Some(c.id),
                    };
                }
                Slot::Conflicted { sides, .. } => {
                    sides.retain(|s| !covered(s));
                    sides.push(mine);
                }
            }
        }
    }
    let mut state = base.clone();
    let mut conflicts = Vec::new();
    for (block, slot) in slots {
        let version = match slot {
            Slot::Clean { version, .. } => version,
            Slot::Conflicted { base, mut sides } => {
                sides.sort();
                let id = conflict_id(&block, base, &sides);
                conflicts.push(Conflict {
                    id,
                    block: block.clone(),
                    base,
                    sides,
                });
                base
            }
        };
        match version {
            Some(v) => {
                state.blocks.insert(block, v);
            }
            None => {
                state.blocks.remove(&block);
            }
        }
    }
    let order = ordered.iter().map(|&at| changes[at].id).collect();
    (
        Merged {
            state,
            conflicts,
            order,
        },
        ordered,
    )
}
