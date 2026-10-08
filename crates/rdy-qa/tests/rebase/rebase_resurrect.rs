//! A canon delete is not silently undone: a view that wrote a block before canon deleted it
//! lands only when one of its changes depends on the delete. Otherwise the propose or the land
//! refuses with the conflict, whatever number of changes the view made to the block after.
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

/// A glossary doc with sections S1 and S2 and one term T in S1, landed. Returns the project,
/// T, S1, S2 and the doc.
fn seeded() -> (tempfile::TempDir, Rdy, String, String, String, String) {
    let (d, mut r) = project();
    let res = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "g", "title": "G", "doc_type": "glossary"}},
            {"op": "create", "type": "section", "id": "$s1", "parent": "$d", "fields": {"title": "One"}},
            {"op": "create", "type": "section", "id": "$s2", "parent": "$d", "fields": {"title": "Two"}},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s1", "fields": {"name": "Tide"}, "body": "first"}
        ]),
    )
    .expect("seed");
    land(&mut r, "seed").expect("land seed");
    let t = res.created["$t"].clone();
    let s1 = res.created["$s1"].clone();
    let s2 = res.created["$s2"].clone();
    let doc = res.created["$d"].clone();
    (d, r, t, s1, s2, doc)
}

#[derive(Clone, Copy, Debug)]
enum Step {
    Edit,
    Move,
    Rename,
    Delete,
}

fn step_ops(step: Step, i: usize, t: &str, parent: &str) -> Value {
    match step {
        Step::Edit => json!([{"op": "update", "id": t, "body": format!("edit {i}")}]),
        Step::Rename => json!([{"op": "update", "id": t, "fields": {"name": format!("Tide {i}")}}]),
        Step::Move => json!([{"op": "move", "id": t, "parent": parent}]),
        Step::Delete => json!([{"op": "delete", "id": t}]),
    }
}

/// View "late" writes `steps` to T, one change each; then view "del" deletes T and lands. Returns
/// the outcome of proposing and landing "late": `Err(code)` on a refusal, else whether T is in
/// canon after the landing.
fn late_writes_after_canon_delete(steps: &[Step]) -> Result<bool, String> {
    let (_d, mut r, t, s1, s2, _doc) = seeded();
    let late = as_agent("agent:late", "r2");
    let mut parent = s1.clone();
    for (i, step) in steps.iter().enumerate() {
        if let Step::Move = step {
            parent = if parent == s1 { s2.clone() } else { s1.clone() };
        }
        write_as(&mut r, &late, "late", step_ops(*step, i, &t, &parent)).expect("late write");
    }
    write_as(
        &mut r,
        &as_agent("agent:del", "r1"),
        "del",
        step_ops(Step::Delete, 0, &t, &s1),
    )
    .expect("delete");
    land(&mut r, "del").expect("land the delete");
    let tid = BlockId::parse(&t).unwrap();
    assert!(
        r.read().block(&Scope::Canon, &tid).is_err(),
        "T deleted in canon"
    );
    land(&mut r, "late").map_err(|e| e.code.to_string())?;
    Ok(r.read().block(&Scope::Canon, &tid).is_ok())
}

fn assert_refused(steps: &[Step]) {
    let out = late_writes_after_canon_delete(steps);
    assert_eq!(
        out,
        Err(E_CONFLICT_UNRESOLVED.to_string()),
        "{steps:?}: a view that never saw the delete must not undo it"
    );
}

#[test]
fn rres1_edit_then_edit_after_a_canon_delete_refuses() {
    assert_refused(&[Step::Edit, Step::Edit]);
}

#[test]
fn rres1_move_then_edit_after_a_canon_delete_refuses() {
    assert_refused(&[Step::Move, Step::Edit]);
}

#[test]
fn rres1_a_single_move_after_a_canon_delete_still_refuses() {
    assert_refused(&[Step::Move]);
}

#[test]
fn rres1_a_single_doc_rename_after_a_canon_delete_still_refuses() {
    let (_d, mut r, t, s1, s2, doc) = seeded();
    write_as(
        &mut r,
        &as_agent("agent:late", "r2"),
        "late",
        json!([{"op": "update", "id": doc, "fields": {"title": "New name"}}]),
    )
    .expect("rename");
    write_as(
        &mut r,
        &as_agent("agent:del", "r1"),
        "del",
        json!([
            {"op": "delete", "id": t},
            {"op": "delete", "id": s1},
            {"op": "delete", "id": s2},
            {"op": "delete", "id": doc}
        ]),
    )
    .expect("delete the doc");
    land(&mut r, "del").expect("land the delete");
    let e = land(&mut r, "late").unwrap_err();
    assert_eq!(e.code, E_CONFLICT_UNRESOLVED, "{e:?}");
    let did = BlockId::parse(&doc).unwrap();
    assert!(r.read().block(&Scope::Canon, &did).is_err());
}

/// Every sequence of one to three changes (edit, move, rename; a delete only last) written to T
/// before canon deleted it: the landing refuses, or T stays deleted.
#[test]
fn rres1_table_no_sequence_of_late_writes_brings_a_canon_delete_back() {
    use Step::*;
    let live = [Edit, Move, Rename];
    let mut seqs: Vec<Vec<Step>> = vec![];
    for n in 1..=3usize {
        let mut prefixes: Vec<Vec<Step>> = vec![vec![]];
        for _ in 1..n {
            prefixes = prefixes
                .iter()
                .flat_map(|p| {
                    live.iter().map(move |s| {
                        let mut q = p.clone();
                        q.push(*s);
                        q
                    })
                })
                .collect();
        }
        for p in prefixes {
            for last in [Edit, Move, Rename, Delete] {
                let mut q = p.clone();
                q.push(last);
                seqs.push(q);
            }
        }
    }
    assert_eq!(seqs.len(), 4 + 12 + 36);
    for steps in seqs {
        match late_writes_after_canon_delete(&steps) {
            Err(code) => assert_eq!(code, E_CONFLICT_UNRESOLVED, "{steps:?}"),
            Ok(back) => assert!(!back, "{steps:?}: T came back to canon"),
        }
    }
}

/// The view sees the delete (the landing rebases it and records the conflict), then writes T
/// from the conflict base: that write depends on the delete, so it re-creates T deliberately.
#[test]
fn rres1_a_write_that_depends_on_the_delete_re_creates_the_block() {
    let (_d, mut r, t, _s1, s2, _doc) = seeded();
    let late = as_agent("agent:late", "r2");
    write_as(&mut r, &late, "late", step_ops(Step::Move, 0, &t, &s2)).unwrap();
    write_as(&mut r, &late, "late", step_ops(Step::Edit, 1, &t, &s2)).unwrap();
    write_as(
        &mut r,
        &as_agent("agent:del", "r1"),
        "del",
        step_ops(Step::Delete, 0, &t, &s2),
    )
    .unwrap();
    land(&mut r, "del").unwrap();
    let tid = BlockId::parse(&t).unwrap();
    let scope = Scope::View("late".into());
    let conflict = r
        .read()
        .conflict_on(&scope, &tid)
        .unwrap()
        .expect("the rebase records the conflict with canon's delete");
    let deleted = conflict
        .sides
        .iter()
        .find(|s| s.version.is_none())
        .expect("canon's side is the delete");
    assert!(
        deleted.change.is_some(),
        "canon's delete side names the change that deleted T: {conflict:?}"
    );
    let base = conflict.base.unwrap().to_string();
    write_as(
        &mut r,
        &late,
        "late",
        json!([{"op": "update", "id": t, "base": base, "body": "kept on purpose"}]),
    )
    .unwrap();
    assert!(r.read().conflicts(&scope).unwrap().is_empty());
    land(&mut r, "late").unwrap();
    assert_eq!(body_of(&r, &Scope::Canon, &t), "kept on purpose");
    r.fsck().unwrap();
}
