//! The edit loop's fixture: a doc with claims that reference each other and a governing decision.
use crate::fixtures::{land, write, writer};
use rdy_engine::brief::expand_error;
use rdy_engine::brief::{Briefing, Coverage};
use rdy_engine::{DecisionInput, Rdy, Scope, WriteResult};
use rdy_engine::{RdyError, Session, WriteRequest};
use rdy_model::BlockId;
use serde_json::Value;
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

/// [`spec_doc`], then the section made stable in view `st` (citing a briefing of it) and
/// landed: every block of the spec is stable in canon.
pub fn stable_spec(r: &mut Rdy) -> Spec {
    let s = spec_doc(r);
    let t = brief_of(r, &Scope::Canon, &[&s.section]).base.token;
    write_cited(
        r,
        "st",
        json!([{"op": "update", "id": s.section.to_string(),
        "fields": {"lifecycle": "stable"}}]),
        Some(&t),
    )
    .expect("make the section stable");
    land(r, "st").expect("land the stable section");
    s
}

/// A write as `s` that cites `brief`.
pub fn write_cited_as(
    r: &mut Rdy,
    s: &Session,
    view: &str,
    ops: Value,
    brief: Option<&str>,
) -> Result<WriteResult, RdyError> {
    let ops = serde_json::from_value(ops)
        .map_err(|e| RdyError::new(rdy_engine::E_BAD_INPUT, e.to_string(), "fix the test's ops"))?;
    let req = WriteRequest {
        view: view.into(),
        ops,
        justification: None,
        message: String::new(),
    };
    r.write_briefed(s, req, brief.map(str::to_string))
}

/// A write as the human writer that cites `brief`.
pub fn write_cited(
    r: &mut Rdy,
    view: &str,
    ops: Value,
    brief: Option<&str>,
) -> Result<WriteResult, RdyError> {
    write_cited_as(r, &writer(), view, ops, brief)
}

/// A briefing refusal expanded as the tools expand it, and the token of its fresh briefing.
pub fn token_in(r: &Rdy, e: RdyError, view: &str) -> (RdyError, String) {
    let e = expand_error(e, &r.read(), view, &r.briefs(), None);
    let t = e
        .briefing
        .as_ref()
        .and_then(|b| b["base"]["token"].as_str())
        .unwrap_or_else(|| panic!("no fresh briefing in {e}"))
        .to_string();
    (e, t)
}
