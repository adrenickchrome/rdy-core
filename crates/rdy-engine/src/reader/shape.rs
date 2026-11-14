//! The site's shape, read once for every output: the page of each folder and doc, the order a
//! reader walks the docs in, each block's number in its doc, the section that holds a block,
//! and a block's place among the siblings that both sides of a comparison hold. The site
//! (rdy-render) builds its pages from it, and the publication review reads its pages, reading
//! places and sections from it.
//!
//! A doc that the site gives no page keeps its reading place: [`Shape::reading`] lists every
//! folder and doc depth first, and [`Shape::page`] is `None` for one with no page.
use crate::error::E_PATH_CLASH;
use crate::{Materialized, RdyError};
use rdy_model::{Block, BlockId, tree};
use std::collections::BTreeMap;

/// The page the generated glossary owns: a top-level doc of this name has no place on the site.
pub const GLOSSARY_NAME: &str = "glossary";

/// The block types numbered within their doc (`Table 2`, `Python code 3`, `Diagram 1`).
pub const NUMBERED: [&str; 3] = ["table", "code", "diagram"];

/// Two siblings whose names match ignoring case: their pages would be one file on a file system
/// that ignores case. Names are unique ignoring case among siblings, but a removed doc or folder
/// is named by its id (D-3M81W), which a sibling's name may match. As checkout does, a build that
/// writes files refuses with `E_PATH_CLASH`, and the fix names the sibling a person can rename:
/// the removed one has no name to change.
#[derive(Clone, Debug)]
pub struct PathClash {
    /// The removed doc or folder of the pair, if one is.
    removed: Option<(BlockId, bool)>,
    /// The sibling the fix names (the second of the pair when neither is removed).
    other: BlockId,
    /// The first of the pair, when neither is removed.
    first: BlockId,
    /// The name of `other`.
    name: String,
}

impl PathClash {
    fn of(
        m: &Materialized,
        names: &BTreeMap<BlockId, String>,
        first: &BlockId,
        second: &BlockId,
    ) -> PathClash {
        let removed = |id: &BlockId| {
            m.get(id)
                .filter(|b| b.is_redacted())
                .map(|b| (b.id.clone(), b.block_type == tree::FOLDER))
        };
        let (removed, other) = match (removed(first), removed(second)) {
            (Some(r), _) => (Some(r), second),
            (None, Some(r)) => (Some(r), first),
            (None, None) => (None, second),
        };
        PathClash {
            removed,
            other: other.clone(),
            first: first.clone(),
            name: names.get(other).cloned().unwrap_or_default(),
        }
    }

    /// The refusal of a build that writes the pages as files; `verb` is the command to run
    /// again.
    pub fn error(&self, verb: &str) -> RdyError {
        let PathClash {
            removed,
            other,
            first,
            name,
        } = self;
        match removed {
            Some((id, folder)) => {
                let (what, file) = match folder {
                    true => ("folder", "directory"),
                    false => ("doc", "file"),
                };
                RdyError::new(
                    E_PATH_CLASH,
                    format!(
                        "{other} is named {name:?}, which matches the {file} name of redacted {what} {id} (ignoring case), so they cannot both be written as files"
                    ),
                    format!(
                        "rename {other} (a redacted {what}'s {file} is named by its id), then {verb} again"
                    ),
                )
            }
            None => RdyError::new(
                E_PATH_CLASH,
                format!(
                    "{first} and {other} are both named {name:?} (ignoring case) under the same parent, so they cannot both be written as files"
                ),
                format!("rename one of them, then {verb} again"),
            ),
        }
    }
}

/// The shape of one state's site: built once by [`Shape::build`], read by the site and the
/// publication review.
#[derive(Clone, Debug)]
pub struct Shape<'a> {
    m: &'a Materialized,
    /// Every folder's and doc's tree path, for those that have a page.
    paths: BTreeMap<BlockId, String>,
    /// Every doc that has a page, in reading order: depth-first, siblings by their order.
    order: Vec<BlockId>,
    /// Every folder and doc that has a page, in tree order: a folder, then what it holds,
    /// siblings by their order.
    tree: Vec<BlockId>,
    /// The top-level docs named `glossary` (in any case): the generated glossary owns
    /// `glossary.html` (D-Y4884), so they have no page.
    glossary_docs: Vec<BlockId>,
    /// The first two siblings, in tree order, whose names match ignoring case.
    path_clash: Option<PathClash>,
    /// The folder or doc whose page each page is (the first in tree order that has it).
    page_owner: BTreeMap<String, BlockId>,
    /// Every doc's blocks in reading order, walked once.
    doc_blocks: BTreeMap<BlockId, Vec<&'a Block>>,
    /// Every table, code and diagram block's number among the blocks of its kind in its doc,
    /// from 1 in reading order.
    numbers: BTreeMap<BlockId, usize>,
    /// Every folder and doc in depth-first reading order, siblings by their order, with or
    /// without a page.
    reading: Vec<BlockId>,
    /// Each entry's place in `reading`.
    positions: BTreeMap<BlockId, usize>,
}

impl<'a> Shape<'a> {
    /// The shape of `m`'s site.
    pub fn build(m: &'a Materialized) -> Shape<'a> {
        // `level` folders are above `parent`'s children; only a damaged store nests deeper than
        // MAX_FOLDER_DEPTH, and what lies below it gets no page
        #[allow(clippy::too_many_arguments)]
        fn walk(
            m: &Materialized,
            parent: Option<&BlockId>,
            prefix: &str,
            level: usize,
            out: &mut BTreeMap<BlockId, String>,
            order: &mut Vec<BlockId>,
            tree_order: &mut Vec<BlockId>,
            dropped: &mut Vec<BlockId>,
            clash: &mut Option<PathClash>,
        ) {
            if level > tree::MAX_FOLDER_DEPTH {
                return;
            }
            let names = m.names_under(parent);
            let mut used: BTreeMap<String, &BlockId> = BTreeMap::new();
            for b in m.children(parent) {
                let Some(name) = names.get(&b.id) else {
                    continue;
                };
                // two siblings whose names match ignoring case would be one file where case is
                // ignored: the first such pair in tree order refuses a build that writes files
                if let Some(other) = used.insert(name.to_ascii_lowercase(), &b.id)
                    && clash.is_none()
                {
                    *clash = Some(PathClash::of(m, &names, other, &b.id));
                }
                // the generated glossary owns `glossary.html`: a top-level doc of that name, in
                // any case (names and many file systems ignore case), has no page, no place in
                // the navigation, and its blocks are not linked
                if parent.is_none()
                    && b.block_type == tree::DOC
                    && name.eq_ignore_ascii_case(GLOSSARY_NAME)
                {
                    dropped.push(b.id.clone());
                    continue;
                }
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}/{name}")
                };
                out.insert(b.id.clone(), path.clone());
                tree_order.push(b.id.clone());
                if b.block_type == tree::FOLDER {
                    walk(
                        m,
                        Some(&b.id),
                        &path,
                        level + 1,
                        out,
                        order,
                        tree_order,
                        dropped,
                        clash,
                    );
                } else if b.block_type == tree::DOC {
                    order.push(b.id.clone());
                }
            }
        }
        let mut paths = BTreeMap::new();
        let mut order = vec![];
        let mut tree_order = vec![];
        let mut dropped = vec![];
        let mut path_clash = None;
        walk(
            m,
            None,
            "",
            0,
            &mut paths,
            &mut order,
            &mut tree_order,
            &mut dropped,
            &mut path_clash,
        );
        let reading = reading_tree(m);
        let positions = reading
            .iter()
            .enumerate()
            .map(|(i, id)| (id.clone(), i))
            .collect();
        let mut shape = Shape {
            m,
            paths,
            order,
            tree: tree_order,
            glossary_docs: dropped,
            path_clash,
            page_owner: BTreeMap::new(),
            doc_blocks: BTreeMap::new(),
            numbers: BTreeMap::new(),
            reading,
            positions,
        };
        shape.doc_blocks = shape
            .order
            .iter()
            .map(|d| (d.clone(), doc_blocks(m, d)))
            .collect();
        let mut page_owner = BTreeMap::new();
        for id in &shape.tree {
            if let Some(p) = shape.page(id) {
                page_owner.entry(p).or_insert_with(|| id.clone());
            }
        }
        shape.page_owner = page_owner;
        let mut numbers = BTreeMap::new();
        for doc in &shape.order {
            let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
            for b in shape.blocks_of(doc) {
                if NUMBERED.contains(&b.block_type.as_str()) {
                    let n = counts.entry(b.block_type.as_str()).or_default();
                    *n += 1;
                    numbers.insert(b.id.clone(), *n);
                }
            }
        }
        shape.numbers = numbers;
        shape
    }

    /// The state this shape reads.
    pub fn state(&self) -> &'a Materialized {
        self.m
    }

    /// The page of a folder or doc, relative to the site root (`a/b.html`, `a/index.html`);
    /// `None` for one the site gives no page.
    pub fn page(&self, id: &BlockId) -> Option<String> {
        let p = self.paths.get(id)?;
        Some(match self.m.get(id)?.block_type == tree::FOLDER {
            true => format!("{p}/index.html"),
            false => format!("{p}.html"),
        })
    }

    /// Whether a doc or folder already has the page at `page`.
    pub fn page_taken(&self, page: &str) -> bool {
        self.page_owner.contains_key(page)
    }

    /// The first two siblings whose names match ignoring case, if any.
    pub fn path_clash(&self) -> Option<&PathClash> {
        self.path_clash.as_ref()
    }

    /// Every doc that has a page, in reading order.
    pub fn docs(&self) -> &[BlockId] {
        &self.order
    }

    /// Every folder and doc that has a page, in tree order.
    pub fn tree(&self) -> &[BlockId] {
        &self.tree
    }

    /// The top-level docs that gave way to the generated glossary: they have no page.
    pub fn glossary_docs(&self) -> &[BlockId] {
        &self.glossary_docs
    }

    /// Every block under a doc that has a page, in reading order.
    pub fn blocks_of(&self, doc: &BlockId) -> &[&'a Block] {
        self.doc_blocks.get(doc).map_or(&[], Vec::as_slice)
    }

    /// The number of a table, code or diagram block among the blocks of its kind in its doc.
    pub fn number(&self, id: &BlockId) -> Option<usize> {
        self.numbers.get(id).copied()
    }

    /// Every folder and doc in depth-first reading order, siblings by their order keys, whether
    /// or not the site gives it a page.
    pub fn reading(&self) -> &[BlockId] {
        &self.reading
    }

    /// The place of a folder or doc in [`Shape::reading`].
    pub fn position(&self, id: &BlockId) -> Option<usize> {
        self.positions.get(id).copied()
    }
}

/// Every folder and doc of `m` in depth-first reading order, siblings by their order keys. A
/// block is visited once, so a damaged store that loops still ends.
fn reading_tree(m: &Materialized) -> Vec<BlockId> {
    let mut out = vec![];
    let mut seen = std::collections::BTreeSet::new();
    let mut stack: Vec<&Block> = m
        .children(None)
        .into_iter()
        .filter(|b| tree::is_tree_type(&b.block_type))
        .rev()
        .collect();
    while let Some(b) = stack.pop() {
        if !seen.insert(&b.id) {
            continue;
        }
        out.push(b.id.clone());
        stack.extend(
            m.children(Some(&b.id))
                .into_iter()
                .filter(|c| tree::is_tree_type(&c.block_type))
                .rev(),
        );
    }
    out
}

/// Each block's place in the depth-first reading order of `m`, siblings by their order keys. A
/// block is visited once, so a damaged store that loops still ends.
pub fn reading_positions(m: &Materialized) -> BTreeMap<BlockId, usize> {
    let mut out = BTreeMap::new();
    let mut stack: Vec<&Block> = m.children(None).into_iter().rev().collect();
    while let Some(b) = stack.pop() {
        if out.contains_key(&b.id) {
            continue;
        }
        out.insert(b.id.clone(), out.len());
        stack.extend(m.children(Some(&b.id)).into_iter().rev());
    }
    out
}

/// Every block under `doc`, in reading order, walked from the state.
pub fn doc_blocks<'m>(m: &'m Materialized, doc: &BlockId) -> Vec<&'m Block> {
    let mut out = vec![];
    let mut stack: Vec<&Block> = m.children(Some(doc)).into_iter().rev().collect();
    while let Some(b) = stack.pop() {
        out.push(b);
        stack.extend(m.children(Some(&b.id)).into_iter().rev());
    }
    out
}

/// Every block under `root` in `m`, depth first in reading order, with its depth below `root`.
pub fn blocks_under<'m>(m: &'m Materialized, root: &BlockId) -> Vec<(&'m Block, usize)> {
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

/// The nearest section above `id` in `m`; none when the block sits directly in its doc or
/// outside any section (a leading block has no section).
pub fn section_of<'m>(m: &'m Materialized, id: &BlockId) -> Option<&'m Block> {
    let mut cur = m.get(id)?.parent.as_ref().and_then(|p| m.get(p));
    // a damaged store may loop: never walk more steps than there are blocks
    for _ in 0..=m.len() {
        let p = cur?;
        if p.block_type == "section" {
            return Some(p);
        }
        cur = p.parent.as_ref().and_then(|p| m.get(p));
    }
    None
}

/// The place of each block that both `from` and `to` hold under `parent`, among the siblings
/// both sides hold there: its index in `from`, then in `to`. A block moved in place when the two
/// differ; added and removed siblings move nothing.
pub fn sibling_places(
    from: &Materialized,
    to: &Materialized,
    parent: Option<&BlockId>,
) -> BTreeMap<BlockId, (usize, usize)> {
    let old: Vec<&BlockId> = from.children(parent).iter().map(|b| &b.id).collect();
    let new: Vec<&BlockId> = to.children(parent).iter().map(|b| &b.id).collect();
    let in_new: std::collections::BTreeSet<&BlockId> = new.iter().copied().collect();
    let in_old: std::collections::BTreeSet<&BlockId> = old.iter().copied().collect();
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
}
