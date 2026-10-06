//! The edit loop's fixture: a doc with claims that reference each other and a governing decision.
use crate::fixtures::{land, write, writer};
use rdy_engine::brief::{Briefing, Coverage};
use rdy_engine::{DecisionInput, Rdy, Scope, WriteResult};
use rdy_model::BlockId;
use serde_json::json;

/// Doc `spec` (canon): section `Rules` with claims A ("A holds."), B (references A, pinned) and
/// C ("C stands alone."), and decision D ("Keep A") under the section, accepted; A is decided_by D.
#[derive(Clone, Debug)]
pub struct Spec {
    pub doc: BlockId,
    pub section: BlockId,
    pub a: BlockId,
    pub b: BlockId,
    pub c: BlockId,
    pub decision: BlockId,
}

fn created(w: &WriteResult, name: &str) -> BlockId {
    BlockId::parse(&w.created[name]).expect("created id")
}

/// Writes [`Spec`] in view `work` and lands it.
pub fn spec_doc(r: &mut Rdy) -> Spec {
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

/// Briefs `ids` in `scope` and registers the briefing in `r`'s registry.
pub fn brief_of(r: &Rdy, scope: &Scope, ids: &[&BlockId]) -> Briefing {
    let cov = Coverage {
        ids: ids.iter().map(|i| i.to_string()).collect(),
        path: None,
    };
    let b = r.read().brief(scope, &cov, None).expect("brief");
    b.register(&r.briefs());
    b
}
