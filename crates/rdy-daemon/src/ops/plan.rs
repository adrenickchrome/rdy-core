//! The plan, reaffirm and repin ops (D-4JMPH, D-EJBPD, D-01QX5, D-KQ4CA): the chat tools' work
//! for the command line. `plan` runs the one function `rdy_plan` runs. The two writes resolve
//! their whole subject set, fetch and register its briefing and run the ordinary write with that
//! token, all in one writer closure, so the selection cannot move between the three. Ownership,
//! frozen and every other write guard are the write path's own.
use super::*;
use rdy_engine::consequence::write_and_report;
use rdy_engine::reaffirm::check_shared_inputs;
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

/// The reaffirm ops `a` names: one per id, or the one `because` op. Exactly one of the two. The
/// reason and targets every op repeats are checked once, by the engine, before any op is built.
fn reaffirm_ops(a: &ReaffirmArgs) -> Result<Vec<OpInput>, RdyError> {
    let none_or_both = || {
        RdyError::new(
            E_BAD_INPUT,
            "reaffirm needs ids or because",
            "pass the decisions to re-affirm, or the block they are suspect on",
        )
    };
    let count = match (&a.ids, &a.because) {
        (Some(_), Some(_)) => {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "reaffirm takes ids or because, not both",
                "pass the decisions to re-affirm, or the block they are suspect on",
            ));
        }
        (Some(ids), None) if !ids.is_empty() => ids.len(),
        (None, Some(_)) => 1,
        _ => return Err(none_or_both()),
    };
    let reason = check_shared_inputs(count, &a.reason, a.targets.as_deref())?;
    let op = |id: Option<String>, because: Option<String>| OpInput::Reaffirm {
        id,
        because,
        base: None,
        targets: a.targets.clone(),
        reason: reason.clone(),
    };
    Ok(match (&a.ids, &a.because) {
        (Some(ids), None) => ids.iter().map(|id| op(Some(id.clone()), None)).collect(),
        (None, Some(b)) => vec![op(None, Some(b.clone()))],
        _ => unreachable!("checked above"),
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(n: usize, reason: String, targets: Option<Vec<String>>) -> ReaffirmArgs {
        ReaffirmArgs {
            ids: Some((0..n).map(|i| format!("D-{i:05}")).collect()),
            because: None,
            targets,
            reason,
            view: None,
        }
    }

    /// The refusal of `a`'s ops. A built request is a failure, reported without printing the ops
    /// (a regression's ops would be hundreds of megabytes).
    fn refused(a: &ReaffirmArgs) -> RdyError {
        match reaffirm_ops(a) {
            Ok(ops) => panic!("{} ops were built, not refused", ops.len()),
            Err(e) => e,
        }
    }

    /// A bulk request is refused on its shared inputs before one op is built: with a reason the
    /// engine refuses, or targets past the pair bound, no per-id copy of them is made. The
    /// reason here is 100 KB, so a regression that copies it per id (4096 times) would copy
    /// 400 MB and return ops instead of the refusal. (An 8 MiB reason, as an attacker sends,
    /// would copy 32 GiB.)
    #[test]
    fn cp11_fix1_reaffirm_ops_refuse_shared_inputs_before_copying_them() {
        let e = refused(&args(4096, "x".repeat(100_000), None));
        assert_eq!(e.code, E_BAD_INPUT, "{e}");
        let e = refused(&args(4096, " ".repeat(100_000), None));
        assert_eq!(e.code, E_BAD_INPUT, "{e}");
        let ts: Vec<String> = (0..30).map(|i| format!("B-{i:05}")).collect();
        let e = refused(&args(4096, "Holds.".into(), Some(ts)));
        assert_eq!(e.code, E_BAD_INPUT, "{e}");
        let big = vec!["p".repeat(1_000_000)];
        let e = refused(&args(4096, "Holds.".into(), Some(big)));
        assert_eq!(e.code, E_BAD_INPUT, "{e}");
        let e = refused(&args(5001, "Holds.".into(), None));
        assert_eq!(e.code, E_BAD_INPUT, "{e}");
    }

    #[test]
    fn cp11_fix1_reaffirm_ops_still_build_a_normal_request() {
        let ops = reaffirm_ops(&args(
            300,
            "  Holds.  ".into(),
            Some(vec!["T-AAAAA".into()]),
        ))
        .unwrap();
        assert_eq!(ops.len(), 300);
        match &ops[0] {
            OpInput::Reaffirm {
                reason, targets, ..
            } => {
                assert_eq!(reason, "Holds.");
                assert_eq!(targets.as_deref(), Some(&["T-AAAAA".to_string()][..]));
            }
            other => panic!("{other:?}"),
        }
    }
}
