//! Ownership (own1): the human reassigns a view (`rdy view own`) and closes idle views in bulk
//! (`rdy view close --stale`).
use super::{CloseResult, ViewHeadInfo, head_info, no_view};
use crate::error::*;
use crate::{Delta, Rdy, Session};
use rdy_store::Owner;
use rdy_store::view::ViewOpKind;
use serde::Serialize;

/// What `rdy view close --stale` did: the views it closed, and the session's own default view
/// when that was idle too and was kept open (the next write there would otherwise be refused).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StaleClose {
    pub closed: Vec<CloseResult>,
    pub kept: Option<String>,
}

/// An owner as `rdy view own` takes it: `<actor>` or `<actor>/<run>`, split at the first `/`.
fn parse_owner(text: &str) -> Result<Owner, RdyError> {
    let (actor, run) = match text.split_once('/') {
        Some((actor, run)) => (actor, Some(run)),
        None => (text, None),
    };
    if actor.is_empty() || run.is_some_and(str::is_empty) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "owner must be <actor>[/<run>]",
            "for example agent:claude/r1, or agent:claude for a session with no run",
        ));
    }
    Ok(Owner {
        actor: actor.to_string(),
        run: run.map(str::to_string),
    })
}

impl Rdy {
    /// own1: the human reassigns `view` to `owner` (`<actor>[/<run>]`), or leaves it unowned
    /// (`None`), so that the next commit claims it. Works on a closed view and leaves it closed.
    pub fn view_own(
        &mut self,
        s: &Session,
        view: &str,
        owner: Option<&str>,
    ) -> Result<ViewHeadInfo, RdyError> {
        if !s.is_human() {
            return Err(RdyError::new(
                E_PERMISSION,
                "only a human reassigns a view",
                "ask the human, or fork the view",
            ));
        }
        if !s.role.can_write() {
            return Err(RdyError::new(
                E_PERMISSION,
                format!("role {:?} cannot reassign views", s.role),
                "start the session as a writer",
            ));
        }
        if self.read().view_head(view).is_none() {
            return Err(no_view(view));
        }
        let owner = owner.map(parse_owner).transpose()?;
        self.store
            .view_append(view, vec![ViewOpKind::Own { owner }], &s.provenance())?;
        self.publish(Delta::View {
            view: view.to_string(),
        })?;
        head_info(&self.read(), view)
    }

    /// own1: the human closes every open view idle for more than `days` days at `now_ms`, each
    /// as `view_close` does, with the reason "idle for more than {days} days". The session's own
    /// default view (the human's `work`) is never closed this way; when it is idle it is kept
    /// and named in `kept`.
    pub fn view_close_stale(
        &mut self,
        s: &Session,
        days: u32,
        now_ms: i64,
    ) -> Result<StaleClose, RdyError> {
        if !s.is_human() {
            return Err(RdyError::new(
                E_PERMISSION,
                "only a human closes views in bulk",
                "run rdy view close --stale as the human",
            ));
        }
        let own = s.default_view();
        let (kept, idle): (Vec<String>, Vec<String>) = self
            .read()
            .view_list(false, days, now_ms)?
            .into_iter()
            .filter(|v| v.idle)
            .map(|v| v.name)
            .partition(|name| *name == own);
        let kept = kept.into_iter().next();
        if idle.is_empty() {
            return Ok(StaleClose {
                closed: vec![],
                kept,
            });
        }
        let closed = self.view_close(s, &idle, &format!("idle for more than {days} days"))?;
        Ok(StaleClose { closed, kept })
    }
}
