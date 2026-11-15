//! The inline subset of Markdown (md1) every running text a reader reads is written in, read
//! once for every output: the site's HTML (rdy-render) and the plain reader text of the search
//! index, the labels and the publication review.
//!
//! References are read first ([`ref_spans`]): each is one opaque atom, and a `\` right before
//! `{{` is the rdy escape (a literal `{{`). Then, outside atoms: backslash escapes, code spans,
//! links and emphasis (`*` and `**` only: an underscore is always literal, FW-F1a). A body is
//! grouped into paragraphs and lists. Anything else is text.
use rdy_model::list_parts::Block as BodyBlock;
use rdy_model::list_parts::{List, Part, parse_body};
use rdy_model::{RefMode, RefSpan, ref_spans};

/// The longest authored link target, local or external, in UTF-8 bytes, a named profile keeps a
/// link for.
pub const MAX_LOCAL_URL: usize = 2048;

/// Which part of the subset a text is read in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// Paragraphs and lists of spans: the body of a block.
    Body,
    /// Spans only: a one-line field.
    Spans,
    /// References and code spans only: a title, a name, a caption, alt text.
    Label,
}

/// The words of `text` read in `form`, with no markup: markers, delimiters and link targets
/// dropped, code and link text kept, escapes resolved. A reference reads as its display text,
/// else as what `label` returns for its token. Each list item and line break is a newline, and
/// blocks are separated by a blank line.
pub fn plain(text: &str, mode: RefMode, form: Form, label: &dyn Fn(&str) -> String) -> String {
    plain_refs(text, mode, form, &|span: &RefSpan| label(&span.token))
}

/// As [`plain`], but `label` sees the whole reference (to tell a key-typed one, FW-F2a). It is
/// called only for a reference with no display text.
pub fn plain_refs(
    text: &str,
    mode: RefMode,
    form: Form,
    label: &dyn Fn(&RefSpan) -> String,
) -> String {
    plain_refs_in(
        text,
        mode,
        form,
        &|span| display_or(span, label),
        &|_| false,
        false,
    )
}

/// A reference's display text, else what `label` says.
fn display_or(span: &RefSpan, label: &dyn Fn(&RefSpan) -> String) -> String {
    match &span.display {
        Some(d) => d.clone(),
        None => label(span),
    }
}

/// As [`plain_refs`], but `words` is called for every reference, one with display text too, and
/// says all it reads as: a reference to a removed block reads as the notice, whatever display
/// text the author gave (D-3M81W). `drop` says which references a named profile drops, with the
/// parentheses they empty ([`prune`]).
pub fn plain_refs_every(
    text: &str,
    mode: RefMode,
    form: Form,
    words: &dyn Fn(&RefSpan) -> String,
    drop: &dyn Fn(&RefSpan) -> bool,
) -> String {
    plain_refs_in(text, mode, form, words, drop, false)
}

/// Opens a code span in the text of [`plain_refs_coded`].
pub const CODE_OPEN: char = '\u{E000}';
/// Closes a code span in the text of [`plain_refs_coded`].
pub const CODE_CLOSE: char = '\u{E001}';
/// In the text of [`plain_refs_coded`], the next character is authored text, not a mark.
pub const CODE_ESC: char = '\u{E002}';

/// `t` as authored text in the coded form ([`plain_refs_coded`]): each of the three mark
/// characters it holds is kept, after [`CODE_ESC`], so a reader of the coded text gets it back.
pub fn code_escape(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    for c in t.chars() {
        if matches!(c, CODE_OPEN | CODE_CLOSE | CODE_ESC) {
            out.push(CODE_ESC);
        }
        out.push(c);
    }
    out
}

/// As [`plain_refs_every`] (`words` reads every reference, one with display text too), but each
/// code span's text stands between [`CODE_OPEN`] and [`CODE_CLOSE`]: a word diff reads it so
/// that a code span stays code (the task-6 ruling). The encoding is reversible: an authored mark
/// character (and [`CODE_ESC`]) is written after [`CODE_ESC`] ([`code_escape`]), so no authored
/// character is lost and only a code span writes a bare mark.
pub fn plain_refs_coded(
    text: &str,
    mode: RefMode,
    form: Form,
    words: &dyn Fn(&RefSpan) -> String,
    drop: &dyn Fn(&RefSpan) -> bool,
) -> String {
    plain_refs_in(text, mode, form, words, drop, true)
}

/// `words` reads every reference, one with display text too; `drop` says which a named profile
/// drops.
fn plain_refs_in(
    text: &str,
    mode: RefMode,
    form: Form,
    words: &dyn Fn(&RefSpan) -> String,
    drop: &dyn Fn(&RefSpan) -> bool,
    coded: bool,
) -> String {
    let read = |t: &str, label_form: bool| {
        let (toks, refs) = tokens(t, mode);
        let nodes = if label_form {
            parse_label(&toks)
        } else {
            parse_spans(&toks, false)
        };
        let mut out = String::new();
        render_plain(&nodes, &refs, words, drop, coded, None, &mut out);
        out
    };
    match form {
        Form::Label => read(text, true),
        Form::Spans => read(text, false),
        Form::Body => parse_body(text)
            .iter()
            .map(|b| match b {
                BodyBlock::Para(p) => read(p, false),
                BodyBlock::List(l) => {
                    let mut lines = vec![];
                    list_plain(l, &|t| read(t, false), coded, &mut lines);
                    lines.join("\n")
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
    }
}

/// Whether a body is exactly one paragraph (no list, no blank line between lines): the shape a
/// doc's lede must have.
pub fn is_one_paragraph(text: &str) -> bool {
    matches!(parse_body(text).as_slice(), [BodyBlock::Para(_)])
}

/// Source that is not Markdown (a diagram's, a figure's) as plain words: each reference in `mode`
/// reads as `words` says (called for every reference, one with display text too, as
/// [`plain_refs_every`]), each rdy escape `\{{` as `{{`, and nothing else changes. In
/// [`RefMode::Off`] it is the text unescaped.
pub fn source_plain(text: &str, mode: RefMode, words: &dyn Fn(&RefSpan) -> String) -> String {
    let mut out = String::new();
    let mut last = 0;
    for span in ref_spans(text, mode) {
        out.push_str(&unescape_braces(&text[last..span.range.start]));
        out.push_str(&words(&span));
        last = span.range.end;
    }
    out.push_str(&unescape_braces(&text[last..]));
    out
}

/// The text with each rdy escape `\{{` read as `{{` and nothing else changed: for diagram
/// source, which is not Markdown.
pub fn unescape_braces(text: &str) -> String {
    text.replace("\\{{", "{{")
}

// ---- tokens -------------------------------------------------------------------------------

/// One unit of a text: a character that may be markup, a character that is literal (escaped),
/// or a reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tok {
    Ch(char),
    Lit(char),
    Atom(usize),
}

impl Tok {
    /// The character flanking rules see: a reference counts as a letter.
    fn flank(self) -> char {
        match self {
            Tok::Ch(c) | Tok::Lit(c) => c,
            Tok::Atom(_) => 'a',
        }
    }
}

/// The text as tokens, with its references read under `mode` and each rdy escape `\{{` as two
/// literal braces.
fn tokens(text: &str, mode: RefMode) -> (Vec<Tok>, Vec<RefSpan>) {
    let refs = ref_spans(text, mode);
    let mut toks = Vec::with_capacity(text.len());
    let mut next_ref = refs.iter().enumerate().peekable();
    let mut i = 0;
    while i < text.len() {
        if let Some((k, r)) = next_ref.peek()
            && r.range.start == i
        {
            toks.push(Tok::Atom(*k));
            i = r.range.end;
            next_ref.next();
            continue;
        }
        let rest = &text[i..];
        if mode != RefMode::Off && rest.starts_with("\\{{") {
            toks.push(Tok::Lit('{'));
            toks.push(Tok::Lit('{'));
            i += 3;
            continue;
        }
        let c = rest.chars().next().expect("in bounds");
        toks.push(Tok::Ch(c));
        i += c.len_utf8();
    }
    (toks, refs)
}

// ---- spans --------------------------------------------------------------------------------

/// One parsed piece of a text. Each output (HTML, plain words) writes the same nodes.
#[derive(Clone, Debug)]
pub enum Node {
    /// Authored text, escapes resolved.
    Text(String),
    /// A reference: its index in the text's references.
    Ref(usize),
    /// A code span: its characters and references, read literally.
    Code(Vec<Node>),
    /// `**strong**` text.
    Strong(Vec<Node>),
    /// `*emphasised*` text.
    Em(Vec<Node>),
    /// A link: its authored target and its text.
    Link { target: String, children: Vec<Node> },
    /// A remote image (D-3DAX4): `![alt](https:…)`, alone or as the whole label of a link. Its
    /// alt text is never empty.
    Image { alt: String, url: String },
}

/// `text` read as spans (a one-line field): its nodes and the references they point into.
pub fn parse_spans_of(text: &str, mode: RefMode) -> (Vec<Node>, Vec<RefSpan>) {
    let (toks, refs) = tokens(text, mode);
    (parse_spans(&toks, false), refs)
}

/// `text` read as a label (references and code spans only): its nodes and the references they
/// point into.
pub fn parse_label_of(text: &str, mode: RefMode) -> (Vec<Node>, Vec<RefSpan>) {
    let (toks, refs) = tokens(text, mode);
    (parse_label(&toks), refs)
}

/// The remote image whose `[` is at `open` (right after an unescaped `!`) and whose label ends
/// at `close`: its alt text and URL and the index after it. None when it is not one rdy reads
/// (D-3DAX4 rule 1): the label must hold text and no reference, and the target must be an
/// `https:` URL that [`href`] allows, within [`MAX_LOCAL_URL`] bytes.
fn image_at(toks: &[Tok], open: usize, close: usize) -> Option<(Node, usize)> {
    let (close, url, end) = link_at(toks, open, close)?;
    let mut alt = String::new();
    let mut j = open + 1;
    while j < close {
        match toks[j] {
            Tok::Atom(_) => return None,
            Tok::Ch('\\') if matches!(toks.get(j + 1), Some(Tok::Ch(p)) if p.is_ascii_punctuation()) =>
            {
                if let Some(Tok::Ch(p)) = toks.get(j + 1) {
                    alt.push(*p);
                }
                j += 2;
                continue;
            }
            Tok::Ch(c) | Tok::Lit(c) => alt.push(c),
        }
        j += 1;
    }
    let https = url
        .get(..6)
        .is_some_and(|s| s.eq_ignore_ascii_case("https:"));
    if alt.trim().is_empty()
        || !https
        || url.len() > MAX_LOCAL_URL
        || href(&url, "").is_none_or(|(_, away)| !away)
    {
        return None;
    }
    Some((Node::Image { alt, url }, end))
}

fn is_punct(c: char) -> bool {
    c.is_ascii_punctuation() || "“”‘’«»‹›—–…".contains(c)
}

/// Pushes a character, joining it to the text before it.
fn push_char(out: &mut Vec<Node>, c: char) {
    match out.last_mut() {
        Some(Node::Text(t)) => t.push(c),
        _ => out.push(Node::Text(c.to_string())),
    }
}

fn push_str(out: &mut Vec<Node>, s: &str) {
    for c in s.chars() {
        push_char(out, c);
    }
}

/// The length of the run of backticks at `i`.
fn tick_run(toks: &[Tok], i: usize) -> usize {
    toks[i..].iter().take_while(|t| **t == Tok::Ch('`')).count()
}

/// The end of a code span opened by the run of `n` backticks at `i` (the index after its
/// closing run), if a run of exactly `n` closes it.
fn code_end(toks: &[Tok], i: usize, n: usize) -> Option<usize> {
    let mut j = i + n;
    while j < toks.len() {
        if toks[j] == Tok::Ch('`') {
            let m = tick_run(toks, j);
            if m == n {
                return Some(j + m);
            }
            j += m;
        } else {
            j += 1;
        }
    }
    None
}

/// The literal content of a code span: its characters, and its references.
fn code_nodes(toks: &[Tok]) -> Vec<Node> {
    let mut out = vec![];
    for t in toks {
        match *t {
            Tok::Ch(c) | Tok::Lit(c) => push_char(&mut out, c),
            Tok::Atom(k) => out.push(Node::Ref(k)),
        }
    }
    out
}

/// A label: references and code spans; every other character as written.
fn parse_label(toks: &[Tok]) -> Vec<Node> {
    let mut out = vec![];
    let mut i = 0;
    while i < toks.len() {
        match toks[i] {
            Tok::Atom(k) => {
                out.push(Node::Ref(k));
                i += 1;
            }
            Tok::Ch('`') => {
                let n = tick_run(toks, i);
                match code_end(toks, i, n) {
                    Some(end) => {
                        out.push(Node::Code(code_nodes(&toks[i + n..end - n])));
                        i = end;
                    }
                    None => {
                        push_str(&mut out, &"`".repeat(n));
                        i += n;
                    }
                }
            }
            Tok::Ch(c) | Tok::Lit(c) => {
                push_char(&mut out, c);
                i += 1;
            }
        }
    }
    out
}

/// The `]` that closes each `[`, or `NO_CLOSE`: balanced brackets, skipping escapes and code
/// spans. One pass, left to right, so an unclosed `[` costs nothing more than a closed one.
fn bracket_closes(toks: &[Tok]) -> Vec<usize> {
    let mut closes = vec![NO_CLOSE; toks.len()];
    let mut open: Vec<usize> = vec![];
    let mut j = 0;
    while j < toks.len() {
        match toks[j] {
            Tok::Ch('[') => open.push(j),
            Tok::Ch(']') => {
                if let Some(o) = open.pop() {
                    closes[o] = j;
                    // a link: its target is read as a target, not as code spans, so skip past
                    // it as `parse_spans` does. A scan starts only at a `](`, and one that
                    // fails stops at the next `(`, so the scans stay linear overall.
                    let image = o > 0 && matches!(toks[o - 1], Tok::Ch('!'));
                    if !(image && was_bang_at(toks, o))
                        && let Some((_, _, end)) = link_at(toks, o, j)
                    {
                        j = end;
                        continue;
                    }
                }
            }
            Tok::Ch('\\') if matches!(toks.get(j + 1), Some(Tok::Ch(p)) if p.is_ascii_punctuation()) => {
                j += 1
            }
            Tok::Ch('`') => {
                let n = tick_run(toks, j);
                j = code_end(toks, j, n).unwrap_or(j + n);
                continue;
            }
            _ => {}
        }
        j += 1;
    }
    closes
}

/// Whether the `[` at `o` follows an unescaped `!`.
fn was_bang_at(toks: &[Tok], o: usize) -> bool {
    let slashes = toks[..o.saturating_sub(1)]
        .iter()
        .rev()
        .take_while(|t| **t == Tok::Ch('\\'))
        .count();
    slashes % 2 == 0
}

/// No `]` closes this `[`.
const NO_CLOSE: usize = usize::MAX;

/// A link at `i` (a `[`) whose text ends at `close`: the end of its text, its target and the
/// index after it.
fn link_at(toks: &[Tok], i: usize, close: usize) -> Option<(usize, String, usize)> {
    if close == NO_CLOSE {
        return None;
    }
    if close == i + 1 || toks.get(close + 1) != Some(&Tok::Ch('(')) {
        return None;
    }
    // the target: no whitespace, no reference, at most one level of balanced parentheses
    let (mut target, mut depth, mut j) = (String::new(), 0usize, close + 2);
    loop {
        let c = match *toks.get(j)? {
            Tok::Atom(_) => return None,
            Tok::Ch('\\') => match toks.get(j + 1) {
                // an escaped character is literal: it neither opens nor closes
                Some(Tok::Ch(p)) if p.is_ascii_punctuation() => {
                    target.push(*p);
                    j += 2;
                    continue;
                }
                _ => '\\',
            },
            Tok::Ch(')') if depth == 0 => break,
            Tok::Ch('(') => {
                depth += 1;
                if depth > 1 {
                    return None;
                }
                '('
            }
            Tok::Ch(')') => {
                depth -= 1;
                ')'
            }
            Tok::Ch(c) | Tok::Lit(c) => c,
        };
        if c.is_whitespace() {
            return None;
        }
        target.push(c);
        j += 1;
    }
    (!target.is_empty()).then_some((close, target, j + 1))
}

/// One piece of a run of spans before emphasis is matched.
enum Piece {
    Node(Node),
    Delim { len: usize, open: bool, close: bool },
}

/// Spans: escapes, code spans, links (not inside a link's own text) and emphasis.
fn parse_spans(toks: &[Tok], in_link: bool) -> Vec<Node> {
    let mut pieces: Vec<Piece> = vec![];
    let text = |pieces: &mut Vec<Piece>, c: char| match pieces.last_mut() {
        Some(Piece::Node(Node::Text(t))) => t.push(c),
        _ => pieces.push(Piece::Node(Node::Text(c.to_string()))),
    };
    // a link's own text holds no link, so only the outermost run needs its brackets matched
    let closes = if in_link {
        vec![]
    } else {
        bracket_closes(toks)
    };
    let mut i = 0;
    // whether the character before is an unescaped `!` (an image, which is not supported)
    let mut bang = false;
    while i < toks.len() {
        let was_bang = std::mem::take(&mut bang);
        match toks[i] {
            Tok::Atom(k) => {
                pieces.push(Piece::Node(Node::Ref(k)));
                i += 1;
            }
            Tok::Lit(c) => {
                text(&mut pieces, c);
                i += 1;
            }
            Tok::Ch('\\') => match toks.get(i + 1) {
                Some(Tok::Ch(p)) if p.is_ascii_punctuation() => {
                    text(&mut pieces, *p);
                    i += 2;
                }
                _ => {
                    text(&mut pieces, '\\');
                    i += 1;
                }
            },
            Tok::Ch('`') => {
                let n = tick_run(toks, i);
                match code_end(toks, i, n) {
                    Some(end) => {
                        pieces.push(Piece::Node(Node::Code(code_nodes(&toks[i + n..end - n]))));
                        i = end;
                    }
                    None => {
                        for _ in 0..n {
                            text(&mut pieces, '`');
                        }
                        i += n;
                    }
                }
            }
            Tok::Ch('[') if !in_link && !was_bang => match link_at(toks, i, closes[i]) {
                Some((close, target, end)) => {
                    // a label that is exactly one remote image is that image (D-3DAX4)
                    let label = &toks[i + 1..close];
                    let image = (label.len() > 1 && label[0] == Tok::Ch('!'))
                        .then(|| image_at(label, 1, bracket_closes(label)[1]))
                        .flatten()
                        .filter(|(_, after)| *after == label.len());
                    let children = match image {
                        Some((img, _)) => vec![img],
                        None => parse_spans(label, true),
                    };
                    pieces.push(Piece::Node(Node::Link { target, children }));
                    i = end;
                }
                None => {
                    text(&mut pieces, '[');
                    i += 1;
                }
            },
            Tok::Ch('*') => {
                let n = toks[i..].iter().take_while(|t| **t == Tok::Ch('*')).count();
                let prev = i.checked_sub(1).map(|p| toks[p].flank());
                let next = toks.get(i + n).map(|t| t.flank());
                let open = next.is_some_and(|c| {
                    !c.is_whitespace()
                        && (!is_punct(c) || prev.is_none_or(|p| p.is_whitespace() || is_punct(p)))
                });
                let close = prev.is_some_and(|c| {
                    !c.is_whitespace()
                        && (!is_punct(c) || next.is_none_or(|n| n.is_whitespace() || is_punct(n)))
                });
                if n >= 3 || !(open || close) {
                    for _ in 0..n {
                        text(&mut pieces, '*');
                    }
                } else {
                    pieces.push(Piece::Delim {
                        len: n,
                        open,
                        close,
                    });
                }
                i += n;
            }
            // a remote image, outside a link's own label (where only a whole label is one)
            Tok::Ch('!')
                if !in_link
                    && toks.get(i + 1) == Some(&Tok::Ch('['))
                    && let Some((img, end)) = image_at(toks, i + 1, closes[i + 1]) =>
            {
                pieces.push(Piece::Node(img));
                i = end;
            }
            Tok::Ch(c) => {
                bang = c == '!';
                text(&mut pieces, c);
                i += 1;
            }
        }
    }
    // a closer takes the nearest open opener of its length; the openers between become literal.
    // One stack per length keeps this linear: the nearest opener of a length is its stack's top.
    let mut pair: Vec<Option<usize>> = vec![None; pieces.len()];
    let mut openers: [Vec<usize>; 2] = [vec![], vec![]];
    for (d, p) in pieces.iter().enumerate() {
        let Piece::Delim { len, open, close } = *p else {
            continue;
        };
        let (mine, other) = (len - 1, 2 - len);
        match openers[mine].last().copied().filter(|_| close) {
            Some(o) => {
                pair[o] = Some(d);
                openers[mine].pop();
                while openers[other].last().is_some_and(|&x| x > o) {
                    openers[other].pop();
                }
            }
            None if open => openers[mine].push(d),
            None => {}
        }
    }
    let mut pieces: Vec<Option<Piece>> = pieces.into_iter().map(Some).collect();
    build(&mut pieces, &pair, 0, pair.len(), 0)
}

/// The most levels emphasis nests; a pair deeper than this is literal, so that no text can make
/// the renderer recurse without bound.
const MAX_EMPHASIS: usize = 16;

/// The nodes of `pieces[lo..hi]`, with each matched pair of delimiters around what it encloses.
fn build(
    pieces: &mut [Option<Piece>],
    pair: &[Option<usize>],
    lo: usize,
    hi: usize,
    depth: usize,
) -> Vec<Node> {
    let mut out = vec![];
    let mut i = lo;
    while i < hi {
        match pieces[i].take() {
            Some(Piece::Delim { len, .. }) => match pair[i] {
                Some(end) if depth < MAX_EMPHASIS => {
                    let inner = build(pieces, pair, i + 1, end, depth + 1);
                    out.push(if len == 2 {
                        Node::Strong(inner)
                    } else {
                        Node::Em(inner)
                    });
                    i = end + 1;
                    continue;
                }
                _ => push_str(&mut out, &"*".repeat(len)),
            },
            Some(Piece::Node(Node::Text(t))) => push_str(&mut out, &t),
            Some(Piece::Node(n)) => out.push(n),
            None => {}
        }
        i += 1;
    }
    out
}

// ---- links --------------------------------------------------------------------------------

/// The href of a link target as the page being written sees it, and whether it leaves the site;
/// none for a target that is not allowed (a scheme other than `http`, `https` or `mailto`, a
/// protocol-relative `//host`, a control character or a backslash).
pub fn href(target: &str, doc_page: &str) -> Option<(String, bool)> {
    if target.is_empty()
        || target.starts_with("//")
        || target
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return None;
    }
    let head_end = target.find(['/', '?', '#']).unwrap_or(target.len());
    if let Some((scheme, _)) = target[..head_end].split_once(':') {
        return ["http", "https", "mailto"]
            .iter()
            .any(|s| scheme.eq_ignore_ascii_case(s))
            .then(|| (target.to_string(), true));
    }
    if target.starts_with('/') || doc_page.is_empty() {
        return Some((target.to_string(), false));
    }
    if target.starts_with(['#', '?']) {
        return Some((format!("{doc_page}{target}"), false));
    }
    // a relative path: from the doc's page's folder, then `..` and `.` folded
    let dir = doc_page.rfind('/').map_or("", |i| &doc_page[..=i]);
    let split = target.find(['?', '#']).unwrap_or(target.len());
    let (path, tail) = target.split_at(split);
    let full = format!("{dir}{path}");
    let mut segs: Vec<&str> = vec![];
    for seg in full.split('/') {
        match seg {
            "." => {}
            ".." if segs.last().is_some_and(|s| *s != "..") => {
                segs.pop();
            }
            s => segs.push(s),
        }
    }
    let joined = segs.join("/");
    let path = if joined.is_empty() { "./" } else { &joined };
    Some((format!("{path}{tail}"), false))
}

/// Whether `c` may stay alone in a pair of parentheses that dropped references leave: white
/// space, a comma or a semicolon (D-7M0YF).
fn filler(c: char) -> bool {
    c.is_whitespace() || c == ',' || c == ';'
}

/// `nodes` as a named profile shows them (D-7M0YF): a reference that `drop` says the output
/// drops shows nothing. When dropped references leave a pair of parentheses with only white
/// space, commas or semicolons inside, the pair goes too, with the white space before it. Other
/// parentheses stay as written. With no dropped reference, `nodes` is unchanged.
pub fn prune(nodes: &[Node], refs: &[RefSpan], drop: &dyn Fn(&RefSpan) -> bool) -> Vec<Node> {
    let dropped = |n: &Node| matches!(n, Node::Ref(k) if drop(&refs[*k]));
    let mut out = nodes.to_vec();
    let mut i = 0;
    while i < out.len() {
        if !dropped(&out[i]) {
            i += 1;
            continue;
        }
        // the text that opens the pair: only filler and dropped references between
        let mut open = None;
        for j in (0..i).rev() {
            match &out[j] {
                n if dropped(n) => {}
                Node::Text(t) => {
                    let kept = t.trim_end_matches(filler);
                    if kept.ends_with('(') {
                        open = Some((j, kept.len() - 1));
                    }
                    if !kept.is_empty() {
                        break;
                    }
                }
                _ => break,
            }
        }
        // the text that closes it, the same way
        let mut close = None;
        for (k, n) in out.iter().enumerate().skip(i + 1) {
            match n {
                n if dropped(n) => {}
                Node::Text(t) => {
                    let kept = t.trim_start_matches(filler);
                    if kept.starts_with(')') {
                        close = Some((k, t.len() - kept.len() + 1));
                    }
                    if !kept.is_empty() {
                        break;
                    }
                }
                _ => break,
            }
        }
        let (Some((j, at)), Some((k, after))) = (open, close) else {
            i += 1;
            continue;
        };
        let (Node::Text(left), Node::Text(right)) = (&out[j], &out[k]) else {
            unreachable!("the pair opens and closes in text");
        };
        let left = left[..at].trim_end().to_string();
        let right = right[after..].to_string();
        out.splice(j..=k, [Node::Text(left), Node::Text(right)]);
        i = j + 2;
    }
    out
}

// ---- plain output ------------------------------------------------------------------------

/// The authored character right after `nodes[i]`: the first character of the text, emphasis or
/// strong run that follows it (else `after`, what follows `nodes`). Another reference, a code span
/// and a link are not authored text here; none of them starts a run that this reads.
pub fn next_char(nodes: &[Node], i: usize, after: Option<char>) -> Option<char> {
    match nodes.get(i + 1) {
        None => after,
        Some(Node::Text(t)) => t.chars().next(),
        Some(Node::Strong(c) | Node::Em(c)) => match c.first() {
            Some(Node::Text(t)) => t.chars().next(),
            _ => None,
        },
        Some(_) => None,
    }
}

/// `words`, the reading of the reference `span`, without its last period when it is a label the
/// reference chose (no display text) and the authored character right after the reference is a
/// period: the authored period then ends the sentence alone. Other text is unchanged.
pub fn one_period(span: &RefSpan, mut words: String, next: Option<char>) -> String {
    if span.display.is_none() && next == Some('.') && words.ends_with('.') {
        words.pop();
    }
    words
}

/// `label` reads every reference, one with display text too; `drop` says which references a
/// named profile drops ([`prune`]).
fn render_plain(
    nodes: &[Node],
    refs: &[RefSpan],
    label: &dyn Fn(&RefSpan) -> String,
    drop: &dyn Fn(&RefSpan) -> bool,
    coded: bool,
    after: Option<char>,
    out: &mut String,
) {
    let nodes = &prune(nodes, refs, drop);
    // with `coded`, only a code span writes a bare mark: authored text is escaped
    let push = |out: &mut String, t: &str| {
        if coded {
            out.push_str(&code_escape(t));
        } else {
            out.push_str(t);
        }
    };
    for (i, n) in nodes.iter().enumerate() {
        let next = next_char(nodes, i, after);
        match n {
            Node::Text(t) => push(out, t),
            Node::Ref(k) => {
                let span = &refs[*k];
                push(out, &one_period(span, label(span), next))
            }
            Node::Code(c) if coded => {
                out.push(CODE_OPEN);
                render_plain(c, refs, label, drop, coded, None, out);
                out.push(CODE_CLOSE);
            }
            Node::Code(c) => render_plain(c, refs, label, drop, coded, None, out),
            Node::Strong(c) | Node::Em(c) => render_plain(c, refs, label, drop, coded, next, out),
            Node::Link { children, .. } => {
                render_plain(children, refs, label, drop, coded, next, out)
            }
            Node::Image { alt, .. } => push(out, alt),
        }
    }
}

/// The plain lines of a list: each paragraph read by `words`, and fenced code as its literal
/// text (in the coded form, between the code marks).
fn list_plain(l: &List, words: &dyn Fn(&str) -> String, coded: bool, lines: &mut Vec<String>) {
    for it in &l.items {
        for part in &it.parts {
            match part {
                Part::Para(t) => lines.push(words(t)),
                Part::Code { text, .. } if coded => {
                    lines.push(format!("{CODE_OPEN}{}{CODE_CLOSE}", code_escape(text)))
                }
                Part::Code { text, .. } => lines.push(text.clone()),
                Part::List(sub) => list_plain(sub, words, coded, lines),
            }
        }
    }
}

// ---- coded text read back -----------------------------------------------------------------

/// One character of a coded text as a reader reads it ([`plain_refs_coded`]), and the code span
/// it is in: `0` for none, else the span's number in that text (two neighbouring spans stay two).
pub type Ch = (char, u32);

/// Reads coded text back ([`plain_refs_coded`]), in order, across the pieces of one side's text.
#[derive(Default)]
pub struct Decoder {
    /// The last character was [`CODE_ESC`]: the next one is authored text.
    esc: bool,
    span: u32,
    spans: u32,
}

impl Decoder {
    /// The characters of `text`, each with its code span; the marks themselves are dropped.
    pub fn read(&mut self, text: &str) -> Vec<Ch> {
        let mut out = vec![];
        for c in text.chars() {
            if std::mem::take(&mut self.esc) {
                out.push((c, self.span));
                continue;
            }
            match c {
                CODE_ESC => self.esc = true,
                CODE_OPEN => {
                    self.spans += 1;
                    self.span = self.spans;
                }
                CODE_CLOSE => self.span = 0,
                _ => out.push((c, self.span)),
            }
        }
        out
    }
}
