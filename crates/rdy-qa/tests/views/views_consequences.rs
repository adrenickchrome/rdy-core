//! vcq1: consequences of view operations (FW-E2, FW-G7a, FW-LP2; amendment §4.5 item 2):
//! include, exclude and undo list what they changed in the view, with `conflict` entries to
//! resolve; a resolution write lists its own; a block the view already wrote is marked.
use rdy_engine::consequence::{CONFLICT_FIX, ConsequenceKind, consequence_id, write_and_report};
use rdy_engine::engine::{Engine, Local};
use rdy_engine::view_api::{ViewRequest, run_view};
use rdy_engine::*;
use rdy_model::{BlockId, Hash};
use rdy_qa::edit_loop::*;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

fn req(v: Value) -> ViewRequest {
    serde_json::from_value(v).unwrap()
}
fn upd(id: &BlockId, body: &str) -> Value {
    json!([{"op": "update", "id": id.to_string(), "body": body}])
}
fn entries(v: &Value) -> &Vec<Value> {
    v["consequences"]
        .as_array()
        .unwrap_or_else(|| panic!("no consequences: {v}"))
}
fn find<'a>(v: &'a Value, block: &BlockId, kind: &str) -> &'a Value {
    entries(v)
        .iter()
        .find(|c| c["block"] == block.to_string() && c["kind"] == kind)
        .unwrap_or_else(|| panic!("no {kind} entry for {block}: {v}"))
}
fn hash(v: &Value) -> Hash {
    v.as_str().unwrap().parse().unwrap()
}
/// A write as `s` in `view` through the engine, with its consequences.
fn write_in(e: &Local, s: &Session, view: &str, ops: Value) -> Value {
    let (s, v) = (s.clone(), view.to_string());
    write_and_report(e, view, None, move |r| write_as(r, &s, &v, ops)).unwrap()
}

/// The spec in canon; agent b's change to A in view `vb`; the engine.
fn setup() -> (tempfile::TempDir, Local, Spec, String) {
    let (d, mut r) = project();
    let s = spec_doc(&mut r);
    let cb = write_as(
        &mut r,
        &as_agent("agent:b", "r1"),
        "vb",
        upd(&s.a, "A, by b."),
    )
    .unwrap()
    .change;
    (d, Local::new(r), s, cb)
}

#[test]
fn vcq1_including_a_change_lists_the_referrer_of_the_block_it_moves() {
    let (_d, e, s, cb) = setup();
    let a = as_agent("agent:a", "r1");
    let v = run_view(
        &e,
        &a,
        "human:qa",
        "r1",
        req(json!({"op": "include", "changes": [cb]})),
    )
    .unwrap();
    let b = find(&v, &s.b, "suspect");
    let a_id = s.a.to_string();
    // D-CFC6V: B states no literal the include removed
    assert_eq!(b["follow_up"], "confirm");
    assert_eq!(
        b["because"],
        json!({"edge": "references", "changed": a_id, "edges": ["references"], "values": []})
    );
    assert_eq!(
        b["id"],
        consequence_id(&hash(&v["head"]), "suspect", &s.b),
        "the batch is the include op"
    );
    assert!(
        b.get("conflict").is_none() && b.get("already_written").is_none(),
        "{b}"
    );
}

#[test]
fn vcq1_excluding_it_again_lists_the_referrer_as_a_dependent() {
    let (_d, e, s, cb) = setup();
    let a = as_agent("agent:a", "r1");
    run_view(
        &e,
        &a,
        "human:qa",
        "r1",
        req(json!({"op": "include", "changes": [cb]})),
    )
    .unwrap();
    let v = run_view(
        &e,
        &a,
        "human:qa",
        "r1",
        req(json!({"op": "exclude", "changes": [cb]})),
    )
    .unwrap();
    let b = find(&v, &s.b, "dependent");
    assert_eq!(b["follow_up"], "confirm");
    assert_eq!(
        b["id"],
        consequence_id(&hash(&v["head"]), "dependent", &s.b)
    );
}

#[test]
fn vcq1_an_include_that_collides_lists_a_conflict_to_resolve() {
    let (_d, e, s, cb) = setup();
    let a = as_agent("agent:a", "r1");
    write_in(&e, &a, "r1", upd(&s.a, "A, by a."));
    let v = run_view(
        &e,
        &a,
        "human:qa",
        "r1",
        req(json!({"op": "include", "changes": [cb]})),
    )
    .unwrap();
    let c = find(&v, &s.a, "conflict");
    assert_eq!(c["follow_up"], "resolve");
    assert_eq!(c["fix"], CONFLICT_FIX);
    assert_eq!(c["conflict"], v["conflicts"][0]["id"]);
    assert_eq!(
        c["because"],
        json!({"edge": "conflict", "changed": s.a.to_string()})
    );
    assert_eq!(c["id"], consequence_id(&hash(&v["head"]), "conflict", &s.a));
    assert_eq!(
        entries(&v)[0]["kind"],
        "conflict",
        "conflict entries come first"
    );
}

#[test]
fn vcq1_undoing_an_include_reports_with_the_undo_op_as_the_batch() {
    let (_d, e, s, cb) = setup();
    let a = as_agent("agent:a", "r1");
    run_view(
        &e,
        &a,
        "human:qa",
        "r1",
        req(json!({"op": "include", "changes": [cb]})),
    )
    .unwrap();
    let v = run_view(&e, &a, "human:qa", "r1", req(json!({"op": "undo"}))).unwrap();
    assert_eq!(v["kind"], "include", "{v}");
    let b = find(&v, &s.b, "dependent");
    assert_eq!(b["id"], consequence_id(&hash(&v["op"]), "dependent", &s.b));
}

#[test]
fn vcq1_a_resolution_write_lists_its_consequences_and_no_conflict_entry() {
    let (_d, e, s, cb) = setup();
    let a = as_agent("agent:a", "r1");
    let canon_a = e
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .version_of(&s.a)
        .unwrap()
        .to_string();
    write_in(&e, &a, "r1", upd(&s.a, "A, by a."));
    run_view(
        &e,
        &a,
        "human:qa",
        "r1",
        req(json!({"op": "include", "changes": [cb]})),
    )
    .unwrap();
    let v = write_in(
        &e,
        &a,
        "r1",
        json!([{"op": "update", "id": s.a.to_string(), "base": canon_a, "body": "A, merged."}]),
    );
    assert!(
        e.read()
            .conflicts(&Scope::View("r1".into()))
            .unwrap()
            .is_empty(),
        "resolved"
    );
    find(&v, &s.b, "suspect");
    assert!(entries(&v).iter().all(|c| c["kind"] != "conflict"), "{v}");
}

#[test]
fn vcq1_a_dependent_the_view_already_wrote_is_marked_already_written() {
    let (_d, e, s, _cb) = setup();
    let a = as_agent("agent:a", "r1");
    let first = write_in(&e, &a, "r1", upd(&s.a, "A, by a."));
    assert!(
        find(&first, &s.b, "suspect")
            .get("already_written")
            .is_none(),
        "{first}"
    );
    write_in(
        &e,
        &a,
        "r1",
        upd(&s.b, &format!("B builds on {{{{{}}}}}, by a.", s.a)),
    );
    let again = write_in(&e, &a, "r1", upd(&s.a, "A, by a, again."));
    assert_eq!(
        find(&again, &s.b, "suspect")["already_written"],
        true,
        "{again}"
    );
}

#[test]
fn vcq1_an_include_with_nothing_new_has_empty_consequences_and_reads_have_none() {
    let (_d, e, _s, cb) = setup();
    let a = as_agent("agent:a", "r1");
    run_view(
        &e,
        &a,
        "human:qa",
        "r1",
        req(json!({"op": "include", "changes": [cb]})),
    )
    .unwrap();
    let again = run_view(
        &e,
        &a,
        "human:qa",
        "r1",
        req(json!({"op": "include", "changes": [cb]})),
    )
    .unwrap();
    assert_eq!(again["consequences"], json!([]));
    let show = run_view(&e, &a, "human:qa", "r1", req(json!({"op": "show"}))).unwrap();
    assert!(show.get("consequences").is_none());
    let fork = run_view(
        &e,
        &a,
        "human:qa",
        "r1",
        req(json!({"op": "fork", "from": "r1", "name": "r1-copy"})),
    )
    .unwrap();
    assert!(fork.get("consequences").is_none());
    assert_eq!(
        serde_json::to_value(ConsequenceKind::Conflict).unwrap(),
        "conflict"
    );
    assert_eq!(
        serde_json::to_value(ConsequenceKind::Tension).unwrap(),
        "tension"
    );
}

#[test]
fn vcq1_max_tokens_cuts_view_op_consequences_with_counts() {
    let (_d, e, _s, cb) = setup();
    let a = as_agent("agent:a", "r1");
    let v = run_view(
        &e,
        &a,
        "human:qa",
        "r1",
        req(json!({"op": "include", "changes": [cb], "max_tokens": 1})),
    )
    .unwrap();
    assert_eq!(v["consequences"], json!([]));
    assert_eq!(v["consequences_omitted"], json!({"suspect": 1}), "{v}");
}

#[test]
fn vcq1_the_full_diff_equals_the_change_driven_one() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let cb = write(&mut r, "vb", upd(&s.a, "A, by b.")).unwrap().change;
    let before = r.read();
    let res = r
        .view_include(&writer(), "r1", std::slice::from_ref(&cb))
        .unwrap();
    let after = r.read();
    let head: Hash = res.head.parse().unwrap();
    let ch: Vec<Hash> = vec![cb.parse().unwrap()];
    let scoped = rdy_engine::tensions::tension_delta(&before, &after, Some(&ch)).unwrap();
    let whole = rdy_engine::tensions::tension_delta(&before, &after, None).unwrap();
    let by_changes = after
        .view_consequences(&before, "r1", &head, Some(&ch), &scoped, None)
        .unwrap();
    let full = after
        .view_consequences(&before, "r1", &head, None, &whole, None)
        .unwrap();
    assert_eq!(by_changes, full);
    assert!(!full.items.is_empty());
}
