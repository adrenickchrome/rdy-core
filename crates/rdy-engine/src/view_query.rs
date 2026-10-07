//! Views as a reader sees them (vls1, cmp1): the list of views with their counts and idle flags,
//! one view's changes, conflicts and op chain, and a block-level compare of two scopes. Every
//! answer is compact: ids, versions and field names, never block bodies.
use crate::error::*;
use crate::read_state::ReadState;
use crate::views::{
    ConflictInfo, OwnerInfo, ViewHeadInfo, conflict_infos, head_info, kind_name, net_undone,
};
use crate::{Materialized, Scope};
use rdy_model::{Block, BlockId, Change, Hash, StateSnapshot, tree};
use rdy_store::view::{ViewOp, ViewOpKind};
use rdy_store::{MergeChange, merge_on_blocks};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// One row of `rdy view ls`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ViewSummary {
    pub name: String,
    pub owner: Option<OwnerInfo>,
    /// The landing the view was created or last rebased on.
    pub base: Option<String>,
    /// The landing the view reads on (canon's head for a view that floats).
    pub effective_base: Option<String>,
    pub pinned: bool,
    pub closed: bool,
    /// How many changes the view includes.
    pub changes: usize,
    /// How many distinct blocks those changes touch.
    pub blocks: usize,
    /// How many blocks the view's merge records a conflict on (0 for a closed view).
    pub conflicts: usize,
    pub last_write_ms: i64,
    /// Open, and nothing written for more than `views.idle_days`.
    pub idle: bool,
}

/// A change a view includes, with the blocks its ops touch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChangeSummary {
    pub id: String,
    pub actor: String,
    pub run: Option<String>,
    pub at_ms: i64,
    pub message: String,
    pub blocks: Vec<String>,
    /// The nonce of the working-copy commit that wrote the change (wccm1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_nonce: Option<String>,
}

/// One op of a view's chain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OpSummary {
    pub id: String,
    pub kind: String,
    pub actor: String,
    pub run: Option<String>,
    pub at_ms: i64,
    /// A later, live `Undo` on the chain reverses this op.
    pub undone: bool,
}

/// `rdy view show`: a view's head, its changes, its conflicts and the chain since its latest
/// checkpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ViewDetail {
    pub head: ViewHeadInfo,
    pub effective_base: Option<String>,
    pub changes: Vec<ChangeSummary>,
    pub blocks: usize,
    pub conflicts: Vec<ConflictInfo>,
    /// From the head back to the latest checkpoint inclusive, newest first.
    pub ops: Vec<OpSummary>,
}

/// One block that differs between two scopes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CompareEntry {
    pub id: String,
    pub block_type: String,
    /// The block's path, for a folder or a doc.
    pub path: Option<String>,
    /// The field names whose values differ, then `body`, `links` and `order` when they differ
    /// (`order` only under the same parent: a moved block's new key is part of the move).
    pub fields: Vec<String>,
    /// For a moved block: its parent in `b`.
    pub from_parent: Option<String>,
    /// For a moved block: its parent in `a`.
    pub to_parent: Option<String>,
    /// For a conflicted block: the conflict's id.
    pub conflict: Option<String>,
}

/// What differs from `b` to `a`: `added` blocks are in `a` only, `deleted` ones in `b` only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Compare {
    pub a: String,
    pub b: String,
    pub added: Vec<CompareEntry>,
    pub deleted: Vec<CompareEntry>,
    pub changed: Vec<CompareEntry>,
    pub moved: Vec<CompareEntry>,
    pub conflicted: Vec<CompareEntry>,
}

/// The scope as the compare shows it: `canon`, the view's name, or `landing:<short>`.
fn scope_text(s: &Scope) -> String {
    match s {
        Scope::Canon => "canon".to_string(),
        Scope::View(v) => v.clone(),
        Scope::At(h) => format!("landing:{}", h.short()),
    }
}

fn is_checkpoint(k: &ViewOpKind) -> bool {
    matches!(
        k,
        ViewOpKind::Create { .. } | ViewOpKind::Fork { .. } | ViewOpKind::Rebase { .. }
    )
}

/// The block ids `changes`' ops touch, each once, in first-seen order.
fn touched(ops: &[rdy_model::Op]) -> Vec<BlockId> {
    let mut seen = HashSet::new();
    ops.iter()
        .map(|op| op.block_id().clone())
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

/// The names whose values differ between `a` and `b`, then `body`, `links` and `order`. A block
/// under a new parent gets a new order key as part of the move, so `order` is listed only when
/// the parent is the same: a pure move is listed under `moved` only.
fn differing(a: &Block, b: &Block) -> Vec<String> {
    let names: BTreeSet<&String> = a.fields.keys().chain(b.fields.keys()).collect();
    let mut out: Vec<String> = names
        .into_iter()
        .filter(|k| a.fields.get(*k) != b.fields.get(*k))
        .cloned()
        .collect();
    if a.body != b.body {
        out.push("body".into());
    }
    if a.links != b.links {
        out.push("links".into());
    }
    if a.order != b.order && a.parent == b.parent {
        out.push("order".into());
    }
    out
}

fn entry(block: &Block, path_in: &Materialized) -> CompareEntry {
    CompareEntry {
        id: block.id.to_string(),
        block_type: block.block_type.clone(),
        path: if tree::is_tree_type(&block.block_type) {
            path_in.path_of(&block.id)
        } else {
            None
        },
        fields: vec![],
        from_parent: None,
        to_parent: None,
        conflict: None,
    }
}

impl ReadState {
    /// Every view (open ones only unless `all`), sorted by name. A view is idle when it is open
    /// and its last write is more than `idle_days` days before `now_ms`.
    pub fn view_list(
        &self,
        all: bool,
        idle_days: u32,
        now_ms: i64,
    ) -> Result<Vec<ViewSummary>, RdyError> {
        let window = i64::from(idle_days).saturating_mul(86_400_000);
        // the base states the counts merge onto, each read once
        let mut bases: HashMap<Option<Hash>, StateSnapshot> = HashMap::new();
        let mut out = vec![];
        for name in self.view_names() {
            let Some(head) = self.view_head(&name) else {
                continue;
            };
            if head.closed && !all {
                continue;
            }
            let included = self.included(&name);
            let changes = included
                .iter()
                .map(|ch| self.reader().change(ch))
                .collect::<Result<Vec<Change>, _>>()?;
            let blocks: BTreeSet<BlockId> = changes
                .iter()
                .flat_map(|c| c.ops.iter().map(|op| op.block_id().clone()))
                .collect();
            let conflicts = if head.closed {
                0
            } else {
                self.conflict_count(&name, &changes, &blocks, &mut bases)?
            };
            out.push(ViewSummary {
                owner: head.owner.as_ref().map(OwnerInfo::from),
                base: head.base.map(|b| b.to_string()),
                effective_base: self.effective_base(&name).map(|b| b.to_string()),
                pinned: head.pinned,
                closed: head.closed,
                changes: included.len(),
                blocks: blocks.len(),
                conflicts,
                last_write_ms: head.last_write_ms,
                idle: !head.closed && now_ms.saturating_sub(head.last_write_ms) > window,
                name,
            });
        }
        Ok(out)
    }

    /// How many blocks view `name`'s merge records a conflict on, without building the view:
    /// the build's count when it is built, else a merge of its `changes` (touching `blocks`)
    /// onto its base restricted to those blocks, whose conflicts are the full merge's
    /// (`merge_on_blocks`), counted once per entry. A cold `rdy view ls` thus costs a merge on
    /// each view's own blocks, not a build of every view.
    fn conflict_count(
        &self,
        name: &str,
        changes: &[Change],
        blocks: &BTreeSet<BlockId>,
        bases: &mut HashMap<Option<Hash>, StateSnapshot>,
    ) -> Result<usize, RdyError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        let Some(entry) = self.views.get(name) else {
            return Ok(0);
        };
        if entry.floats() || entry.included.is_empty() {
            return Ok(0);
        }
        if let Some(b) = entry.built_ok() {
            return Ok(b.conflicts.len());
        }
        if let Some(n) = entry.conflict_count.get() {
            return Ok(*n);
        }
        if let std::collections::hash_map::Entry::Vacant(slot) = bases.entry(entry.base) {
            slot.insert(self.reader().state_of(entry.base)?);
        }
        let input: Vec<MergeChange<'_>> = entry
            .included
            .iter()
            .zip(changes)
            .map(|(id, c)| MergeChange {
                id: *id,
                deps: &c.deps,
                ops: &c.ops,
            })
            .collect();
        let n = merge_on_blocks(
            &bases[&entry.base],
            &input,
            &|v| self.reader().producer(v).ok().flatten(),
            blocks,
        )
        .conflicts
        .len();
        let _ = entry.conflict_count.set(n);
        Ok(n)
    }

    /// The change whose `links.client_nonce` is `nonce` (wccm1, R-I6): view `view`'s included
    /// changes newest first, then canon's landings newest first (each landing's changes last
    /// first); `None` once `limit` changes are read without a match.
    pub fn change_by_nonce(
        &self,
        view: &str,
        nonce: &str,
        limit: usize,
    ) -> Result<Option<Hash>, RdyError> {
        let reader = self.reader();
        let mut read = 0usize;
        let mut seen: HashSet<Hash> = HashSet::new();
        // Some(true): found; Some(false): read (or seen) and not it; None: the limit is reached
        let mut check = |h: &Hash| -> Result<Option<bool>, RdyError> {
            if !seen.insert(*h) {
                return Ok(Some(false));
            }
            if read >= limit {
                return Ok(None);
            }
            read += 1;
            let c = reader.change(h)?;
            Ok(Some(c.links.client_nonce.as_deref() == Some(nonce)))
        };
        for h in self.included(view).iter().rev() {
            match check(h)? {
                Some(true) => return Ok(Some(*h)),
                Some(false) => {}
                None => return Ok(None),
            }
        }
        for landing in self.landing_walk() {
            let (_, rec) = landing?;
            for h in rec.changes.iter().rev() {
                match check(h)? {
                    Some(true) => return Ok(Some(*h)),
                    Some(false) => {}
                    None => return Ok(None),
                }
            }
        }
        Ok(None)
    }

    /// View `name` in detail (FND-7): its head, each change it includes with the blocks it
    /// touches, its conflicts, and its op chain back to the latest checkpoint.
    pub fn view_show(&self, name: &str) -> Result<ViewDetail, RdyError> {
        let info = head_info(self, name)?;
        let reader = self.reader();
        let mut changes = vec![];
        let mut all_blocks = HashSet::new();
        for ch in self.included(name).iter() {
            let change = reader.change(ch)?;
            let blocks = touched(&change.ops);
            all_blocks.extend(blocks.iter().cloned());
            changes.push(ChangeSummary {
                id: ch.to_string(),
                actor: change.provenance.actor.clone(),
                run: change.provenance.run.clone(),
                at_ms: change.provenance.at_ms,
                message: change.message.clone(),
                blocks: blocks.iter().map(|b| b.to_string()).collect(),
                client_nonce: change.links.client_nonce.clone(),
            });
        }
        let head: Hash = info
            .head
            .parse()
            .map_err(|_| RdyError::new(E_INTERNAL, "a view head is not a hash", "run rdy fsck"))?;
        let mut chain: Vec<(Hash, ViewOp)> = vec![];
        let mut cur = Some(head);
        while let Some(h) = cur {
            let op = reader.view_op(&h)?;
            cur = if is_checkpoint(&op.op) { None } else { op.prev };
            chain.push((h, op));
        }
        let undone = net_undone(&chain);
        let ops = chain
            .iter()
            .map(|(h, op)| OpSummary {
                id: h.to_string(),
                kind: kind_name(&op.op).to_string(),
                actor: op.actor.clone(),
                run: op.run.clone(),
                at_ms: op.at_ms,
                undone: undone.contains_key(h),
            })
            .collect();
        Ok(ViewDetail {
            effective_base: info.base.clone(),
            head: info,
            changes,
            blocks: all_blocks.len(),
            conflicts: conflict_infos(self, &self.conflicts(&Scope::View(name.to_string()))?)?,
            ops,
        })
    }

    /// What differs from `b` to `a`, block by block, sorted by id. `conflicted` lists the blocks
    /// `a`'s merge (and `b`'s, when it is a view) records a conflict on.
    pub fn compare(&self, a: &Scope, b: &Scope) -> Result<Compare, RdyError> {
        let (ma, mb) = (self.materialized(a)?, self.materialized(b)?);
        let ids: BTreeSet<&BlockId> = ma.ids().chain(mb.ids()).collect();
        let mut out = Compare {
            a: scope_text(a),
            b: scope_text(b),
            added: vec![],
            deleted: vec![],
            changed: vec![],
            moved: vec![],
            conflicted: vec![],
        };
        for id in ids {
            match (ma.get(id), mb.get(id)) {
                (Some(x), None) => out.added.push(entry(x, &ma)),
                (None, Some(y)) => out.deleted.push(entry(y, &mb)),
                (Some(x), Some(y)) if x.version_hash() != y.version_hash() => {
                    let mut e = entry(x, &ma);
                    e.fields = differing(x, y);
                    if x.parent != y.parent {
                        let mut mv = e.clone();
                        mv.fields = vec![];
                        mv.from_parent = y.parent.as_ref().map(|p| p.to_string());
                        mv.to_parent = x.parent.as_ref().map(|p| p.to_string());
                        out.moved.push(mv);
                        if !e.fields.is_empty() {
                            out.changed.push(e);
                        }
                    } else {
                        out.changed.push(e);
                    }
                }
                _ => {}
            }
        }
        let mut conflicts = (*self.conflicts(a)?).clone();
        if matches!(b, Scope::View(_)) {
            conflicts.extend(self.conflicts(b)?.iter().cloned());
        }
        let mut by_block: BTreeMap<BlockId, String> = BTreeMap::new();
        let mut kinds: BTreeMap<BlockId, Option<Hash>> = BTreeMap::new();
        for c in &conflicts {
            by_block
                .entry(c.block.clone())
                .or_insert_with(|| c.id.to_string());
            kinds
                .entry(c.block.clone())
                .or_insert_with(|| c.sides.iter().find_map(|s| s.version));
        }
        for (block, conflict) in by_block {
            let mut e = match (ma.get(&block), mb.get(&block)) {
                (Some(x), _) => entry(x, &ma),
                (None, Some(y)) => entry(y, &mb),
                // deleted on both scopes: the type comes from a side's version
                (None, None) => {
                    let block_type = kinds
                        .get(&block)
                        .copied()
                        .flatten()
                        .and_then(|h| self.reader().get_block(&h).ok())
                        .map(|x| x.block_type)
                        .unwrap_or_default();
                    CompareEntry {
                        id: block.to_string(),
                        block_type,
                        path: None,
                        fields: vec![],
                        from_parent: None,
                        to_parent: None,
                        conflict: None,
                    }
                }
            };
            e.conflict = Some(conflict);
            out.conflicted.push(e);
        }
        Ok(out)
    }
}
