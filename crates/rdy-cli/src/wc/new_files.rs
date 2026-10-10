//! `rdy new` writes the new doc and folder files into a working copy bound to the target view
//! (wcnw1).

use super::open;
use crate::{Env, bound_view, call};
use rdy_daemon::ErrorReply;
use rdy_engine::wc::files::add_projected;
use rdy_engine::{E_BAD_INPUT, Projection};
use serde_json::json;

/// What `rdy new` wrote into the working copy, and what it left.
#[derive(Default)]
pub(crate) struct NewFiles {
    /// The files written, in the projection's order.
    pub wrote: Vec<String>,
    /// The paths left alone because they hold a file rdy does not track, or another doc's file.
    pub skipped: Vec<String>,
}

impl NewFiles {
    /// The lines `rdy new` adds to its text: one per file written, one warning per file skipped.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for p in &self.wrote {
            out.push_str(&format!("\nwrote {p}"));
        }
        for p in &self.skipped {
            out.push_str(&format!(
                "\nwarning: not written: {p} is occupied by a file rdy does not track, or by another doc's file; move it, then run rdy update"
            ));
        }
        out
    }
}

/// msw16: refuses `rdy new` in a copied worktree directory before the store write, when the write
/// goes to the view the working copy is bound to (`view` unset or that view, in a human
/// session). The real worktree's view would get the doc, and the copy could not take its file.
pub(crate) fn refuse_in_copy(env: &Env, view: Option<&str>) -> Result<(), ErrorReply> {
    let Some(place) = env.place.as_ref() else {
        return Ok(());
    };
    if !crate::conn::human_session(env) {
        return Ok(());
    }
    let Some(bound) = bound_view(env)? else {
        return Ok(());
    };
    if view.is_some_and(|v| v.strip_prefix("view:").unwrap_or(v) != bound) {
        return Ok(());
    }
    match super::copy_refusal(place)? {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Writes the files of the docs and folders `created` (ids) into the working copy, when it is
/// bound to `view`, the view the write went to. Outside a checkout, or bound to another view,
/// nothing is written. A path that holds a file rdy does not track, or another doc's file, is
/// skipped, never overwritten.
pub(crate) fn write_new(env: &Env, view: &str, created: &[String]) -> Result<NewFiles, ErrorReply> {
    // the cheap look first: a project that never checked out takes no lock and gets no state
    if created.is_empty() || bound_view(env)?.as_deref() != Some(view) {
        return Ok(NewFiles::default());
    }
    let mut open = open(env, true)?;
    if open.state.bound.as_ref().map(|b| b.view.as_str()) != Some(view) {
        return Ok(NewFiles::default());
    }
    let v = call(env, "wc_view", json!({"view": view, "ids": created}))?;
    let proj: Projection = serde_json::from_value(v).map_err(|e| {
        ErrorReply::new(
            E_BAD_INPUT,
            format!("the daemon's projection does not parse: {e}"),
            "run rdy serve --stop, then retry with the same rdy binary",
        )
    })?;
    let added = add_projected(&open.place, &mut open.state, &proj)?;
    open.state.save(&open.place)?;
    Ok(NewFiles {
        wrote: added.written,
        skipped: added.skipped,
    })
}
