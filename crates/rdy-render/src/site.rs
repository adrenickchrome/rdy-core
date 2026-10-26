//! The site's shape: which page every folder, doc and block lives on, where on the page, and
//! the order a reader walks the docs in.
use crate::inline::{self, Form};
use rdy_engine::{E_PATH_CLASH, Materialized, RdyError};
use rdy_model::redaction::CONTENT_REMOVED;
use rdy_model::{Block, BlockId, RefMode, decision, entity, tree};
use rdy_schema::Pack;
use regex::Regex;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::LazyLock;

/// Where a block lives: a page of the site and, for a block inside a page, its anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub page: String,
    pub anchor: Option<String>,
}

impl Url {
    /// The href of this url as seen from page `from`: just `#anchor` on the same page.
    pub fn href_from(&self, from: &str) -> String {
        match (&self.anchor, self.page == from) {
            (Some(a), true) => format!("#{a}"),
            (Some(a), false) => format!("{}#{a}", rel(from, &self.page)),
            (None, _) => rel(from, &self.page),
        }
    }
}

/// A link from page `from` to page `to`, both site-relative.
pub fn rel(from: &str, to: &str) -> String {
    format!("{}{to}", "../".repeat(from.matches('/').count()))
}

/// The most characters an anchor keeps of the text it is made from.
const ANCHOR_MAX: usize = 48;

/// The shape of a block id (`d-7KQ2M`): an anchor never takes it, because a visible id is a leak.
static ID_SHAPED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z]{1,3}-[0-9A-HJKMNP-TV-Z]{5}$").expect("a pattern"));

/// Supporting text: off the site (FW-D10). It has no anchor; a link to it leads to its doc's
/// page.
pub const RATIONALE: &str = "rationale";

/// A publication profile: administrative, off every site and print output, removed or not. It
/// has no item, anchor, permalink, search entry or text of its own; a link to it leads to its
/// doc's page and reads as that doc.
pub const PUBLICATION_PROFILE: &str = "publication_profile";

/// Whether `b` is supporting content off the site and every print: a rationale (FW-D10), or a
/// finding or a source, which live only in the store (D-Z8P09). None has an anchor or a place in
/// the text; a link to one leads to its doc's page.
pub fn is_off_site(b: &Block) -> bool {
    matches!(
        b.block_type.as_str(),
        RATIONALE | rdy_schema::supporting::FINDING | rdy_schema::supporting::SOURCE
    )
}

/// Whether `b` is a publication profile ([`PUBLICATION_PROFILE`]).
pub fn is_profile(b: &Block) -> bool {
    b.block_type == PUBLICATION_PROFILE
}

/// The block types numbered within their doc (`Table 2`, `Python code 3`, `Diagram 1`).
const NUMBERED: [&str; 3] = ["table", "code", "diagram"];

/// How many references deep a label reads its references' labels; past it a reference reads
/// as `…` (a title that names a block whose title names it back stops here).
const LABEL_DEPTH: usize = 3;

/// The key a reference typed as a key (`{{id|@key}}`) reads as, if the target has one. A doc, a
/// folder or a section has a title a reader knows it by, so a key-typed reference to one reads as
/// that title, as `{{id}}` does (FW-F2b); a value reads as its value (FW-EN2); any other keyed
/// block reads as its key (FW-F2a). A publication profile's key is its own text, off the site:
/// a key-typed reference to one reads as its doc, as `{{id}}` does.
pub fn typed_key(b: &Block) -> Option<&str> {
    match b.block_type.as_str() {
        tree::DOC | tree::FOLDER | "section" | entity::VALUE | PUBLICATION_PROFILE => None,
        _ => b.field_str("key"),
    }
}

/// The id inside a longer text, as the leak scan reads it.
pub(crate) static ID_INSIDE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[A-Za-z]{1,3}-[0-9A-HJKMNP-TV-Z]{5}\b").expect("a pattern"));

/// One disagreement of the disagreements page: entries with the same sentence and the same pages
/// are one group (ls11). A group of one entry keeps that entry's blocks; a group of more has one
/// block per page. `repeats` is the number of entries; the page does not show it (D-QWSTV).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisagreementGroup {
    pub sentence: &'static str,
    pub blocks: Vec<BlockId>,
    pub repeats: usize,
}

/// One group of the glossary page: the terms of the docs under one top-level folder, or of the
/// top-level docs (the project's group), A to Z (FW-GL1, D-Y4884).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlossaryGroup {
    /// The folder's label, or the project's name.
    pub label: String,
    /// The group heading's anchor on `glossary.html`.
    pub anchor: String,
    pub terms: Vec<BlockId>,
}

/// The page the generated glossary owns: a top-level doc of this name has no place on the site.
pub const GLOSSARY_NAME: &str = "glossary";

/// Two siblings whose names match ignoring case: their pages would be one file on a file system
/// that ignores case. Names are unique ignoring case among siblings, but a removed doc or folder
/// is named by its id (D-3M81W), which a sibling's name may match. As checkout does, a build that
/// writes files refuses with `E_PATH_CLASH`, and the fix names the sibling a person can rename:
/// the removed one has no name to change.
#[derive(Clone, Debug)]
struct PathClash {
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

    fn error(&self, verb: &str) -> RdyError {
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

pub struct Site<'a> {
    pub m: &'a Materialized,
    /// Every folder's and doc's tree path.
    paths: BTreeMap<BlockId, String>,
    /// Every block inside a doc that has a page, by the anchor it has on that page.
    pub anchors: BTreeMap<BlockId, String>,
    /// Every table, code and diagram block's number among the blocks of its kind in its doc,
    /// from 1 in reading order: what its caption and its label count by.
    pub numbers: BTreeMap<BlockId, usize>,
    /// Every doc in reading order: depth-first, siblings by their order.
    pub order: Vec<BlockId>,
    /// Every folder and doc in tree order: a folder, then what it holds, siblings by their order.
    tree: Vec<BlockId>,
    /// The block each decision's permalink and references lead to: the first block it governs
    /// in reading order.
    governs: BTreeMap<BlockId, BlockId>,
    /// The questions no decision answers, in reading order of their docs.
    pub open_questions: Vec<BlockId>,
    /// Those questions' anchors on `questions.html`.
    pub question_anchors: BTreeMap<BlockId, String>,
    /// The entries of `disagreements.html`, in reading order of their docs; none when the render
    /// has no such page (FW-SW2). Filled by [`Site::set_disagreements`].
    pub disagreements: Vec<DisagreementGroup>,
    /// Every term, in the glossary page's order: by group, then by name, case-insensitively.
    pub terms: Vec<BlockId>,
    /// The terms' anchors on `glossary.html`.
    pub term_anchors: BTreeMap<BlockId, String>,
    /// The glossary page's groups: the top-level folders in tree order, then the project's.
    pub term_groups: Vec<GlossaryGroup>,
    /// Whether a top-level doc named `glossary` (in any case) was left off the site: the
    /// generated glossary owns `glossary.html` (D-Y4884).
    pub glossary_doc_dropped: bool,
    /// The docs left off the site for that reason: their terms are still listed, unlinked.
    glossary_docs: Vec<BlockId>,
    /// The project's name: the label of the glossary's group of top-level docs.
    project: String,
    /// The first two siblings, in tree order, whose names match ignoring case (see
    /// [`Site::path_clash`]).
    path_clash: Option<PathClash>,
    /// The navigation's lists, computed once: every folder's children that have a page, in
    /// order, under `Some(folder)`; the top-level docs that have a page under `None`.
    nav_lists: BTreeMap<Option<BlockId>, Vec<&'a Block>>,
    /// The top-level folders that have a page, in order.
    top_folders: Vec<&'a Block>,
    /// Each folder's and doc's place in its navigation list (see `nav_lists`).
    nav_pos: BTreeMap<BlockId, usize>,
    /// The folder or doc whose page each page is (the first in tree order that has it).
    page_owner: BTreeMap<String, BlockId>,
    /// The docs the previous/next pager walks: `order` less the docs left out of navigation.
    pager_order: Vec<BlockId>,
    /// Each doc's place in `pager_order`.
    order_pos: BTreeMap<BlockId, usize>,
    /// The docs of a type with `nav: false`, and the folders holding only such docs and folders:
    /// they have pages, but no place in the sidebar, a folder's index, the home page's contents,
    /// the pager or the search index.
    nav_hidden: BTreeSet<BlockId>,
    /// Every label worked out so far, by block and reference depth: a label is read many times
    /// per render (links, hovers, crumbs, the navigation, the search, the twins).
    labels: RefCell<HashMap<(BlockId, usize), String>>,
    /// Every doc's blocks in reading order (see `blocks_of`), walked once.
    doc_blocks: BTreeMap<BlockId, Vec<&'a Block>>,
    /// Every summary's words worked out so far (see `summary_words`).
    summaries: RefCell<HashMap<(BlockId, usize, bool), String>>,
    /// Every block's place worked out so far (see `url_of`): a block is linked from its doc page,
    /// its permalink, the search and the collected pages, and each walk reads several maps.
    urls: RefCell<HashMap<BlockId, Option<Url>>>,
    /// Every block's doc worked out so far (see `doc_of`).
    /// The doc's id, not the block: a reference held in a cell would make `Site` invariant.
    docs: RefCell<HashMap<BlockId, Option<BlockId>>>,
    /// A named profile's words for a reference that leads nowhere ([`inline::UNAVAILABLE`]): every
    /// label, summary, description and search text reads it in place of its source wording.
    /// None for a source output.
    unavailable: Option<&'static str>,
    /// Test hook: how many times `doc_of` was asked.
    #[cfg(test)]
    doc_calls: std::cell::Cell<usize>,
}

impl<'a> Site<'a> {
    pub fn new(m: &'a Materialized, pack: &Pack, project: &str) -> Site<'a> {
        Site::with_unavailable(m, pack, project, None)
    }

    /// [`Site::new`]; a named profile's site passes `unavailable`, the words every reference that
    /// leads nowhere reads as, whatever its source wording (D-0VCQ5).
    pub fn with_unavailable(
        m: &'a Materialized,
        pack: &Pack,
        project: &str,
        unavailable: Option<&'static str>,
    ) -> Site<'a> {
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
        let (mut nav_lists, mut top_folders, mut nav_pos) =
            (BTreeMap::<_, Vec<&Block>>::new(), vec![], BTreeMap::new());
        let mut nav_hidden: BTreeSet<BlockId> = order
            .iter()
            .filter(|id| {
                m.get(id)
                    .and_then(|b| b.field_str("doc_type"))
                    .and_then(|t| pack.doc_types.get(t))
                    .is_some_and(|t| !t.nav)
            })
            .cloned()
            .collect();
        // a folder whose every doc and folder is left out is left out too (an empty folder
        // stays); children come after their folder in tree order, so walk it backwards
        for id in tree_order.iter().rev() {
            if m.get(id).is_none_or(|b| b.block_type != tree::FOLDER) {
                continue;
            }
            let mut kids = m
                .children(Some(id))
                .into_iter()
                .filter(|c| tree::is_tree_type(&c.block_type) && paths.contains_key(&c.id))
                .peekable();
            if kids.peek().is_some() && kids.all(|c| nav_hidden.contains(&c.id)) {
                nav_hidden.insert(id.clone());
            }
        }
        for id in &tree_order {
            let Some(b) = m.get(id) else { continue };
            if !tree::is_tree_type(&b.block_type) || nav_hidden.contains(id) {
                continue;
            }
            if b.parent.is_none() && b.block_type == tree::FOLDER {
                top_folders.push(b);
                continue;
            }
            let list = nav_lists.entry(b.parent.clone()).or_default();
            nav_pos.insert(b.id.clone(), list.len());
            list.push(b);
        }
        let mut site = Site {
            m,
            paths,
            anchors: BTreeMap::new(),
            numbers: BTreeMap::new(),
            order,
            tree: tree_order,
            governs: BTreeMap::new(),
            open_questions: vec![],
            question_anchors: BTreeMap::new(),
            disagreements: vec![],
            terms: vec![],
            term_anchors: BTreeMap::new(),
            term_groups: vec![],
            glossary_doc_dropped: !dropped.is_empty(),
            glossary_docs: dropped,
            path_clash,
            project: project.to_string(),
            nav_lists,
            top_folders,
            nav_pos,
            page_owner: BTreeMap::new(),
            pager_order: vec![],
            order_pos: BTreeMap::new(),
            nav_hidden,
            labels: RefCell::default(),
            doc_blocks: BTreeMap::new(),
            summaries: RefCell::default(),
            urls: RefCell::default(),
            docs: RefCell::default(),
            unavailable,
            #[cfg(test)]
            doc_calls: Default::default(),
        };
        site.doc_blocks = site
            .order
            .iter()
            .map(|d| (d.clone(), Self::walk_blocks(m, d)))
            .collect();
        let mut page_owner = BTreeMap::new();
        for id in &site.tree {
            if let Some(p) = site.page(id) {
                page_owner.entry(p).or_insert_with(|| id.clone());
            }
        }
        site.page_owner = page_owner;
        site.pager_order = site
            .order
            .iter()
            .filter(|d| !site.nav_hidden.contains(*d))
            .cloned()
            .collect();
        let mut order_pos = BTreeMap::new();
        for (i, d) in site.pager_order.iter().enumerate() {
            order_pos.entry(d.clone()).or_insert(i);
        }
        site.order_pos = order_pos;
        site.compute_numbers();
        site.compute_anchors();
        site.compute_governed();
        site.compute_collected();
        site
    }

    /// Every block under `doc`, in reading order.
    fn blocks_of(&self, doc: &BlockId) -> &[&'a Block] {
        self.doc_blocks.get(doc).map_or(&[], Vec::as_slice)
    }

    /// Every block under `doc`, in reading order, walked from the store.
    fn walk_blocks(m: &'a Materialized, doc: &BlockId) -> Vec<&'a Block> {
        let mut out = vec![];
        let mut stack: Vec<&Block> = m.children(Some(doc)).into_iter().rev().collect();
        while let Some(b) = stack.pop() {
            out.push(b);
            stack.extend(m.children(Some(&b.id)).into_iter().rev());
        }
        out
    }

    fn compute_numbers(&mut self) {
        let mut numbers = BTreeMap::new();
        for doc in &self.order {
            let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
            for b in self.blocks_of(doc) {
                if NUMBERED.contains(&b.block_type.as_str()) {
                    let n = counts.entry(b.block_type.as_str()).or_default();
                    *n += 1;
                    numbers.insert(b.id.clone(), *n);
                }
            }
        }
        self.numbers = numbers;
    }

    fn compute_anchors(&mut self) {
        let mut anchors = BTreeMap::new();
        for doc in &self.order {
            let mut taken = reserved_ids();
            for b in self.blocks_of(doc) {
                // a decision has no place in the text: it is linked through what it governs; a
                // rationale, a finding, a source and a publication profile are off the page
                if b.block_type == decision::DECISION || is_off_site(b) || is_profile(b) {
                    continue;
                }
                anchors.insert(b.id.clone(), unique(self.anchor_text(b), &mut taken));
            }
        }
        self.anchors = anchors;
    }

    fn compute_governed(&mut self) {
        let mut governs: BTreeMap<BlockId, BlockId> = BTreeMap::new();
        // where each block is read: docs in reading order, then each doc's blocks in order
        let mut reading: BTreeMap<&BlockId, usize> = BTreeMap::new();
        for doc in &self.order {
            reading.insert(doc, reading.len());
            for b in self.blocks_of(doc) {
                reading.insert(&b.id, reading.len());
            }
        }
        // a decision governs what it affects (the target read first; a target on no page comes
        // last, in link order), else what names it as `decided_by`, else its parent section
        for b in self.m.iter() {
            if b.block_type != decision::DECISION {
                continue;
            }
            let first = b
                .links
                .get(decision::AFFECTS)
                .into_iter()
                .flatten()
                .enumerate()
                .min_by_key(|(i, l)| (reading.get(&l.target).copied().unwrap_or(usize::MAX), *i));
            if let Some((_, l)) = first {
                governs.insert(b.id.clone(), l.target.clone());
            }
        }
        for b in self.m.iter() {
            for l in b.links.get(decision::DECIDED_BY).into_iter().flatten() {
                if self
                    .m
                    .get(&l.target)
                    .is_some_and(|t| t.block_type == decision::DECISION)
                {
                    governs.entry(l.target.clone()).or_insert(b.id.clone());
                }
            }
        }
        for b in self.m.iter() {
            if b.block_type == decision::DECISION
                && !governs.contains_key(&b.id)
                && let Some(p) = &b.parent
            {
                governs.insert(b.id.clone(), p.clone());
            }
        }
        self.governs = governs;
    }

    fn compute_collected(&mut self) {
        let answered: BTreeSet<&BlockId> = self
            .m
            .iter()
            .flat_map(|b| b.links.get(decision::ANSWERS).into_iter().flatten())
            .map(|l| &l.target)
            .collect();
        let (mut open, mut question_anchors) = (vec![], BTreeMap::new());
        let mut taken = reserved_ids();
        // each term, with the top-level folder its doc sits under (none for a top-level doc)
        let mut terms: Vec<(&Block, Option<BlockId>)> = vec![];
        for doc in &self.order {
            let top = self
                .chain(doc)
                .into_iter()
                .next()
                .filter(|t| self.m.get(t).is_some_and(|b| b.block_type == tree::FOLDER));
            for b in self.blocks_of(doc) {
                if b.block_type == decision::QUESTION && !answered.contains(&b.id) {
                    open.push(b.id.clone());
                    question_anchors.insert(b.id.clone(), unique(self.anchor_text(b), &mut taken));
                }
                if b.block_type == "term" {
                    terms.push((*b, top.clone()));
                }
            }
        }
        // a top-level doc that gave way to the generated glossary has no page, but its terms are
        // still listed, unlinked, in the project's group (ruling task-12)
        let hidden: Vec<&'a Block> = self
            .glossary_docs
            .iter()
            .flat_map(|d| Self::walk_blocks(self.m, d))
            .filter(|b| b.block_type == "term")
            .collect();
        terms.extend(hidden.into_iter().map(|b| (b, None)));
        terms.sort_by_cached_key(|(b, _)| (self.label(b).to_lowercase(), b.id.clone()));
        let (mut term_anchors, mut taken) = (BTreeMap::new(), reserved_ids());
        for (b, _) in &terms {
            term_anchors.insert(b.id.clone(), unique(self.anchor_text(b), &mut taken));
        }
        // the groups: the top-level folders in tree order (the navigation's order), then the
        // top-level docs under the project's name; a group's anchor continues the terms' set
        let mut by_top: BTreeMap<Option<BlockId>, Vec<BlockId>> = BTreeMap::new();
        for (b, top) in terms {
            by_top.entry(top).or_default().push(b.id.clone());
        }
        let mut groups = vec![];
        let tops = self
            .tree
            .iter()
            .filter_map(|id| self.m.get(id))
            .filter(|b| b.parent.is_none() && b.block_type == tree::FOLDER)
            .map(|b| (Some(b.id.clone()), Some(b)))
            .chain([(None, None)]);
        for (key, folder) in tops {
            let Some(terms) = by_top.remove(&key) else {
                continue;
            };
            let label = match folder {
                Some(f) => self.label(f),
                None => self.project.clone(),
            };
            let anchor = unique(label.clone(), &mut taken);
            groups.push(GlossaryGroup {
                label,
                anchor,
                terms,
            });
        }
        // a top-level doc named `questions` already has that page: it keeps it, and the
        // collected page is not written (nor linked); `glossary.html` is always the generated
        // glossary (D-Y4884)
        if !self.page_taken("questions.html") {
            self.open_questions = open;
            self.question_anchors = question_anchors;
        }
        self.terms = groups.iter().flat_map(|g| g.terms.clone()).collect();
        self.term_anchors = term_anchors;
        self.term_groups = groups;
    }

    /// Sets the entries of the disagreements page: only blocks that have a place on the site, in
    /// reading order of their docs, except that the lead of an entry (the "this page" of its
    /// sentence) comes first. Entries with the same sentence and the same pages are one group
    /// (ls11). A top-level doc named `disagreements` keeps its page, and the collected page is
    /// not written (nor linked), as for `questions.html`.
    pub fn set_disagreements(&mut self, list: Vec<crate::disagree::Disagreement>) {
        if self.page_taken("disagreements.html") {
            return;
        }
        // where a block's doc comes in reading order, and which doc it is; each block's place is
        // read once per entry (a sort by a computed key would walk its doc chain at every
        // comparison)
        let at = |id: &BlockId| {
            let doc = self.doc_of(id);
            let pos = doc
                .and_then(|d| self.order_pos.get(&d.id).copied())
                .unwrap_or(usize::MAX);
            (pos, doc.map(|d| d.id.clone()))
        };
        // an entry, the place of its first block, and its pages (the docs of its blocks, each
        // once, in the entry's order; a block with no doc stands for its own page)
        struct Row {
            entry: crate::disagree::Disagreement,
            place: usize,
            pages: Vec<BlockId>,
        }
        let mut rows: Vec<Row> = vec![];
        let mut seen: BTreeSet<crate::disagree::Disagreement> = BTreeSet::new();
        for d in list {
            let lead = d.lead;
            // the page the sentence calls "this page" must be linkable, else the entry would
            // point the reader at the wrong one
            if lead.as_ref().is_some_and(|l| self.url_of(l).is_none()) {
                continue;
            }
            // distinct ids, so the keys are distinct and an unstable sort gives one order
            let mut keyed: Vec<(usize, BlockId, Option<BlockId>)> =
                crate::disagree::distinct(d.blocks)
                    .into_iter()
                    .filter(|b| self.url_of(b).is_some())
                    .map(|b| {
                        let (pos, doc) = at(&b);
                        (pos, b, doc)
                    })
                    .collect();
            keyed.sort_unstable_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
            let mut docs: BTreeMap<BlockId, Option<BlockId>> = BTreeMap::new();
            let mut blocks: Vec<BlockId> = Vec::with_capacity(keyed.len());
            for (_, b, doc) in keyed {
                docs.insert(b.clone(), doc);
                blocks.push(b);
            }
            if let Some(l) = &lead {
                blocks.retain(|b| b != l);
                blocks.insert(0, l.clone());
            }
            if blocks.is_empty() {
                continue;
            }
            let place = at(&blocks[0]).0;
            let mut page_set = BTreeSet::new();
            let pages: Vec<BlockId> = blocks
                .iter()
                .map(|b| docs.get(b).cloned().flatten().unwrap_or_else(|| b.clone()))
                .filter(|p| page_set.insert(p.clone()))
                .collect();
            let entry = crate::disagree::Disagreement {
                sentence: d.sentence,
                blocks,
                lead,
            };
            if seen.insert(entry.clone()) {
                rows.push(Row {
                    entry,
                    place,
                    pages,
                });
            }
        }
        // a total order: the whole block list is the last key
        rows.sort_by(|a, b| {
            (
                a.place,
                &a.entry.blocks[0],
                a.entry.sentence,
                &a.entry.blocks,
            )
                .cmp(&(
                    b.place,
                    &b.entry.blocks[0],
                    b.entry.sentence,
                    &b.entry.blocks,
                ))
        });
        // one group per sentence and pages, in the place of its first entry
        let mut groups: Vec<DisagreementGroup> = vec![];
        let mut by_key: BTreeMap<(&'static str, Vec<BlockId>), usize> = BTreeMap::new();
        for Row { entry, pages, .. } in rows {
            match by_key.entry((entry.sentence, pages)) {
                std::collections::btree_map::Entry::Occupied(o) => {
                    groups[*o.get()].repeats += 1;
                }
                std::collections::btree_map::Entry::Vacant(v) => {
                    v.insert(groups.len());
                    groups.push(DisagreementGroup {
                        sentence: entry.sentence,
                        blocks: entry.blocks,
                        repeats: 1,
                    });
                }
            }
        }
        // a group of more than one entry has one block per page: the first entry's first block
        // on that page
        let page_of = |id: &BlockId| at(id).1.unwrap_or_else(|| id.clone());
        for g in groups.iter_mut().filter(|g| g.repeats > 1) {
            let mut page_set = BTreeSet::new();
            g.blocks.retain(|b| page_set.insert(page_of(b)));
        }
        self.disagreements = groups;
    }

    /// Whether a doc or folder already has the page at `page`.
    pub(crate) fn page_taken(&self, page: &str) -> bool {
        self.page_owner.contains_key(page)
    }

    /// The refusal of a build that writes the site's pages as files, when two siblings' names
    /// match ignoring case (see [`PathClash`]); `verb` is the command to run again.
    pub fn path_clash(&self, verb: &str) -> Option<RdyError> {
        self.path_clash.as_ref().map(|c| c.error(verb))
    }

    /// The page of a folder or doc.
    pub fn page(&self, id: &BlockId) -> Option<String> {
        let p = self.paths.get(id)?;
        Some(match self.m.get(id)?.block_type == tree::FOLDER {
            true => format!("{p}/index.html"),
            false => format!("{p}.html"),
        })
    }

    /// The folders and docs with a page that the navigation lists under `parent` (a folder), or
    /// the top-level docs with a page when `parent` is none.
    pub fn nav_list(&self, parent: Option<&BlockId>) -> &[&'a Block] {
        self.nav_lists
            .get(&parent.cloned())
            .map_or(&[], |l| l.as_slice())
    }

    /// The top-level folders that have a page, in order.
    pub fn top_folders(&self) -> &[&'a Block] {
        &self.top_folders
    }

    /// The place of a folder or doc in its navigation list.
    pub fn nav_pos(&self, id: &BlockId) -> Option<usize> {
        self.nav_pos.get(id).copied()
    }

    /// Every folder and doc that has a page, in tree order.
    pub fn tree_pages(&self) -> impl Iterator<Item = &BlockId> {
        self.tree.iter()
    }

    /// The doc that holds `id` (itself for a doc), as `Materialized::doc_of` finds it, each
    /// block's walked once per render.
    pub fn doc_of(&self, id: &BlockId) -> Option<&'a Block> {
        #[cfg(test)]
        self.doc_calls.set(self.doc_calls.get() + 1);
        if let Some(d) = self.docs.borrow().get(id) {
            return d.as_ref().and_then(|d| self.m.get(d));
        }
        let doc = self.m.doc_of(id);
        self.docs
            .borrow_mut()
            .insert(id.clone(), doc.map(|d| d.id.clone()));
        doc
    }

    /// Where `id` lives, for any block: a tree block on its page, a block inside a doc on the
    /// doc's page at its anchor, a decision where the first block it governs lives, a term at its
    /// place in its doc, an open question on the questions page.
    pub fn url_of(&self, id: &BlockId) -> Option<Url> {
        if let Some(u) = self.urls.borrow().get(id) {
            return u.clone();
        }
        let url = self.url_of_bounded(id, &mut BTreeSet::new());
        self.urls.borrow_mut().insert(id.clone(), url.clone());
        url
    }

    fn url_of_bounded(&self, id: &BlockId, seen: &mut BTreeSet<BlockId>) -> Option<Url> {
        if !seen.insert(id.clone()) {
            return None;
        }
        let b = self.m.get(id)?;
        if tree::is_tree_type(&b.block_type) {
            return Some(Url {
                page: self.page(id)?,
                anchor: None,
            });
        }
        if b.block_type == decision::DECISION {
            return self.url_of_bounded(self.governs.get(id)?, seen);
        }
        if let Some(a) = self.question_anchors.get(id) {
            return Some(Url {
                page: "questions.html".into(),
                anchor: Some(a.clone()),
            });
        }
        let doc = self.doc_of(id)?;
        if is_off_site(b) || is_profile(b) {
            return Some(Url {
                page: self.page(&doc.id)?,
                anchor: None,
            });
        }
        Some(Url {
            page: self.page(&doc.id)?,
            anchor: Some(self.anchors.get(id)?.clone()),
        })
    }

    /// The block a link to `id` lands on: `id` itself, or for a decision the block it governs
    /// (followed through a decision that governs a decision). What a link's hover describes.
    pub fn destination(&self, id: &BlockId) -> Option<&'a Block> {
        let mut seen = BTreeSet::new();
        let mut at = id;
        loop {
            if !seen.insert(at.clone()) {
                return None;
            }
            let b = self.m.get(at)?;
            // a publication profile is off the site: a link to it lands on its doc
            if is_profile(b) {
                return self.doc_of(&b.id);
            }
            if b.block_type != decision::DECISION {
                return Some(b);
            }
            at = self.governs.get(at)?;
        }
    }

    /// The permalink page of a block.
    pub fn go_page(&self, id: &BlockId) -> String {
        format!("go/{id}.html")
    }

    /// What a reader calls `b`: its title, else its `name` field, else (for a doc or folder) its
    /// tree name worded as a title, else (for code) its language and number (`Python code 3`),
    /// else (for a diagram) its alt text or number (`Diagram 2`), else the start of its text. A
    /// reference in the title, name or alt reads as its target's label. Never an id, and never
    /// code or diagram source.
    pub fn label(&self, b: &Block) -> String {
        self.label_at(b, 0)
    }

    fn label_at(&self, b: &Block, depth: usize) -> String {
        let key = (b.id.clone(), depth);
        if let Some(l) = self.labels.borrow().get(&key) {
            return l.clone();
        }
        let label = self.label_uncached(b, depth);
        self.labels.borrow_mut().insert(key, label.clone());
        label
    }

    fn label_uncached(&self, b: &Block, depth: usize) -> String {
        // a publication profile, removed or not, has no words on the site: it is called by its
        // doc, where a link to it leads
        if is_profile(b) {
            return self
                .doc_of(&b.id)
                .map(|d| self.label_at(d, depth))
                .unwrap_or_default();
        }
        // a removed block is called by the notice, never by its id path or old words (D-3M81W)
        if b.is_redacted() {
            return CONTENT_REMOVED.to_string();
        }
        if let Some(v) = entity::value_text(b) {
            return self.as_written_at(&v, depth);
        }
        let tree = tree::is_tree_type(&b.block_type);
        let said = b
            .field_str("title")
            .filter(|t| !t.is_empty())
            .or_else(|| b.field_str("name").filter(|t| !t.is_empty() && !tree));
        let label = match said {
            Some(t) => self.words_at(t, depth),
            None if tree => self
                .m
                .name_of(&b.id)
                .map(|n| name_as_title(&n, &b.id))
                .unwrap_or_default(),
            None if b.block_type == "code" => {
                let k = self.number(b);
                // for a named profile, a language that holds a reference reads as its words
                // exactly, never worded as a language's name
                let lang = b
                    .field_str("lang")
                    .map(|l| (l, self.as_written_at(l, depth)));
                match lang.filter(|(_, w)| !w.trim().is_empty()) {
                    Some((l, w)) if self.reads_ref_words(l) => format!("{} code {k}", w.trim()),
                    Some((_, w)) => format!("{} code {k}", crate::highlight::human_name(&w)),
                    None => format!("Code {k}"),
                }
            }
            None if b.block_type == "diagram" => {
                match b.field_str("alt").filter(|a| !a.trim().is_empty()) {
                    Some(a) => upper_first(&self.words_at(a, depth)),
                    None => format!("Diagram {}", self.number(b)),
                }
            }
            None => self.summary_at(b, depth, true),
        };
        if label.trim().is_empty() {
            "Untitled".to_string()
        } else {
            label
        }
    }

    /// `b`'s number among the blocks of its kind in its doc (see `numbers`); 1 when it has none.
    pub fn number(&self, b: &Block) -> usize {
        self.numbers.get(&b.id).copied().unwrap_or(1)
    }

    /// A field's text as a reader reads it, as a label ([`Form::Label`]): a code span's text, a
    /// reference as its display text, else its target's current key when the author typed a key,
    /// else the target's label; an unresolved one as the unresolved marker's words, `\{{` as
    /// `{{`. Not escaped.
    pub fn words(&self, text: &str) -> String {
        self.words_at(text, 0)
    }

    fn words_at(&self, text: &str, depth: usize) -> String {
        self.plain(text, RefMode::Strict, Form::Label, depth, &|_| {
            crate::html::UNRESOLVED.to_string()
        })
    }

    /// A field read as written (a value and its unit, a code language, a profile's title):
    /// unchanged for a source output; for a named profile, each reference in it reads as
    /// [`Site::ref_text`] says (the display words, the target's key or label, else the profile's
    /// unavailable words), never its token, and nothing else changes but `\{{` as `{{`.
    pub(crate) fn as_written(&self, text: &str) -> String {
        self.as_written_at(text, 0)
    }

    /// Whether a field read as written ([`Site::as_written`]) holds a reference that a named
    /// profile reads as words. Always false for a source output.
    fn reads_ref_words(&self, text: &str) -> bool {
        self.unavailable.is_some() && !rdy_model::ref_spans(text, RefMode::Strict).is_empty()
    }

    fn as_written_at(&self, text: &str, depth: usize) -> String {
        match self.unavailable {
            None => text.to_string(),
            Some(_) => inline::source_plain(text, RefMode::Strict, &|span| {
                self.ref_text(span, depth, &|_| String::new())
            }),
        }
    }

    /// `text` as plain words in `form` ([`inline::plain_refs_every`]): a reference reads as
    /// `ref_text` says (the notice for a removed target, else its display text, else the target's
    /// key when typed as one and the target has one, else its label), or as `unresolved(token)`
    /// when it leads nowhere.
    pub fn plain(
        &self,
        text: &str,
        mode: RefMode,
        form: Form,
        depth: usize,
        unresolved: &dyn Fn(&str) -> String,
    ) -> String {
        inline::plain_refs_every(text, mode, form, &|span| {
            self.ref_text(span, depth, unresolved)
        })
    }

    /// As [`Site::plain`], but each code span of `text` stands between the code marks
    /// ([`inline::plain_refs_coded`]): a word diff keeps a code span code (the task-6 ruling).
    /// A reference reads as plain words, as on the page.
    pub(crate) fn plain_coded(
        &self,
        text: &str,
        mode: RefMode,
        form: Form,
        unresolved: &dyn Fn(&str) -> String,
    ) -> String {
        inline::plain_refs_coded(text, mode, form, &|span| self.ref_text(span, 0, unresolved))
    }

    /// What a reference reads as in plain words: the notice when its target is removed, whatever
    /// display text the author gave (D-3M81W); else its display text; else [`Site::ref_words`].
    fn ref_text(
        &self,
        span: &rdy_model::RefSpan,
        depth: usize,
        unresolved: &dyn Fn(&str) -> String,
    ) -> String {
        let target = BlockId::parse(&span.token)
            .ok()
            .and_then(|id| self.m.get(&id));
        if target.is_some_and(|t| t.is_redacted()) {
            return CONTENT_REMOVED.to_string();
        }
        match &span.display {
            Some(d) => d.clone(),
            None => self.ref_words(span, depth, unresolved),
        }
    }

    /// The words a reference with no display text reads as: its target's current key when typed
    /// as one and the target has one, else the target's label, or `unresolved(token)` when it
    /// leads nowhere.
    fn ref_words(
        &self,
        span: &rdy_model::RefSpan,
        depth: usize,
        unresolved: &dyn Fn(&str) -> String,
    ) -> String {
        match BlockId::parse(&span.token)
            .ok()
            .and_then(|id| self.m.get(&id))
        {
            Some(_) if depth >= LABEL_DEPTH => "…".to_string(),
            Some(t) => match typed_key(t).filter(|k| span.key_typed && !k.is_empty()) {
                Some(k) => k.to_string(),
                None => self.label_at(t, depth + 1),
            },
            None => match self.unavailable {
                Some(words) => words.to_string(),
                None => unresolved(&span.token),
            },
        }
    }

    /// `b`'s one-line summary as a reader reads it: its statement, prompt, name or title, else
    /// its body, else its alt text or caption, as plain words (the form that fits the field),
    /// whitespace collapsed and cut to 80 characters. A reference reads as its display text or
    /// its target's label, one that leads nowhere keeps its id as written, and any id left in the
    /// words is read as `…`.
    pub fn summary(&self, b: &Block) -> String {
        self.summary_at(b, 0, true)
    }

    /// The summary's words, uncut. `keep_token` leaves an unresolved reference as its token
    /// (then every id-shaped word is `…`); otherwise it reads as the unresolved marker's words.
    pub(crate) fn summary_words(&self, b: &Block, depth: usize, keep_token: bool) -> String {
        let key = (b.id.clone(), depth, keep_token);
        if let Some(w) = self.summaries.borrow().get(&key) {
            return w.clone();
        }
        let words = self.summary_words_uncached(b, depth, keep_token);
        self.summaries.borrow_mut().insert(key, words.clone());
        words
    }

    fn summary_words_uncached(&self, b: &Block, depth: usize, keep_token: bool) -> String {
        // a publication profile, removed or not, has no words on the site: a trail's reference
        // to one reads as its doc, where the reference leads (as its label does)
        if is_profile(b) {
            return self
                .doc_of(&b.id)
                .map(|d| self.summary_words(d, depth, keep_token))
                .unwrap_or_default();
        }
        if b.is_redacted() {
            return CONTENT_REMOVED.to_string();
        }
        let unresolved_of = |token: &str| match keep_token {
            true => token.to_string(),
            false => crate::html::UNRESOLVED.to_string(),
        };
        let read = |text: &str, mode: RefMode, form: Form| {
            let words = self.plain(text, mode, form, depth, &unresolved_of);
            words.split_whitespace().collect::<Vec<_>>().join(" ")
        };
        let said: &[(&str, Form)] = if tree::is_tree_type(&b.block_type) {
            &[("title", Form::Label), ("name", Form::Label)]
        } else {
            &[
                ("statement", Form::Spans),
                ("prompt", Form::Spans),
                ("name", Form::Label),
                ("title", Form::Label),
            ]
        };
        if let Some((f, form)) = said
            .iter()
            .find(|(f, _)| b.field_str(f).is_some_and(|s| !s.is_empty()))
        {
            return read(b.field_str(f).unwrap_or(""), RefMode::Strict, *form);
        }
        if !b.body.is_empty() {
            // source is not Markdown: its words are its text, each reference in it (none in
            // code) read as on the drawn diagram, so no id is quoted
            let text = match b.block_type.as_str() {
                "code" | "diagram" | "figure" => {
                    let mode = RefMode::for_body(&b.block_type);
                    let words = inline::source_plain(&b.body, mode, &|span| {
                        self.ref_text(span, depth, &unresolved_of)
                    });
                    words.split_whitespace().collect::<Vec<_>>().join(" ")
                }
                _ => read(&b.body, RefMode::for_body(&b.block_type), Form::Body),
            };
            if !text.is_empty() {
                return text;
            }
        }
        ["alt", "caption"]
            .iter()
            .find_map(|f| b.field_str(f).filter(|s| !s.trim().is_empty()))
            .map(|s| read(s, RefMode::Strict, Form::Label))
            .unwrap_or_default()
    }

    /// A folder's description as plain words on one line: it is running text (spans), a reference
    /// with no display text reads as its target's label, one that leads nowhere as nothing.
    pub(crate) fn description_words(&self, text: &str) -> String {
        let words = self.plain(text, RefMode::Strict, Form::Spans, 0, &|_| String::new());
        words.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn summary_at(&self, b: &Block, depth: usize, keep_token: bool) -> String {
        let words = self.summary_words(b, depth, keep_token);
        let words = ID_INSIDE.replace_all(&words, "…");
        words.chars().take(80).collect()
    }

    /// The text an anchor is made from: a section's title, the name of what a block defines
    /// (their references read as labels), else the first six words of its one-line summary, a
    /// dangling reference there read as the unresolved marker's words, so no id reaches the URL.
    fn anchor_text(&self, b: &Block) -> String {
        // code and diagram source is never a reader's word: their label is
        if matches!(b.block_type.as_str(), "code" | "diagram") {
            return self.label(b);
        }
        let named = match b.block_type.as_str() {
            "section" => b.field_str("title"),
            "term" | "component" | "actor" | "interface" | "flow" | "value" | "entity" => {
                b.field_str("name")
            }
            _ => None,
        };
        match named.filter(|t| !t.trim().is_empty()) {
            Some(t) => self.words(t),
            None => self
                .summary_words(b, 0, false)
                .chars()
                .take(80)
                .collect::<String>()
                .split_whitespace()
                .take(6)
                .collect::<Vec<_>>()
                .join(" "),
        }
    }

    /// The folders and doc from the top level down to `id`, top first.
    pub fn chain(&self, id: &BlockId) -> Vec<BlockId> {
        let mut out = vec![];
        let mut cur = Some(id.clone());
        while let Some(c) = cur {
            if out.len() > self.m.len() {
                break;
            }
            let Some(b) = self.m.get(&c) else { break };
            if tree::is_tree_type(&b.block_type) {
                out.push(c.clone());
            }
            cur = b.parent.clone();
        }
        out.reverse();
        out
    }

    /// Whether `id` has a place in the site's navigation: false for a doc of a type with
    /// `nav: false`, and for a folder holding only such docs (and such folders), which are
    /// rendered but listed nowhere.
    pub fn in_nav(&self, id: &BlockId) -> bool {
        !self.nav_hidden.contains(id)
    }

    /// The docs before and after `doc` in reading order, those left out of navigation skipped
    /// (a doc left out has neither).
    pub fn prev_next(&self, doc: &BlockId) -> (Option<BlockId>, Option<BlockId>) {
        let Some(&i) = self.order_pos.get(doc) else {
            return (None, None);
        };
        (
            i.checked_sub(1).map(|p| self.pager_order[p].clone()),
            self.pager_order.get(i + 1).cloned(),
        )
    }
}

/// `s` with its first letter upper-cased.
fn upper_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A tree name worded as a title: `-` becomes a space, the first letter is upper-cased, and the
/// name `untitled-<id>` that a doc without a title gets (the one `id` gives) reads as `Untitled`.
pub(crate) fn name_as_title(name: &str, id: &BlockId) -> String {
    if name == rdy_model::tree::default_name(None, id) {
        return "Untitled".to_string();
    }
    let words = name.replace('-', " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Element ids the page chrome owns (the `<main>` skip-link target, the search box and its
/// results, the sidebar and its menu button and scrim, the contents list, the version
/// switcher, the review bar's two buttons). A block anchor never equals one, so each per-page
/// `taken` set starts with them. The buttons are reserved on every scope: the bar shows only on a
/// review page, but a document's anchors must not move between a review render and a canon one.
const RESERVED_IDS: &[&str] = &[
    "main",
    "search",
    "results",
    "sidebar",
    "menu",
    "scrim",
    "contents",
    "versions",
    "show-changes",
    "show-diff",
];

/// A fresh per-page `taken` set, holding [`RESERVED_IDS`].
pub(crate) fn reserved_ids() -> BTreeSet<String> {
    RESERVED_IDS.iter().map(|s| s.to_string()).collect()
}

/// `text` as an anchor not in `taken`, which then holds it: slugged, cut to [`ANCHOR_MAX`],
/// `part` when nothing is left, `-x` after anything shaped like an id, `-2`, `-3`... on a clash.
pub(crate) fn unique(text: String, taken: &mut BTreeSet<String>) -> String {
    let slug = tree::slug(&text);
    let cut: String = slug.chars().take(ANCHOR_MAX).collect();
    let mut base = cut.trim_end_matches(['-', '.']).to_string();
    if base.is_empty() {
        base = "part".to_string();
    }
    if ID_SHAPED.is_match(&base) {
        base.push_str("-x");
    }
    let mut candidate = base.clone();
    let mut n = 2;
    while !taken.insert(candidate.clone()) {
        candidate = format!("{base}-{n}");
        n += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Final review M12 (minors item 32): an entry whose "this page" block has no page is
    /// dropped, since its sentence would point the reader at the wrong page; an entry with no lead
    /// keeps the blocks that have a page.
    #[test]
    fn an_entry_whose_lead_has_no_page_is_dropped() {
        use crate::disagree::Disagreement;
        let dir = tempfile::tempdir().expect("tempdir");
        rdy_engine::testkit::generate_tree(dir.path(), 1, 1, 2);
        let r = rdy_engine::Rdy::open(dir.path()).expect("open");
        let st = r.read();
        let m = st
            .materialized(&rdy_engine::Scope::View("work".into()))
            .expect("the view");
        let claim = m
            .iter()
            .find(|b| b.block_type == "claim")
            .expect("a claim")
            .id
            .clone();
        let ghost = BlockId::parse("c-ZZZZZ").expect("an id");
        assert!(m.get(&ghost).is_none());
        let mut site = Site::new(&m, st.pack(), "Project");
        site.set_disagreements(vec![
            Disagreement {
                sentence: "This page still points to something that was retired.",
                blocks: vec![claim.clone(), ghost.clone()],
                lead: Some(ghost.clone()),
            },
            Disagreement {
                sentence: "These pages define the same thing twice.",
                blocks: vec![ghost.clone(), claim.clone()],
                lead: None,
            },
            Disagreement {
                sentence: "These pages define the same thing twice.",
                blocks: vec![claim.clone()],
                lead: None,
            },
        ]);
        assert_eq!(
            site.disagreements,
            vec![DisagreementGroup {
                sentence: "These pages define the same thing twice.",
                blocks: vec![claim],
                repeats: 1,
            }]
        );
    }

    /// render_50k fix: the entries are put in reading order with each block's place read once
    /// per entry (and its link checked once), never at every comparison of a sort. A wide entry
    /// (a term defined in every doc) has thousands of blocks.
    #[test]
    fn set_disagreements_reads_each_blocks_doc_once() {
        use crate::disagree::Disagreement;
        let dir = tempfile::tempdir().expect("tempdir");
        rdy_engine::testkit::generate_tree(dir.path(), 4, 4, 8);
        let r = rdy_engine::Rdy::open(dir.path()).expect("open");
        let st = r.read();
        let m = st
            .materialized(&rdy_engine::Scope::View("work".into()))
            .expect("the view");
        // the claims, last doc first, so the sort has work to do
        let mut claims: Vec<BlockId> = m
            .iter()
            .filter(|b| b.block_type == "claim")
            .map(|b| b.id.clone())
            .collect();
        claims.reverse();
        assert!(claims.len() >= 100, "guard: a wide entry");
        let mut site = Site::new(&m, st.pack(), "Project");
        let before = site.doc_calls.get();
        site.set_disagreements(vec![Disagreement {
            sentence: "These pages define the same thing twice.",
            blocks: claims.clone(),
            lead: None,
        }]);
        let calls = site.doc_calls.get() - before;
        // one for the place, one inside the link check, one for the entry's first block
        assert!(
            calls <= 2 * claims.len() + 1,
            "{calls} doc reads for {} blocks",
            claims.len()
        );
        let at = |id: &BlockId| {
            m.doc_of(id)
                .and_then(|d| site.order_pos.get(&d.id).copied())
                .unwrap_or(usize::MAX)
        };
        let mut want = claims;
        want.sort_by_key(|b| (at(b), b.clone()));
        assert_eq!(site.disagreements[0].blocks, want, "reading order");
    }

    /// The claims of a generated tree of `folders` top-level folders of one doc with one claim
    /// each, in tree order, with the materialized view they come from.
    fn one_claim_per_doc(dir: &std::path::Path, folders: usize) -> rdy_engine::Rdy {
        rdy_engine::testkit::generate_tree(dir, folders, 1, 1);
        rdy_engine::Rdy::open(dir).expect("open")
    }

    /// Final review M6 (b): a group of one entry over more pages than show also keeps every
    /// page (the page folds the rest behind Show all), with its one entry counted once.
    #[test]
    fn ls11_a_lone_entry_over_many_pages_keeps_them_all() {
        use crate::disagree::Disagreement;
        let dir = tempfile::tempdir().expect("tempdir");
        let r = one_claim_per_doc(dir.path(), 7);
        let st = r.read();
        let m = st
            .materialized(&rdy_engine::Scope::View("work".into()))
            .expect("the view");
        let claims: Vec<BlockId> = m
            .iter()
            .filter(|b| b.block_type == "claim")
            .map(|b| b.id.clone())
            .collect();
        assert_eq!(claims.len(), 7, "guard: one claim on each of seven docs");
        let mut site = Site::new(&m, st.pack(), "Project");
        site.set_disagreements(vec![Disagreement {
            sentence: "These pages define the same thing twice.",
            blocks: claims.clone(),
            lead: None,
        }]);
        assert_eq!(site.disagreements.len(), 1);
        assert_eq!(site.disagreements[0].repeats, 1);
        assert_eq!(site.disagreements[0].blocks.len(), 7, "every page stays");
    }

    /// Final review M6 (c): two entries with the same sentence over different pages are two
    /// groups, each of one entry.
    #[test]
    fn ls11_the_same_sentence_over_other_pages_is_another_group() {
        use crate::disagree::Disagreement;
        let dir = tempfile::tempdir().expect("tempdir");
        let r = one_claim_per_doc(dir.path(), 4);
        let st = r.read();
        let m = st
            .materialized(&rdy_engine::Scope::View("work".into()))
            .expect("the view");
        let claims: Vec<BlockId> = m
            .iter()
            .filter(|b| b.block_type == "claim")
            .map(|b| b.id.clone())
            .collect();
        assert_eq!(claims.len(), 4, "guard: one claim on each of four docs");
        let mut site = Site::new(&m, st.pack(), "Project");
        let same = "These pages define the same thing twice.";
        site.set_disagreements(vec![
            Disagreement {
                sentence: same,
                blocks: claims[..2].to_vec(),
                lead: None,
            },
            Disagreement {
                sentence: same,
                blocks: claims[2..].to_vec(),
                lead: None,
            },
        ]);
        let groups: Vec<(usize, usize)> = site
            .disagreements
            .iter()
            .map(|g| (g.repeats, g.blocks.len()))
            .collect();
        assert_eq!(groups, vec![(1, 2), (1, 2)], "{:?}", site.disagreements);
    }

    #[test]
    fn an_anchor_is_a_slug_cut_to_48_that_is_never_empty_or_id_shaped() {
        let mut taken = BTreeSet::new();
        assert_eq!(unique("Guards".into(), &mut taken), "guards");
        assert_eq!(unique("Guards".into(), &mut taken), "guards-2");
        assert_eq!(unique("Guards".into(), &mut taken), "guards-3");
        assert_eq!(unique("Ελληνικά".into(), &mut taken), "part");
        assert_eq!(unique("Ελληνικά".into(), &mut taken), "part-2");
        let long = "word ".repeat(60);
        let a = unique(long, &mut taken);
        assert!(a.chars().count() <= ANCHOR_MAX && !a.ends_with('-'), "{a}");
        // `D-1RRV2` slugs to `d-1rrv2`, which is not an id; digits only are
        assert_eq!(unique("d-12345".into(), &mut taken), "d-12345-x");
    }

    #[test]
    fn a_page_anchor_never_equals_an_id_the_chrome_owns() {
        let mut taken = reserved_ids();
        assert_eq!(unique("Main".into(), &mut taken), "main-2");
        assert_eq!(unique("Main".into(), &mut taken), "main-3");
        for (t, want) in [
            ("Search", "search-2"),
            ("Contents", "contents-2"),
            ("Versions", "versions-2"),
            ("Sidebar", "sidebar-2"),
        ] {
            assert_eq!(unique(t.into(), &mut taken), want);
        }
        assert_eq!(unique("Guards".into(), &mut taken), "guards");
    }

    #[test]
    fn a_name_is_worded_as_a_title() {
        let id: BlockId = "d-7KQ2M".parse().unwrap();
        assert_eq!(name_as_title("untitled-one", &id), "Untitled one");
        assert_eq!(name_as_title("untitled-draft", &id), "Untitled draft");
        assert_eq!(name_as_title("untitled-7kq2m", &id), "Untitled");
        assert_eq!(
            name_as_title("untitled-7kq2m", &"d-AAAAA".parse().unwrap()),
            "Untitled 7kq2m"
        );
        assert_eq!(name_as_title("doc-1", &id), "Doc 1");
        assert_eq!(name_as_title("wallets", &id), "Wallets");
    }

    #[test]
    fn an_href_to_the_same_page_is_just_the_anchor() {
        let u = Url {
            page: "a/b.html".into(),
            anchor: Some("x".into()),
        };
        assert_eq!(u.href_from("a/b.html"), "#x");
        assert_eq!(u.href_from("c.html"), "a/b.html#x");
        assert_eq!(u.href_from("a/c.html"), "../a/b.html#x");
    }

    /// rdx11 (POST-LAUNCH 52): a write can no longer name a sibling like a removed doc's id, but
    /// the clash refusal stays as defence in depth. On a synthetic state, built with no write, a
    /// removed doc's file (named by its id) and a sibling named that id ignoring case refuse a
    /// build that writes files, naming the sibling to rename. `Site::new` writes nothing, and both
    /// builds that write files ask for this refusal before they build anything, so the last site
    /// is kept.
    #[test]
    fn rdx11_a_neutral_path_that_matches_a_sibling_refuses_with_no_write() {
        use rdy_model::FieldValue;
        use rdy_model::redaction::RedactedBlockIdentity;
        let removed = BlockId::parse("d-SECRT").expect("an id");
        let sibling = BlockId::parse("d-NBR00").expect("an id");
        let doc = |id: &BlockId, order: &str, name: &str| Block {
            id: id.clone(),
            block_type: tree::DOC.into(),
            parent: None,
            order: order.into(),
            fields: BTreeMap::from([
                ("name".into(), FieldValue::Str(name.into())),
                ("title".into(), FieldValue::Str("Neighbour".into())),
                ("doc_type".into(), FieldValue::Str("page".into())),
            ]),
            body: String::new(),
            links: BTreeMap::new(),
            pins: BTreeMap::new(),
            read_identity: None,
        };
        let gone = Block::redacted(
            removed.clone(),
            tree::DOC.into(),
            None,
            "a0".into(),
            RedactedBlockIdentity {
                version: rdy_model::Hash::of(b"version"),
                content_hash: rdy_model::Hash::of(b"content"),
                frozen: false,
                request: rdy_model::Hash::of(b"request"),
            },
        );
        let pack = rdy_schema::PackSet::with_builtin()
            .resolve("rdy-standard")
            .expect("the pack");
        let state = |name: &str| {
            Materialized::build(
                rdy_model::StateSnapshot::default(),
                BTreeMap::from([
                    (removed.clone(), gone.clone()),
                    (sibling.clone(), doc(&sibling, "a1", name)),
                ]),
            )
        };
        for name in ["d-secrt", "D-SECRT", "D-secrt"] {
            let m = state(name);
            let site = Site::new(&m, &pack, "Project");
            for verb in ["render", "export"] {
                let e = site.path_clash(verb).expect("a refusal");
                assert_eq!(e.code, E_PATH_CLASH);
                assert_eq!(
                    e.message,
                    format!(
                        "{sibling} is named {name:?}, which matches the file name of redacted doc {removed} (ignoring case), so they cannot both be written as files"
                    )
                );
                assert_eq!(
                    e.fix,
                    format!(
                        "rename {sibling} (a redacted doc's file is named by its id), then {verb} again"
                    )
                );
            }
        }
        // renamed, the sibling no longer matches: no refusal, and both docs have pages
        let m = state("neighbour");
        let site = Site::new(&m, &pack, "Project");
        assert!(site.path_clash("render").is_none());
        assert_eq!(site.page(&removed).as_deref(), Some("d-SECRT.html"));
        assert_eq!(site.page(&sibling).as_deref(), Some("neighbour.html"));
    }
}
