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
use crate::store::{Frames, encode};
use crate::{Store, StoreReader};
use rdy_model::{Answer, Change, Hash, Op, Provenance};
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
    effects: HashMap<Hash, Effect>,
}

/// The kind of effect an op has, without what it changes: enough to tell a close from the rest
/// (`closed_after`). `Change` is an include or an exclude. `Checkpoint` is a `Create` or a
/// `Fork`: its effect is `ResolvedOp::Nothing`, and its undo closes the view. Only a checkpoint
/// resolves to `Nothing`, so no other variant is needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Effect {
    Change,
    Close,
    Reopen,
    Owner,
    Checkpoint,
}

impl Effect {
    /// The kind of effect an undo of an op with effect `self` has, as `Resolver::inverse` gives.
    fn inverse(self) -> Effect {
        match self {
            Effect::Change => Effect::Change,
            Effect::Close => Effect::Reopen,
            Effect::Reopen => Effect::Close,
            Effect::Owner => Effect::Owner,
            Effect::Checkpoint => Effect::Close,
        }
    }
}

impl<'r> Resolver<'r> {
    fn new(reader: &'r StoreReader, view: &'r str) -> Self {
        Resolver {
            reader,
            view,
            resolved: HashMap::new(),
            owner_after: HashMap::new(),
            effects: HashMap::new(),
        }
    }

    /// The kind of effect op `op_id` has, with an `Undo` followed through its targets only: the
    /// same checks as `resolve` (the op is on this chain, the undo depth, no undo of a rebase),
    /// but no owner or change set is rebuilt, so it reads the op and the ops it undoes, never the
    /// history before them. `depth` counts the undos followed to reach it.
    fn effect(&mut self, op_id: &Hash, depth: u32) -> Result<Effect, StoreError> {
        if let Some(e) = self.effects.get(op_id) {
            return Ok(*e);
        }
        let op = self.op(op_id)?;
        if depth > MAX_UNDO_DEPTH {
            return Err(StoreError::UndoTooDeep {
                view: self.view.to_string(),
                op: *op_id,
            });
        }
        let e = match op.op {
            ViewOpKind::Commit { .. } | ViewOpKind::Include { .. } | ViewOpKind::Exclude { .. } => {
                Effect::Change
            }
            ViewOpKind::Close { .. } => Effect::Close,
            ViewOpKind::Own { .. } => Effect::Owner,
            ViewOpKind::Create { .. } | ViewOpKind::Fork { .. } => Effect::Checkpoint,
            ViewOpKind::Rebase { .. } => return Err(self.undone_rebase(op_id)),
            ViewOpKind::Undo { target } => self.effect(&target, depth + 1)?.inverse(),
        };
        self.effects.insert(*op_id, e);
        Ok(e)
    }

    /// What op `op` does to whether the view is open: `Some(Effect::Close)` for a close (a
    /// `Close`, or an `Undo` that closes), `Some(Effect::Reopen)` for an op after which the view
    /// is open (a `Create`, a `Fork`, or an `Undo` that reopens), `None` for an op that leaves
    /// it as it was.
    fn open_effect(&mut self, op: &ViewOpKind) -> Result<Option<Effect>, StoreError> {
        Ok(match op {
            ViewOpKind::Close { .. } => Some(Effect::Close),
            ViewOpKind::Create { .. } | ViewOpKind::Fork { .. } => Some(Effect::Reopen),
            ViewOpKind::Undo { target } => match self.effect(target, 1)?.inverse() {
                e @ (Effect::Close | Effect::Reopen) => Some(e),
                _ => None,
            },
            _ => None,
        })
    }

    /// Op `id`, which must be on this view's chain: an op of another view is a broken chain.
    fn chain_op(&self, id: &Hash) -> Result<ViewOp, StoreError> {
        let op = self.reader.view_op(id)?;
        if op.view != self.view {
            return Err(broken(
                self.view,
                id,
                format!("the op belongs to view {:?}", op.view),
            ));
        }
        Ok(op)
    }

    fn undone_rebase(&self, op_id: &Hash) -> StoreError {
        StoreError::Decode(format!(
            "view {:?}: op {op_id} is a rebase, which cannot be undone",
            self.view
        ))
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
            return Err(StoreError::UndoTooDeep {
                view: self.view.to_string(),
                op: *op_id,
            });
        }
        let r = match op.op {
            ViewOpKind::Commit { change } => ResolvedOp::Include(vec![change]),
            ViewOpKind::Include { changes } => ResolvedOp::Include(changes),
            ViewOpKind::Exclude { changes } => ResolvedOp::Exclude(changes),
            ViewOpKind::Close { .. } => ResolvedOp::Close,
            ViewOpKind::Own { owner } => ResolvedOp::SetOwner(owner),
            ViewOpKind::Create { .. } | ViewOpKind::Fork { .. } => ResolvedOp::Nothing,
            ViewOpKind::Rebase { .. } => return Err(self.undone_rebase(op_id)),
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

/// Whether the resolver's view is closed after its op `at`: the newest op from `at` back that
/// opens or closes the view says which (`Resolver::open_effect`). A chain with none is open.
fn closed_at(resolver: &mut Resolver, at: Hash) -> Result<bool, StoreError> {
    let mut cur = at;
    loop {
        let op = resolver.chain_op(&cur)?;
        if let Some(e) = resolver.open_effect(&op.op)? {
            return Ok(e == Effect::Close);
        }
        match op.prev {
            Some(prev) => cur = prev,
            None => return Ok(false),
        }
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

    /// Whether view op `b`'s frame directly follows view op `a`'s frame in the log, so that one
    /// write wrote them, in that order: a write's frames are contiguous and a batch marker ends
    /// each write. `commit_many` writes its commits in a row; undo treats them as one (mfb3).
    /// `NotFound` for an op the index does not hold.
    pub fn view_ops_in_a_row(&self, a: &Hash, b: &Hash) -> Result<bool, StoreError> {
        let at = |h: &Hash| {
            self.index()
                .typed(kinds::VIEW_OP, h)?
                .ok_or(StoreError::NotFound(*h))
        };
        let (la, lb) = (at(a)?, at(b)?);
        // the frame's stored length, whatever its encoding
        let Some((payload, _, _)) = self.segments().header(la)? else {
            return Err(StoreError::NotFound(*a));
        };
        let len = crate::segment::HEADER + u64::from(payload);
        Ok(la.segment == lb.segment && la.offset + len == lb.offset)
    }

    /// The ids of every op of view `name`'s chain that the index holds, in ascending hash order.
    pub fn view_op_ids(&self, name: &str) -> Result<Vec<Hash>, StoreError> {
        self.index().view_ops(name)
    }

    /// The refusal of a write that copies `change`: `Redacted` (naming the request) or
    /// `ProjectedWrite` when it is a read projection or embeds a projected block version, as a
    /// write of it would be refused. A replay checks its sources with it before it prepares
    /// anything.
    pub fn refuse_copy_of(&self, change: &Change) -> Result<(), StoreError> {
        self.refuse_projection(change)
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
        self.set_at(name, head.head, true)
    }

    /// The changes view `name` included just before the log took the object of kind `kind` with
    /// hash `object` (msw20): the set at the newest op of `name`'s chain that the log holds
    /// before it. The mark is that kind's frame, never another kind's frame with the same bytes.
    /// The walk goes back from the head. A view with no op before the mark (or no ref) is empty.
    /// The object must be indexed as `kind` (`NotFound`). The read never reads or writes the
    /// per-view set cache, so a past set never serves a later `included`.
    pub fn included_before(
        &self,
        name: &str,
        kind: u8,
        object: &Hash,
    ) -> Result<Arc<Vec<Hash>>, StoreError> {
        let mark = self
            .index()
            .typed(kind, object)?
            .ok_or(StoreError::NotFound(*object))?;
        let Some(head) = self.view_head(name)? else {
            return Ok(Arc::new(vec![]));
        };
        let mut cur = head.head;
        loop {
            let at = self
                .index()
                .typed(kinds::VIEW_OP, &cur)?
                .ok_or_else(|| broken(name, &cur, "the op is missing"))?;
            if at < mark {
                return self.set_at(name, cur, false);
            }
            let op = self.view_op(&cur)?;
            if op.view != name {
                return Err(broken(
                    name,
                    &cur,
                    format!("the op belongs to view {:?}", op.view),
                ));
            }
            match op.prev {
                Some(prev) => cur = prev,
                None => return Ok(Arc::new(vec![])),
            }
        }
    }

    /// Whether view `name` was closed when the log took the object of kind `kind` with hash
    /// `object`, or its chain took a close after it (D-K0SS1; ruling final: a version proposed
    /// into a closed view stays withdrawn after the close is undone). A close after it is an op
    /// of the chain from `head` back, written after that object, whose effect is
    /// `ResolvedOp::Close` (a `Close`, or an `Undo` that closes again). Closed when the log took
    /// it: the newest op before the object that opens or closes the view closes it. The mark is
    /// that kind's frame, never another kind's frame with the same bytes. The walk goes back
    /// from `head` and stops at the first op the log holds before the mark. It reads the ops
    /// before the mark only when an op after it reopens the view: with none, the view was at the
    /// mark as it is at `head`, and a caller reads a closed `head` as closed itself. An `Undo`
    /// is followed through its targets only, with the checks `resolve` makes. It reads no clock.
    /// The object must be indexed (`NotFound`); an op the index lacks is a broken chain.
    pub fn closed_after(
        &self,
        name: &str,
        head: &Hash,
        kind: u8,
        object: &Hash,
    ) -> Result<bool, StoreError> {
        let mark = self
            .index()
            .typed(kind, object)?
            .ok_or(StoreError::NotFound(*object))?;
        let mut resolver = Resolver::new(self, name);
        let mut reopened = false;
        let mut cur = *head;
        loop {
            let at = self
                .index()
                .typed(kinds::VIEW_OP, &cur)?
                .ok_or_else(|| broken(name, &cur, "the op is missing"))?;
            if at < mark {
                return if reopened {
                    closed_at(&mut resolver, cur)
                } else {
                    Ok(false)
                };
            }
            let op = resolver.chain_op(&cur)?;
            match resolver.open_effect(&op.op)? {
                Some(Effect::Close) => return Ok(true),
                Some(_) => reopened = true,
                None => {}
            }
            match op.prev {
                Some(prev) => cur = prev,
                None => return Ok(false),
            }
        }
    }

    /// The changes view `name` includes at op `head`: the walk back to a checkpoint, then the
    /// ops after it applied. With `cached`, the walk also stops at the cached head, and the
    /// answer becomes the cached set; without it, the cache is neither read nor written.
    fn set_at(&self, name: &str, head: Hash, cached: bool) -> Result<Arc<Vec<Hash>>, StoreError> {
        let cache = if cached { self.cached_set(name) } else { None };
        if let Some((h, set)) = &cache
            && *h == head
        {
            return Ok(Arc::clone(set));
        }
        // newest first: the ops after the checkpoint (or the cached head), with their ids
        let mut pending: Vec<(Hash, ViewOpKind)> = vec![];
        let mut cur = head;
        let set: Vec<Hash> = loop {
            if let Some((h, set)) = &cache
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
        if cached {
            self.cache_set(name, head, Arc::clone(&set));
        }
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
        let (kind, bytes) =
            crate::codec::item_new(kinds::VIEW_OP, &view_op(name, None, create, by));
        let head = ViewHead {
            head: Hash::of(&bytes),
            base,
            len: 0,
            owner,
            pinned,
            closed: false,
            last_write_ms: by.at_ms,
        };
        let mut items = vec![(kind, bytes)];
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
        let (kind, bytes) = crate::codec::item_new(kinds::VIEW_OP, &view_op(name, None, fork, by));
        let mut head = ViewHead {
            head: Hash::of(&bytes),
            base: src.base,
            len: changes.len() as u64,
            owner,
            pinned: src.pinned,
            closed: false,
            last_write_ms: by.at_ms,
        };
        let mut items = vec![(kind, bytes)];
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
            let merged = self.merge_onto(canon, &self.state_of(canon)?, &rest)?;
            // the rebase combines the view's moves with canon's: refuse a parent loop
            let touched = rest
                .iter()
                .flat_map(|c| c.ops.iter().map(|op| op.block_id().clone()));
            if let Some((a, b)) = self.parent_cycle_in(&merged.state, touched)? {
                return Err(StoreError::ParentCycle {
                    message: format!(
                        "forking view {from:?} onto canon would make {a} and {b} each other's ancestors"
                    ),
                    fix: format!(
                        "fork a view that does not move them, or move one of them out of the other in view {from} first"
                    ),
                });
            }
            let conflicts = merged.conflicts;
            head.base = canon;
            head.len = remaining.len() as u64;
            let rebase = ViewOpKind::Rebase {
                landing,
                dropped,
                remaining,
                conflicts,
            };
            let (kind, bytes) =
                crate::codec::item_new(kinds::VIEW_OP, &view_op(name, Some(head.head), rebase, by));
            head.head = Hash::of(&bytes);
            items.push((kind, bytes));
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
        let (items, new_head, _) = self.view_append_frames(name, ops, by)?;
        self.write(items)?;
        Ok(new_head)
    }

    /// `view_append` for several views, with the answer `a` that holds a person's relay in the
    /// same batch (O14, D-DGCH7): every view's ops and head, then `a` with `targets` set to the
    /// view ops written, and its ref. Each view passes the checks of `view_append` before
    /// anything is written. Refuses, with nothing written, an empty list, a view named twice and
    /// an answer that breaks a rule of the record. Returns each view's new head, in the given
    /// order, and the answer's id.
    pub fn view_append_answered(
        &mut self,
        appends: &[(String, Vec<ViewOpKind>)],
        by: &Provenance,
        a: Answer,
    ) -> Result<(Vec<ViewHead>, Hash), StoreError> {
        if appends.is_empty() {
            return Err(StoreError::Decode("view_append_answered: no views".into()));
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut items = vec![];
        let mut heads = vec![];
        let mut targets = vec![];
        for (name, ops) in appends {
            if !seen.insert(name.as_str()) {
                return Err(StoreError::Decode(format!(
                    "view_append_answered: view {name:?} is named twice"
                )));
            }
            let (frames, head, written) = self.view_append_frames(name, ops.clone(), by)?;
            items.extend(frames);
            heads.push(head);
            targets.extend(written);
        }
        let a = Answer { targets, ..a };
        Ok((heads, self.put_answer_with(&a, items)?))
    }

    /// The frames `view_append` writes for view `name`, its new head, and the ids of the view
    /// ops among them. Nothing is written here.
    fn view_append_frames(
        &self,
        name: &str,
        ops: Vec<ViewOpKind>,
        by: &Provenance,
    ) -> Result<(Frames, ViewHead, Vec<Hash>), StoreError> {
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
        let mut written = vec![];
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
            let (kind, bytes) =
                crate::codec::item_new(kinds::VIEW_OP, &view_op(name, prev, op, by));
            prev = Some(Hash::of(&bytes));
            written.extend(prev);
            items.push((kind, bytes));
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
        Ok((items, new_head, written))
    }

    /// Stores `changes`, each prepared by the caller as a replay (`links.replayed_from` set, its
    /// deps final), and one `Include` of them all into view `name`, in one write: one batch, so
    /// the `Include` is the one op that undoes the whole replay. The deps are kept as given, never
    /// recomputed: each must name an earlier change of `changes` or a stored change. The view
    /// must exist and be open. The float catch-up of `view_append` applies. Refused with nothing
    /// written: no changes, a change that is no replay or that is a read projection (or embeds
    /// one), the same change twice, and a dep the store lacks. Returns the `Include`'s id (the
    /// view's new head) and the changes' ids, in order.
    pub fn view_replay_prepared(
        &mut self,
        name: &str,
        changes: &[Change],
        by: &Provenance,
    ) -> Result<(Hash, Vec<Hash>), StoreError> {
        if changes.is_empty() {
            return Err(StoreError::Decode(
                "view_replay_prepared: no changes".into(),
            ));
        }
        let head = self
            .view_head(name)?
            .ok_or_else(|| StoreError::UnknownView(name.to_string()))?;
        if head.closed {
            return Err(StoreError::ViewClosed(name.to_string()));
        }
        let mut items: Frames = vec![];
        let mut ids: Vec<Hash> = vec![];
        let mut seen: HashSet<Hash> = HashSet::new();
        for change in changes {
            self.refuse_projection(change)?;
            if change.links.replayed_from.is_none() {
                return Err(StoreError::Decode(
                    "view_replay_prepared: a change is no replay".into(),
                ));
            }
            for d in &change.deps {
                if !seen.contains(d) && !self.has_kind(kinds::CHANGE, d)? {
                    return Err(StoreError::NotFound(*d));
                }
            }
            for op in &change.ops {
                if let Op::Create { block } | Op::Update { block, .. } = op {
                    items.push(crate::codec::item_new(kinds::BLOCK, block));
                }
            }
            let item = crate::codec::item(kinds::CHANGE, change)?;
            let id = Hash::of(&item.1);
            if id != change.id() {
                return Err(StoreError::Decode(format!(
                    "view_replay_prepared: change {} encodes as {id}",
                    change.id()
                )));
            }
            if !seen.insert(id) {
                return Err(StoreError::Decode(format!(
                    "change {id} is committed twice in one write"
                )));
            }
            ids.push(id);
            items.push(item);
        }
        let (frames, new_head, _) = self.view_append_frames(
            name,
            vec![ViewOpKind::Include {
                changes: ids.clone(),
            }],
            by,
        )?;
        items.extend(frames);
        self.write(items)?;
        Ok((new_head.head, ids))
    }

    /// A `Close { reason }` op of view `name` after its head `prev_head`, for a caller's batch
    /// (altw1): the op's frame (its raw kind and bytes), its id and the closed head (everything
    /// else kept). Nothing is written here.
    pub(crate) fn close_part(
        &self,
        name: &str,
        prev_head: &ViewHead,
        reason: &str,
        by: &Provenance,
    ) -> ((u8, Vec<u8>), Hash, ViewHead) {
        let close = ViewOpKind::Close {
            reason: reason.to_string(),
        };
        let item = crate::codec::item_new(
            kinds::VIEW_OP,
            &view_op(name, Some(prev_head.head), close, by),
        );
        let id = Hash::of(&item.1);
        let head = ViewHead {
            head: id,
            closed: true,
            ..prev_head.clone()
        };
        (item, id, head)
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
