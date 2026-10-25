//! Drawn diagram SVG made safe for the site (D-FY4VF, D-KAGB2, D-E8P8J).
//!
//! [`standalone`] checks the SVG the pinned crate drew and returns the asset's bytes. It decodes
//! the label codes in the text nodes, renames the element ids `dgm-0`, `dgm-1`, …, scopes the
//! stylesheet to the root and sets the palette: rdy's light colors, then an OS-dark rule.
//! [`inline`] checks that asset again and returns its copy for one place on a page. It prefixes
//! the ids for that place and reads the palette from the site's tokens, so the page's Light or
//! Dark setting wins. It does not decode text again.
//!
//! One bounded reader serves both. It reads the input once, without recursion, and keeps at
//! most a fixed multiple of the input, which is at most 4194304 bytes. Each output is checked
//! against the same bound as it is written. The checks run in a fixed order, and the first
//! class that fails gives the reason: size, XML structure, active content, external resources,
//! element ids.

use crate::html::escape;
use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;

/// The most bytes an input or an output may have.
pub(crate) const MAX_BYTES: usize = 4194304;
pub(crate) const OVER: &str = "the drawn diagram is over 4194304 bytes";
pub(crate) const BAD_IDS: &str = "the drawn diagram has invalid element ids";
pub(crate) const NOT_SVG: &str = "the drawn diagram is not valid SVG";
pub(crate) const EXTERNAL: &str = "the drawn diagram holds an external resource";
pub(crate) const ACTIVE: &str = "the drawn diagram holds active content";

type Fault = &'static str;

const SVG_NS: &str = "http://www.w3.org/2000/svg";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

/// The passive elements. Every other element is active content.
const PASSIVE: [&str; 22] = [
    "svg",
    "g",
    "defs",
    "marker",
    "path",
    "rect",
    "circle",
    "ellipse",
    "line",
    "polyline",
    "polygon",
    "text",
    "tspan",
    "style",
    "title",
    "desc",
    "clipPath",
    "use",
    "symbol",
    "linearGradient",
    "radialGradient",
    "stop",
];

/// The pinned crate's nine root slots: its name, the site token, the light and the dark color.
pub(crate) const PALETTE: [(&str, &str, &str, &str); 9] = [
    ("bg", "--bg", "#fbfbfc", "#121419"),
    ("fg", "--ink", "#1b1e26", "#e5e7ec"),
    ("line", "--line-strong", "#c9cdd6", "#3a404c"),
    ("accent", "--accent", "#2456c9", "#8fb0ff"),
    ("muted", "--ink-soft", "#5d6472", "#a1a7b4"),
    ("surface", "--surface", "#f2f3f6", "#1a1d24"),
    ("border", "--line-ui", "#7f8695", "#737a89"),
    ("cluster-bkg", "--surface-2", "#e9ebf0", "#22262f"),
    ("note-fill", "--note", "#eef3ff", "#182136"),
];

/// rdy's label codes (`encode_label`), decoded in text only.
const LABEL_CODES: [(&str, char); 13] = [
    ("#34;", '"'),
    ("#35;", '#'),
    ("#59;", ';'),
    ("#60;", '<'),
    ("#62;", '>'),
    ("#91;", '['),
    ("#93;", ']'),
    ("#40;", '('),
    ("#41;", ')'),
    ("#123;", '{'),
    ("#125;", '}'),
    ("#124;", '|'),
    ("#96;", '`'),
];

/// Authored named entities, decoded in text only.
const AUTHORED: [(&str, char); 4] = [
    ("&lt;", '<'),
    ("&gt;", '>'),
    ("&amp;", '&'),
    ("&quot;", '"'),
];

/// The longest code: `&#x` and six hex digits, or `&#` and seven decimal digits, and `;`.
const LONGEST_CODE: usize = 10;

/// Checks a drawn SVG and returns the diagram's asset bytes.
pub(crate) fn standalone(svg: &str) -> Result<String, &'static str> {
    if svg.len() > MAX_BYTES {
        return Err(OVER);
    }
    let mut doc = parse(svg)?;
    check_active(&doc)?;
    check_resources(&doc)?;
    let ids = Ids::new(&doc, None)?;
    decode_labels(&mut doc);
    let mut out = Out::default();
    if let Some(decl) = doc.decl {
        out.put(Esc::Raw, decl);
        out.put(Esc::Raw, "\n");
    }
    write(&doc, &ids, Mode::Standalone, &mut out)?;
    out.put(Esc::Raw, "\n");
    out.finish()
}

/// Checks a diagram's asset again and returns its copy for one place on a page. `asset_hash`
/// is the 64 lowercase hex digits of the asset's hash; `occurrence` counts the diagram's places
/// on the page from zero.
pub(crate) fn inline(
    standalone: &str,
    asset_hash: &str,
    occurrence: usize,
) -> Result<String, &'static str> {
    if standalone.len() > MAX_BYTES {
        return Err(OVER);
    }
    let doc = parse(standalone)?;
    check_active(&doc)?;
    check_resources(&doc)?;
    let hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    if asset_hash.len() != 64 || !asset_hash.bytes().all(hex) {
        return Err(BAD_IDS);
    }
    let prefix = format!("dgm-{asset_hash}-{occurrence}-");
    let ids = Ids::new(&doc, Some(&prefix))?;
    let mut out = Out::default();
    write(&doc, &ids, Mode::Inline, &mut out)?;
    out.finish()
}

// ---- the XML reader ----

#[derive(Clone, Copy, PartialEq, Eq)]
enum AttrKind {
    Plain,
    /// `href` with or without a prefix
    Link,
    /// `xmlns` or `xmlns:*`, which is XML metadata
    Namespace,
}

struct Attr<'a> {
    name: &'a str,
    value: Cow<'a, str>,
    kind: AttrKind,
}

enum Tok<'a> {
    Start {
        name: &'a str,
        attrs: Range<usize>,
        /// The index of the matching `End`, or the start's own index for an empty element.
        close: usize,
        /// A style element's stylesheet.
        css: Option<usize>,
    },
    End,
    Text(Cow<'a, str>),
    Comment,
}

/// A document as one flat list of tokens; `toks[0]` starts the root element.
struct Doc<'a> {
    decl: Option<&'a str>,
    toks: Vec<Tok<'a>>,
    attrs: Vec<Attr<'a>>,
    css: Vec<Css>,
}

impl Doc<'_> {
    fn attrs(&self, start: usize) -> &[Attr<'_>] {
        match &self.toks[start] {
            Tok::Start { attrs, .. } => &self.attrs[attrs.clone()],
            _ => &[],
        }
    }

    fn close(&self, start: usize) -> usize {
        match self.toks[start] {
            Tok::Start { close, .. } => close,
            _ => start,
        }
    }
}

/// The namespace prefixes in scope.
#[derive(Default)]
struct Prefixes {
    bound: HashMap<String, Vec<String>>,
    frames: Vec<Vec<String>>,
}

impl Prefixes {
    fn enter(&mut self) {
        self.frames.push(Vec::new());
    }

    fn bind(&mut self, prefix: &str, uri: &str) {
        self.bound
            .entry(prefix.to_string())
            .or_default()
            .push(uri.to_string());
        if let Some(frame) = self.frames.last_mut() {
            frame.push(prefix.to_string());
        }
    }

    fn leave(&mut self) {
        for prefix in self.frames.pop().unwrap_or_default() {
            if let Some(uris) = self.bound.get_mut(&prefix) {
                uris.pop();
            }
        }
    }

    fn resolves(&self, prefix: &str) -> bool {
        prefix == "xml" || self.bound.get(prefix).is_some_and(|u| !u.is_empty())
    }
}

fn xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
}

fn xml_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

fn space_len(s: &str) -> usize {
    s.len() - s.trim_start_matches(xml_space).len()
}

fn name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == ':' || !c.is_ascii()
}

fn name_char(c: char) -> bool {
    name_start(c) || c.is_ascii_digit() || c == '-' || c == '.'
}

/// The XML name at the start of `s`.
fn name_at(s: &str) -> Option<&str> {
    let first = s.chars().next()?;
    if !name_start(first) {
        return None;
    }
    let end = s.find(|c: char| !name_char(c)).unwrap_or(s.len());
    Some(&s[..end])
}

/// A qualified name's prefix and local part, if it has a prefix.
fn split_qname(name: &str) -> Result<Option<(&str, &str)>, Fault> {
    match name.split_once(':') {
        None => Ok(None),
        Some((p, l)) if !p.is_empty() && !l.is_empty() && !l.contains(':') => Ok(Some((p, l))),
        Some(_) => Err(NOT_SVG),
    }
}

/// The character an XML reference at the start of `s` stands for, and its length.
fn xml_ref(s: &str) -> Option<(char, usize)> {
    let end = s.find(';')?;
    let body = &s[1..end];
    let c = match body {
        "lt" => '<',
        "gt" => '>',
        "amp" => '&',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let (digits, radix) = match body.strip_prefix("#x") {
                Some(h) => (h, 16),
                None => (body.strip_prefix('#')?, 10),
            };
            if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
                return None;
            }
            let n = u32::from_str_radix(digits, radix).ok()?;
            char::from_u32(n).filter(|&c| xml_char(c))?
        }
    };
    Some((c, end + 1))
}

/// XML-decodes character data or an attribute value: line ends become `\n`, and in an
/// attribute every literal white space character becomes a space.
fn decode(raw: &str, attr: bool) -> Result<Cow<'_, str>, Fault> {
    let plain = !raw.contains(['&', '\r']) && !(attr && raw.contains(['\t', '\n']));
    if plain {
        return Ok(Cow::Borrowed(raw));
    }
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while let Some(c) = raw[i..].chars().next() {
        match c {
            '&' => {
                let (ch, len) = xml_ref(&raw[i..]).ok_or(NOT_SVG)?;
                out.push(ch);
                i += len;
            }
            '\r' => {
                i += 1;
                if raw[i..].starts_with('\n') {
                    i += 1;
                }
                out.push(if attr { ' ' } else { '\n' });
            }
            '\n' | '\t' if attr => {
                out.push(' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    Ok(Cow::Owned(out))
}

/// Character data of a CDATA section: only its line ends change.
fn cdata(raw: &str) -> Cow<'_, str> {
    if raw.contains('\r') {
        Cow::Owned(raw.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(raw)
    }
}

/// Reads `src` as one SVG element in the SVG namespace.
fn parse(src: &str) -> Result<Doc<'_>, Fault> {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    if !src.chars().all(xml_char) {
        return Err(NOT_SVG);
    }
    let mut doc = Doc {
        decl: None,
        toks: Vec::new(),
        attrs: Vec::new(),
        css: Vec::new(),
    };
    let mut i = 0;
    if let Some(rest) = src.strip_prefix("<?xml")
        && rest.starts_with(xml_space)
    {
        let end = rest.find("?>").ok_or(NOT_SVG)?;
        let body = &rest[..end];
        if !body.trim_start_matches(xml_space).starts_with("version")
            || !body.is_ascii()
            || body.contains(['<', '>', '&'])
        {
            return Err(NOT_SVG);
        }
        i = 5 + end + 2;
        doc.decl = Some(&src[..i]);
    }
    let mut open: Vec<usize> = Vec::new();
    let mut prefixes = Prefixes::default();
    let mut done = false;
    while i < src.len() {
        let rest = &src[i..];
        if !rest.starts_with('<') {
            let end = rest.find('<').unwrap_or(rest.len());
            let raw = &rest[..end];
            if open.is_empty() {
                if !raw.chars().all(xml_space) {
                    return Err(NOT_SVG);
                }
            } else {
                if raw.contains("]]>") {
                    return Err(NOT_SVG);
                }
                doc.toks.push(Tok::Text(decode(raw, false)?));
            }
            i += end;
        } else if let Some(body) = rest.strip_prefix("<!--") {
            let end = body.find("-->").ok_or(NOT_SVG)?;
            let text = &body[..end];
            if text.contains("--") || text.ends_with('-') {
                return Err(NOT_SVG);
            }
            // a comment is never written; inside the root it still parts text runs
            if !open.is_empty() {
                doc.toks.push(Tok::Comment);
            }
            i += 4 + end + 3;
        } else if let Some(body) = rest.strip_prefix("<![CDATA[") {
            if open.is_empty() {
                return Err(NOT_SVG);
            }
            let end = body.find("]]>").ok_or(NOT_SVG)?;
            doc.toks.push(Tok::Text(cdata(&body[..end])));
            i += 9 + end + 3;
        } else if rest.starts_with("<!") || rest.starts_with("<?") {
            // a DTD, another declaration or a processing instruction
            return Err(NOT_SVG);
        } else if let Some(tail) = rest.strip_prefix("</") {
            let name = name_at(tail).ok_or(NOT_SVG)?;
            let after = &tail[name.len()..];
            let after = &after[space_len(after)..];
            if !after.starts_with('>') {
                return Err(NOT_SVG);
            }
            let start = open.pop().ok_or(NOT_SVG)?;
            if !matches!(doc.toks[start], Tok::Start { name: n, .. } if n == name) {
                return Err(NOT_SVG);
            }
            doc.toks.push(Tok::End);
            close(&mut doc, start)?;
            prefixes.leave();
            done = open.is_empty();
            i = src.len() - after.len() + 1;
        } else {
            if done {
                // a second root
                return Err(NOT_SVG);
            }
            let (len, empty) = start_tag(&mut doc, rest, open.is_empty(), &mut prefixes)?;
            let start = doc.toks.len() - 1;
            if empty {
                close(&mut doc, start)?;
                prefixes.leave();
                done = open.is_empty();
            } else {
                open.push(start);
            }
            i += len;
        }
    }
    if !done {
        return Err(NOT_SVG);
    }
    Ok(doc)
}

/// Reads the start tag at the start of `s`, pushes its token, and returns its length and
/// whether it is an empty-element tag.
fn start_tag<'a>(
    doc: &mut Doc<'a>,
    s: &'a str,
    root: bool,
    prefixes: &mut Prefixes,
) -> Result<(usize, bool), Fault> {
    let name = name_at(&s[1..]).ok_or(NOT_SVG)?;
    let mut j = 1 + name.len();
    let first = doc.attrs.len();
    let empty = loop {
        let ws = space_len(&s[j..]);
        j += ws;
        let rest = &s[j..];
        if rest.starts_with("/>") {
            j += 2;
            break true;
        }
        if rest.starts_with('>') {
            j += 1;
            break false;
        }
        if ws == 0 {
            return Err(NOT_SVG);
        }
        let attr = name_at(rest).ok_or(NOT_SVG)?;
        j += attr.len();
        j += space_len(&s[j..]);
        if !s[j..].starts_with('=') {
            return Err(NOT_SVG);
        }
        j += 1;
        j += space_len(&s[j..]);
        let quote = match s[j..].chars().next() {
            Some(q @ ('"' | '\'')) => q,
            _ => return Err(NOT_SVG),
        };
        j += 1;
        let len = s[j..].find(quote).ok_or(NOT_SVG)?;
        let raw = &s[j..j + len];
        if raw.contains('<') {
            return Err(NOT_SVG);
        }
        let value = decode(raw, true)?;
        doc.attrs.push(Attr {
            name: attr,
            value,
            kind: AttrKind::Plain,
        });
        j += len + 1;
    };
    let mut names: Vec<&str> = doc.attrs[first..].iter().map(|a| a.name).collect();
    names.sort_unstable();
    if names.windows(2).any(|w| w[0] == w[1]) {
        return Err(NOT_SVG);
    }
    prefixes.enter();
    let mut default_svg = false;
    for a in &doc.attrs[first..] {
        if a.name == "xmlns" {
            if a.value != SVG_NS {
                return Err(NOT_SVG);
            }
            default_svg = true;
        } else if let Some(p) = a.name.strip_prefix("xmlns:") {
            if p.is_empty() || p.contains(':') || p == "xmlns" || a.value.is_empty() {
                return Err(NOT_SVG);
            }
            if (p == "xml") != (a.value == XML_NS) {
                return Err(NOT_SVG);
            }
            prefixes.bind(p, &a.value);
        }
    }
    if root && (name != "svg" || !default_svg) {
        return Err(NOT_SVG);
    }
    if let Some((p, _)) = split_qname(name)?
        && !prefixes.resolves(p)
    {
        return Err(NOT_SVG);
    }
    for a in &mut doc.attrs[first..] {
        a.kind = if a.name == "xmlns" || a.name.starts_with("xmlns:") {
            AttrKind::Namespace
        } else {
            match split_qname(a.name)? {
                Some((p, _)) if !prefixes.resolves(p) => return Err(NOT_SVG),
                // an HTML parser reads `xlink:href` as a link whatever the prefix is bound to
                Some((_, "href")) => AttrKind::Link,
                None if a.name == "href" => AttrKind::Link,
                _ => AttrKind::Plain,
            }
        };
    }
    doc.toks.push(Tok::Start {
        name,
        attrs: first..doc.attrs.len(),
        close: usize::MAX,
        css: None,
    });
    Ok((j, empty))
}

/// Records where the element at `start` closes; a style element's text becomes its stylesheet.
fn close(doc: &mut Doc<'_>, start: usize) -> Result<(), Fault> {
    let end = doc.toks.len() - 1;
    let end = if end == start { start } else { end };
    let is_style = matches!(doc.toks[start], Tok::Start { name: "style", .. });
    let mut css = None;
    if is_style {
        let mut text = String::new();
        for t in &doc.toks[start + 1..end.max(start + 1)] {
            match t {
                Tok::Text(s) => text.push_str(s),
                _ => return Err(NOT_SVG),
            }
        }
        let items = parse_css(&text).ok_or(NOT_SVG)?;
        doc.css.push(Css { text, items });
        css = Some(doc.css.len() - 1);
    }
    if let Tok::Start {
        close, css: slot, ..
    } = &mut doc.toks[start]
    {
        *close = end;
        *slot = css;
    }
    Ok(())
}

// ---- the checks ----

fn check_active(doc: &Doc<'_>) -> Result<(), Fault> {
    for t in &doc.toks {
        if let Tok::Start { name, .. } = t
            && !PASSIVE.contains(name)
        {
            return Err(ACTIVE);
        }
    }
    let on = |n: &str| n.len() >= 2 && n[..2].eq_ignore_ascii_case("on");
    for a in &doc.attrs {
        let local = a.name.rsplit(':').next().unwrap_or(a.name);
        if on(a.name) || on(local) {
            return Err(ACTIVE);
        }
    }
    Ok(())
}

fn check_resources(doc: &Doc<'_>) -> Result<(), Fault> {
    for a in &doc.attrs {
        match a.kind {
            AttrKind::Namespace => {}
            AttrKind::Link if !local_target(&a.value) => return Err(EXTERNAL),
            AttrKind::Link => {}
            AttrKind::Plain if a.name == "xml:base" => return Err(EXTERNAL),
            AttrKind::Plain => {
                css_scan(&css_unescape(&a.value))?;
            }
        }
    }
    for css in &doc.css {
        css_scan(&css_unescape(&css.text))?;
    }
    Ok(())
}

/// Whether a reference names a local element: `#` and an id.
fn local_target(target: &str) -> bool {
    target.len() > 1 && target.starts_with('#')
}

/// The old and new name of every element id, and the root's new id.
struct Ids {
    map: HashMap<String, String>,
    root: String,
    root_has_id: bool,
}

impl Ids {
    /// Canonical ids (`dgm-0` for the root, then `dgm-1`, `dgm-2` in element order) without a
    /// prefix; with one, each id keeps its name after the prefix.
    fn new(doc: &Doc<'_>, prefix: Option<&str>) -> Result<Ids, Fault> {
        let mut map = HashMap::new();
        let mut next = 0;
        let mut root_id = None;
        for (k, t) in doc.toks.iter().enumerate() {
            if !matches!(t, Tok::Start { .. }) {
                continue;
            }
            let Some(id) = doc.attrs(k).iter().find(|a| a.name == "id") else {
                continue;
            };
            let old: &str = &id.value;
            if !valid_id(old) {
                return Err(BAD_IDS);
            }
            let new = match prefix {
                Some(p) => format!("{p}{old}"),
                None if k == 0 => "dgm-0".to_string(),
                None => {
                    next += 1;
                    format!("dgm-{next}")
                }
            };
            if k == 0 {
                root_id = Some(new.clone());
            }
            if map.insert(old.to_string(), new).is_some() {
                return Err(BAD_IDS);
            }
        }
        let root_has_id = root_id.is_some();
        let root = match root_id {
            Some(r) => r,
            None => {
                let r = format!("{}dgm-0", prefix.unwrap_or(""));
                if map.values().any(|v| *v == r) {
                    return Err(BAD_IDS);
                }
                r
            }
        };
        Ok(Ids {
            map,
            root,
            root_has_id,
        })
    }

    fn get(&self, old: &str) -> Result<&str, Fault> {
        self.map.get(old).map(String::as_str).ok_or(BAD_IDS)
    }
}

/// `[A-Za-z_][A-Za-z0-9_.:-]*`
fn valid_id(id: &str) -> bool {
    let mut bytes = id.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

// ---- label text ----

/// Decodes rdy's label codes and authored entities in the text nodes, once. Consecutive
/// `text` siblings with the same attributes except `y` make one label run, with their tspans;
/// a code may run over its lines. Style text is never decoded.
fn decode_labels(doc: &mut Doc<'_>) {
    let n = doc.toks.len();
    let mut covered = vec![false; n];
    for p in 0..n {
        let Tok::Start { name, close, .. } = doc.toks[p] else {
            continue;
        };
        if covered[p] || name == "style" || close == p {
            continue;
        }
        let mut group: Vec<usize> = Vec::new();
        let mut j = p + 1;
        while j < close {
            match &doc.toks[j] {
                Tok::Start {
                    name: "text",
                    close: c,
                    ..
                } => {
                    let c = *c;
                    if let Some(&first) = group.first()
                        && !same_but_y(doc.attrs(first), doc.attrs(j))
                    {
                        decode_group(doc, &group, &mut covered);
                        group.clear();
                    }
                    group.push(j);
                    j = c + 1;
                }
                Tok::Start { close: c, .. } => {
                    let c = *c;
                    decode_group(doc, &group, &mut covered);
                    group.clear();
                    j = c + 1;
                }
                Tok::Text(t) => {
                    if !t.chars().all(xml_space) {
                        decode_group(doc, &group, &mut covered);
                        group.clear();
                        decode_nodes(doc, &[j]);
                    }
                    j += 1;
                }
                Tok::Comment | Tok::End => {
                    decode_group(doc, &group, &mut covered);
                    group.clear();
                    j += 1;
                }
            }
        }
        decode_group(doc, &group, &mut covered);
    }
}

fn same_but_y(a: &[Attr<'_>], b: &[Attr<'_>]) -> bool {
    let a = a.iter().filter(|x| x.name != "y");
    let b = b.iter().filter(|x| x.name != "y");
    a.map(|x| (x.name, x.value.as_ref()))
        .eq(b.map(|x| (x.name, x.value.as_ref())))
}

/// Decodes one label run: the text nodes of `texts`, in order, outside style elements.
fn decode_group(doc: &mut Doc<'_>, texts: &[usize], covered: &mut [bool]) {
    if texts.is_empty() {
        return;
    }
    let mut nodes = Vec::new();
    for &s in texts {
        let end = doc.close(s);
        covered[s..=end].iter_mut().for_each(|c| *c = true);
        let mut k = s + 1;
        while k < end {
            match &doc.toks[k] {
                Tok::Start {
                    name: "style",
                    close,
                    ..
                } => k = *close + 1,
                Tok::Text(_) => {
                    nodes.push(k);
                    k += 1;
                }
                _ => k += 1,
            }
        }
    }
    decode_nodes(doc, &nodes);
}

fn decode_nodes(doc: &mut Doc<'_>, nodes: &[usize]) {
    let parts: Vec<&str> = nodes
        .iter()
        .map(|&k| match &doc.toks[k] {
            Tok::Text(t) => t.as_ref(),
            _ => "",
        })
        .collect();
    if !parts.iter().any(|p| p.contains(['#', '&'])) {
        return;
    }
    let decoded = decode_run(&parts);
    for (&k, text) in nodes.iter().zip(decoded) {
        doc.toks[k] = Tok::Text(Cow::Owned(text));
    }
}

/// One pass over a run's characters. A decoded character goes where its code began; the rest
/// of the code leaves the later nodes. Every node stays, even when it ends up empty.
fn decode_run(parts: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = parts
        .iter()
        .map(|p| String::with_capacity(p.len()))
        .collect();
    let next = |node: &mut usize, off: &mut usize| -> Option<(usize, char)> {
        while *node < parts.len() && *off >= parts[*node].len() {
            *node += 1;
            *off = 0;
        }
        let c = parts.get(*node)?[*off..].chars().next()?;
        let at = *node;
        *off += c.len_utf8();
        Some((at, c))
    };
    let (mut node, mut off) = (0, 0);
    loop {
        let (save_node, save_off) = (node, off);
        let Some((at, c)) = next(&mut node, &mut off) else {
            break;
        };
        if c == '#' || c == '&' {
            let mut ahead = ['\0'; LONGEST_CODE];
            let mut len = 0;
            let (mut n2, mut o2) = (save_node, save_off);
            while len < LONGEST_CODE {
                let Some((_, ch)) = next(&mut n2, &mut o2) else {
                    break;
                };
                ahead[len] = ch;
                len += 1;
            }
            if let Some((ch, used)) = label_code(&ahead[..len]) {
                out[at].push(ch);
                (node, off) = (save_node, save_off);
                for _ in 0..used {
                    next(&mut node, &mut off);
                }
                continue;
            }
        }
        out[at].push(c);
    }
    out
}

/// The character the code at the start of `ahead` stands for, and the code's length.
fn label_code(ahead: &[char]) -> Option<(char, usize)> {
    let starts =
        |s: &str| s.chars().count() <= ahead.len() && s.chars().zip(ahead).all(|(a, b)| a == *b);
    for (code, ch) in LABEL_CODES.iter().chain(&AUTHORED) {
        if starts(code) {
            return Some((*ch, code.len()));
        }
    }
    let (from, radix, most) = if starts("&#x") {
        (3, 16, 6)
    } else if starts("&#") {
        (2, 10, 7)
    } else {
        return None;
    };
    let digits = ahead[from..]
        .iter()
        .take_while(|c| c.is_digit(radix))
        .count();
    if digits == 0 || digits > most || ahead.get(from + digits) != Some(&';') {
        return None;
    }
    let text: String = ahead[from..from + digits].iter().collect();
    let c = char::from_u32(u32::from_str_radix(&text, radix).ok()?).filter(|&c| xml_char(c))?;
    Some((c, from + digits + 1))
}

// ---- CSS ----

struct Css {
    text: String,
    items: Vec<CssItem>,
}

enum CssItem {
    /// White space and comments between rules.
    Gap(Range<usize>),
    Rule {
        prelude: Range<usize>,
        body: Range<usize>,
    },
    /// `@media …` with its rules; it holds no other at-rule.
    Media {
        head: Range<usize>,
        items: Vec<CssItem>,
    },
    /// `@import` or `@font-face`: always an external resource.
    Fetch,
}

fn ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b >= 0x80
}

fn css_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0C)
}

fn ident_end(b: &[u8], from: usize) -> usize {
    from + b[from..].iter().take_while(|&&c| ident_byte(c)).count()
}

/// Where the string opened at `i` ends, and whether it closed.
fn string_end(b: &[u8], i: usize) -> (usize, bool) {
    let quote = b[i];
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            c if c == quote => return (j + 1, true),
            b'\n' | b'\r' | 0x0C => return (j, false),
            _ => j += 1,
        }
    }
    (b.len(), false)
}

/// Where the comment opened at `i` ends, and whether it closed.
fn comment_end(t: &str, i: usize) -> (usize, bool) {
    match t[i + 2..].find("*/") {
        Some(k) => (i + 2 + k + 2, true),
        None => (t.len(), false),
    }
}

/// Where the escape at `i` ends: the backslash and the next character.
fn escape_end(t: &str, i: usize) -> usize {
    i + 1 + t[i + 1..].chars().next().map_or(0, char::len_utf8)
}

/// Skips white space and comments from `i`; `None` for an unclosed comment.
fn skip_gap(t: &str, mut i: usize) -> Option<usize> {
    let b = t.as_bytes();
    loop {
        if i < b.len() && css_space(b[i]) {
            i += 1;
        } else if t[i..].starts_with("/*") {
            let (end, closed) = comment_end(t, i);
            if !closed {
                return None;
            }
            i = end;
        } else {
            return Some(i);
        }
    }
}

/// The index of the first `want` from `i` outside strings and comments; `None` at a byte of
/// `stop` or the end.
fn find_css(t: &str, mut i: usize, want: u8, stop: &[u8]) -> Option<usize> {
    let b = t.as_bytes();
    while i < b.len() {
        match b[i] {
            c if c == want => return Some(i),
            c if stop.contains(&c) => return None,
            b'"' | b'\'' => {
                let (end, closed) = string_end(b, i);
                if !closed {
                    return None;
                }
                i = end;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let (end, closed) = comment_end(t, i);
                if !closed {
                    return None;
                }
                i = end;
            }
            b'\\' => i = escape_end(t, i),
            _ => i += 1,
        }
    }
    None
}

/// Reads a stylesheet: rules, `@media` blocks of rules, `@import` and `@font-face`. Any other
/// at-rule, a nested block or an unclosed string, comment or block is not valid.
fn parse_css(t: &str) -> Option<Vec<CssItem>> {
    let b = t.as_bytes();
    let mut top = Vec::new();
    let mut media: Option<(Range<usize>, Vec<CssItem>)> = None;
    let mut i = 0;
    loop {
        let gap = skip_gap(t, i)?;
        if gap > i {
            let item = CssItem::Gap(i..gap);
            match &mut media {
                Some((_, inner)) => inner.push(item),
                None => top.push(item),
            }
        }
        i = gap;
        if i == b.len() {
            return media.is_none().then_some(top);
        }
        match b[i] {
            b'}' => {
                let (head, items) = media.take()?;
                top.push(CssItem::Media { head, items });
                i += 1;
            }
            b'@' if media.is_none() => {
                let end = ident_end(b, i + 1);
                let word = &t[i + 1..end];
                if word.eq_ignore_ascii_case("media") {
                    let brace = find_css(t, end, b'{', b";}")?;
                    media = Some((i..brace, Vec::new()));
                    i = brace + 1;
                } else if word.eq_ignore_ascii_case("import") {
                    i = find_css(t, end, b';', b"{}")? + 1;
                    top.push(CssItem::Fetch);
                } else if word.eq_ignore_ascii_case("font-face") {
                    let brace = find_css(t, end, b'{', b";}")?;
                    i = find_css(t, brace + 1, b'}', b"{")? + 1;
                    top.push(CssItem::Fetch);
                } else {
                    return None;
                }
            }
            b'@' => return None,
            _ => {
                let brace = find_css(t, i, b'{', b";}")?;
                let close = find_css(t, brace + 1, b'}', b"{")?;
                if !valid_prelude(&t[i..brace]) {
                    return None;
                }
                let item = CssItem::Rule {
                    prelude: i..brace,
                    body: brace + 1..close,
                };
                match &mut media {
                    Some((_, inner)) => inner.push(item),
                    None => top.push(item),
                }
                i = close + 1;
            }
        }
    }
}

/// The ranges of `t` between top-level `sep` bytes, outside strings, comments, parentheses
/// and brackets.
fn split_top(t: &str, sep: u8) -> Vec<Range<usize>> {
    let b = t.as_bytes();
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut from = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => i = string_end(b, i).0,
            b'/' if b.get(i + 1) == Some(&b'*') => i = comment_end(t, i).0,
            b'\\' => i = escape_end(t, i),
            b'(' | b'[' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            c if c == sep && depth == 0 => {
                parts.push(from..i);
                i += 1;
                from = i;
            }
            _ => i += 1,
        }
    }
    parts.push(from..b.len());
    parts
}

/// Every selector of a prelude is nonempty and starts with no combinator.
fn valid_prelude(p: &str) -> bool {
    split_top(p, b',').into_iter().all(|r| {
        let s = p[r].trim_matches(|c: char| c.is_ascii_whitespace());
        !s.is_empty() && !s.starts_with(['>', '+', '~'])
    })
}

/// Decodes CSS escapes, so that an escaped name reads as the name.
fn css_unescape(t: &str) -> Cow<'_, str> {
    if !t.contains('\\') {
        return Cow::Borrowed(t);
    }
    let mut out = String::with_capacity(t.len());
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            None => out.push('\u{FFFD}'),
            Some('\n' | '\r' | '\x0C') => {
                chars.next();
            }
            Some(h) if h.is_ascii_hexdigit() => {
                let mut n = 0u32;
                for _ in 0..6 {
                    match chars.peek().and_then(|c| c.to_digit(16)) {
                        Some(d) => {
                            n = n * 16 + d;
                            chars.next();
                        }
                        None => break,
                    }
                }
                if chars.peek().is_some_and(|c| c.is_ascii_whitespace()) {
                    chars.next();
                }
                let c = char::from_u32(n).filter(|&c| c != '\0');
                out.push(c.unwrap_or('\u{FFFD}'));
            }
            Some(other) => {
                out.push(other);
                chars.next();
            }
        }
    }
    Cow::Owned(out)
}

/// The target of the `url(` argument that starts at `from`, and where the function ends.
fn url_arg(t: &str, from: usize) -> (Range<usize>, usize) {
    let b = t.as_bytes();
    let mut k = from;
    while k < b.len() && css_space(b[k]) {
        k += 1;
    }
    let target = if k < b.len() && matches!(b[k], b'"' | b'\'') {
        let (end, closed) = string_end(b, k);
        let range = k + 1..if closed { end - 1 } else { end };
        k = end;
        range
    } else {
        let start = k;
        while k < b.len() && b[k] != b')' && !css_space(b[k]) {
            k += 1;
        }
        start..k
    };
    while k < b.len() && css_space(b[k]) {
        k += 1;
    }
    if k < b.len() && b[k] == b')' {
        k += 1;
    }
    (target, k)
}

/// Whether the byte at `i` starts a CSS name.
fn name_starts(b: &[u8], i: usize) -> bool {
    ident_byte(b[i]) && (i == 0 || !(ident_byte(b[i - 1]) || b[i - 1] == b'\\'))
}

/// Checks CSS for resources outside the document: every `url()` names a local id, and there
/// is no `@import`, `@font-face` or other function that fetches. Returns the count of local
/// `url()` references.
fn css_scan(t: &str) -> Result<usize, Fault> {
    let b = t.as_bytes();
    let mut locals = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => i = string_end(b, i).0,
            b'/' if b.get(i + 1) == Some(&b'*') => i = comment_end(t, i).0,
            b'\\' => i = escape_end(t, i),
            b'@' => {
                let end = ident_end(b, i + 1);
                let word = &t[i + 1..end];
                if word.eq_ignore_ascii_case("import") || word.eq_ignore_ascii_case("font-face") {
                    return Err(EXTERNAL);
                }
                i = end.max(i + 1);
            }
            _ if name_starts(b, i) => {
                let end = ident_end(b, i);
                let word = &t[i..end];
                i = end;
                if b.get(end) != Some(&b'(') {
                    continue;
                }
                if word.eq_ignore_ascii_case("url") {
                    let (target, after) = url_arg(t, end + 1);
                    if !local_target(&t[target]) {
                        return Err(EXTERNAL);
                    }
                    locals += 1;
                    i = after;
                } else if ["src", "image", "image-set", "-webkit-image-set"]
                    .iter()
                    .any(|f| word.eq_ignore_ascii_case(f))
                {
                    return Err(EXTERNAL);
                }
            }
            _ => i += 1,
        }
    }
    Ok(locals)
}

// ---- the writer ----

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Standalone,
    Inline,
}

/// Which custom-property names a CSS value renames from `--X` to `--dgm-X`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Rename {
    /// every declared or referenced name: style attributes and style elements
    All,
    /// names inside `var()`: other attributes
    VarOnly,
    None,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Esc {
    Raw,
    Text,
    Attr,
}

/// The output, bounded by [`MAX_BYTES`]. Once over, it keeps nothing more, so a later
/// element id fault still wins.
#[derive(Default)]
struct Out {
    s: String,
    over: bool,
}

impl Out {
    fn put(&mut self, esc: Esc, t: &str) {
        let len = match esc {
            Esc::Raw => t.len(),
            Esc::Text | Esc::Attr => t
                .chars()
                .map(|c| match c {
                    '&' | '\'' => 5,
                    '<' | '>' => 4,
                    '"' => 6,
                    '\r' => 5,
                    '\n' if esc == Esc::Attr => 5,
                    '\t' if esc == Esc::Attr => 4,
                    c => c.len_utf8(),
                })
                .sum(),
        };
        if self.over || self.s.len() + len > MAX_BYTES {
            self.over = true;
            return;
        }
        match esc {
            Esc::Raw => self.s.push_str(t),
            Esc::Text => self.s.push_str(&escape(t).replace('\r', "&#13;")),
            Esc::Attr => {
                let e = escape(t);
                if e.contains(['\t', '\n', '\r']) {
                    let e = e
                        .replace('\t', "&#9;")
                        .replace('\n', "&#10;")
                        .replace('\r', "&#13;");
                    self.s.push_str(&e);
                } else {
                    self.s.push_str(&e);
                }
            }
        }
    }

    fn finish(self) -> Result<String, Fault> {
        if self.over { Err(OVER) } else { Ok(self.s) }
    }
}

/// Which colors a palette rule gives the slots.
#[derive(Clone, Copy)]
enum Column {
    Site,
    Light,
    Dark,
}

fn palette_rule(root: &str, column: Column) -> String {
    let decls: Vec<String> = PALETTE
        .iter()
        .map(|&(slot, token, light, dark)| match column {
            Column::Site => format!("--dgm-{slot}: var({token});"),
            Column::Light => format!("--dgm-{slot}: {light};"),
            Column::Dark => format!("--dgm-{slot}: {dark};"),
        })
        .collect();
    format!("#{root} {{ {} }}", decls.join(" "))
}

/// The palette written first in the stylesheet: the asset's light colors and its OS-dark rule,
/// or the site's tokens inline.
fn palette(mode: Mode, root: &str) -> String {
    match mode {
        Mode::Standalone => format!(
            "\n    {}\n    @media (prefers-color-scheme: dark) {{\n      {}\n    }}",
            palette_rule(root, Column::Light),
            palette_rule(root, Column::Dark)
        ),
        Mode::Inline => format!("\n    {}", palette_rule(root, Column::Site)),
    }
}

/// Writes the root element and everything in it.
fn write(doc: &Doc<'_>, ids: &Ids, mode: Mode, out: &mut Out) -> Result<(), Fault> {
    let has_style = !doc.css.is_empty();
    let mut palette_done = false;
    let mut open: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < doc.toks.len() {
        match &doc.toks[i] {
            Tok::Start {
                name, close, css, ..
            } => {
                let root = i == 0;
                out.put(Esc::Raw, "<");
                out.put(Esc::Raw, name);
                if root && !ids.root_has_id {
                    out.put(Esc::Raw, " id=\"");
                    out.put(Esc::Attr, &ids.root);
                    out.put(Esc::Raw, "\"");
                }
                let mut root_style = false;
                for a in doc.attrs(i) {
                    root_style |= root && a.name == "style";
                    write_attr(a, root, ids, mode, out)?;
                }
                if root && !root_style {
                    out.put(Esc::Raw, " style=\"background-color: var(--dgm-bg);\"");
                }
                if let Some(c) = css {
                    out.put(Esc::Raw, ">");
                    if !palette_done {
                        palette_done = true;
                        out.put(Esc::Text, &palette(mode, &ids.root));
                    }
                    write_css(&doc.css[*c], ids, mode, out)?;
                    out.put(Esc::Raw, "</style>");
                    i = *close + 1;
                    continue;
                }
                let empty = *close == i;
                if empty && !(root && !has_style) {
                    out.put(Esc::Raw, " />");
                } else {
                    out.put(Esc::Raw, ">");
                    if root && !has_style {
                        out.put(Esc::Raw, "<style>");
                        out.put(Esc::Text, &palette(mode, &ids.root));
                        out.put(Esc::Raw, "</style>");
                    }
                    if empty {
                        out.put(Esc::Raw, "</");
                        out.put(Esc::Raw, name);
                        out.put(Esc::Raw, ">");
                    } else {
                        open.push(name);
                    }
                }
            }
            Tok::End => {
                out.put(Esc::Raw, "</");
                out.put(Esc::Raw, open.pop().unwrap_or_default());
                out.put(Esc::Raw, ">");
            }
            Tok::Text(t) => out.put(Esc::Text, t),
            Tok::Comment => {}
        }
        i += 1;
    }
    Ok(())
}

fn write_attr(a: &Attr<'_>, root: bool, ids: &Ids, mode: Mode, out: &mut Out) -> Result<(), Fault> {
    out.put(Esc::Raw, " ");
    out.put(Esc::Raw, a.name);
    out.put(Esc::Raw, "=\"");
    let standalone = mode == Mode::Standalone;
    match a.kind {
        AttrKind::Namespace => out.put(Esc::Attr, &a.value),
        AttrKind::Link => {
            out.put(Esc::Attr, "#");
            out.put(Esc::Attr, ids.get(&a.value[1..])?);
        }
        AttrKind::Plain if a.name == "id" => out.put(Esc::Attr, ids.get(&a.value)?),
        AttrKind::Plain if a.name == "style" && root => {
            write_root_style(&a.value, ids, standalone, out)?
        }
        AttrKind::Plain if a.name == "style" => {
            let rename = if standalone {
                Rename::All
            } else {
                Rename::None
            };
            write_css_value(&a.value, ids, rename, Esc::Attr, out)?
        }
        AttrKind::Plain => {
            let rename = if standalone {
                Rename::VarOnly
            } else {
                Rename::None
            };
            write_css_value(&a.value, ids, rename, Esc::Attr, out)?
        }
    }
    out.put(Esc::Raw, "\"");
    Ok(())
}

/// The name a declaration declares, trimmed.
fn decl_name(seg: &str) -> &str {
    let mut s = seg.trim_start_matches(|c: char| c.is_ascii_whitespace());
    while let Some(rest) = s.strip_prefix("/*") {
        s = rest
            .find("*/")
            .map_or("", |k| &rest[k + 2..])
            .trim_start_matches(|c: char| c.is_ascii_whitespace());
    }
    s.split(':').next().unwrap_or("").trim_end()
}

/// Whether a declaration's name is one of the nine palette slots, once renamed.
fn is_slot(name: &str, renamed: bool) -> bool {
    let Some(rest) = name.strip_prefix("--") else {
        return false;
    };
    let slot = if renamed {
        rest
    } else {
        match rest.strip_prefix("dgm-") {
            Some(s) => s,
            None => return false,
        }
    };
    PALETTE.iter().any(|p| p.0 == slot)
}

/// The root's style: its palette slots move to the stylesheet, and its background follows the
/// `bg` slot.
fn write_root_style(v: &str, ids: &Ids, standalone: bool, out: &mut Out) -> Result<(), Fault> {
    let rename = if standalone {
        Rename::All
    } else {
        Rename::None
    };
    let mut wrote = false;
    let mut background = false;
    for r in split_top(v, b';') {
        let seg = &v[r];
        if seg.trim().is_empty() || is_slot(decl_name(seg), standalone) {
            continue;
        }
        if decl_name(seg).eq_ignore_ascii_case("background-color") {
            let head = seg.find(':').map_or(seg, |c| &seg[..c]);
            out.put(Esc::Attr, head);
            out.put(Esc::Attr, ": var(--dgm-bg);");
            background = true;
        } else {
            write_css_value(seg, ids, rename, Esc::Attr, out)?;
            out.put(Esc::Attr, ";");
        }
        wrote = true;
    }
    if !background {
        out.put(Esc::Attr, if wrote { " " } else { "" });
        out.put(Esc::Attr, "background-color: var(--dgm-bg);");
    }
    Ok(())
}

/// Writes one CSS value or declaration list with its custom properties renamed and its local
/// `url()` references mapped. A reference the map cannot reach is an element id fault.
fn write_css_value(
    t: &str,
    ids: &Ids,
    rename: Rename,
    esc: Esc,
    out: &mut Out,
) -> Result<(), Fault> {
    let b = t.as_bytes();
    let mut locals = 0;
    let mut last = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => i = string_end(b, i).0,
            b'/' if b.get(i + 1) == Some(&b'*') => i = comment_end(t, i).0,
            b'\\' => i = escape_end(t, i),
            _ if name_starts(b, i) => {
                let end = ident_end(b, i);
                let word = &t[i..end];
                if rename == Rename::All && word.len() > 2 && word.starts_with("--") {
                    out.put(esc, &t[last..i]);
                    out.put(esc, "--dgm-");
                    out.put(esc, &word[2..]);
                    last = end;
                    i = end;
                    continue;
                }
                i = end;
                if b.get(end) != Some(&b'(') {
                    continue;
                }
                if word.eq_ignore_ascii_case("url") {
                    let (target, after) = url_arg(t, end + 1);
                    let name = t[target.clone()].strip_prefix('#').ok_or(BAD_IDS)?;
                    out.put(esc, &t[last..target.start]);
                    out.put(esc, "#");
                    out.put(esc, ids.get(name)?);
                    last = target.end;
                    locals += 1;
                    i = after;
                } else if rename == Rename::VarOnly && word.eq_ignore_ascii_case("var") {
                    let mut k = end + 1;
                    while k < b.len() && css_space(b[k]) {
                        k += 1;
                    }
                    let e = ident_end(b, k);
                    if e - k > 2 && t[k..].starts_with("--") {
                        out.put(esc, &t[last..k]);
                        out.put(esc, "--dgm-");
                        out.put(esc, &t[k + 2..e]);
                        last = e;
                        i = e;
                    }
                }
            }
            _ => i += 1,
        }
    }
    out.put(esc, &t[last..]);
    // an escape can hide a `url()` that the rewrite above does not see
    if css_scan(&css_unescape(t)).map_err(|_| BAD_IDS)? != locals {
        return Err(BAD_IDS);
    }
    Ok(())
}

/// Writes a stylesheet scoped to the root. Palette declarations of the stylesheet give way to
/// the palette written first, and a rule or `@media` block left empty by that is dropped
/// with the gap before it.
fn write_css(css: &Css, ids: &Ids, mode: Mode, out: &mut Out) -> Result<(), Fault> {
    let renamed = mode == Mode::Standalone;
    let rename = if renamed { Rename::All } else { Rename::None };
    let t = css.text.as_str();
    let dropped = |item: &CssItem| match item {
        CssItem::Rule { body, .. } => palette_only(&t[body.clone()], renamed),
        CssItem::Media { items, .. } => {
            let rules = items.iter().filter(|r| matches!(r, CssItem::Rule { .. }));
            let mut any = false;
            let all = rules.into_iter().all(|r| {
                any = true;
                matches!(r, CssItem::Rule { body, .. } if palette_only(&t[body.clone()], renamed))
            });
            any && all
        }
        _ => false,
    };
    let keep = |items: &[CssItem]| -> Vec<bool> {
        let mut keep = vec![true; items.len()];
        for (k, item) in items.iter().enumerate() {
            if dropped(item) {
                keep[k] = false;
                if k > 0 && matches!(items[k - 1], CssItem::Gap(_)) {
                    keep[k - 1] = false;
                }
            }
        }
        keep
    };
    let top_keep = keep(&css.items);
    for (item, kept) in css.items.iter().zip(top_keep) {
        if !kept {
            continue;
        }
        match item {
            CssItem::Gap(r) => out.put(Esc::Text, &t[r.clone()]),
            CssItem::Rule { prelude, body } => {
                write_rule(t, prelude, body, ids, rename, renamed, out)?
            }
            CssItem::Media { head, items } => {
                out.put(Esc::Text, &t[head.clone()]);
                out.put(Esc::Text, "{");
                for (inner, kept) in items.iter().zip(keep(items)) {
                    match inner {
                        _ if !kept => {}
                        CssItem::Gap(r) => out.put(Esc::Text, &t[r.clone()]),
                        CssItem::Rule { prelude, body } => {
                            write_rule(t, prelude, body, ids, rename, renamed, out)?
                        }
                        _ => return Err(EXTERNAL),
                    }
                }
                out.put(Esc::Text, "}");
            }
            CssItem::Fetch => return Err(EXTERNAL),
        }
    }
    Ok(())
}

/// Whether a rule body declares palette slots and nothing else.
fn palette_only(body: &str, renamed: bool) -> bool {
    let mut slots = false;
    for r in split_top(body, b';') {
        let seg = &body[r];
        if is_slot(decl_name(seg), renamed) {
            slots = true;
        } else if !seg.trim().is_empty() {
            return false;
        }
    }
    slots
}

fn write_rule(
    t: &str,
    prelude: &Range<usize>,
    body: &Range<usize>,
    ids: &Ids,
    rename: Rename,
    renamed: bool,
    out: &mut Out,
) -> Result<(), Fault> {
    write_prelude(&t[prelude.clone()], ids, out)?;
    out.put(Esc::Text, "{");
    let body = &t[body.clone()];
    let segs = split_top(body, b';');
    let last = segs.len() - 1;
    for (k, r) in segs.into_iter().enumerate() {
        let seg = &body[r];
        if is_slot(decl_name(seg), renamed) {
            continue;
        }
        write_css_value(seg, ids, rename, Esc::Text, out)?;
        if k < last {
            out.put(Esc::Text, ";");
        }
    }
    out.put(Esc::Text, "}");
    Ok(())
}

/// Writes a prelude with each selector scoped to the root. A selector that starts at the root
/// (`svg`, `:root` or the root's id) addresses the root itself; any other selector, and one
/// whose root is followed by a sibling combinator, applies inside the root.
fn write_prelude(p: &str, ids: &Ids, out: &mut Out) -> Result<(), Fault> {
    let parts = split_top(p, b',');
    let count = parts.len();
    for (k, r) in parts.into_iter().enumerate() {
        let part = &p[r];
        let core = part.trim_matches(|c: char| c.is_ascii_whitespace());
        let lead = &part[..part.len()
            - part
                .trim_start_matches(|c: char| c.is_ascii_whitespace())
                .len()];
        let trail = &part[lead.len() + core.len()..];
        out.put(Esc::Text, lead);
        let skip = root_compound(core, ids);
        out.put(Esc::Text, "#");
        out.put(Esc::Text, &ids.root);
        match skip {
            Some(len) => write_selector(&core[len..], ids, out)?,
            None => {
                out.put(Esc::Text, " ");
                write_selector(core, ids, out)?;
            }
        }
        out.put(Esc::Text, trail);
        if k + 1 < count {
            out.put(Esc::Text, ",");
        }
    }
    Ok(())
}

/// The length of a selector's leading root selector (`svg`, `:root` or `#<root id>`), when the
/// selector starts at the root and goes on with no sibling combinator.
fn root_compound(core: &str, ids: &Ids) -> Option<usize> {
    let b = core.as_bytes();
    let len = if core.starts_with("svg") {
        3
    } else if core.starts_with(":root") {
        5
    } else {
        let rest = core.strip_prefix('#')?;
        let end = ident_end(rest.as_bytes(), 0);
        if ids.map.get(&rest[..end]) != Some(&ids.root) {
            return None;
        }
        1 + end
    };
    if b.get(len).is_some_and(|&c| ident_byte(c) || c == b'(') {
        return None;
    }
    // the compound's end: the first combinator outside strings and brackets
    let mut depth = 0usize;
    let mut i = len;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => i = string_end(b, i).0,
            b'\\' => i = escape_end(core, i),
            b'(' | b'[' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            c if depth == 0 && (css_space(c) || matches!(c, b'>' | b'+' | b'~')) => break,
            _ => i += 1,
        }
    }
    let next = core[i..].trim_start_matches(|c: char| c.is_ascii_whitespace());
    (!next.starts_with(['+', '~'])).then_some(len)
}

/// Writes a selector with its id selectors mapped.
fn write_selector(s: &str, ids: &Ids, out: &mut Out) -> Result<(), Fault> {
    let b = s.as_bytes();
    let mut last = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => i = string_end(b, i).0,
            b'\\' => i = escape_end(s, i),
            b'#' => {
                let end = ident_end(b, i + 1);
                let name = &s[i + 1..end];
                if name.is_empty() {
                    return Err(BAD_IDS);
                }
                out.put(Esc::Text, &s[last..i + 1]);
                out.put(Esc::Text, ids.get(name)?);
                last = end;
                i = end;
            }
            _ => i += 1,
        }
    }
    out.put(Esc::Text, &s[last..]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_diagram::{Diagram, encode_label, render};
    use regex::Regex;
    use std::fs;
    use std::path::Path;

    const H: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const NS: &str = r#"xmlns="http://www.w3.org/2000/svg""#;
    /// A label with all 13 escaped characters, an ampersand, a backslash, an apostrophe and
    /// Unicode, as the reader should see it.
    const SHOWN: &str = "\"q\" #h ;s <l> [b] (p) {c} |v `t` & back\\slash it's café ✓";
    const CODES: [(&str, &str); 13] = [
        ("#34;", "\""),
        ("#35;", "#"),
        ("#59;", ";"),
        ("#60;", "<"),
        ("#62;", ">"),
        ("#91;", "["),
        ("#93;", "]"),
        ("#40;", "("),
        ("#41;", ")"),
        ("#123;", "{"),
        ("#125;", "}"),
        ("#124;", "|"),
        ("#96;", "`"),
    ];

    fn fixtures(kind: &str) -> Vec<(String, String)> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../rdy-diagram/tests/fixtures")
            .join(kind);
        let mut all: Vec<(String, String)> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "svg"))
            .map(|p| {
                let name = p.file_stem().unwrap().to_string_lossy().into_owned();
                (name, fs::read_to_string(&p).unwrap())
            })
            .collect();
        all.sort();
        assert!(!all.is_empty(), "no SVG fixtures in {}", dir.display());
        all
    }

    fn drawn(src: &str) -> String {
        match render(src) {
            Diagram::Svg(svg) => svg,
            other => panic!("{src:?} did not draw: {other:?}"),
        }
    }

    /// The shown text of each `<text>` element, in document order, its tspans joined.
    fn shown(svg: &str) -> Vec<String> {
        let mut lines = Vec::new();
        let mut rest = svg;
        while let Some(k) = rest.find("<text") {
            let at = &rest[k..];
            let open = at.find('>').unwrap();
            if at[..open].ends_with('/') {
                lines.push(String::new());
                rest = &at[open + 1..];
                continue;
            }
            let close = at.find("</text>").unwrap();
            lines.push(xml_text(&at[open + 1..close]));
            rest = &at[close + 7..];
        }
        lines
    }

    fn xml_text(inner: &str) -> String {
        let mut plain = String::new();
        let mut in_tag = false;
        for c in inner.chars() {
            match c {
                '<' => in_tag = true,
                '>' if in_tag => in_tag = false,
                c if !in_tag => plain.push(c),
                _ => {}
            }
        }
        plain
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&apos;", "'")
            .replace("&#13;", "\r")
            .replace("&amp;", "&")
    }

    fn squeeze(s: &str) -> String {
        s.chars().filter(|c| !c.is_whitespace()).collect()
    }

    fn doc(body: &str) -> String {
        format!("<svg {NS}>{body}</svg>")
    }

    fn texts(lines: &[&str]) -> String {
        let body: String = lines
            .iter()
            .enumerate()
            .map(|(k, l)| format!("<text x=\"1\" y=\"{k}\">{l}</text>\n"))
            .collect();
        doc(&format!("<g>{body}</g>"))
    }

    fn ids_in(svg: &str) -> Vec<String> {
        let re = Regex::new(r#"\sid="([^"]*)""#).unwrap();
        re.captures_iter(svg).map(|c| c[1].to_string()).collect()
    }

    fn refs_in(svg: &str) -> Vec<String> {
        let url = Regex::new(r#"url\(\s*(?:'|"|&quot;|&#39;)?#([^'")&\s]+)"#).unwrap();
        let href = Regex::new(r##"href="#([^"]*)""##).unwrap();
        let mut refs: Vec<String> = url.captures_iter(svg).map(|c| c[1].to_string()).collect();
        refs.extend(href.captures_iter(svg).map(|c| c[1].to_string()));
        refs
    }

    fn style_text(svg: &str) -> String {
        let mut all = String::new();
        let mut rest = svg;
        while let Some(k) = rest.find("<style>") {
            let at = &rest[k + 7..];
            let close = at.find("</style>").unwrap();
            all.push_str(&xml_text(&at[..close]));
            rest = &at[close..];
        }
        all
    }

    /// Every selector of the stylesheets, trimmed; at-rule preludes are left out.
    fn selectors(svg: &str) -> Vec<String> {
        let css = style_text(svg);
        let mut found = Vec::new();
        let mut from = 0;
        let mut depth_in_rule = false;
        for (k, c) in css.char_indices() {
            match c {
                '{' => {
                    let prelude = css[from..k].trim();
                    if !prelude.starts_with('@') && !depth_in_rule {
                        found.extend(prelude.split(',').map(|s| s.trim().to_string()));
                        depth_in_rule = true;
                    }
                    from = k + 1;
                }
                '}' => {
                    depth_in_rule = false;
                    from = k + 1;
                }
                _ => {}
            }
        }
        found
    }

    fn geometry(svg: &str) -> Vec<(String, String)> {
        let re = Regex::new(
            r#"\s(x|y|x1|y1|x2|y2|cx|cy|r|rx|ry|width|height|d|points|viewBox|transform|refX|refY|markerWidth|markerHeight)="([^"]*)""#,
        )
        .unwrap();
        re.captures_iter(svg)
            .map(|c| (c[1].to_string(), c[2].to_string()))
            .collect()
    }

    fn site_tokens(block_start: &str) -> Vec<(String, String)> {
        let css = include_str!("../theme/rdy-standard/site.css");
        let at = css.find(block_start).unwrap() + block_start.len();
        let block = &css[at..at + css[at..].find('}').unwrap()];
        let re = Regex::new(r"(--[a-z0-9-]+):\s*([^;]+);").unwrap();
        re.captures_iter(block)
            .map(|c| (c[1].to_string(), c[2].trim().to_string()))
            .collect()
    }

    fn token(tokens: &[(String, String)], name: &str) -> String {
        tokens
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("site.css declares no {name}"))
            .1
            .clone()
    }

    #[test]
    fn dgm13_labels_keep_their_shown_text() {
        let e = |ctx: &str| encode_label(&format!("{ctx} {SHOWN}"));
        let families: [(&str, String, &[&str]); 4] = [
            (
                "flowchart",
                format!(
                    "flowchart LR\n    subgraph G[{}]\n        A[{}] -->|{}| B[&lt;command&gt;]\n    end\n",
                    e("group"),
                    e("node"),
                    e("edge")
                ),
                &["group", "node", "edge"],
            ),
            (
                "class",
                format!(
                    "classDiagram\n    class A[\"{}\"]\n    class B {{\n        +{}\n    }}\n    A --> B : {}\n",
                    e("node"),
                    e("member"),
                    e("edge")
                ),
                &["node", "member", "edge"],
            ),
            (
                "sequence",
                format!(
                    "sequenceDiagram\n    box {}\n    participant A as {}\n    end\n    participant B as other\n    loop {}\n    A->>B: {}\n    end\n    Note over A: {}\n",
                    e("group"),
                    e("node"),
                    e("block"),
                    e("edge"),
                    e("note")
                ),
                &["group", "node", "block", "edge", "note"],
            ),
            (
                "state",
                format!(
                    "stateDiagram-v2\n    state \"{}\" as S\n    S --> T : {}\n    note right of T : {}\n",
                    e("node"),
                    e("edge"),
                    e("note")
                ),
                &["node", "edge", "note"],
            ),
        ];
        for (family, src, contexts) in &families {
            let raw = drawn(src);
            assert!(
                raw.contains("#34;"),
                "{family}: the pinned crate kept its codes"
            );
            let out = standalone(&raw).unwrap_or_else(|r| panic!("{family}: {r}"));
            let lines = shown(&out);
            let all = squeeze(&lines.concat());
            for ctx in *contexts {
                let want = squeeze(&format!("{ctx} {SHOWN}"));
                assert!(all.contains(&want), "{family} {ctx}: {lines:?}");
            }
            for (code, _) in CODES {
                assert!(!all.contains(code), "{family}: {code} left in {lines:?}");
            }
            // the output is escaped SVG that reads back
            assert!(out.contains("&lt;l&gt;"), "{family}");
            assert!(!out.contains("<l>"), "{family}");
            assert!(standalone(&out).is_ok(), "{family}");
            let inline_out = inline(&out, H, 0).unwrap();
            assert_eq!(shown(&inline_out), lines, "{family}");
        }
        let flow = standalone(&drawn(&families[0].1)).unwrap();
        assert!(shown(&flow).iter().any(|l| l == "<command>"));
        assert!(flow.contains("&lt;command&gt;"));
        assert!(!flow.contains("<command"));
    }

    #[test]
    fn dgm13_split_codes_decode_across_text_lines() {
        // the pinned crate wraps a long word in the middle of a code
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rdy-diagram/tests/fixtures");
        let raw = drawn(&fs::read_to_string(dir.join("diagram/labels.mmd")).unwrap());
        assert!(raw.contains(">thirdthirdthirdthirdthirdthir#5</text>"));
        assert!(raw.contains(">9;secondsecondsecondsec</text>"));
        let lines = shown(&standalone(&raw).unwrap());
        let k = lines
            .iter()
            .position(|l| l.starts_with("thirdthird"))
            .unwrap();
        assert_eq!(
            lines[k..k + 3],
            [
                "thirdthirdthirdthirdthirdthir;",
                "secondsecondsecondsec",
                "ond"
            ]
        );
        for line in &lines {
            assert!(!line.contains("#5") && !line.starts_with("9;"), "{line:?}");
        }

        // every split point of every code
        let mut codes: Vec<(String, String)> = CODES
            .iter()
            .map(|(c, s)| (c.to_string(), s.to_string()))
            .collect();
        for (c, s) in [
            ("&amp;lt;", "<"),
            ("&amp;gt;", ">"),
            ("&amp;amp;", "&"),
            ("&amp;quot;", "\""),
            ("&amp;#233;", "é"),
            ("&amp;#xe9;", "é"),
        ] {
            codes.push((c.to_string(), s.to_string()));
        }
        for (code, ch) in &codes {
            // split the authored characters, not the XML escape of the ampersand
            let plain = code.replacen("&amp;", "&", 1);
            for p in 1..plain.len() {
                let (a, b) = plain.split_at(p);
                let svg = texts(&[
                    &format!("x{}", a.replace('&', "&amp;")),
                    &format!("{}y", b.replace('&', "&amp;")),
                ]);
                let out = standalone(&svg).unwrap();
                assert_eq!(
                    shown(&out),
                    [format!("x{ch}"), "y".to_string()],
                    "{plain} split at {p}"
                );
            }
        }

        // a code over three lines
        let out = standalone(&texts(&["x&amp;#x", "1F6", "00;y"])).unwrap();
        assert_eq!(shown(&out), ["x\u{1F600}", "", "y"]);
        let out = standalone(&texts(&["x#1", "2", "5;y"])).unwrap();
        assert_eq!(shown(&out), ["x}", "", "y"]);

        // tspans stay in their text's run
        let svg = doc(r#"<text x="1" y="1"><tspan>x#5</tspan><tspan>9;y</tspan></text>"#);
        let out = standalone(&svg).unwrap();
        assert!(out.contains("<tspan>x;</tspan><tspan>y</tspan>"), "{out}");

        // separate labels are never joined
        for apart in [
            r#"<text x="1" y="1">x#5</text><text x="2" y="2">9;y</text>"#,
            r#"<g><text x="1" y="1">x#5</text></g><g><text x="1" y="2">9;y</text></g>"#,
            r#"<text x="1" y="1">x#5</text><rect x="0" y="0" width="1" height="1" /><text x="1" y="2">9;y</text>"#,
            r#"<text x="1" y="1">x#5</text><!-- x --><text x="1" y="2">9;y</text>"#,
            r#"<text x="1" y="1">x#5</text>z<text x="1" y="2">9;y</text>"#,
            r#"<text x="1" y="1" fill="red">x#5</text><text x="1" y="2">9;y</text>"#,
        ] {
            let out = standalone(&doc(apart)).unwrap();
            assert_eq!(shown(&out), ["x#5", "9;y"], "{apart}");
        }
    }

    #[test]
    fn dgm13_decoding_is_once_and_text_only() {
        let out = standalone(&texts(&[
            "#35;59; &amp;amp;lt; &amp;#35;59; #60;b#62; &lt;script&gt;",
        ]))
        .unwrap();
        assert_eq!(shown(&out), ["#59; &lt; #59; <b> <script>"]);
        assert!(!out.contains("<b>") && !out.contains("<script"), "{out}");
        assert!(out.contains("&lt;b&gt; &lt;script&gt;"), "{out}");

        let literal = "&#0; &#xD800; &#x110000; &#1; &#xFFFE; &#; &#x; &#12345678; &#X41; #38; &nbsp; &apos; #034; #34";
        let out = standalone(&texts(&[&literal.replace('&', "&amp;")])).unwrap();
        assert_eq!(shown(&out), [literal]);

        let out = standalone(&texts(&[
            "&amp;#233;&amp;#xe9;&amp;#xE9;&amp;#65;&amp;#0065;",
        ]))
        .unwrap();
        assert_eq!(shown(&out), ["éééAA"]);

        // styles and attributes are not label text
        let svg = doc(concat!(
            r##"<style>.l { font-family: "#34;"; }</style>"##,
            r##"<text x="1" y="1" class="l" data-x="#34;&amp;lt;">#34;</text>"##,
        ));
        let out = standalone(&svg).unwrap();
        assert!(out.contains(r##"data-x="#34;&amp;lt;""##), "{out}");
        assert!(out.contains("font-family: &quot;#34;&quot;;"), "{out}");
        assert_eq!(shown(&out), ["\""]);

        // inline reads the asset's text as it is
        let asset = standalone(&texts(&["&amp;amp;#35;59; &amp;#35;59; &amp;amp;lt;"])).unwrap();
        assert_eq!(shown(&asset), ["&#59; #59; &lt;"]);
        assert_eq!(shown(&inline(&asset, H, 0).unwrap()), ["&#59; #59; &lt;"]);
    }

    #[test]
    fn dgm13_ids_and_css_are_scoped() {
        let svg = concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" style="--bg:#fff; background-color: #fff">"#,
            r#"<style>svg { --_line: var(--line); } #m path { fill: url(#grad); } .edge { stroke: var(--_line); } svg > g { opacity: 1; } svg ~ g { opacity: 0; } :root .x { fill: red; } g { fill: none; } @media (min-width: 1px) { #c rect, svg .x { fill: red; } }</style>"#,
            r#"<defs><marker id="m"><path d="M0 0" /></marker><clipPath id="c"><rect x="0" y="0" width="1" height="1" /></clipPath>"#,
            r#"<linearGradient id="grad"><stop offset="0" /></linearGradient><symbol id="s"><rect x="1" y="1" width="1" height="1" /></symbol></defs>"#,
            r##"<g clip-path="url(#c)"><path class="edge" d="M0 0" marker-end="url(#m)" style="fill:url('#grad')" /><use href="#s" /><use xlink:href="#s" /></g></svg>"##,
        );
        let asset = standalone(svg).unwrap();
        assert_eq!(
            ids_in(&asset),
            ["dgm-0", "dgm-1", "dgm-2", "dgm-3", "dgm-4"]
        );
        let refs = refs_in(&asset);
        assert_eq!(
            refs,
            ["dgm-3", "dgm-2", "dgm-1", "dgm-3", "dgm-4", "dgm-4"],
            "{asset}"
        );
        for sel in selectors(&asset) {
            assert!(sel.starts_with("#dgm-0"), "{sel:?} in {asset}");
        }
        let sels = selectors(&asset);
        assert!(sels.contains(&"#dgm-0".to_string()), "{sels:?}");
        assert!(sels.contains(&"#dgm-0 > g".to_string()), "{sels:?}");
        assert!(sels.contains(&"#dgm-0 svg ~ g".to_string()), "{sels:?}");
        assert!(sels.contains(&"#dgm-0 .x".to_string()), "{sels:?}");
        assert!(sels.contains(&"#dgm-0 g".to_string()), "{sels:?}");
        assert!(sels.contains(&"#dgm-0 #dgm-1 path".to_string()), "{sels:?}");
        assert!(sels.contains(&"#dgm-0 #dgm-2 rect".to_string()), "{sels:?}");
        assert!(style_text(&asset).contains("@media (min-width: 1px)"));

        let first = inline(&asset, H, 0).unwrap();
        let second = inline(&asset, H, 1).unwrap();
        let (a, b) = (ids_in(&first), ids_in(&second));
        assert_eq!(a.len(), 5);
        for id in &a {
            assert!(id.starts_with(&format!("dgm-{H}-0-")), "{id}");
            assert!(!b.contains(id), "{id} is in both");
        }
        for (out, ids, occ) in [(&first, &a, 0), (&second, &b, 1)] {
            let root = format!("#dgm-{H}-{occ}-dgm-0");
            for r in refs_in(out) {
                assert!(ids.contains(&r), "{r} does not resolve in {out}");
            }
            for sel in selectors(out) {
                assert!(sel.starts_with(&root), "{sel:?}");
                assert!(
                    sel == root || sel[root.len()..].starts_with([' ', '.', ':', '#', '[']),
                    "{sel:?}"
                );
            }
        }

        // the generator's own marker references resolve in every fixture
        for (name, raw) in fixtures("diagram").into_iter().chain(fixtures("hostile")) {
            let Ok(asset) = standalone(&raw) else {
                continue;
            };
            let ids = ids_in(&asset);
            for r in refs_in(&asset) {
                assert!(ids.contains(&r), "{name}: {r}");
            }
            let out = inline(&asset, H, 3).unwrap();
            let ids = ids_in(&out);
            for r in refs_in(&out) {
                assert!(ids.contains(&r), "{name}: inline {r}");
            }
            for sel in selectors(&out) {
                assert!(
                    sel.starts_with(&format!("#dgm-{H}-3-dgm-0")),
                    "{name}: {sel}"
                );
            }
        }
    }

    #[test]
    fn dgm13_standalone_palette_matches_site_tokens() {
        let table = [
            ("bg", "--bg", "#fbfbfc", "#121419"),
            ("fg", "--ink", "#1b1e26", "#e5e7ec"),
            ("line", "--line-strong", "#c9cdd6", "#3a404c"),
            ("accent", "--accent", "#2456c9", "#8fb0ff"),
            ("muted", "--ink-soft", "#5d6472", "#a1a7b4"),
            ("surface", "--surface", "#f2f3f6", "#1a1d24"),
            ("border", "--line-ui", "#7f8695", "#737a89"),
            ("cluster-bkg", "--surface-2", "#e9ebf0", "#22262f"),
            ("note-fill", "--note", "#eef3ff", "#182136"),
        ];
        let light = site_tokens(":root {");
        let dark = site_tokens(":root:not([data-theme=\"light\"]) {");
        let forced = site_tokens(":root[data-theme=\"dark\"] {");
        for (slot, tok, l, d) in table {
            assert_eq!(token(&light, tok), l, "{slot}");
            assert_eq!(token(&dark, tok), d, "{slot}");
            assert_eq!(token(&forced, tok), d, "{slot}");
        }
        assert_eq!(PALETTE, table);

        let raw = &fixtures("diagram")
            .into_iter()
            .find(|(n, _)| n == "labels")
            .unwrap()
            .1;
        let asset = standalone(raw).unwrap();
        let css = style_text(&asset);
        let light_rule = css.find("#dgm-0 { --dgm-bg: #fbfbfc;").unwrap();
        let media = css.find("@media (prefers-color-scheme: dark) {").unwrap();
        let dark_rule = css.find("#dgm-0 { --dgm-bg: #121419;").unwrap();
        assert!(light_rule < media && media < dark_rule, "{css}");
        for (slot, _, l, d) in table {
            assert_eq!(
                css.matches(&format!("--dgm-{slot}: {l};")).count(),
                1,
                "{slot}"
            );
            assert_eq!(
                css.matches(&format!("--dgm-{slot}: {d};")).count(),
                1,
                "{slot}"
            );
            assert!(
                !css.contains(&format!("--dgm-{slot}: var(--dgm-{slot})")),
                "{slot}"
            );
        }
        // derived expressions read the renamed slots
        assert!(
            css.contains("--dgm-_text-sec: color-mix(in srgb, var(--dgm-fg) 60%, var(--dgm-bg));"),
            "{css}"
        );
        assert!(
            !css.contains("var(--fg)") && !css.contains("var(--bg)"),
            "{css}"
        );
        let root = Regex::new(r#"^<svg [^>]*style="([^"]*)""#).unwrap();
        let root_style = root.captures(&asset).unwrap()[1].to_string();
        assert_eq!(
            root_style,
            "max-width: 2151.32px; background-color: var(--dgm-bg);"
        );
        assert!(asset.contains("style=\"fill:var(--dgm-bg);\""));

        let page = inline(&asset, H, 0).unwrap();
        let css = style_text(&page);
        let root_id = format!("#dgm-{H}-0-dgm-0");
        assert!(
            css.contains(&format!("{root_id} {{ --dgm-bg: var(--bg);")),
            "{css}"
        );
        for (slot, tok, l, d) in table {
            assert_eq!(
                css.matches(&format!("--dgm-{slot}: var({tok});")).count(),
                1,
                "{slot}"
            );
            assert!(!css.contains(l) && !css.contains(d), "{slot}: {css}");
        }
        assert!(!css.contains("prefers-color-scheme"), "{css}");
        assert!(css.contains("var(--dgm-fg) 60%"), "{css}");
        assert!(page.contains("background-color: var(--dgm-bg);"));
        assert!(!page.contains("<?xml"));

        // with no stylesheet the asset gets one; an XML declaration is kept, then dropped inline
        let bare = standalone(&format!("<?xml version=\"1.0\"?>\n<svg {NS}/>")).unwrap();
        assert!(
            bare.starts_with("<?xml version=\"1.0\"?>\n<svg id=\"dgm-0\""),
            "{bare}"
        );
        assert!(bare.contains("<style>"), "{bare}");
        assert!(bare.contains("background-color: var(--dgm-bg);"), "{bare}");
        let bare_inline = inline(&bare, H, 0).unwrap();
        assert!(bare_inline.starts_with("<svg id="), "{bare_inline}");
        assert!(
            bare_inline.contains("--dgm-bg: var(--bg);"),
            "{bare_inline}"
        );
    }

    #[test]
    fn dgm13_drawing_faults_have_exact_reasons() {
        let check = |svg: &str, want: Result<(), &str>| {
            let got = standalone(svg).map(|_| ());
            assert_eq!(got, want, "standalone {svg:.200}");
            assert_eq!(inline(svg, H, 0).map(|_| ()), want, "inline {svg:.200}");
        };

        // size, at the boundary: an outside comment fills the input and is not written
        let pad = |n: usize| {
            let mut s = doc("<g />");
            let fill = n - s.len() - 7;
            s.push_str("<!--");
            s.push_str(&"x".repeat(fill));
            s.push_str("-->");
            assert_eq!(s.len(), n);
            s
        };
        check(&pad(MAX_BYTES), Ok(()));
        check(&pad(MAX_BYTES + 1), Err(OVER));
        // an output over the bound
        let near = texts(&[&"x".repeat(MAX_BYTES - 200)]);
        assert_eq!(standalone(&near), Err(OVER));
        let many: String = (0..60_000).map(|k| format!("<g id=\"a{k}\" />")).collect();
        let many = standalone(&doc(&many)).unwrap();
        assert_eq!(inline(&many, H, 0), Err(OVER));

        // XML structure
        for bad in [
            String::new(),
            doc("<g></h>"),
            format!("<svg {NS}><g>"),
            format!("<svg {NS} x=\"1\" x=\"2\" />"),
            format!("<!DOCTYPE svg><svg {NS} />"),
            doc("<?pi x?>"),
            format!(" <?xml version=\"1.0\"?><svg {NS} />"),
            doc("<text x=\"1\" y=\"1\">&nbsp;</text>"),
            format!("<svg {NS} /><svg {NS} />"),
            format!("<svg {NS} x=1 />"),
            doc("<text x=\"1\" y=\"1\">a < b</text>"),
            doc("<text x=\"1\" y=\"1\">a & b</text>"),
            doc("<text x=\"1\" y=\"1\">&#0;</text>"),
            doc("<text x=\"1\" y=\"1\">]]></text>"),
            "<svg />".to_string(),
            "<svg xmlns=\"http://www.w3.org/1999/xhtml\" />".to_string(),
            format!("<g {NS} />"),
            doc("<x:g />"),
            doc("<g xmlns=\"http://www.w3.org/1999/xhtml\" />"),
            doc("<style>svg { fill: red</style>"),
            doc("<style>@charset \"x\";</style>"),
            doc("<style><g /></style>"),
            doc("<g fill=\"<script>x</script>\" />"),
            "x".to_string(),
        ] {
            check(&bad, Err(NOT_SVG));
        }
        let decl = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg {NS} />");
        check(&decl, Ok(()));

        // active content
        for bad in [
            doc("<script>alert(1)</script>"),
            doc("<foreignObject><div>x</div></foreignObject>"),
            doc("<a><text x=\"1\" y=\"1\">x</text></a>"),
            doc("<image href=\"#a\" />"),
            doc("<animate attributeName=\"x\" />"),
            doc("<set attributeName=\"x\" to=\"1\" />"),
            doc("<g onload=\"alert(1)\" />"),
            doc("<rect ONCLICK=\"x\" />"),
            format!("<svg {NS} onmouseover=\"x\" />"),
        ] {
            check(&bad, Err(ACTIVE));
        }

        // external resources; namespace declarations are not references
        for bad in [
            doc("<use href=\"https://example.com/x.svg#a\" />"),
            doc("<use href=\"\" />"),
            doc("<use href=\"#\" />"),
            doc("<use href=\"x.svg#a\" />"),
            doc("<use href=\"data:image/svg+xml,x\" />"),
            format!(
                "<svg {NS} xmlns:xlink=\"http://www.w3.org/1999/xlink\"><use xlink:href=\"https://example.com/a\" /></svg>"
            ),
            format!(
                "<svg {NS} xmlns:xlink=\"urn:x\"><use xlink:href=\"https://example.com/a\" /></svg>"
            ),
            doc("<rect style=\"fill:url(https://example.com/x)\" />"),
            doc("<rect fill=\"url('data:x')\" />"),
            doc("<rect fill=\"url(x.svg#a)\" />"),
            doc("<rect fill=\"url()\" />"),
            doc("<rect fill=\"image-set('x.png' 1x)\" />"),
            doc("<g xml:base=\"https://example.com/\" />"),
            doc("<style>@import url(x.css);</style>"),
            doc("<style>@import \"x.css\";</style>"),
            doc("<style>@font-face { font-family: x; src: local(x); }</style>"),
            doc("<style>svg { fill: u\\72l(https://example.com/x); }</style>"),
            doc("<rect style=\"fill:u\\72l(https://example.com/x)\" />"),
        ] {
            check(&bad, Err(EXTERNAL));
        }
        check(
            &format!(
                "<svg {NS} xmlns:xlink=\"http://www.w3.org/1999/xlink\" xmlns:ex=\"https://example.com/ns\"><use xlink:href=\"#a\" /><g id=\"a\" /></svg>"
            ),
            Ok(()),
        );

        // element ids
        for bad in [
            doc("<g id=\"a\" /><g id=\"a\" />"),
            doc("<rect fill=\"url(#nope)\" />"),
            doc("<use href=\"#nope\" />"),
            doc("<style>#nope g { fill: red; }</style>"),
            doc("<g id=\"1a\" />"),
            doc("<g id=\"\" />"),
            doc("<g id=\"a b\" />"),
            doc("<g id=\"é\" />"),
            doc("<style>#\\61 { fill: red; }</style><g id=\"a\" />"),
            doc("<style>svg { fill: u\\72l(#a); }</style><g id=\"a\" />"),
        ] {
            check(&bad, Err(BAD_IDS));
        }
        let fine = standalone(&doc("<g />")).unwrap();
        assert_eq!(inline(&fine, "ABC", 0), Err(BAD_IDS));
        assert_eq!(inline(&fine, &H.to_uppercase(), 0), Err(BAD_IDS));

        // precedence: size, structure, active, resources, ids
        let all = "<script /><use href=\"https://example.com/a\" /><g id=\"a\" /><g id=\"a\" />";
        check(&doc(all), Err(ACTIVE));
        check(
            &doc("<use href=\"https://example.com/a\" /><g id=\"a\" /><g id=\"a\" />"),
            Err(EXTERNAL),
        );
        check(&format!("<svg {NS}><script></svg>"), Err(NOT_SVG));
        check(&format!("<svg {NS}>{}", "<g>".repeat(1_400_000)), Err(OVER));

        // the pinned crate's raw output for hostile sources
        let expect = [
            ("class-hostile", Ok(())),
            ("classdef-event", Err(ACTIVE)),
            ("classdef-url", Err(EXTERNAL)),
            ("click-call", Ok(())),
            ("click-url", Ok(())),
            ("label-attribute-quote", Ok(())),
            ("label-cdata", Ok(())),
            ("label-entities", Ok(())),
            ("label-foreign-object", Ok(())),
            ("label-import", Ok(())),
            ("label-script", Ok(())),
            ("linkstyle-event", Err(ACTIVE)),
            ("linkstyle-url", Err(EXTERNAL)),
            ("sequence-hostile", Ok(())),
            ("state-classdef-event", Err(ACTIVE)),
            ("state-hostile", Ok(())),
            ("style-event", Err(ACTIVE)),
            ("style-script", Err(NOT_SVG)),
            ("style-url", Err(EXTERNAL)),
        ];
        let hostile = fixtures("hostile");
        assert_eq!(
            hostile.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            expect.iter().map(|(n, _)| *n).collect::<Vec<_>>()
        );
        let event = Regex::new(r"<[^>]*\son[a-z]+=").unwrap();
        let fetch = Regex::new(r#"<[^>]*url\(\s*['"]?http"#).unwrap();
        for ((name, raw), (_, want)) in hostile.iter().zip(expect) {
            assert_eq!(standalone(raw).map(|_| ()), want, "{name}");
            assert_eq!(inline(raw, H, 0).map(|_| ()), want, "{name} inline");
            if let Ok(asset) = standalone(raw) {
                for out in [asset.clone(), inline(&asset, H, 0).unwrap()] {
                    for tag in ["<script", "<foreignObject", "<a ", "<img", "<div"] {
                        assert!(!out.contains(tag), "{name}: {tag}");
                    }
                    assert!(!event.is_match(&out), "{name}");
                    assert!(!fetch.is_match(&out), "{name}");
                    assert!(!style_text(&out).contains("url(http"), "{name}");
                }
            }
        }
        let style_event = &hostile.iter().find(|(n, _)| n == "style-event").unwrap().1;
        assert!(style_event.contains(r##"fill="#f00" onload="alert(1)""##));
        let style_url = &hostile.iter().find(|(n, _)| n == "style-url").unwrap().1;
        assert!(style_url.contains(r#"fill="url(https://example.com/x.svg)""#));
    }

    #[test]
    fn dgm13_conversion_preserves_geometry_and_bytes() {
        let mut converted = 0;
        for (name, raw) in fixtures("diagram").into_iter().chain(fixtures("hostile")) {
            let Ok(asset) = standalone(&raw) else {
                continue;
            };
            converted += 1;
            assert_eq!(standalone(&raw).unwrap(), asset, "{name}");
            let page = inline(&asset, H, 0).unwrap();
            assert_eq!(inline(&asset, H, 0).unwrap(), page, "{name}");
            assert_eq!(geometry(&asset), geometry(&raw), "{name}");
            assert_eq!(geometry(&page), geometry(&raw), "{name}");
            // every text node and position stays
            assert_eq!(shown(&asset).len(), shown(&raw).len(), "{name}");
            assert_eq!(shown(&page), shown(&asset), "{name}");
            assert_eq!(
                asset.matches('<').count(),
                raw.matches('<').count() + 2 * usize::from(!raw.contains("<style")),
                "{name}"
            );
        }
        assert_eq!(converted, 15 + 11);
    }
}
