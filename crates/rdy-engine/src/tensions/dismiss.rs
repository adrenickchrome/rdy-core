//! Dismissal (FW-CH7): a review with verdict `Dismiss` of one tension, named by the tension's
//! vkey. A dismissal is state on a still-computed tension: nothing removes the tension from the
//! set, and a change to a side's content gives a new vkey, which no dismissal names. A dismissal
//! is never withdrawn. The vkey hashes content, not versions, so a write that gives a side the
//! same content again (a new version with identical content) keeps the dismissal.
//!
//! Who may dismiss: any writer-role session may dismiss an advisory tension, its authors
//! included. A blocking tension needs the reviewer role or a human session, of an actor that
//! wrote none of its items (another run of the same actor is not independent). On a canon side
//! every item counts: one landing can carry blocks several actors wrote. A canon version is
//! credited to the change that carried it into canon last (as of canon's head), so writing a
//! block back to an earlier text makes the writer its author; the actor whose landed change
//! deleted a block is an author of that block's absent canon item; and an item that names no
//! author fails closed. A read-time intent overlap is never dismissed: its intent resolves it.
//!
//! A dismissal binds to the vkey, which covers the severity: one made while a tension was
//! advisory does not hold once it is blocking, and the blocking tension needs its own review.
use super::set::{find_among, no_tension};
use super::{SideRef, Tension, TensionKind, TensionSeverity, TensionStatus, tension_label};
use crate::error::*;
use crate::gate::open_proposals;
use crate::session::now_ms;
use crate::{Delta, Rdy, RdyError, ReadState, Session};
use rdy_model::Hash;
use rdy_model::{Review, ReviewSubject, Role, Verdict};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;

/// The longest reason a dismissal takes, in characters (after trimming).
pub const MAX_REASON: usize = 2000;

/// What a dismissal returns: the tension (`tn-<12 hex>` and its full id), its kind and severity,
/// the review written (64 hex) and the tension's status now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DismissInfo {
    pub id: String,
    pub tension: String,
    pub kind: TensionKind,
    pub severity: TensionSeverity,
    pub review: String,
    pub status: String,
}

impl ReadState {
    /// The tension `text` names (`tn-` and a hex prefix, or the prefix alone), among the
    /// event-derived set and then, on a miss, the landing tensions of every open proposal
    /// (tgat1: a form the gate refuses with is a tension of its own, so a refusal can always be
    /// dismissed by the id it names). One id is one tension, wherever it is found. A read-time
    /// overlap at `now_ms` (read only when neither holds it) is `E_BAD_INPUT`: it is resolved
    /// through its intent, never dismissed.
    pub fn resolve_tension(&self, text: &str, now_ms: i64) -> Result<Arc<Tension>, RdyError> {
        if let Some(t) = find_among(self.tensions()?.iter(), text)? {
            return Ok(t);
        }
        let mut landing: BTreeMap<Hash, Arc<Tension>> = BTreeMap::new();
        for (_, p) in open_proposals(self)? {
            for t in self.landing_tensions(&p.view, &p.changes)? {
                landing.entry(t.id).or_insert(t);
            }
        }
        if let Some(t) = find_among(landing.values(), text)? {
            return Ok(t);
        }
        let read_time: Vec<Arc<Tension>> = self
            .intent_tensions(now_ms)?
            .into_iter()
            .map(Arc::new)
            .collect();
        if find_among(read_time.iter(), text)?.is_some() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "intent overlaps are read-time; resolve the intent instead",
                "withdraw the intent (rdy_intent {op: withdraw}) or let it expire, or move the edit into the intent's view",
            ));
        }
        Err(no_tension(text))
    }

    /// The changes that wrote canon item `block` at `version`, as of canon's head: the producer
    /// that carried the version into canon last (a block written back to an earlier text is
    /// credited to the change that wrote it back); for a block canon does not hold, every
    /// landed change that deleted it. Empty when none can be named.
    pub(crate) fn canon_item_writers(
        &self,
        block: &rdy_model::BlockId,
        version: Option<Hash>,
    ) -> Result<Vec<Hash>, RdyError> {
        let reader = self.reader();
        match version {
            Some(v) => {
                let producers = reader.producers(&v)?;
                Ok(reader
                    .producer_at(&producers, self.canon_head().as_ref())?
                    .into_iter()
                    .collect())
            }
            None => {
                let mut out = vec![];
                for d in reader.deleters(block)? {
                    if reader.landing_of(&d)?.is_some() {
                        out.push(d);
                    }
                }
                Ok(out)
            }
        }
    }

    /// Every actor who wrote an item of `t`, with the side of the item, in item order: a change
    /// side's change; for each canon item, the changes `canon_item_writers` names; an intent
    /// side's intent. A canon side groups the items one landing carried, which several actors
    /// can have written, so every item counts, not one per side. An author that cannot be read
    /// fails closed: a store error is returned, and a canon item that names no writer is
    /// refused with `E_SELF_REVIEW`.
    fn authors(&self, t: &Tension, label: &str) -> Result<Vec<(SideRef, String)>, RdyError> {
        let reader = self.reader();
        let mut out: Vec<(SideRef, String)> = vec![];
        let mut add = |side: &SideRef, actor: String| {
            if !out.iter().any(|(s, a)| s == side && *a == actor) {
                out.push((side.clone(), actor));
            }
        };
        for i in &t.involved {
            match &i.side {
                SideRef::Change(c) => add(&i.side, reader.change(c)?.provenance.actor),
                SideRef::Canon { .. } => {
                    let writers = self.canon_item_writers(&i.block, i.version)?;
                    if writers.is_empty() {
                        return Err(RdyError::new(
                            E_SELF_REVIEW,
                            format!(
                                "the author of block {} on side {} of {label} cannot be read, so no session is shown independent of it",
                                i.block,
                                i.side.short()
                            ),
                            "run rdy fsck; resolve the tension with a change instead",
                        ));
                    }
                    for c in writers {
                        add(&i.side, reader.change(&c)?.provenance.actor);
                    }
                }
                SideRef::Intent { version, .. } => add(&i.side, reader.intent(version)?.actor),
            }
        }
        Ok(out)
    }
}

impl Rdy {
    /// Dismisses tension `tension` with `reason`: writes the review and the `dismiss/<vkey>` ref,
    /// and publishes the dismissal. Checks, in order: the session may write; the reason is 1 to
    /// [`MAX_REASON`] characters after trimming; the tension resolves; it is not dismissed yet;
    /// a blocking tension needs the reviewer role or a human session, of an actor that wrote
    /// none of its items (see `authors`).
    pub fn dismiss_tension(
        &mut self,
        s: &Session,
        tension: &str,
        reason: &str,
    ) -> Result<DismissInfo, RdyError> {
        if !s.role.can_write() {
            return Err(RdyError::new(
                E_PERMISSION,
                format!("role {:?} cannot dismiss tensions", s.role),
                "start the session as a writer",
            ));
        }
        let reason = reason.trim();
        if reason.is_empty() || reason.chars().count() > MAX_REASON {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "a dismissal needs a reason of 1 to 2000 characters",
                "say in a few words why the tension needs no action",
            ));
        }
        let st = self.read();
        let t = st.resolve_tension(tension, now_ms())?;
        let label = tension_label(&t.id);
        if let TensionStatus::Dismissed(review) = st.tension_status(&t) {
            // an unreadable review still answers that the tension is dismissed
            let by = st
                .reader()
                .review(&review)
                .map_or_else(|_| "unknown actor".to_string(), |r| r.provenance.actor);
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!(
                    "tension {label} is already dismissed by review {} ({by})",
                    review.short()
                ),
                "a dismissal holds until a side changes; nothing more to do",
            ));
        }
        if t.severity == TensionSeverity::Blocking {
            if s.role != Role::Reviewer && !s.is_human() {
                return Err(RdyError::new(
                    E_PERMISSION,
                    "dismissing a blocking tension needs the reviewer role or a human session",
                    "start a session with role reviewer, or ask a human",
                ));
            }
            for (side, actor) in st.authors(&t, &label)? {
                if actor == s.actor {
                    return Err(RdyError::new(
                        E_SELF_REVIEW,
                        format!(
                            "actor {} wrote side {} of {label}; another run of the same actor is not independent",
                            s.actor,
                            side.short()
                        ),
                        "a reviewer of another actor, or a human, dismisses it",
                    ));
                }
            }
        }
        let review = Review {
            subject: ReviewSubject::Tension {
                id: t.id,
                vkey: t.vkey,
                kind: t.kind.name().to_string(),
                severity: t.severity.to_string(),
                sides: t.sides.iter().map(|side| side.label()).collect(),
                blocks: t.blocks(),
            },
            case_hash: None,
            verdict: Verdict::Dismiss,
            reason: reason.to_string(),
            findings: vec![],
            provenance: s.provenance(),
        };
        drop(st);
        let id = self.store_mut().put_dismissal(&review)?;
        self.publish(Delta::Dismissed {
            vkey: t.vkey,
            review: id,
        })?;
        Ok(DismissInfo {
            id: label,
            tension: t.id.to_string(),
            kind: t.kind,
            severity: t.severity,
            review: id.to_string(),
            status: "dismissed".to_string(),
        })
    }
}
