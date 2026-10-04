use rdy_model::{Block, BlockId, Op, StateSnapshot, tree};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

/// The names readers give the docs without a stored name (from M1) under one parent.
#[derive(Debug, Default)]
struct Derived {
    by_id: BTreeMap<BlockId, String>,
    /// Lower-cased derived name → id (derived names are unique ignoring case).
    by_lower: BTreeMap<String, BlockId>,
}

/// `Derived` per parent, computed on first use and dropped when that parent's docs or folders
/// change, so lookups under a parent holding nameless docs don't recompute every call.
#[derive(Debug, Default)]
struct DerivedCache(Mutex<BTreeMap<Option<BlockId>, Arc<Derived>>>);

impl DerivedCache {
    fn lock(&self) -> MutexGuard<'_, BTreeMap<Option<BlockId>, Arc<Derived>>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn forget(&mut self, parent: &Option<BlockId>) {
        self.0
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .remove(parent);
    }
}

impl Clone for DerivedCache {
    fn clone(&self) -> Self {
        DerivedCache(Mutex::new(self.lock().clone()))
    }
}

#[derive(Clone, Debug, Default)]
pub struct Materialized {
    pub snapshot: StateSnapshot,
    pub blocks: BTreeMap<BlockId, Block>,
    children: BTreeMap<Option<BlockId>, BTreeSet<(String, BlockId)>>,
    /// Docs and folders with a stored name, by (parent, lower-cased name).
    names: BTreeMap<(Option<BlockId>, String), BTreeSet<BlockId>>,
    /// How many docs without a stored name (from M1) each parent holds.
    nameless: BTreeMap<Option<BlockId>, usize>,
    derived: DerivedCache,
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
            derived: DerivedCache::default(),
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
            self.derived.forget(&b.parent);
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
                self.derived.forget(&old.parent);
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

    /// The derived names of the docs without a stored name under `parent`.
    fn derived(&self, parent: Option<&BlockId>) -> Arc<Derived> {
        let key = parent.cloned();
        if self.nameless.get(&key).copied().unwrap_or(0) == 0 {
            return Arc::default();
        }
        if let Some(d) = self.derived.lock().get(&key) {
            return d.clone();
        }
        let kids: Vec<&Block> = self
            .children(parent)
            .into_iter()
            .filter(|b| tree::is_tree_type(&b.block_type))
            .collect();
        let mut taken: BTreeSet<String> = kids
            .iter()
            .filter_map(|b| b.field_str("name"))
            .map(|n| n.to_ascii_lowercase())
            .collect();
        let mut nameless: Vec<&Block> = kids
            .into_iter()
            .filter(|b| b.field_str("name").is_none())
            .collect();
        nameless.sort_by(|a, b| a.id.cmp(&b.id));
        let mut d = Derived::default();
        for b in nameless {
            let n = tree::dedupe(&tree::default_name(b.field_str("title"), &b.id), &taken);
            let lower = n.to_ascii_lowercase();
            taken.insert(lower.clone());
            d.by_lower.insert(lower, b.id.clone());
            d.by_id.insert(b.id.clone(), n);
        }
        let d = Arc::new(d);
        self.derived.lock().insert(key, d.clone());
        d
    }

    /// The name of every doc and folder directly under `parent`: its stored name or, for a doc
    /// from M1 without one, its effective name (default name, de-duplicated in id order).
    pub fn names_under(&self, parent: Option<&BlockId>) -> BTreeMap<BlockId, String> {
        let derived = self.derived(parent);
        self.children(parent)
            .into_iter()
            .filter(|b| tree::is_tree_type(&b.block_type))
            .filter_map(|b| {
                let n = match b.field_str("name") {
                    Some(n) => n.to_string(),
                    None => derived.by_id.get(&b.id)?.clone(),
                };
                Some((b.id.clone(), n))
            })
            .collect()
    }

    pub fn name_of(&self, id: &BlockId) -> Option<String> {
        let b = self.get(id)?;
        if !tree::is_tree_type(&b.block_type) {
            return None;
        }
        match b.field_str("name") {
            Some(n) => Some(n.to_string()),
            None => self.derived(b.parent.as_ref()).by_id.get(id).cloned(),
        }
    }

    /// The docs and folders under `parent` whose name, ignoring case, is `lower` (lower-cased).
    pub fn named(&self, parent: Option<&BlockId>, lower: &str) -> Vec<BlockId> {
        let mut out: Vec<BlockId> = self
            .names
            .get(&(parent.cloned(), lower.to_string()))
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        if let Some(id) = self.derived(parent).by_lower.get(lower) {
            out.push(id.clone());
        }
        out
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
        let hits: Vec<(BlockId, String)> = self
            .named(parent, &name.to_ascii_lowercase())
            .into_iter()
            .filter_map(|id| Some((id.clone(), self.name_of(&id)?)))
            .collect();
        if let Some((id, _)) = hits.iter().find(|(_, n)| n == name) {
            return Ok(id.clone());
        }
        match hits.as_slice() {
            [(id, _)] => Ok(id.clone()),
            many => Err(many.iter().map(|(_, n)| n.clone()).collect()),
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
