use crate::error::StoreError;
use crate::kinds;
use crate::land::LandRequest;
use crate::merge::{Conflict, MergeChange, Merged, merge_indexed};
use crate::store::{Frames, encode};
use crate::view::{Owner, ViewHead, ViewOp, ViewOpKind};
use crate::{Store, StoreReader};
use rdy_model::{
    AltChoice, Answer, Block, BlockId, Change, Hash, LandingRecord, Op, Proposal, Provenance,
    StateSnapshot, Tag, decision,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

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
/// `base` is a frozen decision: an update that changes only `store`'s tracking fields passes, and
/// so does one that leaves the block as it is (cf1: the landed side of a conflict, taken).
fn frozen_for(store: &StoreReader, base: &Block, new: Option<&Block>) -> bool {
    decision::is_frozen(base)
        && !new.is_some_and(|n| {
            n == base || decision::only_tracking_changed(base, n, store.tracking())
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
        Ok(merge_indexed(
            base,
            &input,
            &|v| producers.get(v).copied().flatten(),
            &|b| deleters.get(b).copied(),
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
                let create = encode(&view_op(
                    None,
                    ViewOpKind::Create {
                        base,
                        owner: first.clone(),
                        pinned: false,
                    },
                ));
                let id = Hash::of(&create);
                items.push((kinds::VIEW_OP, create));
                (id, base, 0, first, false)
            }
        };
        if catch_up && let Some(landing) = canon {
            let rebase = encode(&view_op(
                Some(prev),
                ViewOpKind::Rebase {
                    landing,
                    dropped: vec![],
                    remaining: vec![],
                    conflicts: vec![],
                },
            ));
            prev = Hash::of(&rebase);
            base = canon;
            items.push((kinds::VIEW_OP, rebase));
        }
        if owner.is_none()
            && let Some(o) = claim
        {
            let own = encode(&view_op(
                Some(prev),
                ViewOpKind::Own {
                    owner: Some(o.clone()),
                },
            ));
            prev = Hash::of(&own);
            owner = Some(o);
            items.push((kinds::VIEW_OP, own));
        }
        // 5. one commit op per change, in order: one write's commits sit in a row in the log
        for (cid, _) in &placed {
            let commit = encode(&view_op(Some(prev), ViewOpKind::Commit { change: *cid }));
            prev = Hash::of(&commit);
            items.push((kinds::VIEW_OP, commit));
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
        items.extend(self.ref_items_pub(vec![(view_ref(name), encode(&new_head))]));
        let written = self.write(items)?;
        let mut ids = Vec::with_capacity(placed.len());
        for (cid, pos) in placed {
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
        let id = Hash::of(&encode(&p));
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
        let id = Hash::of(&encode(&p));
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
        let bytes = encode(p);
        let id = Hash::of(&bytes);
        let mut items = vec![(kinds::PROPOSAL, bytes)];
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
            })?
            .landing)
    }

    /// Does not check for an existing `tag/<name>` ref (it would be overwritten): the engine's
    /// E_TAG_EXISTS check is the guard.
    ///
    /// Writes `tag` and the ref `tag/<name>` → its hash in one atomic write.
    pub fn put_tag(&mut self, tag: &Tag) -> Result<Hash, StoreError> {
        let (id, items) = self.tag_items(tag);
        self.write(items)?;
        Ok(id)
    }

    /// `put_tag`, with the answer `a` that holds the person's relay in the same write (O14,
    /// D-DGCH7): the tag and its ref, then `a` with `targets` set to the tag, and its ref. The
    /// tag is encoded as `put_tag` encodes it (D-F0904). Returns the tag's id and the answer's.
    pub fn put_tag_answered(&mut self, tag: &Tag, a: Answer) -> Result<(Hash, Hash), StoreError> {
        let (id, items) = self.tag_items(tag);
        let a = Answer {
            targets: vec![id],
            ..a
        };
        Ok((id, self.put_answer_with(&a, items)?))
    }

    /// The tag's id and the frames `put_tag` writes: the tag, then `tag/<name>` → its id.
    fn tag_items(&self, tag: &Tag) -> (Hash, Frames) {
        let bytes = postcard::to_allocvec(tag).expect("encode tag");
        let id = Hash::of(&bytes);
        let mut items = vec![(kinds::TAG, bytes)];
        items.extend(self.ref_items_pub(vec![(tag_ref(&tag.name), id.0.to_vec())]));
        (id, items)
    }
}

/// A proposal summary as stored: trimmed, and `None` when blank.
fn clean_summary(summary: Option<&str>) -> Option<String> {
    summary
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}
