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

/// Triage #297: the view deletes T and re-creates it under the same id before canon's delete of
/// T lands. Its own delete agrees with canon's, but the re-create does not know about canon's
/// delete, so landing it would bring T back: it refuses.
#[test]
fn rres2_a_delete_and_re_create_written_before_a_canon_delete_refuses() {
    let (_d, mut r, t, s1, _s2, _doc) = seeded();
    let late = as_agent("agent:late", "r2");
    write_as(&mut r, &late, "late", step_ops(Step::Delete, 0, &t, &s1)).unwrap();
    write_as(&mut r, &late, "late", re_create(&t, &s1, "again")).unwrap();
    write_as(
        &mut r,
        &as_agent("agent:del", "r1"),
        "del",
        step_ops(Step::Delete, 0, &t, &s1),
    )
    .unwrap();
    land(&mut r, "del").unwrap();
    let e = land(&mut r, "late").unwrap_err();
    assert_eq!(e.code, E_CONFLICT_UNRESOLVED, "{e:?}");
    let tid = BlockId::parse(&t).unwrap();
    assert!(
        r.read().block(&Scope::Canon, &tid).is_err(),
        "T stays deleted"
    );
}

/// A re-create written after canon's delete landed knows about it: it lands.
#[test]
fn rres2_a_re_create_written_after_the_canon_delete_lands() {
    let (_d, mut r, t, s1, _s2, _doc) = seeded();
    write_as(
        &mut r,
        &as_agent("agent:del", "r1"),
        "del",
        step_ops(Step::Delete, 0, &t, &s1),
    )
    .unwrap();
    land(&mut r, "del").unwrap();
    // the deleting view itself, and a fresh view, may each bring T back on purpose
    for v in ["del", "fresh"] {
        write_as(
            &mut r,
            &as_agent(&format!("agent:{v}"), "r1"),
            v,
            re_create(&t, &s1, v),
        )
        .unwrap();
        assert!(
            r.read()
                .conflicts(&Scope::View(v.into()))
                .unwrap()
                .is_empty()
        );
    }
    land(&mut r, "fresh").unwrap();
    assert_eq!(body_of(&r, &Scope::Canon, &t), "fresh");
}

fn re_create(t: &str, parent: &str, body: &str) -> Value {
    json!([{"op": "create", "type": "term", "id": t, "parent": parent, "fields": {"name": "Tide"}, "body": body}])
}

fn conflicts_of(r: &Rdy, v: &str) -> Vec<rdy_store::Conflict> {
    r.read()
        .conflicts(&Scope::View(v.into()))
        .unwrap()
        .as_ref()
        .clone()
}

/// The view's conflicts as published in process, and as a fresh build computes them: the two
/// must agree (a daemon or MCP server never reopens to see the truth).
fn published_and_fresh(r: &mut Rdy, v: &str) -> Vec<rdy_store::Conflict> {
    let published = conflicts_of(r, v);
    r.invalidate();
    assert_eq!(
        published,
        conflicts_of(r, v),
        "published build of {v} is stale"
    );
    published
}

/// Review round 1, finding 1: a view that landed earlier and is empty again floats on canon; it
/// re-creates an id canon deleted after the view's stored base. It read canon, so it saw the
/// delete: the re-create depends on it and lands, with no conflict in process or after a rebuild.
#[test]
fn rres3_a_reused_floating_view_re_creates_an_id_canon_deleted_after_its_base() {
    let (_d, mut r, t, s1, s2, _doc) = seeded();
    let a = as_agent("agent:a", "r1");
    write_as(
        &mut r,
        &a,
        "a",
        json!([{"op": "update", "id": s2, "fields": {"title": "Two!"}}]),
    )
    .unwrap();
    land(&mut r, "a").unwrap();
    write_as(
        &mut r,
        &as_agent("agent:b", "r2"),
        "b",
        step_ops(Step::Delete, 0, &t, &s1),
    )
    .unwrap();
    land(&mut r, "b").unwrap();
    write_as(&mut r, &a, "a", re_create(&t, &s1, "back")).unwrap();
    assert!(published_and_fresh(&mut r, "a").is_empty());
    land(&mut r, "a").unwrap();
    assert_eq!(body_of(&r, &Scope::Canon, &t), "back");
}

/// Finding 1, the in-process path: the re-create written before canon's delete refuses; the
/// published conflict is the real one, and writing T from it resolves it with no reopen.
#[test]
fn rres3_a_refused_re_create_resolves_in_process_without_a_reopen() {
    let (_d, mut r, t, s1, _s2, _doc) = seeded();
    let late = as_agent("agent:late", "r2");
    write_as(&mut r, &late, "late", step_ops(Step::Delete, 0, &t, &s1)).unwrap();
    write_as(&mut r, &late, "late", re_create(&t, &s1, "again")).unwrap();
    write_as(
        &mut r,
        &as_agent("agent:del", "r1"),
        "del",
        step_ops(Step::Delete, 0, &t, &s1),
    )
    .unwrap();
    land(&mut r, "del").unwrap();
    let e = land(&mut r, "late").unwrap_err();
    assert_eq!(e.code, E_CONFLICT_UNRESOLVED, "{e:?}");
    let c = published_and_fresh(&mut r, "late");
    assert_eq!(c.len(), 1, "{c:?}");
    let side = c[0].sides.iter().find_map(|s| s.version).unwrap();
    write_as(
        &mut r,
        &late,
        "late",
        json!([{"op": "update", "id": t, "base": side.to_string(), "body": "kept on purpose"}]),
    )
    .unwrap();
    assert!(published_and_fresh(&mut r, "late").is_empty());
    land(&mut r, "late").unwrap();
    assert_eq!(body_of(&r, &Scope::Canon, &t), "kept on purpose");
}

/// Finding 3: two views delete T; v1's delete lands, v3 re-creates T after seeing it, then v2's
/// delete lands though it changed nothing. v1's delete is the one that removed T, so v3 stays
/// clean and lands.
#[test]
fn rres3_a_no_op_delete_landing_later_is_not_the_deleter() {
    let (_d, mut r, t, s1, _s2, _doc) = seeded();
    write_as(
        &mut r,
        &as_agent("agent:v1", "r1"),
        "v1",
        step_ops(Step::Delete, 0, &t, &s1),
    )
    .unwrap();
    write_as(
        &mut r,
        &as_agent("agent:v2", "r2"),
        "v2",
        step_ops(Step::Delete, 0, &t, &s1),
    )
    .unwrap();
    land(&mut r, "v1").unwrap();
    write_as(
        &mut r,
        &as_agent("agent:v3", "r3"),
        "v3",
        re_create(&t, &s1, "back"),
    )
    .unwrap();
    assert!(published_and_fresh(&mut r, "v3").is_empty());
    land(&mut r, "v2").unwrap();
    assert!(published_and_fresh(&mut r, "v3").is_empty());
    land(&mut r, "v3").unwrap();
    assert_eq!(body_of(&r, &Scope::Canon, &t), "back");
}

/// Finding 2: a view pinned where T is deleted (by x) re-creates T, while canon re-created T and
/// deleted it again (by z). The view is clean on its own base (it saw x's delete), so it carries
/// no conflict it cannot resolve. Its landing refuses as a stale base: it never saw z's delete.
/// The hint is one the writer can follow: re-create T from a view that reads canon.
#[test]
fn rres3_a_pinned_view_re_create_is_clean_on_its_base_and_refuses_on_canon_with_a_hint_it_can_follow()
 {
    let (d, mut r, t, s1, _s2, _doc) = seeded();
    write_as(
        &mut r,
        &as_agent("agent:x", "r1"),
        "x",
        step_ops(Step::Delete, 0, &t, &s1),
    )
    .unwrap();
    land(&mut r, "x").unwrap();
    let p = as_agent("agent:p", "rp");
    r.view_create(&p, "pin", None, true).unwrap();
    write_as(
        &mut r,
        &as_agent("agent:y", "r2"),
        "y",
        re_create(&t, &s1, "y"),
    )
    .unwrap();
    land(&mut r, "y").unwrap();
    write_as(
        &mut r,
        &as_agent("agent:z", "r3"),
        "z",
        step_ops(Step::Delete, 0, &t, &s1),
    )
    .unwrap();
    land(&mut r, "z").unwrap();
    write_as(&mut r, &p, "pin", re_create(&t, &s1, "pinned")).unwrap();
    assert!(published_and_fresh(&mut r, "pin").is_empty());
    drop(r);
    let mut r = Rdy::open(d.path()).unwrap();
    assert!(
        conflicts_of(&r, "pin").is_empty(),
        "no conflict after a reopen either"
    );
    let e = land(&mut r, "pin").unwrap_err();
    assert_eq!(e.code, E_STALE_BASE, "{e:?}");
    assert!(e.fix.contains("reads canon"), "{e:?}");
    let tid = BlockId::parse(&t).unwrap();
    assert!(
        r.read().block(&Scope::Canon, &tid).is_err(),
        "T stays deleted"
    );
    // the hint, followed
    write_as(&mut r, &p, "fresh", re_create(&t, &s1, "pinned")).unwrap();
    land(&mut r, "fresh").unwrap();
    assert_eq!(body_of(&r, &Scope::Canon, &t), "pinned");
    r.fsck().unwrap();
}
