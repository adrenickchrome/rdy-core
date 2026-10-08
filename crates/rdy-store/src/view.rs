//! View chains (chain1): each view is a chain of `VIEW_OP` objects, newest first through `prev`,
//! whose head the ref `view/<name>` names in a `ViewHead`. A view's included change set is read by
//! walking back from the head to the latest checkpoint op and applying the ops after it. An
//! `Undo` is resolved to the effect it has (`ResolvedOp`) by reading its target (vop1). View ops
//! are written one per operation by `Store::{view_create, view_fork, view_append}` and the commit
//! path.
use crate::error::StoreError;
use crate::kinds;
use crate::merge::Conflict;
use crate::state::view_ref;
use crate::store::encode;
use crate::{Store, StoreReader};
use rdy_model::{Hash, Provenance};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Who a view belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    pub actor: String,
    pub run: Option<String>,
}

/// What one op does to its view. The variant order is postcard's discriminant: it is format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewOpKind {
    /// A checkpoint: the view starts empty.
    Create {
        base: Option<Hash>,
        owner: Option<Owner>,
        pinned: bool,
    },
    /// A checkpoint: the view starts with `changes`, copied from view `from` at `from_head`.
    Fork {
        from: String,
        from_head: Hash,
        base: Option<Hash>,
        changes: Vec<Hash>,
        owner: Option<Owner>,
    },
    Commit {
        change: Hash,
    },
    Include {
        changes: Vec<Hash>,
    },
    Exclude {
        changes: Vec<Hash>,
    },
    /// A checkpoint: after landing `landing`, the view holds `remaining`. `conflicts` are the
    /// conflicts the rebase found (a record; the view's current conflicts come from the merge).
    Rebase {
        landing: Hash,
        dropped: Vec<Hash>,
        remaining: Vec<Hash>,
        conflicts: Vec<Conflict>,
    },
    /// Reverses op `target` of the same chain: its effect is `target`'s inverse (`ResolvedOp`).
    Undo {
        target: Hash,
    },
    Close {
        reason: String,
    },
    Own {
        owner: Option<Owner>,
    },
}

/// One op of view `view`'s chain (kind `VIEW_OP`). Field order is format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewOp {
    pub view: String,
    pub prev: Option<Hash>,
    pub op: ViewOpKind,
    pub actor: String,
    pub run: Option<String>,
    pub at_ms: i64,
}

/// The value of the ref `view/<name>`. `len` is the size of the included set after `head`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewHead {
    pub head: Hash,
    pub base: Option<Hash>,
    pub len: u64,
    pub owner: Option<Owner>,
    pub pinned: bool,
    pub closed: bool,
    pub last_write_ms: i64,
}

/// What an op does to its view, with every `Undo` resolved to its effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedOp {
    Include(Vec<Hash>),
    Exclude(Vec<Hash>),
    Close,
    Reopen,
    SetOwner(Option<Owner>),
    Nothing,
}

/// The deepest chain of undos of undos `resolve` follows.
const MAX_UNDO_DEPTH: u32 = 64;

fn broken(name: &str, at: &Hash, why: impl std::fmt::Display) -> StoreError {
    StoreError::Decode(format!("view chain of {name:?} is broken at {at}: {why}"))
}

/// A view's included changes in order, with a membership set.
struct ChangeSet {
    list: Vec<Hash>,
    members: HashSet<Hash>,
}

impl ChangeSet {
    fn new(list: Vec<Hash>) -> Self {
        let members = list.iter().copied().collect();
        ChangeSet { list, members }
    }

    /// Appends each change not already included, in order.
    fn include(&mut self, changes: Vec<Hash>) {
        for c in changes {
            if self.members.insert(c) {
                self.list.push(c);
            }
        }
    }

    fn exclude(&mut self, changes: Vec<Hash>) {
        let gone: HashSet<Hash> = changes.into_iter().collect();
        self.list.retain(|c| !gone.contains(c));
        self.members.retain(|c| !gone.contains(c));
    }

    /// Applies a resolved op's effect on the set; the other effects leave it.
    fn apply(&mut self, effect: ResolvedOp) {
        match effect {
            ResolvedOp::Include(changes) => self.include(changes),
            ResolvedOp::Exclude(changes) => self.exclude(changes),
            _ => {}
        }
    }
}

/// What op `op_id` of view `view`'s chain does, with an `Undo` resolved through its target;
/// `depth` counts the undos followed to reach it.
pub(crate) fn resolve(
    reader: &StoreReader,
    view: &str,
    op_id: &Hash,
    depth: u32,
) -> Result<ResolvedOp, StoreError> {
    Resolver::new(reader, view).resolve(op_id, depth)
}

/// Resolves the ops of one view's chain. It remembers what it resolved and the owner after each
/// op it walked, so an undo of an undo of an `Own` reads each op once.
struct Resolver<'r> {
    reader: &'r StoreReader,
    view: &'r str,
    resolved: HashMap<Hash, ResolvedOp>,
    owner_after: HashMap<Hash, Option<Owner>>,
}

impl<'r> Resolver<'r> {
    fn new(reader: &'r StoreReader, view: &'r str) -> Self {
        Resolver {
            reader,
            view,
            resolved: HashMap::new(),
            owner_after: HashMap::new(),
        }
    }

    /// Op `op_id`, which must be on this view's chain.
    fn op(&self, op_id: &Hash) -> Result<ViewOp, StoreError> {
        let op = self.reader.view_op(op_id)?;
        if op.view != self.view {
            return Err(StoreError::Decode(format!(
                "view {:?}: op {op_id} belongs to view {:?}",
                self.view, op.view
            )));
        }
        Ok(op)
    }

    fn resolve(&mut self, op_id: &Hash, depth: u32) -> Result<ResolvedOp, StoreError> {
        if let Some(r) = self.resolved.get(op_id) {
            return Ok(r.clone());
        }
        let op = self.op(op_id)?;
        if depth > MAX_UNDO_DEPTH {
            return Err(StoreError::Decode(format!(
                "view {:?}: the undo chain at {op_id} is deeper than {MAX_UNDO_DEPTH}",
                self.view
            )));
        }
        let r = match op.op {
            ViewOpKind::Commit { change } => ResolvedOp::Include(vec![change]),
            ViewOpKind::Include { changes } => ResolvedOp::Include(changes),
            ViewOpKind::Exclude { changes } => ResolvedOp::Exclude(changes),
            ViewOpKind::Close { .. } => ResolvedOp::Close,
            ViewOpKind::Own { owner } => ResolvedOp::SetOwner(owner),
            ViewOpKind::Create { .. } | ViewOpKind::Fork { .. } => ResolvedOp::Nothing,
            ViewOpKind::Rebase { .. } => {
                return Err(StoreError::Decode(format!(
                    "view {:?}: op {op_id} is a rebase, which cannot be undone",
                    self.view
                )));
            }
            ViewOpKind::Undo { target } => {
                let undone = self.resolve(&target, depth + 1)?;
                self.inverse(&target, undone, depth)?
            }
        };
        self.resolved.insert(*op_id, r.clone());
        Ok(r)
    }

    /// The effect of undoing `target`, whose own effect is `undone`.
    fn inverse(
        &mut self,
        target: &Hash,
        undone: ResolvedOp,
        depth: u32,
    ) -> Result<ResolvedOp, StoreError> {
        Ok(match undone {
            ResolvedOp::Include(x) => ResolvedOp::Exclude(x),
            ResolvedOp::Exclude(x) => ResolvedOp::Include(x),
            ResolvedOp::Close => ResolvedOp::Reopen,
            ResolvedOp::Reopen => ResolvedOp::Close,
            ResolvedOp::SetOwner(_) => {
                let before = match self.op(target)?.prev {
                    Some(prev) => self.owner_after(&prev, depth)?,
                    None => None,
                };
                ResolvedOp::SetOwner(before)
            }
            ResolvedOp::Nothing => match self.op(target)?.op {
                ViewOpKind::Create { .. } | ViewOpKind::Fork { .. } => ResolvedOp::Close,
                _ => ResolvedOp::Nothing,
            },
        })
    }

    /// The owner in force after op `at`: the chain from its first op up to `at`, applying
    /// `Create.owner`, `Fork.owner` and every resolved `SetOwner`. Walks back iteratively to the
    /// first op (or one already walked), then forward.
    fn owner_after(&mut self, at: &Hash, depth: u32) -> Result<Option<Owner>, StoreError> {
        let mut stack: Vec<(Hash, ViewOpKind)> = vec![];
        let mut cur = Some(*at);
        let mut owner = None;
        while let Some(h) = cur {
            if let Some(o) = self.owner_after.get(&h) {
                owner = o.clone();
                break;
            }
            let op = self.op(&h)?;
            cur = op.prev;
            stack.push((h, op.op));
        }
        for (h, kind) in stack.into_iter().rev() {
            match kind {
                ViewOpKind::Create { owner: o, .. }
                | ViewOpKind::Fork { owner: o, .. }
                | ViewOpKind::Own { owner: o } => owner = o,
                ViewOpKind::Undo { .. } => {
                    if let ResolvedOp::SetOwner(o) = self.resolve(&h, depth + 1)? {
                        owner = o;
                    }
                }
                _ => {}
            }
            self.owner_after.insert(h, owner.clone());
        }
        Ok(owner)
    }
}

impl StoreReader {
    /// What an `Undo { target }` on view `view`'s chain does: the inverse of `target`'s effect,
    /// exactly what the walk resolves that undo to (so `target` is resolved one undo deep, and an
    /// undo the walk could not resolve is refused before it is written). `target` must be an op
    /// of `view` and not a `Rebase`.
    pub fn undo_effect(&self, view: &str, target: &Hash) -> Result<ResolvedOp, StoreError> {
        let mut r = Resolver::new(self, view);
        let undone = r.resolve(target, 1)?;
        r.inverse(target, undone, 0)
    }

    /// The head of view `name`'s chain, or None if the view has no ref.
    pub fn view_head(&self, name: &str) -> Result<Option<ViewHead>, StoreError> {
        self.get_ref(&format!("view/{name}"))?
            .map(|bytes| decode_head(name, &bytes))
            .transpose()
    }

    /// The view op with id `h`.
    pub fn view_op(&self, h: &Hash) -> Result<ViewOp, StoreError> {
        self.get_typed(kinds::VIEW_OP, h)
    }

    /// The ids of every op of view `name`'s chain that the index holds, in ascending hash order.
    pub fn view_op_ids(&self, name: &str) -> Result<Vec<Hash>, StoreError> {
        self.index().view_ops(name)
    }

    /// The landing that carried `change` into canon, if one has (lidx1).
    pub fn landing_of(&self, change: &Hash) -> Result<Option<Hash>, StoreError> {
        self.index().landing_of(change)
    }

    /// The changes view `name` includes, in order: the latest checkpoint's set with the ops after
    /// it applied. The set of the head last read is cached, and a later read walks back only to
    /// that head or to a checkpoint, whichever it meets first. A view with no ref is empty.
    pub fn included(&self, name: &str) -> Result<Arc<Vec<Hash>>, StoreError> {
        match self.view_head(name)? {
            Some(head) => self.included_at(name, &head),
            None => Ok(Arc::new(vec![])),
        }
    }

    /// The changes view `name` includes at `head`, a head the caller has read. Ops are
    /// content-addressed, so the answer depends on `head` alone, whatever the writer has done
    /// since: a read that needs a view's head and its set together reads the head once and passes
    /// it here, never `included`, which would read the head again.
    pub fn included_at(&self, name: &str, head: &ViewHead) -> Result<Arc<Vec<Hash>>, StoreError> {
        let cached = self.cached_set(name);
        if let Some((h, set)) = &cached
            && *h == head.head
        {
            return Ok(Arc::clone(set));
        }
        // newest first: the ops after the checkpoint (or the cached head), with their ids
        let mut pending: Vec<(Hash, ViewOpKind)> = vec![];
        let mut cur = head.head;
        let set: Vec<Hash> = loop {
            if let Some((h, set)) = &cached
                && *h == cur
            {
                break set.as_ref().clone();
            }
            let op = match self.view_op(&cur) {
                Ok(op) => op,
                Err(StoreError::NotFound(_)) => {
                    return Err(broken(name, &cur, "the op is missing"));
                }
                Err(e) => return Err(e),
            };
            if op.view != name {
                return Err(broken(
                    name,
                    &cur,
                    format!("the op belongs to view {:?}", op.view),
                ));
            }
            match op.op {
                ViewOpKind::Create { .. } => break vec![],
                ViewOpKind::Fork { changes, .. } => break changes,
                ViewOpKind::Rebase { remaining, .. } => break remaining,
                kind => {
                    pending.push((cur, kind));
                    match op.prev {
                        Some(prev) => cur = prev,
                        None => return Err(broken(name, &cur, "it ends without a checkpoint")),
                    }
                }
            }
        };
        let mut set = ChangeSet::new(set);
        for (id, kind) in pending.into_iter().rev() {
            match kind {
                ViewOpKind::Commit { change } => set.include(vec![change]),
                ViewOpKind::Include { changes } => set.include(changes),
                ViewOpKind::Exclude { changes } => set.exclude(changes),
                ViewOpKind::Close { .. } | ViewOpKind::Own { .. } => {}
                ViewOpKind::Undo { .. } => set.apply(resolve(self, name, &id, 0)?),
                ViewOpKind::Create { .. } | ViewOpKind::Fork { .. } | ViewOpKind::Rebase { .. } => {
                    unreachable!("a checkpoint ends the walk")
                }
            }
        }
        let set = Arc::new(set.list);
        self.cache_set(name, head.head, Arc::clone(&set));
        Ok(set)
    }
}

/// View `name`'s head from the value of its `view/` ref.
pub(crate) fn decode_head(name: &str, bytes: &[u8]) -> Result<ViewHead, StoreError> {
    postcard::from_bytes(bytes)
        .map_err(|e| StoreError::Decode(format!("view {name:?}: its ref is not a view head: {e}")))
}

/// A view op of `name` by `by`, after `prev`.
fn view_op(name: &str, prev: Option<Hash>, op: ViewOpKind, by: &Provenance) -> ViewOp {
    ViewOp {
        view: name.to_string(),
        prev,
        op,
        actor: by.actor.clone(),
        run: by.run.clone(),
        at_ms: by.at_ms,
    }
}

fn exists(name: &str) -> StoreError {
    StoreError::ViewExists(name.to_string())
}

impl Store {
    /// Creates view `name`: a `Create` op and its head in one write. The view must not exist.
    pub fn view_create(
        &mut self,
        name: &str,
        base: Option<Hash>,
        owner: Option<Owner>,
        pinned: bool,
        by: &Provenance,
    ) -> Result<ViewHead, StoreError> {
        if self.view_head(name)?.is_some() {
            return Err(exists(name));
        }
        let create = ViewOpKind::Create {
            base,
            owner: owner.clone(),
            pinned,
        };
        let bytes = encode(&view_op(name, None, create, by));
        let head = ViewHead {
            head: Hash::of(&bytes),
            base,
            len: 0,
            owner,
            pinned,
            closed: false,
            last_write_ms: by.at_ms,
        };
        let mut items = vec![(kinds::VIEW_OP, bytes)];
        items.extend(self.ref_items_pub(vec![(view_ref(name), encode(&head))]));
        self.write(items)?;
        Ok(head)
    }

    /// Forks view `from` (open or closed) as the new view `name`, owned by `owner`: a `Fork` op
    /// copying `from`'s base, pin and included changes. An unpinned, non-empty fork whose base is
    /// behind canon is rebased onto canon in the same write (a `Rebase` op after the `Fork`), so
    /// a fork never starts behind canon.
    pub fn view_fork(
        &mut self,
        from: &str,
        name: &str,
        owner: Option<Owner>,
        by: &Provenance,
    ) -> Result<ViewHead, StoreError> {
        let src = self
            .view_head(from)?
            .ok_or_else(|| StoreError::UnknownView(from.to_string()))?;
        if self.view_head(name)?.is_some() {
            return Err(exists(name));
        }
        let changes = self.included_at(from, &src)?.as_ref().clone();
        let fork = ViewOpKind::Fork {
            from: from.to_string(),
            from_head: src.head,
            base: src.base,
            changes: changes.clone(),
            owner: owner.clone(),
        };
        let bytes = encode(&view_op(name, None, fork, by));
        let mut head = ViewHead {
            head: Hash::of(&bytes),
            base: src.base,
            len: changes.len() as u64,
            owner,
            pinned: src.pinned,
            closed: false,
            last_write_ms: by.at_ms,
        };
        let mut items = vec![(kinds::VIEW_OP, bytes)];
        let canon = self.canon_head()?;
        if let Some(landing) = canon
            && !src.pinned
            && !changes.is_empty()
            && src.base != canon
        {
            let mut dropped = vec![];
            let mut remaining = vec![];
            for c in changes {
                if self.landing_of(&c)?.is_some() {
                    dropped.push(c);
                } else {
                    remaining.push(c);
                }
            }
            let rest = remaining
                .iter()
                .map(|c| self.change(c))
                .collect::<Result<Vec<_>, _>>()?;
            let conflicts = self.merge_onto(&self.state_of(canon)?, &rest)?.conflicts;
            head.base = canon;
            head.len = remaining.len() as u64;
            let rebase = ViewOpKind::Rebase {
                landing,
                dropped,
                remaining,
                conflicts,
            };
            let bytes = encode(&view_op(name, Some(head.head), rebase, by));
            head.head = Hash::of(&bytes);
            items.push((kinds::VIEW_OP, bytes));
        }
        items.extend(self.ref_items_pub(vec![(view_ref(name), encode(&head))]));
        self.write(items)?;
        Ok(head)
    }

    /// Appends `ops` to view `name`'s chain in one write and moves its head. `Create`, `Fork` and
    /// `Commit` are written by `view_create`, `view_fork` and the commit path, not here. An
    /// `Undo` applies `undo_effect`, so an undo of an op off this chain, or of a rebase, fails
    /// before anything is written. The first ops that can add a change to an empty unpinned view
    /// (which reads as canon) are preceded by a `Rebase` onto canon's head when its base is
    /// behind it (the float catch-up).
    pub fn view_append(
        &mut self,
        name: &str,
        ops: Vec<ViewOpKind>,
        by: &Provenance,
    ) -> Result<ViewHead, StoreError> {
        // 1. the head and the ops
        let head = self
            .view_head(name)?
            .ok_or_else(|| StoreError::UnknownView(name.to_string()))?;
        if ops.is_empty() {
            return Err(StoreError::Decode("view_append: no ops".into()));
        }
        for op in &ops {
            let variant = match op {
                ViewOpKind::Create { .. } => "Create",
                ViewOpKind::Fork { .. } => "Fork",
                ViewOpKind::Commit { .. } => "Commit",
                _ => continue,
            };
            return Err(StoreError::Decode(format!(
                "view_append: {variant} is written by view_create, view_fork or the commit path"
            )));
        }
        // 2. the view as it is
        let mut set = ChangeSet::new(self.included_at(name, &head)?.as_ref().clone());
        let mut closed = head.closed;
        let mut owner = head.owner.clone();
        let mut base = head.base;
        let mut last_write = head.last_write_ms;
        let mut prev = Some(head.head);
        // 3. the float catch-up
        let mut ops = ops;
        if !head.pinned && set.list.is_empty() {
            let canon = self.canon_head()?;
            if let Some(landing) = canon
                && canon != head.base
                && self.adds_a_change(name, &ops)?
            {
                ops.insert(
                    0,
                    ViewOpKind::Rebase {
                        landing,
                        dropped: vec![],
                        remaining: vec![],
                        conflicts: vec![],
                    },
                );
            }
        }
        // 4. one op each, applied in order
        let mut items = vec![];
        for op in ops {
            match &op {
                ViewOpKind::Include { changes } => set.include(changes.clone()),
                ViewOpKind::Exclude { changes } => set.exclude(changes.clone()),
                ViewOpKind::Close { .. } => closed = true,
                ViewOpKind::Own { owner: o } => owner = o.clone(),
                ViewOpKind::Rebase {
                    landing, remaining, ..
                } => {
                    base = Some(*landing);
                    set = ChangeSet::new(remaining.clone());
                }
                ViewOpKind::Undo { target } => match self.undo_effect(name, target)? {
                    ResolvedOp::Reopen => closed = false,
                    ResolvedOp::Close => closed = true,
                    ResolvedOp::SetOwner(o) => owner = o,
                    effect => set.apply(effect),
                },
                ViewOpKind::Create { .. } | ViewOpKind::Fork { .. } | ViewOpKind::Commit { .. } => {
                    unreachable!("refused above")
                }
            }
            if matches!(
                op,
                ViewOpKind::Include { .. }
                    | ViewOpKind::Exclude { .. }
                    | ViewOpKind::Undo { .. }
                    | ViewOpKind::Rebase { .. }
            ) {
                last_write = by.at_ms;
            }
            let bytes = encode(&view_op(name, prev, op, by));
            prev = Some(Hash::of(&bytes));
            items.push((kinds::VIEW_OP, bytes));
        }
        // 5. the new head, in the same write
        let new_head = ViewHead {
            head: prev.expect("at least one op"),
            base,
            len: set.list.len() as u64,
            owner,
            pinned: head.pinned,
            closed,
            last_write_ms: last_write,
        };
        items.extend(self.ref_items_pub(vec![(view_ref(name), encode(&new_head))]));
        self.write(items)?;
        Ok(new_head)
    }

    /// A `Close { reason }` op of view `name` after its head `prev_head`, for a caller's batch
    /// (altw1): the op's bytes, its id and the closed head (everything else kept). Nothing is
    /// written here.
    pub(crate) fn close_part(
        &self,
        name: &str,
        prev_head: &ViewHead,
        reason: &str,
        by: &Provenance,
    ) -> (Vec<u8>, Hash, ViewHead) {
        let close = ViewOpKind::Close {
            reason: reason.to_string(),
        };
        let bytes = encode(&view_op(name, Some(prev_head.head), close, by));
        let id = Hash::of(&bytes);
        let head = ViewHead {
            head: id,
            closed: true,
            ..prev_head.clone()
        };
        (bytes, id, head)
    }

    /// Whether `ops` hold an op that can add a change to view `name`: an `Include`, or an `Undo`
    /// whose effect is an include.
    fn adds_a_change(&self, name: &str, ops: &[ViewOpKind]) -> Result<bool, StoreError> {
        for op in ops {
            match op {
                ViewOpKind::Include { .. } => return Ok(true),
                ViewOpKind::Undo { target } => {
                    if matches!(self.undo_effect(name, target)?, ResolvedOp::Include(_)) {
                        return Ok(true);
                    }
                }
                _ => {}
            }
        }
        Ok(false)
    }
}
