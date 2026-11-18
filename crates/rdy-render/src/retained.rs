//! What a review target's detached render preparation keeps, in bytes (D-WYZ97, D-SMP5A, the F3
//! accounting ruling). The site's one memory ledger charges each kept review target by
//! [`crate::live_review::PreparedReview::retained_bytes`], which adds up the estimators here.
//!
//! An estimate covers every retained component: its inline size, its nested strings, its buffer
//! capacities, its collection storage and the overhead of a shared allocation. Storage this crate
//! cannot see into (the engine's persistent maps, the graph's maps, the review's hash maps) is
//! estimated from what each holds, by the conservative formula beside its estimator. No estimate
//! is an `Arc`'s own size or the length of a serialized output.
//!
//! An allocation that several retained targets share is counted once per recount: a recount
//! starts a fresh [`RetainedAllocations`], passes it to every retained target, and drops it after.
//! Its identity is the allocation's address while the recount holds the targets, never a name or
//! equal content: two separate copies with the same bytes count twice. The set holds addresses
//! only, so it keeps no target alive and caches nothing between recounts.
use rdy_engine::Materialized;
use rdy_graph::Graph;
use rdy_model::{Block, BlockId, tree};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::mem::size_of;
use std::sync::Arc;

/// The allocations one site-ledger recount has counted so far, by address.
#[derive(Debug, Default)]
pub struct RetainedAllocations {
    seen: HashSet<usize>,
}

impl RetainedAllocations {
    /// A fresh set, for one recount.
    pub fn new() -> RetainedAllocations {
        RetainedAllocations::default()
    }

    /// Whether this recount meets `a`'s allocation for the first time; it is counted from now on.
    pub(crate) fn first<T: ?Sized>(&mut self, a: &Arc<T>) -> bool {
        self.seen.insert(Arc::as_ptr(a).cast::<()>() as usize)
    }

    /// How many allocations this recount has counted.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// Whether this recount has counted nothing yet.
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

/// The two counts and the allocator's header of an `Arc`'s allocation.
pub(crate) const ARC_HEADER: u64 = 16;

/// The charge for one entry of a map or set whose nodes this crate cannot see: a persistent map's
/// (an `imbl` map holds up to 64 entries per node beside up to 65 child pointers), a hash map's
/// control bytes and spare buckets. 64 bytes is about twice an entry's share of a half-full node,
/// on top of the entry's own key and value.
pub(crate) const OPAQUE_ENTRY: u64 = 64;

/// A `String` or `Vec`'s inline size: pointer, capacity, length.
const VEC_INLINE: u64 = 24;

fn size<T>() -> u64 {
    size_of::<T>() as u64
}

/// The heap bytes of a string: its capacity.
pub(crate) fn string(s: &String) -> u64 {
    s.capacity() as u64
}

/// The heap bytes of an optional string.
pub(crate) fn opt_string(s: &Option<String>) -> u64 {
    s.as_ref().map_or(0, string)
}

/// The heap bytes of a vector: its capacity of `T`, and what each item owns (`each`).
pub(crate) fn vec<T>(v: &Vec<T>, each: impl Fn(&T) -> u64) -> u64 {
    v.capacity() as u64 * size::<T>() + v.iter().map(each).sum::<u64>()
}

/// The heap bytes of a `BTreeMap`: its nodes, and what each key and value owns (`each`). A node
/// holds up to 11 entries and 12 edges, and every node but the root is at least half full, so a
/// map of `n` entries has at most `n / 5 + 1` nodes; each is charged 11 entries, 12 edges and 16
/// bytes of header.
pub(crate) fn btree<K, V>(m: &BTreeMap<K, V>, each: impl Fn(&K, &V) -> u64) -> u64 {
    let nodes = m.len() as u64 / 5 + 1;
    let node = 11 * (size::<K>() + size::<V>()) + 12 * 8 + 16;
    nodes * node + m.iter().map(|(k, v)| each(k, v)).sum::<u64>()
}

/// [`btree`] for a set.
pub(crate) fn btree_set<K>(s: &BTreeSet<K>, each: impl Fn(&K) -> u64) -> u64 {
    let nodes = s.len() as u64 / 5 + 1;
    let node = 11 * size::<K>() + 12 * 8 + 16;
    nodes * node + s.iter().map(each).sum::<u64>()
}

/// The heap bytes of a block id: its string, charged twice its length (its capacity may be up to
/// twice its length).
pub(crate) fn id(i: &BlockId) -> u64 {
    2 * i.as_str().len() as u64
}

/// The bytes of a value that serializes, for storage this crate does not own field by field: a
/// walk of its data. Each string is charged its inline size and twice its length (its capacity
/// may be up to twice its length); each sequence its inline size and 16 bytes per item beside
/// what the item holds; each map entry [`OPAQUE_ENTRY`] beside its key and value; each other
/// value 16 bytes. The walk reads the value's data, not the length of any output. A value that
/// does not serialize is charged nothing here; its caller charges its inline size.
pub(crate) fn data<T: Serialize + ?Sized>(v: &T) -> u64 {
    serde_json::to_value(v).map_or(0, |v| value(&v))
}

fn value(v: &Value) -> u64 {
    match v {
        Value::Null | Value::Bool(_) | Value::Number(_) => 16,
        Value::String(s) => VEC_INLINE + 2 * s.len() as u64,
        Value::Array(items) => VEC_INLINE + items.iter().map(|i| 16 + value(i)).sum::<u64>(),
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| OPAQUE_ENTRY + VEC_INLINE + 2 * k.len() as u64 + value(v))
            .sum::<u64>(),
    }
}

/// The bytes one block keeps in a state: the block itself behind its `Arc` (its inline size, and
/// its fields, body, links and pins by [`data`]), and its entries in the state's maps.
fn block(b: &Block) -> u64 {
    let ids = id(&b.id) + size::<BlockId>();
    // the block in `blocks`, behind its own Arc
    let mut n = OPAQUE_ENTRY + ids + 8 + ARC_HEADER + size::<Block>() + data(b);
    // its version in `versions`
    n += OPAQUE_ENTRY + ids + 32;
    // its place in `children`: its order and id in its parent's set, and the parent's entry
    n += 2 * OPAQUE_ENTRY + 2 * ids + VEC_INLINE + 2 * b.order.len() as u64;
    if tree::is_tree_type(&b.block_type) {
        let name = b.field_str("name").map_or(0, str::len) as u64;
        let title = b.field_str("title").map_or(0, str::len) as u64;
        // its entry in `names` (parent and lower-cased name, and the set of ids under it)
        n += 2 * OPAQUE_ENTRY + 2 * ids + VEC_INLINE + 2 * name;
        // a doc with no stored name may get a derived name on first use: its two entries (by id,
        // by lower-cased name) are charged up front, each a name of its title's length and the
        // room a de-duplication adds
        if b.field_str("name").is_none() {
            n += 2 * (OPAQUE_ENTRY + ids + 2 * VEC_INLINE + 2 * (title + 16));
        }
    }
    n
}

/// The bytes a selected state keeps: each block by [`block`], and the state's own header.
/// `Materialized` keeps its maps private; this is the estimate of what they hold.
pub(crate) fn materialized(m: &Materialized) -> u64 {
    ARC_HEADER + size::<Materialized>() + m.iter().map(block).sum::<u64>()
}

/// The bytes a graph keeps. `Graph` keeps its maps private; each node is estimated from what the
/// graph answers of it: its outgoing and incoming edges (each list's entry and inline size, room
/// for twice its edges, and each edge's data by [`data`]), its content hash and removal entry, and
/// for a decision its status, its re-affirmed premises and its adoption, each an
/// [`OPAQUE_ENTRY`] with its key and 64 bytes of value. The nodes are the blocks of `m` and every
/// target an edge names. 1024 bytes stand for the shared tracking fields.
pub(crate) fn graph(g: &Graph, m: &Materialized) -> u64 {
    let mut nodes: BTreeSet<&BlockId> = m.ids().collect();
    for i in m.ids() {
        nodes.extend(g.outgoing(i).iter().map(|e| &e.to));
    }
    let edges = |list: &[rdy_graph::Edge]| {
        if list.is_empty() {
            return 0;
        }
        OPAQUE_ENTRY
            + VEC_INLINE
            + 2 * list.len() as u64 * size::<rdy_graph::Edge>()
            + list.iter().map(data).sum::<u64>()
    };
    let mut n = ARC_HEADER + size::<Graph>() + 1024;
    for i in nodes {
        let key = id(i) + size::<BlockId>();
        n += edges(g.outgoing(i)) + edges(g.incoming(i));
        // its content hash, and its removal's request when it is removed
        n += 2 * (OPAQUE_ENTRY + key + 32);
        if m.get(i)
            .is_some_and(|b| b.block_type == rdy_model::decision::DECISION)
        {
            // status, re-affirmed premises, adoption
            n += 3 * (OPAQUE_ENTRY + key + 64);
        }
    }
    n
}

/// The bytes a review keeps (`rdy_engine::reader::review::Review`, whose hash maps are private),
/// estimated from what it answers for the blocks of the draft `m`: each block's mark, each doc's
/// change and each folder's mark (an [`OPAQUE_ENTRY`] with its key and value each), each doc's
/// removed lines (their blocks, charged twice their inline size beside their data by [`data`]),
/// the removed pages alike, and each changed block's entry.
pub(crate) fn review(r: &crate::review::Review, m: &Materialized) -> u64 {
    let mut n = size::<crate::review::Review>();
    let entry = |i: &BlockId, v: u64| OPAQUE_ENTRY + id(i) + size::<BlockId>() + v;
    let kept = |b: &Block| 2 * size::<Block>() + data(b);
    for i in m.ids() {
        if r.mark(i).is_some() {
            n += entry(i, 8);
        }
        if r.doc(i).is_some() {
            n += entry(i, 40);
        } else if r.page_mark(i).is_some() {
            n += entry(i, 8);
        }
        let removed = r.removed_in(i);
        if !removed.is_empty() {
            n += entry(i, VEC_INLINE);
            for line in removed {
                n += 2 * size::<crate::review::Removed>()
                    + line.before.as_ref().map_or(0, id)
                    + line.blocks.iter().map(kept).sum::<u64>();
            }
        }
    }
    n += r.removed_pages().iter().map(kept).sum::<u64>();
    n += u64::from(r.changed_blocks()) * (OPAQUE_ENTRY + 64);
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One allocation counts once per recount, however many targets hold it; two separate
    /// copies count twice, even with equal bytes; a new recount counts again.
    #[test]
    fn sod1_retained_allocations_count_each_shared_part_once() {
        let a = Arc::new("x".repeat(100));
        let shared = Arc::clone(&a);
        let copy = Arc::new("x".repeat(100));
        let count = |seen: &mut RetainedAllocations, s: &Arc<String>| {
            if seen.first(s) { string(s) } else { 0 }
        };
        let mut seen = RetainedAllocations::new();
        let n = count(&mut seen, &a) + count(&mut seen, &shared) + count(&mut seen, &copy);
        assert_eq!(
            n,
            2 * string(&a),
            "the shared Arc once, its equal copy again"
        );
        assert_eq!(seen.len(), 2);
        let mut next = RetainedAllocations::new();
        assert_eq!(
            count(&mut next, &shared),
            string(&a),
            "a new recount counts again"
        );
        // a vector's estimate is its capacity, not its length
        let mut v: Vec<u64> = Vec::with_capacity(64);
        v.push(1);
        assert_eq!(vec(&v, |_| 0), 64 * 8);
        // a serialized value's walk charges its strings beyond their length
        assert!(data(&"y".repeat(1000)) >= 2000);
    }
}
