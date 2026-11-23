//! The take of a hub port (D-A3H7S, D-DZDB7, D-3JN9N, D-7AT4S): a verified bundle written into a
//! new pushed view of a holder, in one batch.
//!
//! [`Rdy::port_into_view`] runs under the target's one writer. The first fault wins, in this
//! order:
//!
//! 1. the bundle's length, then its form and hash (a [`PortBundle`] is verified when it is made);
//! 2. the family, then the same store ([`port::check_family`]);
//! 3. the author, then the view name;
//! 4. the target: a holder with a hub canon tip;
//! 5. the view-head compare: an existing view is [`PortOutcome::Behind`], with no write;
//! 6. the target removals, the op checks and the briefing ([`port::prepare_port`]).
//!
//! The batch holds the objects the target lacks, one `Create` on hub canon's tip and one
//! `Commit` per change. It moves through the checked canon move with `SetViewHead`, expected
//! head none, so the compare and the set are one durable batch. The view head has no owner and
//! no pin. Canon, tags, held refs and `meta/origin` do not move. No landing is made.
use crate::error::*;
use crate::port::{self, PortBundle, PortedChange};
use crate::{Rdy, stream_take};
use rdy_model::{BlockId, Hash, Provenance, Role};
use rdy_store::holder::StoreMode;
use rdy_store::hub_canon::{CanonMove, Moved};
use rdy_store::kinds;
use rdy_store::view::{ViewOp, ViewOpKind};
use std::collections::HashSet;

/// The run of every ported change and view op a hub carry writes.
pub const HUB_CARRY_RUN: &str = "hub-carry";

/// What a hub port did: a new pushed view, or the head the view-head compare found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortOutcome {
    /// The view is new and holds the ported changes.
    Created(PortedView),
    /// A view of that name exists: nothing was written. `head` is its current head.
    Behind { head: Option<Hash> },
}

/// A new pushed view of a hub port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortedView {
    /// The view's name.
    pub view: String,
    /// The view's first op: its `Create` on hub canon's tip.
    pub root: Hash,
    /// The view's head: its last `Commit`, or the root when no change was ported.
    pub head: Hash,
    /// Hub canon's tip at the writer's turn, where the view starts.
    pub base: Hash,
    /// Each source change with its new change, in port order.
    pub ported: Vec<PortedChange>,
    /// The blocks the new view conflicts on, sorted.
    pub conflicts: Vec<BlockId>,
}

/// Whether `author` is a person's actor: the actor form, and `human:`.
fn is_person(author: &str) -> bool {
    crate::identity::is_actor(author) && author.starts_with("human:")
}

impl Rdy {
    /// Takes `bundle` into the new pushed view `view` of this holder, authored by the person
    /// `author` at `at_ms` (see the module docs). Returns [`PortOutcome::Created`] with the new
    /// view, or [`PortOutcome::Behind`] with the view's head when the view exists. A refusal
    /// writes nothing.
    pub fn port_into_view(
        &mut self,
        bundle: &PortBundle,
        view: &str,
        author: &str,
        at_ms: i64,
    ) -> Result<PortOutcome, RdyError> {
        if bundle.to_bytes().len() > port::MAX_PORT_BUNDLE_BYTES {
            return Err(port::unverified());
        }
        self.seal_open()?;
        let st = self.read();
        port::check_family(&st, bundle)?;
        if !is_person(author) {
            return Err(port::not_a_person());
        }
        crate::write::writable_view(view)?;
        if st.reader().mode()? != StoreMode::Holder {
            return Err(port::needs_holder());
        }
        let tip = st.canon_head().ok_or_else(port::no_hub_canon)?;
        if let Some(h) = st.view_head(view) {
            return Ok(PortOutcome::Behind { head: Some(h.head) });
        }
        let by = Provenance {
            actor: author.to_string(),
            run: Some(HUB_CARRY_RUN.to_string()),
            role: Role::Writer,
            at_ms,
        };
        let prepared = port::prepare_port(&st, bundle, view, &by, &self.briefs())?;
        // the view: a Create on canon's tip, then one Commit per new change
        let op = |prev: Option<Hash>, kind: ViewOpKind| ViewOp {
            view: view.to_string(),
            prev,
            op: kind,
            actor: author.to_string(),
            run: Some(HUB_CARRY_RUN.to_string()),
            at_ms,
        };
        let mut ops: Vec<(u8, Vec<u8>)> = Vec::with_capacity(prepared.changes.len() + 1);
        let create = rdy_store::codec::item_new(
            kinds::VIEW_OP,
            &op(
                None,
                ViewOpKind::Create {
                    base: Some(tip),
                    owner: None,
                    pinned: false,
                },
            ),
        );
        let root = Hash::of(&create.1);
        let mut head = root;
        ops.push(create);
        for c in &prepared.changes {
            let commit = rdy_store::codec::item_new(
                kinds::VIEW_OP,
                &op(Some(head), ViewOpKind::Commit { change: c.id() }),
            );
            head = Hash::of(&commit.1);
            ops.push(commit);
        }
        // the objects the target lacks, each once, then the view's ops
        let reader = st.reader();
        let mut seen: HashSet<(u8, Hash)> = HashSet::new();
        let mut records: Vec<(u8, Vec<u8>)> = Vec::new();
        for (raw, payload) in prepared.object_frames()?.into_iter().chain(ops) {
            let kind = rdy_store::codec::logical_kind(raw);
            let h = Hash::of(&payload);
            if !seen.insert((kind, h)) || reader.has_kind(kind, &h)? {
                continue;
            }
            records.push((raw, payload));
        }
        let batch = reader.check_move_batch(records, head)?;
        drop(st);
        let mv = CanonMove::SetViewHead {
            name: view.to_string(),
            root,
        };
        match stream_take::move_canon(self, &batch, &mv, None, at_ms)? {
            Moved::Moved { tip: written } => Ok(PortOutcome::Created(PortedView {
                view: view.to_string(),
                root,
                head: written,
                base: tip,
                ported: prepared.ported,
                conflicts: prepared.conflicts,
            })),
            Moved::Behind { tip: found } => Ok(PortOutcome::Behind { head: found }),
        }
    }
}
