//! sg2: order keys under rebase (spec §15.3 `rebase`, D-8EH6E). Views that append under one
//! parent from one base can end with equal order keys once they land; a tie is harmless when the
//! order it gives is stable and a later insert still lands where it was asked. Every state
//! passes the invariant checker, on canon and on each view. The ties that must hold are made on
//! purpose with `land_with_a_tie`, so they stay a tie whatever the engine's landing does.
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed, TestRunner};
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use serde_json::json;

fn canon() -> Scope {
    Scope::Canon
}

/// Canon and each view in `views` pass the checker.
fn all_valid(r: &Rdy, views: &[&str], when: &str) {
    if let Err(v) = check(r, &canon()) {
        panic!("canon broke {when}: {v:?}");
    }
    for view in views {
        if let Err(v) = check(r, &Scope::View((*view).into())) {
            panic!("view {view} broke {when}: {v:?}");
        }
    }
}

fn append(r: &mut Rdy, view: &str, section: &str, body: &str) -> String {
    let w = write_as(
        r,
        &as_agent(&format!("agent:{view}"), "r"),
        view,
        json!([{"op": "create", "type": "claim", "id": "$n", "parent": section, "body": body}]),
    )
    .expect("append");
    w.created["$n"].clone()
}

/// A project whose section holds one claim, "base"; returns it with the section and the claim.
fn seeded() -> (tempfile::TempDir, Rdy, String, String) {
    let (d, mut r) = project();
    let (_doc, section, claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").expect("land the seed");
    (d, r, section, claim)
}

#[test]
fn sg2_two_views_appending_under_one_parent_land_with_a_tie_or_distinct_keys() {
    let (_d, mut r, section, base) = seeded();
    let a = append(&mut r, "a", &section, "from a");
    let b = append(&mut r, "b", &section, "from b");
    all_valid(&r, &["a", "b"], "before any landing");
    land(&mut r, "a").expect("a lands");
    all_valid(&r, &["b"], "after a landed");
    land(&mut r, "b").expect("b lands after its rebase");
    all_valid(&r, &[], "after b landed");
    let section_id = BlockId::parse(&section).unwrap();
    let st = r.read();
    let m = st.materialized(&canon()).unwrap();
    let ids: Vec<String> = m
        .children(Some(&section_id))
        .iter()
        .map(|c| c.id.to_string())
        .collect();
    assert_eq!(ids.len(), 3, "canon lists all three claims: {ids:?}");
    for id in [&base, &a, &b] {
        assert!(ids.contains(id), "{id} is missing from {ids:?}");
    }
    assert_eq!(ids[0], base, "the seeded claim stays first: {ids:?}");
}

#[test]
fn sg2_a_tie_then_an_insert_between_stays_valid() {
    let (_d, mut r, section, base) = seeded();
    let a = append(&mut r, "a", &section, "from a");
    let b = append(&mut r, "b", &section, "from b");
    land_with_a_tie(&mut r, "a");
    land_with_a_tie(&mut r, "b");
    let section_id = BlockId::parse(&section).unwrap();
    let order_of = |r: &Rdy| -> Vec<String> {
        let st = r.read();
        let m = st.materialized(&canon()).unwrap();
        m.children(Some(&section_id))
            .iter()
            .map(|c| c.id.to_string())
            .collect()
    };
    let before = order_of(&r);
    assert_eq!(before.len(), 3, "{before:?}");
    let first_tied = before[1].clone();
    assert!(
        first_tied == a || first_tied == b,
        "the claim after the seeded one is a tied append: {before:?}"
    );
    let w = write_as(
        &mut r,
        &as_agent("agent:c", "r"),
        "c",
        json!([{"op": "create", "type": "claim", "id": "$n", "parent": section, "after": first_tied, "body": "between"}]),
    )
    .expect("insert after the first tied claim");
    let between = w.created["$n"].clone();
    all_valid(&r, &["c"], "after the insert in view c");
    land(&mut r, "c").expect("c lands");
    all_valid(&r, &[], "after the insert landed");
    let after = order_of(&r);
    assert_eq!(after.len(), 4, "{after:?}");
    // the insert is directly after the claim it names (an exact `after`, also under a tie: the
    // ruling of 2026-10-12 on D-8EH6E); the claims that were there keep their order
    let at = after.iter().position(|id| *id == between).unwrap();
    let place = after.iter().position(|id| *id == first_tied).unwrap();
    assert_eq!(
        at,
        place + 1,
        "the insert lands directly after {first_tied}, not after the whole tie: {after:?}"
    );
    let others: Vec<String> = after.iter().filter(|id| **id != between).cloned().collect();
    assert_eq!(others, before, "nothing else moved (base {base})");
    assert_eq!(order_of(&r), after, "the order is stable");
}

/// The order keys of the children of `section` in canon, in canon order.
fn keys_of(r: &Rdy, section: &str) -> Vec<(String, String)> {
    let section_id = BlockId::parse(section).unwrap();
    let st = r.read();
    let m = st.materialized(&canon()).unwrap();
    m.children(Some(&section_id))
        .iter()
        .map(|c| (c.id.to_string(), c.order.clone()))
        .collect()
}

/// A project whose section holds the seeded claim and `n` claims that tie on one order key
/// (n views appended from the same base; all landed by hand, see `land_with_a_tie`). Returns the
/// section and the children in canon order.
fn tied_run(n: usize) -> (tempfile::TempDir, Rdy, String, Vec<String>) {
    let (d, mut r, section, _base) = seeded();
    let names: Vec<String> = (0..n).map(|i| format!("t{i}")).collect();
    for name in &names {
        append(&mut r, name, &section, &format!("from {name}"));
    }
    for name in &names {
        land_with_a_tie(&mut r, name);
    }
    let ids = children_of(&r, &section);
    let keys = keys_of(&r, &section);
    assert!(
        keys[1..].iter().all(|(_, k)| *k == keys[1].1),
        "the run is a tie: {keys:?}"
    );
    (d, r, section, ids)
}

fn children_of(r: &Rdy, section: &str) -> Vec<String> {
    let section_id = BlockId::parse(section).unwrap();
    let st = r.read();
    let m = st.materialized(&canon()).unwrap();
    m.children(Some(&section_id))
        .iter()
        .map(|c| c.id.to_string())
        .collect()
}

/// Inserts a claim in view "c" with `anchor_op` (`after` or `before`) naming `anchor`, lands
/// it, and returns the new id.
fn insert_at(r: &mut Rdy, section: &str, anchor_op: &str, anchor: &str) -> String {
    let w = write_as(
        r,
        &as_agent("agent:c", "r"),
        "c",
        json!([{"op": "create", "type": "claim", "id": "$n", "parent": section, anchor_op: anchor, "body": "placed"}]),
    )
    .unwrap_or_else(|e| panic!("insert {anchor_op} {anchor}: {e:?}"));
    let id = w.created["$n"].clone();
    all_valid(r, &["c"], "after the insert in view c");
    land(r, "c").expect("c lands");
    all_valid(r, &[], "after the insert landed");
    id
}

#[test]
fn sg2_after_and_before_are_exact_anywhere_in_a_tied_run() {
    // run of 3 tied claims after the seeded one: every position, both directions
    for pos in 1..=3 {
        for dir in ["after", "before"] {
            let (_d, mut r, section, ids) = tied_run(3);
            assert_eq!(ids.len(), 4, "{ids:?}");
            let anchor = ids[pos].clone();
            let new = insert_at(&mut r, &section, dir, &anchor);
            let now = children_of(&r, &section);
            let mut want = ids.clone();
            let at = if dir == "after" { pos + 1 } else { pos };
            want.insert(at, new);
            assert_eq!(now, want, "{dir} {anchor} (position {pos}) in a tied run");
            assert_eq!(children_of(&r, &section), want, "the order is stable");
        }
    }
}

#[test]
fn sg2_a_move_after_a_tied_claim_lands_directly_after_it() {
    // (moved index, anchor index) in the canon order [base, x, y, z] where x, y, z tie
    for (moved, anchor) in [(0usize, 2usize), (3, 1), (1, 3), (2, 1)] {
        let (_d, mut r, section, ids) = tied_run(3);
        write_as(
            &mut r,
            &as_agent("agent:c", "r"),
            "c",
            json!([{"op": "move", "id": ids[moved], "parent": section, "after": ids[anchor]}]),
        )
        .unwrap_or_else(|e| panic!("move {moved} after {anchor}: {e:?}"));
        all_valid(&r, &["c"], "after the move in view c");
        land(&mut r, "c").expect("c lands");
        all_valid(&r, &[], "after the move landed");
        let mut want = ids.clone();
        let m = want.remove(moved);
        let at = want.iter().position(|id| *id == ids[anchor]).unwrap() + 1;
        want.insert(at, m);
        assert_eq!(
            children_of(&r, &section),
            want,
            "move {moved} after {anchor}"
        );
    }
}

#[test]
fn sg2_order_run_gives_short_increasing_keys_strictly_between_two_bounds() {
    use rdy_model::{MAX_ORDER_KEY_LEN, OrderError, order_run, valid_order_key};
    let bounds: [(Option<&str>, Option<&str>); 6] = [
        (Some("a1"), Some("a2")),
        (Some("a1"), Some("a1V")),
        (Some("a9"), Some("b01")),
        (None, Some("a1")),
        (Some("a1"), None),
        (None, None),
    ];
    for (lo, hi) in bounds {
        for n in [0usize, 1, 2, 3, 7] {
            let keys = order_run(lo, hi, n).unwrap_or_else(|e| panic!("{lo:?}..{hi:?} x{n}: {e}"));
            assert_eq!(keys.len(), n, "{lo:?}..{hi:?}");
            let mut prev: Option<String> = lo.map(|s| s.to_string());
            for k in &keys {
                assert!(valid_order_key(k), "{k:?} is not a valid key");
                assert!(k.len() <= MAX_ORDER_KEY_LEN);
                if let Some(p) = &prev {
                    assert!(p < k, "{p:?} < {k:?} ({lo:?}..{hi:?} x{n})");
                }
                prev = Some(k.clone());
            }
            if let (Some(last), Some(hi)) = (keys.last(), hi) {
                assert!(last.as_str() < hi, "{last:?} < {hi:?}");
            }
        }
    }
    // no room between equal or reversed bounds: an error, never a panic
    assert!(matches!(
        order_run(Some("a1"), Some("a1"), 2),
        Err(OrderError::NoRoom(..))
    ));
    assert!(order_run(Some("a2"), Some("a1"), 1).is_err());
    assert!(matches!(
        order_run(Some("a1\u{0}"), None, 1),
        Err(OrderError::Malformed(_))
    ));
}

#[test]
fn sg2_rebase_preserves_relative_order_of_many_appends() {
    let config = Config {
        cases: cases(16),
        rng_seed: RngSeed::Fixed(0x71e5_0b17),
        failure_persistence: None,
        max_shrink_iters: 256,
        ..Config::default()
    };
    let mut runner = TestRunner::new(config);
    // each view appends 1 to 5 claims; the landing order is a shuffle of the views
    let strategy = proptest::collection::vec(1usize..=5, 2..=4).prop_flat_map(|counts| {
        let n = counts.len();
        (
            Just(counts),
            Just((0..n).collect::<Vec<_>>()).prop_shuffle(),
        )
    });
    let result = runner.run(&strategy, |(counts, order)| {
        rdy_model::seed_ids_for_tests(0x71e5_0000 + counts.iter().sum::<usize>() as u64);
        let outcome = std::panic::catch_unwind(|| {
            let (_d, mut r, section, _base) = seeded();
            let names: Vec<String> = (0..counts.len()).map(|i| format!("v{i}")).collect();
            let mut own: Vec<Vec<String>> = vec![];
            for (i, &n) in counts.iter().enumerate() {
                own.push(
                    (0..n)
                        .map(|j| append(&mut r, &names[i], &section, &format!("v{i} claim {j}")))
                        .collect(),
                );
            }
            let section_id = BlockId::parse(&section).unwrap();
            for &i in &order {
                land(&mut r, &names[i]).unwrap_or_else(|e| panic!("landing {}: {e:?}", names[i]));
                let views: Vec<&str> = names.iter().map(String::as_str).collect();
                all_valid(&r, &views, &format!("after landing {}", names[i]));
            }
            let st = r.read();
            let m = st.materialized(&canon()).unwrap();
            let ids: Vec<String> = m
                .children(Some(&section_id))
                .iter()
                .map(|c| c.id.to_string())
                .collect();
            assert_eq!(ids.len(), 1 + counts.iter().sum::<usize>(), "{ids:?}");
            for (i, mine) in own.iter().enumerate() {
                let seen: Vec<&String> = ids.iter().filter(|id| mine.contains(id)).collect();
                let want: Vec<&String> = mine.iter().collect();
                assert_eq!(
                    seen, want,
                    "view v{i}'s appends changed their relative order (landing order {order:?})"
                );
            }
        });
        rdy_model::unseed_ids_for_tests();
        outcome.map_err(|e| {
            let text = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "a panic".into());
            TestCaseError::fail(text)
        })
    });
    if let Err(e) = result {
        panic!("{e}");
    }
}

/// A write that edits a sibling of a tied run and asks for a gap inside the run (finding 1 of
/// fix round 2). The insert re-keys the smaller side of the run; the sibling the write also
/// edits takes the new key in the same op, in either order of the two ops.
fn tied_pair_write(
    ops: impl FnOnce(&[String], &str) -> serde_json::Value,
) -> (tempfile::TempDir, Rdy, String, Vec<String>, String) {
    let (d, mut r, section, ids) = tied_run(2);
    assert_eq!(ids.len(), 3, "{ids:?}");
    let w = write_as(&mut r, &as_agent("agent:c", "r"), "c", ops(&ids, &section))
        .unwrap_or_else(|e| panic!("the write is valid: {e:?}"));
    let new = w.created.get("$n").cloned().unwrap_or_default();
    all_valid(&r, &["c"], "after the write in view c");
    (d, r, section, ids, new)
}

fn view_children(r: &Rdy, view: &str, section: &str) -> Vec<String> {
    let section_id = BlockId::parse(section).unwrap();
    let st = r.read();
    let m = st.materialized(&Scope::View(view.into())).unwrap();
    m.children(Some(&section_id))
        .iter()
        .map(|c| c.id.to_string())
        .collect()
}

#[test]
fn sg2_an_update_of_a_tied_sibling_then_an_insert_into_the_tie() {
    // [base, x, y], x and y tie; the up side of an insert after x is [y], which the write edited
    let (_d, mut r, section, ids, new) = tied_pair_write(|ids, section| {
        json!([
            {"op": "update", "id": ids[2], "body": "y edited"},
            {"op": "create", "type": "claim", "id": "$n", "parent": section, "after": ids[1], "body": "between"}
        ])
    });
    let (x, y) = (&ids[1], &ids[2]);
    assert_eq!(
        view_children(&r, "c", &section),
        vec![ids[0].clone(), x.clone(), new.clone(), y.clone()]
    );
    assert_eq!(body_of(&r, &Scope::View("c".into()), y), "y edited");
    land(&mut r, "c").expect("c lands");
    all_valid(&r, &[], "after the write landed");
    assert_eq!(
        children_of(&r, &section),
        vec![ids[0].clone(), x.clone(), new, y.clone()]
    );
    assert_eq!(body_of(&r, &canon(), y), "y edited");
}

#[test]
fn sg2_an_insert_into_the_tie_then_an_update_of_the_re_keyed_sibling() {
    let (_d, mut r, section, ids) = tied_run(2);
    let (x, y) = (ids[1].clone(), ids[2].clone());
    let read = version_of(&r, &canon(), &y);
    let w = write_as(
        &mut r,
        &as_agent("agent:c", "r"),
        "c",
        json!([
            {"op": "create", "type": "claim", "id": "$n", "parent": section, "after": x, "body": "between"},
            {"op": "update", "id": y, "base": read, "body": "y edited"}
        ]),
    )
    .expect("the update of the sibling the insert re-keyed is valid");
    let new = w.created["$n"].clone();
    all_valid(&r, &["c"], "after the write in view c");
    assert_eq!(
        view_children(&r, "c", &section),
        vec![ids[0].clone(), x.clone(), new.clone(), y.clone()]
    );
    assert_eq!(body_of(&r, &Scope::View("c".into()), &y), "y edited");
    land(&mut r, "c").expect("c lands");
    all_valid(&r, &[], "after the write landed");
    assert_eq!(
        children_of(&r, &section),
        vec![ids[0].clone(), x, new, y.clone()]
    );
    assert_eq!(body_of(&r, &canon(), &y), "y edited");
}

#[test]
fn sg2_an_insert_into_the_tie_then_a_delete_of_the_re_keyed_sibling() {
    let (_d, mut r, section, ids) = tied_run(2);
    let (x, y) = (ids[1].clone(), ids[2].clone());
    let w = write_as(
        &mut r,
        &as_agent("agent:c", "r"),
        "c",
        json!([
            {"op": "create", "type": "claim", "id": "$n", "parent": section, "after": x, "body": "between"},
            {"op": "delete", "id": y}
        ]),
    )
    .expect("the delete of the sibling the insert re-keyed is valid");
    let new = w.created["$n"].clone();
    all_valid(&r, &["c"], "after the write in view c");
    assert_eq!(
        view_children(&r, "c", &section),
        vec![ids[0].clone(), x.clone(), new.clone()]
    );
    land(&mut r, "c").expect("c lands");
    all_valid(&r, &[], "after the write landed");
    assert_eq!(children_of(&r, &section), vec![ids[0].clone(), x, new]);
}

#[test]
fn sg2_an_insert_into_the_tie_after_both_tied_siblings_were_edited() {
    // both sides of the gap are touched: nothing is left to refuse, since the re-key folds into
    // the op that already changes the block
    let (_d, r, section, ids, new) = tied_pair_write(|ids, section| {
        json!([
            {"op": "update", "id": ids[1], "body": "x edited"},
            {"op": "update", "id": ids[2], "body": "y edited"},
            {"op": "create", "type": "claim", "id": "$n", "parent": section, "after": ids[1], "body": "between"}
        ])
    });
    assert_eq!(
        view_children(&r, "c", &section),
        vec![ids[0].clone(), ids[1].clone(), new, ids[2].clone()]
    );
    assert_eq!(body_of(&r, &Scope::View("c".into()), &ids[1]), "x edited");
    assert_eq!(body_of(&r, &Scope::View("c".into()), &ids[2]), "y edited");
}

#[test]
fn sg2_two_inserts_into_one_tie_in_one_write() {
    // the second insert re-keys a sibling the first one already re-keyed
    let (_d, r, section, ids, new) = tied_pair_write(|ids, section| {
        json!([
            {"op": "create", "type": "claim", "id": "$n", "parent": section, "after": ids[1], "body": "first"},
            {"op": "create", "type": "claim", "id": "$m", "parent": section, "after": ids[1], "body": "second"}
        ])
    });
    let kids = view_children(&r, "c", &section);
    assert_eq!(kids.len(), 5, "{kids:?}");
    assert_eq!(kids[0], ids[0]);
    assert_eq!(kids[1], ids[1]);
    assert_eq!(kids[4], ids[2]);
    assert!(kids.contains(&new));
}

#[test]
fn sg2_an_insert_into_a_tie_conflicts_with_a_concurrent_edit_of_the_re_keyed_sibling() {
    // The fallback for a tie already in a state (an imported tree, a store from before the
    // landing re-key). The insert re-keys the smaller side of the run, which writes the sibling
    // the agent did not touch; another view's edit of it then conflicts. This test documents
    // that cost: it is why ties are stopped at landing, and why the fallback stays rare.
    let (_d, mut r, section, ids) = tied_run(2);
    let (x, y) = (ids[1].clone(), ids[2].clone());
    write_as(
        &mut r,
        &as_agent("agent:c", "r"),
        "c",
        json!([{"op": "create", "type": "claim", "parent": section, "after": x, "body": "between"}]),
    )
    .unwrap();
    let read = version_of(&r, &canon(), &y);
    write_as(
        &mut r,
        &as_agent("agent:d", "r"),
        "d",
        json!([{"op": "update", "id": y, "base": read, "body": "y edited in d"}]),
    )
    .unwrap();
    land(&mut r, "c").expect("c lands");
    let conflicts = r.read().conflicts(&Scope::View("d".into())).unwrap();
    assert_eq!(
        conflicts
            .iter()
            .map(|c| c.block.to_string())
            .collect::<Vec<_>>(),
        vec![y.clone()],
        "d's edit of {y} conflicts with c's re-key of it"
    );
    assert!(
        land(&mut r, "d").is_err(),
        "d cannot land until the conflict on {y} is resolved"
    );
}
