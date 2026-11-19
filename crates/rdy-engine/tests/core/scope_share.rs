//! A scope off canon (an old landing, a proposal version, a view) shares canon's blocks and
//! carries canon's graph and lexicon across the blocks that differ, so it holds no private copy
//! of the whole project. Each such scope must equal a build of it from the store alone: the same
//! blocks, the same graph and the same lexicon.
use rdy_engine::testkit::rebuilt_scope_for_tests;
use rdy_engine::*;
use rdy_model::*;
use serde_json::{Value, json};
use std::sync::Arc;

/// The summary a version is proposed with.
const SUMMARY: &str = "test summary";

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "w".into(),
        },
    )
    .unwrap()
}

/// Proposes view `view`; returns the proposal version's scope text.
fn propose(r: &mut Rdy, view: &str) -> (String, String) {
    let p = r.propose(&writer(), view, "t", Some(SUMMARY)).unwrap();
    let st = r.read();
    let scope = format!("proposal:{}", p.id);
    st.resolve_scope(&scope).unwrap();
    (p.id, scope)
}

/// Proposes and lands view `view`; returns the landing's scope text and the version's.
fn land(r: &mut Rdy, view: &str) -> (String, String) {
    let (id, version) = propose(r, view);
    let landing = r.land(&approver(), &id).unwrap();
    (format!("landing:{landing}"), version)
}

/// A doc of entities and notes that name them, in one write.
fn doc_ops(n: usize) -> Value {
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"title": format!("Guide {n}"), "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}}),
    ];
    for i in 0..6 {
        ops.push(
            json!({"op": "create", "type": "entity", "id": format!("$e{i}"), "parent": "$s",
            "fields": {"name": format!("Widget {n} {i}")}}),
        );
        ops.push(
            json!({"op": "create", "type": "note", "id": format!("$n{i}"), "parent": "$s",
            "body": format!("The Widget {n} {i} feeds the Gateway `code_{n}_{i}`.")}),
        );
    }
    json!(ops)
}

/// `scope` as `st` reads it, against a build of it from the store alone.
fn assert_matches_rebuild(st: &ReadState, what: &str, scope: &Scope) {
    let m = st.materialized(scope).unwrap();
    let graph = st.graph(scope).unwrap();
    let lexicon = st.lexicon(scope).unwrap();
    let (full, full_graph, full_lexicon) = rebuilt_scope_for_tests(st, scope).unwrap();
    assert!(m.same_as(&full), "{what}: blocks differ from a rebuild");
    assert!(*graph == full_graph, "{what}: graph differs from a rebuild");
    assert!(
        *lexicon == full_lexicon,
        "{what}: lexicon differs from a rebuild"
    );
}

/// The blocks of `scope` that are the very blocks canon holds (shared, not copied).
fn shared_with_canon(st: &ReadState, scope: &Scope) -> (usize, usize) {
    let canon = st.materialized(&Scope::Canon).unwrap();
    let m = st.materialized(scope).unwrap();
    let shared = m
        .iter()
        .filter(|b| {
            let (a, c) = (m.block_arc(&b.id), canon.block_arc(&b.id));
            matches!((a, c), (Some(a), Some(c)) if Arc::ptr_eq(&a, &c))
        })
        .count();
    (shared, m.len())
}

#[test]
fn scopes_off_canon_equal_a_rebuild_and_share_canons_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    // five landings: each adds a doc; the later ones also change and remove blocks of the first
    let first = write(&mut r, "work", doc_ops(0));
    let (mut landings, mut versions) = (vec![], vec![]);
    let (l, v) = land(&mut r, "work");
    landings.push(l);
    versions.push(v);
    for n in 1..5 {
        let e = first.created[&format!("$e{n}")].clone();
        let note = first.created[&format!("$n{n}")].clone();
        let mut ops = doc_ops(n).as_array().unwrap().clone();
        ops.push(json!({"op": "update", "id": e, "fields": {"name": format!("Renamed {n}")}}));
        ops.push(json!({"op": "delete", "id": note}));
        write(&mut r, "work", json!(ops));
        let (l, v) = land(&mut r, "work");
        landings.push(l);
        versions.push(v);
    }
    // four views on canon, each with its own change; v2 and v3 also change an entity the next
    // landing changes, so their merge onto it conflicts and they build from the store state;
    // one more proposal left open
    let views: Vec<String> = (0..4).map(|i| format!("v{i}")).collect();
    let contested = first.created["$e5"].clone();
    for (i, v) in views.iter().enumerate() {
        let mut ops = doc_ops(10 + i).as_array().unwrap().clone();
        if i >= 2 {
            ops.push(
                json!({"op": "update", "id": contested, "fields": {"name": format!("View {i}")}}),
            );
        }
        write(&mut r, v, json!(ops));
    }
    write(&mut r, "open", doc_ops(20));
    let (_, open) = propose(&mut r, "open");
    versions.push(open);
    // a landing after the views
    let mut ops = doc_ops(30).as_array().unwrap().clone();
    ops.push(json!({"op": "update", "id": contested, "fields": {"name": "Landed"}}));
    write(&mut r, "work", json!(ops));
    land(&mut r, "work");

    let st = r.read();
    st.graph(&Scope::Canon).unwrap();
    st.lexicon(&Scope::Canon).unwrap();
    for l in &landings[..4] {
        let scope = st.resolve_scope(l).unwrap();
        assert_matches_rebuild(&st, l, &scope);
        let (shared, len) = shared_with_canon(&st, &scope);
        assert!(shared > 0 && shared < len, "{l}: {shared} of {len} shared");
    }
    for v in &versions {
        let scope = st.resolve_scope(v).unwrap();
        assert_matches_rebuild(&st, v, &scope);
        let (shared, len) = shared_with_canon(&st, &scope);
        assert!(shared > 0, "{v}: {shared} of {len} shared");
    }
    for (i, v) in views.iter().enumerate() {
        let scope = Scope::View(v.clone());
        assert_eq!(
            st.conflicts(&scope).unwrap().is_empty(),
            i < 2,
            "{v}: conflicts"
        );
        assert_matches_rebuild(&st, v, &scope);
        let (shared, len) = shared_with_canon(&st, &scope);
        assert!(shared > 0, "{v}: {shared} of {len} shared");
    }
}

#[test]
fn a_closed_view_drops_its_build() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    write(&mut r, "work", doc_ops(0));
    land(&mut r, "work");
    write(&mut r, "side", doc_ops(1));
    let scope = Scope::View("side".into());
    r.read().lexicon(&scope).unwrap();
    assert!(r.read().lexicon_built(&scope));
    r.view_close(&writer(), &["side".to_string()], "").unwrap();
    assert!(
        !r.read().lexicon_built(&scope),
        "a closed view keeps no build"
    );
}
