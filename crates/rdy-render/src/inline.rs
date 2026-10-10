//! The inline subset of Markdown (md1) every running text on the site is read in, and the
//! display text of references (dtext1).
//!
//! References are read first ([`ref_spans`]): each is one opaque atom, and a `\` right before
//! `{{` is the rdy escape (a literal `{{`). Then, outside atoms: backslash escapes, code spans,
//! links and emphasis (`*` and `**` only: an underscore is always literal, FW-F1a). A body is
//! grouped into paragraphs and lists. Anything else is text, escaped.
use crate::html::{Link, UNRESOLVED, escape};
use rdy_model::{BlockId, RefMode, RefSpan, ref_spans};

/// How a text's references and relative links are read on the page being written.
pub struct Ctx<'a> {
    /// Where a reference leads from the page being written.
    pub link: &'a dyn Fn(&BlockId) -> Option<Link>,
    /// The href of the page of the doc that holds the text, from the page being written, with no
    /// anchor; `""` when that is the page being written.
    pub doc_page: &'a str,
}

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

/// The most lists a body nests.
const MAX_DEPTH: usize = 6;

/// A body as HTML: paragraphs and lists. A blank body is the empty string.
pub fn body_html(text: &str, ctx: &Ctx) -> String {
    let mut out = String::new();
    for b in parse_body(text) {
        match b {
            Block::Para(p) => {
                out.push_str("<p>");
                out.push_str(&spans_html(&p, ctx));
                out.push_str("</p>");
            }
            Block::List(l) => list_html(&l, ctx, &mut out),
        }
    }
    out
}

/// As [`body_html`], but a body that is exactly one paragraph comes back without `<p>`.
pub fn body_inline_html(text: &str, ctx: &Ctx) -> String {
    match parse_body(text).as_slice() {
        [Block::Para(p)] => spans_html(p, ctx),
        _ => body_html(text, ctx),
    }
}

/// A one-line field as HTML: spans only. A newline is `<br>`, a leading `- ` is text.
pub fn spans_html(text: &str, ctx: &Ctx) -> String {
    let (toks, refs) = tokens(text, RefMode::Strict);
    let nodes = parse_spans(&toks, false);
    let mut out = String::new();
    render_html(&nodes, &refs, ctx, false, &mut out);
    out
}

/// A label as HTML: references and code spans only.
pub fn label_html(text: &str, ctx: &Ctx) -> String {
    let (toks, refs) = tokens(text, RefMode::Strict);
    let nodes = parse_label(&toks);
    let mut out = String::new();
    render_html(&nodes, &refs, ctx, false, &mut out);
    out
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
    let words = |t: &str, label_form: bool| {
        let (toks, refs) = tokens(t, mode);
        let nodes = if label_form {
            parse_label(&toks)
        } else {
            parse_spans(&toks, false)
        };
        let mut out = String::new();
        render_plain(&nodes, &refs, label, &mut out);
        out
    };
    match form {
        Form::Label => words(text, true),
        Form::Spans => words(text, false),
        Form::Body => parse_body(text)
            .iter()
            .map(|b| match b {
                Block::Para(p) => words(p, false),
                Block::List(l) => {
                    let mut lines = vec![];
                    list_plain(l, &|t| words(t, false), &mut lines);
                    lines.join("\n")
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
    }
}

/// Whether a body is exactly one paragraph (no list, no blank line between lines): the shape a
/// doc's lede must have.
pub(crate) fn is_one_paragraph(text: &str) -> bool {
    matches!(parse_body(text).as_slice(), [Block::Para(_)])
}

/// Source that is not Markdown (a diagram's, a figure's) as plain words: each reference in `mode`
/// reads as its display text, else as `label` says (called only for one with none), each rdy
/// escape `\{{` as `{{`, and nothing else changes. In [`RefMode::Off`] it is the text unescaped.
pub(crate) fn source_plain(
    text: &str,
    mode: RefMode,
    label: &dyn Fn(&RefSpan) -> String,
) -> String {
    let mut out = String::new();
    let mut last = 0;
    for span in ref_spans(text, mode) {
        out.push_str(&unescape_braces(&text[last..span.range.start]));
        match &span.display {
            Some(d) => out.push_str(d),
            None => out.push_str(&label(&span)),
        }
        last = span.range.end;
    }
    out.push_str(&unescape_braces(&text[last..]));
    out
}

/// The text with each rdy escape `\{{` read as `{{` and nothing else changed: for diagram
/// source, which is not Markdown.
pub(crate) fn unescape_braces(text: &str) -> String {
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

#[derive(Clone, Debug)]
enum Node {
    Text(String),
    Ref(usize),
    Code(Vec<Node>),
    Strong(Vec<Node>),
    Em(Vec<Node>),
    Link { target: String, children: Vec<Node> },
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
                    let children = parse_spans(&toks[i + 1..close], true);
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
fn href(target: &str, doc_page: &str) -> Option<(String, bool)> {
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

// ---- output -------------------------------------------------------------------------------

/// What a reference reads as in words: its display text, the target's key when the author typed
/// a key and the target still has one, else the target's label (the unresolved marker's words
/// when it leads nowhere, unless the author gave display text).
fn ref_words(span: &RefSpan, link: Option<&Link>) -> String {
    if let Some(d) = &span.display {
        return d.clone();
    }
    match link {
        Some(l) => match (&l.key, span.key_typed) {
            (Some(k), true) if !k.is_empty() => k.clone(),
            _ => l.label.clone(),
        },
        None => UNRESOLVED.to_string(),
    }
}

fn target_of(span: &RefSpan, ctx: &Ctx) -> Option<Link> {
    BlockId::parse(&span.token)
        .ok()
        .and_then(|id| (ctx.link)(&id))
}

fn text_html(s: &str) -> String {
    escape(s).replace('\n', "<br>")
}

fn render_html(nodes: &[Node], refs: &[RefSpan], ctx: &Ctx, in_link: bool, out: &mut String) {
    for n in nodes {
        match n {
            Node::Text(t) => out.push_str(&text_html(t)),
            Node::Ref(k) => {
                let span = &refs[*k];
                let link = target_of(span, ctx);
                let words = text_html(&ref_words(span, link.as_ref()));
                match link {
                    _ if in_link => out.push_str(&words),
                    Some(l) if l.title.is_empty() => {
                        out.push_str(&format!("<a href=\"{}\">{words}</a>", escape(&l.href)))
                    }
                    Some(l) => out.push_str(&format!(
                        "<a href=\"{}\" title=\"{}\">{words}</a>",
                        escape(&l.href),
                        escape(&l.title)
                    )),
                    None => out.push_str(&format!(
                        "<span class=\"ref unresolved\" title=\"This reference could not be resolved\">{words}</span>"
                    )),
                }
            }
            Node::Code(c) => {
                out.push_str("<code>");
                render_html(c, refs, ctx, in_link, out);
                out.push_str("</code>");
            }
            Node::Strong(c) => {
                out.push_str("<strong>");
                render_html(c, refs, ctx, in_link, out);
                out.push_str("</strong>");
            }
            Node::Em(c) => {
                out.push_str("<em>");
                render_html(c, refs, ctx, in_link, out);
                out.push_str("</em>");
            }
            Node::Link { target, children } => match href(target, ctx.doc_page) {
                Some((h, away)) => {
                    out.push_str(&format!(
                        "<a href=\"{}\"{}>",
                        escape(&h),
                        if away {
                            " rel=\"noopener noreferrer\""
                        } else {
                            ""
                        }
                    ));
                    render_html(children, refs, ctx, true, out);
                    out.push_str("</a>");
                }
                None => render_html(children, refs, ctx, true, out),
            },
        }
    }
}

fn render_plain(
    nodes: &[Node],
    refs: &[RefSpan],
    label: &dyn Fn(&RefSpan) -> String,
    out: &mut String,
) {
    for n in nodes {
        match n {
            Node::Text(t) => out.push_str(t),
            Node::Ref(k) => {
                let span = &refs[*k];
                match &span.display {
                    Some(d) => out.push_str(d),
                    None => out.push_str(&label(span)),
                }
            }
            Node::Code(c) | Node::Strong(c) | Node::Em(c) => render_plain(c, refs, label, out),
            Node::Link { children, .. } => render_plain(children, refs, label, out),
        }
    }
}

// ---- bodies -------------------------------------------------------------------------------

#[derive(Debug)]
enum Block {
    Para(String),
    List(List),
}

#[derive(Debug)]
struct List {
    ordered: bool,
    start: u32,
    indent: usize,
    items: Vec<Item>,
}

#[derive(Debug)]
struct Item {
    text: String,
    lists: Vec<List>,
}

/// A line's indent: leading spaces, a tab counting as 4.
fn indent(line: &str) -> usize {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

/// A marker line: its indent, whether it is numbered, its number and its text.
fn marker(line: &str) -> Option<(usize, bool, u32, &str)> {
    let rest = line.trim_start_matches([' ', '\t']);
    let (ordered, number, after) = if let Some(r) = rest.strip_prefix(['-', '*']) {
        (false, 1, r)
    } else {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if !(1..=3).contains(&digits) {
            return None;
        }
        let r = rest[digits..].strip_prefix('.')?;
        (true, rest[..digits].parse().ok()?, r)
    };
    if !after.starts_with([' ', '\t']) {
        return None;
    }
    let text = after.trim();
    (!text.is_empty()).then_some((indent(line), ordered, number, text))
}

fn parse_body(text: &str) -> Vec<Block> {
    let lines: Vec<&str> = text.lines().collect();
    // the first line at or after each line that is not blank: one reverse pass, not a scan per
    // blank line
    let mut next_nonblank = vec![lines.len(); lines.len() + 1];
    for (i, l) in lines.iter().enumerate().rev() {
        next_nonblank[i] = if l.trim().is_empty() {
            next_nonblank[i + 1]
        } else {
            i
        };
    }
    let mut out: Vec<Block> = vec![];
    let mut para: Vec<&str> = vec![];
    let mut stack: Vec<List> = vec![];
    fn flush(para: &mut Vec<&str>, out: &mut Vec<Block>) {
        if !para.is_empty() {
            out.push(Block::Para(para.join("\n")));
            para.clear();
        }
    }
    fn close_one(stack: &mut Vec<List>, out: &mut Vec<Block>) {
        if let Some(l) = stack.pop() {
            match stack.last_mut().and_then(|p| p.items.last_mut()) {
                Some(item) => item.lists.push(l),
                None => out.push(Block::List(l)),
            }
        }
    }
    fn close_all(stack: &mut Vec<List>, out: &mut Vec<Block>) {
        while !stack.is_empty() {
            close_one(stack, out);
        }
    }
    let open = |indent: usize, ordered: bool, start: u32, text: &str| List {
        ordered,
        start,
        indent,
        items: vec![Item {
            text: text.to_string(),
            lists: vec![],
        }],
    };
    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            flush(&mut para, &mut out);
            if let Some(inner) = stack.last() {
                let next = lines.get(next_nonblank[i + 1]);
                let keep = next.is_some_and(|n| marker(n).is_some() || indent(n) > inner.indent);
                if !keep {
                    close_all(&mut stack, &mut out);
                }
            }
            continue;
        }
        if let Some((ind, ordered, number, item)) = marker(line) {
            flush(&mut para, &mut out);
            while stack.last().is_some_and(|l| l.indent > ind) {
                close_one(&mut stack, &mut out);
            }
            let new_item = Item {
                text: item.to_string(),
                lists: vec![],
            };
            let depth = stack.len();
            match stack.last_mut() {
                Some(l) if l.indent == ind && l.ordered == ordered => l.items.push(new_item),
                Some(l) if l.indent == ind => {
                    close_one(&mut stack, &mut out);
                    stack.push(open(ind, ordered, number, item));
                }
                // deeper than a list may nest: a sibling of the innermost list's items
                Some(l) if depth >= MAX_DEPTH => l.items.push(new_item),
                _ => stack.push(open(ind, ordered, number, item)),
            }
            continue;
        }
        match stack.last_mut() {
            Some(l) if indent(line) > l.indent => {
                if let Some(it) = l.items.last_mut() {
                    it.text.push('\n');
                    it.text.push_str(line.trim());
                }
            }
            _ => {
                close_all(&mut stack, &mut out);
                para.push(line);
            }
        }
    }
    flush(&mut para, &mut out);
    close_all(&mut stack, &mut out);
    out
}

fn list_html(l: &List, ctx: &Ctx, out: &mut String) {
    let tag = if l.ordered { "ol" } else { "ul" };
    if l.ordered && l.start != 1 {
        out.push_str(&format!("<ol start=\"{}\">", l.start));
    } else {
        out.push_str(&format!("<{tag}>"));
    }
    for it in &l.items {
        out.push_str("<li>");
        out.push_str(&spans_html(&it.text, ctx));
        for sub in &it.lists {
            list_html(sub, ctx, out);
        }
        out.push_str("</li>");
    }
    out.push_str(&format!("</{tag}>"));
}

fn list_plain(l: &List, words: &dyn Fn(&str) -> String, lines: &mut Vec<String>) {
    for it in &l.items {
        lines.push(words(&it.text));
        for sub in &it.lists {
            list_plain(sub, words, lines);
        }
    }
}
