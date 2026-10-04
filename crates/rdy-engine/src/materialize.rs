use rdy_model::{Block, BlockId, Op, StateSnapshot};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default)]
pub struct Materialized {
    pub snapshot: StateSnapshot,
    pub blocks: BTreeMap<BlockId, Block>,
    children: BTreeMap<Option<BlockId>, BTreeSet<(String, BlockId)>>,
}

impl Materialized {
    pub fn build(snapshot: StateSnapshot, blocks: BTreeMap<BlockId, Block>) -> Materialized {
        let mut m = Materialized {
            snapshot,
            blocks: BTreeMap::new(),
            children: BTreeMap::new(),
        };
        for (_, b) in blocks {
            m.insert(b);
        }
        m
    }

    fn insert(&mut self, b: Block) {
        self.children
            .entry(b.parent.clone())
            .or_default()
            .insert((b.order.clone(), b.id.clone()));
        self.snapshot.blocks.insert(b.id.clone(), b.version_hash());
        self.blocks.insert(b.id.clone(), b);
    }

    fn remove(&mut self, id: &BlockId) {
        if let Some(old) = self.blocks.remove(id)
            && let Some(set) = self.children.get_mut(&old.parent)
        {
            set.remove(&(old.order.clone(), old.id.clone()));
        }
        self.snapshot.blocks.remove(id);
    }

    pub fn get(&self, id: &BlockId) -> Option<&Block> {
        self.blocks.get(id)
    }

    pub fn children(&self, parent: Option<&BlockId>) -> Vec<&Block> {
        self.children
            .get(&parent.cloned())
            .map(|set| {
                set.iter()
                    .filter_map(|(_, id)| self.blocks.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn doc_of(&self, id: &BlockId) -> Option<&Block> {
        let mut cur = self.blocks.get(id)?;
        for _ in 0..=self.blocks.len() {
            if cur.block_type == "doc" {
                return Some(cur);
            }
            cur = self.blocks.get(cur.parent.as_ref()?)?;
        }
        None
    }

    pub fn apply(&mut self, ops: &[Op]) {
        for op in ops {
            match op {
                Op::Create { block } | Op::Update { block, .. } => {
                    self.remove(&block.id);
                    self.insert(block.clone());
                }
                Op::Delete { id, .. } => self.remove(id),
            }
        }
    }
}
