use crate::error::StoreError;
use crate::kinds;
use crate::Store;
use rdy_model::{decision, Block, BlockId, Change, Hash, LandingRecord, Op, Proposal, Provenance, StateSnapshot};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewRecord {
    pub base: Option<Hash>,
    pub changes: Vec<Hash>,
}

fn view_ref(name: &str) -> String {
    format!("view/{name}")
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

fn check_base(store: &Store, state: &StateSnapshot, id: &BlockId, base: Hash) -> Result<(), StoreError> {
    let actual = state.blocks.get(id).copied();
    if actual != Some(base) {
        return Err(StoreError::StaleBase { id: id.clone(), expected: base, actual });
    }
    if decision::is_frozen(&store.get_block(&base)?) {
        return Err(StoreError::Frozen(id.clone()));
    }
    Ok(())
}

impl Store {
    pub fn canon_head(&self) -> Result<Option<Hash>, StoreError> {
        self.get_ref("canon")?.map(|b| crate::index::hash_from(&b, "canon ref")).transpose()
    }

    pub fn snapshot_at(&self, landing: &Hash) -> Result<StateSnapshot, StoreError> {
        let rec: LandingRecord = self.get(landing)?;
        self.get(&rec.state)
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

    pub fn view(&self, name: &str) -> Result<ViewRecord, StoreError> {
        match self.get_ref(&view_ref(name))? {
            Some(bytes) => postcard::from_bytes(&bytes).map_err(|e| StoreError::Decode(e.to_string())),
            None => Ok(ViewRecord { base: self.canon_head()?, changes: vec![] }),
        }
    }

    fn replay(&self, mut state: StateSnapshot, changes: &[Hash]) -> Result<StateSnapshot, StoreError> {
        for c in changes {
            let change: Change = self.get(c)?;
            for op in &change.ops {
                apply_op(self, &mut state, op)?;
            }
        }
        Ok(state)
    }

    pub fn view_state(&self, name: &str) -> Result<StateSnapshot, StoreError> {
        let v = self.view(name)?;
        self.replay(self.state_of(v.base)?, &v.changes)
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
    pub fn commit_to_view_at(&mut self, name: &str, change: Change, current: &StateSnapshot) -> Result<Hash, StoreError> {
        let mut view = self.view(name)?;
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
        // The view record must name the change id, which depends on deps; use the same
        // computation as commit_change so the change and its view update land in one write.
        let mut probe = change.clone();
        probe.deps = self.change_deps(&probe)?;
        let expected = probe.id();
        view.changes.push(expected);
        let rec = postcard::to_allocvec(&view).expect("encode view");
        let id = self.commit_change(change, vec![(view_ref(name), rec)])?;
        if id != expected {
            return Err(StoreError::Decode(format!("change id mismatch: view names {expected}, written {id}")));
        }
        Ok(id)
    }

    pub fn propose(&mut self, view: &str, title: &str, by: Provenance) -> Result<Hash, StoreError> {
        let v = self.view(view)?;
        let p = Proposal { title: title.to_string(), view: view.to_string(), changes: v.changes, provenance: by };
        self.put(kinds::PROPOSAL, &p)
    }

    pub fn land(&mut self, proposal: &Hash, approver: Provenance) -> Result<Hash, StoreError> {
        let p: Proposal = self.get(proposal)?;
        let head = self.canon_head()?;
        let state = self.replay(self.canon_state()?, &p.changes)?;
        let snap = postcard::to_allocvec(&state).expect("encode snapshot");
        let rec = LandingRecord { proposal: *proposal, prev: head, state: state.id(), approved_by: approver };
        let landing_id = rec.id();
        let mut view = self.view(&p.view)?;
        view.changes.retain(|c| !p.changes.contains(c));
        view.base = Some(landing_id);
        let mut items = vec![(kinds::SNAPSHOT, snap), (kinds::LANDING, postcard::to_allocvec(&rec).expect("encode landing"))];
        let refs = vec![
            ("canon".to_string(), landing_id.0.to_vec()),
            (view_ref(&p.view), postcard::to_allocvec(&view).expect("encode view")),
        ];
        items.extend(self.ref_items_pub(refs));
        self.write(items)?;
        Ok(landing_id)
    }

    pub fn landings(&self) -> Result<Vec<(Hash, LandingRecord)>, StoreError> {
        let mut out = vec![];
        let mut cur = self.canon_head()?;
        while let Some(h) = cur {
            let rec: LandingRecord = self.get(&h)?;
            cur = rec.prev;
            out.push((h, rec));
        }
        Ok(out)
    }

    pub fn blocks(&self, state: &StateSnapshot) -> Result<BTreeMap<BlockId, Block>, StoreError> {
        state.blocks.iter().map(|(id, v)| Ok((id.clone(), self.get_block(v)?))).collect()
    }
}
