use crate::Store;
use crate::error::StoreError;
use crate::kinds;
use crate::merge::{MergeChange, Merged, merge};
use crate::store::encode;
use crate::view::{ViewHead, ViewOp, ViewOpKind};
use rdy_model::{
    Assurance, Block, BlockId, Change, Hash, LandingRecord, Op, Proposal, Provenance,
    StateSnapshot, Tag, decision,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

/// What `Store::view` returns: a view's base landing and the changes it includes, in order. It is
/// not stored; the view's chain and its `ViewHead` are (chain1).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewRecord {
    pub base: Option<Hash>,
    pub changes: Vec<Hash>,
}

fn view_ref(name: &str) -> String {
    format!("view/{name}")
}

fn tag_ref(name: &str) -> String {
    format!("tag/{name}")
}

pub fn apply_op(store: &Store, state: &mut StateSnapshot, op: &Op) -> Result<(), StoreError> {
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
    store: &Store,
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

impl Store {
    pub fn canon_head(&self) -> Result<Option<Hash>, StoreError> {
        self.get_ref("canon")?
            .map(|b| crate::index::hash_from(&b, "canon ref"))
            .transpose()
    }

    pub fn snapshot_at(&self, landing: &Hash) -> Result<StateSnapshot, StoreError> {
        self.get(&self.landing(landing)?.state)
    }

    fn state_of(&self, landing: Option<Hash>) -> Result<StateSnapshot, StoreError> {
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
        match self.view_head(name)? {
            Some(head) => Ok(ViewRecord {
                base: head.base,
                changes: self.included(name)?.as_ref().clone(),
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
        let input: Vec<MergeChange<'_>> = changes
            .iter()
            .map(|c| MergeChange {
                id: c.id(),
                deps: &c.deps,
                ops: &c.ops,
            })
            .collect();
        Ok(merge(base, &input, &|v| self.producer(v).ok().flatten()))
    }

    /// View `name`'s included changes merged onto its base (canon's head when it has no head).
    pub fn view_merge(&self, name: &str) -> Result<Merged, StoreError> {
        let base = match self.view_head(name)? {
            Some(head) => head.base,
            None => self.canon_head()?,
        };
        let changes = self
            .included(name)?
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
    /// engine's gate and atomic landings share.
    pub fn merge_checked(
        &self,
        canon: &StateSnapshot,
        changes: &[Change],
    ) -> Result<Merged, StoreError> {
        let merged = self.merge_onto(canon, changes)?;
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
        let by_id: HashMap<Hash, &Change> = changes.iter().map(|c| (c.id(), c)).collect();
        for id in &merged.order {
            for op in &by_id[id].ops {
                if let Some(base) = op.base()
                    && decision::is_frozen(&self.get_block(&base)?)
                {
                    return Err(StoreError::Frozen(op.block_id().clone()));
                }
            }
        }
        Ok(merged)
    }

    pub fn commit_to_view(&mut self, name: &str, change: Change) -> Result<Hash, StoreError> {
        let state = self.view_state(name)?;
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
        // apply_op only reads and writes the ids the ops name, so seed a small state with those
        let mut state = StateSnapshot::default();
        for op in &change.ops {
            let id = match op {
                Op::Create { block } | Op::Update { block, .. } => &block.id,
                Op::Delete { id, .. } => id,
            };
            if let Some(v) = current.blocks.get(id) {
                state.blocks.insert(id.clone(), *v);
            }
        }
        for op in &change.ops {
            apply_op(self, &mut state, op)?;
        }
        // The commit op names the change id, which depends on deps, so the change, its view op
        // and the view's head land in one write.
        let mut change = change;
        let (mut items, change_pos) = self.change_frames(&mut change, Some(name))?;
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
        let (create, prev, base, len0, owner, pinned) = match self.view_head(name)? {
            Some(head) => (
                None,
                head.head,
                head.base,
                head.len,
                head.owner,
                head.pinned,
            ),
            None => {
                let base = self.canon_head()?;
                let create = encode(&view_op(
                    None,
                    ViewOpKind::Create {
                        base,
                        owner: None,
                        pinned: false,
                    },
                ));
                let id = Hash::of(&create);
                (Some(create), id, base, 0, None, false)
            }
        };
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
        items.extend(create.map(|c| (kinds::VIEW_OP, c)));
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
        };
        self.put(kinds::PROPOSAL, &p)
    }

    /// Writes the next version of proposal `prev`: its changes followed by `add`, with `stable`
    /// the first version's id and `prev` the version it follows. Returns the new version's id.
    pub fn propose_version(&mut self, prev: &Hash, add: &[Hash]) -> Result<Hash, StoreError> {
        let p = self.proposal(prev)?;
        let mut changes = p.changes;
        changes.extend_from_slice(add);
        let next = Proposal {
            stable: Some(p.stable.unwrap_or(*prev)),
            prev: Some(*prev),
            title: p.title,
            view: p.view,
            changes,
            alternative: p.alternative,
            provenance: p.provenance,
        };
        self.put(kinds::PROPOSAL, &next)
    }

    pub fn land(&mut self, proposal: &Hash, approver: Provenance) -> Result<Hash, StoreError> {
        let p = self.proposal(proposal)?;
        let head = self.canon_head()?;
        let changes = p
            .changes
            .iter()
            .map(|c| self.change(c))
            .collect::<Result<Vec<_>, _>>()?;
        let state = self.merge_checked(&self.canon_state()?, &changes)?.state;
        let snap = postcard::to_allocvec(&state).expect("encode snapshot");
        let rec = LandingRecord {
            proposal: *proposal,
            prev: head,
            state: state.id(),
            approved_by: approver,
            changes: p.changes.clone(),
            chosen: None,
            checks: vec![],
            assurance: Assurance::Local,
            relayed_by: None,
            relay_evidence: None,
            approved_version: Some(*proposal),
            rebased_from: vec![],
        };
        let landing_id = rec.id();
        // the proposing view checkpoints: the landed changes drop out, the rest remain
        let head = self
            .view_head(&p.view)?
            .ok_or_else(|| StoreError::UnknownView(p.view.clone()))?;
        let landed: HashSet<&Hash> = p.changes.iter().collect();
        let (dropped, remaining): (Vec<Hash>, Vec<Hash>) = self
            .included(&p.view)?
            .iter()
            .partition(|c| landed.contains(c));
        let rebase = encode(&ViewOp {
            view: p.view.clone(),
            prev: Some(head.head),
            op: ViewOpKind::Rebase {
                landing: landing_id,
                dropped,
                remaining: remaining.clone(),
            },
            actor: rec.approved_by.actor.clone(),
            run: rec.approved_by.run.clone(),
            at_ms: rec.approved_by.at_ms,
        });
        let new_head = ViewHead {
            head: Hash::of(&rebase),
            base: Some(landing_id),
            len: remaining.len() as u64,
            owner: head.owner,
            pinned: head.pinned,
            closed: head.closed,
            last_write_ms: head.last_write_ms,
        };
        let mut items = vec![
            (kinds::SNAPSHOT, snap),
            (
                kinds::LANDING,
                postcard::to_allocvec(&rec).expect("encode landing"),
            ),
            (kinds::VIEW_OP, rebase),
        ];
        let refs = vec![
            ("canon".to_string(), landing_id.0.to_vec()),
            (view_ref(&p.view), encode(&new_head)),
        ];
        items.extend(self.ref_items_pub(refs));
        self.write(items)?;
        Ok(landing_id)
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
