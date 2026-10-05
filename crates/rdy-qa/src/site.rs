//! What the render-quality gate reads a rendered site with: a visible-text walker over the
//! renderer's own HTML, a small CSS token parser and the WCAG contrast arithmetic.
//! No external parser: the HTML is exactly what `rdy-render` writes.
use regex::Regex;
use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

/// Where a run of text sits on the page: authored content, the "Why ?" trail, the agents'
/// affordance, or chrome (everything else).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Chrome,
    Content,
    Trail,
    Agents,
}

/// One run of visible text (between two tags, entity-decoded, whitespace collapsed).
#[derive(Debug, Clone)]
pub struct Run {
    pub text: String,
    pub zone: Zone,
    /// Some element on the stack (the innermost included) is a label element and the run is
    /// the whole text of its innermost element.
    pub label: bool,
    /// The element stack's tag names, outermost first, the innermost included.
    pub path: Vec<String>,
}

const LABEL_TAGS: [&str; 10] = [
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "th",
    "dt",
    "summary",
    "figcaption",
];
const LABEL_CLASSES: [&str; 18] = [
    "eyebrow",
    "badge",
    "meta",
    "version",
    "snapshot",
    "crumbs",
    "nav-item",
    "nav-group-label",
    "toc-label",
    "toc-link",
    "pager-dir",
    "pager-title",
    "r-where",
    "r-title",
    "lang",
    "card-kind",
    "marker",
    "colophon",
];
const SKIP_TAGS: [&str; 8] = [
    "script", "style", "code", "pre", "kbd", "template", "noscript", "svg",
];
const VOID_TAGS: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

enum Ev<'a> {
    Open {
        tag: String,
        attrs: Vec<(String, &'a str)>,
        selfclose: bool,
    },
    Close(String),
    Text(&'a str),
}

/// Splits `html` into tag and text events. Comments, doctype and processing instructions are
/// dropped; the contents of `script` and `style` come as one raw text event.
fn events(html: &str) -> Vec<Ev<'_>> {
    let b = html.as_bytes();
    let (mut out, mut i, mut text_from) = (vec![], 0, 0);
    while i < b.len() {
        if b[i] != b'<' || i + 1 >= b.len() {
            i += 1;
            continue;
        }
        let next = b[i + 1];
        let tag_end = if html[i..].starts_with("<!--") {
            html[i + 4..].find("-->").map_or(b.len(), |j| i + 4 + j + 3)
        } else if next == b'!' || next == b'?' {
            html[i..].find('>').map_or(b.len(), |j| i + j + 1)
        } else if next == b'/' || next.is_ascii_alphabetic() {
            let mut j = i + 1;
            let mut quote = 0u8;
            while j < b.len() && (quote != 0 || b[j] != b'>') {
                if quote != 0 {
                    quote = if b[j] == quote { 0 } else { quote };
                } else if b[j] == b'"' || b[j] == b'\'' {
                    quote = b[j];
                }
                j += 1;
            }
            (j + 1).min(b.len())
        } else {
            i += 1;
            continue;
        };
        if text_from < i {
            out.push(Ev::Text(&html[text_from..i]));
        }
        let inner = html[i + 1..tag_end].trim_end_matches('>');
        if let Some(name) = inner.strip_prefix('/') {
            out.push(Ev::Close(name.trim().to_ascii_lowercase()));
        } else if !inner.starts_with(['!', '?']) {
            let (tag, attrs, selfclose) = parse_open(inner);
            let raw = !selfclose && (tag == "script" || tag == "style");
            out.push(Ev::Open {
                tag: tag.clone(),
                attrs,
                selfclose,
            });
            if raw {
                let lower = html[tag_end..].to_ascii_lowercase();
                let end = lower
                    .find(&format!("</{tag}"))
                    .map_or(b.len(), |j| tag_end + j);
                if tag_end < end {
                    out.push(Ev::Text(&html[tag_end..end]));
                }
                i = end;
                text_from = end;
                continue;
            }
        }
        i = tag_end;
        text_from = tag_end;
    }
    if text_from < b.len() {
        out.push(Ev::Text(&html[text_from..]));
    }
    out
}

/// `inner` is the text between `<` and `>` of an opening tag.
fn parse_open(inner: &str) -> (String, Vec<(String, &str)>, bool) {
    let selfclose = inner.ends_with('/');
    let inner = inner.strip_suffix('/').unwrap_or(inner);
    let name_end = inner
        .find(|c: char| c.is_ascii_whitespace())
        .unwrap_or(inner.len());
    let tag = inner[..name_end].to_ascii_lowercase();
    let (mut attrs, mut rest) = (vec![], inner[name_end..].trim_start());
    while !rest.is_empty() {
        let key_end = rest
            .find(|c: char| c.is_ascii_whitespace() || c == '=')
            .unwrap_or(rest.len());
        let key = rest[..key_end].to_ascii_lowercase();
        rest = rest[key_end..].trim_start();
        let mut value = "";
        if let Some(r) = rest.strip_prefix('=') {
            let r = r.trim_start();
            let (v, after) = match r.chars().next() {
                Some(q @ ('"' | '\'')) => match r[1..].find(q) {
                    Some(j) => (&r[1..1 + j], &r[j + 2..]),
                    None => (&r[1..], ""),
                },
                _ => r.split_at(r.find(|c: char| c.is_ascii_whitespace()).unwrap_or(r.len())),
            };
            value = v;
            rest = after;
        }
        if !key.is_empty() {
            attrs.push((key, value));
        }
        rest = rest.trim_start();
    }
    (tag, attrs, selfclose)
}

struct El {
    tag: String,
    id: usize,
    rdy: Option<String>,
    skip: bool,
    label: bool,
}

/// The visible text runs of a page, in document order, with zone and label flags.
pub fn visible_runs(html: &str) -> Vec<Run> {
    let (mut stack, mut next_id) = (Vec::<El>::new(), 0);
    let mut runs: Vec<(Run, usize)> = vec![];
    let mut per_element: HashMap<usize, usize> = HashMap::new();
    for ev in events(html) {
        match ev {
            Ev::Open {
                tag,
                attrs,
                selfclose,
            } => {
                if selfclose || VOID_TAGS.contains(&tag.as_str()) {
                    continue;
                }
                let attr = |k: &str| attrs.iter().find(|(n, _)| n == k).map(|(_, v)| *v);
                let classes = attr("class").unwrap_or("");
                let label = LABEL_TAGS.contains(&tag.as_str())
                    || classes
                        .split_ascii_whitespace()
                        .any(|c| LABEL_CLASSES.contains(&c));
                let skip =
                    SKIP_TAGS.contains(&tag.as_str()) || attrs.iter().any(|(n, _)| n == "hidden");
                next_id += 1;
                stack.push(El {
                    tag,
                    id: next_id,
                    rdy: attr("data-rdy").map(str::to_string),
                    skip,
                    label,
                });
            }
            Ev::Close(tag) => {
                if let Some(at) = stack.iter().rposition(|e| e.tag == tag) {
                    stack.truncate(at);
                }
            }
            Ev::Text(raw) => {
                let Some(top) = stack.last() else { continue };
                if stack.iter().any(|e| e.skip) {
                    continue;
                }
                let text = unescape(raw)
                    .split_ascii_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if text.is_empty() {
                    continue;
                }
                let zone = match stack.iter().rev().find_map(|e| e.rdy.as_deref()) {
                    Some("content") => Zone::Content,
                    Some("trail") => Zone::Trail,
                    Some("agents") => Zone::Agents,
                    _ => Zone::Chrome,
                };
                *per_element.entry(top.id).or_default() += 1;
                let label = stack.iter().any(|e| e.label);
                runs.push((
                    Run {
                        text,
                        zone,
                        label,
                        path: stack.iter().map(|e| e.tag.clone()).collect(),
                    },
                    top.id,
                ));
            }
        }
    }
    runs.into_iter()
        .map(|(mut r, id)| {
            r.label &= per_element[&id] == 1;
            r
        })
        .collect()
}

/// The heading levels and texts of a page, in order (a heading's text includes any code in it).
pub fn page_titles(html: &str) -> Vec<(u8, String)> {
    let mut out = vec![];
    let mut open: Option<(u8, String)> = None;
    for ev in events(html) {
        match ev {
            Ev::Open { tag, .. } if tag.len() == 2 && tag.starts_with('h') => {
                if let Ok(level @ 1..=6) = tag[1..].parse::<u8>() {
                    open = Some((level, String::new()));
                }
            }
            Ev::Close(tag)
                if tag.len() == 2 && tag.starts_with('h') && tag[1..].parse::<u8>().is_ok() =>
            {
                if let Some((level, text)) = open.take() {
                    out.push((
                        level,
                        text.split_ascii_whitespace().collect::<Vec<_>>().join(" "),
                    ));
                }
            }
            Ev::Text(raw) => {
                if let Some((_, text)) = open.as_mut() {
                    text.push_str(&unescape(raw));
                }
            }
            _ => {}
        }
    }
    out
}

/// Every attribute value of `attr` in `html`.
pub fn attrs<'a>(html: &'a str, attr: &str) -> Vec<&'a str> {
    let key = format!("{attr}=\"");
    let mut out = vec![];
    let mut rest = html;
    while let Some(i) = rest.find(&key) {
        rest = &rest[i + key.len()..];
        if let Some(j) = rest.find('"') {
            out.push(&rest[..j]);
            rest = &rest[j..];
        }
    }
    out
}

/// Decodes the HTML entities a browser would decode in text or in an attribute value.
pub fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(j) = rest.find(';') else { break };
        let ent = &rest[1..j];
        let ch = match ent {
            "amp" => Some('&'),
            "quot" => Some('"'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            _ => ent
                .strip_prefix("#x")
                .or_else(|| ent.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[j + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------------- CSS

#[derive(Debug, Clone, Default)]
pub struct Css {
    pub blocks: Vec<CssBlock>,
}

/// One rule: its selector (or at-rule, for `@font-face` and `@keyframes`), the `@media`
/// prelude it sits in (one level) and its declarations in source order.
#[derive(Debug, Clone)]
pub struct CssBlock {
    pub selector: String,
    pub media: Option<String>,
    pub decls: Vec<(String, String)>,
}

fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        rest = rest[i + 2..]
            .find("*/")
            .map_or("", |j| &rest[i + 2 + j + 2..]);
    }
    out.push_str(rest);
    out
}

/// Index of the first byte of `s` at paren depth 0 and outside a string that is one of `stops`.
fn find_top(s: &str, stops: &[u8]) -> Option<usize> {
    let (mut depth, mut quote) = (0i32, 0u8);
    for (i, &c) in s.as_bytes().iter().enumerate() {
        if quote != 0 {
            quote = if c == quote { 0 } else { quote };
        } else if c == b'"' || c == b'\'' {
            quote = c;
        } else if c == b'(' || c == b'[' {
            depth += 1;
        } else if c == b')' || c == b']' {
            depth -= 1;
        } else if depth <= 0 && stops.contains(&c) {
            return Some(i);
        }
    }
    None
}

/// Index of the `}` that closes a block whose body starts at the beginning of `s`.
fn find_close(s: &str) -> usize {
    let (mut depth, mut quote) = (0i32, 0u8);
    for (i, &c) in s.as_bytes().iter().enumerate() {
        if quote != 0 {
            quote = if c == quote { 0 } else { quote };
        } else if c == b'"' || c == b'\'' {
            quote = c;
        } else if c == b'{' {
            depth += 1;
        } else if c == b'}' {
            if depth == 0 {
                return i;
            }
            depth -= 1;
        }
    }
    s.len()
}

fn parse_decls(body: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    let mut rest = body;
    while !rest.trim().is_empty() {
        let end = find_top(rest, b";").unwrap_or(rest.len());
        if let Some((name, value)) = rest[..end].split_once(':') {
            let name = name.trim();
            let name = if name.starts_with("--") {
                name.to_string()
            } else {
                name.to_ascii_lowercase()
            };
            out.push((name, value.trim().to_string()));
        }
        rest = rest.get(end + 1..).unwrap_or("");
    }
    out
}

fn collapse(s: &str) -> String {
    s.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_rules(css: &str, media: Option<&str>, out: &mut Vec<CssBlock>) {
    let mut rest = css;
    while !rest.trim().is_empty() {
        let Some(stop) = find_top(rest, b"{;}") else {
            break;
        };
        let prelude = collapse(&rest[..stop]);
        if rest.as_bytes()[stop] != b'{' {
            rest = &rest[stop + 1..];
            continue;
        }
        let body_start = stop + 1;
        let close = find_close(&rest[body_start..]);
        let body = &rest[body_start..body_start + close];
        rest = rest.get(body_start + close + 1..).unwrap_or("");
        if let Some(cond) = prelude.strip_prefix("@media") {
            parse_rules(body, Some(cond.trim()), out);
        } else if ["@supports", "@layer", "@container"]
            .iter()
            .any(|a| prelude.starts_with(a))
        {
            parse_rules(body, media, out);
        } else if prelude.starts_with("@keyframes") || prelude.starts_with("@-webkit-keyframes") {
            let mut frames = vec![];
            parse_rules(body, media, &mut frames);
            let decls = frames.into_iter().flat_map(|f| f.decls).collect();
            out.push(CssBlock {
                selector: prelude,
                media: media.map(str::to_string),
                decls,
            });
        } else {
            out.push(CssBlock {
                selector: prelude,
                media: media.map(str::to_string),
                decls: parse_decls(body),
            });
        }
    }
}

/// A brace-matching parser: each rule's selector, enclosing `@media` prelude and declarations.
pub fn parse_css(css: &str) -> Css {
    let mut blocks = vec![];
    parse_rules(&strip_comments(css), None, &mut blocks);
    Css { blocks }
}

const NAMED_COLOURS: [&str; 148] = [
    "aliceblue",
    "antiquewhite",
    "aqua",
    "aquamarine",
    "azure",
    "beige",
    "bisque",
    "black",
    "blanchedalmond",
    "blue",
    "blueviolet",
    "brown",
    "burlywood",
    "cadetblue",
    "chartreuse",
    "chocolate",
    "coral",
    "cornflowerblue",
    "cornsilk",
    "crimson",
    "cyan",
    "darkblue",
    "darkcyan",
    "darkgoldenrod",
    "darkgray",
    "darkgreen",
    "darkgrey",
    "darkkhaki",
    "darkmagenta",
    "darkolivegreen",
    "darkorange",
    "darkorchid",
    "darkred",
    "darksalmon",
    "darkseagreen",
    "darkslateblue",
    "darkslategray",
    "darkslategrey",
    "darkturquoise",
    "darkviolet",
    "deeppink",
    "deepskyblue",
    "dimgray",
    "dimgrey",
    "dodgerblue",
    "firebrick",
    "floralwhite",
    "forestgreen",
    "fuchsia",
    "gainsboro",
    "ghostwhite",
    "gold",
    "goldenrod",
    "gray",
    "green",
    "greenyellow",
    "grey",
    "honeydew",
    "hotpink",
    "indianred",
    "indigo",
    "ivory",
    "khaki",
    "lavender",
    "lavenderblush",
    "lawngreen",
    "lemonchiffon",
    "lightblue",
    "lightcoral",
    "lightcyan",
    "lightgoldenrodyellow",
    "lightgray",
    "lightgreen",
    "lightgrey",
    "lightpink",
    "lightsalmon",
    "lightseagreen",
    "lightskyblue",
    "lightslategray",
    "lightslategrey",
    "lightsteelblue",
    "lightyellow",
    "lime",
    "limegreen",
    "linen",
    "magenta",
    "maroon",
    "mediumaquamarine",
    "mediumblue",
    "mediumorchid",
    "mediumpurple",
    "mediumseagreen",
    "mediumslateblue",
    "mediumspringgreen",
    "mediumturquoise",
    "mediumvioletred",
    "midnightblue",
    "mintcream",
    "mistyrose",
    "moccasin",
    "navajowhite",
    "navy",
    "oldlace",
    "olive",
    "olivedrab",
    "orange",
    "orangered",
    "orchid",
    "palegoldenrod",
    "palegreen",
    "paleturquoise",
    "palevioletred",
    "papayawhip",
    "peachpuff",
    "peru",
    "pink",
    "plum",
    "powderblue",
    "purple",
    "rebeccapurple",
    "red",
    "rosybrown",
    "royalblue",
    "saddlebrown",
    "salmon",
    "sandybrown",
    "seagreen",
    "seashell",
    "sienna",
    "silver",
    "skyblue",
    "slateblue",
    "slategray",
    "slategrey",
    "snow",
    "springgreen",
    "steelblue",
    "tan",
    "teal",
    "thistle",
    "tomato",
    "turquoise",
    "violet",
    "wheat",
    "white",
    "whitesmoke",
    "yellow",
    "yellowgreen",
];
const COLOUR_FUNCTIONS: [&str; 10] = [
    "rgb", "rgba", "hsl", "hsla", "hwb", "lab", "lch", "oklch", "oklab", "color",
];

/// Length of the balanced `(...)` group that starts at `s[0]` (which is `(`), quotes respected.
fn paren_len(s: &str) -> usize {
    let (mut depth, mut quote) = (0i32, 0u8);
    for (i, &c) in s.as_bytes().iter().enumerate() {
        if quote != 0 {
            quote = if c == quote { 0 } else { quote };
        } else if c == b'"' || c == b'\'' {
            quote = c;
        } else if c == b'(' {
            depth += 1;
        } else if c == b')' {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
    }
    s.len()
}

/// Every colour literal in a declaration value: `#hex` (3, 4, 6 or 8 digits), the colour
/// functions and the 148 named colours as whole words. Strings, `url(...)`, `var(...)` and
/// custom property names are not scanned; `transparent`, `currentcolor` and the CSS-wide
/// keywords are not literals.
pub fn colour_literals(value: &str) -> Vec<String> {
    let b = value.as_bytes();
    let (mut out, mut i) = (vec![], 0);
    let is_ident = |c: u8| c.is_ascii_alphanumeric() || c == b'-' || c == b'_';
    while i < b.len() {
        let c = b[i];
        if c == b'"' || c == b'\'' {
            i += value[i + 1..]
                .find(c as char)
                .map_or(b.len(), |j| i + j + 2);
            i = i.min(b.len());
        } else if c == b'#' {
            let n = b[i + 1..]
                .iter()
                .take_while(|c| c.is_ascii_hexdigit())
                .count();
            let after_ident = b.get(i + 1 + n).is_some_and(|&c| is_ident(c));
            if matches!(n, 3 | 4 | 6 | 8) && !after_ident {
                out.push(value[i..i + 1 + n].to_string());
            }
            i += 1 + n;
        } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit))
        {
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'%') {
                i += 1;
            }
        } else if is_ident(c) {
            let start = i;
            while i < b.len() && is_ident(b[i]) {
                i += 1;
            }
            let word = &value[start..i];
            let lower = word.to_ascii_lowercase();
            if b.get(i) == Some(&b'(') {
                let len = paren_len(&value[i..]);
                if COLOUR_FUNCTIONS.contains(&lower.as_str()) {
                    out.push(value[start..i + len].to_string());
                    i += len;
                } else if lower == "var" || lower == "url" {
                    i += len;
                }
            } else if !word.starts_with("--") && NAMED_COLOURS.contains(&lower.as_str()) {
                out.push(word.to_string());
            }
        } else {
            i += 1;
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairKind {
    Text,
    Ui,
}

/// A foreground and background token that must meet contrast (`--pair-*`).
#[derive(Debug, Clone)]
pub struct Pair {
    pub name: String,
    pub fg: String,
    pub bg: String,
    pub kind: PairKind,
}

#[derive(Debug, Clone, Default)]
pub struct Tokens {
    pub light: BTreeMap<String, String>,
    pub dark_media: BTreeMap<String, String>,
    pub dark_attr: BTreeMap<String, String>,
    pub pairs: Vec<Pair>,
}

fn custom_props(decls: &[(String, String)], into: &mut BTreeMap<String, String>) {
    for (n, v) in decls.iter().filter(|(n, _)| n.starts_with("--")) {
        into.insert(n.clone(), v.clone());
    }
}

static PAIR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^var\((--[A-Za-z0-9_-]+)\)\s+var\((--[A-Za-z0-9_-]+)\)\s+(text|ui)$").unwrap()
});
static VAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^var\(\s*(--[A-Za-z0-9_-]+)\s*(?:,\s*(.*?))?\s*\)$").unwrap());
static MAX_WIDTH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"max-width\s*:\s*(\d+(?:\.\d+)?)px").unwrap());
static PX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d+(?:\.\d+)?)px$").unwrap());

/// The token blocks of a stylesheet and its declared contrast pairs.
pub fn tokens(css: &Css) -> Result<Tokens, String> {
    let mut t = Tokens::default();
    for b in &css.blocks {
        let media = b.media.as_deref().map(|m| m.replace(' ', ""));
        let dark = media
            .as_deref()
            .is_some_and(|m| m.contains("prefers-color-scheme:dark"));
        match (b.selector.as_str(), media.is_some()) {
            (":root", false) => custom_props(&b.decls, &mut t.light),
            (r#":root:not([data-theme="light"])"#, true) if dark => {
                custom_props(&b.decls, &mut t.dark_media)
            }
            (r#":root[data-theme="dark"]"#, false) => custom_props(&b.decls, &mut t.dark_attr),
            _ => {}
        }
    }
    for (name, value) in t.light.iter().filter(|(n, _)| n.starts_with("--pair-")) {
        let caps = PAIR
            .captures(value)
            .ok_or_else(|| format!("{name}: `{value}` is not `var(--fg) var(--bg) text|ui`"))?;
        let kind = if &caps[3] == "text" {
            PairKind::Text
        } else {
            PairKind::Ui
        };
        t.pairs.push(Pair {
            name: name.clone(),
            fg: caps[1].to_string(),
            bg: caps[2].to_string(),
            kind,
        });
    }
    Ok(t)
}

/// Follows `var(--x)` chains (at most 8 links, a fallback used when a link is undefined).
pub fn resolve(theme: &BTreeMap<String, String>, token: &str) -> Option<String> {
    let mut value = theme.get(token)?.clone();
    for _ in 0..8 {
        let Some(caps) = VAR.captures(&value) else {
            return Some(value);
        };
        value = match theme.get(&caps[1]) {
            Some(v) => v.clone(),
            None => caps.get(2)?.as_str().to_string(),
        };
    }
    None
}

/// `#rgb`, `#rrggbb`, `#rrggbbaa` (alpha ignored) and `rgb()`/`rgba()` with integer or percent
/// channels, as channels in 0..=1.
pub fn srgb(value: &str) -> Option<[f64; 3]> {
    let v = value.trim();
    if let Some(hex) = v.strip_prefix('#') {
        if !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let digits: Vec<u32> = hex.chars().filter_map(|c| c.to_digit(16)).collect();
        let ch = |i: usize| match digits.len() {
            3 | 4 => Some(f64::from(digits[i] * 17) / 255.0),
            6 | 8 => Some(f64::from(digits[2 * i] * 16 + digits[2 * i + 1]) / 255.0),
            _ => None,
        };
        return Some([ch(0)?, ch(1)?, ch(2)?]);
    }
    let lower = v.to_ascii_lowercase();
    let args = lower
        .strip_prefix("rgba(")
        .or_else(|| lower.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let mut parts = args
        .split(|c: char| c == ',' || c == '/' || c.is_ascii_whitespace())
        .filter(|p| !p.is_empty());
    let mut ch = || -> Option<f64> {
        let p = parts.next()?;
        let n = match p.strip_suffix('%') {
            Some(pct) => pct.parse::<f64>().ok()? / 100.0,
            None => p.parse::<f64>().ok()? / 255.0,
        };
        Some(n.clamp(0.0, 1.0))
    };
    Some([ch()?, ch()?, ch()?])
}

fn luminance(c: [f64; 3]) -> f64 {
    let lin = |c: f64| {
        if c < 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

/// WCAG 2.x contrast ratio, `(hi + 0.05) / (lo + 0.05)` over relative luminance.
pub fn contrast(a: [f64; 3], b: [f64; 3]) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Every `(selector, property, px)` with `width` or `min-width` in whole pixels, except inside
/// a `max-width` media query narrower than 360px (which never applies to the phone).
pub fn fixed_widths(css: &Css) -> Vec<(String, String, u32)> {
    let mut out = vec![];
    for b in &css.blocks {
        let never = b.media.as_deref().is_some_and(|m| {
            MAX_WIDTH
                .captures(m)
                .and_then(|c| c[1].parse::<f64>().ok())
                .is_some_and(|w| w < 360.0)
        });
        if never {
            continue;
        }
        for (prop, value) in &b.decls {
            if (prop == "width" || prop == "min-width")
                && let Some(px) = PX.captures(value).and_then(|c| c[1].parse::<f64>().ok())
            {
                out.push((b.selector.clone(), prop.clone(), px.round() as u32));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    const PAGE: &str = r##"<html><body><header><nav class="crumbs" aria-label="Breadcrumb"><a href="x.html">Pattern matching</a><span>Guards</span></nav><span class="version">v1.0.0</span></header>
<main><h1>Guards</h1><div data-rdy="content"><p>A guard is <code>d-7KQ2M</code> evaluated <a href="#g">after</a> the pattern.</p><pre>_assets/x</pre>
<details class="why"><summary>Why</summary><div class="why-body" data-rdy="trail"><p>8 February 2021: ruled.</p></div></details>
<details data-rdy="agents"><summary>For agents</summary><dl><dt>Id</dt><dd>r-7KQ2M</dd></dl></details></div>
<script>var x = "p-00000";</script><style>.a{color:#fff}</style><p hidden>r-11111</p></main></body></html>"##;

    #[test]
    fn scan1_runs_carry_zone_and_label() {
        let runs = visible_runs(PAGE);
        let text = |z: Zone| {
            runs.iter()
                .filter(|r| r.zone == z)
                .map(|r| r.text.as_str())
                .collect::<Vec<_>>()
                .join(" | ")
        };
        assert_eq!(
            text(Zone::Chrome),
            "Pattern matching | Guards | v1.0.0 | Guards"
        );
        assert_eq!(
            text(Zone::Content),
            "A guard is | evaluated | after | the pattern. | Why"
        );
        assert_eq!(text(Zone::Trail), "8 February 2021: ruled.");
        assert_eq!(text(Zone::Agents), "For agents | Id | r-7KQ2M");
        let labels: Vec<&str> = runs
            .iter()
            .filter(|r| r.label)
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                "Pattern matching",
                "Guards",
                "v1.0.0",
                "Guards",
                "Why",
                "For agents",
                "Id"
            ]
        );
        assert!(!runs.iter().any(|r| r.text.contains("p-00000")
            || r.text.contains("#fff")
            || r.text.contains("r-11111")
            || r.text.contains("_assets")));
    }

    #[test]
    fn scan1_entities_are_decoded_and_whitespace_collapsed() {
        let runs = visible_runs("<p>a &amp; b\n\n  &lt;c&gt; &#8217;d&#x2019;</p>");
        assert_eq!(runs[0].text, "a & b <c> ’d’");
    }

    const CSS: &str = r#"/* c */ :root { --bg: #fbfbfc; --ink: #1b1e26; --pair-body: var(--ink) var(--bg) text; --pair-ring: var(--focus) var(--bg) ui; --focus: var(--ink); }
@media (prefers-color-scheme: dark) { :root:not([data-theme="light"]) { --bg: #121419; --ink: #e5e7ec; --focus: var(--ink); } }
:root[data-theme="dark"] { --bg: #121419; --ink: #e5e7ec; --focus: var(--ink); }
:root { --font-text: 'Onest', sans-serif; }
.x { color: var(--ink); width: 400px; background: rgba(0,0,0,0.3); border-color: Red; }
@media (max-width: 760px) { .y { min-width: 350px; } }"#;

    #[test]
    fn css1_tokens_pairs_and_literals_parse() {
        let css = parse_css(CSS);
        let t = tokens(&css).unwrap();
        assert_eq!(t.light["--bg"], "#fbfbfc");
        assert_eq!(t.dark_media["--bg"], "#121419");
        assert_eq!(t.dark_attr["--ink"], "#e5e7ec");
        assert_eq!(t.light["--font-text"], "'Onest', sans-serif");
        assert_eq!(t.pairs.len(), 2);
        assert_eq!(
            (
                t.pairs[0].fg.as_str(),
                t.pairs[0].bg.as_str(),
                t.pairs[0].kind
            ),
            ("--ink", "--bg", PairKind::Text)
        );
        let x = css.blocks.iter().find(|b| b.selector == ".x").unwrap();
        let lits: Vec<String> = x
            .decls
            .iter()
            .flat_map(|(_, v)| colour_literals(v))
            .collect();
        assert_eq!(lits, ["rgba(0,0,0,0.3)", "Red"]);
        assert_eq!(
            fixed_widths(&css),
            vec![
                (".x".to_string(), "width".to_string(), 400),
                (".y".to_string(), "min-width".to_string(), 350)
            ]
        );
        assert_eq!(resolve(&t.light, "--focus").as_deref(), Some("#1b1e26"));
    }

    #[test]
    fn css1_contrast_matches_wcag_reference_values() {
        let c = |a: &str, b: &str| contrast(srgb(a).unwrap(), srgb(b).unwrap());
        assert!((c("#000000", "#ffffff") - 21.0).abs() < 0.01);
        assert!((c("#1b1e26", "#fbfbfc") - 16.11).abs() < 0.02);
        assert!((c("#8f96a3", "#fbfbfc") - 2.88).abs() < 0.02);
        assert!((c("rgb(255, 255, 255)", "#777") - 4.48).abs() < 0.02);
    }
    #[test]
    fn css1_colour_literals_skip_vars_strings_urls_and_keywords() {
        let lits = |v: &str| colour_literals(v);
        assert_eq!(NAMED_COLOURS.len(), 148);
        assert!(lits("var(--red) transparent currentcolor inherit none").is_empty());
        assert!(lits("url(#fff) 'red' \"blue\" dark-red 2em 10px").is_empty());
        assert_eq!(
            lits("1px solid #FFF, 0 0 3px #0008 #12345678"),
            ["#FFF", "#0008", "#12345678"]
        );
        assert!(lits("#12345 #ggg").is_empty());
        assert_eq!(
            lits("linear-gradient(to right, hsl(0 0% 0%), WhiteSmoke 50%, var(--a, red))"),
            ["hsl(0 0% 0%)", "WhiteSmoke"]
        );
        assert_eq!(
            lits("color-mix(in srgb, oklch(50% 0.1 20), blue)"),
            ["oklch(50% 0.1 20)", "blue"]
        );
    }

    #[test]
    fn css1_at_rules_and_odd_values_parse() {
        let css = parse_css(
            "@charset \"utf-8\"; @font-face { font-family: X; src: url(data:font/woff2;base64,AAA=) format('woff2'); }\n@keyframes spin { from { color: red } to { color: blue } }\n@media (max-width: 300px) { .z { width: 100px } }\n.a,\n.b { width: 12.4px; min-width: 50% }",
        );
        let sels: Vec<&str> = css.blocks.iter().map(|b| b.selector.as_str()).collect();
        assert_eq!(sels, ["@font-face", "@keyframes spin", ".z", ".a, .b"]);
        assert_eq!(
            css.blocks[0].decls[1].1,
            "url(data:font/woff2;base64,AAA=) format('woff2')"
        );
        assert_eq!(css.blocks[1].decls.len(), 2);
        assert_eq!(css.blocks[2].media.as_deref(), Some("(max-width: 300px)"));
        assert_eq!(
            fixed_widths(&css),
            vec![(".a, .b".to_string(), "width".to_string(), 12)]
        );
    }

    #[test]
    fn css1_unparsable_pair_is_an_error_and_resolve_stops_on_cycles() {
        let t = tokens(&parse_css(":root { --pair-x: var(--a) text; }"));
        assert!(t.unwrap_err().contains("--pair-x"));
        let css = parse_css(
            ":root { --a: var(--b); --b: var(--a); --c: var(--missing, #fff); --d: var(--missing); }",
        );
        let t = tokens(&css).unwrap();
        assert_eq!(resolve(&t.light, "--a"), None);
        assert_eq!(resolve(&t.light, "--c").as_deref(), Some("#fff"));
        assert_eq!(resolve(&t.light, "--d"), None);
        assert_eq!(srgb("rgb(100% 0% 0% / 50%)"), Some([1.0, 0.0, 0.0]));
        assert_eq!(srgb("#ff000080"), Some([1.0, 0.0, 0.0]));
        assert_eq!(srgb("hsl(0 0% 0%)"), None);
    }

    #[test]
    fn scan1_titles_attribute_text_and_nbsp() {
        let html = "<h1>Guards <code>x</code></h1><h2 title=\"a > b\">Terms &amp; more</h2><p title='<p>no</p>'>a&nbsp;b</p>";
        assert_eq!(
            page_titles(html),
            [(1, "Guards x".to_string()), (2, "Terms & more".to_string())]
        );
        let runs = visible_runs(html);
        let texts: Vec<&str> = runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, ["Guards", "Terms & more", "a\u{a0}b"]);
        assert_eq!(runs[0].path, ["h1"]);
        assert!(runs[0].label && runs[1].label && !runs[2].label);
    }
}
