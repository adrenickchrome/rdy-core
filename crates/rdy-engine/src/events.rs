//! The event stream (evt1): what a write batch did, derived from the batch's own frames, and the
//! in-process bus that fans the events out to subscribers.
use crate::RdyError;
use rdy_model::Hash;
use rdy_store::segment::Location;
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

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventKind {
    ChangeCommitted {
        change: Hash,
        view: String,
        actor: String,
        run: Option<String>,
    },
    /// Any view op other than a commit, a rebase or a close; `kind` is `create`, `fork`,
    /// `include`, `exclude`, `undo` or `own`.
    ViewOp {
        view: String,
        op: Hash,
        kind: String,
    },
    /// A landing rebased `view`; `conflicts` are the ids of the conflicts the `Rebase` op
    /// recorded, in op order.
    ViewRebased {
        view: String,
        landing: Hash,
        conflicts: Vec<Hash>,
    },
    /// A `Close` op on `view`.
    ViewClosed {
        view: String,
        reason: String,
    },
    Proposed {
        proposal: Hash,
        view: String,
    },
    Landed {
        landing: Hash,
        proposal: Hash,
        view: String,
    },
}

/// The events of one batch, in frame order. A frame whose object cannot be read is an error.
pub fn events_for_batch(
    reader: &StoreReader,
    seq: Location,
    frames: &[(u8, Hash)],
) -> Result<Vec<Event>, RdyError> {
    let mut out = Vec::new();
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
            _ => {}
        }
    }
    Ok(out)
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
