//! gcar1: a graph carried across a write or a landing equals a rebuild of the new state.
use rdy_engine::*;
use rdy_graph::Graph;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::json;

// a copy of rdy_qa::edit_loop::spec_doc: this file was written beside Task 1

/// Doc `spec` (canon): section `Rules` with claims A ("A holds."), B (references A, pinned) and
/// C ("C stands alone."), and decision D ("Keep A") under the section, accepted; A is decided_by D.
#[derive(Clone, Debug)]
#[allow(dead_code)] // `doc` is read by the shared helper's other callers, not here
struct Spec {
    doc: BlockId,
    section: BlockId,
    a: BlockId,
    b: BlockId,
    c: BlockId,
    decision: BlockId,
}

fn created(w: &WriteResult, name: &str) -> BlockId {
    BlockId::parse(&w.created[name]).expect("created id")
}

/// Writes [`Spec`] in view `work` and lands it.
fn spec_doc(r: &mut Rdy) -> Spec {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
        {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds."},
        {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}}."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C stands alone."}
    ]))
    .expect("the spec doc");
    let (doc, section) = (created(&w, "$d"), created(&w, "$s"));
    let (a, b, c) = (created(&w, "$a"), created(&w, "$b"), created(&w, "$c"));
    let d = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: section.to_string(),
                statement: "Keep A".into(),
                ..Default::default()
            },
        )
        .expect("decide");
    let decision = created(&d, "$decision");
    write(
        r,
        "work",
        json!([{"op": "update", "id": a.to_string(),
        "links": {"decided_by": [{"target": decision.to_string()}]}}]),
    )
    .expect("A decided_by D");
    land(r, "work").expect("land the spec");
    Spec {
        doc,
        section,
        a,
        b,
        c,
        decision,
    }
}

fn assert_carried(r: &Rdy, scope: &Scope, what: &str) {
    let st = r.read();
    assert!(
        st.is_built(scope).1,
        "{what}: the graph was seeded by the publish, not built by this read"
    );
    let carried = st.graph(scope).unwrap();
    let rebuilt = Graph::build_from(st.materialized(scope).unwrap().iter());
    assert!(
        *carried == rebuilt,
        "{what}: the carried graph differs from a rebuild"
    );
}

#[test]
fn gcar1_a_carried_graph_equals_a_rebuild_after_every_kind_of_write() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let v = Scope::View("w".into());
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": s.c.to_string(), "body": "C warms the view."}]),
    )
    .unwrap();
    let (a, b, c, sec) = (
        s.a.to_string(),
        s.b.to_string(),
        s.c.to_string(),
        s.section.to_string(),
    );
    let steps = vec![
        (
            "create with a reference",
            json!([{"op": "create", "type": "claim", "id": "$n", "parent": sec, "body": format!("N cites {{{{{a}}}}}.")}]),
        ),
        (
            "update a body",
            json!([{"op": "update", "id": a, "body": "A changed."}]),
        ),
        // B still cites A here, so B has something to repin (a repin after "drop a reference"
        // is refused: nothing to repin)
        ("repin", json!([{"op": "repin", "id": b}])),
        (
            "drop a reference",
            json!([{"op": "update", "id": b, "body": "B stands alone now."}]),
        ),
        (
            "add a link",
            json!([{"op": "update", "id": c, "links": {"decided_by": [{"target": s.decision.to_string()}]}}]),
        ),
        (
            "remove a link kind",
            json!([{"op": "update", "id": c, "links": {"decided_by": []}}]),
        ),
        ("delete", json!([{"op": "delete", "id": c}])),
        (
            "move B ahead of A",
            json!([{"op": "move", "id": b, "parent": sec, "before": a}]),
        ),
    ];
    for (what, ops) in steps {
        r.read().graph(&v).unwrap(); // the agent's read before its write
        write(&mut r, "w", ops).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_carried(&r, &v, what);
    }
    r.read().graph(&v).unwrap();
    r.supersede(
        &writer(),
        "w",
        &s.decision.to_string(),
        None,
        DecisionInput {
            parent: sec.clone(),
            statement: "Keep A, firmly".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_carried(&r, &v, "a supersede (a decision's status moves)");
}

#[test]
fn gcar1_a_landing_carries_canons_graph() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": s.a.to_string(), "body": "A, landed."}]),
    )
    .unwrap();
    r.read().graph(&Scope::Canon).unwrap();
    land(&mut r, "w").unwrap();
    assert_carried(&r, &Scope::Canon, "a landing");
}

// FW-IB2 (D-Z0EMP): each write logs its tension and suspect moves, so a cold write builds
// the in-flight graph once; the next write carries it instead of building it again.
#[test]
fn gcar1_a_cold_write_builds_the_graph_once_and_the_next_write_carries_it() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    r.invalidate();
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": s.c.to_string(), "body": "Cold."}]),
    )
    .unwrap();
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": s.a.to_string(), "body": "Warm."}]),
    )
    .unwrap();
    assert_carried(&r, &Scope::View("w".into()), "a write after a cold write");
}
