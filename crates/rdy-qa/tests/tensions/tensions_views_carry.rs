//! ibperf2: a view op carries the tension set across the changes it moves (plan M2e2: each
//! batch's moves follow its delta, never a full build per write).
use proptest::prelude::*;
use rdy_engine::tensions::set::full_builds;
use rdy_engine::{DecisionInput, Rdy};
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use serde_json::json;

/// The tension set of the published state equals a full build, writes included.
fn same_as_rebuilt(r: &Rdy) -> Result<(), String> {
    let st = r.read();
    let set = st.tensions().map_err(|e| e.to_string())?;
    let full = st.tensions_rebuilt().map_err(|e| e.to_string())?;
    let a: Vec<_> = set.iter().collect();
    let b: Vec<_> = full.iter().collect();
    if a != b {
        return Err(format!("carried {a:#?}\nrebuilt {b:#?}"));
    }
    if !set.writes().same_as(full.writes()) {
        return Err("the carried writes differ from a build's".into());
    }
    Ok(())
}

/// The changes view `v` includes, as the ids view ops take.
fn included(r: &Rdy, v: &str) -> Vec<String> {
    r.read().included(v).iter().map(|c| c.to_string()).collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(12), ..ProptestConfig::default() })]
    #[test]
    fn ibperf2_a_view_op_carries_the_same_set_as_a_rebuild(
        steps in proptest::collection::vec((0u8..12, 0u8..3, 0u8..3), 1..14)
    ) {
        let (_d, mut r) = project();
        let s = spec_doc(&mut r);
        let names = ["a", "b", "c"];
        let claims = [s.a.clone(), s.b.clone(), s.c.clone()];
        // texts that form a duplicate group (two normalize to one) and one that does not
        let texts = ["Fees are charged monthly.", "fees ARE charged  monthly.", "Fees are charged yearly."];
        for (i, (kind, v, k)) in steps.into_iter().enumerate() {
            let vn = names[v as usize];
            let other = names[(v as usize + 1) % 3];
            let target = &claims[k as usize];
            let _ok = match kind {
                0 => try_put(&mut r, vn, upd(target, &format!("{vn} {i}"))).is_ok(),
                1 => try_put(&mut r, vn, cite(&s.section, target)).is_ok(),
                2 => try_put(&mut r, vn, term(&s.section, ["Fee", "Charge", "fee"][k as usize])).is_ok(),
                3 => try_put(&mut r, vn, json!([{"op": "create", "type": "claim",
                        "parent": s.section.to_string(), "body": texts[k as usize]}])).is_ok(),
                // a change and its deps moved into another view
                4 => match included(&r, other).last() {
                    Some(c) => r.view_include(&ag(vn), vn, std::slice::from_ref(c)).is_ok(),
                    None => false,
                },
                // a bulk include of another view's every change
                5 => {
                    let all = included(&r, other);
                    !all.is_empty() && r.view_include(&ag(vn), vn, &all).is_ok()
                }
                // an exclude, its dependents with it: the change leaves flight unless another
                // view holds it
                6 => match included(&r, vn).last() {
                    Some(c) => r.view_exclude(&ag(vn), vn, std::slice::from_ref(c), true).is_ok(),
                    None => false,
                },
                7 => match included(&r, vn).first() {
                    Some(c) => r.view_exclude(&ag(vn), vn, std::slice::from_ref(c), true).is_ok(),
                    None => false,
                },
                8 => r.view_close(&ag(vn), &[vn.to_string()], "done").is_ok(),
                // a structural create, and a delete: steps the carry refuses when they move flight
                9 => {
                    let name = ["Fee", "Charge", "fee"][k as usize];
                    try_put(&mut r, vn, json!([
                        {"op": "create", "type": "section", "id": "$n", "parent": s.doc.to_string(), "fields": {"title": format!("New {i}")}},
                        {"op": "create", "type": "claim", "parent": "$n", "body": texts[k as usize]},
                        {"op": "create", "type": "term", "parent": "$n", "fields": {"name": name}, "body": "A term."}
                    ])).is_ok()
                }
                10 => try_put(&mut r, vn, json!([{"op": "delete", "id": target.to_string()}])).is_ok(),
                _ => land(&mut r, vn).is_ok(),
            };
            prop_assert_eq!(same_as_rebuilt(&r), Ok(()), "step {}", i);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(12), ..ProptestConfig::default() })]
    #[test]
    fn ibperf2_a_landing_carries_the_same_set_as_a_rebuild(
        steps in proptest::collection::vec((0u8..10, 0u8..3, 0u8..3), 1..14)
    ) {
        let (_d, mut r) = project();
        let s = spec_doc(&mut r);
        let names = ["a", "b", "c"];
        let claims = [s.a.clone(), s.b.clone(), s.c.clone()];
        let texts = ["Fees are charged monthly.", "fees ARE charged  monthly.", "Fees are charged yearly."];
        for (i, (kind, v, k)) in steps.into_iter().enumerate() {
            let vn = names[v as usize];
            let target = &claims[k as usize];
            let _ok = match kind {
                0 => try_put(&mut r, vn, upd(target, &format!("{vn} {i}"))).is_ok(),
                1 => try_put(&mut r, vn, cite(&s.section, target)).is_ok(),
                2 => try_put(&mut r, vn, term(&s.section, ["Fee", "Charge", "fee"][k as usize])).is_ok(),
                3 => r.supersede(&ag(vn), vn, &s.decision.to_string(), None, DecisionInput {
                        parent: s.section.to_string(), statement: format!("s{i}"), ..Default::default()
                    }).is_ok(),
                4 => try_put(&mut r, vn, json!([{"op": "create", "type": "claim",
                        "parent": s.section.to_string(), "body": texts[k as usize]}])).is_ok(),
                5 => try_put(&mut r, vn, json!([{"op": "repin", "id": target.to_string()}])).is_ok(),
                9 => try_put(&mut r, vn, json!([{"op": "delete", "id": target.to_string()}])).is_ok(),
                _ => land(&mut r, vn).is_ok(),
            };
            prop_assert_eq!(same_as_rebuilt(&r), Ok(()), "step {}", i);
        }
    }
}

#[test]
fn ibperf2_an_include_an_exclude_and_a_close_carry_the_set() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    put(&mut r, "c", cite(&s.section, &s.b));
    r.read().tensions().unwrap();
    let a = included(&r, "a");
    let c = included(&r, "c");
    // into a view that does not exist yet: its create and the include are two batches
    let builds = full_builds();
    r.view_include(&ag("d"), "d", &a).unwrap();
    assert_eq!(
        r.read().tensions_carried(),
        Some(true),
        "the include carried"
    );
    r.view_include(&ag("d"), "d", &c).unwrap();
    r.view_exclude(&ag("d"), "d", &a, false).unwrap();
    assert_eq!(
        r.read().tensions_carried(),
        Some(true),
        "the exclude carried"
    );
    r.view_close(&ag("c"), &["c".to_string()], "done").unwrap();
    assert_eq!(r.read().tensions_carried(), Some(true), "the close carried");
    assert_eq!(full_builds() - builds, 0, "no view op built the whole set");
    same_as_rebuilt(&r).unwrap();
}

#[test]
fn ibperf2_the_first_write_after_init_builds_no_whole_set() {
    let (_d, mut r) = project();
    let builds = full_builds();
    // a commit carries from the empty project's set: init reconciled it, so the write's moves
    // need no full build of the state before
    put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "doc", "id": "$d",
            "fields": {"name": "x", "title": "X", "doc_type": "overview"}}]),
    );
    assert_eq!(
        full_builds() - builds,
        0,
        "the first write built a whole set"
    );
    assert_eq!(r.reconcile_transitions().unwrap(), 0, "init reconciled");
    assert_eq!(r.fsck().unwrap().unlogged_moves, 0);
}

#[test]
fn ibperf2_invalidate_on_the_logged_state_builds_nothing() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    let builds = full_builds();
    r.invalidate();
    assert_eq!(full_builds() - builds, 0, "invalidate built the set");
    assert!(!r.read().tensions_built(), "every cell starts empty");
    assert_eq!(r.fsck().unwrap().unlogged_moves, 0);
}

#[test]
fn ibperf2_a_commit_that_writes_more_than_was_in_flight_builds_in_full() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    r.read().tensions().unwrap();
    // more claims than the floor in one change, against one block in flight: re-detecting
    // around every seed reads more than one full build does
    let n = rdy_engine::tensions::set::CARRY_SEEDS_FLOOR + 1;
    let mut ops: Vec<_> = (0..n)
        .map(|i| json!({"op": "create", "type": "claim", "parent": s.section.to_string(), "body": format!("Fee {i}.")}))
        .collect();
    ops.push(json!({"op": "create", "type": "term", "parent": s.section.to_string(), "fields": {"name": "Fee"}, "body": "A fee."}));
    ops.push(json!({"op": "create", "type": "term", "parent": s.section.to_string(), "fields": {"name": "fee"}, "body": "A fee again."}));
    put(&mut r, "b", json!(ops));
    assert_eq!(r.read().tensions_carried(), Some(false), "built in full");
    same_as_rebuilt(&r).unwrap();
    // a small commit after it carries again
    put(&mut r, "c", upd(&s.b, "B by c."));
    assert_eq!(r.read().tensions_carried(), Some(true), "carried");
    same_as_rebuilt(&r).unwrap();
}

#[test]
fn ibperf2_a_landing_carries_the_set() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    put(&mut r, "c", cite(&s.section, &s.b));
    put(&mut r, "d", upd(&s.b, "B by d."));
    r.read().tensions().unwrap();
    let builds = full_builds();
    // the landing moves canon under b's write (a collision against canon) and c's citation
    // (a premise shift)
    land(&mut r, "a").unwrap();
    assert_eq!(
        r.read().tensions_carried(),
        Some(true),
        "the landing carried"
    );
    same_as_rebuilt(&r).unwrap();
    land(&mut r, "d").unwrap();
    assert_eq!(
        r.read().tensions_carried(),
        Some(true),
        "the landing carried"
    );
    same_as_rebuilt(&r).unwrap();
    assert_eq!(full_builds() - builds, 0, "no landing built the whole set");
}

#[test]
fn ibperf2_a_carry_unfiles_only_the_keys_of_the_blocks_it_writes() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    // a flight big enough that a 200-claim commit carries
    let many: Vec<_> = (0..300)
        .map(|i| json!({"op": "create", "type": "claim", "parent": s.section.to_string(), "body": format!("Standing claim number {i} holds here.")}))
        .collect();
    put(&mut r, "a", json!(many));
    r.read().tensions().unwrap();
    let n = 200;
    let ops: Vec<_> = (0..n)
        .map(|i| json!({"op": "create", "type": "claim", "parent": s.section.to_string(), "body": format!("Batch claim number {i} holds here.")}))
        .collect();
    let before = rdy_engine::tensions::index::unfiled_keys();
    put(&mut r, "b", json!(ops));
    assert_eq!(
        r.read().tensions_carried(),
        Some(true),
        "the commit carried"
    );
    let walked = rdy_engine::tensions::index::unfiled_keys() - before;
    // each block has at most a few keys (name, text, targets, edges): never every key before it
    assert!(
        walked <= 8 * n as u64,
        "{walked} keys walked for {n} blocks"
    );
    same_as_rebuilt(&r).unwrap();
}

#[test]
fn ibperf2_a_landing_carries_the_canon_part() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    // canon duplicates the landing joins, leaves and keeps
    put(&mut r, "a", term(&s.section, "Fee"));
    land(&mut r, "a").unwrap();
    put(&mut r, "b", term(&s.section, "fee"));
    put(&mut r, "c", upd(&s.a, "Fees are charged monthly."));
    put(&mut r, "d", upd(&s.b, "fees ARE charged  monthly."));
    r.read().tensions().unwrap();
    for v in ["b", "c", "d"] {
        let runs = rdy_engine::tensions::set::canon_part_runs();
        land(&mut r, v).unwrap();
        assert_eq!(
            rdy_engine::tensions::set::canon_part_runs() - runs,
            0,
            "landing {v} ran the canon part in full"
        );
        same_as_rebuilt(&r).unwrap();
    }
    assert!(
        !of_kind(&r, rdy_engine::tensions::TensionKind::DuplicateDefinition).is_empty(),
        "the canon duplicates are in the set"
    );
}
