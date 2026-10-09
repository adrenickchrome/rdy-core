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
use super::set::{ambiguous, matching, no_tension, tension_hex};
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
    /// event-derived set, the landing tensions of every open proposal (tgat1: a form the gate
    /// refuses with is a tension of its own, so a refusal can always be dismissed by the id it
    /// names) and the read-time overlaps at `now_ms`. Only a prefix that one tension matches
    /// across all three resolves (see `resolve_in`). A read-time overlap is `E_BAD_INPUT`: it
    /// is resolved through its intent, never dismissed.
    pub fn resolve_tension(&self, text: &str, now_ms: i64) -> Result<Arc<Tension>, RdyError> {
        resolve_in(
            text,
            self.tensions()?.iter(),
            || {
                let mut landing: BTreeMap<Hash, Arc<Tension>> = BTreeMap::new();
                for (_, p) in open_proposals(self)? {
                    for t in self.landing_tensions(&p.view, &p.changes)? {
                        landing.entry(t.id).or_insert(t);
                    }
                }
                Ok(landing.into_values().collect())
            },
            || {
                Ok(self
                    .intent_tensions(now_ms)?
                    .into_iter()
                    .map(Arc::new)
                    .collect())
            },
        )
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

/// The one tension `text` names among the event-derived set `event`, the `landing` forms of
/// open proposals and the `read_time` overlaps. Only a prefix that one tension matches across
/// all three lists resolves; one id is one tension, wherever it is found (the event-derived
/// form first). A full label (12 hex or more) found in one list ends the search there, so the
/// later lists are read only on a miss; a shorter prefix reads every list, so a match in a
/// later list makes it ambiguous. A read-time overlap is refused: it is resolved through its
/// intent, never dismissed.
fn resolve_in<'a>(
    text: &str,
    event: impl IntoIterator<Item = &'a Arc<Tension>>,
    landing: impl FnOnce() -> Result<Vec<Arc<Tension>>, RdyError>,
    read_time: impl FnOnce() -> Result<Vec<Arc<Tension>>, RdyError>,
) -> Result<Arc<Tension>, RdyError> {
    let Some(hex) = tension_hex(text) else {
        return Err(no_tension(text));
    };
    let full = hex.len() >= 12;
    fn add<'a>(
        found: &mut BTreeMap<Hash, Arc<Tension>>,
        list: impl IntoIterator<Item = &'a Arc<Tension>>,
        hex: &str,
    ) {
        for t in matching(list, hex) {
            found.entry(t.id).or_insert_with(|| Arc::clone(t));
        }
    }
    let mut found: BTreeMap<Hash, Arc<Tension>> = BTreeMap::new();
    add(&mut found, event, &hex);
    if !full || found.is_empty() {
        add(&mut found, &landing()?, &hex);
    }
    if !full || found.is_empty() {
        add(&mut found, &read_time()?, &hex);
    }
    let found: Vec<Arc<Tension>> = found.into_values().collect();
    match found.as_slice() {
        [] => Err(no_tension(text)),
        [t] if t.read_time => Err(read_time_refusal()),
        [t] => Ok(Arc::clone(t)),
        many => Err(ambiguous(text, many.iter())),
    }
}

/// `E_BAD_INPUT` for a read-time overlap named for dismissal.
fn read_time_refusal() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "intent overlaps are read-time; resolve the intent instead",
        "withdraw the intent (rdy_intent {op: withdraw}) or let it expire, or move the edit into the intent's view",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tension whose id starts with the bytes `head` (the rest zero, then `tail`).
    fn tn(head: &[u8], tail: u8, read_time: bool) -> Arc<Tension> {
        let mut b = [0u8; 32];
        b[..head.len()].copy_from_slice(head);
        b[31] = tail;
        Arc::new(Tension {
            id: Hash(b),
            vkey: Hash([tail; 32]),
            kind: TensionKind::ALL[0],
            severity: TensionSeverity::Advisory,
            overridden: false,
            sides: vec![],
            involved: vec![],
            summary: String::new(),
            read_time,
        })
    }

    const HEAD: &[u8] = &[0xab, 0xcd, 0xef];
    const SHORT: &str = "tn-abcdef";

    fn resolve(
        text: &str,
        event: &[Arc<Tension>],
        landing: &[Arc<Tension>],
        read_time: &[Arc<Tension>],
    ) -> Result<Arc<Tension>, RdyError> {
        resolve_in(
            text,
            event.iter(),
            || Ok(landing.to_vec()),
            || Ok(read_time.to_vec()),
        )
    }

    fn assert_ambiguous(r: Result<Arc<Tension>, RdyError>) {
        let e = r.unwrap_err();
        assert_eq!(e.code, E_NOT_FOUND, "{e}");
        assert_eq!(
            e.message,
            format!("tension {SHORT} is ambiguous (2 tensions)")
        );
    }

    #[test]
    fn tdis1_a_short_prefix_of_an_event_tension_and_a_landing_form_is_ambiguous() {
        assert_ambiguous(resolve(
            SHORT,
            &[tn(HEAD, 1, false)],
            &[tn(HEAD, 2, false)],
            &[],
        ));
    }

    #[test]
    fn tdis1_a_short_prefix_of_an_event_tension_and_a_read_time_overlap_is_ambiguous() {
        assert_ambiguous(resolve(
            SHORT,
            &[tn(HEAD, 1, false)],
            &[],
            &[tn(HEAD, 2, true)],
        ));
    }

    #[test]
    fn tdis1_a_short_prefix_of_a_landing_form_and_a_read_time_overlap_is_ambiguous() {
        assert_ambiguous(resolve(
            SHORT,
            &[],
            &[tn(HEAD, 1, false)],
            &[tn(HEAD, 2, true)],
        ));
    }

    #[test]
    fn tdis1_one_id_in_two_lists_is_one_tension() {
        let t = tn(HEAD, 1, false);
        let got = resolve(SHORT, &[Arc::clone(&t)], &[Arc::clone(&t)], &[]).unwrap();
        assert_eq!(got.id, t.id);
    }

    #[test]
    fn tdis1_a_full_label_of_an_event_tension_reads_no_other_list() {
        // M6: a full label found in the event-derived set needs no landing or read-time read
        let t = tn(HEAD, 1, false);
        let got = resolve_in(
            &tension_label(&t.id),
            [Arc::clone(&t)].iter(),
            || panic!("landing forms read"),
            || panic!("read-time overlaps read"),
        )
        .unwrap();
        assert_eq!(got.id, t.id);
    }

    #[test]
    fn tdis1_a_full_label_of_a_landing_form_reads_no_read_time_overlap() {
        let t = tn(HEAD, 1, false);
        let got = resolve_in(
            &tension_label(&t.id),
            [].iter(),
            || Ok(vec![Arc::clone(&t)]),
            || panic!("read-time overlaps read"),
        )
        .unwrap();
        assert_eq!(got.id, t.id);
    }

    #[test]
    fn tdis1_a_unique_short_prefix_resolves_and_a_read_time_one_is_refused() {
        let t = tn(HEAD, 1, false);
        let other = tn(&[0x12, 0x34, 0x56], 2, true);
        let got = resolve(SHORT, &[], &[Arc::clone(&t)], std::slice::from_ref(&other)).unwrap();
        assert_eq!(got.id, t.id);
        let e = resolve(SHORT, &[], &[], &[tn(HEAD, 3, true)]).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{e}");
        assert_eq!(
            e.message,
            "intent overlaps are read-time; resolve the intent instead"
        );
        let e = resolve("tn-abcdee", &[t], &[], &[other]).unwrap_err();
        assert_eq!(e.code, E_NOT_FOUND, "{e}");
    }
}
