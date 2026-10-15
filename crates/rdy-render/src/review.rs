//! The review: what a draft changes against the canon it is based on. A draft render reads it to
//! mark the blocks a reader sees as edited or new, to show the text a draft removed where it
//! stood, and to count each doc's changes. Only what is on the page counts: decisions and
//! rationale give no mark, no count and no removed line. Every word the review gives is a reader
//! word (`Edited`, `New`, a count like `2 edited · 1 new · 1 removed`), never an id, a hash or a
//! type name.
use crate::site::RATIONALE;
use rdy_engine::{Compare, CompareEntry, Materialized, RdyError, ReadState, Scope};
use rdy_model::{Block, BlockId, decision, tree};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

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
    /// The count of changed blocks (O2); see [`Review::changed_blocks`].
    changed_blocks: u32,
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
    /// (its parent, or its order under the same parent) or own field a reader sees (`title`,
    /// `name`, `description`) changed. Decisions, rationale and pins-only changes never count.
    pub fn changed_blocks(&self) -> u32 {
        self.changed_blocks
    }
}

/// The fields of a doc or folder that a reader sees: a change in one counts the page (O2 (e)).
const PAGE_FIELDS: [&str; 3] = ["title", "name", "description"];

/// [`Review::changed_blocks`], worked out once from the compare and both sides.
fn count_changed(rv: &Review, cmp: &Compare, dm: &Materialized, bm: &Materialized) -> u32 {
    let mut counted: HashSet<&BlockId> = HashSet::new();
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
    // changed (a move to another parent is in `moved`; a new place under the same parent is an
    // `order` change)
    let tree_entry = |e: &&CompareEntry| tree::is_tree_type(&e.block_type);
    let pages: Vec<BlockId> = cmp
        .added
        .iter()
        .chain(&cmp.deleted)
        .chain(&cmp.moved)
        .filter(tree_entry)
        .chain(cmp.changed.iter().filter(tree_entry).filter(|e| {
            e.fields
                .iter()
                .any(|f| f == "order" || PAGE_FIELDS.contains(&f.as_str()))
        }))
        .filter_map(|e| BlockId::parse(&e.id).ok())
        .collect();
    counted.extend(pages.iter());
    u32::try_from(counted.len()).unwrap_or(u32::MAX)
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
    let cmp = rdy.compare(scope, &base)?;
    let (dm, bm) = (rdy.materialized(scope)?, rdy.materialized(&base)?);
    Ok(Some(review(&cmp, &dm, &bm)))
}

/// The count of [`Review::changed_blocks`] for a proposal version that has no base here: one
/// made before the first landing, read once canon has landed. It was made on the empty project,
/// so it is counted against that: every block it holds is added. Its page shows no marks
/// ([`base_of`]), but it still names the version and its count. Any other scope gives none.
pub fn unreviewed_changed_blocks(rdy: &ReadState, scope: &Scope) -> Result<Option<u32>, RdyError> {
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
    Ok(Some(review(&cmp, &dm, &empty).changed_blocks()))
}

/// On the page: neither a decision nor its rationale.
fn visible(b: &Block) -> bool {
    b.block_type != decision::DECISION && b.block_type != RATIONALE
}

fn ids(entries: &[CompareEntry]) -> impl Iterator<Item = BlockId> + '_ {
    entries.iter().filter_map(|e| BlockId::parse(&e.id).ok())
}

/// The changed entries a reader can see: an entry whose `fields` is empty differs only in its
/// pins (compare never lists pins), so it carries no Edited mark (orchestrator ruling,
/// DX4 final review).
fn seen_changes(entries: &[CompareEntry]) -> impl Iterator<Item = BlockId> + '_ {
    entries
        .iter()
        .filter(|e| !e.fields.is_empty())
        .filter_map(|e| BlockId::parse(&e.id).ok())
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
    let edited: Vec<(&Block, Mark)> = seen_changes(&cmp.changed)
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
    rv.changed_blocks = count_changed(&rv, cmp, dm, bm);
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
        let seen: Vec<String> = seen_changes(&e).map(|b| b.to_string()).collect();
        assert_eq!(seen, vec!["p-BBBBB".to_string()]);
    }
}
