//! The conflict-aware merge (plan M2a1, merge1): deterministic, order-independent, equal to a
//! sequential replay for any valid history, and equal to a slow reference.
use proptest::prelude::*;
use rdy_model::{Block, BlockId, Hash, Op, StateSnapshot};
use rdy_store::merge::{Conflict, MergeChange, Merged, Side, conflict_id, merge};
use std::collections::{BTreeMap, HashMap};

fn blk(n: u8, body: &str) -> Block {
    Block {
        id: BlockId::parse(&format!("b-0000{n}")).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
    }
}

#[derive(Clone, Debug)]
struct Owned {
    id: Hash,
    deps: Vec<Hash>,
    ops: Vec<Op>,
}

fn view(cs: &[Owned]) -> Vec<MergeChange<'_>> {
    cs.iter()
        .map(|c| MergeChange {
            id: c.id,
            deps: &c.deps,
            ops: &c.ops,
        })
        .collect()
}

fn owned(label: &str, deps: Vec<Hash>, ops: Vec<Op>) -> Owned {
    Owned {
        id: Hash::of(label.as_bytes()),
        deps,
        ops,
    }
}

const CANON: &[u8] = b"canon producer";

fn canon_producer(base: &StateSnapshot) -> impl Fn(&Hash) -> Option<Hash> + '_ {
    move |v| {
        base.blocks
            .values()
            .any(|x| x == v)
            .then(|| Hash::of(CANON))
    }
}

fn base_with(blocks: &[Block]) -> StateSnapshot {
    let mut s = StateSnapshot::default();
    for b in blocks {
        s.blocks.insert(b.id.clone(), b.version_hash());
    }
    s
}

#[test]
fn merge1_two_edits_from_one_base_are_one_conflict_whatever_the_order() {
    let x0 = blk(1, "zero");
    let base = base_with(std::slice::from_ref(&x0));
    let a = owned(
        "a",
        vec![Hash::of(CANON)],
        vec![Op::Update {
            base: x0.version_hash(),
            block: blk(1, "a"),
        }],
    );
    let b = owned(
        "b",
        vec![Hash::of(CANON)],
        vec![Op::Update {
            base: x0.version_hash(),
            block: blk(1, "b"),
        }],
    );
    let p = canon_producer(&base);
    let ab = merge(&base, &view(&[a.clone(), b.clone()]), &p);
    let ba = merge(&base, &view(&[b.clone(), a.clone()]), &p);
    assert_eq!(ab, ba);
    assert_eq!(ab.conflicts.len(), 1);
    let c = &ab.conflicts[0];
    assert_eq!(
        (c.block.clone(), c.base),
        (x0.id.clone(), Some(x0.version_hash()))
    );
    let mut want = vec![
        Side {
            change: Some(a.id),
            version: Some(blk(1, "a").version_hash()),
        },
        Side {
            change: Some(b.id),
            version: Some(blk(1, "b").version_hash()),
        },
    ];
    want.sort();
    assert_eq!(c.sides, want);
    assert_eq!(c.id, conflict_id(&c.block, c.base, &c.sides));
    assert_eq!(
        ab.state.blocks[&x0.id],
        x0.version_hash(),
        "a conflicted block reads as its base"
    );
}

#[test]
fn merge1_a_change_depending_on_every_side_resolves_the_conflict() {
    // canon moved X from x0 to y (made by change P); the view's C still edits from x0;
    // R writes X from C's version and depends on C and on P: the rebased-then-resolved view.
    let (x0, y) = (blk(1, "zero"), blk(1, "canon"));
    let base = base_with(std::slice::from_ref(&y));
    let p_id = Hash::of(CANON);
    let c = owned(
        "c",
        vec![],
        vec![Op::Update {
            base: x0.version_hash(),
            block: blk(1, "view"),
        }],
    );
    let r = owned(
        "r",
        vec![c.id, p_id],
        vec![Op::Update {
            base: blk(1, "view").version_hash(),
            block: blk(1, "resolved"),
        }],
    );
    let m = merge(
        &base,
        &view(std::slice::from_ref(&c)),
        &canon_producer(&base),
    );
    assert_eq!(m.conflicts.len(), 1);
    assert!(
        m.conflicts[0].sides.contains(&Side {
            change: Some(p_id),
            version: Some(y.version_hash())
        }),
        "canon's side names its producer"
    );
    let m = merge(
        &base,
        &view(&[r.clone(), c.clone()]),
        &canon_producer(&base),
    );
    assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
    assert_eq!(m.state.blocks[&y.id], blk(1, "resolved").version_hash());
    assert_eq!(m.order, vec![c.id, r.id]);
}

#[test]
fn merge1_a_change_covering_one_side_replaces_it() {
    let x0 = blk(1, "zero");
    let base = base_with(std::slice::from_ref(&x0));
    let a = owned(
        "a",
        vec![],
        vec![Op::Update {
            base: x0.version_hash(),
            block: blk(1, "a"),
        }],
    );
    let b = owned(
        "b",
        vec![],
        vec![Op::Update {
            base: x0.version_hash(),
            block: blk(1, "b"),
        }],
    );
    let a2 = owned(
        "a2",
        vec![a.id],
        vec![Op::Update {
            base: blk(1, "a").version_hash(),
            block: blk(1, "a2"),
        }],
    );
    let m = merge(&base, &view(&[a, b.clone(), a2.clone()]), &|_| None);
    assert_eq!(m.conflicts.len(), 1);
    let mut want = vec![
        Side {
            change: Some(a2.id),
            version: Some(blk(1, "a2").version_hash()),
        },
        Side {
            change: Some(b.id),
            version: Some(blk(1, "b").version_hash()),
        },
    ];
    want.sort();
    assert_eq!(m.conflicts[0].sides, want);
    assert_eq!(m.conflicts[0].base, Some(x0.version_hash()));
}

#[test]
fn merge1_convergent_edits_and_deletes_are_not_conflicts() {
    let x0 = blk(1, "zero");
    let base = base_with(&[x0.clone(), blk(2, "two")]);
    let a = owned(
        "a",
        vec![],
        vec![Op::Update {
            base: x0.version_hash(),
            block: blk(1, "same"),
        }],
    );
    let b = owned(
        "b",
        vec![],
        vec![Op::Update {
            base: x0.version_hash(),
            block: blk(1, "same"),
        }],
    );
    let d1 = owned(
        "d1",
        vec![],
        vec![Op::Delete {
            base: blk(2, "two").version_hash(),
            id: blk(2, "").id,
        }],
    );
    let d2 = owned(
        "d2",
        vec![],
        vec![Op::Delete {
            base: blk(2, "two").version_hash(),
            id: blk(2, "").id,
        }],
    );
    let m = merge(&base, &view(&[a, b, d1, d2]), &|_| None);
    assert!(m.conflicts.is_empty());
    assert_eq!(m.state.blocks[&x0.id], blk(1, "same").version_hash());
    assert!(!m.state.blocks.contains_key(&blk(2, "").id));
}

#[test]
fn merge1_two_creates_of_one_id_conflict_with_no_base() {
    let base = StateSnapshot::default();
    let a = owned("a", vec![], vec![Op::Create { block: blk(3, "a") }]);
    let b = owned("b", vec![], vec![Op::Create { block: blk(3, "b") }]);
    let m = merge(&base, &view(&[a, b]), &|_| None);
    assert_eq!(m.conflicts.len(), 1);
    assert_eq!(m.conflicts[0].base, None);
    assert!(
        !m.state.blocks.contains_key(&blk(3, "").id),
        "reads as its (absent) base"
    );
}

/// A label whose hash sorts below `than`, so that id order alone would apply it first.
fn label_below(prefix: &str, than: Hash) -> String {
    (0..)
        .map(|i| format!("{prefix}{i}"))
        .find(|l| Hash::of(l.as_bytes()) < than)
        .unwrap()
}

/// Review Focus 5: the re-create carries the store-added dep on its deleter.
#[test]
fn merge1_delete_then_recreate_follows_the_deleter_dep_not_id_order() {
    let x0 = blk(1, "zero");
    let base = base_with(std::slice::from_ref(&x0));
    let del = owned(
        "delete",
        vec![Hash::of(CANON)],
        vec![Op::Delete {
            base: x0.version_hash(),
            id: x0.id.clone(),
        }],
    );
    let mut re = owned(
        &label_below("create", del.id),
        vec![del.id],
        vec![Op::Create {
            block: blk(1, "again"),
        }],
    );
    assert!(re.id < del.id, "id order alone would re-create first");
    let m = merge(
        &base,
        &view(&[re.clone(), del.clone()]),
        &canon_producer(&base),
    );
    assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
    assert_eq!(m.state.blocks[&x0.id], blk(1, "again").version_hash());
    re.deps.clear();
    let m = merge(&base, &view(&[del, re]), &canon_producer(&base));
    assert_eq!(
        m.conflicts.len(),
        1,
        "without the dep, id order applies the create first"
    );
}

// ---------- property tests ----------

/// A valid sequential history from a base with blocks 0 and 1: each step writes one op on
/// block `n`, with deps as the store computes them (the producer of the base, and for a create
/// every earlier delete of that id). Returns the base, the changes and the replayed state.
fn sequential(steps: &[(u8, u8)]) -> (StateSnapshot, Vec<Owned>, StateSnapshot) {
    let base = base_with(&[blk(0, "base 0"), blk(1, "base 1")]);
    let mut state = base.clone();
    let mut producers: HashMap<Hash, Hash> = HashMap::new();
    let mut deleters: HashMap<BlockId, Vec<Hash>> = HashMap::new();
    let mut out = vec![];
    for (i, (n, action)) in steps.iter().enumerate() {
        let id = Hash::of(format!("seq {i}").as_bytes());
        let bid = blk(*n, "").id;
        let (op, deps) = match state.blocks.get(&bid).copied() {
            None => {
                let deps = deleters.get(&bid).cloned().unwrap_or_default();
                (
                    Op::Create {
                        block: blk(*n, &format!("step {i}")),
                    },
                    deps,
                )
            }
            Some(v) => {
                let dep = producers.get(&v).copied().unwrap_or(Hash::of(CANON));
                if action % 3 == 0 {
                    (
                        Op::Delete {
                            base: v,
                            id: bid.clone(),
                        },
                        vec![dep],
                    )
                } else {
                    (
                        Op::Update {
                            base: v,
                            block: blk(*n, &format!("step {i}")),
                        },
                        vec![dep],
                    )
                }
            }
        };
        match &op {
            Op::Create { block } | Op::Update { block, .. } => {
                state.blocks.insert(bid.clone(), block.version_hash());
                producers.insert(block.version_hash(), id);
            }
            Op::Delete { .. } => {
                state.blocks.remove(&bid);
                deleters.entry(bid.clone()).or_default().push(id);
            }
        }
        out.push(Owned {
            id,
            deps,
            ops: vec![op],
        });
    }
    (base, out, state)
}

/// The reference: the same rules, written the slow and obvious way. It calls nothing from
/// `rdy_store::merge` except the plain data types, so a bug in `merge` or `conflict_id` cannot
/// cancel out here.
fn reference(
    base: &StateSnapshot,
    input: &[Owned],
    producer: &dyn Fn(&Hash) -> Option<Hash>,
) -> Merged {
    let mut uniq: Vec<&Owned> = vec![];
    for c in input {
        if !uniq.iter().any(|u| u.id == c.id) {
            uniq.push(c);
        }
    }
    let ids: Vec<Hash> = uniq.iter().map(|c| c.id).collect();
    let preds: Vec<Vec<Hash>> = uniq
        .iter()
        .map(|c| {
            c.deps
                .iter()
                .copied()
                .filter(|d| ids.contains(d) && *d != c.id)
                .collect()
        })
        .collect();
    let mut order: Vec<Hash> = vec![];
    while order.len() < uniq.len() {
        let ready = (0..uniq.len())
            .filter(|&i| !order.contains(&uniq[i].id) && preds[i].iter().all(|p| order.contains(p)))
            .map(|i| uniq[i].id)
            .min();
        match ready {
            Some(id) => order.push(id),
            None => {
                let mut rest: Vec<Hash> =
                    ids.iter().copied().filter(|i| !order.contains(i)).collect();
                rest.sort();
                order.extend(rest);
            }
        }
    }
    enum Slot {
        Clean(Option<Hash>, Option<Hash>),
        Conflicted(Option<Hash>, Vec<Side>),
    }
    let mut slots: BTreeMap<BlockId, Slot> = BTreeMap::new();
    for id in &order {
        let c = uniq.iter().find(|u| u.id == *id).unwrap();
        for op in &c.ops {
            let b = op.block_id().clone();
            let slot = slots.entry(b.clone()).or_insert_with(|| {
                let v = base.blocks.get(&b).copied();
                Slot::Clean(v, v.and_then(|v| producer(&v)))
            });
            let (expected, new) = match op {
                Op::Create { block } => (None, Some(block.version_hash())),
                Op::Update { base, block } => (Some(*base), Some(block.version_hash())),
                Op::Delete { base, .. } => (Some(*base), None),
            };
            let covered = |s: &Side| s.change.is_some_and(|x| c.deps.contains(&x));
            let next = match std::mem::replace(slot, Slot::Clean(None, None)) {
                Slot::Clean(v, _) if v == expected => Slot::Clean(new, Some(c.id)),
                Slot::Clean(v, o) if v == new => Slot::Clean(v, o),
                Slot::Clean(v, o) => Slot::Conflicted(
                    expected,
                    vec![
                        Side {
                            change: o,
                            version: v,
                        },
                        Side {
                            change: Some(c.id),
                            version: new,
                        },
                    ],
                ),
                Slot::Conflicted(_, sides)
                    if sides.iter().all(|s| s.change.is_none() || covered(s)) =>
                {
                    Slot::Clean(new, Some(c.id))
                }
                Slot::Conflicted(b0, mut sides) => {
                    sides.retain(|s| !covered(s));
                    sides.push(Side {
                        change: Some(c.id),
                        version: new,
                    });
                    Slot::Conflicted(b0, sides)
                }
            };
            *slot = next;
        }
    }
    let mut state = base.clone();
    let mut conflicts = vec![];
    for (b, slot) in slots {
        let v = match slot {
            Slot::Clean(v, _) => v,
            Slot::Conflicted(b0, mut sides) => {
                sides.sort();
                let id = Hash::of(&postcard::to_allocvec(&(&b, b0, &sides)).unwrap());
                conflicts.push(Conflict {
                    id,
                    block: b.clone(),
                    base: b0,
                    sides,
                });
                b0
            }
        };
        match v {
            Some(v) => {
                state.blocks.insert(b, v);
            }
            None => {
                state.blocks.remove(&b);
            }
        }
    }
    Merged {
        state,
        conflicts,
        order,
    }
}

#[derive(Clone, Debug)]
struct Spec {
    ops: Vec<(u8, u8, u8, u8)>,
    deps_mask: u8,
}

fn arb_specs() -> impl Strategy<Value = Vec<Spec>> {
    let op = (0u8..3, 0u8..3, any::<u8>(), 0u8..4);
    let spec = (prop::collection::vec(op, 1..3), any::<u8>())
        .prop_map(|(ops, deps_mask)| Spec { ops, deps_mask });
    prop::collection::vec(spec, 1..8)
}

/// Arbitrary (often conflicting) change sets: ops on blocks 0..3 whose bases are drawn from the
/// versions seen so far, with deps a random subset of earlier changes.
fn build(specs: &[Spec]) -> (StateSnapshot, Vec<Owned>) {
    let base = base_with(&[blk(0, "base 0"), blk(1, "base 1")]);
    let mut pool: Vec<Hash> = base.blocks.values().copied().collect();
    let mut out: Vec<Owned> = vec![];
    for (i, s) in specs.iter().enumerate() {
        let mut ops = vec![];
        for (n, kind, pick, body) in &s.ops {
            let b = blk(*n, &format!("body {body}"));
            let v = pool[*pick as usize % pool.len()];
            ops.push(match kind {
                0 => Op::Create { block: b },
                1 => Op::Update { base: v, block: b },
                _ => Op::Delete {
                    base: v,
                    id: b.id.clone(),
                },
            });
        }
        for op in &ops {
            if let Op::Create { block } | Op::Update { block, .. } = op {
                pool.push(block.version_hash());
            }
        }
        let deps = out
            .iter()
            .enumerate()
            .filter(|(j, _)| s.deps_mask & (1 << (j % 8)) != 0)
            .map(|(_, c)| c.id)
            .collect();
        out.push(Owned {
            id: Hash::of(format!("change {i}").as_bytes()),
            deps,
            ops,
        });
    }
    (base, out)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn merge1_prop_a_sequential_history_merges_cleanly_in_any_input_order(
        (base, input, end) in prop::collection::vec((0u8..4, any::<u8>()), 1..24)
            .prop_flat_map(|steps| {
                let (b, cs, end) = sequential(&steps);
                (Just(b), Just(cs).prop_shuffle(), Just(end))
            })
    ) {
        let m = merge(&base, &view(&input), &canon_producer(&base));
        prop_assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        prop_assert_eq!(m.state, end);
    }

    #[test]
    fn merge1_prop_the_merge_matches_the_reference(specs in arb_specs()) {
        let (base, input) = build(&specs);
        let p = canon_producer(&base);
        prop_assert_eq!(merge(&base, &view(&input), &p), reference(&base, &input, &p));
    }

    #[test]
    fn merge1_prop_input_order_does_not_matter(
        (base, a, b) in arb_specs().prop_flat_map(|specs| {
            let (base, input) = build(&specs);
            (Just(base), Just(input.clone()), Just(input).prop_shuffle())
        })
    ) {
        let p = canon_producer(&base);
        prop_assert_eq!(merge(&base, &view(&a), &p), merge(&base, &view(&b), &p));
    }
}
