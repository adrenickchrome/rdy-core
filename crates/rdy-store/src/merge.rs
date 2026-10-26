//! The conflict-aware merge (merge1). Pure: it reads no store and does no I/O.
//!
//! Changes apply in "deps first, then change id" order (a replay after the ordinary changes
//! that do not wait for it). Each block is either clean (a version
//! and the change that produced it) or conflicted (the failing op's base and the competing
//! sides). A conflict is data, not an error; a change that depends on every side of a conflict
//! resolves it. A block the base lacks reads as deleted by the change `deleter` names; when no
//! change is known for that delete, no change can depend on it, so its side is never resolved. A
//! create of a deleted id that does not depend on the delete conflicts with it. A base version
//! that differs from an op's base only by its order key (a landing's re-key, a key re-derive) is
//! no conflict for that op (D-TX3KH): the op applies, so order never costs a person a conflict.
use rdy_model::{BlockId, Hash, Op, StateSnapshot};
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap};

/// One change as the merge sees it.
#[derive(Clone, Copy, Debug)]
pub struct MergeChange<'a> {
    pub id: Hash,
    pub deps: &'a [Hash],
    pub ops: &'a [Op],
    /// Whether the change replays another (`ChangeLinks::replayed_from` is set). A replay
    /// applies after the ordinary changes that do not wait for it. Its op on a clean block
    /// follows the ordinary rules: an equal base applies, an equal result agrees whatever the
    /// bases and whoever wrote the version (the op is already satisfied), a base version that
    /// differs from its base only by its order key agrees as it does for any op, and anything
    /// else conflicts. Its op on a conflicted block adds its side (none when a side already holds its
    /// version) and keeps every side there: its deps never resolve a conflict (only an ordinary
    /// write that covers every side does).
    pub is_replay: bool,
}

/// One competing version of a conflicted block.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Side {
    /// The change whose op produced this side; `None` when the side comes from the base snapshot
    /// and its version has no known producer (or, for a deleted side, no known deleter).
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

/// A parent loop reachable from `starts` under `parent_of` (a block's parent, None at the top
/// or for a block that is absent): two blocks on it, each the other's ancestor; None when every
/// walk reaches the top. Each block is walked at most once in all, so a damaged state never
/// loops it.
pub fn find_parent_cycle<E>(
    starts: impl IntoIterator<Item = BlockId>,
    mut parent_of: impl FnMut(&BlockId) -> Result<Option<BlockId>, E>,
) -> Result<Option<(BlockId, BlockId)>, E> {
    let mut done: std::collections::HashSet<BlockId> = std::collections::HashSet::new();
    for start in starts {
        let mut path: Vec<BlockId> = vec![];
        let mut on_path: std::collections::HashSet<BlockId> = std::collections::HashSet::new();
        let mut cur = Some(start);
        while let Some(c) = cur {
            if done.contains(&c) {
                break;
            }
            let p = parent_of(&c)?;
            if !on_path.insert(c.clone()) {
                // `c` is on the loop, and so is its parent
                return Ok(Some((c.clone(), p.unwrap_or(c))));
            }
            path.push(c);
            cur = p;
        }
        done.extend(path);
    }
    Ok(None)
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
        /// The version is the base snapshot's: no change of this merge wrote it yet.
        from_base: bool,
    },
    Conflicted {
        base: Option<Hash>,
        sides: Vec<Side>,
    },
}

/// Deps first, then ordinary changes before replays, then change id: Kahn's algorithm over the
/// distinct input changes, its ready set ordered by (replay, id) ascending, so a replay applies
/// after every ordinary change that does not wait for one (the target it was replayed onto);
/// any nodes a cycle leaves unplaced follow in ascending id order. Without replays the order is
/// deps first, then change id.
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
    let key = |i: usize| (changes[nodes[i]].is_replay, changes[nodes[i]].id);
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
    let mut ready: BinaryHeap<Reverse<((bool, Hash), usize)>> = indegree
        .iter()
        .enumerate()
        .filter(|(_, n)| **n == 0)
        .map(|(i, _)| Reverse((key(i), i)))
        .collect();
    let mut placed = vec![false; nodes.len()];
    let mut out = Vec::with_capacity(nodes.len());
    while let Some(Reverse((_, i))) = ready.pop() {
        placed[i] = true;
        out.push(nodes[i]);
        for &s in &succ[i] {
            indegree[s] -= 1;
            if indegree[s] == 0 {
                ready.push(Reverse((key(s), s)));
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

/// Merges `changes` onto `base`. `producer` names the change that produced a base version, and
/// `deleter` the change that deleted a block the base lacks, so that a side from the base can be
/// depended on. `order_only(v, e)` says whether base version `v` differs from an op's base `e`
/// only by its order key (`StoreReader::order_only_pairs` reads them).
pub fn merge(
    base: &StateSnapshot,
    changes: &[MergeChange<'_>],
    producer: &dyn Fn(&Hash) -> Option<Hash>,
    deleter: &dyn Fn(&BlockId) -> Option<Hash>,
    order_only: &dyn Fn(&Hash, &Hash) -> bool,
) -> Merged {
    merge_indexed(base, changes, producer, deleter, order_only).0
}

/// `merge` restricted to `blocks`: the base holds only those blocks and each change only its ops
/// on them (a change left with no ops stays, so the order and the deps are the full merge's).
/// Its conflicts are the full merge's conflicts on `blocks`, and its state holds only `blocks`.
pub fn merge_on_blocks(
    base: &StateSnapshot,
    changes: &[MergeChange<'_>],
    producer: &dyn Fn(&Hash) -> Option<Hash>,
    deleter: &dyn Fn(&BlockId) -> Option<Hash>,
    order_only: &dyn Fn(&Hash, &Hash) -> bool,
    blocks: &BTreeSet<BlockId>,
) -> Merged {
    let base = StateSnapshot {
        blocks: blocks
            .iter()
            .filter_map(|b| base.blocks.get(b).map(|v| (b.clone(), *v)))
            .collect(),
    };
    let ops: Vec<Vec<Op>> = changes
        .iter()
        .map(|c| {
            c.ops
                .iter()
                .filter(|op| blocks.contains(op.block_id()))
                .cloned()
                .collect()
        })
        .collect();
    let changes: Vec<MergeChange<'_>> = changes
        .iter()
        .zip(&ops)
        .map(|(c, ops)| MergeChange {
            id: c.id,
            deps: c.deps,
            ops,
            is_replay: c.is_replay,
        })
        .collect();
    merge(&base, &changes, producer, deleter, order_only)
}

/// `merge`, with each entry of `Merged::order` also given as its position in `changes` (its
/// first, when a change is listed twice), so a caller can find the applied changes without
/// hashing them again.
pub fn merge_indexed(
    base: &StateSnapshot,
    changes: &[MergeChange<'_>],
    producer: &dyn Fn(&Hash) -> Option<Hash>,
    deleter: &dyn Fn(&BlockId) -> Option<Hash>,
    order_only: &dyn Fn(&Hash, &Hash) -> bool,
) -> (Merged, Vec<usize>) {
    let ordered = order(changes);
    let mut slots: BTreeMap<BlockId, Slot> = BTreeMap::new();
    for c in ordered.iter().map(|&at| &changes[at]) {
        let mut deps = c.deps.to_vec();
        deps.sort_unstable();
        let covered = |s: &Side| s.change.is_some_and(|x| deps.binary_search(&x).is_ok());
        // a base version with no known producer cannot be depended on, so it does not hold a
        // conflict open; a delete with no known deleter does: only a change that knows about a
        // delete may undo it
        let settled = |s: &Side| covered(s) || (s.change.is_none() && s.version.is_some());
        // a create of a deleted id re-creates it on purpose only when it depends on the delete
        let knows = |owner: &Option<Hash>| owner.is_none_or(|o| deps.binary_search(&o).is_ok());
        for op in c.ops {
            let block = op.block_id();
            let slot = slots.entry(block.clone()).or_insert_with(|| {
                let version = base.blocks.get(block).copied();
                Slot::Clean {
                    version,
                    owner: match version {
                        Some(v) => producer(&v),
                        None => deleter(block),
                    },
                    from_base: true,
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
                Slot::Clean {
                    version,
                    owner,
                    from_base,
                } if *version == expected && (expected.is_some() || knows(owner)) => {
                    *version = new;
                    *owner = Some(c.id);
                    *from_base = false;
                }
                // ops that reach the block's version agree with it, whatever their bases and
                // whoever the writers, replays included
                Slot::Clean { version, .. } if *version == new => {}
                // a canon side that differs from the op's base only by its order key (a
                // landing's re-key, a key re-derive) is no conflict: the op applies with its own
                // version, and its own landing re-keys a tie it would make (D-TX3KH)
                Slot::Clean {
                    version,
                    owner,
                    from_base,
                } if *from_base
                    && version.is_some_and(|v| expected.is_some_and(|e| order_only(&v, &e))) =>
                {
                    *version = new;
                    *owner = Some(c.id);
                    *from_base = false;
                }
                Slot::Clean { version, owner, .. } => {
                    let theirs = Side {
                        change: *owner,
                        version: *version,
                    };
                    *slot = Slot::Conflicted {
                        base: expected,
                        sides: vec![theirs, mine],
                    };
                }
                // a replay adds its side, unless a side already holds its version: "v against
                // v" never reaches a person
                Slot::Conflicted { sides, .. } if c.is_replay => {
                    if !sides.iter().any(|s| s.version == mine.version) {
                        sides.push(mine);
                    }
                }
                Slot::Conflicted { sides, .. } if sides.iter().all(settled) => {
                    *slot = Slot::Clean {
                        version: new,
                        owner: Some(c.id),
                        from_base: false,
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
