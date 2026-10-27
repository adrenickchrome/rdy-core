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
//!
//! The same walk gives the two rewrites of the drawing input (D-R7C3W, D-438Y2): the blank lines
//! that directly follow a subgraph header ([`normalize_subgraph_blanks`]), and the top-level
//! subgraphs that hidden arrows may stack ([`Reading::stacked`]). Neither reaches the source a
//! reader sees.
//!
//! It also gives the arrow cost (D-VACA9, [`Reading::arrows`]). A connector that links two node
//! groups costs its rank span ([`span`]) once for each pair of a source and a target, as the
//! pinned crate expands it before layout, times the label weight when it has a label. A
//! connector is charged once, at its place, however many reads reach it. Every count saturates
//! at `MAX_ARROWS + 1`, and no pair is ever built.

use crate::ARROW_CAP;
use crate::arrows::Tally;
use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::hash_map::Entry;

/// No entry yet.
const NONE: u32 = u32::MAX;
/// The statement or body that starts here cannot be read.
const FAIL: u32 = u32::MAX - 1;
/// The context of a place outside every subgraph.
const TOP: u32 = u32::MAX;
/// The most ids the walk keeps for the stack check; past it, nothing is stacked.
const MAX_IDS: usize = 65536;
/// The keywords that start a statement other than a node, which a hidden arrow's id must not
/// be.
const STATEMENT_KEYWORDS: [&[u8]; 8] = [
    b"style",
    b"classDef",
    b"class",
    b"click",
    b"linkStyle",
    b"direction",
    b"subgraph",
    b"end",
];

/// The deepest subgraph nesting the pinned flowchart grammar attempts in `src`, the diagram's
/// source with any BOM removed and CRLF as LF.
#[cfg(test)]
pub(crate) fn depth(src: &str) -> usize {
    read(src).depth
}

/// The bound, and whether it is exact: the grammar reads the whole text and every subgraph it
/// attempts closes. Then the bound is the nesting the grammar parses.
#[cfg(test)]
pub(crate) fn walk(src: &str) -> (usize, bool) {
    let r = read(src);
    (r.depth, r.exact)
}

/// What the walk finds in a flowchart.
pub(crate) struct Reading {
    /// The depth bound.
    pub depth: usize,
    /// Whether the bound is the nesting the grammar parses (see [`walk`]).
    #[cfg(test)]
    pub exact: bool,
    /// Whether a subgraph the grammar attempts finds no `end`, so its header is read again as
    /// nodes. The pinned parser keeps no memo, so it reads such a body again for each one.
    pub unclosed: bool,
    /// The longest arrow the walk read: its bytes from its first character through its head and
    /// its `|label|`, with the label text of a labelled arrow between. An arrow that starts but
    /// has no end on its line counts from its start to the line end, the line end excluded.
    pub longest_arrow: usize,
    /// The ids of the top-level subgraphs, in source order, when the drawing input stacks them
    /// with hidden arrows (D-438Y2); else empty. The grammar reads the whole text and every
    /// subgraph closes; there are at least two top-level subgraphs; each sets its own direction
    /// in its own body and has an id; no header at any depth has only a quoted title; and no id
    /// is read both inside one top-level subgraph and inside another or outside every subgraph,
    /// and no arrow completes outside every subgraph. `render` keeps the stacked drawing only
    /// when it removes an overlap and adds none.
    pub stacked: Vec<String>,
    /// What the arrows cost (D-VACA9), the hidden arrows of `stacked` included, saturated at
    /// `MAX_ARROWS + 1`.
    pub arrows: usize,
    /// How many expanded arrows have a label, for the parity pin.
    #[cfg(test)]
    pub labelled: usize,
}

/// Reads `src`, the diagram's source with any BOM removed and CRLF as LF, as the pinned
/// flowchart grammar reads it.
pub(crate) fn read(src: &str) -> Reading {
    let text = preprocess(crate::after_front_matter(src), None);
    let mut grammar = Grammar::new(&text);
    let (depth, whole) = match grammar.flowchart() {
        Ok(deepest) => (deepest, !grammar.reread),
        Err(deepest) => (deepest.max(keyword_bound(text.as_bytes())), false),
    };
    let stacked = if whole { grammar.stacked() } else { Vec::new() };
    // each hidden arrow is one invisible connector of span 1 (D-ZJVRT, O7)
    let mut tally = grammar.tally;
    if stacked.len() >= 2 {
        tally.add(1, false, stacked.len() - 1);
    }
    Reading {
        depth,
        #[cfg(test)]
        exact: whole,
        unclosed: grammar.reread,
        longest_arrow: grammar.longest_arrow,
        stacked,
        arrows: tally.cost,
        #[cfg(test)]
        labelled: tally.labelled,
    }
}

/// `src`, the diagram's source with any BOM removed and CRLF as LF, without the blank lines that
/// directly follow a subgraph header (D-R7C3W): borrowed when there are none.
///
/// The pinned grammar takes one line end after a header, so a blank line there leaves the
/// subgraph open. A header is one the walk reads at a statement position. The lines it drops are
/// the run of complete blank lines (spaces or tabs, then LF or a lone CR) that follows a header
/// that ends with a line end, in the text the grammar parses, and a last line of spaces or tabs
/// at the end of the text. They go at their places in `src`; the lines the pinned crate drops
/// before it parses stay. A header that a `;` ends keeps the lines after it.
pub(crate) fn normalize_subgraph_blanks(src: &str) -> Cow<'_, str> {
    let body = crate::after_front_matter(src);
    let base = src.len() - body.len();
    // for each byte of the parsed text, its place in `body`
    let mut map = Vec::with_capacity(body.len() + 1);
    let text = preprocess(body, Some(&mut map));
    let mut grammar = Grammar::new(&text);
    grammar.drop_blanks = true;
    let _ = grammar.flowchart();
    let mut runs = std::mem::take(&mut grammar.dropped);
    if runs.is_empty() {
        return Cow::Borrowed(src);
    }
    runs.sort_unstable();
    runs.dedup();
    // the bytes of `src` to delete, as ranges in order; places only grow along the text
    let bytes = src.as_bytes();
    let mut cut: Vec<(usize, usize)> = Vec::new();
    for (from, to) in runs {
        for k in from..to {
            let at = base + map[k as usize] as usize;
            // an LF that joins lines stands for the line's whole end: a CR there is the CR of a
            // CR LF that CR CR LF left, so both go
            let len = if bytes[at..].starts_with(b"\r\n") {
                2
            } else {
                1
            };
            match cut.last_mut() {
                Some((_, end)) if *end >= at => *end = (*end).max(at + len),
                _ => cut.push((at, at + len)),
            }
        }
    }
    let mut out = String::with_capacity(src.len());
    let mut last = 0;
    for (from, to) in cut {
        // every deleted byte is a space, a tab or a line end
        out.push_str(&src[last..from]);
        last = to;
    }
    out.push_str(&src[last..]);
    Cow::Owned(out)
}

/// The operations the walk takes on `src`, and the length of the text it walks.
#[cfg(test)]
pub(crate) fn work(src: &str) -> (usize, usize) {
    let text = preprocess(crate::after_front_matter(src), None);
    let mut grammar = Grammar::new(&text);
    let _ = grammar.flowchart();
    (grammar.work.get(), text.len())
}

/// The text the pinned crate's flowchart grammar parses: the lines after front matter, from
/// the header on, without `%%{...}%%` directives and without lines the grammar cannot start. A
/// kept line starts with a letter, digit, `_` or `;`, or is blank, a `%%` comment or a keyword
/// line. Kept lines are joined with LF.
///
/// With `map`, it also gets, for each byte of the text, its place in `input`: a kept line's
/// bytes are its own, and the LF after a kept line is the line end that follows it in `input`.
fn preprocess(input: &str, mut map: Option<&mut Vec<u32>>) -> String {
    let mut out = String::with_capacity(input.len());
    let mut header = false;
    // where the last kept line ends in `input`
    let mut kept_end = 0;
    for line in input.lines() {
        // the line is a slice of `input`, so its place is its offset
        let start = line.as_ptr() as usize - input.as_ptr() as usize;
        let mut keep = |out: &mut String| {
            push_line(out, line, start, &mut kept_end, map.as_deref_mut());
        };
        let trimmed = line.trim();
        if trimmed.starts_with("%%{") && trimmed.ends_with("}%%") {
            continue;
        }
        if !header {
            let first = trimmed.split_whitespace().next().unwrap_or("");
            if first.eq_ignore_ascii_case("graph") || first.eq_ignore_ascii_case("flowchart") {
                header = true;
                keep(&mut out);
            }
            continue;
        }
        if trimmed.starts_with("%%") || trimmed.is_empty() || kept_keyword_line(trimmed) {
            keep(&mut out);
            continue;
        }
        let lower = |n: usize| trimmed.as_bytes().get(..n).map(<[u8]>::to_ascii_lowercase);
        if lower(8).as_deref() == Some(b"acctitle") || lower(8).as_deref() == Some(b"accdescr") {
            continue;
        }
        let first = trimmed.chars().next().unwrap_or(' ');
        if first.is_alphanumeric() || first == '_' || first == ';' {
            keep(&mut out);
        }
    }
    if input.ends_with('\n') && !out.ends_with('\n') {
        out.push('\n');
        if let Some(map) = map {
            map.push(kept_end as u32);
        }
    }
    out
}

/// Adds a kept line that starts at `start` in the input, after an LF when it is not the first.
fn push_line(
    out: &mut String,
    line: &str,
    start: usize,
    kept_end: &mut usize,
    map: Option<&mut Vec<u32>>,
) {
    let first = out.is_empty();
    if !first {
        out.push('\n');
    }
    out.push_str(line);
    if let Some(map) = map {
        // the source is at most 65536 bytes, so every place fits in a u32
        if !first {
            map.push(*kept_end as u32);
        }
        map.extend(start as u32..(start + line.len()) as u32);
    }
    *kept_end = start + line.len();
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

/// A connector's rank span, as the pinned compiler sets its `minlen` (mmdflux 2.6.1
/// `compiler.rs:742-758`): the length of its run (the dashes after the first dash, the equals
/// signs after the first equals sign, or the dots); a solid or thick connector with no head at
/// either end is one less, and at least one.
fn span(link: Link, run: usize, headed: bool) -> usize {
    match link {
        Link::Dotted => run,
        Link::Thick | Link::Solid if headed => run,
        Link::Thick | Link::Solid => run.saturating_sub(1).max(1),
    }
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
    /// The top-level subgraph it is in, by index, or TOP.
    ctx: u32,
}

/// A top-level subgraph, for the stack check.
struct Top {
    /// Where its header's id is, if it has one.
    id: Option<(u32, u32)>,
    /// Whether its own body holds a `direction` statement.
    direction: bool,
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
    /// The longest arrow read so far, in bytes.
    longest_arrow: usize,
    /// Whether a header that ends with a line end skips the blank lines after it, as the
    /// drawing input has none ([`normalize_subgraph_blanks`]).
    drop_blanks: bool,
    /// The blank-line runs the headers skipped, as ranges of the text.
    dropped: Vec<(u32, u32)>,
    /// The stack check: the top-level subgraph the statement being read is in, or TOP.
    ctx: u32,
    /// The top-level subgraphs attempted, in order.
    tops: Vec<Top>,
    /// Each id a node's head reads and each subgraph id, with its context.
    ids: Vec<(u32, u32, u32)>,
    /// Whether an arrow completes outside every subgraph, or the id table is full.
    crossed: bool,
    /// Whether a header at any depth has only a quoted title. The pinned crate names such a
    /// subgraph `subGraph<N>`, an id the walk does not see, so an arrow could reach it unseen.
    quoted_only: bool,
    /// Whether the statement just read is a `direction` statement.
    direction_read: bool,
    /// Where the id of the last subgraph spec read is, if it has one.
    header_id: Option<(u32, u32)>,
    /// The rank span of the last connector read, and whether it has a label.
    span: usize,
    labelled: bool,
    /// For each place, whether the connector that starts there is charged already.
    charged: Vec<bool>,
    /// What the charged connectors cost.
    tally: Tally,
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
            longest_arrow: 0,
            drop_blanks: false,
            dropped: Vec::new(),
            ctx: TOP,
            tops: Vec::new(),
            ids: Vec::new(),
            crossed: false,
            quoted_only: false,
            direction_read: false,
            header_id: None,
            span: 1,
            labelled: false,
            charged: vec![false; n + 1],
            tally: Tally::default(),
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
            ctx: TOP,
        }];
        // the statements the loops have read so far, with their nesting
        let mut read: Vec<(u32, u32)> = Vec::new();
        loop {
            let top = loops.len() - 1;
            let Loop {
                body,
                at,
                from,
                ctx,
            } = loops[top];
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
            self.ctx = ctx;
            self.direction_read = false;
            match self.step(at) {
                Step::Next(next, nest) => {
                    // a `direction` in a top-level subgraph's own body
                    if top == 1
                        && self.direction_read
                        && let Some(t) = self.tops.get_mut(ctx as usize)
                    {
                        t.direction = true;
                    }
                    read.push((at as u32, nest));
                    loops[top].at = next;
                    if !body {
                        deepest = deepest.max(nest as usize);
                    }
                }
                Step::Open(line) => {
                    // a top-level header opens a new context; a nested one stays in its own
                    let ctx = if top == 0 {
                        self.tops.push(Top {
                            id: self.header_id,
                            direction: false,
                        });
                        (self.tops.len() - 1) as u32
                    } else {
                        ctx
                    };
                    if let Some((start, end)) = self.header_id {
                        self.record(start as usize, end as usize, ctx);
                    }
                    loops.push(Loop {
                        body: true,
                        at: line,
                        from: read.len(),
                        ctx,
                    });
                }
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
            if self.header_id.is_none() {
                self.quoted_only = true;
            }
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

    /// `subgraph_keyword ~ subgraph_spec ~ (NEWLINE | ";")` at `at`: where its body starts. With
    /// `drop_blanks`, a header that ends with a line end also takes the blank lines after it.
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
        if self.newline().is_some() {
            if self.drop_blanks {
                self.blank_run();
            }
            return Some(self.i);
        }
        self.byte(b';').then_some(self.i)
    }

    /// Moves past the complete blank lines at `self.i` (spaces or tabs, then a line end) and a
    /// last line of spaces or tabs at the end of the text, and keeps them as a dropped run.
    fn blank_run(&mut self) {
        let from = self.i;
        let mut end = from;
        loop {
            self.i = end;
            self.ws();
            if self.newline().is_some() {
                end = self.i;
            } else {
                if self.i == self.s.len() {
                    end = self.i;
                }
                break;
            }
        }
        if end > from {
            self.dropped.push((from as u32, end as u32));
        }
        self.i = end;
    }

    /// `subgraph_spec`: an id with a `[title]`, an id with a `"title"`, a `"title"`, or an id.
    fn subgraph_spec(&mut self) -> bool {
        self.header_id = None;
        let start = self.i;
        if !self.subgraph_id() {
            return self.quoted_title();
        }
        let after_id = self.i;
        self.header_id = Some((start as u32, after_id as u32));
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
                self.direction_read = true;
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
        // the connector that waits for its target group to end: its place, span and label, and
        // the size of its source group; and the size of the group being read
        let mut pending: Option<(usize, usize, bool, usize)> = None;
        let mut group = 1usize;
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
                group = group.saturating_add(1).min(ARROW_CAP);
                continue;
            }
            // else `edge_connector ~ node_group`
            self.i = node_end;
            self.ws();
            let arrow = self.i;
            if self.edge_connector() {
                self.longest_arrow = self.longest_arrow.max(self.i - arrow);
                if self.ctx == TOP {
                    self.crossed = true;
                }
                let (span, labelled) = (self.span, self.labelled);
                self.ws();
                if self.node_head() {
                    // the connector links two read groups: the one before it is complete
                    self.charge(pending, group);
                    pending = Some((arrow, span, labelled, group));
                    group = 1;
                    continue;
                }
            } else if self.connector_start(arrow) {
                // an arrow that starts but has no end: from its start to the line end
                self.longest_arrow = self.longest_arrow.max(self.line_end(arrow) - arrow);
            }
            self.i = node_end;
            break;
        }
        self.charge(pending, group);
        for head in heads.drain(..) {
            self.rest_end[head as usize] = self.i as u32;
        }
        self.heads = heads;
    }

    /// Charges a connector that links two read node groups, `targets` being its target group's
    /// size: its span (times the label weight) for each pair of a source and a target, once
    /// for its place.
    fn charge(&mut self, connector: Option<(usize, usize, bool, usize)>, targets: usize) {
        let Some((at, span, labelled, sources)) = connector else {
            return;
        };
        if std::mem::replace(&mut self.charged[at], true) {
            return;
        }
        let pairs = sources.saturating_mul(targets).min(ARROW_CAP);
        self.tally.add(span, labelled, pairs);
    }

    /// A node's head: `identifier ~ shape?`. Its id is kept for the stack check.
    fn node_head(&mut self) -> bool {
        let start = self.i;
        if !self.identifier() {
            return false;
        }
        self.record(start, self.i, self.ctx);
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
        match self.edge_label() {
            Some(labelled) => self.labelled = labelled,
            None => {
                // a `|` that no label closes: the arrow it starts runs to the line end
                if self.s.get(self.i) == Some(&b'|') {
                    self.longest_arrow = self.longest_arrow.max(self.line_end(start) - start);
                }
                self.i = before;
            }
        }
        true
    }

    /// Whether a connector starts at `at`: an optional left head, then the grammar's spaces, then
    /// a dash or an equals sign (a solid, dotted or thick start), or a tilde (an invisible start).
    ///
    /// An `x` or `o` is a left head only where the identifier rule stops after it: before a space
    /// or tab, an equals sign, or a dash that `-`, `>` or `.` follows. Else the grammar reads it
    /// as the start of a node id, such as `x-ray`.
    fn connector_start(&self, at: usize) -> bool {
        self.tick();
        if self.s.get(at) == Some(&b'~') {
            return true;
        }
        let head = match self.s.get(at) {
            Some(b'<') => true,
            Some(b'x' | b'o') => match self.s.get(at + 1) {
                Some(b' ' | b'\t' | b'=') => true,
                Some(b'-') => matches!(self.s.get(at + 2), Some(b'-' | b'>' | b'.')),
                _ => false,
            },
            _ => false,
        };
        let mut k = at + usize::from(head);
        while matches!(self.s.get(k), Some(b' ' | b'\t')) {
            self.tick();
            k += 1;
        }
        matches!(self.s.get(k), Some(b'-' | b'='))
    }

    /// Keeps an id read at `start..end` in context `ctx`, for the stack check.
    fn record(&mut self, start: usize, end: usize, ctx: u32) {
        if self.ids.len() == MAX_IDS {
            self.crossed = true;
        } else {
            self.ids.push((start as u32, end as u32, ctx));
        }
    }

    /// The top-level subgraph ids that hidden arrows may stack, after a walk that read the whole
    /// text with every subgraph closed (see [`Reading::stacked`]); else empty.
    fn stacked(&mut self) -> Vec<String> {
        if self.crossed
            || self.quoted_only
            || self.tops.len() < 2
            || !self.tops.iter().all(|t| t.direction)
        {
            return Vec::new();
        }
        let mut ids = Vec::with_capacity(self.tops.len());
        for k in 0..self.tops.len() {
            let id = self.tops[k].id;
            match id {
                Some((start, end)) if self.plain_id(start as usize, end as usize) => {
                    ids.push((start as usize, end as usize));
                }
                _ => return Vec::new(),
            }
        }
        // an id read in two contexts may join two subgraphs, or one to the top level
        let mut seen: HashMap<&[u8], u32> = HashMap::with_capacity(self.ids.len());
        for &(start, end, ctx) in &self.ids {
            match seen.entry(&self.s[start as usize..end as usize]) {
                Entry::Occupied(e) if *e.get() != ctx => return Vec::new(),
                Entry::Occupied(_) => {}
                Entry::Vacant(e) => {
                    e.insert(ctx);
                }
            }
        }
        ids.into_iter()
            .map(|(start, end)| self.text[start..end].to_string())
            .collect()
    }

    /// Whether the subgraph id at `start..end` reads back as one node id at the start of a
    /// hidden arrow's line: a whole identifier, and no keyword that starts another statement.
    fn plain_id(&mut self, start: usize, end: usize) -> bool {
        self.i = start;
        if STATEMENT_KEYWORDS.iter().any(|kw| self.keyword_at(kw)) {
            return false;
        }
        self.identifier() && self.i == end
    }

    /// A labelled link: its start (atomic), then a label on the same line up to the first
    /// place the link's end starts, then the end.
    fn labelled_link(&mut self, link: Link) -> bool {
        let left = self.arrow_left();
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
                let Some((run, right)) = self.link_end(link) else {
                    return false;
                };
                self.span = span(link, run, left || right);
                self.labelled = !self.text[text..end].trim().is_empty();
                true
            }
            _ => false,
        }
    }

    /// A link's end, with the grammar's spaces between its parts and an optional arrow head:
    /// the length of its run and whether it has the head.
    fn link_end(&mut self, link: Link) -> Option<(usize, bool)> {
        let run = match link {
            Link::Dotted => {
                self.byte(b'-');
                self.ws();
                let run = self.run(b'.');
                self.ws();
                if run == 0 || !self.byte(b'-') {
                    return None;
                }
                run
            }
            Link::Thick | Link::Solid => {
                let b = if matches!(link, Link::Thick) {
                    b'='
                } else {
                    b'-'
                };
                if !self.byte(b) {
                    return None;
                }
                self.ws();
                self.run(b)
            }
        };
        (run > 0).then(|| (run, self.arrow_right()))
    }

    /// An unlabelled link: dotted `-` `.`+ `-`, thick `=` `=`+, solid `-` `-`+, each with an
    /// optional arrow head at both ends and the grammar's spaces between parts.
    fn plain_link(&mut self, link: Link) -> bool {
        let left = self.arrow_left();
        self.ws();
        let run = match link {
            Link::Dotted => {
                if !self.byte(b'-') {
                    return false;
                }
                self.ws();
                let run = self.run(b'.');
                self.ws();
                if run == 0 || !self.byte(b'-') {
                    return false;
                }
                run
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
                self.run(b)
            }
        };
        if run == 0 {
            return false;
        }
        let right = self.arrow_right();
        self.span = span(link, run, left || right);
        self.labelled = false;
        true
    }

    /// `link_invisible = { "~" ~ "~" ~ "~"+ }`: its span is one.
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
                self.span = 1;
                self.labelled = false;
                return true;
            }
        }
    }

    /// `edge_label = { "|" ~ edge_label_text ~ "|" }`, its text not empty and across lines:
    /// whether its text has more than white space, if there is one.
    fn edge_label(&mut self) -> Option<bool> {
        let start = self.i;
        if self.byte(b'|') {
            self.ws();
            let text = self.i;
            if let Some(end) = self.label_end(text, b"|") {
                self.i = end + 1;
                return Some(!self.text[text..end].trim().is_empty());
            }
        }
        self.i = start;
        None
    }

    /// An optional `<`, `x` or `o` before a link; the walk keeps it, as the grammar does.
    /// Whether it is there.
    fn arrow_left(&mut self) -> bool {
        self.tick();
        let head = matches!(self.s.get(self.i), Some(b'<' | b'x' | b'o'));
        if head {
            self.i += 1;
        }
        head
    }

    /// An optional `>`, `x` or `o` after a link, after the grammar's spaces. Whether it is
    /// there.
    fn arrow_right(&mut self) -> bool {
        let before = self.i;
        self.ws();
        if matches!(self.s.get(self.i), Some(b'>' | b'x' | b'o')) {
            self.i += 1;
            true
        } else {
            self.i = before;
            false
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

    /// A run of `b`: how many, none included.
    fn run(&mut self, b: u8) -> usize {
        let start = self.i;
        while self.s.get(self.i) == Some(&b) {
            self.tick();
            self.i += 1;
        }
        self.i - start
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
