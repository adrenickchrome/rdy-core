//! Ownership (own1): the human reassigns a view (`rdy view own`) and closes idle views in bulk
//! (`rdy view close --stale`). Both are a person's acts, so an agent session relays them with the
//! person's words (D-KQ4CA): the act then runs as the person, and an `ANSWER` in the same batch
//! holds the relay (`view_own_as`, `view_close_stale_as`). A new owner is an actor and an
//! optional run in separate fields (D-PWB82), never one string.
use super::{CloseResult, ViewHeadInfo, head_info, no_view};
use crate::answer::relay::{RelayInput, RelayRefusal, relayed};
use crate::error::*;
use crate::version::answer_write_error;
use crate::view_api::OwnerInput;
use crate::{Delta, Rdy, Session};
use rdy_model::{ActorRun, Answer, AnswerAct, EvidenceKind};
use rdy_store::Owner;
use rdy_store::view::ViewOpKind;
use serde::Serialize;

/// What `rdy view close --stale` did: the views it closed, and the session's own default view
/// when that was idle too and was kept open (the next write there would otherwise be refused).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StaleClose {
    pub closed: Vec<CloseResult>,
    pub kept: Option<String>,
    /// The agent that relayed the person's words, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relayed_by: Option<ActorRun>,
    /// The `ANSWER` record's hash, for a relayed close (also when no view was idle).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

/// What `Rdy::view_own_as` did: the view's head, and for a relayed hand-over the agent that
/// relayed it and the `ANSWER` that holds the relay (D-DGCH7).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OwnInfo {
    #[serde(flatten)]
    pub head: ViewHeadInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relayed_by: Option<ActorRun>,
    /// The `ANSWER` record's hash.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

/// The fix of a refusal of a human act by an agent session.
const RELAY_FIX: &str = "relay the person's words with evidence";

/// Whether `relay` holds nothing: an act with no relay input runs as the session itself, under
/// today's rule. A `question` or `session_ref` alone is case 5 of the table.
fn no_relay(relay: &RelayInput) -> Result<bool, RdyError> {
    if relay.evidence.is_some() || relay.relayed_by.is_some() {
        return Ok(false);
    }
    for (field, given) in [
        ("question", relay.question.is_some()),
        ("session_ref", relay.session_ref.is_some()),
    ] {
        if given {
            return Err(RelayRefusal::NeedsEvidence { field }.error());
        }
    }
    Ok(true)
}

/// The answer of a relayed view act: `item`, the act, and the person's words (evidence `text`).
fn answer_of(item: String, act: AnswerAct, who: &Session, relay: rdy_store::Relay) -> Answer {
    Answer {
        item,
        act,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets: vec![],
        person: who.provenance(),
        relayed_by: Some(relay.by),
        relay_evidence: Some(relay.evidence),
        evidence_kind: Some(EvidenceKind::Text),
        note: None,
    }
}

/// The stored owner of a new owner input (D-PWB82): the actor, then a supplied run, each in its
/// form; then the two fields copied as they are. The actor is never split: `agent:a/b` with no
/// run is the actor `agent:a/b`, and a run comes only from the run field.
fn owner_of(input: &OwnerInput) -> Result<Owner, RdyError> {
    crate::identity::validate_owner_actor(&input.actor)?;
    if let Some(run) = &input.run {
        crate::identity::validate_run(run)?;
    }
    Ok(Owner {
        actor: input.actor.clone(),
        run: input.run.clone(),
    })
}

impl Rdy {
    /// own1: the human reassigns `view` to `owner` (an actor and an optional run), or leaves it unowned
    /// (`None`), so that the next commit claims it. Works on a closed view and leaves it closed.
    pub fn view_own(
        &mut self,
        s: &Session,
        view: &str,
        owner: Option<&OwnerInput>,
    ) -> Result<ViewHeadInfo, RdyError> {
        if !s.is_human() {
            return Err(RdyError::new(
                E_PERMISSION,
                "only a human reassigns a view",
                RELAY_FIX,
            ));
        }
        self.check_own(s, view)?;
        let owner = owner.map(owner_of).transpose()?;
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
                RELAY_FIX,
            ));
        }
        let (kept, idle) = self.idle_views(s, days, now_ms)?;
        if idle.is_empty() {
            return Ok(StaleClose {
                closed: vec![],
                kept,
                relayed_by: None,
                answer: None,
            });
        }
        let closed = self.view_close(s, &idle, &format!("idle for more than {days} days"))?;
        Ok(StaleClose {
            closed,
            kept,
            relayed_by: None,
            answer: None,
        })
    }

    /// own1, relayed (D-KQ4CA, D-HA7YZ): `view_own` for the person `person`, whose words an
    /// agent may relay. The relay table ([`relayed`]) decides who acts.
    /// - No `evidence` and no `relayed_by`: today's rule, as `s` itself ([`Rdy::view_own`]); an
    ///   agent session is refused with the fix "relay the person's words with evidence". A
    ///   `question` or `session_ref` alone is case 5 of the table.
    /// - Otherwise the table: a human session that names a relay is refused; an agent session
    ///   needs the person's words, and names no `relayed_by` over MCP (`implicit` when it names
    ///   none) or its own actor alone at the command line (the relay records the session's
    ///   run). The hand-over then runs as the person, and one batch holds the view op and an
    ///   `ANSWER` (act `view_own`, item `own:<view>`, evidence `text`, `targets` the view op)
    ///   that holds the relay.
    /// - The new owner's actor and run are checked after the relay and the view's rules, before
    ///   any write: a refusal writes neither the view op nor the `ANSWER`.
    pub fn view_own_as(
        &mut self,
        s: &Session,
        person: &str,
        relay: &RelayInput,
        view: &str,
        owner: Option<&OwnerInput>,
    ) -> Result<OwnInfo, RdyError> {
        if no_relay(relay)? {
            return Ok(OwnInfo {
                head: self.view_own(s, view, owner)?,
                relayed_by: None,
                answer: None,
            });
        }
        let implicit = !s.is_human() && relay.relayed_by.is_none();
        let (who, by) = relayed(s, person, relay, implicit).map_err(|e| e.error())?;
        let Some(by) = by else {
            return Ok(OwnInfo {
                head: self.view_own(s, view, owner)?,
                relayed_by: None,
                answer: None,
            });
        };
        self.check_own(&who, view)?;
        let owner = owner.map(owner_of).transpose()?;
        let relayed_by = by.by.clone();
        let a = answer_of(format!("own:{view}"), AnswerAct::ViewOwn, &who, by);
        let appends = [(view.to_string(), vec![ViewOpKind::Own { owner }])];
        let (_, answer) = self
            .store
            .view_append_answered(&appends, &who.provenance(), a)
            .map_err(answer_write_error)?;
        self.publish(Delta::View {
            view: view.to_string(),
        })?;
        Ok(OwnInfo {
            head: head_info(&self.read(), view)?,
            relayed_by: Some(relayed_by),
            answer: Some(answer.to_string()),
        })
    }

    /// own1, relayed: `view_close_stale` for the person `person`, by the same table as
    /// [`Rdy::view_own_as`]. A relayed close runs as the person, so it closes what the person's
    /// own `rdy view close --stale` would, and keeps the person's default view. One batch holds
    /// every close and an `ANSWER` (act `close_stale`, item `stale:<the person's default view>`,
    /// `targets` the close ops). When nothing is idle, no view changes, and the `ANSWER` is still
    /// written alone, with no targets: the person's words are kept.
    pub fn view_close_stale_as(
        &mut self,
        s: &Session,
        person: &str,
        relay: &RelayInput,
        days: u32,
        now_ms: i64,
    ) -> Result<StaleClose, RdyError> {
        if no_relay(relay)? {
            return self.view_close_stale(s, days, now_ms);
        }
        let implicit = !s.is_human() && relay.relayed_by.is_none();
        let (who, by) = relayed(s, person, relay, implicit).map_err(|e| e.error())?;
        let Some(by) = by else {
            return self.view_close_stale(s, days, now_ms);
        };
        let relayed_by = by.by.clone();
        let (kept, idle) = self.idle_views(&who, days, now_ms)?;
        let a = answer_of(
            format!("stale:{}", who.default_view()),
            AnswerAct::CloseStale,
            &who,
            by,
        );
        if idle.is_empty() {
            // nothing to close, but the person's words are kept: an answer with no targets
            let answer = self.store.put_answer(&a).map_err(answer_write_error)?;
            self.publish(Delta::Seq)?;
            return Ok(StaleClose {
                closed: vec![],
                kept,
                relayed_by: Some(relayed_by),
                answer: Some(answer.to_string()),
            });
        }
        let (closed, answer) =
            self.view_close_answered(&who, &idle, &format!("idle for more than {days} days"), a)?;
        Ok(StaleClose {
            closed,
            kept,
            relayed_by: Some(relayed_by),
            answer: Some(answer.to_string()),
        })
    }

    /// The rules of a hand-over by `s`, before any write: a writer, and the view exists.
    fn check_own(&self, s: &Session, view: &str) -> Result<(), RdyError> {
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
        Ok(())
    }

    /// The open views idle for more than `days` days at `now_ms`, split as a stale close splits
    /// them for session `s`: `s`'s own default view (kept) and the rest.
    fn idle_views(
        &self,
        s: &Session,
        days: u32,
        now_ms: i64,
    ) -> Result<(Option<String>, Vec<String>), RdyError> {
        let own = s.default_view();
        let (kept, idle): (Vec<String>, Vec<String>) = self
            .read()
            .view_list(false, days, now_ms)?
            .into_iter()
            .filter(|v| v.idle)
            .map(|v| v.name)
            .partition(|name| *name == own);
        Ok((kept.into_iter().next(), idle))
    }
}
