//! cc11: the structural coverage layer carried across a write equals a rebuild, and a carry
//! builds nothing in full.
use rdy_engine::coverage::{
    carry_for_perf, rebuilt_for_tests, structural_builds, structural_for_tests,
};
use rdy_engine::*;
use rdy_graph::DerivedStatus;
use rdy_model::{BlockId, Hash};
use rdy_qa::fixtures::*;
use rdy_qa::tensions::{j_typo, try_put_j};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Every write of this file goes through `put_j` with `j_typo()`, so stable content takes it;
/// the agent is named after the view. An op the engine refuses is an error, never a panic.
fn write(r: &mut Rdy, view: &str, ops: Value) -> Result<WriteResult, RdyError> {
    try_put_j(r, view, ops, j_typo())
}

/// Task 6's world (`engine_coverage_issues.rs`): doc `ops` (overview: needs Summary and Goals;
/// has Summary, stable, and Empty) holds term Ledger, claim c1 (names Ledger twice, unlinked,
/// no decision), claim c2 (describes Ledger, decided_by d1), question q (unanswered), decision
/// d1 (live, applied by c2), decision d2 (live, nothing applies it), decision d3 (live,
/// needs_content: false). Doc `lone` holds one prose block. Landed.
fn cc6_world(r: &mut Rdy) -> BTreeMap<String, String> {
    let w = rdy_qa::fixtures::write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Summary", "lifecycle": "stable"}},
        {"op": "create", "type": "section", "id": "$empty", "parent": "$o", "fields": {"title": "Empty"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."},
        {"op": "create", "type": "claim", "id": "$c1", "parent": "$s", "body": "The Ledger is append-only, and the Ledger never shrinks."},
        {"op": "create", "type": "decision", "id": "$d1", "parent": "$s", "fields": {"statement": "Balances live in one store", "status": "proposed"}},
        {"op": "create", "type": "claim", "id": "$c2", "parent": "$s", "body": "Balances are kept in one place.",
         "links": {"describes": [{"target": "$t"}], "decided_by": [{"target": "$d1"}]}},
        {"op": "create", "type": "decision", "id": "$d2", "parent": "$s", "fields": {"statement": "Refunds need two approvers", "status": "proposed"}},
        {"op": "create", "type": "decision", "id": "$d3", "parent": "$s", "fields": {"statement": "Reviews happen on Fridays", "status": "proposed", "needs_content": false}},
        {"op": "create", "type": "question", "id": "$q", "parent": "$s", "fields": {"prompt": "Who approves refunds?"}},
        {"op": "create", "type": "doc", "id": "$l", "fields": {"name": "lone", "title": "Lone", "doc_type": "page"}},
        {"op": "create", "type": "prose", "parent": "$l", "body": "Nothing here."}
    ]))
    .unwrap();
    land(r, "work").unwrap(); // the landing accepts d1, d2 and d3: they are live
    w.created
}

/// The memoised (possibly carried) structural layer equals a full rebuild, block by block.
fn same(r: &Rdy, scope: &Scope) {
    let st = r.read();
    let carried = structural_for_tests(&st, scope).unwrap();
    let rebuilt = rebuilt_for_tests(&st, scope).unwrap();
    for (id, e) in rebuilt.blocks.iter() {
        assert_eq!(carried.blocks.get(id), Some(e), "block {id}");
    }
    assert_eq!(carried.blocks.len(), rebuilt.blocks.len());
    assert_eq!(carried.mentions, rebuilt.mentions);
}

#[test]
fn cc11_a_plain_write_carries_and_builds_nothing_in_full() {
    let (_d, mut r) = project();
    let c = cc6_world(&mut r);
    let v = Scope::View("next".into());
    write(
        &mut r,
        "next",
        json!([{"op": "create", "type": "prose", "parent": c["$s"], "body": "One more line."}]),
    )
    .unwrap();
    r.read().coverage_issues(&v).unwrap();
    let before = structural_builds();
    write(
        &mut r,
        "next",
        json!([{"op": "create", "type": "prose", "parent": c["$s"], "body": "And one more."}]),
    )
    .unwrap();
    r.read().coverage_issues(&v).unwrap();
    assert_eq!(structural_builds(), before, "carried, not rebuilt");
    same(&r, &v);
}

#[test]
fn cc11_a_rename_or_lifecycle_change_rebuilds() {
    let (_d, mut r) = project();
    let c = cc6_world(&mut r);
    let v = Scope::View("next".into());
    for ops in [
        json!([{"op": "update", "id": c["$t"], "fields": {"name": "Book"}, "body": "Holds balances."}]),
        json!([{"op": "update", "id": c["$s"], "fields": {"title": "Summary", "lifecycle": "draft"}}]),
    ] {
        write(&mut r, "next", ops).unwrap();
        same(&r, &v);
    }
}

#[test]
fn cc11_carried_equals_rebuilt_over_random_writes() {
    let (_d, mut r) = project();
    let c = cc6_world(&mut r);
    let v = Scope::View("next".into());
    let mut seed: u64 = 0x5eed;
    for step in 0..200 {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let ops = random_op(&r, &v, &c, seed, step); // one of the cases below, on live ids of the view
        if write(&mut r, "next", ops).is_ok() {
            r.read().coverage_issues(&v).unwrap();
            same(&r, &v);
        }
    }
}

/// Targeted writes the random ones rarely reach, each carried (no full build) and equal to a
/// rebuild: a ref that starts to dangle when its target goes; on canon, a landed decision that
/// supersedes the one a claim is decided by, then a landed decision that affects the entity the
/// claim describes. Deleting the entity then gives up and rebuilds, still equal.
#[test]
fn cc11_dangling_refs_and_landed_decisions_carry_exactly() {
    let (_d, mut r) = project();
    let c = cc6_world(&mut r);
    let v = Scope::View("next".into());
    let carried = |r: &mut Rdy, view: &str, scope: &Scope, ops: Value, landing: bool| {
        r.read().coverage_issues(scope).unwrap();
        let before = structural_builds();
        write(r, view, ops).unwrap();
        if landing {
            land(r, view).unwrap();
        }
        r.read().coverage_issues(scope).unwrap();
        assert_eq!(structural_builds(), before, "carried, not rebuilt");
        same(r, scope);
    };
    let cite = format!("See {{{{{}}}}}.", c["$c1"]);
    carried(
        &mut r,
        "next",
        &v,
        json!([{"op": "create", "type": "prose", "parent": c["$s"], "body": cite}]),
        false,
    );
    carried(
        &mut r,
        "next",
        &v,
        json!([{"op": "delete", "id": c["$c1"]}]),
        false,
    );

    let canon = Scope::Canon;
    let c2 = BlockId::parse(&c["$c2"]).unwrap();
    let ungrounded = |r: &Rdy| {
        r.read().coverage_issues(&canon).unwrap().blocks[&c2]
            .issues
            .iter()
            .any(|i| i.code == IssueCode::Ungrounded)
    };
    assert!(!ungrounded(&r), "c2 is decided by d1");
    carried(
        &mut r,
        "sup",
        &canon,
        json!([{"op": "create", "type": "decision", "parent": c["$s"],
        "fields": {"statement": "Balances live in two stores", "status": "proposed"},
        "links": {"supersedes": [{"target": c["$d1"]}]}}]),
        true,
    );
    assert!(ungrounded(&r), "d1 is superseded: c2 has no live decision");
    carried(
        &mut r,
        "aff",
        &canon,
        json!([{"op": "create", "type": "decision", "parent": c["$s"],
        "fields": {"statement": "The Ledger is audited", "status": "proposed"},
        "links": {"affects": [{"target": c["$t"]}]}}]),
        true,
    );
    assert!(
        !ungrounded(&r),
        "a live decision affects the entity c2 describes"
    );

    write(&mut r, "next", json!([{"op": "delete", "id": c["$t"]}])).unwrap();
    r.read().coverage_issues(&v).unwrap();
    same(&r, &v);
}

/// One element of `ids` picked by `seed`, or `fallback` when there is none.
fn pick(ids: &[BlockId], seed: u64, fallback: &str) -> String {
    if ids.is_empty() {
        return fallback.to_string();
    }
    ids[((seed >> 24) as usize) % ids.len()].to_string()
}

/// One random write on the live ids of `scope`, chosen by `seed % 10`: create prose naming
/// Ledger under a random section; create a claim that describes Ledger; update a random content
/// block's body; delete a random leaf block; add a `decided_by` from a random claim to a random
/// live decision; remove all links from a random claim; create a reference from a new prose
/// block to a random doc; create a question; create a decision that affects the term; create a
/// decision that supersedes a random live decision.
fn random_op(
    r: &Rdy,
    scope: &Scope,
    c: &BTreeMap<String, String>,
    seed: u64,
    step: usize,
) -> Value {
    let st = r.read();
    let m = st.materialized(scope).unwrap();
    let g = st.graph(scope).unwrap();
    let of_type = |t: &str| -> Vec<BlockId> {
        m.iter()
            .filter(|b| b.block_type == t)
            .map(|b| b.id.clone())
            .collect()
    };
    let sections = of_type("section");
    let claims = of_type("claim");
    let docs = of_type("doc");
    let content: Vec<BlockId> = m
        .iter()
        .filter(|b| b.block_type == "prose" || b.block_type == "claim")
        .map(|b| b.id.clone())
        .collect();
    let leaves: Vec<BlockId> = m
        .iter()
        .filter(|b| m.children(Some(&b.id)).is_empty())
        .map(|b| b.id.clone())
        .collect();
    let live: Vec<BlockId> = of_type("decision")
        .into_iter()
        .filter(|d| {
            matches!(
                g.status(d),
                Some(DerivedStatus::Live | DerivedStatus::PartiallySuperseded { .. })
            )
        })
        .collect();
    let sec = pick(&sections, seed, &c["$s"]);
    let term = c["$t"].clone();
    match seed % 10 {
        0 => json!([{"op": "create", "type": "prose", "parent": sec,
                     "body": format!("The Ledger grows at step {step}.")}]),
        1 => json!([{"op": "create", "type": "claim", "parent": sec,
                     "body": format!("Entries are final after step {step}."),
                     "links": {"describes": [{"target": term}]}}]),
        2 => {
            let body = if step.is_multiple_of(2) {
                format!("Rewritten at step {step}: the Ledger is kept.")
            } else {
                format!("Rewritten at step {step}: nothing named.")
            };
            json!([{"op": "update", "id": pick(&content, seed, &c["$c1"]), "body": body}])
        }
        3 => json!([{"op": "delete", "id": pick(&leaves, seed, &c["$q"])}]),
        4 => json!([{"op": "update", "id": pick(&claims, seed, &c["$c1"]),
                     "links": {"decided_by": [{"target": pick(&live, seed >> 8, &c["$d1"])}]}}]),
        5 => json!([{"op": "update", "id": pick(&claims, seed, &c["$c2"]), "links": {}}]),
        6 => json!([{"op": "create", "type": "prose", "parent": sec,
                     "body": format!("See {{{{{}}}}} at step {step}.", pick(&docs, seed >> 8, &c["$l"]))}]),
        7 => json!([{"op": "create", "type": "question", "parent": sec,
                     "fields": {"prompt": format!("Who owns step {step}?")}}]),
        8 => json!([{"op": "create", "type": "decision", "parent": sec,
                     "fields": {"statement": format!("Ledger rule {step}"), "status": "proposed"},
                     "links": {"affects": [{"target": term}]}}]),
        _ => json!([{"op": "create", "type": "decision", "parent": sec,
                     "fields": {"statement": format!("A new rule at step {step}"), "status": "proposed"},
                     "links": {"supersedes": [{"target": pick(&live, seed >> 8, &c["$d2"])}]}}]),
    }
}

/// Fix round 1: an entity rename that keeps the name's phrase ("Ledger" to "LEDGER" or to
/// "Ledger.") finds the same mentions, but an unlinked mention's fix names the entity's raw
/// name, and the mentioning blocks are not in the dirty set. So the carry gives up: in a view,
/// and in a rename landed on canon, the layer equals a rebuild.
#[test]
fn cc11_a_rename_that_keeps_the_phrase_equals_a_rebuild() {
    let (_d, mut r) = project();
    let c = cc6_world(&mut r);
    let v = Scope::View("next".into());
    write(
        &mut r,
        "next",
        json!([{"op": "create", "type": "prose", "parent": c["$s"], "body": "One more line."}]),
    )
    .unwrap();
    for name in ["LEDGER", "Ledger."] {
        r.read().coverage_issues(&v).unwrap();
        write(
            &mut r,
            "next",
            json!([{"op": "update", "id": c["$t"], "fields": {"name": name}, "body": "Holds balances."}]),
        )
        .unwrap();
        r.read().coverage_issues(&v).unwrap();
        same(&r, &v);
    }

    let canon = Scope::Canon;
    r.read().coverage_issues(&canon).unwrap();
    write(
        &mut r,
        "case",
        json!([{"op": "update", "id": c["$t"], "fields": {"name": "LEDGER"}, "body": "Holds balances."}]),
    )
    .unwrap();
    land(&mut r, "case").unwrap();
    r.read().coverage_issues(&canon).unwrap();
    same(&r, &canon);
}

/// Fix round 1: writes that change entries outside the dirty set give up, so the next read
/// builds the layer in full and it equals a rebuild: a folder renamed with a doc under it (the
/// doc's path changes); a section with children moved to another doc (the children's doc
/// changes); a doc's `doc_type` changed; a doc created in a folder; a doc moved into a folder.
#[test]
fn cc11_tree_writes_and_doc_type_changes_rebuild() {
    let (_d, mut r) = project();
    let c = cc6_world(&mut r);
    let v = Scope::View("next".into());
    let w = write(
        &mut r,
        "next",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "area", "title": "Area"}},
            {"op": "create", "type": "doc", "id": "$fd", "parent": "$f", "fields": {"name": "inner", "title": "Inner", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$fd", "body": "The Ledger is inside."},
            {"op": "create", "type": "section", "id": "$m", "parent": c["$o"], "fields": {"title": "Moving"}},
            {"op": "create", "type": "prose", "parent": "$m", "body": "The Ledger moves along."},
            {"op": "create", "type": "claim", "parent": "$m", "body": "Moves keep the Ledger."}
        ]),
    )
    .unwrap()
    .created;
    let rebuilds = |r: &mut Rdy, ops: Value| {
        r.read().coverage_issues(&v).unwrap();
        let before = structural_builds();
        write(r, "next", ops.clone()).unwrap();
        r.read().coverage_issues(&v).unwrap();
        assert!(structural_builds() > before, "gave up and rebuilt: {ops}");
        same(r, &v);
    };
    rebuilds(
        &mut r,
        json!([{"op": "update", "id": w["$f"], "fields": {"name": "area-two", "title": "Area"}}]),
    );
    rebuilds(
        &mut r,
        json!([{"op": "move", "id": w["$m"], "parent": c["$l"]}]),
    );
    rebuilds(
        &mut r,
        json!([{"op": "update", "id": c["$l"], "fields": {"name": "lone", "title": "Lone", "doc_type": "overview"}}]),
    );
    rebuilds(
        &mut r,
        json!([{"op": "create", "type": "doc", "parent": w["$f"], "fields": {"name": "second", "title": "Second", "doc_type": "page"}}]),
    );
    rebuilds(
        &mut r,
        json!([{"op": "move", "id": c["$l"], "parent": w["$f"]}]),
    );
}

/// Fix round 1: the perf hook `carry_for_perf` (what `coverage_carry` times) makes the call
/// `Built::after` makes: for the ops of one write, from the state before it to the state after,
/// it returns the layer the write carried, builds nothing in full, and gives up where the write
/// gave up.
#[test]
fn cc11_the_perf_hook_carries_as_a_write_does() {
    let (_d, mut r) = project();
    let c = cc6_world(&mut r);
    let v = Scope::View("next".into());
    write(
        &mut r,
        "next",
        json!([{"op": "create", "type": "prose", "parent": c["$s"], "body": "One more line."}]),
    )
    .unwrap();
    let hooked = |r: &mut Rdy, ops: Value| {
        let prev = r.read();
        prev.coverage_issues(&v).unwrap();
        let w = write(r, "next", ops).unwrap();
        let next = r.read();
        let change: Hash = w.change.parse().unwrap();
        let ops = next.reader().change(&change).unwrap().ops;
        let before = structural_builds();
        let out = carry_for_perf(&prev, &next, &v, &ops).unwrap();
        assert_eq!(
            structural_builds(),
            before,
            "the hook builds nothing in full"
        );
        out.map(|s| (s, structural_for_tests(&next, &v).unwrap()))
    };
    let (carried, written) = hooked(
        &mut r,
        json!([{"op": "create", "type": "claim", "parent": c["$s"], "body": "The Ledger is checked.",
                "links": {"describes": [{"target": c["$t"]}]}}]),
    )
    .expect("a plain write carries");
    assert_eq!(carried, *written, "the hook's layer is the write's");
    same(&r, &v);
    assert!(
        hooked(
            &mut r,
            json!([{"op": "update", "id": c["$t"], "fields": {"name": "Book"}, "body": "Holds balances."}]),
        )
        .is_none(),
        "a rename gives up"
    );
}
