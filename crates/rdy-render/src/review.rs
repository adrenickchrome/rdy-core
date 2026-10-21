//! The review: what a draft changes against the canon it is based on. A draft render reads it to
//! mark the blocks a reader sees as edited or new, to show the text a draft removed where it
//! stood, and to count each doc's changes. Only what is on the page counts: decisions and
//! rationale give no mark, no count and no removed line. Every word the review gives is a reader
//! word (`Edited`, `New`, a count like `2 edited · 1 new · 1 removed`), never an id, a hash or a
//! type name.
use crate::site::RATIONALE;
use rdy_engine::view_query::compare_states;
use rdy_engine::{Compare, CompareEntry, Materialized, RdyError, ReadState, Scope};
use rdy_model::{Block, BlockId, decision, tree};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};

/// The word a changed block, doc or folder carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mark {
    Edited,
    New,
}

impl Mark {
    /// The word a reader sees.
    pub fn word(self) -> &'static str {
        match self {
            Mark::Edited => "Edited",
            Mark::New => "New",
        }
    }
}

/// What a draft changes in one doc: the doc's word, and for an edited doc how many of its blocks
/// a reader sees edited, new and removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocChange {
    pub mark: Mark,
    pub edited: usize,
    pub new: usize,
    pub removed: usize,
}

impl DocChange {
    /// `2 edited · 1 new · 1 removed`, leaving out the parts that are zero; none for a new doc or
    /// a doc with nothing counted.
    pub fn counts(&self) -> Option<String> {
        if self.mark == Mark::New {
            return None;
        }
        let parts: Vec<String> = [
            (self.edited, "edited"),
            (self.new, "new"),
            (self.removed, "removed"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, word)| format!("{n} {word}"))
        .collect();
        (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
    }
}

/// One removed line: the text a draft took out of a doc, shown where it stood.
#[derive(Clone, Debug)]
pub struct Removed {
    /// The draft block the line is shown just before, or none for the doc's end.
    pub before: Option<BlockId>,
    /// The removed block, then its removed visible descendants, in the base's reading order, as
    /// the base holds them.
    pub blocks: Vec<Block>,
}

/// A draft's changes against its base.
#[derive(Clone, Debug, Default)]
pub struct Review {
    marks: HashMap<BlockId, Mark>,
    folders: HashMap<BlockId, Mark>,
    docs: HashMap<BlockId, DocChange>,
    removed: HashMap<BlockId, Vec<Removed>>,
    removed_pages: Vec<Block>,
    /// The changed blocks (O2); see [`Review::changed_blocks`].
    changed: BTreeSet<BlockId>,
}

impl Review {
    /// The word a block carries in the draft, if it carries one.
    pub fn mark(&self, id: &BlockId) -> Option<Mark> {
        self.marks.get(id).copied()
    }

    /// What the draft changes in a doc, if anything a reader sees.
    pub fn doc(&self, id: &BlockId) -> Option<&DocChange> {
        self.docs.get(id)
    }

    /// The word a page carries: a doc's change mark, or a folder's own mark.
    pub fn page_mark(&self, id: &BlockId) -> Option<Mark> {
        self.docs
            .get(id)
            .map(|d| d.mark)
            .or_else(|| self.folders.get(id).copied())
    }

    /// The removed lines of a draft doc, in the base's reading order.
    pub fn removed_in(&self, doc: &BlockId) -> &[Removed] {
        self.removed.get(doc).map(Vec::as_slice).unwrap_or_default()
    }

    /// The docs and folders the draft removed, top-most only, in the base's reading order.
    pub fn removed_pages(&self) -> &[Block] {
        &self.removed_pages
    }

    /// The blocks a review render changes (O2, closed; the task-5 ruling), as the review chip,
    /// the text label and the change summary count them. Each block counts once, when it is any
    /// of: (a) a visible non-tree block marked Edited or New; (b) a visible non-tree block under a
    /// New doc; (c) a block of a removed line (the removed block and its removed visible
    /// descendants); (d) a visible non-tree block under a removed page; (e) a doc or folder that
    /// the review render adds or removes, each one (a folder's docs too), or whose own place
    /// (its parent, or its position among the siblings both sides hold under the same parent)
    /// or own field a reader sees (`title`, `name`, `description`) changed.
    /// Decisions, rationale, pins-only changes and a new order key at the same place never
    /// count.
    pub fn changed_blocks(&self) -> u32 {
        u32::try_from(self.changed.len()).unwrap_or(u32::MAX)
    }

    /// The blocks [`Review::changed_blocks`] counts, by id: the change summary shows exactly
    /// these (sr6), so the chip's count and the summary's entries agree.
    pub(crate) fn changed_set(&self) -> &BTreeSet<BlockId> {
        &self.changed
    }
}

/// The fields of a doc or folder that a reader sees: a change in one counts the page (O2 (e)).
const PAGE_FIELDS: [&str; 3] = ["title", "name", "description"];

/// The blocks [`Review::changed_blocks`] counts, worked out once from the compare and both sides.
fn count_changed(
    rv: &Review,
    cmp: &Compare,
    dm: &Materialized,
    bm: &Materialized,
    moved_in_place: &HashSet<BlockId>,
) -> BTreeSet<BlockId> {
    let mut counted: BTreeSet<&BlockId> = BTreeSet::new();
    let content = |b: &Block| visible(b) && !tree::is_tree_type(&b.block_type);
    // (a) the marked blocks (only visible ones carry a mark)
    for id in rv.marks.keys() {
        if dm
            .get(id)
            .is_some_and(|b| !tree::is_tree_type(&b.block_type))
        {
            counted.insert(id);
        }
    }
    // (b) the content of each New doc
    for (id, mark) in &rv.marks {
        if *mark == Mark::New && dm.get(id).is_some_and(|b| b.block_type == tree::DOC) {
            counted.extend(
                reading_order(dm, id)
                    .into_iter()
                    .filter(|(b, _)| content(b))
                    .map(|(b, _)| &b.id),
            );
        }
    }
    // (c) the blocks of the removed lines
    for lines in rv.removed.values() {
        counted.extend(lines.iter().flat_map(|l| l.blocks.iter().map(|b| &b.id)));
    }
    // (d) the content of each removed page
    for page in &rv.removed_pages {
        counted.extend(
            reading_order(bm, &page.id)
                .into_iter()
                .filter(|(b, _)| content(b))
                .map(|(b, _)| &b.id),
        );
    }
    // (e) the docs and folders added, removed or moved, and those whose own field a reader sees
    // changed. A move to another parent is in `moved`; a new place under the same parent is in
    // `moved_in_place`. The name `order` in `fields` is never read: it is the order key or a
    // custom field, and neither alone moves a page (the task-5 ruling, Task 4's rule 1).
    let tree_entry = |e: &&CompareEntry| tree::is_tree_type(&e.block_type);
    let pages: Vec<BlockId> = cmp
        .added
        .iter()
        .chain(&cmp.deleted)
        .chain(&cmp.moved)
        .filter(tree_entry)
        .chain(
            cmp.changed
                .iter()
                .filter(tree_entry)
                .filter(|e| e.fields.iter().any(|f| PAGE_FIELDS.contains(&f.as_str()))),
        )
        .filter_map(|e| BlockId::parse(&e.id).ok())
        .chain(
            moved_in_place
                .iter()
                .filter(|id| {
                    dm.get(id)
                        .is_some_and(|b| tree::is_tree_type(&b.block_type))
                })
                .cloned(),
        )
        .collect();
    counted.extend(pages.iter());
    counted.into_iter().cloned().collect()
}

/// The scope a draft is read against: canon at the landing the view is based on, or canon itself
/// when that is canon's head (or the view floats). A proposal version (ls8) is read against canon
/// at the landing it is based on, or canon itself when that is canon's head. A version made before the first landing is based on the empty
/// canon: while nothing has landed that is canon, so it is reviewed as a view is (D-N5NSN, the
/// task-8 ruling); once canon has landed no scope names that base, and it has none. Canon and
/// as-of scopes have no base.
pub fn base_of(rdy: &ReadState, scope: &Scope) -> Option<Scope> {
    if let Scope::Proposal(p) = scope {
        // an unreadable version has no base here: its own read reports the error
        return match rdy.proposal_base(p).ok()? {
            // at canon's head that is canon itself: no as-of build of the same state
            Some(at) if Some(at) == rdy.canon_head() => Some(Scope::Canon),
            Some(at) => Some(Scope::At(at)),
            None if rdy.canon_head().is_none() => Some(Scope::Canon),
            None => None,
        };
    }
    let Scope::View(v) = scope else {
        return None;
    };
    match rdy.effective_base(v) {
        Some(b) if Some(b) != rdy.canon_head() => Some(Scope::At(b)),
        _ => Some(Scope::Canon),
    }
}

/// The review of `scope` against its base, or none when it has no base.
pub fn build(rdy: &ReadState, scope: &Scope) -> Result<Option<Review>, RdyError> {
    let Some(base) = base_of(rdy, scope) else {
        return Ok(None);
    };
    between(rdy, &base, scope).map(Some)
}

/// The changes from scope `from` to scope `to`, read as a review reads a view against its base:
/// the change summary of the landings since a tag reads it (sr6), so it counts by the same rule.
pub fn between(rdy: &ReadState, from: &Scope, to: &Scope) -> Result<Review, RdyError> {
    let (dm, bm) = (rdy.materialized(to)?, rdy.materialized(from)?);
    Ok(between_states(&bm, &dm))
}

/// The changes from state `from` to state `to`, by the rules of [`between`] (spv2, D-CWC91): a
/// review of a profile reads each side's selected state, and an empty side is
/// [`Materialized::default`]. A doc or folder that enters the selection is New and one that
/// leaves it is removed, even when its authored version is the same on both sides. The review
/// reads only the compare's state rows, never its conflicts, so no scope is needed.
pub(crate) fn between_states(from: &Materialized, to: &Materialized) -> Review {
    let cmp = compare_states(to, from);
    review(&cmp, to, from)
}

/// What a scope changes, as its change rows read it: the review against its base, else, for a
/// proposal version made on the empty project that no scope names, the review against that empty
/// project ([`unreviewed`]). The menu's count, the home page's list and the change summary all
/// read their rows from it (O12), so they agree. Review marks on the pages read only [`build`].
pub(crate) fn listed(rdy: &ReadState, scope: &Scope) -> Result<Option<Review>, RdyError> {
    match build(rdy, scope)? {
        Some(rv) => Ok(Some(rv)),
        None => unreviewed(rdy, scope),
    }
}

/// The count of [`Review::changed_blocks`] for a proposal version that has no base here: one
/// made before the first landing, read once canon has landed. It was made on the empty project,
/// so it is counted against that: every block it holds is added. Its page shows no marks
/// ([`base_of`]), but it still names the version and its count. Any other scope gives none.
pub fn unreviewed_changed_blocks(rdy: &ReadState, scope: &Scope) -> Result<Option<u32>, RdyError> {
    Ok(unreviewed(rdy, scope)?.map(|rv| rv.changed_blocks()))
}

/// The review behind [`unreviewed_changed_blocks`]: the proposal version against the empty
/// project. Its change summary lists every block it holds as new (sr6).
pub(crate) fn unreviewed(rdy: &ReadState, scope: &Scope) -> Result<Option<Review>, RdyError> {
    let Scope::Proposal(p) = scope else {
        return Ok(None);
    };
    if rdy.proposal_base(p)?.is_some() || rdy.canon_head().is_none() {
        return Ok(None);
    }
    let dm = rdy.materialized(scope)?;
    let empty = Materialized::default();
    let cmp = Compare {
        a: String::new(),
        b: String::new(),
        added: dm
            .iter()
            .map(|b| CompareEntry {
                id: b.id.to_string(),
                block_type: b.block_type.clone(),
                path: None,
                fields: vec![],
                from_parent: None,
                to_parent: None,
                conflict: None,
            })
            .collect(),
        deleted: vec![],
        changed: vec![],
        moved: vec![],
        conflicted: vec![],
    };
    Ok(Some(review(&cmp, &dm, &empty)))
}

/// On the page: neither a decision, its rationale nor a publication profile.
fn visible(b: &Block) -> bool {
    b.block_type != decision::DECISION && b.block_type != RATIONALE && !crate::site::is_profile(b)
}

fn ids(entries: &[CompareEntry]) -> impl Iterator<Item = BlockId> + '_ {
    entries.iter().filter_map(|e| BlockId::parse(&e.id).ok())
}

/// The changed entries a reader can see: an entry whose `fields` is empty differs only in its
/// pins (compare never lists pins), so it carries no Edited mark (orchestrator ruling,
/// DX4 final review). The name `order` stands for the order key or a custom field: it is seen
/// only when the custom field differs or the block took a new place among its siblings
/// (`moved_in_place`); a new order key at the same place is not seen (the task-5 ruling).
fn seen_changes<'a>(
    entries: &'a [CompareEntry],
    dm: &'a Materialized,
    bm: &'a Materialized,
    moved_in_place: &'a HashSet<BlockId>,
) -> impl Iterator<Item = BlockId> + 'a {
    entries.iter().filter_map(move |e| {
        let id = BlockId::parse(&e.id).ok()?;
        let custom_order = || {
            let field = |m: &Materialized| m.get(&id).and_then(|b| b.fields.get("order").cloned());
            field(dm) != field(bm)
        };
        let seen = e.fields.iter().any(|f| f != "order")
            || (e.fields.iter().any(|f| f == "order")
                && (moved_in_place.contains(&id) || custom_order()));
        seen.then_some(id)
    })
}

/// The blocks under the same parent on both sides that took a new place among their siblings
/// (Task 4's rule 1: movement is decided only from the structural place). A block moved in place
/// when both are true: its order key differs, and its index among the siblings both sides hold
/// under that parent differs. A new key at the same place (an append of the already-last block,
/// a rebalance) is no move; nor is a block that its siblings move around while it keeps its
/// key; nor are added or removed siblings. Each touched parent's children are walked once per
/// side.
fn reordered(cmp: &Compare, dm: &Materialized, bm: &Materialized) -> HashSet<BlockId> {
    let mut places: HashMap<Option<BlockId>, HashMap<BlockId, (usize, usize)>> = HashMap::new();
    let mut out = HashSet::new();
    for id in ids(&cmp.changed) {
        let (Some(new), Some(old)) = (dm.get(&id), bm.get(&id)) else {
            continue;
        };
        if new.parent != old.parent || new.order == old.order {
            continue;
        }
        let at = places.entry(new.parent.clone()).or_insert_with(|| {
            let parent = new.parent.as_ref();
            let (was, now) = (bm.children(parent), dm.children(parent));
            let in_now: HashSet<&BlockId> = now.iter().map(|b| &b.id).collect();
            let in_was: HashSet<&BlockId> = was.iter().map(|b| &b.id).collect();
            let mut at: HashMap<BlockId, (usize, usize)> = HashMap::new();
            for (i, b) in was.iter().filter(|b| in_now.contains(&b.id)).enumerate() {
                at.insert(b.id.clone(), (i, 0));
            }
            for (i, b) in now.iter().filter(|b| in_was.contains(&b.id)).enumerate() {
                if let Some(slot) = at.get_mut(&b.id) {
                    slot.1 = i;
                }
            }
            at
        });
        if at.get(&id).is_some_and(|(a, b)| a != b) {
            out.insert(id);
        }
    }
    out
}

fn doc_id<'m>(m: &'m Materialized, id: &BlockId) -> Option<&'m BlockId> {
    m.doc_of(id).map(|d| &d.id)
}

fn review(cmp: &Compare, dm: &Materialized, bm: &Materialized) -> Review {
    let mut rv = Review::default();
    // marks: new docs first, so that their blocks carry none of their own
    let added: Vec<(&Block, Mark)> = ids(&cmp.added)
        .filter_map(|id| dm.get(&id))
        .map(|b| (b, Mark::New))
        .collect();
    let new_docs: HashSet<&BlockId> = added
        .iter()
        .filter(|(b, _)| b.block_type == tree::DOC)
        .map(|(b, _)| &b.id)
        .collect();
    let moved_in_place = reordered(cmp, dm, bm);
    let edited: Vec<(&Block, Mark)> = seen_changes(&cmp.changed, dm, bm, &moved_in_place)
        .chain(ids(&cmp.moved))
        .filter_map(|id| dm.get(&id))
        .map(|b| (b, Mark::Edited))
        .collect();
    for (b, mark) in added.into_iter().chain(edited) {
        if !visible(b) || rv.marks.contains_key(&b.id) {
            continue;
        }
        let tree_block = tree::is_tree_type(&b.block_type);
        if !tree_block && doc_id(dm, &b.id).is_some_and(|d| new_docs.contains(d)) {
            continue;
        }
        rv.marks.insert(b.id.clone(), mark);
        if b.block_type == tree::FOLDER {
            rv.folders.insert(b.id.clone(), mark);
        }
    }
    // the blocks gone from their base doc: deleted, or moved into another doc
    let mut gone: HashSet<BlockId> = HashSet::new();
    for id in ids(&cmp.deleted) {
        if let Some(b) = bm.get(&id)
            && visible(b)
            && !tree::is_tree_type(&b.block_type)
        {
            gone.insert(id);
        }
    }
    for id in ids(&cmp.moved) {
        if let Some(b) = bm.get(&id)
            && visible(b)
            && !tree::is_tree_type(&b.block_type)
            && doc_id(bm, &id) != doc_id(dm, &id)
        {
            gone.insert(id);
        }
    }
    // the base docs those blocks were in, still docs in the draft, in the base's reading order
    let mut docs: Vec<&Block> = vec![];
    let mut seen: HashSet<&BlockId> = HashSet::new();
    for id in &gone {
        if let Some(doc) = bm.doc_of(id)
            && dm.get(&doc.id).is_some_and(|d| d.block_type == tree::DOC)
            && seen.insert(&doc.id)
        {
            docs.push(doc);
        }
    }
    for doc in docs {
        let lines = removed_lines(&doc.id, &gone, dm, bm);
        if !lines.is_empty() {
            rv.removed.insert(doc.id.clone(), lines);
        }
    }
    // per doc counts
    for (id, mark) in &rv.marks {
        let Some(b) = dm.get(id) else { continue };
        if b.block_type == tree::DOC {
            let change = rv.docs.entry(id.clone()).or_insert(DocChange {
                mark: *mark,
                edited: 0,
                new: 0,
                removed: 0,
            });
            if *mark == Mark::Edited {
                change.edited += 1;
            }
        } else if !tree::is_tree_type(&b.block_type)
            && let Some(doc) = doc_id(dm, id)
        {
            let change = rv.docs.entry(doc.clone()).or_insert(DocChange {
                mark: Mark::Edited,
                edited: 0,
                new: 0,
                removed: 0,
            });
            match mark {
                Mark::Edited => change.edited += 1,
                Mark::New => change.new += 1,
            }
        }
    }
    for (doc, lines) in &rv.removed {
        rv.docs
            .entry(doc.clone())
            .or_insert(DocChange {
                mark: Mark::Edited,
                edited: 0,
                new: 0,
                removed: 0,
            })
            .removed = lines.len();
    }
    rv.removed_pages = removed_pages(cmp, bm);
    rv.changed = count_changed(&rv, cmp, dm, bm, &moved_in_place);
    rv
}

/// Every block under `root` in `m`, depth first in reading order, with its depth below `root`.
fn reading_order<'m>(m: &'m Materialized, root: &BlockId) -> Vec<(&'m Block, usize)> {
    let mut out = vec![];
    let mut stack: Vec<(&Block, usize)> = m
        .children(Some(root))
        .into_iter()
        .rev()
        .map(|b| (b, 1))
        .collect();
    while let Some((b, depth)) = stack.pop() {
        out.push((b, depth));
        stack.extend(
            m.children(Some(&b.id))
                .into_iter()
                .rev()
                .map(|c| (c, depth + 1)),
        );
    }
    out
}

/// The removed lines of base doc `doc`: one per gone block with no gone base ancestor, holding it
/// and its gone visible descendants, placed before the next block after it that stays in the doc.
fn removed_lines(
    doc: &BlockId,
    gone: &HashSet<BlockId>,
    dm: &Materialized,
    bm: &Materialized,
) -> Vec<Removed> {
    let order = reading_order(bm, doc);
    let n = order.len();
    // the end of each block's subtree: the first later block that is not below it
    let mut end = vec![n; n];
    let mut open: Vec<usize> = vec![];
    for (i, (_, depth)) in order.iter().enumerate() {
        while let Some(&j) = open.last()
            && order[j].1 >= *depth
        {
            end[j] = i;
            open.pop();
        }
        open.push(i);
    }
    // the first block at or after each position that stays in the doc and is on the page
    let stays = |b: &Block| {
        dm.get(&b.id).is_some_and(|d| {
            visible(d) && !tree::is_tree_type(&d.block_type) && doc_id(dm, &d.id) == Some(doc)
        })
    };
    let mut next: Vec<Option<&BlockId>> = vec![None; n + 1];
    for i in (0..n).rev() {
        next[i] = if stays(order[i].0) {
            Some(&order[i].0.id)
        } else {
            next[i + 1]
        };
    }
    let left = |b: &Block| dm.get(&b.id).is_none() || doc_id(dm, &b.id) != Some(doc);
    let mut lines = vec![];
    // the end of the last line's subtree: a gone block before it has a gone base ancestor
    let mut inside = 0;
    for (i, (b, _)) in order.iter().enumerate() {
        if i < inside || !gone.contains(&b.id) {
            continue;
        }
        inside = end[i];
        let mut blocks = vec![(*b).clone()];
        blocks.extend(
            order[i + 1..end[i]]
                .iter()
                .map(|(c, _)| *c)
                .filter(|c| visible(c) && left(c))
                .cloned(),
        );
        lines.push(Removed {
            before: next[end[i]].cloned(),
            blocks,
        });
    }
    lines
}

/// The deleted docs and folders whose base parent is not deleted, in the base's reading order.
fn removed_pages(cmp: &Compare, bm: &Materialized) -> Vec<Block> {
    let deleted: HashSet<BlockId> = ids(&cmp.deleted).collect();
    let mut out = vec![];
    let mut stack: Vec<&Block> = bm.children(None).into_iter().rev().collect();
    while let Some(b) = stack.pop() {
        if !tree::is_tree_type(&b.block_type) {
            continue;
        }
        if deleted.contains(&b.id) && !b.parent.as_ref().is_some_and(|p| deleted.contains(p)) {
            out.push(b.clone());
        }
        stack.extend(bm.children(Some(&b.id)).into_iter().rev());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, fields: &[&str]) -> CompareEntry {
        CompareEntry {
            id: id.to_string(),
            block_type: "prose".to_string(),
            path: None,
            fields: fields.iter().map(|f| f.to_string()).collect(),
            from_parent: None,
            to_parent: None,
            conflict: None,
        }
    }

    #[test]
    fn rv1_a_pins_only_change_is_not_edited() {
        let e = [entry("p-AAAAA", &[]), entry("p-BBBBB", &["body"])];
        let none = Materialized::default();
        let seen: Vec<String> = seen_changes(&e, &none, &none, &HashSet::new())
            .map(|b| b.to_string())
            .collect();
        assert_eq!(seen, vec!["p-BBBBB".to_string()]);
    }

    /// The task-5 ruling: `order` alone is seen only for a new place among the siblings (or a
    /// custom field that differs, which needs both sides' blocks).
    #[test]
    fn sr5_an_order_key_alone_is_seen_only_for_a_new_place() {
        let e = [entry("p-AAAAA", &["order"]), entry("p-BBBBB", &["order"])];
        let none = Materialized::default();
        let moved: HashSet<BlockId> = [BlockId::parse("p-BBBBB").unwrap()].into();
        let seen: Vec<String> = seen_changes(&e, &none, &none, &moved)
            .map(|b| b.to_string())
            .collect();
        assert_eq!(seen, vec!["p-BBBBB".to_string()]);
    }

    // spv2 (D-CWC91): a review of two supplied selected states

    fn id(s: &str) -> BlockId {
        BlockId::parse(s).unwrap()
    }

    fn blk(i: &str, ty: &str, parent: Option<&str>, order: &str, body: &str) -> Block {
        let mut fields = std::collections::BTreeMap::new();
        if tree::is_tree_type(ty) {
            fields.insert("name".into(), rdy_model::FieldValue::Str(i.to_lowercase()));
            fields.insert("title".into(), rdy_model::FieldValue::Str(i.to_string()));
        }
        Block {
            id: id(i),
            block_type: ty.to_string(),
            parent: parent.map(id),
            order: order.to_string(),
            fields,
            body: body.to_string(),
            links: Default::default(),
            pins: Default::default(),
            read_identity: None,
        }
    }

    fn state(blocks: &[&Block]) -> Materialized {
        let ops: Vec<rdy_model::Op> = blocks
            .iter()
            .map(|b| rdy_model::Op::Create {
                block: (*b).clone(),
            })
            .collect();
        Materialized::default().with(&ops)
    }

    fn changed(rv: &Review) -> Vec<String> {
        rv.changed_set().iter().map(|b| b.to_string()).collect()
    }

    fn sorted(ids: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        v.sort();
        v
    }

    /// The same authored blocks on both sides: only the selection differs. A doc or folder that
    /// enters is New with its content counted once; one that leaves is a removed page, top-most
    /// only, with its content counted once.
    #[test]
    fn spv2_selection_only_changes_are_new_and_removed() {
        let f = blk("f-AAAAA", tree::FOLDER, None, "a", "");
        let d1 = blk("d-AAAAA", tree::DOC, Some("f-AAAAA"), "a", "");
        let p1 = blk("p-AAAAA", "prose", Some("d-AAAAA"), "a", "One.");
        let p2 = blk("p-BBBBB", "prose", Some("d-AAAAA"), "b", "Two.");
        let d2 = blk("d-BBBBB", tree::DOC, Some("f-AAAAA"), "b", "");
        let q1 = blk("p-CCCCC", "prose", Some("d-BBBBB"), "a", "Three.");
        let g = blk("f-BBBBB", tree::FOLDER, None, "c", "");
        let d3 = blk("d-CCCCC", tree::DOC, Some("f-BBBBB"), "a", "");
        let r1 = blk("p-DDDDD", "prose", Some("d-CCCCC"), "a", "Four.");
        let h = blk("f-CCCCC", tree::FOLDER, None, "b", "");
        let d4 = blk("d-DDDDD", tree::DOC, Some("f-CCCCC"), "a", "");
        let s1 = blk("p-EEEEE", "prose", Some("d-DDDDD"), "a", "Five.");
        let from = state(&[&f, &d1, &p1, &p2, &h, &d4, &s1]);
        let to = state(&[&f, &d2, &q1, &g, &d3, &r1]);
        let rv = between_states(&from, &to);
        // entering: the docs and the folder are New; their content carries no mark of its own
        for page in [&d2.id, &g.id, &d3.id] {
            assert_eq!(rv.page_mark(page), Some(Mark::New), "{page}");
        }
        assert_eq!(rv.doc(&d2.id).map(|d| d.mark), Some(Mark::New));
        assert_eq!(rv.doc(&d2.id).and_then(|d| d.counts()), None);
        assert_eq!(rv.mark(&q1.id), None);
        assert_eq!(rv.mark(&r1.id), None);
        assert_eq!(rv.mark(&f.id), None, "the shared folder is unchanged");
        // leaving: the removed pages, top-most only, in the base's reading order
        let removed: Vec<&BlockId> = rv.removed_pages().iter().map(|b| &b.id).collect();
        assert_eq!(removed, vec![&d1.id, &h.id]);
        assert!(
            rv.removed_in(&d1.id).is_empty(),
            "a removed page has no removed lines"
        );
        // each page and its content once, never twice through its folder
        assert_eq!(
            changed(&rv),
            sorted(&[
                "d-BBBBB", "p-CCCCC", "f-BBBBB", "d-CCCCC", "p-DDDDD", "d-AAAAA", "p-AAAAA",
                "p-BBBBB", "f-CCCCC", "d-DDDDD", "p-EEEEE"
            ])
        );
        assert_eq!(rv.changed_blocks(), 11);
        // the other way round the roles swap
        let back = between_states(&to, &from);
        assert_eq!(back.page_mark(&d1.id), Some(Mark::New));
        assert_eq!(back.page_mark(&h.id), Some(Mark::New));
        let removed: Vec<&BlockId> = back.removed_pages().iter().map(|b| &b.id).collect();
        assert_eq!(removed, vec![&d2.id, &g.id]);
        assert_eq!(back.changed_blocks(), 11);
        // equal selections change nothing
        let same = between_states(&to, &to);
        assert_eq!(same.changed_blocks(), 0);
        assert!(same.removed_pages().is_empty());
    }

    /// Against an empty side every selected block a reader sees is New and counted once, as the
    /// existing count gives it; decisions, rationale and profiles count nothing, and a change to
    /// a block's pins alone is no change.
    #[test]
    fn spv2_empty_side_and_hidden_blocks_use_existing_counts() {
        let f = blk("f-AAAAA", tree::FOLDER, None, "a", "");
        let d = blk("d-AAAAA", tree::DOC, Some("f-AAAAA"), "a", "");
        let p1 = blk("p-AAAAA", "prose", Some("d-AAAAA"), "a", "One.");
        let p2 = blk("p-BBBBB", "prose", Some("d-AAAAA"), "b", "Two.");
        let dec = blk(
            "dc-AAAAA",
            decision::DECISION,
            Some("d-AAAAA"),
            "c",
            "Use one.",
        );
        let why = blk("r-AAAAA", RATIONALE, Some("dc-AAAAA"), "a", "Because.");
        let pf = blk(
            "pf-AAAAA",
            crate::site::PUBLICATION_PROFILE,
            Some("d-AAAAA"),
            "d",
            "",
        );
        let all = [&f, &d, &p1, &p2, &dec, &why, &pf];
        let rv = between_states(&Materialized::default(), &state(&all));
        assert_eq!(rv.page_mark(&f.id), Some(Mark::New));
        assert_eq!(rv.page_mark(&d.id), Some(Mark::New));
        for hidden in [&dec.id, &why.id, &pf.id] {
            assert_eq!(rv.mark(hidden), None, "{hidden}");
        }
        assert_eq!(
            changed(&rv),
            sorted(&["f-AAAAA", "d-AAAAA", "p-AAAAA", "p-BBBBB"])
        );
        // the same count the existing review gives that state against the empty project
        let none = Materialized::default();
        let to = state(&all);
        let cmp = compare_states(&to, &none);
        assert_eq!(changed(&review(&cmp, &to, &none)), changed(&rv));
        // everything leaves: one removed page, its visible content counted
        let gone = between_states(&state(&all), &Materialized::default());
        assert_eq!(gone.removed_pages().len(), 1);
        assert_eq!(gone.changed_blocks(), 4);

        // hidden blocks edited, added and removed, and a pins-only change: nothing counts
        let mut pinned = p1.clone();
        pinned.pins.insert(p2.id.clone(), p2.version_hash());
        let mut dec2 = dec.clone();
        dec2.body = "Use two.".into();
        let mut pf2 = pf.clone();
        pf2.body = "changed".into();
        let why2 = blk("r-BBBBB", RATIONALE, Some("dc-AAAAA"), "b", "Also.");
        let from = state(&[&f, &d, &p1, &p2, &dec, &why, &pf]);
        let to = state(&[&f, &d, &pinned, &p2, &dec2, &why2, &pf2]);
        let rv = between_states(&from, &to);
        assert_eq!(rv.changed_blocks(), 0, "{:?}", changed(&rv));
        assert_eq!(rv.mark(&p1.id), None);
        assert!(rv.doc(&d.id).is_none());
        assert!(rv.removed_in(&d.id).is_empty());
    }

    /// The review of supplied states keeps the existing semantics: a type change, a custom
    /// `type` or `order` field, a move to another doc, a new place among siblings and removed
    /// code; a new order key at the same place is no change.
    #[test]
    fn spv2_review_keeps_moves_fields_and_reading_order() {
        let a = blk("d-AAAAA", tree::DOC, None, "a", "");
        let b = blk("d-BBBBB", tree::DOC, None, "b", "");
        let retyped = blk("p-AAAAA", "prose", Some("d-AAAAA"), "a", "Retyped.");
        let typed = blk("p-BBBBB", "prose", Some("d-AAAAA"), "b", "Custom type.");
        let ordered = blk("p-CCCCC", "prose", Some("d-AAAAA"), "c", "Custom order.");
        let rekeyed = blk("p-DDDDD", "prose", Some("d-AAAAA"), "d", "Same place.");
        let mover = blk("p-EEEEE", "prose", Some("d-AAAAA"), "e", "Moves to b.");
        let code = blk("p-FFFFF", "code", Some("d-AAAAA"), "f", "fn main() {}");
        let last = blk("p-GGGGG", "prose", Some("d-AAAAA"), "g", "Stays last.");
        let s1 = blk("p-HHHHH", "prose", Some("d-BBBBB"), "a", "First.");
        let s2 = blk("p-JJJJJ", "prose", Some("d-BBBBB"), "b", "Second.");
        let s3 = blk("p-KKKKK", "prose", Some("d-BBBBB"), "c", "Third.");
        let from = state(&[
            &a, &b, &retyped, &typed, &ordered, &rekeyed, &mover, &code, &last, &s1, &s2, &s3,
        ]);
        let mut retyped2 = retyped.clone();
        retyped2.block_type = "quote".into();
        let mut typed2 = typed.clone();
        typed2
            .fields
            .insert("type".into(), rdy_model::FieldValue::Str("note".into()));
        let mut ordered2 = ordered.clone();
        ordered2
            .fields
            .insert("order".into(), rdy_model::FieldValue::Str("2".into()));
        let mut rekeyed2 = rekeyed.clone();
        rekeyed2.order = "dd".into();
        let mut mover2 = mover.clone();
        mover2.parent = Some(b.id.clone());
        mover2.order = "z".into();
        let mut s3moved = s3.clone();
        s3moved.order = "0".into();
        let to = state(&[
            &a, &b, &retyped2, &typed2, &ordered2, &rekeyed2, &mover2, &last, &s1, &s2, &s3moved,
        ]);
        let rv = between_states(&from, &to);
        for edited in [&retyped.id, &typed.id, &ordered.id, &mover.id, &s3.id] {
            assert_eq!(rv.mark(edited), Some(Mark::Edited), "{edited}");
        }
        assert_eq!(rv.mark(&rekeyed.id), None, "a new key at the same place");
        assert_eq!(rv.mark(&s1.id), None, "moved around, it kept its key");
        assert_eq!(rv.mark(&a.id), None);
        // removed in a's reading order: the moved block, then the code, each before the next
        // block that stays
        let lines: Vec<(Option<&BlockId>, Vec<&BlockId>)> = rv
            .removed_in(&a.id)
            .iter()
            .map(|l| (l.before.as_ref(), l.blocks.iter().map(|b| &b.id).collect()))
            .collect();
        assert_eq!(
            lines,
            vec![
                (Some(&last.id), vec![&mover.id]),
                (Some(&last.id), vec![&code.id]),
            ]
        );
        assert_eq!(rv.removed_in(&a.id)[1].blocks[0].body, "fn main() {}");
        let da = rv.doc(&a.id).unwrap();
        assert_eq!(
            (da.mark, da.edited, da.new, da.removed),
            (Mark::Edited, 3, 0, 2)
        );
        assert_eq!(da.counts().as_deref(), Some("3 edited \u{b7} 2 removed"));
        let db = rv.doc(&b.id).unwrap();
        assert_eq!(
            (db.mark, db.edited, db.new, db.removed),
            (Mark::Edited, 2, 0, 0)
        );
        assert_eq!(
            changed(&rv),
            sorted(&[
                "p-AAAAA", "p-BBBBB", "p-CCCCC", "p-EEEEE", "p-FFFFF", "p-KKKKK"
            ])
        );
        // a doc moved under another parent is a page move: Edited and counted
        let f = blk("f-AAAAA", tree::FOLDER, None, "c", "");
        let mut a_moved = a.clone();
        a_moved.parent = Some(f.id.clone());
        let rv = between_states(&state(&[&a, &f]), &state(&[&a_moved, &f]));
        assert_eq!(rv.page_mark(&a.id), Some(Mark::Edited));
        assert_eq!(changed(&rv), vec!["d-AAAAA".to_string()]);
    }
}
