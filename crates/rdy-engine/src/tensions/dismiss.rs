//! Dismissal (FW-CH7): a review with verdict `Dismiss` of one tension, named by the tension's
//! vkey. A dismissal is state on a still-computed tension: nothing removes the tension from the
//! set, and a change to a side's content gives a new vkey, which no dismissal names. A dismissal
//! is never withdrawn. The vkey hashes content, not versions, so a write that gives a side the
//! same content again (a new version with identical content) keeps the dismissal.
//!
//! Who may dismiss: any writer-role session may dismiss an advisory tension, its authors
//! included. A blocking tension needs the reviewer role or a human session, of an actor that
//! wrote none of its items (another run of the same actor is not independent). On a canon side
//! every item counts: one landing can carry blocks several actors wrote, and the actor whose
//! landed change deleted a block is an author of that block's absent canon item.
//!
//! A dismissal binds to the vkey, which covers the severity: one made while a tension was
//! advisory does not hold once it is blocking, and the blocking tension needs its own review.
use super::set::resolve_among;
use super::{SideRef, Tension, TensionKind, TensionSeverity, TensionStatus, tension_label};
use crate::error::*;
use crate::session::now_ms;
use crate::{Delta, Rdy, RdyError, ReadState, Session};
use rdy_model::{Review, ReviewSubject, Role, Verdict};
use serde::Serialize;
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
    /// The tension `text` names (`tn-` and a hex prefix, or the prefix alone): among the
    /// event-derived set, then the read-time ones at `now_ms`.
    pub fn resolve_tension(&self, text: &str, now_ms: i64) -> Result<Arc<Tension>, RdyError> {
        let set = self.tensions()?;
        let read_time: Vec<Arc<Tension>> = self
            .intent_tensions(now_ms)?
            .into_iter()
            .map(Arc::new)
            .collect();
        resolve_among(set.iter().chain(read_time.iter()), text)
    }

    /// Every actor who wrote an item of `t`, with the side of the item, in item order: a change
    /// side's change; for each canon item, the change that produced its version, or, for a block
    /// canon does not hold, every landed change that deleted it (none when canon never held
    /// it); an intent side's intent. A canon side groups the items one landing carried, which
    /// several actors can have written, so every item counts, not one per side. An author that
    /// cannot be read fails closed: a store error is returned, and a canon version with no
    /// producer is refused with `E_SELF_REVIEW`.
    fn authors(&self, t: &Tension, label: &str) -> Result<Vec<(SideRef, String)>, RdyError> {
        let reader = self.reader();
        let mut out: Vec<(SideRef, String)> = vec![];
        let mut add = |side: &SideRef, actor: String| {
            if !out.iter().any(|(s, a)| s == side && *a == actor) {
                out.push((side.clone(), actor));
            }
        };
        for i in &t.involved {
            match (&i.side, i.version) {
                (SideRef::Change(c), _) => add(&i.side, reader.change(c)?.provenance.actor),
                (SideRef::Canon { .. }, Some(v)) => {
                    let Some(c) = reader.producer(&v)? else {
                        return Err(RdyError::new(
                            E_SELF_REVIEW,
                            format!(
                                "the author of block {} on side {} of {label} cannot be read, so no session is shown independent of it",
                                i.block,
                                i.side.short()
                            ),
                            "run rdy fsck; the store has a version that no change produced",
                        ));
                    };
                    add(&i.side, reader.change(&c)?.provenance.actor);
                }
                (SideRef::Canon { .. }, None) => {
                    for d in reader.deleters(&i.block)? {
                        if reader.landing_of(&d)?.is_some() {
                            add(&i.side, reader.change(&d)?.provenance.actor);
                        }
                    }
                }
                (SideRef::Intent { version, .. }, _) => add(&i.side, reader.intent(version)?.actor),
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
            let by = st.reader().review(&review)?.provenance.actor;
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
