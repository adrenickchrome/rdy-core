//! The block diff of two scopes (sr4; D-W40KZ, ra-R7PM6).
//!
//! [`ReadState::diff`] builds on [`ReadState::compare`]: one [`DiffEntry`] per changed block, in
//! reading order, with its change kind, its changed field values and the word pieces of its body
//! ([`crate::worddiff`]). The site's diff pages read it, and `rdy_diff` serializes its entries.
//! There is one diff engine.
//!
//! The rules (closed):
//!
//! 1. **Entries.** One per id in `compare(to, from)`'s `added` (→ [`DiffChange::Added`]),
//!    `deleted` (→ [`DiffChange::Removed`]), `changed` and `moved`. For a block on both sides
//!    (task-4 rulings 1 and 2):
//!    - its content changed when its body, its links, its type or any custom field differs;
//!    - its place changed when its parent differs, or, under the same parent, when its order
//!      key differs and its place among its siblings differs too (`reordered_only: true`). Its
//!      place among its siblings is its index among the siblings that the parent holds on both
//!      sides. A new key at the same place (an append of the last block, a rebalance) is no
//!      move, and neither is a sibling moving around a block that keeps its key;
//!    - content changed: [`DiffChange::Updated`], with `moved` set when its place also changed;
//!    - only its place changed: [`DiffChange::Moved`];
//!    - neither (a pins-only change): `Updated` with only `repinned` set (rule 9). The engine
//!      reports it, and the renderer leaves it out.
//! 2. **`doc`.** The doc that holds the block: in `to`, or in `from` for a removed block. A doc's
//!    own entry names itself. A folder has none.
//! 3. **`fields`.** The custom fields whose values differ, by name, with the value from each side
//!    (`Block.fields` as JSON). They are read only from the field map, and structure only from
//!    the block's own members, so a custom field named `body`, `links`, `order` or `type` is an
//!    ordinary field. The structure has its own members: `type` (`from` and `to`), `links`
//!    (rule 8), `body` (the pieces) and `moved`.
//! 4. **`body`.** `worddiff::diff(from.body, to.body, unit)` when the bodies differ, with
//!    [`Unit::Lines`] for a `code` block, else [`Unit::Words`]. An added block has
//!    `[Add(body)]` and a removed one `[Del(body)]`, when the body is not empty. When either
//!    side is a removed version's stub (D-3M81W), its body is the notice `Content removed.` and
//!    is never diffed word by word: the pieces are `[Del(from.body), Add(to.body)]`, each when
//!    not empty. A stub keeps its id and recorded version; its removed fields and links are
//!    absent, so no removed title or name reaches an entry, and paths name it by its id.
//! 5. **Order.** The depth-first reading order of `to` (`children` in order). A removed block
//!    goes right before the first block that follows it in `from`'s reading order and is still
//!    in `to`. With none, it goes after every entry of its old parent's subtree in `to` (the
//!    nearest old ancestor still in `to`; the end when there is none). A removed subtree keeps
//!    `from`'s order.
//! 6. **Counts.** One per entry, by its change: `added`, `removed`, `updated` and `moved`. An
//!    `Updated` entry that also moved counts once, under `updated`.
//! 7. **`summary`.** [`compact_summary`] of the block in `to`, or in `from` for a removed block.
//!    A removed version's stub gives the notice line, never removed text.
//! 8. **`links`.** One row per link kind whose targets differ, in kind-name order. `added` holds
//!    the targets in `to` and not in `from`, `removed` the targets in `from` and not in `to`, each
//!    sorted by id. A link is compared whole (target and `part`), so a target whose `part` changed
//!    is in both lists. An added or a removed block has no rows. A list that only changed order
//!    has no row: it still counts as a content change under rule 1.
//! 9. **`repinned`.** The ids in both sides' `pins` whose pinned hash differs, sorted. A pin that
//!    only one side holds is not listed: it came with a new or removed link or reference. An added
//!    or a removed block has none.
//! 10. **The root.** [`diff_states_under`] cuts each side to the root's subtree, and treats each
//!     cut as the state. A block under the root on both sides whose version differs follows rule
//!     1. A block under it on one side only is added or removed. When the other side holds that
//!     block outside the root and its own parent differs between the sides, it is the block that
//!     moved: `moved` names both parents and paths, with `reordered_only: false`. A block under
//!     it, whose parent is the same on both sides, has no `moved`. Rule 5 reads the two cuts.
//! 11. **The status pass.** [`add_decision_status`], which only the diff request runs. For each
//!     decision on both sides (inside the root on both sides, when a root is set) whose derived
//!     status tag differs, `decision_status` holds the two tags. A decision with no entry (a
//!     decision that a new one supersedes keeps its version) gets an `Updated` entry with only
//!     `summary`, the two equal versions and `decision_status`, at its place by rule 5. The counts
//!     count it. [`ReadState::diff`], [`diff_states`] and [`diff_states_under`] never set it, so
//!     the renderer's pages do not change.
//!
//! `path` is the block's own path for a doc or a folder, as the compare gives it. In [`Moved`],
//! `from_path` and `to_path` say where the block is on each side: its own path for a doc or a
//! folder, else the path of the doc or folder that holds it.
//!
//! The diff reads stored states and writes nothing. The same two scopes always give the same
//! [`Diff`]: every order here comes from the trees or from sorted maps.
//!
//! **What M3f built (D-W40KZ, D-HZYK0).** The word diff and this block diff came from the Site
//! review plan. M3f added the entry parts only the diff request reads (`summary`, `links` per kind,
//! `decision_status` and `repinned`), the cut to a root's subtree and the status pass, and froze
//! the serialized [`DiffEntry`]: its key sets, its values, absent against `null`, and the order of
//! every array, not the order of keys in a JSON object. [`Diff`] itself stays the renderer's
//! struct; its shape is not a promise.

use std::collections::{BTreeMap, BTreeSet};

use rdy_graph::Graph;
use rdy_model::{Block, BlockId, Hash, Link, tree};
use serde::Serialize;

use crate::error::*;
use crate::query::{compact_summary, field_json};
use crate::read_state::ReadState;
use crate::view_query::CompareEntry;
use crate::worddiff::{self, Op as PieceOp, Piece, Unit};
use crate::{Materialized, Scope};

/// A version in JSON: 64 hex characters, as every reply writes a hash, or `null`.
fn hex_version<S: serde::Serializer>(v: &Option<Hash>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(h) => s.collect_str(h),
        None => s.serialize_none(),
    }
}

/// What happened to a block from `from` to `to` (rule 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffChange {
    Added,
    Removed,
    /// Its content changed; `moved` says whether its place changed too.
    Updated,
    /// Only its place changed.
    Moved,
}

/// One structural member's value on each side.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FromTo<T> {
    pub from: T,
    pub to: T,
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

/// One changed field with its value on each side (left out where the side lacks it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FieldChange {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<serde_json::Value>,
}

/// One link kind's changed targets (rule 8).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LinkChange {
    pub rel: String,
    pub added: Vec<BlockId>,
    pub removed: Vec<BlockId>,
}

/// A decision's derived status tag on each side (rule 11).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StatusChange {
    pub from: String,
    pub to: String,
}

/// One changed block. Its key sets are frozen, not their order: `doc`, `path`, `from_version` and
/// `to_version` are always present, `null` when they do not apply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DiffEntry {
    pub id: BlockId,
    pub block_type: String,
    /// The doc that holds the block (rule 2).
    pub doc: Option<BlockId>,
    /// The block's path, for a doc or a folder.
    pub path: Option<String>,
    pub change: DiffChange,
    /// The block's compact read (rule 7).
    pub summary: String,
    #[serde(serialize_with = "hex_version")]
    pub from_version: Option<Hash>,
    #[serde(serialize_with = "hex_version")]
    pub to_version: Option<Hash>,
    /// Where the block was and is, when its place changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moved: Option<Moved>,
    /// The block's type on each side, when it changed (`type` in JSON).
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_change: Option<FromTo<String>>,
    /// The custom fields that changed (rule 3).
    pub fields: Vec<FieldChange>,
    /// The link kinds whose targets changed (rule 8).
    pub links: Vec<LinkChange>,
    /// A decision's status change; only the status pass sets it (rule 11).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_status: Option<StatusChange>,
    /// The pins that changed with both sides holding them (rule 9).
    pub repinned: Vec<BlockId>,
    /// The word pieces of the body (rule 4); `None` when the body did not change.
    #[serde(skip_serializing_if = "Option::is_none")]
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
pub(crate) fn reading_order(m: &Materialized) -> Vec<BlockId> {
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

/// The custom fields whose values differ, by name (rule 3: the field maps only).
fn changed_fields(old: &Block, new: &Block) -> Vec<FieldChange> {
    let names: BTreeSet<&String> = old.fields.keys().chain(new.fields.keys()).collect();
    names
        .into_iter()
        .filter(|n| old.fields.get(*n) != new.fields.get(*n))
        .map(|n| FieldChange {
            name: n.clone(),
            from: old.fields.get(n).map(field_json),
            to: new.fields.get(n).map(field_json),
        })
        .collect()
}

/// The targets of `links` that `other` does not hold, compared whole, sorted by id (rule 8).
fn targets_not_in(links: &[Link], other: &[Link]) -> Vec<BlockId> {
    let other: BTreeSet<(&BlockId, &Option<String>)> =
        other.iter().map(|l| (&l.target, &l.part)).collect();
    let targets: BTreeSet<&BlockId> = links
        .iter()
        .filter(|l| !other.contains(&(&l.target, &l.part)))
        .map(|l| &l.target)
        .collect();
    targets.into_iter().cloned().collect()
}

fn links_of<'a>(b: &'a Block, rel: &str) -> &'a [Link] {
    b.links.get(rel).map_or(&[], Vec::as_slice)
}

/// The link kinds whose targets differ, in kind-name order (rule 8).
fn changed_links(old: &Block, new: &Block) -> Vec<LinkChange> {
    let kinds: BTreeSet<&String> = old.links.keys().chain(new.links.keys()).collect();
    kinds
        .into_iter()
        .filter_map(|rel| {
            let (from, to) = (links_of(old, rel), links_of(new, rel));
            let added = targets_not_in(to, from);
            let removed = targets_not_in(from, to);
            (!added.is_empty() || !removed.is_empty()).then(|| LinkChange {
                rel: rel.clone(),
                added,
                removed,
            })
        })
        .collect()
}

/// The ids both sides pin at different hashes, sorted (rule 9).
fn changed_pins(old: &Block, new: &Block) -> Vec<BlockId> {
    new.pins
        .iter()
        .filter(|(target, hash)| old.pins.get(*target).is_some_and(|was| was != *hash))
        .map(|(target, _)| target.clone())
        .collect()
}

fn unit_of(b: &Block) -> Unit {
    if b.block_type == "code" {
        Unit::Lines
    } else {
        Unit::Words
    }
}

/// The body pieces of a block on both sides whose bodies differ (rule 4).
fn body_pieces(old: &Block, new: &Block) -> Vec<Piece> {
    if !old.is_redacted() && !new.is_redacted() {
        return worddiff::diff(&old.body, &new.body, unit_of(new));
    }
    [(PieceOp::Del, &old.body), (PieceOp::Add, &new.body)]
        .into_iter()
        .filter(|(_, text)| !text.is_empty())
        .map(|(op, text)| Piece {
            op,
            text: text.clone(),
        })
        .collect()
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

/// Each block's index among the siblings its parent holds on both sides, in `from` and in `to`,
/// one parent at a time (rule 1: the place among the siblings).
struct SiblingPlaces<'a> {
    from: &'a Materialized,
    to: &'a Materialized,
    by_parent: BTreeMap<Option<BlockId>, BTreeMap<BlockId, (usize, usize)>>,
}

impl<'a> SiblingPlaces<'a> {
    fn new(from: &'a Materialized, to: &'a Materialized) -> Self {
        Self {
            from,
            to,
            by_parent: BTreeMap::new(),
        }
    }

    /// Whether `id`, under `parent` on both sides, has another index among the siblings both
    /// sides hold there.
    fn reordered(&mut self, parent: Option<&BlockId>, id: &BlockId) -> bool {
        let (from, to) = (self.from, self.to);
        let places = self.by_parent.entry(parent.cloned()).or_insert_with(|| {
            let old: Vec<&BlockId> = from.children(parent).iter().map(|b| &b.id).collect();
            let new: Vec<&BlockId> = to.children(parent).iter().map(|b| &b.id).collect();
            let in_new: BTreeSet<&BlockId> = new.iter().copied().collect();
            let in_old: BTreeSet<&BlockId> = old.iter().copied().collect();
            let mut out: BTreeMap<BlockId, (usize, usize)> = BTreeMap::new();
            for (i, id) in old.into_iter().filter(|id| in_new.contains(id)).enumerate() {
                out.insert(id.clone(), (i, 0));
            }
            for (i, id) in new.into_iter().filter(|id| in_old.contains(id)).enumerate() {
                if let Some(slot) = out.get_mut(id) {
                    slot.1 = i;
                }
            }
            out
        });
        places.get(id).is_some_and(|(a, b)| a != b)
    }
}

/// One entry for a block on both sides (rules 1 to 4, from the blocks' own members).
fn both_sides(places: &mut SiblingPlaces, old: &Block, new: &Block) -> DiffEntry {
    let (from, to) = (places.from, places.to);
    let fields = changed_fields(old, new);
    let type_change = (old.block_type != new.block_type).then(|| FromTo {
        from: old.block_type.clone(),
        to: new.block_type.clone(),
    });
    let body = (old.body != new.body).then(|| body_pieces(old, new));
    let content =
        !fields.is_empty() || type_change.is_some() || old.links != new.links || body.is_some();
    let same_parent = old.parent == new.parent;
    // a new key alone is no move: the place among the siblings must differ too
    let reordered =
        same_parent && old.order != new.order && places.reordered(new.parent.as_ref(), &new.id);
    let moved = (!same_parent || reordered).then(|| Moved {
        from_parent: old.parent.clone(),
        to_parent: new.parent.clone(),
        from_path: from.path_of(&old.id),
        to_path: to.path_of(&new.id),
        reordered_only: same_parent,
    });
    let change = if !content && moved.is_some() {
        DiffChange::Moved
    } else {
        DiffChange::Updated
    };
    DiffEntry {
        id: new.id.clone(),
        block_type: new.block_type.clone(),
        doc: doc_of(to, &new.id),
        path: own_path(to, new),
        change,
        summary: compact_summary(new),
        from_version: Some(old.version_hash()),
        to_version: Some(new.version_hash()),
        moved,
        type_change,
        fields,
        links: changed_links(old, new),
        decision_status: None,
        repinned: changed_pins(old, new),
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
        summary: compact_summary(b),
        from_version: if added { None } else { version },
        to_version: if added { version } else { None },
        moved: None,
        type_change: None,
        fields: vec![],
        links: vec![],
        decision_status: None,
        repinned: vec![],
        body,
    }
}

/// The move of a block that one side's cut holds and the other side holds outside it, when its
/// own parent differs (rule 10). With no root the two sides never disagree this way.
fn crossed(from: &Materialized, to: &Materialized, id: &BlockId) -> Option<Moved> {
    let (old, new) = (from.get(id)?, to.get(id)?);
    (old.parent != new.parent).then(|| Moved {
        from_parent: old.parent.clone(),
        to_parent: new.parent.clone(),
        from_path: from.path_of(id),
        to_path: to.path_of(id),
        reordered_only: false,
    })
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

        // the ids only: each entry reads its blocks' own members (rule 3)
        let ids = |list: &[CompareEntry]| -> Result<BTreeSet<BlockId>, RdyError> {
            list.iter().map(|e| parse_id(&e.id)).collect()
        };
        let mut both = ids(&cmp.changed)?;
        both.extend(ids(&cmp.moved)?);
        let added = ids(&cmp.added)?;
        let removed = ids(&cmp.deleted)?;
        Ok(build(&mf, &mt, &both, &added, &removed, None))
    }
}

/// The block diff from the state `from` to the state `to`: [`ReadState::diff`] of two states
/// given as blocks. The entries are the ids the compare gives: added in `to`, removed from
/// `from`, and on both sides with another version.
pub fn diff_states(from: &Materialized, to: &Materialized) -> Diff {
    diff_states_under(from, to, None)
}

/// The diff of two states, cut to `root`'s subtree on each side when given (rule 10, C8): the
/// ids under the root on both sides whose version differs, the ids under it in `to` only
/// (added) and in `from` only (removed). An added block that `from` holds outside the root,
/// or a removed block that `to` holds outside it, gets `moved` with both parents and paths and
/// `reordered_only: false` only when its own parent differs between the sides: that is the
/// block that moved. A block under it, whose parent is the same on both sides, shows as added
/// or removed without `moved` (C8).
pub fn diff_states_under(from: &Materialized, to: &Materialized, root: Option<&BlockId>) -> Diff {
    let cut = root.map(|r| Cut::new(from, to, r));
    let in_from = |id: &BlockId| cut.as_ref().is_none_or(|c| c.from.contains(id));
    let in_to = |id: &BlockId| cut.as_ref().is_none_or(|c| c.to.contains(id));
    let mut both = BTreeSet::new();
    let mut removed = BTreeSet::new();
    for (id, v) in from.versions().filter(|(id, _)| in_from(id)) {
        match to.version_of(id).filter(|_| in_to(id)) {
            Some(w) if w != *v => {
                both.insert(id.clone());
            }
            Some(_) => {}
            None => {
                removed.insert(id.clone());
            }
        }
    }
    let added = to
        .ids()
        .filter(|id| in_to(id) && (from.get(id).is_none() || !in_from(id)))
        .cloned()
        .collect();
    build(from, to, &both, &added, &removed, cut.as_ref())
}

/// The status pass (rule 11): sets `decision_status`, adds status-only entries in `to`'s reading
/// order, and recounts. Only the diff request calls it.
pub fn add_decision_status(
    diff: &mut Diff,
    from: &Materialized,
    to: &Materialized,
    from_graph: &Graph,
    to_graph: &Graph,
    root: Option<&BlockId>,
) {
    let cut = root.map(|r| Cut::new(from, to, r));
    let at: BTreeMap<BlockId, usize> = diff
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.id.clone(), i))
        .collect();
    let mut added = false;
    for new in to.iter() {
        let id = &new.id;
        if cut
            .as_ref()
            .is_some_and(|c| !c.from.contains(id) || !c.to.contains(id))
        {
            continue;
        }
        let Some(old) = from.get(id) else { continue };
        let (Some(was), Some(is)) = (from_graph.status(id), to_graph.status(id)) else {
            continue;
        };
        if was.tag() == is.tag() {
            continue;
        }
        let status = StatusChange {
            from: was.tag().to_string(),
            to: is.tag().to_string(),
        };
        if let Some(&i) = at.get(id) {
            diff.entries[i].decision_status = Some(status);
            continue;
        }
        added = true;
        diff.entries.push(DiffEntry {
            id: id.clone(),
            block_type: new.block_type.clone(),
            doc: doc_of(to, id),
            path: own_path(to, new),
            change: DiffChange::Updated,
            summary: compact_summary(new),
            from_version: Some(old.version_hash()),
            to_version: Some(new.version_hash()),
            moved: None,
            type_change: None,
            fields: vec![],
            links: vec![],
            decision_status: Some(status),
            repinned: vec![],
            body: None,
        });
    }
    if !added {
        return;
    }
    let from_order = order_in(from, cut.as_ref().map(|c| &c.from));
    let to_order = order_in(to, cut.as_ref().map(|c| &c.to));
    let places = Places::new(from, to, &from_order, &to_order);
    let keyed = std::mem::take(&mut diff.entries)
        .into_iter()
        .map(|e| {
            let key = match from.get(&e.id).filter(|_| e.change == DiffChange::Removed) {
                Some(b) => places.removed(b),
                None => places.kept(&e.id),
            };
            (key, e)
        })
        .collect();
    *diff = ordered(keyed);
}

/// The ids under a root on each side, the root included. A root a side lacks has an empty cut.
struct Cut {
    from: BTreeSet<BlockId>,
    to: BTreeSet<BlockId>,
}

impl Cut {
    fn new(from: &Materialized, to: &Materialized, root: &BlockId) -> Self {
        Self {
            from: subtree(from, root),
            to: subtree(to, root),
        }
    }
}

/// `root` and every block under it in `m`; empty when `m` lacks it.
fn subtree(m: &Materialized, root: &BlockId) -> BTreeSet<BlockId> {
    let mut out = BTreeSet::new();
    if m.get(root).is_none() {
        return out;
    }
    // an explicit stack: deep trees never overflow the call stack
    let mut stack = vec![root.clone()];
    while let Some(id) = stack.pop() {
        stack.extend(m.children(Some(&id)).iter().map(|b| b.id.clone()));
        out.insert(id);
    }
    out
}

/// `m`'s reading order, kept to `cut` when given.
fn order_in(m: &Materialized, cut: Option<&BTreeSet<BlockId>>) -> Vec<BlockId> {
    let mut order = reading_order(m);
    if let Some(cut) = cut {
        order.retain(|id| cut.contains(id));
    }
    order
}

/// (place in `to`, before (0) or at (1) that place, `from` index) sorts the entries (rule 5).
type Place = (usize, u8, usize);

/// Where entries go in the reading order of the two sides (rule 5).
struct Places<'a> {
    from: &'a Materialized,
    to_pos: BTreeMap<BlockId, (usize, usize)>,
    end: usize,
    from_index: BTreeMap<&'a BlockId, usize>,
    from_end: usize,
    /// For each place in `from`'s order: where the first later block that `to` still holds sits
    /// in `to`, if any (one pass, so a large removed subtree stays linear).
    next_kept: Vec<Option<usize>>,
}

impl<'a> Places<'a> {
    fn new(
        from: &'a Materialized,
        to: &Materialized,
        from_order: &'a [BlockId],
        to_order: &[BlockId],
    ) -> Self {
        let to_pos = positions(to, to_order);
        let from_index = from_order
            .iter()
            .enumerate()
            .map(|(i, id)| (id, i))
            .collect();
        let mut next_kept: Vec<Option<usize>> = vec![None; from_order.len() + 1];
        for (i, id) in from_order.iter().enumerate().rev() {
            next_kept[i] = to_pos.get(id).map(|p| p.0).or(next_kept[i + 1]);
        }
        Self {
            from,
            to_pos,
            end: to_order.len(),
            from_index,
            from_end: from_order.len(),
            next_kept,
        }
    }

    /// The place of a block that `to` holds.
    fn kept(&self, id: &BlockId) -> Place {
        (self.to_pos.get(id).map_or(self.end, |p| p.0), 1, 0)
    }

    /// The place of a removed block.
    fn removed(&self, b: &Block) -> Place {
        let fi = self.from_index.get(&b.id).copied().unwrap_or(self.from_end);
        let after = self.next_kept.get(fi + 1).copied().flatten();
        let at = after.unwrap_or_else(|| subtree_end(self.from, &self.to_pos, b, self.end));
        (at, 0, fi)
    }
}

/// The entries in place order, with their counts (rules 5 and 6).
fn ordered(mut keyed: Vec<(Place, DiffEntry)>) -> Diff {
    keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));
    let entries: Vec<DiffEntry> = keyed.into_iter().map(|(_, e)| e).collect();
    let mut counts = DiffCounts::default();
    for e in &entries {
        match e.change {
            DiffChange::Added => counts.added += 1,
            DiffChange::Removed => counts.removed += 1,
            DiffChange::Updated => counts.updated += 1,
            DiffChange::Moved => counts.moved += 1,
        }
    }
    Diff { entries, counts }
}

/// The entries of the given ids, ordered and counted (rules 1 to 6, and 10 with a cut).
fn build(
    mf: &Materialized,
    mt: &Materialized,
    both: &BTreeSet<BlockId>,
    added: &BTreeSet<BlockId>,
    removed: &BTreeSet<BlockId>,
    cut: Option<&Cut>,
) -> Diff {
    let from_order = order_in(mf, cut.map(|c| &c.from));
    let to_order = order_in(mt, cut.map(|c| &c.to));
    let places = Places::new(mf, mt, &from_order, &to_order);

    let mut keyed: Vec<(Place, DiffEntry)> = vec![];
    let mut siblings = SiblingPlaces::new(mf, mt);
    for id in both {
        let (Some(old), Some(new)) = (mf.get(id), mt.get(id)) else {
            continue;
        };
        keyed.push((places.kept(id), both_sides(&mut siblings, old, new)));
    }
    for id in added {
        if let Some(b) = mt.get(id) {
            let mut e = one_side(mt, b, true);
            e.moved = crossed(mf, mt, id);
            keyed.push((places.kept(id), e));
        }
    }
    for id in removed {
        let Some(b) = mf.get(id) else { continue };
        let mut e = one_side(mf, b, false);
        e.moved = crossed(mf, mt, id);
        keyed.push((places.removed(b), e));
    }
    ordered(keyed)
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
