//! `rdy update` (wcup1, spec §12.4, D-WY79P): brings the bound view's changes made elsewhere into
//! the working copy. Local edits are kept, disjoint ones merge, and what changed on both sides is
//! a conflict written beside the file as `.theirs.yaml`, resolved through `rdy add`.

use super::{Open, open, tree};
use crate::{Env, call, hash_short, path_arg};
use rdy_daemon::ErrorReply;
use rdy_engine::wc::layout::BLOBS_DIR;
use rdy_engine::wc::merge::{apply_update, plan_update};
use rdy_engine::wc::state::ConflictKind;
use rdy_engine::wc::status::conflict_message;
use rdy_engine::{E_BAD_INPUT, E_WC_CONFLICT, Projection, RdyError};
use serde_json::{Value, json};

pub(super) fn run(env: &Env) -> Result<(Value, String), ErrorReply> {
    let Open {
        place,
        lock,
        mut state,
    } = open(env, true)?;
    // a kept file's base is older than the store: a merge against it would record its new
    // blocks a second time
    let kept: Vec<&str> = state
        .conflicts
        .iter()
        .filter(|c| c.kind == ConflictKind::Kept)
        .map(|c| c.path.as_str())
        .collect();
    if !kept.is_empty() {
        return Err(RdyError::new(
            E_WC_CONFLICT,
            format!(
                "{} file(s) were committed but not written back: {}",
                kept.len(),
                kept.join(", ")
            ),
            "copy the files named aside, then run rdy checkout --force (it discards uncommitted edits)",
        )
        .into());
    }
    let view = state.require_bound()?.view.clone();
    let (pack, wt) = tree(env, &place)?;
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
    let head = proj.head.strip_prefix("canon:").unwrap_or(&proj.head);
    let mut text = format!(
        "updated {view} to {}: {} file(s) written, {} conflict(s)",
        hash_short(head),
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
        "head": proj.head,
        "written": written,
        "removed": plan.removes,
        "conflicts": conflicts,
    });
    Ok((out, text))
}
