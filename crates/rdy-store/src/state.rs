use crate::error::StoreError;
use crate::kinds;
use crate::land::LandRequest;
use crate::merge::{Conflict, MergeChange, Merged, merge_indexed};
use crate::store::encode;
use crate::view::{Owner, ViewHead, ViewOp, ViewOpKind};
use crate::{Store, StoreReader};
use rdy_model::{
    Block, BlockId, Change, Hash, LandingRecord, Op, Proposal, Provenance, StateSnapshot, Tag,
    decision,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
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
            check_base(store, state, &block.id, *base)?;
            state.blocks.insert(block.id.clone(), block.version_hash());
        }
        Op::Delete { base, id } => {
            check_base(store, state, id, *base)?;
            state.blocks.remove(id);
        }
    }
    Ok(())
}

fn check_base(
    store: &StoreReader,
    state: &StateSnapshot,
    id: &BlockId,
    base: Hash,
) -> Result<(), StoreError> {
    let actual = state.blocks.get(id).copied();
    if actual != Some(base) {
        return Err(StoreError::StaleBase {
            id: id.clone(),
            expected: base,
            actual,
        });
    }
    if decision::is_frozen(&store.get_block(&base)?) {
        return Err(StoreError::Frozen(id.clone()));
    }
    Ok(())
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

    /// Merges `changes` onto `base` (merge1): deps first, then change id, with conflicts as data.
    pub fn merge_onto(
        &self,
        base: &StateSnapshot,
        changes: &[Change],
    ) -> Result<Merged, StoreError> {
        Ok(self.merge_onto_indexed(base, changes).0)
    }

    /// `merge_onto`, with each applied change's position in `changes` (see `merge_indexed`).
    fn merge_onto_indexed(&self, base: &StateSnapshot, changes: &[Change]) -> (Merged, Vec<usize>) {
        let input: Vec<MergeChange<'_>> = changes
            .iter()
            .map(|c| MergeChange {
                id: c.id(),
                deps: &c.deps,
                ops: &c.ops,
            })
            .collect();
        merge_indexed(base, &input, &|v| self.producer(v).ok().flatten())
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
        self.merge_onto(&self.state_of(base)?, &changes)
    }

    pub fn view_state(&self, name: &str) -> Result<StateSnapshot, StoreError> {
        Ok(self.view_merge(name)?.state)
    }

    /// `merge_onto(canon, changes)` with the landing checks: the first conflict (block order) is
    /// `StaleBase` naming its base and canon's version, or `AlreadyExists` when its base is none;
    /// an op whose base is a frozen decision is `Frozen`. The one mapping the store's `land`, the
    /// engine's gate and atomic landings share. With the merge, it returns each applied change's
    /// position in `changes`, in the order applied (see `merge_indexed`).
    pub fn merge_checked(
        &self,
        canon: &StateSnapshot,
        changes: &[Change],
    ) -> Result<(Merged, Vec<usize>), StoreError> {
        let (merged, at) = self.merge_onto_indexed(canon, changes);
        if let Some(c) = merged.conflicts.first() {
            return Err(match c.base {
                Some(base) => StoreError::StaleBase {
                    id: c.block.clone(),
                    expected: base,
                    actual: canon.blocks.get(&c.block).copied(),
                },
                None => StoreError::AlreadyExists(c.block.clone()),
            });
        }
        for &i in &at {
            for op in &changes[i].ops {
                if let Some(base) = op.base()
                    && decision::is_frozen(&self.get_block(&base)?)
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
    ///   resolves the conflict; other ops check against `current` as ever;
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
            for op in &change.ops {
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
        // 2. the ops. apply_op only reads and writes the ids the ops name, so seed a small state
        // with those; an op on a conflicted block, first in this change to touch it, checks its
        // base against the conflict's base and sides instead
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
                    if decision::is_frozen(&self.get_block(base)?) {
                        return Err(StoreError::Frozen(id.clone()));
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
        // 3. The commit op names the change id, which depends on deps, so the change, its view
        // ops and the view's head land in one write.
        let mut change = change;
        let (mut items, change_pos) = self.change_frames(&mut change, Some(name), conflicts)?;
        let cid = Hash::of(&items[change_pos].1);
        let p = &change.provenance;
        let view_op = |prev: Option<Hash>, op: ViewOpKind| ViewOp {
            view: name.to_string(),
            prev,
            op,
            actor: p.actor.clone(),
            run: p.run.clone(),
            at_ms: p.at_ms,
        };
        // 4. the view ops before the commit: the implicit create, or the float catch-up and the
        // claim, each op's prev the one before it
        let (mut prev, mut base, len0, mut owner, pinned) = match head {
            Some(head) => (head.head, head.base, head.len, head.owner, head.pinned),
            None => {
                let base = canon;
                let create = encode(&view_op(
                    None,
                    ViewOpKind::Create {
                        base,
                        owner: claim.clone(),
                        pinned: false,
                    },
                ));
                let id = Hash::of(&create);
                items.push((kinds::VIEW_OP, create));
                (id, base, 0, claim.clone(), false)
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
        let commit = encode(&view_op(Some(prev), ViewOpKind::Commit { change: cid }));
        let new_head = ViewHead {
            head: Hash::of(&commit),
            base,
            len: len0 + 1,
            owner,
            pinned,
            closed: false,
            last_write_ms: p.at_ms,
        };
        items.push((kinds::VIEW_OP, commit));
        items.extend(self.ref_items_pub(vec![(view_ref(name), encode(&new_head))]));
        let id = self.write(items)?[change_pos];
        if id != cid {
            return Err(StoreError::Decode(format!(
                "change id mismatch: view names {cid}, written {id}"
            )));
        }
        Ok(id)
    }

    /// Proposes view `view`'s included changes as a new proposal: the proposal, its ref
    /// `proposal/<id>` (the latest version, itself) and `open/<view>` (its stable id, itself) in
    /// one write. Returns the proposal's id.
    pub fn propose(&mut self, view: &str, title: &str, by: Provenance) -> Result<Hash, StoreError> {
        let changes = self.included(view)?.as_ref().clone();
        let p = Proposal {
            stable: None,
            prev: None,
            title: title.to_string(),
            view: view.to_string(),
            changes,
            alternative: None,
            provenance: by,
            canon_at: self.canon_head()?,
        };
        let id = Hash::of(&encode(&p));
        self.write_version(&p, id)
    }

    /// Writes the next version of proposal `prev`: its changes followed by `add`, with `stable`
    /// the first version's id and `prev` the version it follows, and moves `proposal/<stable>` to
    /// it. Returns the new version's id.
    pub fn propose_version(&mut self, prev: &Hash, add: &[Hash]) -> Result<Hash, StoreError> {
        let mut p = self.proposal(prev)?;
        p.changes.extend_from_slice(add);
        self.next_version(prev, p)
    }

    /// Writes the next version of proposal `prev` with `changes`, `title` and `by` in place of
    /// its own (the view and the alternative carry over), and moves `proposal/<stable>` to it.
    /// Returns the new version's id.
    pub fn propose_next(
        &mut self,
        prev: &Hash,
        changes: Vec<Hash>,
        title: &str,
        by: Provenance,
    ) -> Result<Hash, StoreError> {
        let p = Proposal {
            changes,
            title: title.to_string(),
            provenance: by,
            ..self.proposal(prev)?
        };
        self.next_version(prev, p)
    }

    /// Writes `p` as the version after `prev`: `stable` the first version's id, `prev` the
    /// version it follows, `canon_at` canon's head now.
    fn next_version(&mut self, prev: &Hash, p: Proposal) -> Result<Hash, StoreError> {
        let stable = p.stable.unwrap_or(*prev);
        let next = Proposal {
            stable: Some(stable),
            prev: Some(*prev),
            canon_at: self.canon_head()?,
            ..p
        };
        self.write_version(&next, stable)
    }

    /// Writes proposal version `p` with the refs `proposal/<stable>` = its id and
    /// `open/<p.view>` = `stable`, in one write. Returns its id.
    fn write_version(&mut self, p: &Proposal, stable: Hash) -> Result<Hash, StoreError> {
        let bytes = encode(p);
        let id = Hash::of(&bytes);
        let mut items = vec![(kinds::PROPOSAL, bytes)];
        items.extend(self.ref_items_pub(vec![
            (proposal_ref(&stable), id.0.to_vec()),
            (open_ref(&p.view), stable.0.to_vec()),
        ]));
        self.write(items)?;
        Ok(id)
    }

    /// Lands proposal version `proposal` as is: `land_atomic` with no acceptance change, no
    /// chosen alternative and no relay. Returns the landing id.
    pub fn land(&mut self, proposal: &Hash, approver: Provenance) -> Result<Hash, StoreError> {
        Ok(self
            .land_atomic(LandRequest {
                proposal: *proposal,
                accept: None,
                approver,
                chosen: None,
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
        let bytes = postcard::to_allocvec(tag).expect("encode tag");
        let id = Hash::of(&bytes);
        let mut items = vec![(kinds::TAG, bytes)];
        items.extend(self.ref_items_pub(vec![(tag_ref(&tag.name), id.0.to_vec())]));
        self.write(items)?;
        Ok(id)
    }
}
