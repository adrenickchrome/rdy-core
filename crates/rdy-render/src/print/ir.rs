//! The print model's reader of the renderer's own HTML: the blocks and runs a reader sees on a
//! page, and nothing else.
//!
//! It reads a closed list of elements: `p`, `h1`–`h6`, `ul`, `ol` (`start`), `li`, `table`,
//! `thead`, `tbody`, `tr`, `th`, `td`, `pre` (with `code` and its `language-*` class), `code`,
//! `strong`, `em`, `a` (`href`), `br`, `figure`, `img` (`src`, `alt`), `figcaption`, `aside` (a
//! callout; its `span.marker` is the label), `div.normative` (its `span.marker` leads the
//! paragraph in bold), `dl`/`dt`/`dd` (a term in bold, then its text) and `details`/`summary`
//! (a diagram's source: a code block in the diagram's language). Any other element adds its
//! text as plain runs; an element [`unseen`] names adds nothing.
use super::{Href, PBlock, Run};
use crate::llms::{attr, decode, unseen};
use rdy_model::Hash;
use std::cell::RefCell;
use std::collections::BTreeSet;

/// A node of the renderer's HTML: text (decoded), or an element with its start tag's text (its
/// name, then its attributes) and its children.
#[derive(Debug)]
enum Node {
    Text(String),
    Elem {
        name: String,
        tag: String,
        children: Vec<Node>,
    },
}

/// The elements that have no closing tag.
const VOID: [&str; 6] = ["br", "img", "hr", "input", "meta", "link"];

/// The elements that part blocks: outside the closed list, one of these is a container whose
/// children are read as blocks; any other element sits inside a line of text.
const BLOCKISH: [&str; 13] = [
    "div",
    "header",
    "footer",
    "article",
    "section",
    "main",
    "blockquote",
    "caption",
    "hr",
    "thead",
    "tbody",
    "tr",
    "li",
];

/// The tree of a piece of the renderer's HTML. A closing tag closes the nearest open element of
/// its name; one that matches nothing open is ignored.
fn parse(html: &str) -> Vec<Node> {
    // the open elements: (name, tag, children); the root at the bottom
    let mut stack: Vec<(String, String, Vec<Node>)> = vec![(String::new(), String::new(), vec![])];
    let mut rest = html;
    loop {
        let (text, tag) = match rest.find('<') {
            Some(i) => {
                let close = rest[i..].find('>').map_or(rest.len(), |e| i + e);
                (&rest[..i], Some(&rest[i + 1..close]))
            }
            None => (rest, None),
        };
        if !text.is_empty() {
            let top = &mut stack.last_mut().expect("the root is never closed").2;
            top.push(Node::Text(decode(text)));
        }
        let Some(tag) = tag else { break };
        rest = rest.get(text.len() + tag.len() + 2..).unwrap_or_default();
        let closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if name.is_empty() || name.starts_with('!') {
            continue;
        }
        if closing {
            if let Some(at) = stack.iter().rposition(|(n, _, _)| *n == name)
                && at > 0
            {
                while stack.len() > at {
                    let (name, tag, children) = stack.pop().expect("above the root");
                    let top = &mut stack.last_mut().expect("the root is never closed").2;
                    top.push(Node::Elem {
                        name,
                        tag,
                        children,
                    });
                }
            }
            continue;
        }
        if VOID.contains(&name.as_str()) || tag.ends_with('/') {
            let top = &mut stack.last_mut().expect("the root is never closed").2;
            top.push(Node::Elem {
                name,
                tag: tag.to_string(),
                children: vec![],
            });
        } else {
            stack.push((name, tag.to_string(), vec![]));
        }
    }
    while stack.len() > 1 {
        let (name, tag, children) = stack.pop().expect("above the root");
        let top = &mut stack.last_mut().expect("the root is never closed").2;
        top.push(Node::Elem {
            name,
            tag,
            children,
        });
    }
    stack.pop().map(|(_, _, c)| c).unwrap_or_default()
}

/// Whether the start tag has the class `class`.
fn has_class(tag: &str, class: &str) -> bool {
    attr(tag, "class").is_some_and(|c| c.split_whitespace().any(|c| c == class))
}

/// An element's `id`, when it has a non-empty one.
fn id_of(tag: &str) -> Option<String> {
    attr(tag, "id").map(decode).filter(|a| !a.is_empty())
}

/// Where an `href` (or a `src`) on `page` leads, as [`crate::site::Url::href_from`] wrote it:
/// `http`, `https` and `mailto` lead out of the print; a relative target is a page of the site
/// (its path resolved against `page`) and its anchor. Any other target (another scheme, an
/// absolute path, a path above the site's root) leads nowhere a print knows.
fn resolve(href: &str, page: &str) -> Option<Href> {
    let lower = href.to_ascii_lowercase();
    if ["http://", "https://", "mailto:"]
        .iter()
        .any(|s| lower.starts_with(s))
    {
        return Some(Href::External(href.to_string()));
    }
    let (path, anchor) = match href.split_once('#') {
        Some((p, a)) => (p, (!a.is_empty()).then(|| a.to_string())),
        None => (href, None),
    };
    // a scheme names no page of the site
    let first = path.split(['/', '?']).next().unwrap_or("");
    if first.contains(':') || path.starts_with('/') || path.contains('?') {
        return None;
    }
    if path.is_empty() {
        return Some(Href::Page {
            page: page.to_string(),
            anchor,
        });
    }
    let mut parts: Vec<&str> = page.split('/').collect();
    parts.pop();
    for seg in path.split('/') {
        match seg {
            "." | "" => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(Href::Page {
        page: parts.join("/"),
        anchor,
    })
}

/// The blob an image's `src` on `page` names, and the site file it names it by: a site asset is
/// `assets/<hash>.<extension>`.
fn asset_of(src: &str, page: &str) -> Option<(Hash, String)> {
    match resolve(src, page)? {
        Href::Page { page, anchor: None } => {
            let file = page.strip_prefix("assets/")?;
            let (stem, _ext) = file.split_once('.')?;
            Some((stem.parse().ok()?, page))
        }
        _ => None,
    }
}

/// How the runs being read look.
#[derive(Clone, Default)]
struct Style {
    bold: bool,
    italic: bool,
    code: bool,
    link: Option<Href>,
}

impl Style {
    fn run(&self, text: String) -> Run {
        Run {
            text,
            bold: self.bold,
            italic: self.italic,
            code: self.code,
            link: self.link.clone(),
        }
    }
}

/// Text as a page shows it: each run of white space is one space.
fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            space = true;
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(c);
        }
    }
    if space {
        out.push(' ');
    }
    out
}

/// Runs as a line shows them: no space at either end, no space after a space or a line break,
/// no empty run, and neighbours that look the same joined.
pub(crate) fn tidy(runs: Vec<Run>) -> Vec<Run> {
    let mut out: Vec<Run> = vec![];
    for mut r in runs {
        let after_space = out
            .last()
            .is_none_or(|p: &Run| p.text.ends_with([' ', '\n']));
        if after_space {
            r.text = r.text.trim_start_matches(' ').to_string();
        }
        // a line break takes the space before it away
        if r.text.starts_with('\n')
            && let Some(p) = out.last_mut()
        {
            p.text.truncate(p.text.trim_end_matches(' ').len());
            if p.text.is_empty() {
                out.pop();
            }
        }
        if r.text.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(p)
                if p.bold == r.bold
                    && p.italic == r.italic
                    && p.code == r.code
                    && p.link == r.link =>
            {
                p.text.push_str(&r.text);
            }
            _ => out.push(r),
        }
    }
    // no space or line break at the end, nor at the start
    while let Some(last) = out.last_mut() {
        last.text.truncate(last.text.trim_end().len());
        if last.text.is_empty() {
            out.pop();
        } else {
            break;
        }
    }
    while let Some(first) = out.first_mut() {
        first.text = first.text.trim_start().to_string();
        if first.text.is_empty() {
            out.remove(0);
        } else {
            break;
        }
    }
    out
}

/// The text of every node, as written (a code block keeps its lines), skipping what no reader
/// sees.
fn text_of(nodes: &[Node], out: &mut String) {
    for n in nodes {
        match n {
            Node::Text(t) => out.push_str(t),
            Node::Elem { name, tag, .. } if unseen(name, tag) => {}
            Node::Elem { name, .. } if name == "br" => out.push('\n'),
            Node::Elem { children, .. } => text_of(children, out),
        }
    }
}

/// The `language-*` class of the first `code` element among `nodes`.
fn code_lang(nodes: &[Node]) -> Option<String> {
    nodes.iter().find_map(|n| match n {
        Node::Elem { name, tag, .. } if name == "code" => attr(tag, "class").and_then(|c| {
            c.split_whitespace()
                .find_map(|c| c.strip_prefix("language-"))
                .filter(|l| !l.is_empty())
                .map(decode)
        }),
        _ => None,
    })
}

/// The first element named `want` among `nodes` and their descendants.
fn find<'a>(nodes: &'a [Node], want: &str) -> Option<&'a Node> {
    nodes.iter().find_map(|n| match n {
        Node::Elem { name, .. } if name == want => Some(n),
        Node::Elem { children, .. } => find(children, want),
        Node::Text(_) => None,
    })
}

/// The text of a `span.marker` among `nodes` (a direct child), and the other nodes.
fn split_marker(nodes: &[Node]) -> (Option<String>, Vec<&Node>) {
    let mut marker = None;
    let mut rest = vec![];
    for n in nodes {
        match n {
            Node::Elem {
                name,
                tag,
                children,
            } if marker.is_none() && name == "span" && has_class(tag, "marker") => {
                let mut t = String::new();
                text_of(children, &mut t);
                marker = Some(collapse(&t).trim().to_string());
            }
            n => rest.push(n),
        }
    }
    (marker, rest)
}

/// What a reader reads of a code block: its text, its last line break dropped.
fn code_block(anchor: Option<String>, lang: Option<String>, nodes: &[Node]) -> PBlock {
    let mut text = String::new();
    text_of(nodes, &mut text);
    let text = text.trim_end_matches('\n').to_string();
    PBlock::Code { anchor, lang, text }
}

/// What a walk knows about where it is.
#[derive(Clone, Default)]
struct Ctx {
    /// The language of the diagram being read (a `figure`'s `data-lang`).
    diagram_lang: Option<String>,
}

struct Reader<'a> {
    page: &'a str,
    /// The site file of each figure read, in the order of the figures.
    figures: RefCell<Vec<String>>,
}

impl Reader<'_> {
    /// The blob an image's `src` names; the file it names it by is the file of a figure.
    fn figure_blob(&self, src: &str) -> Option<Hash> {
        let (h, file) = asset_of(&decode(src), self.page)?;
        self.figures.borrow_mut().push(file);
        Some(h)
    }

    /// The runs of `nodes` in a line of text.
    fn inline(&self, nodes: &[&Node], style: &Style, out: &mut Vec<Run>) {
        for n in nodes {
            match n {
                Node::Text(t) => out.push(style.run(collapse(t))),
                Node::Elem { name, tag, .. } if unseen(name, tag) => {}
                Node::Elem {
                    name,
                    tag,
                    children,
                } => {
                    let kids: Vec<&Node> = children.iter().collect();
                    let mut s = style.clone();
                    match name.as_str() {
                        "br" => {
                            out.push(style.run("\n".into()));
                            continue;
                        }
                        "img" => continue,
                        "strong" | "b" => s.bold = true,
                        "em" | "i" => s.italic = true,
                        "code" => s.code = true,
                        "a" => {
                            s.link = attr(tag, "href").and_then(|h| resolve(&decode(h), self.page))
                        }
                        _ => {}
                    }
                    // an element that parts words keeps its text apart from its neighbours'
                    let parts = BLOCKISH.contains(&name.as_str())
                        || matches!(
                            name.as_str(),
                            "p" | "ul"
                                | "ol"
                                | "dl"
                                | "dt"
                                | "dd"
                                | "table"
                                | "td"
                                | "th"
                                | "figure"
                                | "figcaption"
                                | "pre"
                                | "aside"
                                | "details"
                                | "h1"
                                | "h2"
                                | "h3"
                                | "h4"
                                | "h5"
                                | "h6"
                        );
                    if parts {
                        out.push(style.run(" ".into()));
                    }
                    self.inline(&kids, &s, out);
                    if parts {
                        out.push(style.run(" ".into()));
                    }
                }
            }
        }
    }

    fn runs(&self, nodes: &[Node], style: &Style) -> Vec<Run> {
        let kids: Vec<&Node> = nodes.iter().collect();
        let mut out = vec![];
        self.inline(&kids, style, &mut out);
        tidy(out)
    }

    /// The blocks of `nodes`.
    fn blocks(&self, nodes: &[&Node], ctx: &Ctx) -> Vec<PBlock> {
        let mut out = vec![];
        let mut pending: Vec<Run> = vec![];
        for n in nodes {
            self.block(n, ctx, &mut out, &mut pending);
        }
        flush(&mut out, &mut pending);
        out
    }

    fn block(&self, n: &Node, ctx: &Ctx, out: &mut Vec<PBlock>, pending: &mut Vec<Run>) {
        let Node::Elem {
            name,
            tag,
            children,
        } = n
        else {
            if let Node::Text(t) = n {
                pending.push(Style::default().run(collapse(t)));
            }
            return;
        };
        if unseen(name, tag) {
            return;
        }
        let kids: Vec<&Node> = children.iter().collect();
        let plain = Style::default();
        match name.as_str() {
            "p" => {
                flush(out, pending);
                let runs = self.runs(children, &plain);
                if !runs.is_empty() {
                    out.push(PBlock::Para {
                        anchor: id_of(tag),
                        runs,
                    });
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                flush(out, pending);
                out.push(PBlock::Heading {
                    level: name.as_bytes()[1] - b'0',
                    anchor: id_of(tag),
                    runs: self.runs(children, &plain),
                });
            }
            "ul" | "ol" => {
                flush(out, pending);
                let items = children
                    .iter()
                    .filter_map(|c| match c {
                        Node::Elem { name, children, .. } if name == "li" => {
                            Some(self.blocks(&children.iter().collect::<Vec<_>>(), ctx))
                        }
                        _ => None,
                    })
                    .collect();
                out.push(PBlock::List {
                    ordered: name == "ol",
                    start: attr(tag, "start").and_then(|s| s.parse().ok()).unwrap_or(1),
                    items,
                });
            }
            "table" => {
                flush(out, pending);
                self.table(tag, children, out);
            }
            "pre" => {
                flush(out, pending);
                let lang = code_lang(children).or_else(|| ctx.diagram_lang.clone());
                out.push(code_block(id_of(tag), lang, children));
            }
            "figure" => {
                flush(out, pending);
                match find(children, "img") {
                    Some(Node::Elem { tag: img, .. }) => {
                        let caption = find(children, "figcaption")
                            .map(|c| match c {
                                Node::Elem { children, .. } => self.runs(children, &plain),
                                Node::Text(_) => vec![],
                            })
                            .unwrap_or_default();
                        out.push(PBlock::Figure {
                            anchor: id_of(tag),
                            blob: attr(img, "src").and_then(|s| self.figure_blob(s)),
                            alt: attr(img, "alt").map(decode).unwrap_or_default(),
                            caption,
                        });
                    }
                    _ => {
                        // a diagram: its caption, then its source in its own language
                        let ctx = Ctx {
                            diagram_lang: attr(tag, "data-lang")
                                .map(decode)
                                .filter(|l| !l.is_empty()),
                        };
                        out.extend(self.blocks(&kids, &ctx));
                    }
                }
            }
            "img" => {
                flush(out, pending);
                out.push(PBlock::Figure {
                    anchor: id_of(tag),
                    blob: attr(tag, "src").and_then(|s| self.figure_blob(s)),
                    alt: attr(tag, "alt").map(decode).unwrap_or_default(),
                    caption: vec![],
                });
            }
            "figcaption" => {
                flush(out, pending);
                let runs = self.runs(children, &plain);
                if !runs.is_empty() {
                    out.push(PBlock::Para { anchor: None, runs });
                }
            }
            "details" => {
                flush(out, pending);
                match find(children, "pre") {
                    Some(Node::Elem { children: pre, .. }) => out.push(code_block(
                        None,
                        ctx.diagram_lang.clone().or_else(|| code_lang(pre)),
                        pre,
                    )),
                    _ => {
                        let rest: Vec<&Node> = children
                            .iter()
                            .filter(|c| !matches!(c, Node::Elem { name, .. } if name == "summary"))
                            .collect();
                        out.extend(self.blocks(&rest, ctx));
                    }
                }
            }
            "summary" => {}
            "aside" => {
                flush(out, pending);
                let (label, rest) = split_marker(children);
                out.push(PBlock::Callout {
                    anchor: id_of(tag),
                    label: label.unwrap_or_default(),
                    blocks: self.blocks(&rest, ctx),
                });
            }
            "div" if has_class(tag, "normative") => {
                flush(out, pending);
                let (marker, rest) = split_marker(children);
                let mut inner = self.blocks(&rest, ctx);
                if let Some(m) = marker.filter(|m| !m.is_empty()) {
                    let lead = vec![
                        Run {
                            text: m,
                            bold: true,
                            italic: false,
                            code: false,
                            link: None,
                        },
                        plain.run(" ".into()),
                    ];
                    match inner.first_mut() {
                        Some(PBlock::Para { runs, .. }) => {
                            let mut all = lead;
                            all.append(runs);
                            *runs = tidy(all);
                        }
                        _ => inner.insert(
                            0,
                            PBlock::Para {
                                anchor: None,
                                runs: tidy(lead),
                            },
                        ),
                    }
                }
                out.extend(inner);
            }
            "dl" => {
                flush(out, pending);
                self.terms(children, ctx, out);
            }
            "br" => pending.push(plain.run("\n".into())),
            n if BLOCKISH.contains(&n) || matches!(n, "dt" | "dd" | "td" | "th") => {
                flush(out, pending);
                out.extend(self.blocks(&kids, ctx));
            }
            _ => self.inline(&[n], &plain, pending),
        }
    }

    /// A table: the text of its caption first, then its head row and body rows.
    fn table(&self, tag: &str, children: &[Node], out: &mut Vec<PBlock>) {
        let plain = Style::default();
        let mut head: Option<Vec<Vec<Run>>> = None;
        let mut rows: Vec<Vec<Vec<Run>>> = vec![];
        let mut caption: Vec<Run> = vec![];
        let row = |cells: &[Node]| -> Vec<Vec<Run>> {
            cells
                .iter()
                .filter_map(|c| match c {
                    Node::Elem { name, children, .. } if name == "td" || name == "th" => {
                        Some(self.runs(children, &plain))
                    }
                    _ => None,
                })
                .collect()
        };
        for c in children {
            let Node::Elem { name, children, .. } = c else {
                continue;
            };
            match name.as_str() {
                "caption" => caption.extend(self.runs(children, &plain)),
                "thead" | "tbody" => {
                    for r in children {
                        if let Node::Elem { name, children, .. } = r
                            && name == "tr"
                        {
                            if c_is(c, "thead") && head.is_none() {
                                head = Some(row(children));
                            } else {
                                rows.push(row(children));
                            }
                        }
                    }
                }
                "tr" => rows.push(row(children)),
                _ => {}
            }
        }
        let caption = tidy(caption);
        if !caption.is_empty() {
            out.push(PBlock::Para {
                anchor: None,
                runs: caption,
            });
        }
        out.push(PBlock::Table {
            anchor: id_of(tag),
            head: head.unwrap_or_default(),
            rows,
        });
    }

    /// A list of terms: each term in bold, then its text in the same paragraph.
    fn terms(&self, children: &[Node], ctx: &Ctx, out: &mut Vec<PBlock>) {
        let bold = Style {
            bold: true,
            ..Style::default()
        };
        let mut term: Option<(Option<String>, Vec<Run>)> = None;
        for c in children {
            let Node::Elem {
                name,
                tag,
                children,
            } = c
            else {
                continue;
            };
            if unseen(name, tag) {
                continue;
            }
            match name.as_str() {
                "dt" => {
                    if let Some((anchor, runs)) = term.take() {
                        out.push(PBlock::Para { anchor, runs });
                    }
                    term = Some((id_of(tag), self.runs(children, &bold)));
                }
                "dd" => {
                    let mut blocks = self.blocks(&children.iter().collect::<Vec<_>>(), ctx);
                    if let Some((anchor, mut runs)) = term.take() {
                        if let Some(PBlock::Para { anchor: None, .. }) = blocks.first()
                            && let PBlock::Para { runs: more, .. } = blocks.remove(0)
                        {
                            runs.push(Style::default().run(" ".into()));
                            runs.extend(more);
                        }
                        out.push(PBlock::Para {
                            anchor,
                            runs: tidy(runs),
                        });
                    }
                    out.extend(blocks);
                }
                _ => {}
            }
        }
        if let Some((anchor, runs)) = term {
            out.push(PBlock::Para { anchor, runs });
        }
    }
}

fn c_is(n: &Node, want: &str) -> bool {
    matches!(n, Node::Elem { name, .. } if name == want)
}

/// Ends the paragraph of loose text being read, if it has words.
fn flush(out: &mut Vec<PBlock>, pending: &mut Vec<Run>) {
    let runs = tidy(std::mem::take(pending));
    if !runs.is_empty() {
        out.push(PBlock::Para { anchor: None, runs });
    }
}

/// The blocks a reader sees in a piece of the renderer's own HTML on `page` (a site-relative
/// page url). A link's `href` is resolved against `page`: `http`, `https` and `mailto` are
/// [`Href::External`]; a page of the site is [`Href::Page`] (the print then keeps it only when
/// it prints that page, [`unlink`]); any other target leaves its words as plain text. An `img`
/// whose `src` is a site asset (`assets/<hash>.<extension>`) names its blob's hash.
pub fn from_html(html: &str, page: &str) -> Vec<PBlock> {
    from_html_with_figures(html, page).0
}

/// [`from_html`], and the site file of each [`PBlock::Figure`] it reads, in the order of the
/// figures: the files a reader of the page sees as images (a download link names none).
pub(crate) fn from_html_with_figures(html: &str, page: &str) -> (Vec<PBlock>, Vec<String>) {
    let nodes = parse(html);
    let kids: Vec<&Node> = nodes.iter().collect();
    let reader = Reader {
        page,
        figures: RefCell::default(),
    };
    let blocks = reader.blocks(&kids, &Ctx::default());
    (blocks, reader.figures.into_inner())
}

/// Every link of `blocks` to a page `printed` does not hold becomes plain text (a permalink,
/// the home page, a page the print leaves out).
pub fn unlink(blocks: &mut [PBlock], printed: &dyn Fn(&str) -> bool) {
    map_links(blocks, &|h| match h {
        Href::Page { page, .. } if !printed(page) => None,
        h => Some(h.clone()),
    });
}

/// Every link of `blocks` leads where `to` says: a new target, or none (its words stay, as
/// plain text).
pub(crate) fn map_links(blocks: &mut [PBlock], to: &dyn Fn(&Href) -> Option<Href>) {
    let fix = |runs: &mut Vec<Run>| {
        for r in runs.iter_mut() {
            if let Some(h) = &r.link {
                r.link = to(h);
            }
        }
        *runs = tidy(std::mem::take(runs));
    };
    for b in blocks {
        match b {
            PBlock::Heading { runs, .. } | PBlock::Para { runs, .. } => fix(runs),
            PBlock::List { items, .. } => {
                for item in items {
                    map_links(item, to);
                }
            }
            PBlock::Table { head, rows, .. } => {
                for cell in head.iter_mut().chain(rows.iter_mut().flatten()) {
                    fix(cell);
                }
            }
            PBlock::Figure { caption, .. } => fix(caption),
            PBlock::Callout { blocks, .. } => map_links(blocks, to),
            PBlock::Code { .. } => {}
        }
    }
}

/// The anchors of `blocks` and the blocks inside them.
pub(crate) fn anchors(blocks: &[PBlock], out: &mut BTreeSet<String>) {
    for b in blocks {
        let anchor = match b {
            PBlock::Heading { anchor, .. }
            | PBlock::Para { anchor, .. }
            | PBlock::Table { anchor, .. }
            | PBlock::Code { anchor, .. }
            | PBlock::Figure { anchor, .. } => anchor,
            PBlock::Callout { anchor, blocks, .. } => {
                anchors(blocks, out);
                anchor
            }
            PBlock::List { items, .. } => {
                items.iter().for_each(|i| anchors(i, out));
                continue;
            }
        };
        out.extend(anchor.clone());
    }
}

/// Gives the first block of `blocks` that can hold an anchor `anchor`, unless it has one.
pub(crate) fn set_anchor(blocks: &mut [PBlock], anchor: &str) -> bool {
    for b in blocks {
        let slot = match b {
            PBlock::Heading { anchor, .. }
            | PBlock::Para { anchor, .. }
            | PBlock::Table { anchor, .. }
            | PBlock::Code { anchor, .. }
            | PBlock::Figure { anchor, .. }
            | PBlock::Callout { anchor, .. } => anchor,
            PBlock::List { items, .. } => {
                if items.iter_mut().any(|i| set_anchor(i, anchor)) {
                    return true;
                }
                continue;
            }
        };
        if slot.is_none() {
            *slot = Some(anchor.to_string());
        }
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> Run {
        Run {
            text: text.into(),
            bold: false,
            italic: false,
            code: false,
            link: None,
        }
    }

    fn code(text: &str) -> Run {
        Run {
            code: true,
            ..run(text)
        }
    }

    fn bold(text: &str) -> Run {
        Run {
            bold: true,
            ..run(text)
        }
    }

    fn link(text: &str, href: Href) -> Run {
        Run {
            link: Some(href),
            ..run(text)
        }
    }

    fn page(page: &str, anchor: Option<&str>) -> Href {
        Href::Page {
            page: page.into(),
            anchor: anchor.map(String::from),
        }
    }

    #[test]
    fn ec3_ir_reads_lists_tables_code_and_links_from_renderer_html() {
        let hash = "667893735fd6ddea275b7aa47f9bf71622da7924b3304e8764ab068f69e6dd7c";
        let html = format!(
            "<h2 class=\"section-title\">The <code>case</code> clause</h2>\
             <p>See <a href=\"../tutorial/adventure.html#guards\" title=\"Doc\">guards</a>, \
             <a href=\"#capture\">capture</a>, <a href=\"https://example.invalid/x\" \
             rel=\"noopener noreferrer\">the PEP</a>, <a href=\"mailto:a@example.invalid\">mail</a> \
             and <a href=\"javascript:x\">this</a> &amp; <strong>that</strong> <em>too</em>.</p>\
             <ol start=\"3\"><li>three<ul><li>inner</li></ul></li><li>four</li></ol>\
             <div class=\"table-wrap\"><table><caption>Table 1</caption><thead><tr>\
             <th scope=\"col\">A</th><th scope=\"col\">B</th></tr></thead><tbody><tr>\
             <td>a<br>b</td><td class=\"num\">2</td></tr></tbody></table></div>\
             <div class=\"code-block\"><div class=\"code-head\"><span class=\"lang\">Python</span>\
             <button type=\"button\" class=\"tool copy-code\" aria-label=\"Copy code\">Copy</button>\
             </div><pre class=\"code\"><code class=\"language-python\"><span class=\"k\">if</span> \
             x &lt; 1:\n    pass\n</code></pre></div>\
             <figure class=\"figure\"><img src=\"../assets/{hash}.png\" alt=\"A &quot;case&quot;\" \
             loading=\"lazy\"><figcaption>A case block</figcaption></figure>\
             <figure class=\"diagram\" data-lang=\"mermaid\"><p class=\"visually-hidden\">Alt</p>\
             <div class=\"diagram-canvas\" aria-hidden=\"true\"><pre class=\"mermaid\">A#91;x#93;\
             </pre></div><figcaption>Alt</figcaption><details class=\"diagram-source\">\
             <summary>Diagram source</summary><pre class=\"code\"><code>flowchart TD\n  A --&gt; B\
             </code></pre></details></figure>\
             <aside class=\"callout callout-note\"><span class=\"marker\">Note</span><div><p>Mind \
             it.</p></div></aside>\
             <div class=\"normative\"><span class=\"marker\">Requirement · must</span><p>Do it.</p></div>\
             <dl class=\"term\"><dt id=\"guard\">guard</dt><dd>A condition. \
             <p class=\"entity-aliases\">Also: if-clause</p></dd></dl>"
        );
        let blocks = from_html(&html, "specification/patterns.html");
        let expected = vec![
            PBlock::Heading {
                level: 2,
                anchor: None,
                runs: vec![run("The "), code("case"), run(" clause")],
            },
            PBlock::Para {
                anchor: None,
                runs: vec![
                    run("See "),
                    link("guards", page("tutorial/adventure.html", Some("guards"))),
                    run(", "),
                    link(
                        "capture",
                        page("specification/patterns.html", Some("capture")),
                    ),
                    run(", "),
                    link(
                        "the PEP",
                        Href::External("https://example.invalid/x".into()),
                    ),
                    run(", "),
                    link("mail", Href::External("mailto:a@example.invalid".into())),
                    run(" and this & "),
                    bold("that"),
                    run(" "),
                    Run {
                        italic: true,
                        ..run("too")
                    },
                    run("."),
                ],
            },
            PBlock::List {
                ordered: true,
                start: 3,
                items: vec![
                    vec![
                        PBlock::Para {
                            anchor: None,
                            runs: vec![run("three")],
                        },
                        PBlock::List {
                            ordered: false,
                            start: 1,
                            items: vec![vec![PBlock::Para {
                                anchor: None,
                                runs: vec![run("inner")],
                            }]],
                        },
                    ],
                    vec![PBlock::Para {
                        anchor: None,
                        runs: vec![run("four")],
                    }],
                ],
            },
            PBlock::Para {
                anchor: None,
                runs: vec![run("Table 1")],
            },
            PBlock::Table {
                anchor: None,
                head: vec![vec![run("A")], vec![run("B")]],
                rows: vec![vec![vec![run("a\nb")], vec![run("2")]]],
            },
            PBlock::Para {
                anchor: None,
                runs: vec![run("Python")],
            },
            PBlock::Code {
                anchor: None,
                lang: Some("python".into()),
                text: "if x < 1:\n    pass".into(),
            },
            PBlock::Figure {
                anchor: None,
                blob: Some(hash.parse().unwrap()),
                alt: "A \"case\"".into(),
                caption: vec![run("A case block")],
            },
            PBlock::Para {
                anchor: None,
                runs: vec![run("Alt")],
            },
            PBlock::Code {
                anchor: None,
                lang: Some("mermaid".into()),
                text: "flowchart TD\n  A --> B".into(),
            },
            PBlock::Callout {
                anchor: None,
                label: "Note".into(),
                blocks: vec![PBlock::Para {
                    anchor: None,
                    runs: vec![run("Mind it.")],
                }],
            },
            PBlock::Para {
                anchor: None,
                runs: vec![bold("Requirement · must"), run(" Do it.")],
            },
            PBlock::Para {
                anchor: Some("guard".into()),
                runs: vec![bold("guard"), run(" A condition.")],
            },
            PBlock::Para {
                anchor: None,
                runs: vec![run("Also: if-clause")],
            },
        ];
        assert_eq!(blocks.len(), expected.len(), "{blocks:#?}");
        for (got, want) in blocks.iter().zip(&expected) {
            assert_eq!(got, want);
        }
    }

    #[test]
    fn ec3_ir_unlinks_what_the_print_does_not_hold() {
        let html = "<p><a href=\"../go/p-AAAAA.html\">a permalink</a>, \
            <a href=\"../index.html\">home</a>, <a href=\"other.html#x\">out</a> and \
            <a href=\"kept.html#y\">kept</a>.</p>";
        let mut blocks = from_html(html, "spec/doc.html");
        unlink(&mut blocks, &|p| p == "spec/kept.html");
        assert_eq!(
            blocks,
            vec![PBlock::Para {
                anchor: None,
                runs: vec![
                    run("a permalink, home, out and "),
                    link("kept", page("spec/kept.html", Some("y"))),
                    run("."),
                ],
            }]
        );
        // a path above the site's root leads nowhere
        assert_eq!(resolve("../../x.html", "spec/doc.html"), None);
    }

    #[test]
    fn ec3_ir_drops_what_a_reader_never_sees() {
        let html = "<p>Before <button type=\"button\">Copy</button>after\
            <span class=\"visually-hidden\">only heard</span>.</p>\
            <div class=\"diagram-canvas\" aria-hidden=\"true\"><canvas aria-hidden=\"true\">x</canvas>\
            <pre class=\"mermaid\">A</pre></div>\
            <canvas aria-hidden=\"true\">drawn</canvas>\
            <script>var x = \"<p>no</p>\";</script><style>.a{color:red}</style>\
            <template><p>Later</p></template><p class=\"note visually-hidden\">Gone</p>";
        assert_eq!(
            from_html(html, "a.html"),
            vec![PBlock::Para {
                anchor: None,
                runs: vec![run("Before after.")],
            }]
        );
    }
}
