//! Carrying the structural layer of coverage across a write (cc11, FW-PF2): recompute the
//! entries of the blocks the ops can change, by a closed dirty set, and keep the rest. A write
//! that can change entries the dirty set does not reach gives up, and the next read rebuilds.
use super::model::Structural;
use super::structural::{EntryCx, entry_of, named_in};
use crate::lexicon::Lexicon;
use crate::tensions::index::is_deprecated;
use crate::{Materialized, RdyError, ReadState, Scope};
use rdy_graph::{EdgeKind, Graph, edges_of};
use rdy_model::{Block, BlockId, FieldValue, Op, decision, entity, tree};
use rdy_schema::Pack;
use std::cell::Cell;
use std::collections::BTreeSet;

thread_local! {
    static CARRIED_ENTRIES: Cell<u64> = const { Cell::new(0) };
}

/// Test hook: this thread's count of block entries recomputed by carries (the dirty sets).
#[doc(hidden)]
pub fn carried_entries() -> u64 {
    CARRIED_ENTRIES.with(Cell::get)
}

/// `prev`, the structural layer of `old` (whose graph is `old_g`), carried across `ops` to
/// `new`, whose graph is `g` and whose lexicon is `lex`: equal to a rebuild. `None` when the ops
/// can change entries the dirty set does not reach (the next read rebuilds): an entity created,
/// deleted or renamed; a lifecycle change or a move of a block that has children; a block
/// deleted with children left; a doc's `doc_type` changed; a doc or a folder created, deleted,
/// renamed or moved (it changes the paths of docs other than itself). With the layer, the blocks
/// whose entries it recomputed (every other entry is `prev`'s).
#[allow(clippy::too_many_arguments)]
pub(crate) fn carry(
    prev: &Structural,
    old: &Materialized,
    new: &Materialized,
    old_g: &Graph,
    g: &Graph,
    lex: &Lexicon,
    pack: &Pack,
    ops: &[Op],
) -> Option<(Structural, BTreeSet<BlockId>)> {
    let touched: BTreeSet<&BlockId> = ops.iter().map(Op::block_id).collect();
    if touched
        .iter()
        .any(|id| gives_up(old, new, lex, old.get(id), new.get(id)))
    {
        return None;
    }

    let mut dirty: BTreeSet<BlockId> = BTreeSet::new();
    for id in &touched {
        dirty.insert((*id).clone());
        let (a, b) = (old.get(id), new.get(id));
        // what a block that points at this one reads of it: that it exists, its type and, for a
        // decision, its status (its liveness); a body or other field edit changes none of it
        let reshaped = a.is_some() != b.is_some()
            || a.map(|x| &x.block_type) != b.map(|x| &x.block_type)
            || a.and_then(decision::status) != b.and_then(decision::status);
        // what a decision governs: its status, and its affects, supersedes and revokes edges
        let governs_anew = reshaped || governing_edges(a) != governing_edges(b);
        for (m, x) in [(old, a), (new, b)] {
            let Some(x) = x else { continue };
            // 1. the block, its parent and its doc, before and after
            if let Some(p) = &x.parent {
                dirty.insert(p.clone());
            }
            docs_above(m, id, &mut dirty);
            // 2. each target of an outgoing edge, old or new, and that target's doc
            for e in edges_of(x) {
                docs_above(new, &e.to, &mut dirty);
                dirty.insert(e.to);
            }
            // 4. a decision whose status or governing edges change: what its liveness and its
            // affects reach
            if x.block_type == decision::DECISION && governs_anew {
                decision_reach(g, x, &mut dirty);
            }
        }
        // 3. a block that appears, goes, changes type or (a decision) changes status: each
        // source of an incoming edge in the new graph
        if reshaped {
            for e in g.incoming(id) {
                dirty.insert(e.from.clone());
            }
        }
        // 5. an entity that starts or stops being deprecated: the blocks that name it (an
        // unlinked mention of a deprecated entity is no check)
        let is_entity = |x: Option<&Block>| x.is_some_and(|x| lex.is_entity_type(&x.block_type));
        if (is_entity(a) || is_entity(b))
            && is_deprecated(old, old_g, id) != is_deprecated(new, g, id)
            && let Some(named) = prev.mentions.get(*id)
        {
            dirty.extend(named.iter().cloned());
        }
    }

    let cx = EntryCx::new(new, g, lex, pack);
    CARRIED_ENTRIES.with(|c| c.set(c.get() + dirty.len() as u64));
    let mut blocks = prev.blocks.clone();
    let mut mentions = prev.mentions.clone();
    // the mentions a touched block made before the ops go; the lexicon's names did not change
    for id in &touched {
        let Some(b) = old.get(id) else { continue };
        for entity in named_in(pack, lex, b) {
            if let Some(set) = mentions.get_mut(&entity) {
                set.remove(*id);
                if set.is_empty() {
                    mentions.remove(&entity);
                }
            }
        }
    }
    for id in &dirty {
        match entry_of(&cx, id) {
            Some((entry, named)) => {
                if touched.contains(id) {
                    for entity in named {
                        mentions.entry(entity).or_default().insert(id.clone());
                    }
                }
                blocks.insert(id.clone(), entry);
            }
            None => {
                blocks.remove(id);
            }
        }
    }
    Some((Structural { blocks, mentions }, dirty))
}

/// Perf hook (cc11, FW-PF2; what the `qa-perf` op `coverage_carry` times): the structural layer
/// of `scope` in `prev`, carried across `ops` to `next`, by the call `Built::after` makes
/// (`Built::carried_structural`). `next` is the state the write of `ops` published on `prev`.
/// `None` when `prev`'s layer is not built or the carry gives up. It never builds the layer in
/// full; it reads both states' builds of `scope`, built first if they are not.
#[doc(hidden)]
pub fn carry_for_perf(
    prev: &ReadState,
    next: &ReadState,
    scope: &Scope,
    ops: &[Op],
) -> Result<Option<Structural>, RdyError> {
    let scope = prev.coverage_scope(scope)?;
    let (old, new) = (prev.built(&scope)?, next.built(&scope)?);
    Ok(old
        .carried_structural(&new, ops, next.pack())
        .map(|(s, _)| s))
}

/// Whether one op's block, `a` before and `b` after, changes entries the dirty set does not
/// reach.
fn gives_up<'a>(
    old: &Materialized,
    new: &Materialized,
    lex: &Lexicon,
    a: Option<&'a Block>,
    b: Option<&'a Block>,
) -> bool {
    let Some(id) = a.or(b).map(|x| &x.id) else {
        return false;
    };
    // an entity created, deleted or renamed: the mentions of every block can change, and so
    // can the fix of every unlinked mention, which names the raw name (a rename that keeps the
    // phrase, "Ledger" to "LEDGER", finds the same mentions but changes those fixes)
    let entity = |x: Option<&'a Block>| x.filter(|x| lex.is_entity_type(&x.block_type));
    match (entity(a), entity(b)) {
        (None, None) => {}
        (Some(x), Some(y)) if raw_names(x) == raw_names(y) => {}
        _ => return true,
    }
    // a doc or a folder created, deleted, renamed or moved: the paths under and beside it
    let tree_type = |x: Option<&Block>| x.is_some_and(|x| tree::is_tree_type(&x.block_type));
    if (tree_type(a) || tree_type(b))
        && (old.name_of(id) != new.name_of(id) || a.map(|x| &x.parent) != b.map(|x| &x.parent))
    {
        return true;
    }
    // a doc's doc_type changed
    let doc_type = |x: Option<&'a Block>| {
        x.filter(|x| x.block_type == tree::DOC)
            .and_then(|x| x.field_str("doc_type"))
    };
    if doc_type(a) != doc_type(b) {
        return true;
    }
    let has_children = !old.children(Some(id)).is_empty() || !new.children(Some(id)).is_empty();
    match (a, b) {
        // a lifecycle change or a move of a block that has children
        (Some(x), Some(y)) => {
            has_children
                && (x.fields.get("lifecycle") != y.fields.get("lifecycle") || x.parent != y.parent)
        }
        // a block deleted with children left
        (Some(_), None) => !new.children(Some(id)).is_empty(),
        _ => false,
    }
}

/// The raw `name` and `aliases` fields of entity `b`, as written: its mentions are found by
/// their phrases, and an unlinked mention's fix names the raw name.
fn raw_names(b: &Block) -> (Option<&FieldValue>, Option<&FieldValue>) {
    (b.fields.get("name"), b.fields.get(entity::ALIASES))
}

/// Adds `id` (when it is a doc) and every doc above it in `m`.
fn docs_above(m: &Materialized, id: &BlockId, dirty: &mut BTreeSet<BlockId>) {
    let mut cur = m.get(id);
    for _ in 0..=m.len() {
        let Some(b) = cur else { break };
        if b.block_type == tree::DOC {
            dirty.insert(b.id.clone());
        }
        cur = b.parent.as_ref().and_then(|p| m.get(p));
    }
}

/// Whether an edge is the link `name`.
fn is_link(e: &rdy_graph::Edge, name: &str) -> bool {
    matches!(&e.kind, EdgeKind::Link(n) if n == name)
}

/// The edges of decision `b` that decide what it governs and whose liveness it changes: its
/// `affects`, `supersedes` and `revokes` links (none for an absent block or another type).
fn governing_edges(b: Option<&Block>) -> Vec<rdy_graph::Edge> {
    b.filter(|b| b.block_type == decision::DECISION)
        .map(|b| {
            edges_of(b)
                .into_iter()
                .filter(|e| {
                    is_link(e, decision::AFFECTS)
                        || is_link(e, decision::SUPERSEDES)
                        || is_link(e, decision::REVOKES)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Adds `target` and the sources of its incoming `describes` edges: the blocks governed
/// through it when a decision affects it.
fn affected(g: &Graph, target: &BlockId, dirty: &mut BTreeSet<BlockId>) {
    dirty.insert(target.clone());
    for e in g.incoming(target) {
        if is_link(e, decision::DESCRIBES) {
            dirty.insert(e.from.clone());
        }
    }
}

/// Rule 4 for decision `d` (one version of it): the targets of its `affects` edges and the
/// blocks that describe them; the targets of its `supersedes` and `revokes` edges, whose
/// liveness it can change, with the sources of their incoming `decided_by` and `implements`
/// edges and the targets of their `affects` edges and the blocks that describe those.
fn decision_reach(g: &Graph, d: &Block, dirty: &mut BTreeSet<BlockId>) {
    for e in edges_of(d) {
        if is_link(&e, decision::AFFECTS) {
            affected(g, &e.to, dirty);
        } else if is_link(&e, decision::SUPERSEDES) || is_link(&e, decision::REVOKES) {
            dirty.insert(e.to.clone());
            for x in g.incoming(&e.to) {
                if is_link(x, decision::DECIDED_BY) || is_link(x, decision::IMPLEMENTS) {
                    dirty.insert(x.from.clone());
                }
            }
            for x in g.outgoing(&e.to) {
                if is_link(x, decision::AFFECTS) {
                    affected(g, &x.to, dirty);
                }
            }
        }
    }
}
