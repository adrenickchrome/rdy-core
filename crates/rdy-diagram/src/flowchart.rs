//! The flowchart depth bound: the pinned grammar's own reading, with the one choice it takes
//! back.
//!
//! The pinned crate first drops the lines its flowchart grammar cannot start (see
//! [`preprocess`]), then parses the rest with a PEG grammar. The walk reads that same text the
//! way the grammar reads it: statements, comments, node shapes, edge labels and subgraph
//! headers, each with the grammar's own terminators. So a keyword in a comment or a label is
//! text, a subgraph can open after a node on the same line, and subgraphs in a row never add up.
//!
//! The grammar takes back one choice after it has read on: a subgraph. It reads `subgraph`, its
//! spec and its body up to the `end` that closes it. When no `end` comes, because the body
//! meets the end of the text or text it cannot read, it reads the same header again as plain
//! nodes and goes on from there. The walk does the same, whatever the title. A title can read
//! differently as nodes (a quote that a later quote closes swallows the text between), and the
//! walk then goes on where the nodes end.
//!
//! The grammar reads a statement that starts at a given place the same way at every depth, and
//! a body that starts there the same way in every subgraph. So the walk keeps, for each place,
//! where its statement ends and where a body that starts there ends, and reads each one once.
//! The rest of a vertex statement after a node's head (its class, then `&` nodes and links)
//! also reads the same from a given place. Titles read as nodes can reach one closer from many
//! starts, so the walk also keeps where the statement ends for each such place.
//! It does not recurse: the bodies it is reading are a stack. Every search for a terminator is
//! a lookup in a table built in one pass, so the walk is linear in the text.
//!
//! The bound is the deepest nesting the grammar attempts: a body counts while it is read, also
//! when no `end` closes it and its header is then read as nodes. When the grammar cannot read
//! the text at all, the bound is also at least the keyword bound ([`keyword_bound`]).

/// No entry yet.
const NONE: u32 = u32::MAX;
/// The statement or body that starts here cannot be read.
const FAIL: u32 = u32::MAX - 1;

/// The deepest subgraph nesting the pinned flowchart grammar attempts in `src`, the diagram's
/// source with any BOM removed and CRLF as LF.
pub(crate) fn depth(src: &str) -> usize {
    walk(src).0
}

/// The bound, and whether it is exact: the grammar reads the whole text and every subgraph it
/// attempts closes. Then the bound is the nesting the grammar parses.
pub(crate) fn walk(src: &str) -> (usize, bool) {
    let text = preprocess(crate::after_front_matter(src));
    let mut grammar = Grammar::new(&text);
    match grammar.flowchart() {
        Ok(deepest) => (deepest, !grammar.reread),
        Err(deepest) => (deepest.max(keyword_bound(text.as_bytes())), false),
    }
}

/// The operations the walk takes on `src`, and the length of the text it walks.
#[cfg(test)]
pub(crate) fn work(src: &str) -> (usize, usize) {
    let text = preprocess(crate::after_front_matter(src));
    let mut grammar = Grammar::new(&text);
    let _ = grammar.flowchart();
    (grammar.work.get(), text.len())
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

/// The terminators the walk searches for. `Grammar::at` holds where each one starts.
const TERMINATORS: [&[u8]; 14] = [
    b")", b"))", b"])", b"]]", b")]", b"}}", b"\\]", b"/]", b"]", b"}", b"|", b"\"", b"--", b"==",
];

/// The header's and `direction`'s values.
const DIRECTIONS: [&[u8]; 5] = [b"TD", b"TB", b"BT", b"LR", b"RL"];

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

/// What the statement at a place gives.
enum Step {
    /// The statement's nesting, and where the next one starts.
    Next(usize, u32),
    /// A subgraph header whose body, which starts here, the walk has not read yet.
    Open(usize),
    /// Text the grammar cannot read, after this nesting attempted.
    Stuck(u32),
}

/// A statement loop being read: the top level, or the body of a subgraph being attempted.
struct Loop {
    body: bool,
    /// Where its next statement starts.
    at: usize,
    /// Where its statements start in the walk's list of statements read.
    from: usize,
}

struct Grammar<'a> {
    text: &'a str,
    s: &'a [u8],
    i: usize,
    /// For each terminator, the positions it starts at, in order.
    at: Vec<Vec<u32>>,
    /// Where a dotted link's end (`-`? `.`+ `-`, no spaces) starts, in order.
    dotted: Vec<u32>,
    /// Where a line end (LF or CR) is, in order.
    line_ends: Vec<u32>,
    /// Where a line end or `;` is, in order.
    stops: Vec<u32>,
    /// For each place outside a quoted chunk, the `]` that ends a rectangle label read from
    /// there.
    rect: Vec<u32>,
    /// For each statement start: where the next statement starts, and the nesting attempted.
    next: Vec<u32>,
    nest: Vec<u32>,
    /// For each place a body line starts: where the body's closing `end` starts, and the
    /// nesting attempted from there to the body's end.
    body_end: Vec<u32>,
    body_max: Vec<u32>,
    /// For each place a node's head (its identifier and shape) ends in a vertex statement:
    /// where the statement ends. Titles read as nodes can reach the same closer from many
    /// starts, and the rest of the statement is then read once.
    rest_end: Vec<u32>,
    /// The heads [`Grammar::rest`] passes before it knows where the statement ends.
    heads: Vec<u32>,
    /// Whether a subgraph found no `end` and its header was read again as nodes.
    reread: bool,
    #[cfg(test)]
    work: std::cell::Cell<usize>,
}

impl<'a> Grammar<'a> {
    fn new(text: &'a str) -> Self {
        let s = text.as_bytes();
        let n = s.len();
        // the source is at most 65536 bytes, so every position fits in a u32
        let mut at = vec![Vec::new(); TERMINATORS.len()];
        let (mut line_ends, mut stops) = (Vec::new(), Vec::new());
        for i in 0..n {
            for (k, t) in TERMINATORS.iter().enumerate() {
                if s[i..].starts_with(t) {
                    at[k].push(i as u32);
                }
            }
            if matches!(s[i], b'\n' | b'\r') {
                line_ends.push(i as u32);
            }
            if matches!(s[i], b'\n' | b'\r' | b';') {
                stops.push(i as u32);
            }
        }
        // a dotted link's end starts on each dot of a run that a `-` follows, and on the `-`
        // just before such a run
        let mut dotted = Vec::new();
        let mut i = 0;
        while i < n {
            if s[i] != b'.' {
                i += 1;
                continue;
            }
            let run = i;
            while i < n && s[i] == b'.' {
                i += 1;
            }
            if s.get(i) == Some(&b'-') {
                if run > 0 && s[run - 1] == b'-' {
                    dotted.push(run as u32 - 1);
                }
                dotted.extend(run as u32..i as u32);
            }
        }
        // `text_rect`: a `"` opens a chunk that the next `"` not after `\` closes; with no
        // close it is text. Read from the right, each place points past its chunks to its `]`.
        let mut rect = vec![NONE; n + 1];
        let mut quote = NONE;
        for k in (0..n).rev() {
            rect[k] = match s[k] {
                b']' => k as u32,
                b'"' if quote != NONE => rect[quote as usize + 1],
                _ => rect[k + 1],
            };
            if s[k] == b'"' && (k == 0 || s[k - 1] != b'\\') {
                quote = k as u32;
            }
        }
        Grammar {
            text,
            s,
            i: 0,
            at,
            dotted,
            line_ends,
            stops,
            rect,
            next: vec![NONE; n + 1],
            nest: vec![0; n + 1],
            body_end: vec![NONE; n + 1],
            body_max: vec![0; n + 1],
            rest_end: vec![NONE; n + 1],
            heads: Vec::new(),
            reread: false,
            #[cfg(test)]
            work: Default::default(),
        }
    }

    /// `flowchart = SOI ~ header ~ separator ~ ((statement | comment) ~ separator?)* ~ EOI`.
    /// Ok with the deepest nesting attempted when the grammar reads the whole text; else Err
    /// with the deepest nesting attempted before it stops.
    fn flowchart(&mut self) -> Result<usize, usize> {
        let Some(start) = self.header() else {
            return Err(0);
        };
        let mut deepest = 0;
        let mut loops = vec![Loop {
            body: false,
            at: start,
            from: 0,
        }];
        // the statements the loops have read so far, with their nesting
        let mut read: Vec<(u32, u32)> = Vec::new();
        loop {
            let top = loops.len() - 1;
            let Loop { body, at, from } = loops[top];
            if !body && at == self.s.len() {
                return Ok(deepest);
            }
            if body {
                // `subgraph_body_line = !(end_keyword ~ (NEWLINE | ";" | EOI)) ~ ...`
                let ended = if self.body_end[at] != NONE {
                    Some((self.body_end[at], self.body_max[at]))
                } else if at == self.s.len() {
                    Some((FAIL, 0))
                } else if self.closes(at) {
                    Some((at as u32, 0))
                } else {
                    None
                };
                if let Some((end, max)) = ended {
                    self.finish(&mut read, from, at, end, max);
                    loops.pop();
                    continue;
                }
            }
            match self.step(at) {
                Step::Next(next, nest) => {
                    read.push((at as u32, nest));
                    loops[top].at = next;
                    if !body {
                        deepest = deepest.max(nest as usize);
                    }
                }
                Step::Open(line) => loops.push(Loop {
                    body: true,
                    at: line,
                    from: read.len(),
                }),
                Step::Stuck(nest) if !body => return Err(deepest.max(nest as usize)),
                Step::Stuck(nest) => {
                    self.finish(&mut read, from, at, FAIL, nest);
                    loops.pop();
                }
            }
        }
    }

    /// Ends the body loop whose statements are `read[from..]` and that stopped at `at`. `end`
    /// is where its closing `end` starts, or FAIL; `max` is the nesting attempted from `at` on.
    /// Each statement start in it gets the same end, and the nesting from there on.
    fn finish(&mut self, read: &mut Vec<(u32, u32)>, from: usize, at: usize, end: u32, max: u32) {
        self.body_end[at] = end;
        self.body_max[at] = max;
        let mut deepest = max;
        for &(p, nest) in read[from..].iter().rev() {
            deepest = deepest.max(nest);
            self.body_end[p as usize] = end;
            self.body_max[p as usize] = deepest;
        }
        read.truncate(from);
    }

    /// The statement (or comment) that starts at `at`, then `separator?`: where the next one
    /// starts. A subgraph whose body closes is its header to its `end`; one whose body does not
    /// close is read as nodes. Either way its nesting is one more than its body's.
    fn step(&mut self, at: usize) -> Step {
        self.tick();
        match self.next[at] {
            FAIL => return Step::Stuck(self.nest[at]),
            NONE => {}
            next => return Step::Next(next as usize, self.nest[at]),
        }
        let mut nest = 0;
        let end = if self.s[at..].starts_with(b"%%") {
            Some(self.line_end(at))
        } else if let Some(body) = self.subgraph_header(at) {
            let body = self.skip_ws(body);
            match self.body_end[body] {
                NONE => return Step::Open(body),
                FAIL => {
                    self.reread = true;
                    nest = self.body_max[body] + 1;
                    self.i = at;
                    self.statement().then_some(self.i)
                }
                end => {
                    nest = self.body_max[body] + 1;
                    Some(end as usize + 3)
                }
            }
        } else {
            self.i = at;
            self.statement().then_some(self.i)
        };
        self.nest[at] = nest;
        match end {
            Some(end) => {
                let next = self.after(end);
                self.next[at] = next as u32;
                Step::Next(next, nest)
            }
            None => {
                self.next[at] = FAIL;
                Step::Stuck(nest)
            }
        }
    }

    /// `header ~ separator`: where the first statement starts.
    fn header(&mut self) -> Option<usize> {
        self.i = 0;
        self.ws();
        if !(self.word(b"graph") || self.word(b"flowchart")) {
            return None;
        }
        self.ws();
        for d in DIRECTIONS {
            if self.word(d) {
                break;
            }
        }
        self.ws();
        if self.newline().is_none() && !self.byte(b';') {
            return None;
        }
        Some(self.after(self.i))
    }

    /// Where the next statement starts after one that ends at `end`: past spaces, separators
    /// and spaces.
    fn after(&mut self, end: usize) -> usize {
        self.i = end;
        self.ws();
        while self.newline().is_some() || self.byte(b';') {
            self.ws();
        }
        self.i
    }

    fn skip_ws(&mut self, at: usize) -> usize {
        self.i = at;
        self.ws();
        self.i
    }

    /// Whether a body line at `at` is the `end` that closes its subgraph: `end` before a line
    /// end, `;` or the end of the text.
    fn closes(&mut self, at: usize) -> bool {
        self.i = at;
        if !self.word(b"end") {
            return false;
        }
        self.ws();
        matches!(self.s.get(self.i), None | Some(b'\n' | b'\r' | b';'))
    }

    /// `subgraph_keyword ~ subgraph_spec ~ (NEWLINE | ";")` at `at`: where its body starts.
    fn subgraph_header(&mut self, at: usize) -> Option<usize> {
        self.i = at;
        if !self.word(b"subgraph") {
            return None;
        }
        self.ws();
        if !self.subgraph_spec() {
            return None;
        }
        self.ws();
        (self.newline().is_some() || self.byte(b';')).then_some(self.i)
    }

    /// `subgraph_spec`: an id with a `[title]`, an id with a `"title"`, a `"title"`, or an id.
    fn subgraph_spec(&mut self) -> bool {
        if !self.subgraph_id() {
            return self.quoted_title();
        }
        let after_id = self.i;
        self.ws();
        if self.byte(b'[') {
            self.ws();
            if let Some(end) = self.label_end(self.i, b"]") {
                self.i = end + 1;
                return true;
            }
        }
        self.i = after_id;
        self.ws();
        if !self.quoted_title() {
            self.i = after_id;
        }
        true
    }

    /// `subgraph_id = @{ (LETTER | NUMBER | "_") ~ (LETTER | NUMBER | "_" | "-" | ".")* }`.
    fn subgraph_id(&mut self) -> bool {
        match self.char_here() {
            Some(c) if is_letter_or_number(c) || c == '_' => self.i += c.len_utf8(),
            _ => return false,
        }
        while let Some(c) = self.char_here() {
            if !(is_letter_or_number(c) || matches!(c, '_' | '-' | '.')) {
                break;
            }
            self.i += c.len_utf8();
        }
        true
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

    /// One statement other than a subgraph whose body closes: a keyword line or a vertex
    /// statement. Starts at `self.i`.
    fn statement(&mut self) -> bool {
        for kw in [&b"style"[..], b"classDef", b"class", b"click", b"linkStyle"] {
            if self.keyword_at(kw) {
                // to the line end or `;`
                self.i = next_in(&self.stops, self.i).unwrap_or(self.s.len());
                return true;
            }
        }
        if self.keyword_at(b"direction") {
            let start = self.i;
            self.i += b"direction".len();
            self.ws();
            if DIRECTIONS.iter().any(|d| self.word(d)) {
                return true;
            }
            self.i = start;
        }
        self.vertex_statement()
    }

    /// A keyword with no letter, digit or `_` after it, in any case. Nothing moves.
    fn keyword_at(&self, kw: &[u8]) -> bool {
        self.tick();
        let Some(head) = self.s[self.i..].get(..kw.len()) else {
            return false;
        };
        head.eq_ignore_ascii_case(kw)
            && !self.text[self.i + kw.len()..]
                .chars()
                .next()
                .is_some_and(|c| is_letter_or_number(c) || c == '_')
    }

    /// `vertex_statement = node_group ~ edge_segment*`, with `node_group = node ~ ("&" ~
    /// node)*` and `node = identifier ~ shape? ~ class_annotation?`.
    fn vertex_statement(&mut self) -> bool {
        if !self.node_head() {
            return false;
        }
        self.rest();
        true
    }

    /// The rest of a vertex statement after a node's head, from `self.i`: the node's class,
    /// then `("&" ~ node)*`, then `edge_segment*`. After each node the rest reads the same, so
    /// it depends only on where the head ends. Each place gets its statement end once, in
    /// `rest_end`.
    fn rest(&mut self) {
        let mut heads = std::mem::take(&mut self.heads);
        loop {
            self.tick();
            let head = self.i;
            if self.rest_end[head] != NONE {
                self.i = self.rest_end[head] as usize;
                break;
            }
            heads.push(head as u32);
            // `class_annotation?`
            self.ws();
            if !(self.bytes(b":::") && {
                self.ws();
                self.class_name()
            }) {
                self.i = head;
            }
            let node_end = self.i;
            // `"&" ~ node`, which the node's group tries first
            self.ws();
            if self.byte(b'&') && {
                self.ws();
                self.node_head()
            } {
                continue;
            }
            // else `edge_connector ~ node_group`
            self.i = node_end;
            self.ws();
            if self.edge_connector() && {
                self.ws();
                self.node_head()
            } {
                continue;
            }
            self.i = node_end;
            break;
        }
        for head in heads.drain(..) {
            self.rest_end[head as usize] = self.i as u32;
        }
        self.heads = heads;
    }

    /// A node's head: `identifier ~ shape?`.
    fn node_head(&mut self) -> bool {
        if !self.identifier() {
            return false;
        }
        let before = self.i;
        self.ws();
        if !self.shape() {
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

    /// `"[" ~ text_rect ~ "]"`: the label runs to the first `]` outside quoted chunks, and is
    /// not empty.
    fn rectangle(&mut self) -> bool {
        let start = self.i;
        if !self.byte(b'[') {
            return false;
        }
        self.ws();
        if self.s.get(self.i).is_some_and(|b| *b != b']') && self.rect[self.i] != NONE {
            self.i = self.rect[self.i] as usize + 1;
            return true;
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
        // the first place the link's end starts, read with no spaces: `-`? `.`+ `-` for
        // dotted, `=` `=`+ for thick, `-` `-`+ for solid
        let end = match link {
            Link::Dotted => {
                self.tick();
                next_in(&self.dotted, text)
            }
            Link::Thick => self.next_at(b"==", text),
            Link::Solid => self.next_at(b"--", text),
        };
        let line_end = self.line_end(text);
        match end {
            Some(end) if end > text && end < line_end => {
                self.i = end;
                self.link_end(link)
            }
            _ => false,
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
        self.tick();
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
        self.tick();
        let k = TERMINATORS.iter().position(|x| *x == t)?;
        next_in(&self.at[k], from)
    }

    /// The position of the next line end at or after `i`, or the end.
    fn line_end(&self, i: usize) -> usize {
        self.tick();
        next_in(&self.line_ends, i).unwrap_or(self.s.len())
    }

    /// A line end: LF, CRLF or a lone CR. Moves past it.
    fn newline(&mut self) -> Option<()> {
        self.tick();
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

    /// Counts one operation, for the linearity test.
    fn tick(&self) {
        #[cfg(test)]
        self.work.set(self.work.get() + 1);
    }

    fn ws(&mut self) {
        self.tick();
        while matches!(self.s.get(self.i), Some(b' ' | b'\t')) {
            self.tick();
            self.i += 1;
        }
    }

    fn byte(&mut self, b: u8) -> bool {
        self.tick();
        let found = self.s.get(self.i) == Some(&b);
        if found {
            self.i += 1;
        }
        found
    }

    fn bytes(&mut self, b: &[u8]) -> bool {
        self.tick();
        let found = self.s[self.i..].starts_with(b);
        if found {
            self.i += b.len();
        }
        found
    }

    /// One or more `b`.
    fn run(&mut self, b: u8) -> bool {
        let start = self.i;
        while self.s.get(self.i) == Some(&b) {
            self.tick();
            self.i += 1;
        }
        self.i > start
    }

    /// An ASCII word in any case, with nothing required after it.
    fn word(&mut self, w: &[u8]) -> bool {
        self.tick();
        let found = self.s[self.i..]
            .get(..w.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(w));
        if found {
            self.i += w.len();
        }
        found
    }

    fn char_here(&self) -> Option<char> {
        self.tick();
        self.text[self.i..].chars().next()
    }
}

/// The first entry of the ordered list `at` that is at or after `from`.
fn next_in(at: &[u32], from: usize) -> Option<usize> {
    let n = at.partition_point(|p| (*p as usize) < from);
    at.get(n).map(|p| *p as usize)
}

/// The grammar's `LETTER | NUMBER`, as Rust's alphabetic and numeric classes give it.
fn is_letter_or_number(c: char) -> bool {
    c.is_alphabetic() || c.is_numeric()
}
