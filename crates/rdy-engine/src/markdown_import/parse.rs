//! The converter behind [`super::convert`]. It checks the paths, reads each file's front matter,
//! then walks each file's CommonMark events twice: first for its top-level headings (so every
//! link target in the source is known), then to build its blocks. Every walk is a loop over the
//! parser's event stream with a depth counter, never a recursion.
use super::{
    BLOCKS, FOLDER_DEPTH, FRONT_MATTER_BYTES, MAX_BLOCKS, MAX_FRONT_MATTER_BYTES, MAX_NESTING,
    MAX_PATH_BYTES, MAX_UNMAPPED, MarkdownFile, NESTING, PATH_BYTES, Prepared, PreparedBlock,
    PreparedContent, PreparedDoc, PreparedFolder, Segment, Target, Text, UNMAPPED_ROWS, Unmapped,
    UnmappedKind, limit_error,
};
use crate::error::{E_BAD_INPUT, E_FILE_FORMAT, RdyError};
use crate::yaml_pos::{Seg, positions};
use pulldown_cmark::{CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};
use rdy_model::tree;
use rdy_schema::Pack;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

/// The CommonMark parser options: tables map; footnotes, strikethrough, task lists and GFM
/// alerts are parsed only so that they can be reported as `extension` rows.
fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM
}

pub(super) fn unsafe_path(path: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("Markdown source path is not a contained regular file or directory: {path}"),
        "copy regular files into a private source directory",
    )
}

pub(super) fn collision(first: &str, second: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("Markdown source paths collide: {first}, {second}"),
        "rename one source path and update its links",
    )
}

/// Whether `text` starts with a drive prefix (`C:`).
fn is_drive(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// The number of segments of a plain logical relative path: non-empty segments other than `.`
/// and `..`, joined by `/`, with no `\`, NUL or drive prefix.
pub(super) fn check_path(path: &str) -> Result<usize, RdyError> {
    if path.len() > MAX_PATH_BYTES {
        return Err(limit_error(PATH_BYTES));
    }
    let bad = path.is_empty()
        || path.contains(['\\', '\0'])
        || is_drive(path)
        || path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..");
    if bad {
        return Err(unsafe_path(path));
    }
    Ok(path.split('/').count())
}

/// Adds `path` and every directory above it.
fn with_ancestors<'a>(set: &mut BTreeSet<&'a str>, path: &'a str) {
    let mut p = path;
    while set.insert(p) {
        match p.rsplit_once('/') {
            Some((up, _)) => p = up,
            None => break,
        }
    }
}

/// A file name without its `.md` or `.markdown` suffix (any case).
fn stem(leaf: &str) -> &str {
    let lower = leaf.to_ascii_lowercase();
    for ext in [".markdown", ".md"] {
        if lower.len() > ext.len() && lower.ends_with(ext) {
            return &leaf[..leaf.len() - ext.len()];
        }
    }
    leaf
}

/// A valid rdy name from a source name: its slug, else `<kind>-<slug>`, else `<kind>`; then the
/// first of `name`, `name-2`, … not taken under the parent (ignoring case).
fn name_for(source: &str, kind: &str, taken: &mut BTreeSet<String>) -> String {
    let slug = tree::slug(source);
    let prefixed: String = format!("{kind}-{slug}")
        .chars()
        .take(tree::NAME_MAX)
        .collect::<String>()
        .trim_end_matches(['-', '.'])
        .to_string();
    let base = [slug, prefixed]
        .into_iter()
        .find(|n| tree::name_problem(n).is_none())
        .unwrap_or_else(|| kind.to_string());
    let name = tree::dedupe(&base, taken);
    taken.insert(name.to_ascii_lowercase());
    name
}

struct DocShell<'a> {
    path: &'a str,
    parent: Option<usize>,
    name: String,
    file: usize,
}

struct Inventory<'a> {
    folders: Vec<PreparedFolder>,
    docs: Vec<DocShell<'a>>,
}

/// The folders and docs in path order (UTF-8 bytes), with their parents and names.
fn inventory<'a>(dirs: &'a [String], files: &'a [MarkdownFile]) -> Result<Inventory<'a>, RdyError> {
    let mut dir_set: BTreeSet<&'a str> = BTreeSet::new();
    for d in dirs {
        if check_path(d)? > tree::MAX_FOLDER_DEPTH {
            return Err(limit_error(FOLDER_DEPTH));
        }
        with_ancestors(&mut dir_set, d);
    }
    let mut file_set: BTreeMap<&'a str, usize> = BTreeMap::new();
    for (i, f) in files.iter().enumerate() {
        if check_path(&f.path)? - 1 > tree::MAX_FOLDER_DEPTH {
            return Err(limit_error(FOLDER_DEPTH));
        }
        if file_set.insert(&f.path, i).is_some() {
            return Err(collision(&f.path, &f.path));
        }
        if let Some((up, _)) = f.path.rsplit_once('/') {
            with_ancestors(&mut dir_set, up);
        }
    }
    if let Some(p) = file_set.keys().find(|p| dir_set.contains(*p)) {
        return Err(collision(p, p));
    }
    let mut entries: Vec<(&'a str, Option<usize>)> = dir_set
        .into_iter()
        .map(|d| (d, None))
        .chain(file_set.into_iter().map(|(p, i)| (p, Some(i))))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut inv = Inventory {
        folders: vec![],
        docs: vec![],
    };
    let mut folder_of: BTreeMap<&str, usize> = BTreeMap::new();
    let mut taken: BTreeMap<Option<usize>, BTreeSet<String>> = BTreeMap::new();
    for (path, file) in entries {
        let (up, leaf) = match path.rsplit_once('/') {
            Some((up, leaf)) => (Some(up), leaf),
            None => (None, path),
        };
        // a parent sorts before its children, so its folder is already there
        let parent = up.map(|u| folder_of[u]);
        let names = taken.entry(parent).or_default();
        match file {
            None => {
                let name = name_for(leaf, tree::FOLDER, names);
                folder_of.insert(path, inv.folders.len());
                inv.folders.push(PreparedFolder {
                    path: path.to_string(),
                    parent,
                    name,
                });
            }
            Some(file) => {
                let name = name_for(stem(leaf), tree::DOC, names);
                inv.docs.push(DocShell {
                    path,
                    parent,
                    name,
                    file,
                });
            }
        }
    }
    Ok(inv)
}

/// Front matter: none, valid (its title and doc type), or kept as literal text up to `end` with
/// the source lines of its problems.
enum FrontMatter {
    None,
    Valid {
        title: Option<String>,
        doc_type: Option<String>,
    },
    Literal {
        end: usize,
        lines: Vec<usize>,
    },
}

/// A decoded source file: its text (without a byte-order mark), its front matter, and where its
/// Markdown body starts.
struct Source<'a> {
    path: &'a str,
    text: &'a str,
    front: FrontMatter,
    body: usize,
}

/// The schema's refusal of an unknown doc type, at the source position of the `doc_type` key.
fn unknown_doc_type(path: &str, at: (usize, usize), value: &str, pack: &Pack) -> RdyError {
    RdyError::new(
        "E_DOC_TYPE",
        format!("{path}:{}:{}: unknown doc_type {value:?}", at.0, at.1),
        format!(
            "use one of: {}",
            pack.doc_types
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
}

/// The line at `from`: the end of its content (before `\n`) and the start of the next line, if
/// it has a line break.
fn line_at(text: &str, from: usize) -> (usize, Option<usize>) {
    match text[from..].find('\n') {
        Some(i) => (from + i, Some(from + i + 1)),
        None => (text.len(), None),
    }
}

fn is_fence(line: &str, marks: &[&str]) -> bool {
    marks.contains(&line.trim_end_matches([' ', '\t', '\r']))
}

/// Reads front matter: a first line `---` and a later line `---` or `...`. Without a closing line
/// there is no front matter. A YAML problem keeps the whole front matter as literal text; only
/// an unknown `doc_type` refuses (D-19BZD).
fn read_front_matter(
    path: &str,
    text: &str,
    pack: &Pack,
) -> Result<(FrontMatter, usize), RdyError> {
    let (first_end, Some(content_start)) = line_at(text, 0) else {
        return Ok((FrontMatter::None, 0));
    };
    if !is_fence(&text[..first_end], &["---"]) {
        return Ok((FrontMatter::None, 0));
    }
    let mut at = content_start;
    let (content_end, body) = loop {
        let (end, next) = line_at(text, at);
        if is_fence(&text[at..end], &["---", "..."]) {
            break (at, next.unwrap_or(text.len()));
        }
        match next {
            Some(n) if n < text.len() => at = n,
            _ => return Ok((FrontMatter::None, 0)),
        }
    };
    if content_end - content_start > MAX_FRONT_MATTER_BYTES {
        return Err(limit_error(FRONT_MATTER_BYTES));
    }
    let yaml = text[content_start..content_end].replace("\r\n", "\n");
    // YAML line n is source line n + 1: the opening fence is line 1
    let literal = |lines: Vec<usize>| FrontMatter::Literal { end: body, lines };
    let map = match serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&yaml) {
        Err(e) => {
            let line = 1 + e.location().map_or(0, |l| l.line());
            return Ok((literal(vec![line]), body));
        }
        Ok(serde_yaml_ng::Value::Null) => {
            let none = FrontMatter::Valid {
                title: None,
                doc_type: None,
            };
            return Ok((none, body));
        }
        Ok(serde_yaml_ng::Value::Mapping(m)) => m,
        Ok(_) => return Ok((literal(vec![1]), body)),
    };
    let pos = positions(&yaml).unwrap_or_default();
    let at_key = |k: &str| {
        pos.get(&vec![Seg::Key(k.to_string())])
            .map_or((1, 1), |&(l, c)| (l + 1, c))
    };
    let (mut title, mut doc_type, mut problems) = (None, None, vec![]);
    for (k, v) in &map {
        let Some(key) = k.as_str() else {
            problems.push(1);
            continue;
        };
        match (key, v) {
            ("title", serde_yaml_ng::Value::String(s)) if !s.trim().is_empty() => {
                title = Some(s.clone());
            }
            ("doc_type", serde_yaml_ng::Value::String(s)) => {
                // rdy never guesses a type: an unknown one refuses whatever else is wrong
                if !pack.doc_types.contains_key(s) {
                    return Err(unknown_doc_type(path, at_key(key), s, pack));
                }
                doc_type = Some(s.clone());
            }
            _ => problems.push(at_key(key).0),
        }
    }
    if problems.is_empty() {
        Ok((FrontMatter::Valid { title, doc_type }, body))
    } else {
        Ok((literal(problems), body))
    }
}

fn source<'a>(path: &'a str, bytes: &'a [u8], pack: &Pack) -> Result<Source<'a>, RdyError> {
    // the export import's refusal of a file it cannot read as UTF-8
    let text = std::str::from_utf8(bytes).map_err(|_| {
        RdyError::new(
            E_FILE_FORMAT,
            format!("{path}:1:1: cannot read the file: stream did not contain valid UTF-8"),
            "check the file",
        )
    })?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let (front, body) = read_front_matter(path, text, pack)?;
    Ok(Source {
        path,
        text,
        front,
        body,
    })
}

fn bump(count: &mut usize) -> Result<(), RdyError> {
    *count += 1;
    if *count > MAX_BLOCKS {
        return Err(limit_error(BLOCKS));
    }
    Ok(())
}

/// A heading's anchor as GitHub writes it: lowercase; letters, digits, `-` and `_` kept, a space
/// as `-`, everything else dropped.
fn heading_slug(plain: &str) -> String {
    plain
        .chars()
        .flat_map(char::to_lowercase)
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// The anchors of a doc's sections: each anchor to its section's ordinal (the nth top-level
/// heading with text). A repeated anchor takes `-1`, `-2`, …, skipping one already taken.
#[derive(Default)]
struct Anchors {
    by_slug: BTreeMap<String, usize>,
    seen: BTreeMap<String, usize>,
    count: usize,
}

impl Anchors {
    fn add(&mut self, plain: &str) {
        let base = heading_slug(plain);
        let mut n = self.seen.get(&base).copied().unwrap_or(0);
        let mut slug = if n == 0 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        while self.by_slug.contains_key(&slug) {
            n += 1;
            slug = format!("{base}-{n}");
        }
        self.seen.insert(base, n + 1);
        self.by_slug.insert(slug, self.count);
        self.count += 1;
    }
}

/// The first walk: refuses nesting past the bound and counts the sections toward the block
/// bound before any block is built.
fn anchors(src: &Source, planned: &mut usize) -> Result<Anchors, RdyError> {
    let mut out = Anchors::default();
    let mut depth = 0usize;
    // the open top-level heading: whether it has text, and its plain text
    let mut heading: Option<(bool, String)> = None;
    for ev in Parser::new_ext(&src.text[src.body..], options()) {
        match &ev {
            Event::Start(tag) => {
                depth += 1;
                if depth > MAX_NESTING {
                    return Err(limit_error(NESTING));
                }
                if depth == 1 {
                    if matches!(tag, Tag::Heading { .. }) {
                        heading = Some((false, String::new()));
                    }
                    continue;
                }
            }
            Event::End(_) => {
                depth -= 1;
                if depth == 0 {
                    if let Some((true, plain)) = heading.take() {
                        bump(planned)?;
                        out.add(&plain);
                    }
                    continue;
                }
            }
            _ => {}
        }
        if let Some((inner, plain)) = &mut heading {
            *inner = true;
            match &ev {
                Event::Text(t) | Event::Code(t) => plain.push_str(t),
                Event::SoftBreak | Event::HardBreak => plain.push(' '),
                _ => {}
            }
        }
    }
    Ok(out)
}

/// What a link target names.
enum Resolved {
    /// A URL: kept as written.
    External,
    /// One imported doc, and one section of it (by ordinal) for a fragment.
    Local(Target),
    /// A local target that names no single included Markdown file and heading.
    Broken,
}

/// `text` with each `%XX` decoded once; `None` when a decoded byte is a separator or NUL, or
/// the result is not UTF-8.
fn decode_once(text: &str) -> Option<String> {
    let b = text.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (
                b.get(i + 1).copied().and_then(hex),
                b.get(i + 2).copied().and_then(hex),
            )
        {
            let byte = (h * 16 + l) as u8;
            if matches!(byte, b'/' | b'\\' | 0) {
                return None;
            }
            out.push(byte);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `rel` joined to the directory `base`; `.` and empty segments dropped, `..` resolved. `None`
/// when `..` leaves the source root.
fn normalize(base: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

/// Whether `text` starts with a URL scheme (`https:`, `mailto:`); one letter is a drive.
fn has_scheme(text: &str) -> bool {
    let Some(colon) = text.find(':') else {
        return false;
    };
    let scheme = &text[..colon];
    scheme.len() >= 2
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// The included Markdown files and their sections, for link resolution. A link never opens a
/// file: a target outside these paths is broken, whatever exists on disk.
struct Links<'a> {
    by_path: BTreeMap<&'a str, usize>,
    paths: Vec<&'a str>,
    anchors: Vec<Anchors>,
}

impl Links<'_> {
    fn resolve(&self, from: usize, dest: &str) -> Resolved {
        if dest.is_empty() {
            return Resolved::Broken;
        }
        if dest.starts_with("//") || (!is_drive(dest) && has_scheme(dest)) {
            return Resolved::External;
        }
        match self.local(from, dest) {
            Some(t) => Resolved::Local(t),
            None => Resolved::Broken,
        }
    }

    fn local(&self, from: usize, dest: &str) -> Option<Target> {
        let (path, fragment) = match dest.split_once('#') {
            Some((p, f)) => (p, Some(f)),
            None => (dest, None),
        };
        if path.contains('?') {
            return None;
        }
        let doc = if path.is_empty() {
            from
        } else {
            let decoded = decode_once(path)?;
            if is_drive(&decoded) {
                return None;
            }
            match decoded.strip_prefix('/') {
                Some(rooted) => self.rooted(rooted)?,
                None => {
                    let base = self.paths[from].rsplit_once('/').map_or("", |(d, _)| d);
                    *self.by_path.get(normalize(base, &decoded)?.as_str())?
                }
            }
        };
        let block = match fragment {
            None | Some("") => None,
            Some(f) => Some(*self.anchors[doc].by_slug.get(&decode_once(f)?)?),
        };
        Some(Target { doc, block })
    }

    /// D-H5MKZ: a root link drops a trailing `/`, `.md` or `.markdown`, then finds exactly one of
    /// `a/b.md`, `a/b.markdown`, `a/b/index.md` and `a/b/index.markdown` (`index.md` or
    /// `index.markdown` for `/` alone), byte for byte.
    fn rooted(&self, rooted: &str) -> Option<usize> {
        let base = rooted
            .strip_suffix('/')
            .or_else(|| rooted.strip_suffix(".md"))
            .or_else(|| rooted.strip_suffix(".markdown"))
            .unwrap_or(rooted);
        let base = normalize("", base)?;
        let candidates: Vec<String> = if base.is_empty() {
            vec!["index.md".into(), "index.markdown".into()]
        } else {
            ["", "/index"]
                .iter()
                .flat_map(|mid| [".md", ".markdown"].map(|ext| format!("{base}{mid}{ext}")))
                .collect()
        };
        let mut found = candidates
            .iter()
            .filter_map(|c| self.by_path.get(c.as_str()).copied());
        match (found.next(), found.next()) {
            (Some(doc), None) => Some(doc),
            _ => None,
        }
    }
}

/// The stored form of source text: every `{{` that is not already escaped gets `\`, so it binds
/// to nothing (D-19BZD).
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    while let Some(found) = text[pos..].find("{{") {
        let at = pos + found;
        out.push_str(&text[pos..at]);
        if !text[..at].ends_with('\\') {
            out.push('\\');
        }
        out.push_str("{{");
        pos = at + 2;
    }
    out.push_str(&text[pos..]);
    out
}

/// `text` with CR LF and a lone CR as LF: the line breaks CommonMark reads.
fn lf(text: &str) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text.to_string()
    }
}

/// `text` on one line: each line break with the blanks around it as one space.
fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut skip = false;
    for c in lf(text).chars() {
        match c {
            '\n' => {
                out.truncate(out.trim_end_matches([' ', '\t']).len());
                out.push(' ');
                skip = true;
            }
            ' ' | '\t' if skip => {}
            c => {
                skip = false;
                out.push(c);
            }
        }
    }
    out
}

/// A link's display text in stored form; `None` when it cannot sit inside `{{id|…}}` without
/// changing what rdy reads (it holds `{{` or `}}`, or ends with `}` or `\`).
fn display_form(raw: &str) -> Option<Option<String>> {
    let d = one_line(raw);
    let d = d.trim();
    if d.is_empty() {
        return Some(None);
    }
    if d.contains("{{") || d.contains("}}") || d.ends_with(['}', '\\']) {
        return None;
    }
    Some(Some(if d.starts_with('@') {
        format!("\\{d}")
    } else {
        d.to_string()
    }))
}

/// How a text's source lines are kept: a block keeps its line breaks, each later line without
/// up to `indent` leading spaces (the first line's own indent); a title or cell is one line.
#[derive(Clone, Copy)]
enum Mode {
    Block { indent: usize },
    Inline,
}

enum Piece {
    Src(Range<usize>),
    Ref(Target, Option<String>),
    /// Source text already in stored form but for `{{` (an external link rewritten).
    Out(String),
}

/// A text under construction: source ranges and the references that replace links.
struct TextBuild {
    start: Option<usize>,
    cursor: usize,
    end: usize,
    pieces: Vec<Piece>,
}

impl TextBuild {
    /// A text whose source starts at the first inner event it is shown.
    fn open() -> TextBuild {
        TextBuild {
            start: None,
            cursor: 0,
            end: 0,
            pieces: vec![],
        }
    }

    /// A text whose source is `range`.
    fn over(range: &Range<usize>) -> TextBuild {
        TextBuild {
            start: Some(range.start),
            cursor: range.start,
            end: range.end,
            pieces: vec![],
        }
    }

    fn touch(&mut self, r: &Range<usize>) {
        if self.start.is_none() {
            self.start = Some(r.start);
            self.cursor = r.start;
        }
        self.end = self.end.max(r.end);
    }

    /// Replaces the link at `range` with a reference; false (the link stays as source text) when
    /// the reference would not read back: right after `{` or `\`, or with a display it cannot
    /// hold.
    fn link(&mut self, text: &str, range: &Range<usize>, target: Target, display: &str) -> bool {
        if self.start.is_none() || range.start < self.cursor {
            return false;
        }
        if text[self.cursor..range.start].ends_with(['{', '\\']) {
            return false;
        }
        let Some(display) = display_form(display) else {
            return false;
        };
        if range.start > self.cursor {
            self.pieces.push(Piece::Src(self.cursor..range.start));
        }
        self.pieces.push(Piece::Ref(target, display));
        self.cursor = range.end;
        true
    }

    /// Replaces the external link at `range` with `inline`, its `[text](target)` form; false
    /// (the link stays as source text) when the range is not ahead of the text built so far. A
    /// `!` right before becomes `\!`, so the link does not read as an image.
    fn rewrite(&mut self, text: &str, range: &Range<usize>, inline: &str) -> bool {
        if self.start.is_none() || range.start < self.cursor {
            return false;
        }
        let before = &text[self.cursor..range.start];
        let bang = before
            .strip_suffix('!')
            .is_some_and(|b| (b.len() - b.trim_end_matches('\\').len()) % 2 == 0);
        let keep = if bang { range.start - 1 } else { range.start };
        if keep > self.cursor {
            self.pieces.push(Piece::Src(self.cursor..keep));
        }
        let lead = if bang { "\\!" } else { "" };
        self.pieces.push(Piece::Out(format!("{lead}{inline}")));
        self.cursor = range.end;
        true
    }

    /// Writes a backslash before the `!` at `at`, which opens an image that rdy's own text now
    /// reads as a remote image (D-3DAX4), so the image stays literal text as D-19BZD keeps it;
    /// false (the text stays as written) when `at` is not ahead of the text built so far.
    fn escape_image(&mut self, at: usize) -> bool {
        if self.start.is_none() || at < self.cursor {
            return false;
        }
        if at > self.cursor {
            self.pieces.push(Piece::Src(self.cursor..at));
        }
        self.pieces.push(Piece::Out("\\".to_string()));
        self.cursor = at;
        true
    }

    fn finish(mut self, text: &str, mode: Mode) -> Text {
        if self.start.is_none() {
            return Text::default();
        }
        if self.end > self.cursor {
            self.pieces.push(Piece::Src(self.cursor..self.end));
        }
        let mut segments = vec![];
        let mut lit = String::new();
        let mut skip = 0usize;
        for p in self.pieces {
            match p {
                Piece::Src(r) => {
                    let src = &text[r];
                    match mode {
                        Mode::Inline => lit.push_str(&one_line(src)),
                        Mode::Block { indent } => {
                            for c in lf(src).chars() {
                                match c {
                                    '\n' => {
                                        lit.push('\n');
                                        skip = indent;
                                    }
                                    ' ' if skip > 0 => skip -= 1,
                                    c => {
                                        skip = 0;
                                        lit.push(c);
                                    }
                                }
                            }
                        }
                    }
                }
                Piece::Out(out) => {
                    lit.push_str(&out);
                    skip = 0;
                }
                Piece::Ref(target, display) => {
                    if !lit.is_empty() {
                        segments.push(Segment::Literal(escape(&lit)));
                        lit.clear();
                    }
                    skip = 0;
                    segments.push(Segment::Ref { target, display });
                }
            }
        }
        if !lit.is_empty() {
            segments.push(Segment::Literal(escape(&lit)));
        }
        Text { segments }
    }
}

/// `range` without its leading and trailing whitespace.
fn trimmed(text: &str, range: Range<usize>) -> Range<usize> {
    let s = &text[range.clone()];
    let lead = s.len() - s.trim_start().len();
    if lead == s.len() {
        return range.start..range.start;
    }
    let trail = s.len() - s.trim_end().len();
    range.start + lead..range.end - trail
}

/// The spaces before `at` on its line, when nothing else is there.
fn indent_of(text: &str, at: usize) -> usize {
    let line = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let before = &text[line..at];
    if before.bytes().all(|b| b == b' ') {
        before.len()
    } else {
        0
    }
}

/// A block's text, as written, over `range`.
fn block_text(text: &str, range: Range<usize>) -> (TextBuild, Mode) {
    let r = trimmed(text, range);
    let mode = Mode::Block {
        indent: indent_of(text, r.start),
    };
    (TextBuild::over(&r), mode)
}

/// Source lines of offsets, counted from the last offset asked (forward or back), so a walk in
/// source order counts each byte once.
struct Lines<'a> {
    text: &'a str,
    at: usize,
    line: usize,
}

impl Lines<'_> {
    fn of(&mut self, offset: usize) -> usize {
        let breaks = |r: Range<usize>| {
            self.text.as_bytes()[r]
                .iter()
                .filter(|&&b| b == b'\n')
                .count()
        };
        if offset >= self.at {
            self.line += breaks(self.at..offset);
        } else {
            self.line -= breaks(offset..self.at);
        }
        self.at = offset;
        self.line
    }
}

/// The `unmapped` rows: one per (doc, line, kind), bounded.
#[derive(Default)]
struct Rows(BTreeSet<(usize, usize, UnmappedKind)>);

impl Rows {
    fn add(&mut self, doc: usize, line: usize, kind: UnmappedKind) -> Result<(), RdyError> {
        let row = (doc, line, kind);
        if self.0.contains(&row) {
            return Ok(());
        }
        if self.0.len() >= MAX_UNMAPPED {
            return Err(limit_error(UNMAPPED_ROWS));
        }
        self.0.insert(row);
        Ok(())
    }
}

/// The top-level block being read.
enum Top {
    None,
    Heading {
        level: usize,
        title: TextBuild,
        range: Range<usize>,
    },
    Prose {
        text: TextBuild,
        mode: Mode,
    },
    Code {
        lang: Option<String>,
        body: String,
        range: Range<usize>,
    },
    Table(Box<Table>),
}

/// A table being read: its cells, and the same source as text in case a row has more cells than
/// the header (the parser would drop them).
struct Table {
    text: TextBuild,
    mode: Mode,
    columns: Vec<Text>,
    rows: Vec<Vec<Text>>,
    row: Vec<Text>,
    cell: Option<TextBuild>,
    row_line: usize,
    last_cell_end: usize,
    excess: bool,
}

/// `text` with each character rdy's inline subset reads as markup inside a link's text escaped:
/// an autolink's text is literal.
fn literal_words(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '`' | '*' | '[' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// An external link in the one form rdy's inline subset reads, `[text](target)`, with the
/// source's text (on one line; an autolink's URL, literal; the URL when empty) and no title.
/// `None` when rdy would not link the target anyway (a scheme other than `http`, `https` or
/// `mailto`, whitespace, a control character, a backslash, or a reference mark), so the source
/// text stays.
fn inline_link(l: &OpenLink, display: &str) -> Option<String> {
    let dest = l.dest.as_str();
    let head = &dest[..dest.find(['/', '?', '#']).unwrap_or(dest.len())];
    let scheme = head.split_once(':')?.0;
    let linked = ["http", "https", "mailto"]
        .iter()
        .any(|s| scheme.eq_ignore_ascii_case(s));
    if !linked
        || dest.contains("{{")
        || dest.contains("}}")
        || dest
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return None;
    }
    let words = one_line(display);
    let words = match (&l.shown, words.trim()) {
        (Some(shown), _) => literal_words(shown),
        (None, "") => literal_words(dest),
        (None, w) => w.to_string(),
    };
    let target = dest.replace('(', "\\(").replace(')', "\\)");
    Some(format!("[{words}]({target})"))
}

/// An open link: its source range and line, its destination, its definition (for a reference
/// link) and the range of its text.
struct OpenLink {
    range: Range<usize>,
    line: usize,
    dest: String,
    /// An autolink's text (`<https://…>`, `<a@b.c>`): what it shows, literal.
    shown: Option<String>,
    def: Option<usize>,
    inner: Option<Range<usize>>,
}

enum Draft {
    Block(PreparedBlock),
    /// Source between two top-level blocks: link reference definitions, kept as text unless
    /// every link that uses one became a reference.
    Gap {
        parent: Option<usize>,
        range: Range<usize>,
    },
}

/// A built doc: its blocks, its first section's title, and each section's block by ordinal.
struct Built {
    blocks: Vec<PreparedBlock>,
    first_title: Option<Text>,
    sections: Vec<usize>,
}

struct DocWalk<'s, 'a> {
    doc: usize,
    text: &'a str,
    count: &'s mut usize,
    rows: &'s mut Rows,
    lines: Lines<'a>,
    drafts: Vec<Draft>,
    /// Open sections: heading level and block index.
    stack: Vec<(usize, usize)>,
    sections: Vec<usize>,
    first_title: Option<Text>,
    defs: Vec<Range<usize>>,
    def_used: Vec<bool>,
    def_kept: Vec<bool>,
}

impl DocWalk<'_, '_> {
    fn parent(&self) -> Option<usize> {
        self.stack.last().map(|&(_, i)| i)
    }

    fn row(&mut self, offset: usize, kind: UnmappedKind) -> Result<(), RdyError> {
        let line = self.lines.of(offset);
        self.rows.add(self.doc, line, kind)
    }

    fn push(&mut self, parent: Option<usize>, content: PreparedContent) -> Result<usize, RdyError> {
        bump(self.count)?;
        self.drafts
            .push(Draft::Block(PreparedBlock { parent, content }));
        Ok(self.drafts.len() - 1)
    }

    fn prose(&mut self, body: Text) -> Result<(), RdyError> {
        let parent = self.parent();
        self.push(parent, PreparedContent::Prose { body })?;
        Ok(())
    }

    /// Text between top-level blocks becomes a gap block, settled when the doc ends.
    fn gap(&mut self, range: Range<usize>) {
        if range.start < range.end && !self.text[range.clone()].trim().is_empty() {
            let parent = self.parent();
            self.drafts.push(Draft::Gap { parent, range });
        }
    }

    fn begin(&self, tag: &Tag, range: &Range<usize>) -> Top {
        match tag {
            Tag::Heading { level, .. } => Top::Heading {
                level: *level as usize,
                title: TextBuild::open(),
                range: range.clone(),
            },
            Tag::CodeBlock(kind) => Top::Code {
                lang: match kind {
                    CodeBlockKind::Fenced(info) => info.split_whitespace().next().map(escape),
                    CodeBlockKind::Indented => None,
                },
                body: String::new(),
                range: range.clone(),
            },
            Tag::Table(_) => {
                let (text, mode) = block_text(self.text, range.clone());
                Top::Table(Box::new(Table {
                    text,
                    mode,
                    columns: vec![],
                    rows: vec![],
                    row: vec![],
                    cell: None,
                    row_line: 0,
                    last_cell_end: range.start,
                    excess: false,
                }))
            }
            // paragraphs, lists, block quotes, HTML and footnote definitions: running text
            _ => {
                let (text, mode) = block_text(self.text, range.clone());
                Top::Prose { text, mode }
            }
        }
    }

    fn finish(&mut self, top: Top) -> Result<(), RdyError> {
        match top {
            Top::None => {}
            Top::Heading {
                level,
                title,
                range,
            } => {
                if title.start.is_none() {
                    // a heading with no text is no section: its marks stay as text
                    let (text, mode) = block_text(self.text, range);
                    return self.prose(text.finish(self.text, mode));
                }
                let title = title.finish(self.text, Mode::Inline);
                while self.stack.last().is_some_and(|&(l, _)| l >= level) {
                    self.stack.pop();
                }
                let parent = self.parent();
                if self.first_title.is_none() {
                    self.first_title = Some(title.clone());
                }
                let at = self.push(parent, PreparedContent::Section { title })?;
                self.stack.push((level, at));
                self.sections.push(at);
            }
            Top::Prose { text, mode } => self.prose(text.finish(self.text, mode))?,
            Top::Code { lang, body, range } => {
                let body = lf(&body);
                let body = body.strip_suffix('\n').unwrap_or(&body);
                if body.is_empty() {
                    // code requires a body: an empty fence stays as text
                    let (text, mode) = block_text(self.text, range);
                    return self.prose(text.finish(self.text, mode));
                }
                let parent = self.parent();
                let body = body.to_string();
                self.push(parent, PreparedContent::Code { lang, body })?;
            }
            Top::Table(t) => {
                let t = *t;
                if t.excess {
                    return self.prose(t.text.finish(self.text, t.mode));
                }
                let parent = self.parent();
                let (columns, rows) = (t.columns, t.rows);
                self.push(parent, PreparedContent::Table { columns, rows })?;
            }
        }
        Ok(())
    }

    /// Settles each gap: a definition every use of which became a reference is dropped; the rest
    /// stays as text. Then numbers the blocks without the empty gaps.
    fn settle(mut self) -> Result<Built, RdyError> {
        let mut index = vec![usize::MAX; self.drafts.len()];
        let mut blocks: Vec<PreparedBlock> = Vec::with_capacity(self.drafts.len());
        let drafts = std::mem::take(&mut self.drafts);
        for (i, d) in drafts.into_iter().enumerate() {
            let block = match d {
                Draft::Block(b) => b,
                Draft::Gap { parent, range } => {
                    let mut kept = String::new();
                    let mut at = range.start;
                    for (k, def) in self.defs.iter().enumerate() {
                        let inside = def.start >= at && def.end <= range.end;
                        if inside && self.def_used[k] && !self.def_kept[k] {
                            kept.push_str(&self.text[at..def.start]);
                            kept.push('\n');
                            at = def.end;
                        }
                    }
                    kept.push_str(&self.text[at..range.end]);
                    let lines: Vec<&str> = kept
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .collect();
                    if lines.is_empty() {
                        continue;
                    }
                    bump(self.count)?;
                    let body = Text::literal(escape(&lines.join("\n")));
                    PreparedBlock {
                        parent,
                        content: PreparedContent::Prose { body },
                    }
                }
            };
            index[i] = blocks.len();
            blocks.push(block);
        }
        // a gap is never a parent: every parent is a section, which is always kept
        for b in &mut blocks {
            b.parent = b.parent.map(|p| index[p]);
        }
        Ok(Built {
            blocks,
            first_title: self.first_title,
            sections: self.sections.iter().map(|&s| index[s]).collect(),
        })
    }
}

/// The second walk: builds one doc's blocks and rows. References name sections by ordinal.
fn build(
    links: &Links,
    doc: usize,
    src: &Source,
    count: &mut usize,
    rows: &mut Rows,
) -> Result<Built, RdyError> {
    let text = src.text;
    let off = src.body;
    let mut it = Parser::new_ext(&text[off..], options()).into_offset_iter();
    let mut defs: Vec<Range<usize>> = it
        .reference_definitions()
        .iter()
        .map(|(_, d)| d.span.start + off..d.span.end + off)
        .collect();
    defs.sort_by_key(|r| r.start);
    let def_at: BTreeMap<usize, usize> =
        defs.iter().enumerate().map(|(i, r)| (r.start, i)).collect();
    let mut w = DocWalk {
        doc,
        text,
        count,
        rows,
        lines: Lines {
            text,
            at: 0,
            line: 1,
        },
        drafts: vec![],
        stack: vec![],
        sections: vec![],
        first_title: None,
        def_used: vec![false; defs.len()],
        def_kept: vec![false; defs.len()],
        defs,
    };
    if let FrontMatter::Literal { end, lines } = &src.front {
        for &line in lines {
            w.rows.add(doc, line, UnmappedKind::FrontMatter)?;
        }
        let body = lf(text[..*end].trim_end());
        let lang = Some("yaml".to_string());
        w.push(None, PreparedContent::Code { lang, body })?;
    }
    let mut cursor = off;
    let mut depth = 0usize;
    let mut images = 0usize;
    let mut html_blocks = 0usize;
    let mut top = Top::None;
    let mut link: Option<OpenLink> = None;
    while let Some((ev, r)) = it.next() {
        let r = r.start + off..r.end + off;
        match &ev {
            Event::Start(tag) if depth == 0 => {
                depth = 1;
                w.gap(cursor..r.start);
                match tag {
                    Tag::HtmlBlock => {
                        html_blocks += 1;
                        w.row(r.start, UnmappedKind::Html)?;
                    }
                    Tag::BlockQuote(Some(_)) | Tag::FootnoteDefinition(_) => {
                        w.row(r.start, UnmappedKind::Extension)?;
                    }
                    _ => {}
                }
                top = w.begin(tag, &r);
                continue;
            }
            Event::End(end) if depth == 1 => {
                depth = 0;
                if matches!(end, TagEnd::HtmlBlock) {
                    html_blocks -= 1;
                }
                let done = std::mem::replace(&mut top, Top::None);
                w.finish(done)?;
                cursor = r.end;
                continue;
            }
            _ if depth == 0 => {
                // a thematic break, or any other event outside a block: kept as text
                w.gap(cursor..r.start);
                let (t, mode) = block_text(text, r.clone());
                w.prose(t.finish(text, mode))?;
                cursor = r.end;
                continue;
            }
            Event::Start(_) => depth += 1,
            Event::End(_) => depth -= 1,
            _ => {}
        }
        // an event inside a top-level block: first what it reports
        match &ev {
            Event::Start(Tag::Image { .. }) => {
                images += 1;
                w.row(r.start, UnmappedKind::Image)?;
            }
            Event::End(TagEnd::Image) => images -= 1,
            Event::Start(Tag::HtmlBlock) => {
                html_blocks += 1;
                w.row(r.start, UnmappedKind::Html)?;
            }
            Event::End(TagEnd::HtmlBlock) => html_blocks -= 1,
            Event::Html(_) if html_blocks == 0 => w.row(r.start, UnmappedKind::Html)?,
            Event::InlineHtml(_) => w.row(r.start, UnmappedKind::Html)?,
            Event::Start(
                Tag::Strikethrough | Tag::FootnoteDefinition(_) | Tag::BlockQuote(Some(_)),
            )
            | Event::TaskListMarker(_)
            | Event::FootnoteReference(_) => w.row(r.start, UnmappedKind::Extension)?,
            _ => {}
        }
        // a link's text spans the events between its start and its end
        let closes_link = matches!(ev, Event::End(TagEnd::Link)) && images == 0 && link.is_some();
        if let Some(l) = &mut link
            && !closes_link
        {
            let inner = l.inner.get_or_insert(r.clone());
            inner.end = inner.end.max(r.end);
        }
        // then what it adds to the block
        match &mut top {
            Top::Heading { title, .. } => title.touch(&r),
            Top::Code { body, .. } => {
                if let Event::Text(t) = &ev {
                    body.push_str(t);
                }
            }
            Top::Table(t) => match &ev {
                Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow) => {
                    t.row.clear();
                    t.row_line = w.lines.of(r.start);
                }
                Event::End(TagEnd::TableHead) => t.columns = std::mem::take(&mut t.row),
                Event::End(TagEnd::TableRow) => {
                    // the parser drops cells past the header's count: keep the table as text
                    let rest = text[t.last_cell_end.min(r.end)..r.end].trim();
                    if !rest.trim_start_matches('|').trim().is_empty() {
                        t.excess = true;
                        w.rows.add(doc, t.row_line, UnmappedKind::Extension)?;
                    }
                    t.rows.push(std::mem::take(&mut t.row));
                }
                Event::Start(Tag::TableCell) => {
                    t.cell = Some(TextBuild::open());
                    t.last_cell_end = r.end;
                }
                Event::End(TagEnd::TableCell) => {
                    if let Some(c) = t.cell.take() {
                        t.row.push(c.finish(text, Mode::Inline));
                    }
                }
                _ => {
                    if let Some(c) = &mut t.cell {
                        c.touch(&r);
                    }
                }
            },
            Top::Prose { .. } | Top::None => {}
        }
        // an image rdy's text reads as a remote image (D-3DAX4): `![alt](https:…)` written
        // inline. Its `!` gets a backslash, so it stays literal text with its `image` row
        // (D-19BZD); rdy imports and fetches no image
        if let Event::Start(Tag::Image {
            link_type: LinkType::Inline,
            dest_url,
            ..
        }) = &ev
            && text[r.start..].starts_with("![")
            && dest_url
                .get(..6)
                .is_some_and(|s| s.eq_ignore_ascii_case("https:"))
        {
            let escape = |b: &mut TextBuild| {
                b.escape_image(r.start);
            };
            match &mut top {
                Top::Heading { title, .. } => escape(title),
                Top::Prose { text, .. } => escape(text),
                Top::Table(t) => {
                    escape(&mut t.text);
                    if let Some(c) = &mut t.cell {
                        escape(c);
                    }
                }
                Top::Code { .. } | Top::None => {}
            }
        }
        // and the links it opens or closes
        match &ev {
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                id,
                ..
            }) if images == 0 && link.is_none() => {
                let shown = matches!(link_type, LinkType::Autolink | LinkType::Email)
                    .then(|| dest_url.to_string());
                let def = match link_type {
                    LinkType::Reference | LinkType::Collapsed | LinkType::Shortcut => it
                        .reference_definitions()
                        .get(id.as_ref())
                        .and_then(|d| def_at.get(&(d.span.start + off)).copied()),
                    _ => None,
                };
                // the parser's range of a collapsed reference (`[text][]`) stops before its `[]`
                let mut range = r.clone();
                if matches!(link_type, LinkType::Collapsed) && text[range.end..].starts_with("[]") {
                    range.end += 2;
                }
                link = Some(OpenLink {
                    range,
                    line: w.lines.of(r.start),
                    // an email autolink names its address alone
                    dest: if matches!(link_type, LinkType::Email) {
                        format!("mailto:{dest_url}")
                    } else {
                        dest_url.to_string()
                    },
                    shown,
                    def,
                    inner: None,
                });
            }
            _ if closes_link => {
                let l = link.take().expect("an open link closes");
                let display = l.inner.clone().map_or("", |i| &text[i]);
                let mut converted = false;
                let mut broken = false;
                match links.resolve(doc, &l.dest) {
                    Resolved::External => {
                        // rdy reads `[text](target)` alone: every other form is rewritten to it
                        if let Some(inline) = inline_link(&l, display) {
                            converted = true;
                            if text[l.range.clone()] != inline {
                                let mut apply = |b: &mut TextBuild| {
                                    converted &= b.rewrite(text, &l.range, &inline);
                                };
                                match &mut top {
                                    Top::Heading { title, .. } => apply(title),
                                    Top::Prose { text, .. } => apply(text),
                                    Top::Table(t) => {
                                        apply(&mut t.text);
                                        if let Some(c) = &mut t.cell {
                                            apply(c);
                                        }
                                    }
                                    Top::Code { .. } | Top::None => {}
                                }
                            }
                        }
                    }
                    Resolved::Broken => broken = true,
                    Resolved::Local(target) => {
                        converted = true;
                        let mut apply = |b: &mut TextBuild| {
                            if !b.link(text, &l.range, target, display) {
                                converted = false;
                                broken = true;
                            }
                        };
                        match &mut top {
                            Top::Heading { title, .. } => apply(title),
                            Top::Prose { text, .. } => apply(text),
                            Top::Table(t) => {
                                apply(&mut t.text);
                                if let Some(c) = &mut t.cell {
                                    apply(c);
                                }
                            }
                            Top::Code { .. } | Top::None => {}
                        }
                    }
                }
                if broken {
                    w.rows.add(doc, l.line, UnmappedKind::Link)?;
                }
                if let Some(d) = l.def {
                    w.def_used[d] = true;
                    w.def_kept[d] |= !converted;
                }
            }
            _ => {}
        }
    }
    w.gap(cursor..text.len());
    w.settle()
}

/// Rewrites each reference's section ordinal as the section's block index.
fn number_targets(text: &mut Text, sections: &[Vec<usize>]) {
    for s in &mut text.segments {
        if let Segment::Ref { target, .. } = s
            && let Some(o) = target.block
        {
            target.block = Some(sections[target.doc][o]);
        }
    }
}

pub(super) fn convert(
    pack: &Pack,
    dirs: &[String],
    files: &[MarkdownFile],
) -> Result<Prepared, RdyError> {
    let inv = inventory(dirs, files)?;
    let mut count = 0usize;
    for _ in 0..inv.folders.len() + inv.docs.len() {
        bump(&mut count)?;
    }
    let sources: Vec<Source> = inv
        .docs
        .iter()
        .map(|d| source(d.path, &files[d.file].bytes, pack))
        .collect::<Result<_, _>>()?;
    let mut planned = count;
    let anchors: Vec<Anchors> = sources
        .iter()
        .map(|s| anchors(s, &mut planned))
        .collect::<Result<_, _>>()?;
    let links = Links {
        by_path: inv
            .docs
            .iter()
            .enumerate()
            .map(|(i, d)| (d.path, i))
            .collect(),
        paths: inv.docs.iter().map(|d| d.path).collect(),
        anchors,
    };
    let mut rows = Rows::default();
    let mut built = Vec::with_capacity(sources.len());
    for (i, src) in sources.iter().enumerate() {
        let b = build(&links, i, src, &mut count, &mut rows)?;
        debug_assert_eq!(b.sections.len(), links.anchors[i].count);
        built.push(b);
    }
    let sections: Vec<Vec<usize>> = built.iter().map(|b| b.sections.clone()).collect();
    let mut docs = Vec::with_capacity(built.len());
    for ((shell, src), b) in inv.docs.iter().zip(&sources).zip(built) {
        let (fm_title, fm_type) = match &src.front {
            FrontMatter::Valid { title, doc_type } => (title.clone(), doc_type.clone()),
            _ => (None, None),
        };
        let leaf = src.path.rsplit('/').next().unwrap_or(src.path);
        let title = match fm_title {
            Some(t) => Text::literal(escape(&one_line(&t))),
            None => b
                .first_title
                .unwrap_or_else(|| Text::literal(escape(stem(leaf)))),
        };
        let doc_type = match fm_type {
            Some(t) => t,
            None => crate::paths::default_doc_type(pack)?,
        };
        docs.push(PreparedDoc {
            path: shell.path.to_string(),
            parent: shell.parent,
            name: shell.name.clone(),
            title,
            doc_type,
            blocks: b.blocks,
        });
    }
    for d in &mut docs {
        number_targets(&mut d.title, &sections);
        for b in &mut d.blocks {
            match &mut b.content {
                PreparedContent::Section { title } => number_targets(title, &sections),
                PreparedContent::Prose { body } => number_targets(body, &sections),
                PreparedContent::Table { columns, rows } => {
                    for t in columns.iter_mut().chain(rows.iter_mut().flatten()) {
                        number_targets(t, &sections);
                    }
                }
                PreparedContent::Code { .. } => {}
            }
        }
    }
    let unmapped = rows
        .0
        .into_iter()
        .map(|(doc, line, kind)| Unmapped {
            path: docs[doc].path.clone(),
            line,
            kind,
        })
        .collect();
    Ok(Prepared {
        folders: inv.folders,
        docs,
        unmapped,
    })
}
