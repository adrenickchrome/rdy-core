//! The plan, reaffirm and repin ops (D-4JMPH, D-EJBPD, D-01QX5, D-KQ4CA): the chat tools' work
//! for the command line. `plan` runs the one function `rdy_plan` runs. The two writes resolve
//! their whole subject set, fetch and register its briefing and run the ordinary write with that
//! token, all in one writer closure, so the selection cannot move between the three. Ownership,
//! frozen and every other write guard are the write path's own.
use super::*;
use rdy_engine::consequence::write_and_report;
use rdy_engine::{OpInput, RdyError, WriteRequest};
use rdy_mcp::tools::{PlanParams, run_plan};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReaffirmArgs {
    ids: Option<Vec<String>>,
    because: Option<String>,
    targets: Option<Vec<String>>,
    reason: String,
    view: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RepinArgs {
    id: String,
    targets: Option<Vec<String>>,
    #[serde(default)]
    code: bool,
    view: Option<String>,
}

/// Reads a change plan, or answers items of the session's view plan.
pub(super) fn plan(cx: &OpContext, a: PlanParams) -> Result<Value, RdyError> {
    run_plan(cx.engine, cx.session, &cx.session.default_view(), a)
}

/// The reaffirm ops `a` names: one per id, or the one `because` op. Exactly one of the two.
fn reaffirm_ops(a: &ReaffirmArgs) -> Result<Vec<OpInput>, RdyError> {
    let op = |id: Option<String>, because: Option<String>| OpInput::Reaffirm {
        id,
        because,
        base: None,
        targets: a.targets.clone(),
        reason: a.reason.clone(),
    };
    match (&a.ids, &a.because) {
        (Some(_), Some(_)) => Err(RdyError::new(
            E_BAD_INPUT,
            "reaffirm takes ids or because, not both",
            "pass the decisions to re-affirm, or the block they are suspect on",
        )),
        (Some(ids), None) if !ids.is_empty() => {
            Ok(ids.iter().map(|id| op(Some(id.clone()), None)).collect())
        }
        (None, Some(b)) => Ok(vec![op(None, Some(b.clone()))]),
        _ => Err(RdyError::new(
            E_BAD_INPUT,
            "reaffirm needs ids or because",
            "pass the decisions to re-affirm, or the block they are suspect on",
        )),
    }
}

/// Re-affirms the decisions `a` selects, briefed whole, as one write.
pub(super) fn reaffirm(cx: &OpContext, a: ReaffirmArgs) -> Result<Value, RdyError> {
    let ops = reaffirm_ops(&a)?;
    let s = cx.session.clone();
    let view = view_of(&s, a.view);
    let briefs = cx.engine.briefs();
    let named = view.clone();
    write_and_report(cx.engine, &named, None, move |r| {
        let ids = r.reaffirm_selection(&s, &view, &ops)?;
        let briefing = r.read().brief_ids(&Scope::View(view.clone()), &ids, None)?;
        briefing.register(&briefs);
        let token = briefing.base.token.clone();
        r.write_briefed(
            &s,
            WriteRequest {
                view,
                ops,
                justification: None,
                message: String::new(),
            },
            Some(token),
        )
    })
}

/// Repins one block, briefed, as one write. The reply is `{pins, result}`: the design pins the
/// repin selected (a pin already current included; `code` adds none) and the write's report.
pub(super) fn repin(cx: &OpContext, a: RepinArgs) -> Result<Value, RdyError> {
    let s = cx.session.clone();
    let view = view_of(&s, a.view);
    let briefs = cx.engine.briefs();
    let named = view.clone();
    let pins = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&pins);
    let (id, targets, code) = (a.id, a.targets, a.code);
    let result = write_and_report(cx.engine, &named, None, move |r| {
        let (block, count) = r.repin_selection(&s, &view, &id, targets.as_deref())?;
        seen.store(count, Ordering::SeqCst);
        let briefing = r.read().brief_ids(
            &Scope::View(view.clone()),
            &std::collections::BTreeSet::from([block.clone()]),
            None,
        )?;
        briefing.register(&briefs);
        let token = briefing.base.token.clone();
        r.write_briefed(
            &s,
            WriteRequest {
                view,
                ops: vec![OpInput::Repin {
                    id: block.to_string(),
                    base: None,
                    targets: targets.unwrap_or_default(),
                    code,
                }],
                justification: None,
                message: String::new(),
            },
            Some(token),
        )
    })?;
    Ok(json!({"pins": pins.load(Ordering::SeqCst), "result": result}))
}
