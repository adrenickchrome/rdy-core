use rdy_model::{Block, BlockId, Op, StateSnapshot, tree};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default)]
pub struct Materialized {
    pub snapshot: StateSnapshot,
    pub blocks: BTreeMap<BlockId, Block>,
    children: BTreeMap<Option<BlockId>, BTreeSet<(String, BlockId)>>,
    /// Docs and folders with a stored name, by (parent, lower-cased name).
    names: BTreeMap<(Option<BlockId>, String), BTreeSet<BlockId>>,
    /// How many docs without a stored name (from M1) each parent holds.
    nameless: BTreeMap<Option<BlockId>, usize>,
}

/// Why a path did not resolve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathMiss {
    /// The path of the deepest folder reached; None means the top level.
    pub reached: Option<String>,
    /// The names under it.
    pub children: Vec<String>,
    /// The names that matched the failing segment ignoring case, when more than one did.
    pub ambiguous: Vec<String>,
}

impl Materialized {
    pub fn build(snapshot: StateSnapshot, blocks: BTreeMap<BlockId, Block>) -> Materialized {
        let mut m = Materialized {
            snapshot,
            blocks: BTreeMap::new(),
            children: BTreeMap::new(),
            names: BTreeMap::new(),
            nameless: BTreeMap::new(),
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
        if tree::is_tree_type(&b.block_type) {
            match b.field_str("name") {
                Some(n) => {
                    self.names
                        .entry((b.parent.clone(), n.to_ascii_lowercase()))
                        .or_default()
                        .insert(b.id.clone());
                }
                None => *self.nameless.entry(b.parent.clone()).or_default() += 1,
            }
        }
        self.snapshot.blocks.insert(b.id.clone(), b.version_hash());
        self.blocks.insert(b.id.clone(), b);
    }

    fn remove(&mut self, id: &BlockId) {
        if let Some(old) = self.blocks.remove(id) {
            if let Some(set) = self.children.get_mut(&old.parent) {
                set.remove(&(old.order.clone(), old.id.clone()));
            }
            if tree::is_tree_type(&old.block_type) {
                match old.field_str("name") {
                    Some(n) => {
                        let key = (old.parent.clone(), n.to_ascii_lowercase());
                        if let Some(set) = self.names.get_mut(&key) {
                            set.remove(&old.id);
                            if set.is_empty() {
                                self.names.remove(&key);
                            }
                        }
                    }
                    None => {
                        if let Some(c) = self.nameless.get_mut(&old.parent) {
                            *c = c.saturating_sub(1);
                        }
                    }
                }
            }
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

    /// The name of every doc and folder directly under `parent`: its stored name or, for a doc
    /// from M1 without one, its effective name (default name, de-duplicated in id order).
    pub fn names_under(&self, parent: Option<&BlockId>) -> BTreeMap<BlockId, String> {
        let kids: Vec<&Block> = self
            .children(parent)
            .into_iter()
            .filter(|b| tree::is_tree_type(&b.block_type))
            .collect();
        let mut out = BTreeMap::new();
        let mut taken = BTreeSet::new();
        for b in &kids {
            if let Some(n) = b.field_str("name") {
                taken.insert(n.to_ascii_lowercase());
                out.insert(b.id.clone(), n.to_string());
            }
        }
        let mut nameless: Vec<&Block> = kids
            .into_iter()
            .filter(|b| b.field_str("name").is_none())
            .collect();
        nameless.sort_by(|a, b| a.id.cmp(&b.id));
        for b in nameless {
            let n = tree::dedupe(&tree::default_name(b.field_str("title"), &b.id), &taken);
            taken.insert(n.to_ascii_lowercase());
            out.insert(b.id.clone(), n);
        }
        out
    }

    pub fn name_of(&self, id: &BlockId) -> Option<String> {
        let b = self.get(id)?;
        if !tree::is_tree_type(&b.block_type) {
            return None;
        }
        match b.field_str("name") {
            Some(n) => Some(n.to_string()),
            None => self.names_under(b.parent.as_ref()).remove(id),
        }
    }

    pub fn path_of(&self, id: &BlockId) -> Option<String> {
        let mut cur = self.get(id)?;
        for _ in 0..=self.blocks.len() {
            if tree::is_tree_type(&cur.block_type) {
                break;
            }
            cur = self.get(cur.parent.as_ref()?)?;
        }
        let mut parts = vec![];
        let mut node = Some(cur);
        for _ in 0..=self.blocks.len() {
            let Some(b) = node else { break };
            parts.push(self.name_of(&b.id)?);
            node = b.parent.as_ref().and_then(|p| self.get(p));
        }
        parts.reverse();
        Some(parts.join("/"))
    }

    pub fn child_named(
        &self,
        parent: Option<&BlockId>,
        name: &str,
    ) -> Result<BlockId, Vec<String>> {
        let lower = name.to_ascii_lowercase();
        if self.nameless.get(&parent.cloned()).copied().unwrap_or(0) == 0 {
            let hits: Vec<&BlockId> = self
                .names
                .get(&(parent.cloned(), lower))
                .map(|s| s.iter().collect())
                .unwrap_or_default();
            if let Some(exact) = hits
                .iter()
                .find(|id| self.get(id).and_then(|b| b.field_str("name")) == Some(name))
            {
                return Ok((*exact).clone());
            }
            return match hits.as_slice() {
                [one] => Ok((*one).clone()),
                many => Err(many.iter().filter_map(|id| self.name_of(id)).collect()),
            };
        }
        let names = self.names_under(parent);
        if let Some((id, _)) = names.iter().find(|(_, n)| n.as_str() == name) {
            return Ok(id.clone());
        }
        let hits: Vec<(&BlockId, &String)> = names
            .iter()
            .filter(|(_, n)| n.to_ascii_lowercase() == lower)
            .collect();
        match hits.as_slice() {
            [(id, _)] => Ok((*id).clone()),
            many => Err(many.iter().map(|(_, n)| (*n).clone()).collect()),
        }
    }

    pub fn resolve_path(&self, path: &str) -> Result<BlockId, PathMiss> {
        let mut cur: Option<BlockId> = None;
        for seg in path.split('/').filter(|s| !s.is_empty()) {
            match self.child_named(cur.as_ref(), seg) {
                Ok(id) => cur = Some(id),
                Err(ambiguous) => {
                    return Err(PathMiss {
                        reached: cur.as_ref().and_then(|c| self.path_of(c)),
                        children: self.names_under(cur.as_ref()).into_values().collect(),
                        ambiguous,
                    });
                }
            }
        }
        cur.ok_or_else(|| PathMiss {
            reached: None,
            children: self.names_under(None).into_values().collect(),
            ambiguous: vec![],
        })
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
