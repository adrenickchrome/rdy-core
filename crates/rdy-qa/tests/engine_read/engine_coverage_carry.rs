//! cc11: the structural coverage layer carried across a write equals a rebuild, and a carry
//! builds nothing in full.
use rdy_engine::coverage::{
    carried_entries, carry_for_perf, coverage_change, governed_scans, issues_rebuilt_for_tests,
    kinds_builds, rebuilt_for_tests, state_layer_builds, structural_builds, structural_for_tests,
};
use rdy_engine::mention_review::MentionVerdict;
use rdy_engine::*;
use rdy_graph::DerivedStatus;
use rdy_model::{BlockId, Hash};
use rdy_qa::fixtures::*;
use rdy_qa::tensions::{j_typo, try_put_j};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

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
    let mut written: BTreeMap<u64, usize> = BTreeMap::new();
    for step in 0..300 {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let ops = random_op(&r, &v, &c, seed, step); // one of the cases below, on live ids of the view
        if write(&mut r, "next", ops).is_ok() {
            *written.entry(seed % 15).or_default() += 1;
            r.read().coverage_issues(&v).unwrap();
            same(&r, &v);
            same_issues(&r, &v); // fix round 2: the carried issues too
        }
    }
    // final review I1, I4: the new cases are reached
    for case in 10..15 {
        assert!(
            written.get(&case).is_some_and(|n| *n > 0),
            "case {case}: {written:?}"
        );
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

/// One random write on the live ids of `scope`, chosen by `seed % 15`: create prose naming
/// Ledger under a random section; create a claim that describes Ledger; update a random content
/// block's body; delete a random leaf block; add a `decided_by` from a random claim to a random
/// live decision; remove all links from a random claim; create a reference from a new prose
/// block to a random doc; create a question; create a decision that affects the term; create a
/// decision that supersedes a random live decision; edit the term's body; edit a proposed
/// decision's statement; create a deprecated section; move the term under a random section
/// (in or out of a deprecated one); delete a random section (refused while it has children).
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
    let proposed: Vec<BlockId> = m
        .iter()
        .filter(|b| b.block_type == "decision" && b.field_str("status") == Some("proposed"))
        .map(|b| b.id.clone())
        .collect();
    let sec = pick(&sections, seed, &c["$s"]);
    let term = c["$t"].clone();
    match seed % 15 {
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
        9 => json!([{"op": "create", "type": "decision", "parent": sec,
                     "fields": {"statement": format!("A new rule at step {step}"), "status": "proposed"},
                     "links": {"supersedes": [{"target": pick(&live, seed >> 8, &c["$d2"])}]}}]),
        // final review I1: body and field edits that reach no pointing block
        10 => json!([{"op": "update", "id": term, "fields": {"name": "Ledger"},
                      "body": format!("Holds balances, step {step}.")}]),
        11 => json!([{"op": "update", "id": pick(&proposed, seed, &c["$d2"]),
                      "fields": {"statement": format!("Restated at step {step}"), "status": "proposed"}}]),
        // final review I4: the term moves in and out of a deprecated section
        12 => json!([{"op": "create", "type": "section", "parent": c["$o"],
                      "fields": {"title": format!("Retired {step}"), "lifecycle": "deprecated"}}]),
        13 => json!([{"op": "move", "id": term, "parent": sec}]),
        // a section deleted (a view refuses it while it has children)
        _ => json!([{"op": "delete", "id": pick(&sections, seed, &c["$s"])}]),
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
/// changes); a doc's `doc_type` changed; a doc moved into a folder. (perf3: a doc created with
/// a stored name carries; `perf3_a_named_new_doc_carries`.)
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
        json!([{"op": "move", "id": c["$l"], "parent": w["$f"]}]),
    );
}

/// perf3: a doc or a folder created with a stored name, beside no nameless doc, moves no other
/// block's path, so the layer and the coverage issues carry (a `new_doc` reply reads its
/// coverage change from them): no full build of either, and both equal a rebuild. A doc in a
/// new folder, a doc at the root, and a doc by `new_doc` with its skeleton.
#[test]
fn perf3_a_named_new_doc_carries() {
    let (_d, mut r) = project();
    let c = cc6_world(&mut r);
    let v = Scope::View("next".into());
    write(
        &mut r,
        "next",
        json!([{"op": "create", "type": "prose", "parent": c["$s"], "body": "One more line."}]),
    )
    .unwrap();
    let carries = |r: &mut Rdy, ops: Option<Value>| {
        r.read().coverage_issues(&v).unwrap();
        let (layers, states) = (structural_builds(), state_layer_builds());
        match &ops {
            Some(ops) => {
                write(r, "next", ops.clone()).unwrap();
            }
            None => {
                r.new_doc(&writer(), "next", "made/by-new-doc", None, None)
                    .unwrap();
            }
        }
        r.read().coverage_issues(&v).unwrap();
        assert_eq!(structural_builds(), layers, "the layer carried: {ops:?}");
        assert_eq!(state_layer_builds(), states, "the issues carried: {ops:?}");
        same(r, &v);
        same_issues(r, &v);
    };
    carries(
        &mut r,
        Some(json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "area", "title": "Area"}},
            {"op": "create", "type": "doc", "id": "$fd", "parent": "$f", "fields": {"name": "inner", "title": "Inner", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$fd", "body": "The Ledger is inside."}
        ])),
    );
    carries(
        &mut r,
        Some(json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "second", "title": "Second", "doc_type": "overview"}},
            {"op": "create", "type": "section", "parent": "$d", "fields": {"title": "Summary"}},
            {"op": "create", "type": "claim", "parent": c["$s"], "body": "See the second page.", "links": {"depends_on": [{"target": "$d"}]}}
        ])),
    );
    carries(&mut r, None);
}

/// perf3 review: a named doc created beside a nameless one (an M1 doc without a stored name,
/// committed raw) gives up (`!has_nameless_under`), and the next read equals a rebuild. The
/// guard is kept although no write reaches a divergence: a write refuses a name that any
/// sibling's effective name holds (`E_PATH_CLASH`), and a nameless doc's effective name is the
/// first free one of its candidates, so a name no sibling holds changes no derived name.
#[test]
fn perf3_a_named_doc_beside_a_nameless_one_rebuilds() {
    let (_d, mut r) = project();
    cc6_world(&mut r);
    // committed raw in a view of its own and landed raw (no write takes a nameless doc)
    raw_commit(
        &mut r,
        "raw",
        vec![
            raw(
                "d-NNNNN",
                "doc",
                None,
                "W",
                &[("title", "Legacy"), ("doc_type", "page")],
                "",
            ),
            raw(
                "p-NNNNN",
                "prose",
                Some("d-NNNNN"),
                "V",
                &[],
                "The Ledger was here.",
            ),
        ],
    );
    let p = r
        .store_mut()
        .propose("raw", "raw", writer().provenance())
        .unwrap();
    r.store_mut().land(&p, approver().provenance()).unwrap();
    r.invalidate();
    let v = Scope::View("next".into());
    write(
        &mut r,
        "next",
        json!([{"op": "create", "type": "prose", "parent": "d-NNNNN", "body": "One more line."}]),
    )
    .unwrap();
    r.read().coverage_issues(&v).unwrap();
    // the effective name is held: the write is refused
    let clash = write(
        &mut r,
        "next",
        json!([{"op": "create", "type": "doc", "fields": {"name": "LEGACY", "title": "Clash", "doc_type": "page"}}]),
    )
    .unwrap_err();
    assert_eq!(clash.code, E_PATH_CLASH);
    for (name, title) in [("legacy-2", "Legacy"), ("fresh", "Legacy")] {
        r.read().coverage_issues(&v).unwrap();
        let before = structural_builds();
        write(
            &mut r,
            "next",
            json!([{"op": "create", "type": "doc", "fields": {"name": name, "title": title, "doc_type": "page"}}]),
        )
        .unwrap();
        r.read().coverage_issues(&v).unwrap();
        assert!(structural_builds() > before, "{name}: the carry gave up");
        same(&r, &v);
        same_issues(&r, &v);
    }
    assert_eq!(
        r.read()
            .materialized(&v)
            .unwrap()
            .name_of(&BlockId::parse("d-NNNNN").unwrap()),
        Some("legacy".to_string())
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

/// The memoised (possibly carried) coverage issues equal a full build of them.
fn same_issues(r: &Rdy, scope: &Scope) {
    let st = r.read();
    let memo = st.coverage_issues(scope).unwrap();
    let rebuilt = issues_rebuilt_for_tests(&st, scope).unwrap();
    for (id, e) in rebuilt.blocks.iter() {
        assert_eq!(memo.blocks.get(id), Some(e), "{scope:?}: block {id}");
    }
    assert_eq!(*memo, rebuilt, "{scope:?}");
}

/// Whether `scope` has an issue with `code`.
fn has_issue(r: &Rdy, scope: &Scope, code: IssueCode) -> bool {
    r.read()
        .coverage_issues(scope)
        .unwrap()
        .blocks
        .values()
        .any(|e| e.issues.iter().any(|i| i.code == code))
}

/// Fix round 2 (FW-PF2): the first coverage read after a write carries the issues of the
/// state before (tension and suspect issues included) instead of applying every counted
/// tension again: a plain write in a view builds no state layer in full, for the view or for
/// canon, and both equal a full build.
#[test]
fn cc11_fix2_a_write_carries_the_coverage_issues() {
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
    r.read().coverage_summary(&Scope::Canon).unwrap();
    let before = state_layer_builds();
    write(
        &mut r,
        "next",
        json!([{"op": "create", "type": "prose", "parent": c["$s"], "body": "And one more."}]),
    )
    .unwrap();
    r.read().coverage_issues(&v).unwrap();
    r.read().coverage_summary(&Scope::Canon).unwrap();
    assert_eq!(state_layer_builds(), before, "carried, not built in full");
    same_issues(&r, &v);
    same_issues(&r, &Scope::Canon);
}

/// Fix round 2: the carried issues equal a full build through writes that open tensions (a
/// duplicate in a view, an edit collision between two views, which the view not written counts
/// too), a suspect, a landing (canon carried), a tension dismissal and a mention dismissal (the
/// last two build in full). Each scope is read before every step, as status and write replies
/// read it.
#[test]
fn cc11_fix2_carried_issues_equal_a_full_build_through_tensions_and_suspects() {
    let (_d, mut r) = project();
    let c = cc6_world(&mut r);
    let (canon, a, b) = (
        Scope::Canon,
        Scope::View("a".into()),
        Scope::View("b".into()),
    );
    let scopes = [canon.clone(), a.clone(), b.clone()];
    let read_all = |r: &Rdy| {
        for s in &scopes {
            r.read().coverage_issues(s).unwrap();
        }
    };
    let check = |r: &Rdy, carried: &[&Scope], before: u64, what: &str| {
        for s in carried {
            r.read().coverage_issues(s).unwrap();
        }
        assert_eq!(
            state_layer_builds(),
            before,
            "{what}: carried, not built in full"
        );
        for s in &scopes {
            same_issues(r, s);
        }
    };
    // each view starts with a change of its own
    for view in ["a", "b", "c"] {
        write(
            &mut r,
            view,
            json!([{"op": "create", "type": "prose", "parent": c["$s"], "body": format!("Notes of {view}.")}]),
        )
        .unwrap();
    }
    read_all(&r);

    // a landing: canon's layer is carried
    let before = state_layer_builds();
    land(&mut r, "c").unwrap();
    check(&r, &[&canon], before, "a landing");
    read_all(&r);

    // a duplicate in view a
    let before = state_layer_builds();
    let w = write(&mut r, "a", json!([
        {"op": "create", "type": "section", "id": "$x", "parent": c["$o"], "fields": {"title": "Fees", "lifecycle": "draft"}},
        {"op": "create", "type": "claim", "parent": "$x", "body": "Fees are charged every month."},
        {"op": "create", "type": "claim", "parent": "$x", "body": "Fees are charged every month."}
    ]))
    .unwrap();
    check(&r, &[&a, &b, &canon], before, "a duplicate");
    assert!(
        has_issue(&r, &a, IssueCode::OpenTension),
        "the duplicate counts in a"
    );
    read_all(&r);

    // an edit collision: b's write opens a tension view a counts too
    write(
        &mut r,
        "a",
        json!([{"op": "update", "id": c["$c1"], "body": "The Ledger is append-only."}]),
    )
    .unwrap();
    read_all(&r);
    let collided = |r: &Rdy, s: &Scope| {
        r.read().coverage_issues(s).unwrap().blocks[&BlockId::parse(&c["$c1"]).unwrap()]
            .issues
            .iter()
            .any(|i| i.code == IssueCode::OpenTension)
    };
    assert!(!collided(&r, &a), "no collision yet");
    let before = state_layer_builds();
    write(
        &mut r,
        "b",
        json!([{"op": "update", "id": c["$c1"], "body": "The Ledger only grows."}]),
    )
    .unwrap();
    check(&r, &[&a, &b, &canon], before, "an edit collision");
    assert!(collided(&r, &a), "view a counts the collision b opened");
    read_all(&r);

    // a suspect: a pinned citation, then its target edited
    let cite = format!("Balances: {{{{{}}}}}.", c["$c2"]);
    write(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "parent": c["$s"], "body": cite}]),
    )
    .unwrap();
    read_all(&r);
    let before = state_layer_builds();
    write(
        &mut r,
        "a",
        json!([{"op": "update", "id": c["$c2"], "body": "Balances are kept in one store."}]),
    )
    .unwrap();
    check(&r, &[&a, &b, &canon], before, "a suspect");
    assert!(
        has_issue(&r, &a, IssueCode::OpenSuspect),
        "the citation is suspect"
    );
    read_all(&r);

    // a tension dismissal and a mention dismissal: built in full, still equal
    let x = BlockId::parse(&w.created["$x"]).unwrap();
    let st = r.read();
    let dup = st
        .tensions()
        .unwrap()
        .iter()
        .find(|t| {
            t.kind == tensions::TensionKind::DuplicateDefinition
                && st
                    .materialized(&a)
                    .unwrap()
                    .get(&t.blocks()[0])
                    .is_some_and(|blk| blk.parent.as_ref() == Some(&x))
        })
        .map(|t| tensions::tension_label(&t.id))
        .expect("the duplicate in a");
    drop(st);
    r.dismiss_tension(&writer(), &dup, "said twice on purpose")
        .unwrap();
    for s in &scopes {
        same_issues(&r, s);
    }
    assert!(has_issue(&r, &a, IssueCode::DismissedTension));
    read_all(&r);
    r.review_mention(
        &writer(),
        &canon,
        &c["$c1"],
        &c["$t"],
        MentionVerdict::Dismiss,
        "everyday word",
    )
    .unwrap();
    for s in &scopes {
        same_issues(&r, s);
    }
}

/// Fix round 3 (FW-PF2): a write reply's coverage change reads the carry, the scores before
/// and after and the touched pages' rollups. It counts no entity kinds (a summary's part,
/// for status and overview) and builds no state layer in full; its scores are the summaries'.
#[test]
fn cc11_fix3_a_write_replys_coverage_change_counts_no_kinds() {
    let (_d, mut r) = project();
    let c = cc6_world(&mut r);
    let v = Scope::View("next".into());
    write(
        &mut r,
        "next",
        json!([{"op": "create", "type": "prose", "parent": c["$s"], "body": "One more line."}]),
    )
    .unwrap();
    let prev = r.read();
    // the previous reply read the state before
    coverage_change(&prev, &v, &prev, &v, &BTreeSet::new()).unwrap();
    let w = write(
        &mut r,
        "next",
        json!([{"op": "create", "type": "claim", "parent": c["$s"], "body": "The Ledger is checked."}]),
    )
    .unwrap();
    let next = r.read();
    let change: Hash = w.change.parse().unwrap();
    let touched: BTreeSet<BlockId> = next
        .reader()
        .change(&change)
        .unwrap()
        .ops
        .iter()
        .map(|o| o.block_id().clone())
        .collect();
    let (kinds, layers) = (kinds_builds(), state_layer_builds());
    let got = coverage_change(&prev, &v, &next, &v, &touched).unwrap();
    assert_eq!(
        kinds_builds(),
        kinds,
        "a write reply counts no entity kinds"
    );
    assert_eq!(state_layer_builds(), layers, "the issues after are carried");
    assert_eq!(got.before, prev.coverage_summary(&v).unwrap().score);
    assert_eq!(got.after, next.coverage_summary(&v).unwrap().score);
    assert!(kinds_builds() > kinds, "a summary still counts the kinds");
    assert_eq!(got.pages.len(), 1, "the touched page: {got:?}");
}

/// Fix round 3: a write can open a suspect on a block the structural carry does not recompute
/// and no tension names. Decision r refines d1 (a link no structural check follows from d1's
/// liveness, and not a pointing edge, so no deprecated reference); a landing then supersedes
/// d1. r becomes suspect, and its entry is rebuilt only because the suspects changed: the
/// carried issues equal a full build and name the suspect.
#[test]
fn cc11_fix3_a_suspect_the_structural_carry_misses_is_carried() {
    let (_d, mut r) = project();
    let c = cc6_world(&mut r);
    let canon = Scope::Canon;
    let w = write(
        &mut r,
        "ref",
        json!([{"op": "create", "type": "decision", "id": "$r", "parent": c["$s"],
        "fields": {"statement": "Balances are read from the primary store", "status": "proposed"},
        "links": {"refines": [{"target": c["$d1"]}]}}]),
    )
    .unwrap();
    land(&mut r, "ref").unwrap();
    let refines = BlockId::parse(&w.created["$r"]).unwrap();
    let suspect = |r: &Rdy| {
        r.read().coverage_issues(&canon).unwrap().blocks[&refines]
            .issues
            .iter()
            .any(|i| i.code == IssueCode::OpenSuspect)
    };
    assert!(!suspect(&r), "d1 is live: nothing to check yet");
    let (layers, structural) = (state_layer_builds(), structural_builds());
    write(
        &mut r,
        "sup",
        json!([{"op": "create", "type": "decision", "parent": c["$s"],
        "fields": {"statement": "Balances live in two stores", "status": "proposed"},
        "links": {"supersedes": [{"target": c["$d1"]}]}}]),
    )
    .unwrap();
    let p = r
        .propose(&writer(), "sup", "sup", Some(rdy_qa::intake::TEST_SUMMARY))
        .unwrap();
    assert_eq!(
        (state_layer_builds(), structural_builds()),
        (layers, structural),
        "the write and the proposal build nothing in full"
    );
    // F2: the person confirms the exception (the supersede) first. The queue they read builds
    // the prospective canon in full; the landing's own carry is measured from here.
    rdy_qa::intake::confirm_before_landing(&mut r, &approver(), &p);
    let (layers, structural) = (state_layer_builds(), structural_builds());
    r.land(&approver(), &p.id).unwrap();
    r.read().coverage_issues(&canon).unwrap();
    assert_eq!(
        structural_builds(),
        structural,
        "the structural layer is carried"
    );
    assert_eq!(state_layer_builds(), layers, "the issues are carried");
    assert!(
        !r.read()
            .tensions()
            .unwrap()
            .iter()
            .any(|t| t.blocks().contains(&refines)),
        "no tension names r (d1's deprecated reference is c2's)"
    );
    same(&r, &canon);
    same_issues(&r, &canon);
    assert!(
        suspect(&r),
        "a decision that refines a superseded one is suspect"
    );
}

/// A hub: doc `hub` with a stable section holding term Hub, decision dh (affects Hub, live once
/// landed) and `n` claims that describe Hub (each governed through it). Landed. Returns (term,
/// decision, section).
fn hub_world(r: &mut Rdy, n: usize) -> (String, String, String) {
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$o", "fields": {"name": "hub", "title": "Hub", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Rules", "lifecycle": "stable"}}),
        json!({"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Hub"}, "body": "The centre."}),
        json!({"op": "create", "type": "decision", "id": "$d", "parent": "$s",
            "fields": {"statement": "The hub is audited", "status": "proposed"},
            "links": {"affects": [{"target": "$t"}]}}),
    ];
    ops.extend((0..n).map(|k| {
        json!({"op": "create", "type": "claim", "parent": "$s", "body": format!("Rule {k} holds."),
            "links": {"describes": [{"target": "$t"}]}})
    }));
    let w = write(r, "seed", Value::Array(ops)).unwrap().created;
    land(r, "seed").unwrap();
    (w["$t"].clone(), w["$d"].clone(), w["$s"].clone())
}

/// Final review I1: a body or field edit of a block changes no entry of the blocks that point
/// at it, so the carry recomputes O(1) entries: a body edit of a term that 1,000 claims
/// describe, and a statement edit of a proposed decision that affects it (its status and its
/// edges unchanged). The landing that accepts that decision (a status change in canon) still
/// reaches every describer. Each carried layer equals a rebuild.
#[test]
fn cc11_a_body_edit_of_a_hub_recomputes_o1_entries() {
    let (_d, mut r) = project();
    let (t, _d1, s) = hub_world(&mut r, 1000);
    let v = Scope::View("next".into());
    let d2 = write(
        &mut r,
        "next",
        json!([{"op": "create", "type": "decision", "id": "$d2", "parent": s,
            "fields": {"statement": "The hub is backed up", "status": "proposed"},
            "links": {"affects": [{"target": t}]}}]),
    )
    .unwrap()
    .created["$d2"]
        .clone();
    let recomputed = |r: &mut Rdy, scope: &Scope, ops: Value, landing: bool| -> u64 {
        r.read().coverage_issues(scope).unwrap();
        let (builds, before) = (structural_builds(), carried_entries());
        write(r, "next", ops).unwrap();
        if landing {
            land(r, "next").unwrap();
        }
        r.read().coverage_issues(scope).unwrap();
        assert_eq!(structural_builds(), builds, "carried, not rebuilt");
        same(r, scope);
        carried_entries() - before
    };
    let n = recomputed(
        &mut r,
        &v,
        json!([{"op": "update", "id": t, "fields": {"name": "Hub"}, "body": "The centre of it all."}]),
        false,
    );
    assert!(n <= 4, "a term body edit recomputed {n} entries");
    let n = recomputed(
        &mut r,
        &v,
        json!([{"op": "update", "id": d2, "fields": {"statement": "The hub is backed up daily", "status": "proposed"},
            "links": {"affects": [{"target": t}]}}]),
        false,
    );
    assert!(n <= 4, "a decision statement edit recomputed {n} entries");
    let n = recomputed(
        &mut r,
        &Scope::Canon,
        json!([{"op": "create", "type": "prose", "parent": s, "body": "Landed with the rest."}]),
        true,
    );
    assert!(
        n > 1000,
        "the landing accepts d2, which reaches every describer: {n}"
    );
}

/// Final review I1: whether an entity has a live incoming `affects` is read once per build (or
/// carry), not once per block that describes it: a cold build of a hub that 1,000 claims
/// describe scans the hub's incoming edges for governance once.
#[test]
fn cc11_a_cold_build_reads_a_hubs_governance_once() {
    let (_d, mut r) = project();
    hub_world(&mut r, 1000);
    let st = r.read();
    let before = governed_scans();
    rebuilt_for_tests(&st, &Scope::Canon).unwrap();
    assert_eq!(governed_scans() - before, 1);
}

/// Final review M8 (the carry's "deleted with children left" branch): in flight, view `a` puts
/// a block under canon's empty section and view `b` deletes that section (empty in `b`). The
/// in-flight state holds the delete with a child left, so the carry gives up (the perf hook
/// gives no layer) and the next read rebuilds, equal to a rebuild.
#[test]
fn cc11_a_delete_with_children_left_in_flight_gives_up() {
    let (_d, mut r) = project();
    let c = cc6_world(&mut r);
    let inflight = Scope::Inflight;
    write(
        &mut r,
        "a",
        json!([{"op": "create", "type": "prose", "parent": c["$empty"], "body": "Now it has one."}]),
    )
    .unwrap();
    let prev = r.read();
    prev.coverage_issues(&inflight).unwrap();
    let w = write(&mut r, "b", json!([{"op": "delete", "id": c["$empty"]}])).unwrap();
    let next = r.read();
    let m = next.materialized(&inflight).unwrap();
    let empty = BlockId::parse(&c["$empty"]).unwrap();
    assert!(
        m.get(&empty).is_none(),
        "guard: in flight the section is deleted"
    );
    assert!(
        !m.children(Some(&empty)).is_empty(),
        "guard: its child from view a is left"
    );
    let change: Hash = w.change.parse().unwrap();
    let ops = next.reader().change(&change).unwrap().ops;
    assert!(
        carry_for_perf(&prev, &next, &inflight, &ops)
            .unwrap()
            .is_none(),
        "the carry gives up"
    );
    next.coverage_issues(&inflight).unwrap();
    same(&r, &inflight);
}

/// perf4: a scope's rollup read after a write takes the old entries of the blocks the carry
/// rebuilt out and their new ones in: it adds up no scope in full, and it equals the rollup of
/// a full build. The carry reads the suspects of no graph edge by edge. Through a plain write, a write that opens a duplicate (tension issues), and
/// one that resolves it.
#[test]
fn perf4_a_carried_rollup_adds_up_only_the_rebuilt_entries() {
    use rdy_engine::coverage::rollup_walks;
    let (_d, mut r) = project();
    let c = cc6_world(&mut r);
    let v = Scope::View("next".into());
    write(
        &mut r,
        "next",
        json!([{"op": "create", "type": "prose", "parent": c["$s"], "body": "One more line."}]),
    )
    .unwrap();
    r.read().scope_rollup(&v).unwrap();
    let steps = [
        json!([{"op": "create", "type": "prose", "parent": c["$s"], "body": "And one more."}]),
        json!([
            {"op": "create", "type": "claim", "id": "$a", "parent": c["$empty"], "body": "Fees are charged every month."},
            {"op": "create", "type": "claim", "parent": c["$empty"], "body": "Fees are charged every month."}
        ]),
    ];
    let mut dup = None;
    for (i, ops) in steps.into_iter().enumerate() {
        let w = write(&mut r, "next", ops).unwrap();
        if i == 1 {
            dup = Some(w.created["$a"].clone());
        }
        check_rollup(&r, &v, rollup_walks());
    }
    write(
        &mut r,
        "next",
        json!([{"op": "update", "id": dup.unwrap(), "body": "Fees are charged once a year."}]),
    )
    .unwrap();
    check_rollup(&r, &v, rollup_walks());
    same_issues(&r, &v);
}

/// The rollup of `scope` is read without adding up the scope (`walks` unchanged) or reading the
/// suspects edge by edge, and equals the rollup of a full build.
fn check_rollup(r: &Rdy, scope: &Scope, walks: u64) {
    let st = r.read();
    let suspects = rdy_engine::coverage::suspect_walks();
    let got = st.scope_rollup(scope).unwrap();
    assert_eq!(
        rdy_engine::coverage::rollup_walks(),
        walks,
        "carried, not added up"
    );
    assert_eq!(
        rdy_engine::coverage::suspect_walks(),
        suspects,
        "the suspects carried, not read edge by edge"
    );
    let full = issues_rebuilt_for_tests(&st, scope).unwrap();
    assert_eq!(
        *got,
        rdy_engine::coverage::rollup(st.pack(), full.blocks.values())
    );
}
