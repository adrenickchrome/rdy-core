//! The block diff of two scopes (sr4; D-W40KZ, ra-R7PM6).
//!
//! [`ReadState::diff`] builds on [`ReadState::compare`]: one [`DiffEntry`] per changed block, in
//! reading order, with its change kind, its changed field values and the word pieces of its body
//! ([`crate::worddiff`]). The site's diff pages read it; M3f's `rdy_diff` will serialize the same
//! [`Diff`]. There is one diff engine.
//!
//! The rules (closed):
//!
//! 1. **Entries.** One per id in `compare(to, from)`'s `added` (→ [`DiffChange::Added`]),
//!    `deleted` (→ [`DiffChange::Removed`]), `changed` and `moved`. A changed block whose
//!    fields are only `order` is [`DiffChange::Moved`] with `reordered_only: true`. A block
//!    whose parent differs is `Moved` (`reordered_only: false`), or
//!    [`DiffChange::MovedAndUpdated`] when any other field differs. A block with other changed
//!    fields is [`DiffChange::Updated`], or `MovedAndUpdated` when `order` is among them
//!    (`reordered_only: true`). A pins-only change (no field differs) is `Updated` with no
//!    fields: the engine reports it, and the renderer leaves it out.
//! 2. **`doc`.** The doc that holds the block: in `to`, or in `from` for a removed block. A doc's
//!    own entry names itself. A folder has none.
//! 3. **`fields`.** Each name the compare gives, with the value from each side (`Block.fields`
//!    as JSON, `links` as the link list, `order` as the key). `body` is not in `fields`: it is
//!    the `body` pieces.
//! 4. **`body`.** `worddiff::diff(from.body, to.body, unit)` when the bodies differ, with
//!    [`Unit::Lines`] for a `code` block, else [`Unit::Words`]. An added block has
//!    `[Add(body)]` and a removed one `[Del(body)]`, when the body is not empty.
//! 5. **Order.** The depth-first reading order of `to` (`children` in order). A removed block
//!    goes right before the first block that follows it in `from`'s reading order and is still
//!    in `to`. With none, it goes after every entry of its old parent's subtree in `to` (the
//!    nearest old ancestor still in `to`; the end when there is none). A removed subtree keeps
//!    `from`'s order.
//! 6. **Counts.** `added`, `removed`, `updated` (`Updated` and `MovedAndUpdated`) and `moved`
//!    (`Moved` and `MovedAndUpdated`).
//!
//! `path` is the block's own path for a doc or a folder, as the compare gives it. In [`Moved`],
//! `from_path` and `to_path` say where the block is on each side: its own path for a doc or a
//! folder, else the path of the doc or folder that holds it.
//!
//! The diff reads stored states and writes nothing. The same two scopes always give the same
//! [`Diff`]: every order here comes from the trees or from sorted maps.
//!
//! **What moves from M3f (D-W40KZ).** The word diff and this block diff move into the Site review
//! plan. M3f keeps `rdy diff` and its text output, `rdy_diff` with its outputs and its token
//! budget, the one-argument defaults and the `--doc` subtree, the entry parts only those tools
//! read (`summary`, `links` added and removed per kind, `decision_status`, `repinned`, and the
//! `from`/`to` scope labels, added later as new fields), freezing the serialized shape as public
//! API, `rdy show <landing>`, `id@version`, the filter grammar, `rdy_asof` and `rdy_query`. Until
//! M3f ships `rdy_diff`, no tool sends a [`Diff`], so its shape stays fix forward.

use std::collections::{BTreeMap, BTreeSet};

use rdy_model::{Block, BlockId, Hash, tree};
use serde::Serialize;

use crate::error::*;
use crate::query::field_json;
use crate::read_state::ReadState;
use crate::view_query::CompareEntry;
use crate::worddiff::{self, Piece, Unit};
use crate::{Materialized, Scope};

/// What happened to a block from `from` to `to` (rule 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffChange {
    Added,
    Removed,
    Updated,
    Moved,
    MovedAndUpdated,
}

/// Where a moved block was and where it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Moved {
    pub from_parent: Option<BlockId>,
    pub to_parent: Option<BlockId>,
    pub from_path: Option<String>,
    pub to_path: Option<String>,
    /// The parent is the same: only the block's place among its siblings changed.
    pub reordered_only: bool,
}

/// One changed field with its value on each side (`None` where the side lacks it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FieldChange {
    pub name: String,
    pub from: Option<serde_json::Value>,
    pub to: Option<serde_json::Value>,
}

/// One changed block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DiffEntry {
    pub id: BlockId,
    pub block_type: String,
    /// The doc that holds the block (rule 2).
    pub doc: Option<BlockId>,
    /// The block's path, for a doc or a folder.
    pub path: Option<String>,
    pub change: DiffChange,
    pub from_version: Option<Hash>,
    pub to_version: Option<Hash>,
    pub moved: Option<Moved>,
    pub fields: Vec<FieldChange>,
    /// The word pieces of the body (rule 4); `None` when the body did not change.
    pub body: Option<Vec<Piece>>,
}

/// How many entries of each kind (rule 6).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DiffCounts {
    pub added: usize,
    pub removed: usize,
    pub updated: usize,
    pub moved: usize,
}

/// The block diff of two scopes: its entries in reading order, and their counts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Diff {
    pub entries: Vec<DiffEntry>,
    pub counts: DiffCounts,
}

/// The depth-first reading order of `m`: each block, then its children in order. Blocks no root
/// reaches follow, in id order, so every block has a place.
fn reading_order(m: &Materialized) -> Vec<BlockId> {
    let mut out = Vec::with_capacity(m.len());
    let mut seen: BTreeSet<BlockId> = BTreeSet::new();
    // an explicit stack: deep trees never overflow the call stack
    let mut stack: Vec<BlockId> = m
        .children(None)
        .iter()
        .rev()
        .map(|b| b.id.clone())
        .collect();
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        stack.extend(m.children(Some(&id)).iter().rev().map(|b| b.id.clone()));
        out.push(id);
    }
    let mut rest: Vec<&BlockId> = m.ids().filter(|id| !seen.contains(*id)).collect();
    rest.sort();
    out.extend(rest.into_iter().cloned());
    out
}

fn parse_id(text: &str) -> Result<BlockId, RdyError> {
    BlockId::parse(text).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("the compare gave a bad block id {text}: {e}"),
            "report this as a bug",
        )
    })
}

fn field_value(b: &Block, name: &str) -> Option<serde_json::Value> {
    match name {
        "links" => serde_json::to_value(&b.links).ok(),
        "order" => Some(serde_json::Value::String(b.order.clone())),
        _ => b.fields.get(name).map(field_json),
    }
}

fn unit_of(b: &Block) -> Unit {
    if b.block_type == "code" {
        Unit::Lines
    } else {
        Unit::Words
    }
}

fn own_path(m: &Materialized, b: &Block) -> Option<String> {
    if tree::is_tree_type(&b.block_type) {
        m.path_of(&b.id)
    } else {
        None
    }
}

fn doc_of(m: &Materialized, id: &BlockId) -> Option<BlockId> {
    m.doc_of(id).map(|d| d.id.clone())
}

/// One entry for a block on both sides; `names` are the compare's field names.
fn both_sides(
    from: &Materialized,
    to: &Materialized,
    old: &Block,
    new: &Block,
    names: &[String],
) -> DiffEntry {
    let parent_moved = old.parent != new.parent;
    let has_order = names.iter().any(|n| n == "order");
    let others = names.iter().any(|n| n != "order");
    let (change, reordered_only) = if parent_moved {
        let change = if names.is_empty() {
            DiffChange::Moved
        } else {
            DiffChange::MovedAndUpdated
        };
        (change, Some(false))
    } else if has_order && !others {
        (DiffChange::Moved, Some(true))
    } else if has_order {
        (DiffChange::MovedAndUpdated, Some(true))
    } else {
        (DiffChange::Updated, None)
    };
    let moved = reordered_only.map(|reordered_only| Moved {
        from_parent: old.parent.clone(),
        to_parent: new.parent.clone(),
        from_path: from.path_of(&old.id),
        to_path: to.path_of(&new.id),
        reordered_only,
    });
    let fields = names
        .iter()
        .filter(|n| *n != "body")
        .map(|n| FieldChange {
            name: n.clone(),
            from: field_value(old, n),
            to: field_value(new, n),
        })
        .collect();
    let body = (old.body != new.body).then(|| worddiff::diff(&old.body, &new.body, unit_of(new)));
    DiffEntry {
        id: new.id.clone(),
        block_type: new.block_type.clone(),
        doc: doc_of(to, &new.id),
        path: own_path(to, new),
        change,
        from_version: Some(old.version_hash()),
        to_version: Some(new.version_hash()),
        moved,
        fields,
        body,
    }
}

/// One entry for a block on one side only: `added` in `to`, else removed from `from`.
fn one_side(m: &Materialized, b: &Block, added: bool) -> DiffEntry {
    let body = (!b.body.is_empty()).then(|| {
        if added {
            worddiff::diff("", &b.body, unit_of(b))
        } else {
            worddiff::diff(&b.body, "", unit_of(b))
        }
    });
    let version = Some(b.version_hash());
    DiffEntry {
        id: b.id.clone(),
        block_type: b.block_type.clone(),
        doc: doc_of(m, &b.id),
        path: own_path(m, b),
        change: if added {
            DiffChange::Added
        } else {
            DiffChange::Removed
        },
        from_version: if added { None } else { version },
        to_version: if added { version } else { None },
        moved: None,
        fields: vec![],
        body,
    }
}

/// Where each block of `order` sits, and the end (exclusive) of its subtree in that order.
fn positions(m: &Materialized, order: &[BlockId]) -> BTreeMap<BlockId, (usize, usize)> {
    let index: BTreeMap<&BlockId, usize> =
        order.iter().enumerate().map(|(i, id)| (id, i)).collect();
    let mut out: BTreeMap<BlockId, (usize, usize)> = BTreeMap::new();
    // the reading order is depth first, so a subtree is the run of blocks after its root whose
    // chain reaches that root; walk back and widen each ancestor's end
    for (i, id) in order.iter().enumerate() {
        out.insert(id.clone(), (i, i + 1));
    }
    for (i, id) in order.iter().enumerate().rev() {
        let end = out[id].1;
        let Some(parent) = m.get(id).and_then(|b| b.parent.as_ref()) else {
            continue;
        };
        if let (Some(&pi), Some(slot)) = (index.get(parent), out.get_mut(parent))
            && pi < i
        {
            slot.1 = slot.1.max(end);
        }
    }
    out
}

impl ReadState {
    /// The block diff from `from` to `to` (the module docs give the rules).
    pub fn diff(&self, from: &Scope, to: &Scope) -> Result<Diff, RdyError> {
        let cmp = self.compare(to, from)?;
        let (mf, mt) = (self.materialized(from)?, self.materialized(to)?);

        // the compare's field names per id, merged over `changed` and `moved`
        let mut both: BTreeMap<BlockId, Vec<String>> = BTreeMap::new();
        for e in cmp.changed.iter().chain(&cmp.moved) {
            let names = both.entry(parse_id(&e.id)?).or_default();
            for n in &e.fields {
                if !names.contains(n) {
                    names.push(n.clone());
                }
            }
        }
        let ids = |list: &[CompareEntry]| -> Result<BTreeSet<BlockId>, RdyError> {
            list.iter().map(|e| parse_id(&e.id)).collect()
        };
        let added = ids(&cmp.added)?;
        let removed = ids(&cmp.deleted)?;

        let to_order = reading_order(&mt);
        let to_pos = positions(&mt, &to_order);
        let from_order = reading_order(&mf);
        let from_index: BTreeMap<&BlockId, usize> = from_order
            .iter()
            .enumerate()
            .map(|(i, id)| (id, i))
            .collect();
        // for each place in `from`'s order: where the first later block that `to` still holds
        // sits in `to`, if any (one pass, so a large removed subtree stays linear)
        let mut next_kept: Vec<Option<usize>> = vec![None; from_order.len() + 1];
        for (i, id) in from_order.iter().enumerate().rev() {
            next_kept[i] = to_pos.get(id).map(|p| p.0).or(next_kept[i + 1]);
        }

        // (place in `to`, before (0) or at (1) that place, `from` index) sorts the entries
        let mut keyed: Vec<((usize, u8, usize), DiffEntry)> = vec![];
        for (id, names) in &both {
            let (Some(old), Some(new)) = (mf.get(id), mt.get(id)) else {
                continue;
            };
            let at = to_pos.get(id).map_or(to_order.len(), |p| p.0);
            keyed.push(((at, 1, 0), both_sides(&mf, &mt, old, new, names)));
        }
        for id in &added {
            if let Some(b) = mt.get(id) {
                let at = to_pos.get(id).map_or(to_order.len(), |p| p.0);
                keyed.push(((at, 1, 0), one_side(&mt, b, true)));
            }
        }
        for id in &removed {
            let Some(b) = mf.get(id) else { continue };
            let fi = from_index.get(id).copied().unwrap_or(from_order.len());
            let after = next_kept.get(fi + 1).copied().flatten();
            let at = after.unwrap_or_else(|| subtree_end(&mf, &to_pos, b, to_order.len()));
            keyed.push(((at, 0, fi), one_side(&mf, b, false)));
        }
        keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));

        let entries: Vec<DiffEntry> = keyed.into_iter().map(|(_, e)| e).collect();
        let mut counts = DiffCounts::default();
        for e in &entries {
            match e.change {
                DiffChange::Added => counts.added += 1,
                DiffChange::Removed => counts.removed += 1,
                DiffChange::Updated => counts.updated += 1,
                DiffChange::Moved => counts.moved += 1,
                DiffChange::MovedAndUpdated => {
                    counts.updated += 1;
                    counts.moved += 1;
                }
            }
        }
        Ok(Diff { entries, counts })
    }
}

/// Where a removed block with no later block in `to` goes (rule 5): the end of its nearest old
/// ancestor's subtree in `to`, else the end.
fn subtree_end(
    from: &Materialized,
    to_pos: &BTreeMap<BlockId, (usize, usize)>,
    b: &Block,
    end: usize,
) -> usize {
    let mut parent = b.parent.as_ref();
    for _ in 0..=from.len() {
        let Some(p) = parent else { break };
        if let Some(pos) = to_pos.get(p) {
            return pos.1;
        }
        parent = from.get(p).and_then(|x| x.parent.as_ref());
    }
    end
}
