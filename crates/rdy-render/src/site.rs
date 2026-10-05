//! The site's shape: which page every folder, doc and block lives on, where on the page, and
//! the order a reader walks the docs in.
use crate::inline::{self, Form};
use rdy_engine::Materialized;
use rdy_model::{Block, BlockId, RefMode, decision, tree};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
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

/// The block types numbered within their doc (`Table 2`, `Python code 3`, `Diagram 1`).
const NUMBERED: [&str; 3] = ["table", "code", "diagram"];

/// How many references deep a label reads its references' labels; past it a reference reads
/// as `…` (a title that names a block whose title names it back stops here).
const LABEL_DEPTH: usize = 3;

/// The id inside a longer text, as the leak scan reads it.
static ID_INSIDE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[A-Za-z]{1,3}-[0-9A-HJKMNP-TV-Z]{5}\b").expect("a pattern"));

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
    /// The first block each decision governs.
    governs: BTreeMap<BlockId, BlockId>,
    /// The questions no decision answers, in reading order of their docs.
    pub open_questions: Vec<BlockId>,
    /// Those questions' anchors on `questions.html`.
    pub question_anchors: BTreeMap<BlockId, String>,
    /// Every term, by name, case-insensitively.
    pub terms: Vec<BlockId>,
    /// The terms' anchors on `glossary.html`.
    pub term_anchors: BTreeMap<BlockId, String>,
}

impl<'a> Site<'a> {
    pub fn new(m: &'a Materialized) -> Site<'a> {
        // `level` folders are above `parent`'s children; only a damaged store nests deeper than
        // MAX_FOLDER_DEPTH, and what lies below it gets no page
        fn walk(
            m: &Materialized,
            parent: Option<&BlockId>,
            prefix: &str,
            level: usize,
            out: &mut BTreeMap<BlockId, String>,
            order: &mut Vec<BlockId>,
            tree_order: &mut Vec<BlockId>,
        ) {
            if level > tree::MAX_FOLDER_DEPTH {
                return;
            }
            let names = m.names_under(parent);
            for b in m.children(parent) {
                let Some(name) = names.get(&b.id) else {
                    continue;
                };
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}/{name}")
                };
                out.insert(b.id.clone(), path.clone());
                tree_order.push(b.id.clone());
                if b.block_type == tree::FOLDER {
                    walk(m, Some(&b.id), &path, level + 1, out, order, tree_order);
                } else if b.block_type == tree::DOC {
                    order.push(b.id.clone());
                }
            }
        }
        let mut paths = BTreeMap::new();
        let mut order = vec![];
        let mut tree_order = vec![];
        walk(m, None, "", 0, &mut paths, &mut order, &mut tree_order);
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
            terms: vec![],
            term_anchors: BTreeMap::new(),
        };
        site.compute_numbers();
        site.compute_anchors();
        site.compute_governed();
        site.compute_collected();
        site
    }

    /// Every block under `doc`, in reading order.
    fn blocks_of(&self, doc: &BlockId) -> Vec<&'a Block> {
        let mut out = vec![];
        let mut stack: Vec<&Block> = self.m.children(Some(doc)).into_iter().rev().collect();
        while let Some(b) = stack.pop() {
            out.push(b);
            stack.extend(self.m.children(Some(&b.id)).into_iter().rev());
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
                // a decision has no place in the text: it is linked through what it governs
                if b.block_type == decision::DECISION {
                    continue;
                }
                anchors.insert(b.id.clone(), unique(self.anchor_text(b), &mut taken));
            }
        }
        self.anchors = anchors;
    }

    fn compute_governed(&mut self) {
        let mut governs: BTreeMap<BlockId, BlockId> = BTreeMap::new();
        // a decision governs what it affects, else what names it as `decided_by`, else its
        // parent section
        for b in self.m.blocks.values() {
            if b.block_type != decision::DECISION {
                continue;
            }
            if let Some(l) = b.links.get(decision::AFFECTS).and_then(|l| l.first()) {
                governs.insert(b.id.clone(), l.target.clone());
            }
        }
        for b in self.m.blocks.values() {
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
        for b in self.m.blocks.values() {
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
            .blocks
            .values()
            .flat_map(|b| b.links.get(decision::ANSWERS).into_iter().flatten())
            .map(|l| &l.target)
            .collect();
        let (mut open, mut question_anchors) = (vec![], BTreeMap::new());
        let mut taken = reserved_ids();
        let mut terms: Vec<&Block> = vec![];
        for doc in &self.order {
            for b in self.blocks_of(doc) {
                if b.block_type == decision::QUESTION && !answered.contains(&b.id) {
                    open.push(b.id.clone());
                    question_anchors.insert(b.id.clone(), unique(self.anchor_text(b), &mut taken));
                }
                if b.block_type == "term" {
                    terms.push(b);
                }
            }
        }
        terms.sort_by_cached_key(|b| (self.label(b).to_lowercase(), b.id.clone()));
        let (mut term_anchors, mut taken) = (BTreeMap::new(), reserved_ids());
        for b in &terms {
            term_anchors.insert(b.id.clone(), unique(self.anchor_text(b), &mut taken));
        }
        // a top-level doc named `glossary` or `questions` already has that page: it keeps it,
        // and the collected page is not written (nor linked)
        if !self.page_taken("questions.html") {
            self.open_questions = open;
            self.question_anchors = question_anchors;
        }
        if !self.page_taken("glossary.html") {
            self.terms = terms.iter().map(|b| b.id.clone()).collect();
            self.term_anchors = term_anchors;
        }
    }

    /// Whether a doc or folder already has the page at `page`.
    fn page_taken(&self, page: &str) -> bool {
        self.paths
            .keys()
            .any(|id| self.page(id).as_deref() == Some(page))
    }

    /// The page of a folder or doc.
    pub fn page(&self, id: &BlockId) -> Option<String> {
        let p = self.paths.get(id)?;
        Some(match self.m.get(id)?.block_type == tree::FOLDER {
            true => format!("{p}/index.html"),
            false => format!("{p}.html"),
        })
    }

    /// Every folder and doc that has a page, in tree order.
    pub fn tree_pages(&self) -> impl Iterator<Item = &BlockId> {
        self.tree.iter()
    }

    /// Where `id` lives, for any block: a tree block on its page, a block inside a doc on the
    /// doc's page at its anchor, a decision where the first block it governs lives, a term at its
    /// place in its doc, an open question on the questions page.
    pub fn url_of(&self, id: &BlockId) -> Option<Url> {
        self.url_of_bounded(id, &mut BTreeSet::new())
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
        let doc = self.m.doc_of(id)?;
        Some(Url {
            page: self.page(&doc.id)?,
            anchor: Some(self.anchors.get(id)?.clone()),
        })
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
                .map(|n| name_as_title(&n))
                .unwrap_or_default(),
            None if b.block_type == "code" => {
                let k = self.number(b);
                match b.field_str("lang").filter(|l| !l.trim().is_empty()) {
                    Some(l) => format!("{} code {k}", crate::highlight::human_name(l)),
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

    /// `text` as plain words in `form` ([`inline::plain_refs`]): a reference with no display text reads as `ref_words`
    /// says (the target's key when typed as one and the target has one, else its label), or as
    /// `unresolved(token)` when it leads nowhere.
    pub fn plain(
        &self,
        text: &str,
        mode: RefMode,
        form: Form,
        depth: usize,
        unresolved: &dyn Fn(&str) -> String,
    ) -> String {
        inline::plain_refs(text, mode, form, &|span| match BlockId::parse(&span.token)
            .ok()
            .and_then(|id| self.m.get(&id))
        {
            Some(_) if depth >= LABEL_DEPTH => "…".to_string(),
            Some(t) => match t
                .field_str("key")
                .filter(|k| span.key_typed && !k.is_empty())
            {
                Some(k) => k.to_string(),
                None => self.label_at(t, depth + 1),
            },
            None => unresolved(&span.token),
        })
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
            // source is not Markdown: its words are its text
            let text = match b.block_type.as_str() {
                "code" | "diagram" | "figure" => {
                    let raw = inline::unescape_braces(&b.body);
                    raw.split_whitespace().collect::<Vec<_>>().join(" ")
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
            "term" | "component" | "actor" | "interface" | "flow" => b.field_str("name"),
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
            if out.len() > self.m.blocks.len() {
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

    /// The docs before and after `doc` in reading order.
    pub fn prev_next(&self, doc: &BlockId) -> (Option<BlockId>, Option<BlockId>) {
        let Some(i) = self.order.iter().position(|d| d == doc) else {
            return (None, None);
        };
        (
            i.checked_sub(1).map(|p| self.order[p].clone()),
            self.order.get(i + 1).cloned(),
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
/// name `untitled-<id>` that a doc without a title gets reads as `Untitled`.
fn name_as_title(name: &str) -> String {
    let id_chars = "0123456789abcdefghjkmnpqrstvwxyz";
    let derived = name
        .strip_prefix("untitled-")
        .is_some_and(|b| b.chars().count() == 5 && b.chars().all(|c| id_chars.contains(c)));
    if derived {
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
/// switcher). A block anchor never equals one, so each per-page `taken` set starts with them.
const RESERVED_IDS: &[&str] = &[
    "main", "search", "results", "sidebar", "menu", "scrim", "contents", "versions",
];

/// A fresh per-page `taken` set, holding [`RESERVED_IDS`].
fn reserved_ids() -> BTreeSet<String> {
    RESERVED_IDS.iter().map(|s| s.to_string()).collect()
}

/// `text` as an anchor not in `taken`, which then holds it: slugged, cut to [`ANCHOR_MAX`],
/// `part` when nothing is left, `-x` after anything shaped like an id, `-2`, `-3`... on a clash.
fn unique(text: String, taken: &mut BTreeSet<String>) -> String {
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
        assert_eq!(name_as_title("untitled-one"), "Untitled one");
        assert_eq!(name_as_title("untitled-7kq2m"), "Untitled");
        assert_eq!(name_as_title("doc-1"), "Doc 1");
        assert_eq!(name_as_title("wallets"), "Wallets");
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
}
