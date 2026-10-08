//! `rdy checkout [<view>] [--force]` (wcco1): writes the view as files and binds the working copy
//! to it.

use super::{Open, open, open_at, tree};
use crate::{Env, call, hash_short, path_arg};
use rdy_daemon::ErrorReply;
use rdy_engine::wc::files::{checkout, dirty_paths};
use rdy_engine::wc::layout::BLOBS_DIR;
use rdy_engine::wc::state::Place;
use rdy_engine::{E_BAD_INPUT, E_WC_DIRTY, Projection, RdyError};
use serde_json::{Value, json};

pub(super) fn run(
    env: &Env,
    view: Option<String>,
    force: bool,
) -> Result<(Value, String), ErrorReply> {
    let open = open(env, false)?;
    checkout_in(env, open, view, force)
}

/// The checkout of `view` into the working copy `open` holds; `rdy worktree add` runs it with
/// the new worktree's place.
pub(super) fn checkout_place(
    env: &Env,
    place: Place,
    view: Option<String>,
    force: bool,
) -> Result<(Value, String), ErrorReply> {
    let open = open_at(env, place, false)?;
    checkout_in(env, open, view, force)
}

fn checkout_in(
    env: &Env,
    open: Open,
    view: Option<String>,
    force: bool,
) -> Result<(Value, String), ErrorReply> {
    let Open {
        place,
        lock,
        mut state,
    } = open;
    let (_pack, wt) = tree(env, &place)?;
    if !force {
        state.require_settled()?;
    }
    if state.bound.is_some() && !force {
        let dirty = dirty_paths(&place, &state, &wt);
        if !dirty.is_empty() {
            return Err(RdyError::new(
                E_WC_DIRTY,
                format!(
                    "uncommitted edits in {} file(s): {}",
                    dirty.len(),
                    dirty
                        .iter()
                        .take(10)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "commit them, or pass --force to discard them",
            )
            .into());
        }
    }
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
    let s = checkout(&place, &mut state, &wt, &proj, force)?;
    drop(lock);
    let head = proj.head.strip_prefix("canon:").unwrap_or(&proj.head);
    let text = format!(
        "checked out {} at {}: {} docs, {} folders, {} blobs",
        proj.view,
        hash_short(head),
        s.docs,
        s.folders,
        s.blobs
    );
    let out = json!({
        "view": proj.view,
        "head": proj.head,
        "docs": s.docs,
        "folders": s.folders,
        "blobs": s.blobs,
        "files": s.written,
    });
    Ok((out, text))
}
