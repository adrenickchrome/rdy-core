//! `rdy update` (wcup1, spec §12.4, D-WY79P): brings the bound view's changes made elsewhere into
//! the working copy. Local edits are kept, disjoint ones merge, and what changed on both sides is
//! a conflict written beside the file as `.theirs.yaml`, resolved through `rdy add`.

use super::{Open, open, tree};
use crate::{Env, call, hash_short, path_arg};
use rdy_daemon::ErrorReply;
use rdy_engine::wc::layout::BLOBS_DIR;
use rdy_engine::wc::merge::{UpdatePlan, apply_update, plan_update, resume_update};
use rdy_engine::wc::status::conflict_message;
use rdy_engine::{E_BAD_INPUT, Projection};
use serde_json::{Value, json};

pub(super) fn run(env: &Env) -> Result<(Value, String), ErrorReply> {
    let Open {
        place,
        lock,
        mut state,
    } = open(env, false)?;
    if state.checkout.as_ref().is_some_and(|m| m.update) {
        // an update was interrupted: finish it from its record before anything else
        let j = resume_update(&place, &mut state)?;
        drop(lock);
        return Ok(report(&j.view, &j.head, &j.plan, true));
    }
    // a kept file (committed, not written back) becomes a conflict with the store's side beside
    // it (`plan_update`)
    let view = state.require_bound()?.view.clone();
    let (pack, wt) = tree(env, &place)?;
    // as checkout: the blobs are written before `plan_update` can refuse (named by hash, so a
    // refused update's blobs are never wrong)
    let blobs_dir = path_arg(&place.root.join(BLOBS_DIR))?;
    let v = call(
        env,
        "wc_view",
        json!({"view": view, "blobs_dir": blobs_dir}),
    )?;
    let proj: Projection = serde_json::from_value(v).map_err(|e| {
        ErrorReply::new(
            E_BAD_INPUT,
            format!("the daemon's projection does not parse: {e}"),
            "run rdy serve --stop, then retry with the same rdy binary",
        )
    })?;
    let plan = plan_update(&place, &state, &wt, &proj, &pack)?;
    apply_update(&place, &mut state, &plan, &proj)?;
    drop(lock);
    Ok(report(&view, &proj.head, &plan, false))
}

/// The output of an update; `finished` when it finished an interrupted one.
fn report(view: &str, head: &str, plan: &UpdatePlan, finished: bool) -> (Value, String) {
    let short = head.strip_prefix("canon:").unwrap_or(head);
    let mut text = format!(
        "{} {view} to {}: {} file(s) written, {} conflict(s)",
        if finished {
            "finished the interrupted update of"
        } else {
            "updated"
        },
        hash_short(short),
        plan.writes.len(),
        plan.conflicts.len()
    );
    let conflicts: Vec<Value> = plan
        .conflicts
        .iter()
        .map(|c| {
            let message = conflict_message(c);
            text.push('\n');
            text.push_str(&message);
            json!({"path": c.path, "kind": c.kind, "message": message})
        })
        .collect();
    let written: Vec<&str> = plan.writes.iter().map(|(p, _)| p.as_str()).collect();
    let out = json!({
        "view": view,
        "head": head,
        "written": written,
        "removed": plan.removes,
        "conflicts": conflicts,
    });
    (out, text)
}
