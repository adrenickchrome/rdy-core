use crate::error::StoreError;
use crate::kinds;
use crate::land::LandRequest;
use crate::merge::{Conflict, MergeChange, Merged, merge_indexed};
use crate::store::{Frames, encode};
use crate::view::{Owner, ViewHead, ViewOp, ViewOpKind};
use crate::{Store, StoreReader};
use rdy_model::assessment::Assessment;
use rdy_model::plan::Plan;
use rdy_model::signature::Signature;
use rdy_model::{
    AltChoice, Answer, Block, BlockId, Change, Hash, LandingRecord, Op, Proposal, Provenance,
    StateSnapshot, Tag, decision,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// Whether `a` and `b` are two versions of one block that differ only by their order key (a
/// redacted projection never does: its read identity is its own).
fn differs_only_in_order(a: &Block, b: &Block) -> bool {
    a.order != b.order
        && a.read_identity.is_none()
        && b.read_identity.is_none()
        && Block {
            order: b.order.clone(),
            ..a.clone()
        } == *b
}

/// The order-only edits `StoreReader::order_only_pairs` found: (canon's version, op base, op's
/// new version).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OrderOnlyEdits(BTreeSet<(Hash, Hash, Hash)>);

impl OrderOnlyEdits {
    /// Whether `op` is an order-only edit against canon's version `v` (`merge::merge`'s
    /// `order_only`).
    pub fn holds(&self, v: &Hash, op: &Op) -> bool {
        match op {
            Op::Update { base, block } => self.0.contains(&(*v, *base, block.version_hash())),
            _ => false,
        }
    }
}

/// The frames of one view commit (`Store::commit_parts`): the changes' frames, then the view ops
/// and the head ref, and each change's id with the position of its `CHANGE` frame in `changes`.
pub(crate) struct CommitParts {
    pub(crate) changes: Frames,
    pub(crate) views: Frames,
    pub(crate) placed: Vec<(Hash, usize)>,
}

/// What `Store::view` returns: a view's base landing and the changes it includes, in order. It is
/// not stored; the view's chain and its `ViewHead` are (chain1).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewRecord {
    pub base: Option<Hash>,
    pub changes: Vec<Hash>,
}

pub(crate) fn view_ref(name: &str) -> String {
    format!("view/{name}")
}

/// The ref naming the latest version of the proposal whose first version is `stable`.
pub(crate) fn proposal_ref(stable: &Hash) -> String {
    format!("proposal/{stable}")
}

/// The ref naming the stable id of view `view`'s open proposal (empty when it has none).
pub(crate) fn open_ref(view: &str) -> String {
    format!("open/{view}")
}

/// The hash a proposal version is stored under: its frame's, in the PROPOSAL kind's latest
/// encoding.
fn proposal_hash(p: &Proposal) -> Hash {
    crate::codec::encode_new(kinds::PROPOSAL, p).hash()
}

fn tag_ref(name: &str) -> String {
    format!("tag/{name}")
}

pub fn apply_op(store: &StoreReader, state: &mut StateSnapshot, op: &Op) -> Result<(), StoreError> {
    match op {
        Op::Create { block } => {
            if state.blocks.contains_key(&block.id) {
                return Err(StoreError::AlreadyExists(block.id.clone()));
            }
            state.blocks.insert(block.id.clone(), block.version_hash());
        }
        Op::Update { base, block } => {
            check_base(store, state, &block.id, *base, Some(block))?;
            state.blocks.insert(block.id.clone(), block.version_hash());
        }
        Op::Delete { base, id } => {
            check_base(store, state, id, *base, None)?;
            state.blocks.remove(id);
        }
    }
    Ok(())
}

/// The op's base is the state's version of `id`, and not a frozen decision, unless the op is an
/// update to `new` that changes only tracking fields of it (trk1).
fn check_base(
    store: &StoreReader,
    state: &StateSnapshot,
    id: &BlockId,
    base: Hash,
    new: Option<&Block>,
) -> Result<(), StoreError> {
    let actual = state.blocks.get(id).copied();
    if actual != Some(base) {
        return Err(StoreError::StaleBase {
            id: id.clone(),
            expected: base,
            actual,
        });
    }
    if frozen_for(store, &store.get_block(&base)?, new) {
        return Err(StoreError::Frozen(id.clone()));
    }
    Ok(())
}

/// Whether an op on `base` (an update to `new`, or a delete when `None`) is refused because
/// `base` is a frozen decision: an update that changes only `store`'s tracking fields passes, so
/// does a re-affirmation (only `reaffirmed` and pin hashes, D-EJBPD), and so does one that leaves
/// the block as it is (cf1: the landed side of a conflict, taken).
fn frozen_for(store: &StoreReader, base: &Block, new: Option<&Block>) -> bool {
    decision::is_frozen(base)
        && !new.is_some_and(|n| {
            n == base
                || decision::only_tracking_changed(base, n, store.tracking())
                || decision::only_reaffirmation_changed(base, n)
        })
}

impl StoreReader {
    pub fn canon_head(&self) -> Result<Option<Hash>, StoreError> {
        self.get_ref("canon")?
            .map(|b| crate::index::hash_from(&b, "canon ref"))
            .transpose()
    }

    pub fn snapshot_at(&self, landing: &Hash) -> Result<StateSnapshot, StoreError> {
        self.get(&self.landing(landing)?.state)
    }

    /// A parent loop in `state` reachable from `starts`: two blocks on it, each the other's
    /// ancestor (`merge::find_parent_cycle`, each block's parent read from its version).
    pub fn parent_cycle_in(
        &self,
        state: &StateSnapshot,
        starts: impl IntoIterator<Item = BlockId>,
    ) -> Result<Option<(BlockId, BlockId)>, StoreError> {
        crate::merge::find_parent_cycle(starts, |id| match state.blocks.get(id) {
            Some(v) => Ok(self.get_block(v)?.parent),
            None => Ok(None),
        })
    }

    /// The order-only edits of `ops` against `base` (`merge::merge`'s `order_only`): each update
    /// whose base is not `base`'s version of its block and that `order_only_edit` accepts. A
    /// version this cannot read makes no entry, so the merge keeps its conflict.
    pub fn order_only_pairs<'a>(
        &self,
        base: &StateSnapshot,
        ops: impl IntoIterator<Item = &'a Op>,
    ) -> OrderOnlyEdits {
        let mut seen: BTreeSet<(Hash, Hash, Hash)> = BTreeSet::new();
        let mut out = BTreeSet::new();
        for op in ops {
            let Op::Update { base: e, block } = op else {
                continue;
            };
            let Some(v) = base.blocks.get(&block.id) else {
                continue;
            };
            let key = (*v, *e, block.version_hash());
            if !seen.insert(key) {
                continue;
            }
            if self.order_only_edit(v, e, block) {
                out.insert(key);
            }
        }
        OrderOnlyEdits(out)
    }

    /// Whether an update from version `e` to `new` is an order-only edit against canon's version
    /// `v`: `v` differs from `e` only by its order key, and `new` keeps `e`'s parent and order key
    /// (D-TX3KH, D-1FT5Y; final ruling round 4). A version this cannot read, or a redacted one,
    /// never makes one. Only such an edit applies with no conflict, and only its landing keeps
    /// canon's place: a move, a parent change or a re-key is a choice of its own. The merge, the
    /// edit-collision detector and the landing all ask this.
    pub fn order_only_edit(&self, v: &Hash, e: &Hash, new: &Block) -> bool {
        v != e
            && matches!(
                (self.get_block(v), self.get_block(e)),
                (Ok(a), Ok(b)) if differs_only_in_order(&a, &b)
                    && new.parent == b.parent
                    && new.order == b.order
            )
    }

    /// The state landing `landing` recorded; the empty state for None.
    pub fn state_of(&self, landing: Option<Hash>) -> Result<StateSnapshot, StoreError> {
        match landing {
            Some(l) => self.snapshot_at(&l),
            None => Ok(StateSnapshot::default()),
        }
    }

    pub fn canon_state(&self) -> Result<StateSnapshot, StoreError> {
        self.state_of(self.canon_head()?)
    }

    /// View `name`'s base and included changes. A view with no head floats on canon, empty.
    pub fn view(&self, name: &str) -> Result<ViewRecord, StoreError> {
        // one head read: the base and the set must describe the same head
        match self.view_head(name)? {
            Some(head) => Ok(ViewRecord {
                base: head.base,
                changes: self.included_at(name, &head)?.as_ref().clone(),
            }),
            None => Ok(ViewRecord {
                base: self.canon_head()?,
                changes: vec![],
            }),
        }
    }

    /// Merges `changes` onto `base`, the state of landing `at` (merge1): deps first, then change
    /// id, with conflicts as data. A block `base` lacks reads as removed by the delete that
    /// removed it as of `at` (`deleters_at`).
    pub fn merge_onto(
        &self,
        at: Option<Hash>,
        base: &StateSnapshot,
        changes: &[Change],
    ) -> Result<Merged, StoreError> {
        Ok(self.merge_onto_indexed(at, base, changes)?.0)
    }

    /// `merge_onto`, with each applied change's position in `changes` (see `merge_indexed`).
    fn merge_onto_indexed(
        &self,
        at: Option<Hash>,
        base: &StateSnapshot,
        changes: &[Change],
    ) -> Result<(Merged, Vec<usize>), StoreError> {
        let input: Vec<MergeChange<'_>> = changes
            .iter()
            .map(|c| MergeChange {
                id: c.id(),
                deps: &c.deps,
                ops: &c.ops,
                is_replay: c.links.replayed_from.is_some(),
            })
            .collect();
        let deleters = self.merge_deleters(
            at.as_ref(),
            base,
            changes
                .iter()
                .flat_map(|c| c.ops.iter().map(|op| op.block_id())),
        )?;
        // the producer of each base version an op touches, read before the merge, so a read
        // error fails the merge: read as "no producer", it would settle canon's side
        let mut producers: HashMap<Hash, Option<Hash>> = HashMap::new();
        for op in changes.iter().flat_map(|c| c.ops.iter()) {
            if let Some(v) = base.blocks.get(op.block_id())
                && !producers.contains_key(v)
            {
                producers.insert(*v, self.producer(v)?);
            }
        }
        let order_only = self.order_only_pairs(base, changes.iter().flat_map(|c| &c.ops));
        Ok(merge_indexed(
            base,
            &input,
            &|v| producers.get(v).copied().flatten(),
            &|b| deleters.get(b).copied(),
            &|v, op| order_only.holds(v, op),
        ))
    }

    /// View `name`'s included changes merged onto its base (canon's head when it has no head).
    pub fn view_merge(&self, name: &str) -> Result<Merged, StoreError> {
        // one head read: a landing between two reads would pair the old base with the rebased
        // (emptied) set and drop the landed changes
        let (base, included) = match self.view_head(name)? {
            Some(head) => (head.base, self.included_at(name, &head)?),
            None => (self.canon_head()?, Arc::new(vec![])),
        };
        let changes = included
            .iter()
            .map(|c| self.change(c))
            .collect::<Result<Vec<_>, _>>()?;
        self.merge_onto(base, &self.state_of(base)?, &changes)
    }

    pub fn view_state(&self, name: &str) -> Result<StateSnapshot, StoreError> {
        Ok(self.view_merge(name)?.state)
    }

    /// `merge_onto(canon, changes)` with the landing checks: the first conflict (block order) is
    /// `StaleBase` naming its base and canon's version, `DeletedSince` when its base is none, canon
    /// lacks the block and a side is a delete (a re-create of an id canon removed after the
    /// writer's base), or
    /// `AlreadyExists` when its base is none;
    /// an op whose base is a frozen decision is `Frozen`. The one mapping the store's `land`, the
    /// engine's gate and atomic landings share. With the merge, it returns each applied change's
    /// position in `changes`, in the order applied (see `merge_indexed`).
    pub fn merge_checked(
        &self,
        canon: &StateSnapshot,
        changes: &[Change],
    ) -> Result<(Merged, Vec<usize>), StoreError> {
        let (merged, at) = self.merge_onto_indexed(self.canon_head()?, canon, changes)?;
        if let Some(c) = merged.conflicts.first() {
            return Err(match c.base {
                Some(base) => StoreError::StaleBase {
                    id: c.block.clone(),
                    expected: base,
                    actual: canon.blocks.get(&c.block).copied(),
                },
                None if !canon.blocks.contains_key(&c.block)
                    && c.sides.iter().any(|s| s.version.is_none()) =>
                {
                    StoreError::DeletedSince(c.block.clone())
                }
                None => StoreError::AlreadyExists(c.block.clone()),
            });
        }
        for &i in &at {
            for op in &changes[i].ops {
                let new = match op {
                    Op::Update { block, .. } => Some(block),
                    _ => None,
                };
                if let Some(base) = op.base()
                    && frozen_for(self, &self.get_block(&base)?, new)
                {
                    return Err(StoreError::Frozen(op.block_id().clone()));
                }
            }
        }
        Ok((merged, at))
    }

    pub fn landings(&self) -> Result<Vec<(Hash, LandingRecord)>, StoreError> {
        let mut out = vec![];
        let mut cur = self.canon_head()?;
        while let Some(h) = cur {
            let rec = self.landing(&h)?;
            cur = rec.prev;
            out.push((h, rec));
        }
        Ok(out)
    }

    pub fn blocks(&self, state: &StateSnapshot) -> Result<BTreeMap<BlockId, Block>, StoreError> {
        state
            .blocks
            .iter()
            .map(|(id, v)| Ok((id.clone(), self.get_block(v)?)))
            .collect()
    }

    pub fn read_tag(&self, h: &Hash) -> Result<Tag, StoreError> {
        self.get_named(kinds::TAG, "tag", h)
    }

    /// Every `tag/*` ref as (name, tag hash, tag), in ref-name order.
    pub fn tags(&self) -> Result<Vec<(String, Hash, Tag)>, StoreError> {
        self.refs_with_prefix("tag/")?
            .into_iter()
            .map(|(name, value)| {
                let h = crate::index::hash_from(&value, "tag ref")?;
                let name = name.strip_prefix("tag/").unwrap_or(&name).to_string();
                Ok((name, h, self.read_tag(&h)?))
            })
            .collect()
    }
}

impl Store {
    /// Commits `change` into view `name`, checked against what the view reads: its state, or
    /// canon's for an empty unpinned view (the floating read rule), so the check agrees with the
    /// float catch-up that moves such a view onto canon in the same batch.
    pub fn commit_to_view(&mut self, name: &str, change: Change) -> Result<Hash, StoreError> {
        let state = match self.view_head(name)? {
            Some(h) if !h.pinned && h.len == 0 => self.canon_state()?,
            _ => self.view_state(name)?,
        };
        self.commit_to_view_at(name, change, &state)
    }

    /// Engine-only fast path: like `commit_to_view`, but checks the change against `current`, the
    /// caller's copy of the view's present state, instead of replaying every change in the view
    /// (O(view length)). `current` MUST equal `view_state(name)`; a stale or wrong `current` lets
    /// invalid changes through. Callers that cannot guarantee that must use `commit_to_view`.
    #[doc(hidden)]
    pub fn commit_to_view_at(
        &mut self,
        name: &str,
        change: Change,
        current: &StateSnapshot,
    ) -> Result<Hash, StoreError> {
        self.commit_to_view_with(name, change, &|id| current.blocks.get(id).copied())
    }

    /// `commit_to_view_at` with the view's present state read through `current`: the version of
    /// a block id in the view now, or None if the view has no such block. The same contract:
    /// `current` MUST answer as the view reads, which is `view_state(name)` except for an empty
    /// unpinned view, which reads as canon (the floating read rule): there `current` MUST answer
    /// as `canon_state()`. When the float catch-up fires, the store enforces this on the blocks
    /// the change writes (see `commit_to_view_resolving`).
    pub fn commit_to_view_with(
        &mut self,
        name: &str,
        change: Change,
        current: &dyn Fn(&BlockId) -> Option<Hash>,
    ) -> Result<Hash, StoreError> {
        self.commit_to_view_resolving(name, change, current, &[], None)
    }

    /// `commit_to_view_with`, aware of the view's recorded conflicts and of who commits (res1):
    /// - an `Update` or `Delete` of a block with a conflict in `conflicts` may name the conflict's
    ///   base or any side's version as its base, and the change depends on every side, so it
    ///   resolves the conflict; other ops check against `current` as ever. When the conflict's
    ///   base or any side is a frozen decision the op is `Frozen`: resolving it would change or
    ///   drop an accepted or rejected decision;
    /// - a closed view refuses the commit;
    /// - `claim` is the committer's owner: it owns a view the commit creates, and an `Own` op
    ///   before the `Commit` claims an existing unowned view;
    /// - the first commit into an empty unpinned view (which reads as canon) is preceded by a
    ///   `Rebase` onto canon's head when its base is behind it (the float catch-up). The ops were
    ///   checked against `current`, so `current` must answer as canon there: for every block the
    ///   change writes, a `current` that differs from canon's version is refused (`StaleBase`
    ///   naming the op's base and canon's version, or `AlreadyExists` for a create), since the
    ///   write was based on a state the view no longer reads.
    pub fn commit_to_view_resolving(
        &mut self,
        name: &str,
        change: Change,
        current: &dyn Fn(&BlockId) -> Option<Hash>,
        conflicts: &[Conflict],
        claim: Option<Owner>,
    ) -> Result<Hash, StoreError> {
        let committer = change.provenance.clone();
        let ids = self.commit_many(name, vec![change], current, conflicts, claim, &committer)?;
        Ok(ids[0])
    }

    /// `commit_to_view_resolving` for several changes in one durable write, all or nothing
    /// (mfb3: an import writes one change per author). Every change is checked against
    /// `current`, the view's state before the write, so the changes must write disjoint blocks: a
    /// block in two of them is `Conflict` naming it, and the same change twice is refused, before
    /// anything is written. Each change keeps its own provenance. The view ops the write adds
    /// (the implicit create, the float catch-up, the claim and one `Commit` per change, in
    /// order) carry `committer`'s actor, run and time, the implicit create's owner is
    /// `committer`, and the head's last write is `committer.at_ms`. Returns the changes' ids, in
    /// order; no changes write nothing.
    pub fn commit_many(
        &mut self,
        name: &str,
        changes: Vec<Change>,
        current: &dyn Fn(&BlockId) -> Option<Hash>,
        conflicts: &[Conflict],
        claim: Option<Owner>,
        committer: &Provenance,
    ) -> Result<Vec<Hash>, StoreError> {
        if changes.is_empty() {
            return Ok(vec![]);
        }
        let parts = self.commit_parts(name, changes, current, conflicts, claim, committer)?;
        let mut items = parts.changes;
        items.extend(parts.views);
        let written = self.write(items)?;
        let mut ids = Vec::with_capacity(parts.placed.len());
        for (cid, pos) in parts.placed {
            let id = written[pos];
            if id != cid {
                return Err(StoreError::Decode(format!(
                    "change id mismatch: view names {cid}, written {id}"
                )));
            }
            ids.push(id);
        }
        Ok(ids)
    }

    /// The checks and frames of `commit_many`, which writes nothing: the changes' frames (each
    /// change's `BLOCK` frames, then its `CHANGE` frame), then the view ops and the view's head
    /// ref, and each change's id with the position of its `CHANGE` frame in `changes`. The one
    /// view commit writer: the redaction writer puts its records around these frames.
    pub(crate) fn commit_parts(
        &self,
        name: &str,
        changes: Vec<Change>,
        current: &dyn Fn(&BlockId) -> Option<Hash>,
        conflicts: &[Conflict],
        claim: Option<Owner>,
        committer: &Provenance,
    ) -> Result<CommitParts, StoreError> {
        // a read projection is refused before any check reads it as a write
        for change in &changes {
            self.refuse_projection(change)?;
        }
        // 0. the changes write disjoint blocks: each is checked against the state before them
        if changes.len() > 1 {
            let mut seen: BTreeSet<&BlockId> = BTreeSet::new();
            for change in &changes {
                let mine: BTreeSet<&BlockId> = change.ops.iter().map(Op::block_id).collect();
                for id in mine {
                    if !seen.insert(id) {
                        return Err(StoreError::Conflict { id: id.clone() });
                    }
                }
            }
        }
        // 1. the head; a closed view takes no commit
        let head = self.view_head(name)?;
        if head.as_ref().is_some_and(|h| h.closed) {
            return Err(StoreError::ViewClosed(name.to_string()));
        }
        // the float catch-up fires for an empty unpinned view behind canon; the view then reads
        // as canon, so the ops must have been checked against canon on the blocks they write
        let canon = self.canon_head()?;
        let catch_up = head
            .as_ref()
            .is_some_and(|h| !h.pinned && h.len == 0 && canon.is_some() && canon != h.base);
        if catch_up {
            let canon_state = self.state_of(canon)?;
            for op in changes.iter().flat_map(|c| &c.ops) {
                let id = op.block_id();
                let actual = canon_state.blocks.get(id).copied();
                if current(id) != actual {
                    return Err(match op.base() {
                        Some(expected) => StoreError::StaleBase {
                            id: id.clone(),
                            expected,
                            actual,
                        },
                        None => StoreError::AlreadyExists(id.clone()),
                    });
                }
            }
        }
        // 2. the ops of each change. apply_op only reads and writes the ids the ops name, so seed
        // a small state with those; an op on a conflicted block, first in its change to touch
        // it, checks its base against the conflict's base and sides instead
        for change in &changes {
            self.check_ops(change, current, conflicts)?;
        }
        // 3. each change's frames; a commit op names the change id, which depends on deps, so
        // the changes, their view ops and the view's head land in one write
        let mut items = vec![];
        let mut placed: Vec<(Hash, usize)> = vec![];
        for mut change in changes {
            let (frames, pos) = self.change_frames(&mut change, Some(name), conflicts)?;
            let cid = Hash::of(&frames[pos].1);
            if placed.iter().any(|(c, _)| *c == cid) {
                return Err(StoreError::Decode(format!(
                    "change {cid} is committed twice in one write"
                )));
            }
            placed.push((cid, items.len() + pos));
            items.extend(frames);
        }
        let mut views: Frames = vec![];
        let p = committer;
        let view_op = |prev: Option<Hash>, op: ViewOpKind| ViewOp {
            view: name.to_string(),
            prev,
            op,
            actor: p.actor.clone(),
            run: p.run.clone(),
            at_ms: p.at_ms,
        };
        // 4. the view ops before the commits: the implicit create, or the float catch-up and the
        // claim, each op's prev the one before it
        let (mut prev, mut base, len0, mut owner, pinned) = match head {
            Some(head) => (head.head, head.base, head.len, head.owner, head.pinned),
            None => {
                let base = canon;
                // M2b's rule: the implicit create's owner is the committer (a single commit's
                // committer is its change's provenance; the engine's claim for a new view is that
                // same owner)
                let first = Some(Owner {
                    actor: p.actor.clone(),
                    run: p.run.clone(),
                });
                let create = crate::codec::item_new(
                    kinds::VIEW_OP,
                    &view_op(
                        None,
                        ViewOpKind::Create {
                            base,
                            owner: first.clone(),
                            pinned: false,
                        },
                    ),
                );
                let id = Hash::of(&create.1);
                views.push(create);
                (id, base, 0, first, false)
            }
        };
        if catch_up && let Some(landing) = canon {
            let rebase = crate::codec::item_new(
                kinds::VIEW_OP,
                &view_op(
                    Some(prev),
                    ViewOpKind::Rebase {
                        landing,
                        dropped: vec![],
                        remaining: vec![],
                        conflicts: vec![],
                    },
                ),
            );
            prev = Hash::of(&rebase.1);
            base = canon;
            views.push(rebase);
        }
        if owner.is_none()
            && let Some(o) = claim
        {
            let own = crate::codec::item_new(
                kinds::VIEW_OP,
                &view_op(
                    Some(prev),
                    ViewOpKind::Own {
                        owner: Some(o.clone()),
                    },
                ),
            );
            prev = Hash::of(&own.1);
            owner = Some(o);
            views.push(own);
        }
        // 5. one commit op per change, in order: one write's commits sit in a row in the log
        for (cid, _) in &placed {
            let commit = crate::codec::item_new(
                kinds::VIEW_OP,
                &view_op(Some(prev), ViewOpKind::Commit { change: *cid }),
            );
            prev = Hash::of(&commit.1);
            views.push(commit);
        }
        let new_head = ViewHead {
            head: prev,
            base,
            len: len0 + placed.len() as u64,
            owner,
            pinned,
            closed: false,
            last_write_ms: p.at_ms,
        };
        views.extend(self.ref_items_pub(vec![(view_ref(name), encode(&new_head))]));
        Ok(CommitParts {
            changes: items,
            views,
            placed,
        })
    }

    /// Step 2 of `commit_many` for one change: its ops against `current`, or for a block with a
    /// recorded conflict (first touched in this change) against the conflict's base and sides.
    fn check_ops(
        &self,
        change: &Change,
        current: &dyn Fn(&BlockId) -> Option<Hash>,
        conflicts: &[Conflict],
    ) -> Result<(), StoreError> {
        let mut state = StateSnapshot::default();
        for op in &change.ops {
            let id = op.block_id();
            if let Some(v) = current(id) {
                state.blocks.insert(id.clone(), v);
            }
        }
        let mut touched: BTreeSet<&BlockId> = BTreeSet::new();
        for op in &change.ops {
            let id = op.block_id();
            let conflict = match touched.insert(id) {
                true => conflicts.iter().find(|c| c.block == *id),
                false => None,
            };
            match (op, conflict) {
                (Op::Update { base, .. } | Op::Delete { base, .. }, Some(c)) => {
                    let accepted =
                        c.base == Some(*base) || c.sides.iter().any(|s| s.version == Some(*base));
                    if !accepted {
                        return Err(StoreError::StaleBase {
                            id: id.clone(),
                            expected: *base,
                            actual: current(id),
                        });
                    }
                    // the base is one of these: a frozen version anywhere in the conflict means
                    // the write would change or drop an accepted or rejected decision, unless it
                    // takes the landed side unchanged: a side's version that is canon's version
                    // of the block and frozen (cf1, D-X4GMH, `rdy view resolve --landed`)
                    let takes_side = match op {
                        Op::Update { block, .. }
                            if block.version_hash() == *base
                                && c.sides.iter().any(|s| s.version == Some(*base)) =>
                        {
                            self.canon_state()?.blocks.get(id) == Some(base)
                                && decision::is_frozen(block)
                        }
                        _ => false,
                    };
                    let versions = c
                        .base
                        .iter()
                        .chain(c.sides.iter().filter_map(|s| s.version.as_ref()));
                    for v in versions.filter(|_| !takes_side) {
                        if decision::is_frozen(&self.get_block(v)?) {
                            return Err(StoreError::Frozen(id.clone()));
                        }
                    }
                    match op {
                        Op::Update { block, .. } => {
                            state.blocks.insert(id.clone(), block.version_hash());
                        }
                        _ => {
                            state.blocks.remove(id);
                        }
                    }
                }
                _ => apply_op(self, &mut state, op)?,
            }
        }
        Ok(())
    }

    /// Proposes view `view`'s included changes as a new proposal: the proposal, its ref
    /// `proposal/<id>` (the latest version, itself) and `open/<view>` (its stable id, itself) in
    /// one write. Returns the proposal's id.
    pub fn propose(&mut self, view: &str, title: &str, by: Provenance) -> Result<Hash, StoreError> {
        let changes = self.included(view)?.as_ref().clone();
        self.propose_changes(view, title, None, by, changes, None)
    }

    /// `propose` with `changes` (a closed subset of the view's included changes, which the
    /// caller checks) in place of the view's whole set. An empty `changes` is `EmptyProposal`.
    /// A blank `summary` is `None`. `alternative` is the member the version chooses, which the
    /// caller checks.
    pub fn propose_changes(
        &mut self,
        view: &str,
        title: &str,
        summary: Option<&str>,
        by: Provenance,
        changes: Vec<Hash>,
        alternative: Option<AltChoice>,
    ) -> Result<Hash, StoreError> {
        let p = self.first_of(view, title, summary, by, changes, alternative)?;
        let id = proposal_hash(&p);
        self.write_version(&p, id)
    }

    /// `propose_changes`, with the answer `a` that holds the person's relay in the same batch
    /// (O14, D-KQ4CA): the proposal's checks, then its frames, then `a` with `targets` set to the
    /// new version. Returns the version's id and the answer's.
    #[allow(clippy::too_many_arguments)]
    pub fn propose_changes_answered(
        &mut self,
        view: &str,
        title: &str,
        summary: Option<&str>,
        by: Provenance,
        changes: Vec<Hash>,
        alternative: Option<AltChoice>,
        a: Answer,
    ) -> Result<(Hash, Hash), StoreError> {
        let p = self.first_of(view, title, summary, by, changes, alternative)?;
        let id = proposal_hash(&p);
        self.write_version_answered(&p, id, a)
    }

    /// Writes the next version of proposal `prev`: its changes followed by `add`, with `stable`
    /// the first version's id and `prev` the version it follows, and moves `proposal/<stable>` to
    /// it. Returns the new version's id.
    pub fn propose_version(&mut self, prev: &Hash, add: &[Hash]) -> Result<Hash, StoreError> {
        let mut p = self.proposal(prev)?;
        p.changes.extend_from_slice(add);
        let (next, stable) = self.linked_next(prev, p)?;
        self.write_version(&next, stable)
    }

    /// Writes the next version of proposal `prev` with `changes`, `title`, `by` and
    /// `alternative` in place of its own (the view carries over), and moves `proposal/<stable>`
    /// to it. A non-blank `summary` replaces the previous version's; a blank or `None` one keeps
    /// it. Returns the new version's id.
    pub fn propose_next(
        &mut self,
        prev: &Hash,
        changes: Vec<Hash>,
        title: &str,
        summary: Option<&str>,
        by: Provenance,
        alternative: Option<AltChoice>,
    ) -> Result<Hash, StoreError> {
        let p = self.next_of(prev, changes, title, summary, by, alternative)?;
        let (next, stable) = self.linked_next(prev, p)?;
        self.write_version(&next, stable)
    }

    /// `propose_next`, with the answer `a` that holds the person's relay in the same batch (O14,
    /// D-KQ4CA): the version's checks, then its frames, then `a` with `targets` set to the new
    /// version. Returns the version's id and the answer's.
    #[allow(clippy::too_many_arguments)]
    pub fn propose_next_answered(
        &mut self,
        prev: &Hash,
        changes: Vec<Hash>,
        title: &str,
        summary: Option<&str>,
        by: Provenance,
        alternative: Option<AltChoice>,
        a: Answer,
    ) -> Result<(Hash, Hash), StoreError> {
        let p = self.next_of(prev, changes, title, summary, by, alternative)?;
        let (next, stable) = self.linked_next(prev, p)?;
        self.write_version_answered(&next, stable, a)
    }

    /// Prepares the proposal version `req` names, writing nothing (D-4JMPH): a key change rdy
    /// adds to the view first (its frames, view ops and head ref, as `commit_to_view_with`
    /// writes them), then the version and its refs (`propose_changes` or `propose_next`), then
    /// the answer that holds the person's relay (its `targets` the version). Every check of those
    /// writers runs here. [`Store::bind_plan`] adds the version's bound plan;
    /// [`Store::commit_propose`] writes it all in one batch.
    pub fn prepare_propose(&self, req: ProposeRequest<'_>) -> Result<PreparedProposal, StoreError> {
        let ProposeRequest {
            view,
            title,
            summary,
            by,
            mut changes,
            alternative,
            prev,
            rekey,
            answer,
        } = req;
        let mut items: Frames = vec![];
        let mut rekeyed = None;
        if let Some((change, current)) = rekey {
            let committer = change.provenance.clone();
            let parts = self.commit_parts(view, vec![change], current, &[], None, &committer)?;
            let (cid, pos) = parts.placed[0];
            items.extend(parts.changes);
            items.extend(parts.views);
            changes.push(cid);
            rekeyed = Some((cid, pos));
        }
        let (p, stable) = match prev {
            Some(prev) => {
                let p = self.next_of(&prev, changes, title, summary, by, alternative)?;
                self.linked_next(&prev, p)?
            }
            None => {
                let p = self.first_of(view, title, summary, by, changes, alternative)?;
                let id = proposal_hash(&p);
                (p, id)
            }
        };
        let (version, frames) = self.version_items(&p, stable)?;
        items.extend(frames);
        let answer = match answer {
            None => None,
            Some(a) => {
                let a = Answer {
                    targets: vec![version],
                    ..a
                };
                let (aid, all) = self.answer_items(&a, items)?;
                items = all;
                Some(aid)
            }
        };
        Ok(PreparedProposal {
            items,
            version,
            stable,
            proposal: p,
            rekey: rekeyed,
            answer,
            plan: None,
            assessment: None,
            fresh: None,
        })
    }

    /// Prepares the proposal of a re-affirmation card's isolated landing (D-CQMA4), writing
    /// nothing: the internal view's `Create` op, the members' change, its `Commit` op, then the
    /// proposal's first version and its refs (`proposal/<id>`, `open/<view>`), as the existing
    /// commit and proposal writers build them (`commit_parts`, `version_items`). The view's head
    /// ref is left to the landing's close ([`Store::close_reaffirm_view`]), so the batch moves it
    /// once. Refused, with nothing built: a view outside [`crate::REAFFIRM_VIEW_PREFIX`]
    /// (`Refused`), a view that exists (`ViewExists`: an old internal view is never reused), a
    /// canon that moved since the card was read (`Refused`), a change of another view or with an
    /// op other than an update (`Refused`), and each check of the commit writer (a stale base
    /// is `StaleBase`). [`Store::bind_plan`] adds its bound plan; only
    /// [`Store::prepare_reaffirm_land`] takes it on.
    pub fn prepare_reaffirm_propose(
        &self,
        req: ReaffirmProposeRequest<'_>,
    ) -> Result<PreparedProposal, StoreError> {
        let ReaffirmProposeRequest {
            view,
            canon_at,
            change,
            title,
            summary,
            by,
        } = req;
        if !view.starts_with(crate::REAFFIRM_VIEW_PREFIX) {
            return Err(StoreError::Refused(format!(
                "a re-affirmation view starts with {}",
                crate::REAFFIRM_VIEW_PREFIX
            )));
        }
        if self.view_head(view)?.is_some() {
            return Err(StoreError::ViewExists(view.to_string()));
        }
        if self.canon_head()? != canon_at {
            return Err(StoreError::Refused(
                "canon moved since the re-affirmation card was read".into(),
            ));
        }
        if change.view != view
            || change.ops.is_empty()
            || !change.ops.iter().all(|op| matches!(op, Op::Update { .. }))
        {
            return Err(StoreError::Refused(
                "a re-affirmation change holds only updates of its own view".into(),
            ));
        }
        let canon = self.canon_state()?;
        let current = |id: &BlockId| canon.blocks.get(id).copied();
        let parts = self.commit_parts(view, vec![change], &current, &[], None, &by)?;
        let (cid, pos) = parts.placed[0];
        let committed: Change =
            crate::codec::decode_record(parts.changes[pos].0, &parts.changes[pos].1)?;
        // the view's Create op first, then the change, then its Commit op; the head ref is the
        // close's to write
        let mut ops = parts.views.into_iter();
        let mut items: Frames = vec![];
        let mut head = None;
        if let Some(create) = ops.next() {
            items.push(create);
        }
        items.extend(parts.changes);
        for (kind, payload) in ops {
            if kind == kinds::REF_UPDATE {
                head = crate::holder::decode_ref(&payload)
                    .and_then(|u| crate::view::decode_head(view, &u.value).ok());
                continue;
            }
            items.push((kind, payload));
        }
        let head = head.ok_or_else(|| {
            StoreError::Refused("the re-affirmation commit gave no view head".into())
        })?;
        let p = self.first_of(view, title, Some(summary), by, vec![cid], None)?;
        let stable = proposal_hash(&p);
        let (version, frames) = self.version_items(&p, stable)?;
        items.extend(frames);
        Ok(PreparedProposal {
            items,
            version,
            stable,
            proposal: p,
            rekey: None,
            answer: None,
            plan: None,
            assessment: None,
            fresh: Some(FreshView {
                head,
                change: committed,
            }),
        })
    }

    /// Adds `plan`, the plan version bound to the prepared version, to `prepared`'s batch, after
    /// its frames: the PLAN and `plan/<view>` naming it. The plan must name that version and
    /// carry no seal (`plan <hash>: invalid proposal` or `invalid seal`), and keeps every plan
    /// rule against the store and the batch (`Store::check_plan`). Writes nothing; a version
    /// binds one plan. Returns the plan's id.
    pub fn bind_plan(
        &self,
        prepared: &mut PreparedProposal,
        plan: &Plan,
    ) -> Result<Hash, StoreError> {
        let id = crate::plan::plan_hash(plan);
        if plan.seal.is_some() {
            return Err(StoreError::Refused(crate::plan::damage(
                &id,
                crate::plan::INVALID_SEAL,
            )));
        }
        if plan.proposal != Some(prepared.version) || prepared.plan.is_some() {
            return Err(StoreError::Refused(crate::plan::damage(
                &id,
                crate::plan::INVALID_PROPOSAL,
            )));
        }
        let (id, frames) = self.plan_frames(plan, &prepared.items)?;
        prepared.items.extend(frames);
        prepared.plan = Some(id);
        Ok(id)
    }

    /// Adds `a`, the assessment of the prepared version, to its batch after the plan: the
    /// ASSESSMENT frame (D-ME2HD: written in the propose batch). A re-affirmation proposal takes
    /// none (C7: `Refused`). `a.proposal` must be the prepared version and `a.validate()` must
    /// pass, as must the carried bindings it names (`record <hash> (kind 25): <reason>`,
    /// `carried_records::refusal`), and every object it names must be in the batch or the store
    /// with its kind (`NotFound(<hash>)`): a plan its tension answers name is bound first. Writes
    /// nothing; one per version batch. Returns its hash.
    pub fn bind_assessment(
        &self,
        prepared: &mut PreparedProposal,
        a: &Assessment,
    ) -> Result<Hash, StoreError> {
        if prepared.fresh.is_some() {
            return Err(StoreError::Refused(REAFFIRM_WITH_LANDING.into()));
        }
        let item = crate::codec::item_new(kinds::ASSESSMENT, a);
        let id = Hash::of(&item.1);
        if a.proposal != prepared.version || prepared.assessment.is_some() {
            return Err(crate::carried_records::refusal(
                &id,
                kinds::ASSESSMENT,
                crate::carried_records::ASSESSMENT_PROPOSAL,
            ));
        }
        self.check_carried::<Assessment>(&item, &prepared.items)?;
        prepared.items.push(item);
        prepared.assessment = Some(id);
        Ok(id)
    }

    /// Writes `a` in a batch of its own (RULING F1's fresh assessment), with the rules of
    /// [`Store::bind_assessment`]: the version it assesses, like every object it names, must be
    /// held (`NotFound` otherwise). Returns its hash.
    pub fn put_assessment(&mut self, a: &Assessment) -> Result<Hash, StoreError> {
        let item = crate::codec::item_new(kinds::ASSESSMENT, a);
        let id = Hash::of(&item.1);
        self.check_carried::<Assessment>(&item, &[])?;
        self.write_guarded(vec![item])?;
        Ok(id)
    }

    /// Checks `item`, a carried record's frame (an ASSESSMENT, a DISPUTE or a
    /// DISPUTE_RESOLUTION, decoded as a `T`), as the frame after `batch`: its bounded encoding and
    /// its own rules (`carried_records::validate_local`: `Assessment::validate`,
    /// `Dispute::validate`), as `record <hash> (kind <kind>): <reason>`; every object it names
    /// (`object_refs::refs_of`) in `batch` or the store with its kind (`NotFound(<hash>)` for the
    /// first that is not); and the bindings it names (`carried_records::validate_refs`, reading
    /// `batch` before the store). Writes nothing.
    pub(crate) fn check_carried<T: serde::de::DeserializeOwned + 'static>(
        &self,
        item: &(u8, Vec<u8>),
        batch: &[(u8, Vec<u8>)],
    ) -> Result<(), StoreError> {
        let id = Hash::of(&item.1);
        let kind = crate::codec::logical_kind(item.0);
        let refuse = |reason: &str| crate::carried_records::refusal(&id, kind, reason);
        // within the bounded decoder's limits, so every reader can read it back
        let split = crate::codec::split(item.0, &item.1).map_err(|e| refuse(&e.to_string()))?;
        crate::codec::decode_bounded_split::<T>(&split).map_err(|e| refuse(&e.to_string()))?;
        crate::carried_records::validate_local(item.0, &item.1).map_err(refuse)?;
        // the objects it names are held, as an answer's target is (`answer.rs`): an object the
        // carried bindings do not find passes them, and a landing that named the record would
        // then give a closure that does not resolve
        let edges =
            crate::object_refs::refs_of(item.0, &item.1).map_err(|e| refuse(&e.to_string()))?;
        // each batch frame hashed once, whatever the number of edges
        let pending: std::collections::BTreeSet<(u8, Hash)> = batch
            .iter()
            .map(|(raw, payload)| (crate::codec::logical_kind(*raw), Hash::of(payload)))
            .collect();
        for edge in edges {
            let in_batch = match edge.kind {
                Some(k) => pending.contains(&(k, edge.hash)),
                None => pending.iter().any(|(_, h)| *h == edge.hash),
            };
            let held = in_batch
                || match edge.kind {
                    Some(k) => self.has_kind(k, &edge.hash)?,
                    None => self.has(&edge.hash)?,
                };
            if !held {
                return Err(StoreError::NotFound(edge.hash));
            }
        }
        let key = crate::object_refs::ObjectKey { kind, hash: id };
        let mut source = PendingFrames {
            store: crate::carried_records::StoreObjects(self),
            batch,
            last: item,
        };
        if let Some(reason) = crate::carried_records::validate_refs(&mut source, key)? {
            return Err(refuse(reason));
        }
        Ok(())
    }

    /// Writes the proposal `prepared` holds in one batch (one write, so one marker): its key
    /// change, the version, the relay's answer, the bound plan and the bound assessment. A
    /// failed write writes nothing.
    /// Returns what it wrote.
    pub fn commit_propose(&mut self, prepared: PreparedProposal) -> Result<Proposed, StoreError> {
        let PreparedProposal {
            items,
            version,
            stable,
            rekey,
            answer,
            plan,
            assessment,
            fresh,
            ..
        } = prepared;
        // a re-affirmation card's proposal is written only with its landing
        if fresh.is_some() {
            return Err(StoreError::Refused(REAFFIRM_WITH_LANDING.into()));
        }
        crate::redaction::refuse_redaction_kinds(&items)?;
        let written = self.write_guarded(items)?;
        if let Some((cid, pos)) = rekey
            && written[pos] != cid
        {
            return Err(StoreError::Decode(format!(
                "change id mismatch: view names {cid}, written {}",
                written[pos]
            )));
        }
        Ok(Proposed {
            version,
            stable,
            rekey: rekey.map(|(c, _)| c),
            answer,
            plan,
            assessment,
        })
    }

    /// A new proposal's first version, as `propose_changes` builds it.
    fn first_of(
        &self,
        view: &str,
        title: &str,
        summary: Option<&str>,
        by: Provenance,
        changes: Vec<Hash>,
        alternative: Option<AltChoice>,
    ) -> Result<Proposal, StoreError> {
        Ok(Proposal {
            stable: None,
            prev: None,
            title: title.to_string(),
            view: view.to_string(),
            changes,
            alternative,
            provenance: by,
            canon_at: self.canon_head()?,
            summary: clean_summary(summary),
        })
    }

    /// The version after `prev` as `propose_next` builds it, before `linked_next` links it.
    fn next_of(
        &self,
        prev: &Hash,
        changes: Vec<Hash>,
        title: &str,
        summary: Option<&str>,
        by: Provenance,
        alternative: Option<AltChoice>,
    ) -> Result<Proposal, StoreError> {
        let before = self.proposal(prev)?;
        Ok(Proposal {
            changes,
            title: title.to_string(),
            provenance: by,
            alternative,
            summary: clean_summary(summary).or_else(|| before.summary.clone()),
            ..before
        })
    }

    /// `p` as the version after `prev`, and its stable id: `stable` the first version's id,
    /// `prev` the version it follows, `canon_at` canon's head now. Only the latest version has a
    /// next one (`ProposalMoved`).
    fn linked_next(&self, prev: &Hash, p: Proposal) -> Result<(Proposal, Hash), StoreError> {
        let stable = p.stable.unwrap_or(*prev);
        // only the latest version has a next one: a next of a superseded version would branch
        // the chain and move the ref (as `land_atomic` checks)
        let latest = match self.get_ref(&proposal_ref(&stable))? {
            Some(b) => crate::index::hash_from(&b, "proposal ref")?,
            None => {
                return Err(StoreError::Decode(format!(
                    "proposal {stable} has no ref; run rdy reindex"
                )));
            }
        };
        if latest != *prev {
            return Err(StoreError::ProposalMoved { stable, latest });
        }
        let next = Proposal {
            stable: Some(stable),
            prev: Some(*prev),
            canon_at: self.canon_head()?,
            ..p
        };
        Ok((next, stable))
    }

    /// Writes proposal version `p` with the refs `proposal/<stable>` = its id and
    /// `open/<p.view>` = `stable`, in one write. Returns its id. A version with no changes is
    /// refused (`EmptyProposal`): it would land as an empty landing, as often as it is approved.
    fn write_version(&mut self, p: &Proposal, stable: Hash) -> Result<Hash, StoreError> {
        let (id, items) = self.version_items(p, stable)?;
        self.write(items)?;
        Ok(id)
    }

    /// `write_version`, with the answer `a` (its `targets` the version) at the end of the same
    /// write (O14). Returns the version's id and the answer's.
    fn write_version_answered(
        &mut self,
        p: &Proposal,
        stable: Hash,
        a: Answer,
    ) -> Result<(Hash, Hash), StoreError> {
        let (id, items) = self.version_items(p, stable)?;
        let a = Answer {
            targets: vec![id],
            ..a
        };
        Ok((id, self.put_answer_with(&a, items)?))
    }

    /// The version's id and the frames `write_version` writes; `EmptyProposal` for no changes.
    fn version_items(&self, p: &Proposal, stable: Hash) -> Result<(Hash, Frames), StoreError> {
        if p.changes.is_empty() {
            return Err(StoreError::EmptyProposal(p.view.clone()));
        }
        let (kind, bytes) = crate::codec::item_new(kinds::PROPOSAL, p);
        let id = Hash::of(&bytes);
        let mut items = vec![(kind, bytes)];
        items.extend(self.ref_items_pub(vec![
            (proposal_ref(&stable), id.0.to_vec()),
            (open_ref(&p.view), stable.0.to_vec()),
        ]));
        Ok((id, items))
    }

    /// Lands proposal version `proposal` as is: `land_atomic` with no acceptance change, no
    /// chosen alternative and no relay. Returns the landing id.
    pub fn land(&mut self, proposal: &Hash, approver: Provenance) -> Result<Hash, StoreError> {
        Ok(self
            .land_atomic(LandRequest {
                proposal: *proposal,
                accept: None,
                approver,
                alt: None,
                relay: None,
                evidence: None,
                assessment: None,
            })?
            .landing)
    }

    /// Does not check for an existing `tag/<name>` ref (it would be overwritten): the engine's
    /// E_TAG_EXISTS check is the guard.
    ///
    /// Writes `tag` and the ref `tag/<name>` → its hash in one atomic write.
    pub fn put_tag(&mut self, tag: &Tag) -> Result<Hash, StoreError> {
        let prepared = self.prepare_tag(tag, None)?;
        Ok(self.commit_tag(prepared, &[])?.0)
    }

    /// `put_tag`, with the answer `a` that holds the person's relay in the same write (O14,
    /// D-DGCH7): the tag and its ref, then `a` with `targets` set to the tag, and its ref. The
    /// tag is encoded as `put_tag` encodes it (D-F0904). Returns the tag's id and the answer's.
    pub fn put_tag_answered(&mut self, tag: &Tag, a: Answer) -> Result<(Hash, Hash), StoreError> {
        let prepared = self.prepare_tag(tag, Some(a))?;
        let (id, answer) = self.commit_tag(prepared, &[])?;
        Ok((id, answer.expect("an answered tag writes its answer")))
    }

    /// The write of `put_tag` (with `a` none) or `put_tag_answered` (with `a`), prepared without
    /// writing: the tag and its ref, then the answer with `targets` set to the tag, and its ref.
    /// The answer's rules are checked here. [`Store::commit_tag`] writes it.
    pub fn prepare_tag(&self, tag: &Tag, a: Option<Answer>) -> Result<PreparedTag, StoreError> {
        let (id, items) = self.tag_items(tag);
        let (answer, items) = match a {
            None => (None, items),
            Some(a) => {
                let a = Answer {
                    targets: vec![id],
                    ..a
                };
                let (aid, items) = self.answer_items(&a, items)?;
                (Some((aid, a)), items)
            }
        };
        Ok(PreparedTag {
            items,
            tag: tag.clone(),
            hash: id,
            answer,
        })
    }

    /// Writes the tag `prepared` holds, with `signatures` (each of its tag, D-1Z951) in the same
    /// batch, after the tag and its answer. Returns the tag's id and the answer's. A failed write
    /// writes nothing.
    pub fn commit_tag(
        &mut self,
        prepared: PreparedTag,
        signatures: &[Signature],
    ) -> Result<(Hash, Option<Hash>), StoreError> {
        let PreparedTag {
            mut items,
            hash,
            answer,
            ..
        } = prepared;
        items.extend(crate::signature::frames(signatures)?);
        self.write(items)?;
        Ok((hash, answer.map(|(h, _)| h)))
    }

    /// The tag's id and the frames `put_tag` writes: the tag, then `tag/<name>` → its id.
    fn tag_items(&self, tag: &Tag) -> (Hash, Frames) {
        let (kind, bytes) = crate::codec::item_new(kinds::TAG, tag);
        let id = Hash::of(&bytes);
        let mut items = vec![(kind, bytes)];
        items.extend(self.ref_items_pub(vec![(tag_ref(&tag.name), id.0.to_vec())]));
        (id, items)
    }
}

/// A tag built by [`Store::prepare_tag`] and not yet written: the tag, its hash and the final
/// answer that relays it (its `targets` the tag), with private frames that only
/// [`Store::commit_tag`] writes.
#[derive(Clone, Debug)]
pub struct PreparedTag {
    items: Frames,
    tag: Tag,
    hash: Hash,
    answer: Option<(Hash, Answer)>,
}

impl PreparedTag {
    /// The tag the commit writes.
    pub fn tag(&self) -> &Tag {
        &self.tag
    }

    /// The final TAG hash.
    pub fn hash(&self) -> Hash {
        self.hash
    }

    /// The final ANSWER and its hash, `targets` included; none for a tag with no relay.
    pub fn answer(&self) -> Option<(Hash, &Answer)> {
        self.answer.as_ref().map(|(h, a)| (*h, a))
    }
}

/// The version of a block id in a view as the view reads now (`Store::commit_to_view_with`).
pub type ViewReads<'a> = &'a dyn Fn(&BlockId) -> Option<Hash>;

/// A proposal version to prepare ([`Store::prepare_propose`]).
pub struct ProposeRequest<'a> {
    pub view: &'a str,
    pub title: &'a str,
    /// A non-blank summary replaces the previous version's; a blank or `None` one keeps it.
    pub summary: Option<&'a str>,
    pub by: Provenance,
    /// The proposed changes, which the caller checks (a closed subset of the view's).
    pub changes: Vec<Hash>,
    pub alternative: Option<AltChoice>,
    /// The latest version of the view's open proposal, which this version follows; `None` for
    /// a new proposal.
    pub prev: Option<Hash>,
    /// A change rdy adds to the view before the version (the key change, K1), with the view's
    /// present state as `commit_to_view_with` reads it; it joins the version's changes.
    pub rekey: Option<(Change, ViewReads<'a>)>,
    /// The answer that holds the person's relay (O14); its `targets` become the version.
    pub answer: Option<Answer>,
}

/// The proposal of a re-affirmation card's isolated landing (D-CQMA4, D-EJBPD), to prepare with
/// [`Store::prepare_reaffirm_propose`]: the card's fresh internal view, the canon head the card
/// was read on, the one change of the members' metadata-only updates, the card's title and
/// summary, and the person who answered the card. Only the engine's re-affirmation card writer
/// builds it; no public create, write or propose path reaches it.
pub struct ReaffirmProposeRequest<'a> {
    /// The internal view: [`crate::REAFFIRM_VIEW_PREFIX`] and the card hash's first 12 digits.
    pub view: &'a str,
    /// The canon head the card was read on; canon must still stand there.
    pub canon_at: Option<Hash>,
    /// The members' updates, each on its block's version in canon.
    pub change: Change,
    pub title: &'a str,
    pub summary: &'a str,
    pub by: Provenance,
}

/// The fresh internal view of a prepared re-affirmation proposal: its head after its `Create`
/// and `Commit` ops, and its one change as committed (its deps set).
#[derive(Clone, Debug)]
pub(crate) struct FreshView {
    pub(crate) head: ViewHead,
    pub(crate) change: Change,
}

/// A proposal built by [`Store::prepare_propose`] and not yet written. Its frames are private:
/// only [`Store::bind_plan`] adds to them and only [`Store::commit_propose`] writes them. One
/// built by [`Store::prepare_reaffirm_propose`] is written only as the start of its landing's
/// batch ([`Store::prepare_reaffirm_land`]).
#[derive(Debug)]
pub struct PreparedProposal {
    pub(crate) items: Frames,
    version: Hash,
    stable: Hash,
    pub(crate) proposal: Proposal,
    /// The key change's id and the position of its CHANGE frame.
    rekey: Option<(Hash, usize)>,
    answer: Option<Hash>,
    pub(crate) plan: Option<Hash>,
    /// The bound assessment ([`Store::bind_assessment`]); none until one is bound.
    pub(crate) assessment: Option<Hash>,
    /// The fresh internal view of a re-affirmation card's proposal; none for every other.
    pub(crate) fresh: Option<FreshView>,
}

impl PreparedProposal {
    /// The version's hash.
    pub fn version(&self) -> Hash {
        self.version
    }

    /// The proposal's stable id.
    pub fn stable(&self) -> Hash {
        self.stable
    }

    /// The version the commit writes.
    pub fn proposal(&self) -> &Proposal {
        &self.proposal
    }

    /// The id of the key change rdy adds, if any.
    pub fn rekey(&self) -> Option<Hash> {
        self.rekey.map(|(c, _)| c)
    }

    /// The change of a re-affirmation card's proposal as committed to its internal view (its
    /// deps set); none for every other proposal.
    pub fn fresh_change(&self) -> Option<&Change> {
        self.fresh.as_ref().map(|f| &f.change)
    }
}

/// What [`Store::commit_propose`] wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Proposed {
    pub version: Hash,
    pub stable: Hash,
    pub rekey: Option<Hash>,
    pub answer: Option<Hash>,
    pub plan: Option<Hash>,
    /// The assessment written in the batch, if one was bound.
    pub assessment: Option<Hash>,
}

/// The refusal of a re-affirmation card's proposal outside its landing (D-CQMA4): its commit
/// ([`Store::commit_propose`]) and an assessment bound to it ([`Store::bind_assessment`]; C7).
const REAFFIRM_WITH_LANDING: &str = "a re-affirmation proposal is written only with its landing";

/// A proposal summary as stored: trimmed, and `None` when blank.
fn clean_summary(summary: Option<&str>) -> Option<String> {
    summary
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The store's objects, the frames of a batch not written yet and one more frame after them, as
/// the carried checks read them: the batch's frames first.
pub(crate) struct PendingFrames<'a> {
    pub(crate) store: crate::carried_records::StoreObjects<'a>,
    pub(crate) batch: &'a [(u8, Vec<u8>)],
    pub(crate) last: &'a (u8, Vec<u8>),
}

impl crate::carried_records::CarriedSource for PendingFrames<'_> {
    fn object(
        &mut self,
        key: crate::object_refs::ObjectKey,
    ) -> Result<Option<(u16, Vec<u8>)>, StoreError> {
        let pending = self.batch.iter().chain(std::iter::once(self.last));
        for (raw, payload) in pending {
            if crate::codec::logical_kind(*raw) == key.kind && Hash::of(payload) == key.hash {
                return Ok(crate::codec::split(*raw, payload)
                    .ok()
                    .map(|o| (o.version, o.body.to_vec())));
            }
        }
        self.store.object(key)
    }
}

impl StoreReader {
    /// The assessment `h`, decoded; an object of another kind with that hash is named as what it
    /// is.
    pub fn assessment(&self, h: &Hash) -> Result<Assessment, StoreError> {
        self.get_named(kinds::ASSESSMENT, "assessment", h)
    }

    /// The newest ASSESSMENT whose `proposal` is `version` and whose frame lies before `end` (a
    /// captured log end): a later one replaces an earlier one.
    pub fn assessment_of_proposal(
        &self,
        version: &Hash,
        end: crate::segment::Location,
    ) -> Result<Option<Hash>, StoreError> {
        self.index().assessment_proposal(version, end)
    }

    /// The DISPUTEs of `landing` whose frames lie before `end` (a captured log end), in log
    /// order (D-ZWP7Q): one keyed range read of the dispute table.
    pub fn disputes_of(
        &self,
        landing: &Hash,
        end: crate::segment::Location,
    ) -> Result<Vec<Hash>, StoreError> {
        self.index().landing_disputes(landing, end)
    }

    /// The DISPUTE_RESOLUTION of `dispute` whose frame lies before `end`: the first in log order.
    /// None while the dispute is open.
    pub fn resolution_of(
        &self,
        dispute: &Hash,
        end: crate::segment::Location,
    ) -> Result<Option<Hash>, StoreError> {
        self.index().dispute_resolution(dispute, end)
    }

    /// The first `max` DISPUTEs whose frames lie before `end`, in log order, and how many more
    /// there are (omitted).
    pub fn all_disputes(
        &self,
        end: crate::segment::Location,
        max: usize,
    ) -> Result<(Vec<Hash>, usize), StoreError> {
        self.index().disputes_before(end, max)
    }

    /// The location of the log's first CHANGE whose links record a briefing (O3); none while
    /// no change records one.
    pub fn first_briefed_change(&self) -> Result<Option<crate::segment::Location>, StoreError> {
        self.index().first_briefed_change()
    }
}
