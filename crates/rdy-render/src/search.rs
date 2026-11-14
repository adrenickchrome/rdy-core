//! The search index: `search-index.json`, one entry per doc, section, term, question and folder,
//! read by the client in `theme/rdy-standard/js/search.js`.
use crate::inline::Form;
use crate::page::PageContext;
use crate::site::Site;
use rdy_engine::Materialized;
use rdy_engine::reader::text as reader;
use rdy_model::redaction::CONTENT_REMOVED;
use rdy_model::{Block, BlockId, RefMode, tree};
use serde::Serialize;
use std::borrow::Cow;
use std::collections::BTreeMap;

/// The most characters of an entry's own text the index keeps.
const TEXT_MAX: usize = 400;

/// The most characters of a question's prompt its entry's title keeps (the text holds the rest).
const TITLE_MAX: usize = 120;

/// One searchable place on the site. Its fields are in the order the file has always written
/// them, by name: an entry is measured as written, borrowing what its page holds, before it is
/// kept.
#[derive(Clone, Debug, Serialize)]
struct Entry<'a> {
    anchor: Option<Cow<'a, str>>,
    /// The title of the doc (or folder) the entry is on.
    doc: Cow<'a, str>,
    kind: &'static str,
    /// Site-relative page url.
    page: Cow<'a, str>,
    text: Cow<'a, str>,
    title: Cow<'a, str>,
}

impl Entry<'_> {
    /// The entry with its own copy of what it borrowed.
    fn into_owned(self) -> Entry<'static> {
        let own = |c: Cow<str>| Cow::Owned(c.into_owned());
        Entry {
            anchor: self.anchor.map(own),
            doc: own(self.doc),
            kind: self.kind,
            page: own(self.page),
            text: own(self.text),
            title: own(self.title),
        }
    }

    /// The bytes its text takes.
    fn held(&self) -> usize {
        [&self.doc, &self.page, &self.text, &self.title]
            .into_iter()
            .chain(self.anchor.as_ref())
            .map(|s| s.len())
            .sum()
    }
}

/// `search-index.json`: its keys in the order it has always written them, by name.
#[derive(Serialize)]
struct File<'a> {
    entries: Vec<&'a Entry<'static>>,
    site: &'a str,
    version: u8,
}

/// The bytes `v` takes as JSON, counted as it is written: nothing is kept.
fn json_len<T: Serialize>(v: &T) -> u64 {
    struct Count(u64);
    impl std::io::Write for Count {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(b.len() as u64);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut c = Count(0);
    serde_json::to_writer(&mut c, v).expect("json");
    c.0
}

/// The entries of `search-index.json`, held to the bytes the file may take: each entry is
/// measured as the file will write it before it is kept, and an entry that would take the file
/// over `cap` is not kept. The index is then over, and keeps no further entry.
pub struct Index {
    /// Entries with the tree position of their page; pages are built in no particular order.
    entries: Vec<(usize, Entry<'static>)>,
    /// A page's position in a depth-first walk of the tree, filled by the first page.
    rank: Option<BTreeMap<String, (usize, BlockId)>>,
    site: String,
    /// The bytes of the file with the entries kept so far.
    len: u64,
    /// The most bytes the file may take.
    cap: u64,
    over: bool,
    /// The bytes the kept entries hold.
    held: usize,
}

impl Index {
    /// An empty index of the site named `site`, its file held to `cap` bytes.
    pub fn new(site: &str, cap: u64) -> Index {
        let len = json_len(&File {
            entries: vec![],
            site,
            version: 1,
        });
        Index {
            entries: vec![],
            rank: None,
            site: site.to_string(),
            len,
            cap,
            over: len > cap,
            held: 0,
        }
    }

    /// An entry would have taken the file over its cap.
    pub fn over(&self) -> bool {
        self.over
    }

    /// The bytes the file takes with the entries kept.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// The bytes the kept entries hold.
    pub fn held(&self) -> usize {
        self.held
    }

    /// Keeps `entry` at `rank` when the file has room for it.
    fn add(&mut self, rank: usize, entry: Entry) {
        if self.over {
            return;
        }
        // one more entry is parted from the one before it by a comma
        let n = json_len(&entry) + u64::from(!self.entries.is_empty());
        match self.len.checked_add(n).filter(|end| *end <= self.cap) {
            Some(end) => {
                self.len = end;
                self.held += entry.held();
                self.entries.push((rank, entry.into_owned()));
            }
            None => self.over = true,
        }
    }
}

/// The tree's folders and docs by page url, with each one's position in a depth-first walk in
/// sibling order.
fn tree_rank(m: &Materialized, site: &Site) -> BTreeMap<String, (usize, BlockId)> {
    let mut out = BTreeMap::new();
    let mut stack: Vec<(&Block, usize)> =
        m.children(None).into_iter().rev().map(|b| (b, 0)).collect();
    while let Some((b, level)) = stack.pop() {
        if !tree::is_tree_type(&b.block_type) || level > tree::MAX_FOLDER_DEPTH {
            continue;
        }
        let Some(page) = site.page(&b.id) else {
            continue;
        };
        let n = out.len();
        if out.insert(page, (n, b.id.clone())).is_some() {
            continue;
        }
        stack.extend(
            m.children(Some(&b.id))
                .into_iter()
                .rev()
                .map(|c| (c, level + 1)),
        );
    }
    out
}

/// `s` with whitespace collapsed and cut to `max` characters.
fn clip(s: &str, max: usize) -> String {
    let mut out = String::new();
    for word in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
        if out.chars().count() >= max {
            break;
        }
    }
    out.chars()
        .take(max)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// `text` as a reader sees it ([`Site::plain`]): Markdown flattened, a reference as its display
/// text or its target's label (nothing when it leads nowhere), `\{{` a literal `{{`.
fn plain(text: &str, mode: RefMode, form: Form, site: &Site) -> String {
    site.plain(text, mode, form, 0, &|_| String::new())
}

/// A field's words: a statement or prompt reads as running text, any other as a label
/// ([`reader::field`]).
fn field(b: &Block, name: &str, site: &Site) -> String {
    reader::field(site, b, name)
}

/// The text a reader can find in `b` itself (not in its children), with its own whitespace
/// ([`reader::own_text`]).
pub(crate) fn own_text(b: &Block, site: &Site) -> String {
    reader::own_text(site, b)
}

/// As [`own_text`], with each code span of the block's spans between the code marks
/// ([`reader::own_text_coded`]): the word diff of a change summary reads it (sr6).
pub(crate) fn own_text_coded(b: &Block, site: &Site) -> String {
    reader::own_text_coded(site, b)
}

/// A section's own words: what its direct children that are not sections say. A rationale, a
/// finding and a source are off the page (FW-D10, D-Z8P09), so they are off the index too; a
/// doc's direct words are read the same way.
fn section_text(sec: &Block, m: &Materialized, site: &Site) -> String {
    let mut out = plain(&sec.body, RefMode::Strict, Form::Body, site);
    for c in m.children(Some(&sec.id)) {
        if c.block_type == "section" || crate::site::is_off_site(c) || crate::site::is_profile(c) {
            continue;
        }
        out.push(' ');
        out.push_str(&own_text(c, site));
        if out.chars().count() > TEXT_MAX * 2 {
            break;
        }
    }
    clip(&out, TEXT_MAX)
}

/// The text of the first prose block under `doc`, in reading order.
fn first_prose(doc: &BlockId, m: &Materialized, site: &Site) -> String {
    let mut stack: Vec<&Block> = m.children(Some(doc)).into_iter().rev().collect();
    while let Some(b) = stack.pop() {
        if b.block_type == "prose" {
            return clip(&plain(&b.body, RefMode::Strict, Form::Body, site), TEXT_MAX);
        }
        stack.extend(m.children(Some(&b.id)).into_iter().rev());
    }
    String::new()
}

/// An entry on `page`, borrowing the page's url and title.
fn entry_on<'a>(
    page: &'a PageContext,
    kind: &'static str,
    anchor: Option<&'a str>,
    title: String,
    text: String,
) -> Entry<'a> {
    Entry {
        anchor: anchor.filter(|a| !a.is_empty()).map(Cow::Borrowed),
        doc: Cow::Borrowed(page.title.as_str()),
        kind,
        page: Cow::Borrowed(page.page_url.as_str()),
        text: Cow::Owned(text),
        title: Cow::Owned(title),
    }
}

/// Adds the page's entries to the index.
pub fn collect(index: &mut Index, page: &PageContext, m: &Materialized, site: &Site) {
    if index.over || (page.kind != "doc" && page.kind != "folder") {
        return;
    }
    let Some((rank, id)) = index
        .rank
        .get_or_insert_with(|| tree_rank(m, site))
        .get(&page.page_url)
        .cloned()
    else {
        return;
    };
    // a doc left out of navigation is left out of search too
    if !site.in_nav(&id) {
        return;
    }
    if let Some(folder) = &page.body.folder {
        let text = clip(
            &site.description_words(folder.description.as_deref().unwrap_or("")),
            TEXT_MAX,
        );
        index.add(
            rank,
            entry_on(page, "folder", None, folder.title.clone(), text),
        );
        return;
    }
    let Some(doc) = &page.body.doc else { return };
    // what the doc says directly (outside any section), else the first prose under it
    let direct = m
        .get(&id)
        .map(|d| section_text(d, m, site))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| first_prose(&id, m, site));
    index.add(rank, entry_on(page, "doc", None, doc.title.clone(), direct));
    for item in &page.items {
        let Some(b) = BlockId::parse(&item.id).ok().and_then(|id| m.get(&id)) else {
            continue;
        };
        match b.block_type.as_str() {
            "section" => {
                let title = item.title.clone().unwrap_or_else(|| site.label(b));
                index.add(
                    rank,
                    entry_on(
                        page,
                        "section",
                        Some(item.anchor.as_str()),
                        title,
                        section_text(b, m, site),
                    ),
                );
            }
            "term" => {
                let title = item.title.clone().unwrap_or_else(|| site.label(b));
                let text = clip(&plain(&b.body, RefMode::Strict, Form::Body, site), TEXT_MAX);
                index.add(
                    rank,
                    entry_on(page, "term", Some(item.anchor.as_str()), title, text),
                );
            }
            "question" => {
                // a removed question's prompt is the notice: its stub has no prompt
                let prompt = match b.is_redacted() {
                    true => CONTENT_REMOVED.to_string(),
                    false => clip(&field(b, "prompt", site), TEXT_MAX),
                };
                let url = site.url_of(&b.id);
                let mut e = entry_on(
                    page,
                    "question",
                    url.as_ref().and_then(|u| u.anchor.as_deref()),
                    clip(&prompt, TITLE_MAX),
                    prompt,
                );
                if let Some(u) = &url {
                    e.page = Cow::Borrowed(u.page.as_str());
                }
                index.add(rank, e);
            }
            _ => {}
        }
    }
}

/// The bytes of `search-index.json`, the entries in tree order: exactly [`Index::len`] bytes,
/// written straight from the entries.
pub fn bytes(index: &Index) -> Vec<u8> {
    let mut entries: Vec<&(usize, Entry)> = index.entries.iter().collect();
    entries.sort_by_key(|(rank, _)| *rank);
    let file = File {
        entries: entries.into_iter().map(|(_, e)| e).collect(),
        site: &index.site,
        version: 1,
    };
    let mut out = Vec::with_capacity(usize::try_from(index.len).unwrap_or(0));
    serde_json::to_writer(&mut out, &file).expect("json");
    debug_assert_eq!(out.len() as u64, index.len, "the measured index");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(doc: &str, title: &str, anchor: Option<&str>) -> Entry<'static> {
        Entry {
            anchor: anchor.map(|a| Cow::Owned(a.to_string())),
            doc: Cow::Owned(doc.to_string()),
            kind: "section",
            page: Cow::Owned("a/b.html".to_string()),
            text: Cow::Owned("Text with \"quotes\", a \\ and \u{1} é \u{2028}.".to_string()),
            title: Cow::Owned(title.to_string()),
        }
    }

    /// Fix round 2: the file is written straight from the entries, in the bytes the `json!`
    /// value always wrote (its keys by name), and is exactly as long as it was measured.
    #[test]
    fn hpb6_the_index_is_written_as_the_json_value_wrote_it_and_measured_exactly() {
        let mut index = Index::new("Site \"é\"", u64::MAX);
        index.add(2, sample("Doc ü", "Two", None));
        index.add(0, sample("Doc", "Zero", Some("s0")));
        index.add(1, sample("Doc </script>", "One", Some("é")));
        let mut entries: Vec<&(usize, Entry)> = index.entries.iter().collect();
        entries.sort_by_key(|(rank, _)| *rank);
        let was = serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "site": "Site \"é\"",
            "entries": entries.iter().map(|(_, e)| e).collect::<Vec<_>>(),
        }))
        .unwrap();
        let out = bytes(&index);
        assert_eq!(
            String::from_utf8(out.clone()).unwrap(),
            String::from_utf8(was).unwrap()
        );
        assert_eq!(out.len() as u64, index.len());
        let empty = Index::new("S", u64::MAX);
        assert_eq!(bytes(&empty), br#"{"entries":[],"site":"S","version":1}"#);
        assert_eq!(empty.len(), 37);
    }

    /// Fix round 2: an entry that would take the file over its cap is never kept, and nothing is
    /// kept after it.
    #[test]
    fn hpb6_an_entry_over_the_cap_is_not_kept() {
        let mut free = Index::new("S", u64::MAX);
        free.add(0, sample("D", "T", None));
        let one = free.len();
        let mut index = Index::new("S", one + 3);
        index.add(0, sample("D", "T", None));
        assert_eq!((index.len(), index.over()), (one, false));
        index.add(1, sample(&"d".repeat(1 << 16), "T", None));
        assert!(index.over());
        index.add(2, sample("D", "T", None));
        assert_eq!((index.entries.len(), index.len()), (1, one));
        assert_eq!(index.held(), index.entries[0].1.held());
        // an index with no room for its frame is over before any entry
        assert!(Index::new("S", 10).over());
    }

    #[test]
    fn clip_collapses_whitespace_and_cuts_by_characters() {
        assert_eq!(clip("  a \n b\t c ", 400), "a b c");
        let long = "é".repeat(500);
        assert_eq!(clip(&long, 400).chars().count(), 400);
        assert_eq!(
            clip(&format!("{} {}", "x".repeat(399), "yy"), 400),
            "x".repeat(399)
        );
    }
}
