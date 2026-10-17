//! The event stream (evt1): what a write batch did, derived from the batch's own frames, and the
//! in-process bus that fans the events out to subscribers.
use crate::RdyError;
use rdy_model::{AltSet, AltStatus, Hash, ReviewSubject, Verdict};
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::Location;
use rdy_store::transition::{SuspectMove, TensionMove};
use rdy_store::view::ViewOpKind;
use rdy_store::{StoreReader, kinds};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

/// One thing that happened. `seq` is the location just past the batch's `BATCH_COMMIT` frame;
/// `at_ms` comes from the object the event was derived from, so a replay reproduces it exactly.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Event {
    pub seq: Location,
    pub at_ms: i64,
    pub kind: EventKind,
}

/// An event's JSON names every hash in lowercase hex, as every other rdy surface does.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventKind {
    ChangeCommitted {
        #[serde(serialize_with = "hex::one")]
        change: Hash,
        view: String,
        actor: String,
        run: Option<String>,
    },
    /// Any view op other than a commit, a rebase or a close; `kind` is `create`, `fork`,
    /// `include`, `exclude`, `undo` or `own`.
    ViewOp {
        view: String,
        #[serde(serialize_with = "hex::one")]
        op: Hash,
        kind: String,
    },
    /// A landing rebased `view`; `conflicts` are the ids of the conflicts the `Rebase` op
    /// recorded, in op order.
    ViewRebased {
        view: String,
        #[serde(serialize_with = "hex::one")]
        landing: Hash,
        #[serde(serialize_with = "hex::many")]
        conflicts: Vec<Hash>,
    },
    /// A `Close` op on `view`; `set` is the alternative set whose version the same batch wrote
    /// (a choice or an abandon closing its members), by stable id.
    ViewClosed {
        view: String,
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none", serialize_with = "hex::opt")]
        set: Option<Hash>,
    },
    Proposed {
        #[serde(serialize_with = "hex::one")]
        proposal: Hash,
        view: String,
    },
    Landed {
        #[serde(serialize_with = "hex::one")]
        landing: Hash,
        #[serde(serialize_with = "hex::one")]
        proposal: Hash,
        view: String,
    },
    /// An intent's first version (cevt1); a renewal or a withdrawal emits nothing.
    IntentPosted {
        #[serde(serialize_with = "hex::one")]
        intent: Hash,
        #[serde(serialize_with = "hex::one")]
        version: Hash,
        actor: String,
        run: Option<String>,
        view: String,
        area: Option<String>,
        blocks: Vec<String>,
    },
    /// A version of alternative set `set` (by stable id). `change` is `created`, `chosen`,
    /// `abandoned`, `member_added`, `member_removed` or `updated`; `members` are the member
    /// views, in order.
    AltSetChanged {
        #[serde(serialize_with = "hex::one")]
        set: Hash,
        #[serde(serialize_with = "hex::one")]
        version: Hash,
        change: String,
        members: Vec<String>,
    },
    /// A review dismissed tension `tension` (its id when dismissed); the dismissal binds to
    /// `vkey` (FW-CH7).
    TensionDismissed {
        #[serde(serialize_with = "hex::one")]
        tension: Hash,
        #[serde(serialize_with = "hex::one")]
        vkey: Hash,
        #[serde(serialize_with = "hex::one")]
        review: Hash,
        actor: String,
        run: Option<String>,
    },
    /// A reviewer's verdict on a candidate pair (plan M2e1); `pair` is the pair key.
    ReviewSubmitted {
        #[serde(serialize_with = "hex::one")]
        review: Hash,
        #[serde(serialize_with = "hex::one")]
        pair: Hash,
        verdict: String,
        actor: String,
        run: Option<String>,
    },
    /// A tension that opened in this batch, read from the batch's `TRANSITION` record (plan
    /// M2e2, founder decision C, D-Z0EMP): the move's fields beside `event`.
    TensionOpened(TensionMove),
    /// A tension that this batch resolved (or dismissed), from the batch's record.
    TensionResolved(TensionMove),
    /// A suspect that this batch made new, from the batch's record.
    SuspectNew(SuspectMove),
    /// A suspect that this batch made gone, from the batch's record.
    SuspectGone(SuspectMove),
}

/// Hashes in an event's JSON as lowercase hex strings.
mod hex {
    use rdy_model::Hash;
    use serde::Serializer;

    pub(super) fn one<S: Serializer>(h: &Hash, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(h)
    }

    pub(super) fn opt<S: Serializer>(h: &Option<Hash>, s: S) -> Result<S::Ok, S::Error> {
        match h {
            Some(h) => s.collect_str(h),
            None => s.serialize_none(),
        }
    }

    pub(super) fn many<S: Serializer>(hs: &[Hash], s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(hs.iter().map(ToString::to_string))
    }
}

/// The events of every committed batch in the log from `from` to `upto`, in log order: what a
/// subscriber would have received had it been listening since `from`. The first batch whose
/// events cannot be read ends the walk with its error.
///
/// A frame whose payload an applied redaction removed keeps its kind, hash and place in its
/// batch: the walk reads the record walker, so a dropped frame never ends it, and an event names
/// the same seq it named before the removal. No event reads a removable payload (a block, a
/// change or a blob chunk); a reader that needs a change gets it through its residue.
pub fn replay(
    reader: &StoreReader,
    from: Location,
    upto: Location,
) -> Result<Vec<Event>, RdyError> {
    let mut out = Vec::new();
    let mut group: Vec<(u8, Hash)> = Vec::new();
    let mut failure: Option<RdyError> = None;
    let walk = reader.for_each_record_from(from, upto, |_, frame, next| {
        let (kind, hash) = frame_kind(&frame);
        if kind == kinds::BATCH_COMMIT {
            match events_for_batch(reader, next, &group) {
                Ok(events) => out.extend(events),
                Err(e) => {
                    failure = Some(e);
                    // stops the walk; the batch's own error is returned below
                    return Err(rdy_store::StoreError::Db("replay stopped".into()));
                }
            }
            group.clear();
        } else {
            group.push((kind, hash));
        }
        Ok(())
    });
    if let Some(e) = failure {
        return Err(e);
    }
    walk?;
    Ok(out)
}

/// The kind and hash of a frame the record walker hands out, present or dropped.
pub(crate) fn frame_kind(frame: &StoredFrame<'_>) -> (u8, Hash) {
    match *frame {
        StoredFrame::Present { kind, hash, .. } | StoredFrame::Dropped { kind, hash, .. } => {
            (kind, hash)
        }
    }
}

/// The events of one batch, in frame order. A frame whose object cannot be read is an error.
pub fn events_for_batch(
    reader: &StoreReader,
    seq: Location,
    frames: &[(u8, Hash)],
) -> Result<Vec<Event>, RdyError> {
    let mut out = Vec::new();
    // the set a close in this batch belongs to: the batch's alternative set version, if any
    let set = match frames.iter().find(|(k, _)| *k == kinds::ALT_SET) {
        Some((_, h)) => Some(reader.alt_set(h)?.stable_id(*h)),
        None => None,
    };
    for &(kind, hash) in frames {
        match kind {
            kinds::VIEW_OP => {
                let op = reader.view_op(&hash)?;
                let at_ms = op.at_ms;
                let ev = match op.op {
                    ViewOpKind::Commit { change } => EventKind::ChangeCommitted {
                        change,
                        view: op.view,
                        actor: op.actor,
                        run: op.run,
                    },
                    ViewOpKind::Rebase {
                        landing, conflicts, ..
                    } => EventKind::ViewRebased {
                        view: op.view,
                        landing,
                        conflicts: conflicts.iter().map(|c| c.id).collect(),
                    },
                    ViewOpKind::Close { reason } => EventKind::ViewClosed {
                        view: op.view,
                        reason,
                        set,
                    },
                    other => EventKind::ViewOp {
                        view: op.view,
                        op: hash,
                        kind: op_name(&other).to_string(),
                    },
                };
                out.push(Event {
                    seq,
                    at_ms,
                    kind: ev,
                });
            }
            kinds::PROPOSAL => {
                let p = reader.proposal(&hash)?;
                out.push(Event {
                    seq,
                    at_ms: p.provenance.at_ms,
                    kind: EventKind::Proposed {
                        proposal: hash,
                        view: p.view,
                    },
                });
            }
            kinds::LANDING => {
                let l = reader.landing(&hash)?;
                let view = reader.proposal(&l.proposal)?.view;
                out.push(Event {
                    seq,
                    at_ms: l.approved_by.at_ms,
                    kind: EventKind::Landed {
                        landing: hash,
                        proposal: l.proposal,
                        view,
                    },
                });
            }
            kinds::INTENT => {
                let i = reader.intent(&hash)?;
                if i.prev.is_none() {
                    out.push(Event {
                        seq,
                        at_ms: i.at_ms,
                        kind: EventKind::IntentPosted {
                            intent: i.stable_id(hash),
                            version: hash,
                            actor: i.actor,
                            run: i.run,
                            view: i.view,
                            area: i.area,
                            blocks: i.blocks.iter().map(ToString::to_string).collect(),
                        },
                    });
                }
            }
            kinds::ALT_SET => {
                let a = reader.alt_set(&hash)?;
                let before = match a.prev {
                    Some(p) => Some(reader.alt_set(&p)?),
                    None => None,
                };
                out.push(Event {
                    seq,
                    at_ms: a.provenance.at_ms,
                    kind: EventKind::AltSetChanged {
                        set: a.stable_id(hash),
                        version: hash,
                        change: alt_change(before.as_ref(), &a).to_string(),
                        members: a.members.iter().map(|m| m.view.clone()).collect(),
                    },
                });
            }
            kinds::REVIEW => {
                let r = reader.review(&hash)?;
                let kind = match r.subject {
                    ReviewSubject::Tension { id, vkey, .. } => EventKind::TensionDismissed {
                        tension: id,
                        vkey,
                        review: hash,
                        actor: r.provenance.actor,
                        run: r.provenance.run,
                    },
                    ReviewSubject::Pair { key, .. } => EventKind::ReviewSubmitted {
                        review: hash,
                        pair: key,
                        verdict: verdict_name(r.verdict).to_string(),
                        actor: r.provenance.actor,
                        run: r.provenance.run,
                    },
                    // a mention review (FW-CV4) is not a tension dismissal; no event names it yet
                    ReviewSubject::Mention { .. } => continue,
                };
                out.push(Event {
                    seq,
                    at_ms: r.provenance.at_ms,
                    kind,
                });
            }
            kinds::TRANSITION => {
                let rec = reader.transitions(&hash)?;
                let at_ms = rec.at_ms;
                let moves = rec
                    .resolved
                    .into_iter()
                    .map(EventKind::TensionResolved)
                    .chain(rec.opened.into_iter().map(EventKind::TensionOpened))
                    .chain(rec.gone.into_iter().map(EventKind::SuspectGone))
                    .chain(rec.new.into_iter().map(EventKind::SuspectNew));
                out.extend(moves.map(|kind| Event { seq, at_ms, kind }));
            }
            _ => {}
        }
    }
    Ok(out)
}

/// A verdict's wire name.
fn verdict_name(v: Verdict) -> &'static str {
    match v {
        Verdict::Dismiss => "dismiss",
        Verdict::Restore => "restore",
        Verdict::Tension => "tension",
        Verdict::NoTension => "no_tension",
    }
}

/// What a set version changed from `before`, its `prev` (None for a first version).
fn alt_change(before: Option<&AltSet>, now: &AltSet) -> &'static str {
    let Some(before) = before else {
        return "created";
    };
    match (&before.status, &now.status) {
        (AltStatus::Chosen { .. }, AltStatus::Chosen { .. })
        | (AltStatus::Abandoned { .. }, AltStatus::Abandoned { .. }) => {}
        (_, AltStatus::Chosen { .. }) => return "chosen",
        (_, AltStatus::Abandoned { .. }) => return "abandoned",
        _ => {}
    }
    match now.members.len().cmp(&before.members.len()) {
        std::cmp::Ordering::Greater => "member_added",
        std::cmp::Ordering::Less => "member_removed",
        std::cmp::Ordering::Equal => "updated",
    }
}

fn op_name(k: &ViewOpKind) -> &'static str {
    match k {
        ViewOpKind::Create { .. } => "create",
        ViewOpKind::Fork { .. } => "fork",
        ViewOpKind::Commit { .. } => "commit",
        ViewOpKind::Include { .. } => "include",
        ViewOpKind::Exclude { .. } => "exclude",
        ViewOpKind::Rebase { .. } => "rebase",
        ViewOpKind::Undo { .. } => "undo",
        ViewOpKind::Close { .. } => "close",
        ViewOpKind::Own { .. } => "own",
    }
}

/// An in-process fan-out. Channels are unbounded, so a publisher never waits on a slow consumer.
#[derive(Default)]
pub struct Events {
    subscribers: Mutex<Vec<Sender<Arc<Event>>>>,
}

impl Events {
    pub fn new() -> Events {
        Events::default()
    }

    pub fn subscribe(&self) -> Receiver<Arc<Event>> {
        let (tx, rx) = channel();
        self.lock().push(tx);
        rx
    }

    /// Sends `e` to every live subscriber and forgets those whose receiver was dropped.
    pub fn publish(&self, e: Arc<Event>) {
        self.lock().retain(|tx| tx.send(Arc::clone(&e)).is_ok());
    }

    #[doc(hidden)]
    pub fn subscriber_count(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Sender<Arc<Event>>>> {
        self.subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
