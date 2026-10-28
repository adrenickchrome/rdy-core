//! Views as a reader sees them (vls1, cmp1): the list of views with their counts and idle flags,
//! one view's changes, conflicts and op chain, and a block-level compare of two scopes. Every
//! answer is compact: ids, versions and field names, never block bodies.
use crate::coord_graph::{AltRef, alt_label, intent_label};
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

/// The answer of a nonce lookup: the change that carries the nonce, if any, how many frames the
/// lookup read, and whether it stopped at its limit before it read everything it searches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NonceLookup {
    pub change: Option<Hash>,
    pub read: usize,
    /// The lookup reached its limit with no match: the change may still be further back.
    pub capped: bool,
}

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
    /// Why a closed view was closed: the reason of its latest close op.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<String>,
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
    /// The intent the change was written under (`in-<short>`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// The alternative set and member the change was written as.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternative: Option<AltRef>,
    /// The full hash of the change this one replays, or null for any other change.
    pub replayed_from: Option<String>,
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
    /// Why the view was closed, on a `close` op.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
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
    /// The field names whose values differ, then `type`, `body`, `links` and `order` when they
    /// differ (`order` only under the same parent: a moved block's new key is part of the move).
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

/// The scope as the compare shows it: `canon`, the view's name, `landing:<short>` or
/// `proposal:<short>`.
fn scope_text(s: &Scope) -> String {
    match s {
        Scope::Canon => "canon".to_string(),
        Scope::View(v) => v.clone(),
        Scope::At(h) => format!("landing:{}", h.short()),
        Scope::Proposal(v) => format!("proposal:{}", v.short()),
        Scope::Inflight => crate::INFLIGHT_SCOPE.to_string(),
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

/// The names whose values differ between `a` and `b`, then `type`, `body`, `links` and `order`. A
/// type change is listed, so a retyped block is changed even when it also moves. A block
/// under a new parent gets a new order key as part of the move, so `order` is listed only when
/// the parent is the same: a pure move is listed under `moved` only.
fn differing(a: &Block, b: &Block) -> Vec<String> {
    let names: BTreeSet<&String> = a.fields.keys().chain(b.fields.keys()).collect();
    let mut out: Vec<String> = names
        .into_iter()
        .filter(|k| a.fields.get(*k) != b.fields.get(*k))
        .cloned()
        .collect();
    if a.block_type != b.block_type {
        out.push("type".into());
    }
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

/// What differs from state `b` to state `a`, block by block, sorted by id: the state rows of
/// [`ReadState::compare`] for two supplied states (spv2, D-CWC91). A review of a profile
/// compares the selected state of each side; an empty side is [`Materialized::default`]. The
/// labels are empty and `conflicted` is empty: a supplied state names no scope, so it holds no
/// merge conflict.
pub fn compare_states(a: &Materialized, b: &Materialized) -> Compare {
    let ids: BTreeSet<&BlockId> = a.ids().chain(b.ids()).collect();
    let mut out = Compare {
        a: String::new(),
        b: String::new(),
        added: vec![],
        deleted: vec![],
        changed: vec![],
        moved: vec![],
        conflicted: vec![],
    };
    for id in ids {
        match (a.get(id), b.get(id)) {
            (Some(x), None) => out.added.push(entry(x, a)),
            (None, Some(y)) => out.deleted.push(entry(y, b)),
            (Some(x), Some(y)) if x.version_hash() != y.version_hash() => {
                let mut e = entry(x, a);
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
    out
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
            let (conflicts, close_reason) = if head.closed {
                (0, self.close_reason(head.head)?)
            } else {
                (
                    self.conflict_count(&name, &changes, &blocks, &mut bases)?,
                    None,
                )
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
                close_reason,
                name,
            });
        }
        Ok(out)
    }

    /// The open views that hold at least one included change, each with its change count, in
    /// name order. A landed view stays open and empty, and nothing closes it, so it is left out
    /// here; `view_list` stays the full list.
    pub fn views_with_changes(&self) -> Vec<(String, usize)> {
        self.view_names()
            .into_iter()
            .filter(|name| self.view_head(name).is_some_and(|h| !h.closed))
            .filter_map(|name| {
                let n = self.included(&name).len();
                (n > 0).then_some((name, n))
            })
            .collect()
    }

    /// The reason of the latest close op on the chain from `head` back to its latest checkpoint.
    fn close_reason(&self, head: Hash) -> Result<Option<String>, RdyError> {
        let reader = self.reader();
        let mut cur = Some(head);
        while let Some(h) = cur {
            let op = reader.view_op(&h)?;
            if let ViewOpKind::Close { reason } = op.op {
                return Ok(Some(reason));
            }
            cur = if is_checkpoint(&op.op) { None } else { op.prev };
        }
        Ok(None)
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
                is_replay: c.links.replayed_from.is_some(),
            })
            .collect();
        let deleters =
            self.reader()
                .merge_deleters(entry.base.as_ref(), &bases[&entry.base], blocks)?;
        // the producer of each base version of `blocks`, read before the merge, so a read error
        // fails the count: read as "no producer", it would settle canon's side
        let mut producers: HashMap<Hash, Option<Hash>> = HashMap::new();
        for v in blocks
            .iter()
            .filter_map(|b| bases[&entry.base].blocks.get(b))
        {
            if !producers.contains_key(v) {
                producers.insert(*v, self.reader().producer(v)?);
            }
        }
        let order_only = self
            .reader()
            .order_only_pairs(&bases[&entry.base], changes.iter().flat_map(|c| &c.ops));
        let n = merge_on_blocks(
            &bases[&entry.base],
            &input,
            &|v| producers.get(v).copied().flatten(),
            &|b| deleters.get(b).copied(),
            &|v, op| order_only.holds(v, op),
            blocks,
        )
        .conflicts
        .len();
        let _ = entry.conflict_count.set(n);
        Ok(n)
    }

    /// The change whose `links.client_nonce` is `nonce` (wccm1, R-I6): view `view`'s included
    /// changes newest first, then canon's landings newest first (each landing's changes last
    /// first); not found (`capped`) once `limit` changes are read without a match.
    pub fn change_by_nonce(
        &self,
        view: &str,
        nonce: &str,
        limit: usize,
    ) -> Result<NonceLookup, RdyError> {
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
        let mut found = None;
        let mut capped = false;
        'scan: {
            for h in self.included(view).iter().rev() {
                match check(h)? {
                    Some(true) => {
                        found = Some(*h);
                        break 'scan;
                    }
                    Some(false) => {}
                    None => {
                        capped = true;
                        break 'scan;
                    }
                }
            }
            for landing in self.landing_walk() {
                let (_, rec) = landing?;
                for h in rec.changes.iter().rev() {
                    match check(h)? {
                        Some(true) => {
                            found = Some(*h);
                            break 'scan;
                        }
                        Some(false) => {}
                        None => {
                            capped = true;
                            break 'scan;
                        }
                    }
                }
            }
        }
        Ok(NonceLookup {
            change: found,
            read,
            capped,
        })
    }

    /// `change_by_nonce` over the changes view `view` gained after the head `since` (msw11): the
    /// probe a working-copy commit makes after an error reply, with `since` the view head the
    /// copy last saw. It walks the view's op chain from its head back to `since` and checks the
    /// change of every op on the way (a commit's, an include's or a fork's). The commit's own op
    /// is always on that walk, also when the view was rebased onto a landing meanwhile, since the
    /// chain keeps every op. A `since` that is not a view op hash (`canon:<hash>`, for a view that
    /// had no head) walks the whole chain. The probe runs on the daemon's writer, so the walk
    /// stops (`capped`) once it has read `limit` frames (ops and changes) with no match: the
    /// caller cannot tell then whether the commit was written.
    pub fn change_by_nonce_since(
        &self,
        view: &str,
        nonce: &str,
        since: &str,
        limit: usize,
    ) -> Result<NonceLookup, RdyError> {
        let stop = since.parse::<Hash>().ok();
        let reader = self.reader();
        let mut read = 0usize;
        let mut seen: HashSet<Hash> = HashSet::new();
        let mut cur = self.view_head(view).map(|h| h.head);
        let capped = |read| NonceLookup {
            change: None,
            read,
            capped: true,
        };
        while let Some(h) = cur {
            if Some(h) == stop {
                break;
            }
            if read >= limit {
                return Ok(capped(read));
            }
            read += 1;
            let op = reader.view_op(&h)?;
            cur = op.prev;
            let changes = match op.op {
                ViewOpKind::Commit { change } => vec![change],
                ViewOpKind::Include { changes } | ViewOpKind::Fork { changes, .. } => changes,
                _ => vec![],
            };
            for c in changes.iter().rev() {
                if !seen.insert(*c) {
                    continue;
                }
                if read >= limit {
                    return Ok(capped(read));
                }
                read += 1;
                if reader.change(c)?.links.client_nonce.as_deref() == Some(nonce) {
                    return Ok(NonceLookup {
                        change: Some(*c),
                        read,
                        capped: false,
                    });
                }
            }
        }
        Ok(NonceLookup {
            change: None,
            read,
            capped: false,
        })
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
                intent: change.links.intent.as_ref().map(intent_label),
                alternative: change.links.alternative.map(|a| AltRef {
                    set: alt_label(&a.set),
                    member: a.member,
                }),
                replayed_from: change.links.replayed_from.map(|h| h.to_string()),
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
                reason: match &op.op {
                    ViewOpKind::Close { reason } => Some(reason.clone()),
                    _ => None,
                },
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

    /// What differs from `b` to `a`, block by block, sorted by id: [`compare_states`] of the two
    /// scopes' states, with the scopes' labels. `conflicted` lists the blocks
    /// `a`'s merge (and `b`'s, when `b` is a view or `inflight`) records a conflict on. Canon
    /// and an as-of scope have no conflicts.
    pub fn compare(&self, a: &Scope, b: &Scope) -> Result<Compare, RdyError> {
        let (ma, mb) = (self.materialized(a)?, self.materialized(b)?);
        let mut out = compare_states(&ma, &mb);
        out.a = scope_text(a);
        out.b = scope_text(b);
        let mut conflicts = (*self.conflicts(a)?).clone();
        if !matches!(b, Scope::Canon | Scope::At(_)) {
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
