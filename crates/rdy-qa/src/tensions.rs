//! Helpers for the tension tests: agents writing in their own views, a few op builders, and the
//! event-derived tensions of the published state.
use crate::fixtures::{as_agent, write_as};
use rdy_engine::brief::{E_BRIEF_REQUIRED, E_BRIEF_STALE};
use rdy_engine::tensions::{SideRef, Tension, TensionKind};
use rdy_engine::{DecisionInput, OpInput, Rdy, RdyError, Session, WriteRequest, WriteResult};
use rdy_model::{BlockId, EditorialClass, Hash, Justification};
use serde_json::{Value, json};
use std::sync::Arc;

/// Agent `agent:<name>` with run `<name>`: its own view is `<name>`.
pub fn ag(name: &str) -> Session {
    as_agent(&format!("agent:{name}"), name)
}

/// Writes `ops` as agent `name` into its view `name`.
pub fn try_put(r: &mut Rdy, name: &str, ops: Value) -> Result<WriteResult, RdyError> {
    write_as(r, &ag(name), name, ops)
}

/// `try_put`, which must succeed.
pub fn put(r: &mut Rdy, name: &str, ops: Value) -> WriteResult {
    try_put(r, name, ops).expect("write")
}

/// The change a write made.
pub fn change_of(w: &WriteResult) -> Hash {
    w.change.parse().expect("change hash")
}

/// The id a write created for placeholder `name` (`$x`).
pub fn created(w: &WriteResult, name: &str) -> BlockId {
    BlockId::parse(&w.created[name]).expect("created id")
}

/// Every event-derived tension of the published state, by id.
pub fn open(r: &Rdy) -> Vec<Arc<Tension>> {
    r.read()
        .tensions()
        .expect("tensions")
        .iter()
        .cloned()
        .collect()
}

/// The tensions of `kind`.
pub fn of_kind(r: &Rdy, kind: TensionKind) -> Vec<Arc<Tension>> {
    open(r).into_iter().filter(|t| t.kind == kind).collect()
}

/// The one tension of `kind`; panics unless there is exactly one.
pub fn one(r: &Rdy, kind: TensionKind) -> Arc<Tension> {
    let mut v = of_kind(r, kind);
    assert_eq!(v.len(), 1, "{kind}: {v:#?}");
    v.remove(0)
}

/// The one tension of `kind` that involves `block`; panics unless there is exactly one.
pub fn one_involving(r: &Rdy, kind: TensionKind, block: &BlockId) -> Arc<Tension> {
    let mut v: Vec<_> = of_kind(r, kind)
        .into_iter()
        .filter(|t| t.blocks().contains(block))
        .collect();
    assert_eq!(v.len(), 1, "{kind} on {block}: {v:#?}");
    v.remove(0)
}

/// The change sides of `t`, sorted.
pub fn change_sides(t: &Tension) -> Vec<Hash> {
    t.sides
        .iter()
        .filter_map(|s| match s {
            SideRef::Change(h) => Some(*h),
            _ => None,
        })
        .collect()
}

/// Whether `t` has a canon side.
pub fn has_canon(t: &Tension) -> bool {
    t.sides.iter().any(|s| matches!(s, SideRef::Canon { .. }))
}

/// One update op setting `id`'s body.
pub fn upd(id: &BlockId, body: &str) -> Value {
    json!([{"op": "update", "id": id.to_string(), "body": body}])
}

/// One create op: claim `$y` under `parent` whose body references `target`.
pub fn cite(parent: &BlockId, target: &BlockId) -> Value {
    json!([{"op": "create", "type": "claim", "id": "$y", "parent": parent.to_string(),
            "body": format!("Builds on {{{{{target}}}}}.")}])
}

/// One create op: term `$t` named `name` under `parent`.
pub fn term(parent: &BlockId, name: &str) -> Value {
    json!([{"op": "create", "type": "term", "id": "$t", "parent": parent.to_string(),
            "fields": {"name": name}, "body": format!("{name} is a term.")}])
}

/// Runs `f` with no briefing. On `E_BRIEF_REQUIRED` or `E_BRIEF_STALE`, runs it once more with
/// the fresh briefing's token for `view`. Any other result is returned as is.
pub fn try_briefed(
    r: &mut Rdy,
    view: &str,
    f: impl Fn(&mut Rdy, Option<String>) -> Result<WriteResult, RdyError>,
) -> Result<WriteResult, RdyError> {
    match f(r, None) {
        Err(e) if e.code == E_BRIEF_REQUIRED || e.code == E_BRIEF_STALE => {
            let (_, t) = crate::edit_loop::token_in(r, e, view);
            f(r, Some(t))
        }
        other => other,
    }
}

/// Writes `ops` as agent `name` into its view `name` with justification `j`, briefed as needed
/// (see [`try_briefed`]).
pub fn try_put_j(
    r: &mut Rdy,
    name: &str,
    ops: Value,
    j: Option<Justification>,
) -> Result<WriteResult, RdyError> {
    let ops: Vec<OpInput> = crate::fixtures::parse_test_ops(ops)?;
    try_briefed(r, name, |r, brief| {
        r.write_briefed(
            &ag(name),
            WriteRequest {
                view: name.into(),
                ops: ops.clone(),
                justification: j.clone(),
                message: String::new(),
            },
            brief,
        )
    })
}

/// `try_put_j`, which must succeed.
pub fn put_j(r: &mut Rdy, name: &str, ops: Value, j: Option<Justification>) -> WriteResult {
    try_put_j(r, name, ops, j).expect("write")
}

/// Decides `statement` under `parent` as agent `name` in its view `name`, briefed as needed
/// (see [`try_briefed`]); the created decision's id.
pub fn try_decide_in(
    r: &mut Rdy,
    name: &str,
    parent: &BlockId,
    statement: &str,
) -> Result<BlockId, RdyError> {
    let w = try_briefed(r, name, |r, brief| {
        r.decide(
            &ag(name),
            name,
            DecisionInput {
                parent: parent.to_string(),
                statement: statement.into(),
                brief,
                ..Default::default()
            },
        )
    })?;
    Ok(created(&w, "$decision"))
}

/// The justification `implements: [d]`.
pub fn j_implements(d: &BlockId) -> Option<Justification> {
    Some(Justification::Implements(vec![d.clone()]))
}

/// An editorial justification: a typo fix.
pub fn j_typo() -> Option<Justification> {
    Some(Justification::Editorial {
        class: EditorialClass::Typo,
        reason: "typo".into(),
    })
}
