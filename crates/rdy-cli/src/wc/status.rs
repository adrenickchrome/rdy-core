//! `rdy status`, the working-copy part (wcss1): runs under the working copy's lock and adds
//! `working_copy` to the store status.

use super::{Open, open, tree};
use crate::Env;
use rdy_daemon::ErrorReply;
use rdy_engine::wc::state::WcState;
use rdy_engine::wc::status::{compute, status_text};
use serde_json::Value;

/// Adds the working copy's status to the store's: its JSON under `working_copy` and its text
/// after one blank line. Does nothing when nothing is checked out here.
pub(crate) fn attach(env: &Env, v: &mut Value, t: &mut String) -> Result<(), ErrorReply> {
    let Some(place) = env.place.as_ref() else {
        return Ok(());
    };
    if WcState::load_bound(place)?.is_none() {
        return Ok(());
    }
    let Open { place, lock, state } = open(env, true)?;
    let (_pack, wt) = tree(env, &place)?;
    let s = compute(&place, &state, &wt);
    drop(lock);
    v["working_copy"] = serde_json::to_value(&s).unwrap_or(Value::Null);
    t.push_str("\n\n");
    t.push_str(&status_text(&s));
    Ok(())
}
