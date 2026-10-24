//! The flowchart depth bound: one forward walk over the pinned grammar's structure.
//!
//! The pinned crate first drops the lines its flowchart grammar cannot start (see
//! [`preprocess`]), then parses the rest with a PEG grammar. The walk reads that same text the
//! way the grammar's first attempt reads it: statements, comments, node shapes, edge labels and
//! subgraph headers, each with the grammar's own terminators. So a keyword in a comment or a
//! label is text, a subgraph can open after a node on the same line, and subgraphs in a row
//! never add up. It keeps no parse tree: only a position, a depth and the deepest depth.
//!
//! When the walk meets text that the grammar's first attempt cannot read, the pinned crate
//! backtracks and can read earlier text another way. The walk does not follow that. So then,
//! and when a subgraph is left open whose header reads differently as plain nodes, the bound is
//! the larger of the walk's depth and the keyword bound ([`keyword_bound`]). No reading nests
//! deeper than the keyword bound.

use std::ops::Range;

/// The deepest subgraph nesting the pinned flowchart grammar can read in `src`, the diagram's
/// source with any BOM removed and CRLF as LF.
pub(crate) fn depth(src: &str) -> usize {
    walk(src).0
}

/// The bound, and whether the walk read the whole source with every subgraph closed: then the
/// bound is the depth the grammar reads.
pub(crate) fn walk(src: &str) -> (usize, bool) {
    let text = preprocess(crate::after_front_matter(src));
    let mut walk = Walk::new(&text);
    let read = walk.flowchart();
    let exact = read.is_ok() && walk.depth == 0;
    // A subgraph left open is read again as plain nodes, which nests no deeper unless its
    // header reads differently as nodes.
    if read.is_ok() && !(walk.depth > 0 && walk.reread) {
        (walk.deepest, exact)
    } else {
        (walk.deepest.max(keyword_bound(text.as_bytes())), exact)
    }
}

/// The text the pinned crate's flowchart grammar parses: the lines after front matter, from
/// the header on, without `%%{...}%%` directives and without lines the grammar cannot start. A
/// kept line starts with a letter, digit, `_` or `;`, or is blank, a `%%` comment or a keyword
/// line. Kept lines are joined with LF.
fn preprocess(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut header = false;
    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("%%{") && trimmed.ends_with("}%%") {
            continue;
        }
        if !header {
            let first = trimmed.split_whitespace().next().unwrap_or("");
            if first.eq_ignore_ascii_case("graph") || first.eq_ignore_ascii_case("flowchart") {
                header = true;
                push_line(&mut out, line);
            }
            continue;
        }
        if trimmed.starts_with("%%") || trimmed.is_empty() || kept_keyword_line(trimmed) {
            push_line(&mut out, line);
            continue;
        }
        let lower = |n: usize| trimmed.as_bytes().get(..n).map(<[u8]>::to_ascii_lowercase);
        if lower(8).as_deref() == Some(b"acctitle") || lower(8).as_deref() == Some(b"accdescr") {
            continue;
        }
        let first = trimmed.chars().next().unwrap_or(' ');
        if first.is_alphanumeric() || first == '_' || first == ';' {
            push_line(&mut out, line);
        }
    }
    if input.ends_with('\n') && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn push_line(out: &mut String, line: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(line);
}

/// A line the pinned crate keeps for its keyword, in any case.
fn kept_keyword_line(line: &str) -> bool {
    let starts = |prefix: &str| {
        line.as_bytes()
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_bytes()))
    };
    [
        "style ",
        "classdef ",
        "class ",
        "click ",
        "linkstyle ",
        "direction ",
        "subgraph ",
        "end ",
        "end;",
    ]
    .into_iter()
    .any(starts)
        || line.eq_ignore_ascii_case("subgraph")
        || line.eq_ignore_ascii_case("end")
}

/// An upper bound on the subgraph depth of any reading of `bytes`: the most `n` for which `n`
/// subgraph keywords come before some point and `n` end keywords after it.
///
/// The grammar opens each subgraph with the keyword `subgraph` and needs an `end` keyword to
/// close it, both in any case and with no word boundary after. A keyword can follow any byte
/// but an ASCII letter, digit or `_`, which would join it to an identifier. This bound reads
/// no labels, strings or comments, so it holds for every way the grammar can read the text.
pub(crate) fn keyword_bound(bytes: &[u8]) -> usize {
    let mut closers = 0;
    keywords(bytes, |opener| {
        if !opener {
            closers += 1;
        }
    });
    let (mut openers, mut bound) = (0usize, 0usize);
    keywords(bytes, |opener| {
        if opener {
            openers += 1;
            bound = bound.max(openers.min(closers));
        } else {
            closers -= 1;
        }
    });
    bound
}

/// Calls `found(true)` for each `subgraph` keyword and `found(false)` for each `end` keyword,
/// in order.
fn keywords(bytes: &[u8], mut found: impl FnMut(bool)) {
    for i in 0..bytes.len() {
        if i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
            continue;
        }
        let rest = &bytes[i..];
        if rest.len() >= 8 && rest[..8].eq_ignore_ascii_case(b"subgraph") {
            found(true);
        } else if rest.len() >= 3 && rest[..3].eq_ignore_ascii_case(b"end") {
            found(false);
        }
    }
}

/// The text the grammar's first attempt cannot read here.
struct Stuck;

/// The terminators the walk searches for. `Walk::at` holds where each one starts.
const TERMINATORS: [&[u8]; 12] = [
    b")", b"))", b"])", b"]]", b")]", b"}}", b"\\]", b"/]", b"]", b"}", b"|", b"\"",
];

/// A node shape: its opener, the text that ends its label, and its closer at that point.
struct Shape {
    open: &'static [u8],
    stop: &'static [u8],
    close: &'static [u8],
}

/// The grammar's node shapes, in its order. A rectangle (`[`) is read on its own, because its
/// label can hold quoted text.
const SHAPES: [Shape; 9] = [
    Shape {
        open: b"(((",
        stop: b")",
        close: b")))",
    },
    Shape {
        open: b"((",
        stop: b"))",
        close: b"))",
    },
    Shape {
        open: b"([",
        stop: b"])",
        close: b"])",
    },
    Shape {
        open: b"[[",
        stop: b"]]",
        close: b"]]",
    },
    Shape {
        open: b"[(",
        stop: b")]",
        close: b")]",
    },
    Shape {
        open: b"{{",
        stop: b"}}",
        close: b"}}",
    },
    Shape {
        open: b"[/",
        stop: b"\\]",
        close: b"\\]",
    },
    Shape {
        open: b"[\\",
        stop: b"/]",
        close: b"/]",
    },
    Shape {
        open: b">",
        stop: b"]",
        close: b"]",
    },
];
const ROUND: Shape = Shape {
    open: b"(",
    stop: b")",
    close: b")",
};
const DIAMOND: Shape = Shape {
    open: b"{",
    stop: b"}",
    close: b"}",
};

/// The three link kinds.
#[derive(Clone, Copy)]
enum Link {
    Dotted,
    Thick,
    Solid,
}

struct Walk<'a> {
    text: &'a str,
    s: &'a [u8],
    i: usize,
    depth: usize,
    deepest: usize,
    /// Whether a subgraph header the walk opened reads differently as plain nodes, should the
    /// grammar read it again that way.
    reread: bool,
    /// Whether a `"` in a rectangle label can still open a quoted chunk: once one finds no
    /// close, no later one can.
    chunks: bool,
    /// For each terminator, the positions it starts at, in order.
    at: Vec<Vec<u32>>,
}

impl<'a> Walk<'a> {
    fn new(text: &'a str) -> Self {
        let s = text.as_bytes();
        let mut at = vec![Vec::new(); TERMINATORS.len()];
        for i in 0..s.len() {
            for (k, t) in TERMINATORS.iter().enumerate() {
                if s[i..].starts_with(t) {
                    // the source is at most 65536 bytes
                    at[k].push(i as u32);
                }
            }
        }
        Walk {
            text,
            s,
            i: 0,
            depth: 0,
            deepest: 0,
            reread: false,
            chunks: true,
            at,
        }
    }

    /// `flowchart = SOI ~ header ~ separator ~ ((statement | comment) ~ separator?)* ~ EOI`,
    /// with each `subgraph_stmt` read without recursion: its body is the same loop one level
    /// deeper, and it ends at a body line that is `end` before a line end, `;` or the end.
    fn flowchart(&mut self) -> Result<(), Stuck> {
        self.ws();
        if !(self.word(b"graph") || self.word(b"flowchart")) {
            return Err(Stuck);
        }
        self.ws();
        for d in [b"TD", b"TB", b"BT", b"LR", b"RL"] {
            if self.word(d) {
                break;
            }
        }
        self.ws();
        if !self.separators() {
            return Err(Stuck);
        }
        loop {
            self.ws();
            if self.i == self.s.len() {
                return Ok(());
            }
            if self.depth > 0 && self.closes() {
                self.i += 3;
                self.depth -= 1;
            } else if self.at_bytes(b"%%") {
                self.i = self.line_end(self.i);
            } else if self.subgraph_header() {
                self.depth += 1;
                self.deepest = self.deepest.max(self.depth);
                continue;
            } else if !self.statement() {
                return Err(Stuck);
            }
            self.ws();
            self.separators();
        }
    }

    /// Whether a body line here is the `end` that closes its subgraph.
    fn closes(&self) -> bool {
        if !self.s[self.i..]
            .get(..3)
            .is_some_and(|w| w.eq_ignore_ascii_case(b"end"))
        {
            return false;
        }
        let mut j = self.i + 3;
        while matches!(self.s.get(j), Some(b' ' | b'\t')) {
            j += 1;
        }
        matches!(self.s.get(j), None | Some(b'\n' | b'\r' | b';'))
    }

    /// A subgraph header that opens a body: `subgraph`, its spec, then one line end or `;`, and
    /// a body line next. The grammar reads a header with a separator or the end right after it
    /// as plain nodes, because its body needs a line before `end`. Otherwise nothing moves.
    fn subgraph_header(&mut self) -> bool {
        let start = self.i;
        if !self.word(b"subgraph") {
            return false;
        }
        self.ws();
        let reread = match self.subgraph_spec() {
            Some(reread) => reread,
            None => {
                self.i = start;
                return false;
            }
        };
        self.ws();
        if self.newline().is_none() && !self.byte(b';') {
            self.i = start;
            return false;
        }
        let body = self.i;
        self.ws();
        if self.i == self.s.len() || self.newline().is_some() || self.at_bytes(b";") {
            self.i = start;
            return false;
        }
        self.i = body;
        self.reread |= reread;
        true
    }

    /// `subgraph_spec`: an id with a `[title]`, an id with a `"title"`, a `"title"`, or an id.
    /// Some means it matched; its flag says whether plain nodes read it differently: a title
    /// that a node shape or a quoted chunk would end elsewhere, or an id that a node id would
    /// end early.
    fn subgraph_spec(&mut self) -> Option<bool> {
        let start = self.i;
        if let Some(id) = self.subgraph_id() {
            let node_id_differs = self.s[id.clone()]
                .windows(2)
                .any(|w| w[0] == b'-' && matches!(w[1], b'-' | b'>' | b'.'));
            let after_id = self.i;
            self.ws();
            if self.byte(b'[') {
                let shape_differs = matches!(self.s.get(self.i), Some(b'[' | b'(' | b'/' | b'\\'));
                self.ws();
                if let Some(end) = self.label_end(self.i, b"]") {
                    let quoted = self.s[self.i..end].contains(&b'"');
                    self.i = end + 1;
                    return Some(node_id_differs || shape_differs || quoted);
                }
            }
            self.i = after_id;
            self.ws();
            if self.quoted_title() {
                return Some(node_id_differs);
            }
            self.i = after_id;
            return Some(node_id_differs);
        }
        self.i = start;
        self.quoted_title().then_some(false)
    }

    /// `subgraph_id = @{ (LETTER | NUMBER | "_") ~ (LETTER | NUMBER | "_" | "-" | ".")* }`.
    fn subgraph_id(&mut self) -> Option<Range<usize>> {
        let start = self.i;
        let first = self.char_here()?;
        if !(is_letter_or_number(first) || first == '_') {
            return None;
        }
        self.i += first.len_utf8();
        while let Some(c) = self.char_here() {
            if !(is_letter_or_number(c) || matches!(c, '_' | '-' | '.')) {
                break;
            }
            self.i += c.len_utf8();
        }
        Some(start..self.i)
    }

    /// `"` ~ text without `"` ~ `"`, the text not empty.
    fn quoted_title(&mut self) -> bool {
        let start = self.i;
        if !self.byte(b'"') {
            return false;
        }
        self.ws();
        match self.label_end(self.i, b"\"") {
            Some(end) => {
                self.i = end + 1;
                true
            }
            None => {
                self.i = start;
                false
            }
        }
    }

    /// One statement other than a subgraph that opens: a keyword line or a vertex statement.
    fn statement(&mut self) -> bool {
        for kw in [&b"style"[..], b"classDef", b"class", b"click", b"linkStyle"] {
            if self.keyword_at(kw) {
                self.i += kw.len();
                while self.i < self.s.len() && !matches!(self.s[self.i], b'\n' | b'\r' | b';') {
                    self.i += 1;
                }
                return true;
            }
        }
        if self.keyword_at(b"direction") {
            let start = self.i;
            self.i += b"direction".len();
            self.ws();
            if [b"TD", b"TB", b"BT", b"LR", b"RL"]
                .iter()
                .any(|d| self.word(*d))
            {
                return true;
            }
            self.i = start;
        }
        self.vertex_statement()
    }

    /// A keyword with no letter, digit or `_` after it, in any case. Nothing moves.
    fn keyword_at(&self, kw: &[u8]) -> bool {
        let Some(head) = self.s[self.i..].get(..kw.len()) else {
            return false;
        };
        head.eq_ignore_ascii_case(kw)
            && !self.text[self.i + kw.len()..]
                .chars()
                .next()
                .is_some_and(|c| is_letter_or_number(c) || c == '_')
    }

    /// `vertex_statement = node_group ~ edge_segment*`.
    fn vertex_statement(&mut self) -> bool {
        if !self.node_group() {
            return false;
        }
        loop {
            let before = self.i;
            self.ws();
            if !(self.edge_connector() && {
                self.ws();
                self.node_group()
            }) {
                self.i = before;
                return true;
            }
        }
    }

    /// `node_group = node ~ ("&" ~ node)*`.
    fn node_group(&mut self) -> bool {
        if !self.node() {
            return false;
        }
        loop {
            let before = self.i;
            self.ws();
            if !(self.byte(b'&') && {
                self.ws();
                self.node()
            }) {
                self.i = before;
                return true;
            }
        }
    }

    /// `node = identifier ~ shape? ~ class_annotation?`.
    fn node(&mut self) -> bool {
        if !self.identifier() {
            return false;
        }
        let before = self.i;
        self.ws();
        if !self.shape() {
            self.i = before;
        }
        let before = self.i;
        self.ws();
        if !(self.bytes(b":::") && {
            self.ws();
            self.class_name()
        }) {
            self.i = before;
        }
        true
    }

    /// `identifier = @{ (LETTER | NUMBER | "_") ~ (LETTER | NUMBER | "_" | "." | ("-" ~ !("-" |
    /// ">" | ".")))* }`.
    fn identifier(&mut self) -> bool {
        match self.char_here() {
            Some(c) if is_letter_or_number(c) || c == '_' => self.i += c.len_utf8(),
            _ => return false,
        }
        while let Some(c) = self.char_here() {
            let ok = is_letter_or_number(c)
                || matches!(c, '_' | '.')
                || (c == '-' && !matches!(self.s.get(self.i + 1), Some(b'-' | b'>' | b'.')));
            if !ok {
                break;
            }
            self.i += c.len_utf8();
        }
        true
    }

    /// `class_name = @{ (LETTER | NUMBER | "_" | "-")+ }`.
    fn class_name(&mut self) -> bool {
        let start = self.i;
        while let Some(c) = self.char_here() {
            if !(is_letter_or_number(c) || matches!(c, '_' | '-')) {
                break;
            }
            self.i += c.len_utf8();
        }
        self.i > start
    }

    /// `shape`: the first of the grammar's shapes that matches. A label runs to the first
    /// place its terminator starts, across lines, and is not empty.
    fn shape(&mut self) -> bool {
        for shape in &SHAPES {
            if self.labelled(shape) {
                return true;
            }
        }
        if self.rectangle() || self.labelled(&ROUND) || self.labelled(&DIAMOND) {
            return true;
        }
        // `@{` ~ any text without `}` ~ `}`
        let start = self.i;
        if self.bytes(b"@{") {
            self.ws();
            if let Some(end) = self.next_at(b"}", self.i) {
                self.i = end + 1;
                return true;
            }
        }
        self.i = start;
        false
    }

    /// One shape whose label is text up to its stop and whose closer must start there.
    fn labelled(&mut self, shape: &Shape) -> bool {
        let start = self.i;
        if !self.bytes(shape.open) {
            return false;
        }
        self.ws();
        if let Some(end) = self.label_end(self.i, shape.stop)
            && self.s[end..].starts_with(shape.close)
        {
            self.i = end + shape.close.len();
            return true;
        }
        self.i = start;
        false
    }

    /// `"[" ~ text_rect ~ "]"`: the label runs to the first `]` outside quoted chunks. A chunk
    /// is `"` to the next `"` that is not `\"`; a `"` with no close is text.
    fn rectangle(&mut self) -> bool {
        let start = self.i;
        if !self.byte(b'[') {
            return false;
        }
        self.ws();
        let mut j = self.i;
        while j < self.s.len() {
            match self.s[j] {
                b']' if j > self.i => {
                    self.i = j + 1;
                    return true;
                }
                b']' => break,
                b'"' if self.chunks => match crate::close_quote(self.s, j + 1, false) {
                    Some(close) => j = close + 1,
                    None => {
                        self.chunks = false;
                        j += 1;
                    }
                },
                _ => j += 1,
            }
        }
        self.i = start;
        false
    }

    /// `edge_connector`: a labelled dotted, thick or solid link, else an invisible, dotted,
    /// thick or solid link with an optional `|label|`.
    fn edge_connector(&mut self) -> bool {
        let start = self.i;
        for link in [Link::Dotted, Link::Thick, Link::Solid] {
            if self.labelled_link(link) {
                return true;
            }
            self.i = start;
        }
        if !(self.invisible_link()
            || {
                self.i = start;
                self.plain_link(Link::Dotted)
            }
            || {
                self.i = start;
                self.plain_link(Link::Thick)
            }
            || {
                self.i = start;
                self.plain_link(Link::Solid)
            })
        {
            self.i = start;
            return false;
        }
        let before = self.i;
        self.ws();
        if !self.edge_label() {
            self.i = before;
        }
        true
    }

    /// A labelled link: its start (atomic), then a label on the same line up to the first
    /// place the link's end starts, then the end.
    fn labelled_link(&mut self, link: Link) -> bool {
        self.arrow_left();
        let (first, second, refused): (u8, u8, &[u8]) = match link {
            Link::Dotted => (b'-', b'.', b".->xo"),
            Link::Thick => (b'=', b'=', b"=>xo"),
            Link::Solid => (b'-', b'-', b"->xo"),
        };
        if !(self.byte(first) && self.byte(second)) {
            return false;
        }
        if self.s.get(self.i).is_some_and(|b| refused.contains(b)) {
            return false;
        }
        self.ws();
        let text = self.i;
        let mut j = text;
        while j < self.s.len() && !matches!(self.s[j], b'\n' | b'\r') {
            if self.link_end_at(link, j) {
                break;
            }
            j += 1;
        }
        if j == text || j == self.s.len() || matches!(self.s[j], b'\n' | b'\r') {
            return false;
        }
        self.i = j;
        self.link_end(link)
    }

    /// Whether a link's end starts at `j`, read with no spaces: `-`? `.`+ `-` for dotted,
    /// `=` `=`+ for thick, `-` `-`+ for solid.
    fn link_end_at(&self, link: Link, j: usize) -> bool {
        let s = self.s;
        match link {
            Link::Dotted => {
                let mut k = j;
                if s.get(k) == Some(&b'-') {
                    k += 1;
                }
                let dots = k;
                while s.get(k) == Some(&b'.') {
                    k += 1;
                }
                k > dots && s.get(k) == Some(&b'-')
            }
            Link::Thick => s[j..].starts_with(b"=="),
            Link::Solid => s[j..].starts_with(b"--"),
        }
    }

    /// A link's end, with the grammar's spaces between its parts and an optional arrow head.
    fn link_end(&mut self, link: Link) -> bool {
        match link {
            Link::Dotted => {
                self.byte(b'-');
                self.ws();
                if !self.run(b'.') {
                    return false;
                }
                self.ws();
                if !self.byte(b'-') {
                    return false;
                }
            }
            Link::Thick | Link::Solid => {
                let b = if matches!(link, Link::Thick) {
                    b'='
                } else {
                    b'-'
                };
                if !self.byte(b) {
                    return false;
                }
                self.ws();
                if !self.run(b) {
                    return false;
                }
            }
        }
        self.arrow_right();
        true
    }

    /// An unlabelled link: dotted `-` `.`+ `-`, thick `=` `=`+, solid `-` `-`+, each with an
    /// optional arrow head at both ends and the grammar's spaces between parts.
    fn plain_link(&mut self, link: Link) -> bool {
        self.arrow_left();
        self.ws();
        let ok = match link {
            Link::Dotted => {
                self.byte(b'-')
                    && {
                        self.ws();
                        self.run(b'.')
                    }
                    && {
                        self.ws();
                        self.byte(b'-')
                    }
            }
            Link::Thick | Link::Solid => {
                let b = if matches!(link, Link::Thick) {
                    b'='
                } else {
                    b'-'
                };
                self.byte(b) && {
                    self.ws();
                    self.run(b)
                }
            }
        };
        if ok {
            self.arrow_right();
        }
        ok
    }

    /// `link_invisible = { "~" ~ "~" ~ "~"+ }`.
    fn invisible_link(&mut self) -> bool {
        for _ in 0..3 {
            self.ws();
            if !self.byte(b'~') {
                return false;
            }
        }
        loop {
            let before = self.i;
            self.ws();
            if !self.byte(b'~') {
                self.i = before;
                return true;
            }
        }
    }

    /// `edge_label = { "|" ~ edge_label_text ~ "|" }`, its text not empty and across lines.
    fn edge_label(&mut self) -> bool {
        let start = self.i;
        if self.byte(b'|') {
            self.ws();
            if let Some(end) = self.label_end(self.i, b"|") {
                self.i = end + 1;
                return true;
            }
        }
        self.i = start;
        false
    }

    /// An optional `<`, `x` or `o` before a link; the walk keeps it, as the grammar does.
    fn arrow_left(&mut self) {
        if matches!(self.s.get(self.i), Some(b'<' | b'x' | b'o')) {
            self.i += 1;
        }
    }

    /// An optional `>`, `x` or `o` after a link, after the grammar's spaces.
    fn arrow_right(&mut self) {
        let before = self.i;
        self.ws();
        if matches!(self.s.get(self.i), Some(b'>' | b'x' | b'o')) {
            self.i += 1;
        } else {
            self.i = before;
        }
    }

    /// Where a label that starts at `from` ends: the first place `stop` starts, if the label is
    /// not empty there.
    fn label_end(&self, from: usize, stop: &[u8]) -> Option<usize> {
        self.next_at(stop, from).filter(|end| *end > from)
    }

    /// The first place at or after `from` where the terminator `t` starts.
    fn next_at(&self, t: &[u8], from: usize) -> Option<usize> {
        let k = TERMINATORS.iter().position(|x| *x == t)?;
        let at = &self.at[k];
        let n = at.partition_point(|p| (*p as usize) < from);
        at.get(n).map(|p| *p as usize)
    }

    /// One or more line ends or `;`, spaces between them.
    fn separators(&mut self) -> bool {
        if self.newline().is_none() && !self.byte(b';') {
            return false;
        }
        loop {
            let before = self.i;
            self.ws();
            if self.newline().is_none() && !self.byte(b';') {
                self.i = before;
                return true;
            }
        }
    }

    /// A line end: LF, CRLF or a lone CR. Moves past it.
    fn newline(&mut self) -> Option<()> {
        match self.s.get(self.i) {
            Some(b'\n') => self.i += 1,
            Some(b'\r') => {
                self.i += 1;
                if self.s.get(self.i) == Some(&b'\n') {
                    self.i += 1;
                }
            }
            _ => return None,
        }
        Some(())
    }

    /// The position of the next line end at or after `i`, or the end.
    fn line_end(&self, i: usize) -> usize {
        self.s[i..]
            .iter()
            .position(|b| matches!(b, b'\n' | b'\r'))
            .map_or(self.s.len(), |n| i + n)
    }

    fn ws(&mut self) {
        while matches!(self.s.get(self.i), Some(b' ' | b'\t')) {
            self.i += 1;
        }
    }

    fn byte(&mut self, b: u8) -> bool {
        let found = self.s.get(self.i) == Some(&b);
        if found {
            self.i += 1;
        }
        found
    }

    fn bytes(&mut self, b: &[u8]) -> bool {
        let found = self.at_bytes(b);
        if found {
            self.i += b.len();
        }
        found
    }

    /// One or more `b`.
    fn run(&mut self, b: u8) -> bool {
        let start = self.i;
        while self.s.get(self.i) == Some(&b) {
            self.i += 1;
        }
        self.i > start
    }

    fn at_bytes(&self, b: &[u8]) -> bool {
        self.s[self.i..].starts_with(b)
    }

    /// An ASCII word in any case, with nothing required after it.
    fn word(&mut self, w: &[u8]) -> bool {
        let found = self.s[self.i..]
            .get(..w.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(w));
        if found {
            self.i += w.len();
        }
        found
    }

    fn char_here(&self) -> Option<char> {
        self.text[self.i..].chars().next()
    }
}

/// The grammar's `LETTER | NUMBER`, as Rust's alphabetic and numeric classes give it.
fn is_letter_or_number(c: char) -> bool {
    c.is_alphabetic() || c.is_numeric()
}
