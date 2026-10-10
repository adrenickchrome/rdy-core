//! `rdy status`, the working-copy part (wcss1): runs under the working copy's lock and adds
//! `working_copy` to the store status.

use super::{Open, copy_refusal, open, tree};
use crate::Env;
use rdy_daemon::ErrorReply;
use rdy_engine::wc::state::{CHECKOUT_FILE, WcState};
use rdy_engine::wc::status::{compute, status_text};
use serde_json::{Value, json};

/// Adds the working copy's status to the store's: its JSON under `working_copy` and its text
/// after one blank line. Does nothing when nothing is checked out here. While a checkout or an
/// update is interrupted, the part is the refusal every other command gives (`interrupted`:
/// code, message and fix), so the store status still shows; in a copied worktree directory it is
/// that refusal (`refused`).
pub(crate) fn attach(env: &Env, v: &mut Value, t: &mut String) -> Result<(), ErrorReply> {
    let Some(place) = env.place.as_ref() else {
        return Ok(());
    };
    let marked = std::fs::symlink_metadata(place.state.join(CHECKOUT_FILE)).is_ok();
    if WcState::load_bound(place)?.is_none() && !marked {
        return Ok(());
    }
    // msw16: in a copied worktree directory the part is the refusal every other command gives,
    // so the store status still shows
    if let Some(e) = copy_refusal(place)? {
        v["working_copy"] = json!({
            "refused": {"code": e.code, "message": e.message, "fix": e.fix},
        });
        t.push_str(&format!(
            "\n\nworking copy: {}\n  fix: {}",
            e.message, e.fix
        ));
        return Ok(());
    }
    let Open { place, lock, state } = open(env, false)?;
    if let Err(e) = state.require_settled() {
        drop(lock);
        v["working_copy"] = json!({
            "interrupted": {"code": e.code, "message": e.message, "fix": e.fix},
        });
        t.push_str(&format!(
            "\n\nworking copy: {}\n  fix: {}",
            e.message, e.fix
        ));
        return Ok(());
    }
    state.require_bound()?;
    let (_pack, wt) = tree(env, &place)?;
    let s = compute(&place, &state, &wt);
    drop(lock);
    v["working_copy"] = serde_json::to_value(&s).unwrap_or(Value::Null);
    t.push_str("\n\n");
    t.push_str(&status_text(&s));
    Ok(())
}
