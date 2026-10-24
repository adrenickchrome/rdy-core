//! Diagrams drawn as SVG: one pinned crate behind one module (D-FY4VF, D-KAGB2).
//!
//! [`render`] takes a diagram's resolved plain source and returns its SVG, or why it draws none.
//! The drawn families are the pinned crate's: flowchart (and its `graph` alias), class, sequence
//! and state. A recognized header of another family is source-only, and its body is never
//! read. Bounds are checked before the source reaches the pinned crate: bytes first, then a
//! linear count of statements and nesting depth. It is not a second Mermaid parser.
//!
//! The pinned crate reads six environment variables at run time. The binary removes them at
//! startup (`rdy_daemon::sys`); a library host must start without them (D-E8P8J).

use mmdflux::{OutputFormat, RenderConfig, SvgThemeConfig, SvgThemeMode};

/// The most bytes a source may have.
pub const MAX_SOURCE_BYTES: usize = 65536;
/// The most statements a drawn or unknown source may have.
pub const MAX_STATEMENTS: usize = 2048;
/// The deepest a drawn source may nest subgraphs or blocks.
pub const MAX_DEPTH: usize = 32;
/// The most bytes a drawn SVG may have.
pub const MAX_SVG_BYTES: usize = 4194304;

const OVER_BYTES: &str = "the source is over 65536 bytes";
const OVER_STATEMENTS: &str = "the source has over 2048 statements";
const OVER_DEPTH: &str = "the source nests subgraphs or blocks over 32 deep";
const OVER_SVG: &str = "the drawn diagram is over 4194304 bytes";
/// The pinned crate's text when it detects no diagram type.
const UNKNOWN: &str = "unknown diagram type";
/// The prefix the pinned crate puts before a parse failure.
const PARSE_ERROR: &str = "parse error: ";

/// What a diagram's source draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Diagram {
    /// The drawn SVG, exactly as the pinned crate wrote it.
    Svg(String),
    /// A recognized header of a family rdy does not draw, in its source spelling.
    Unsupported { header: String },
    /// The source is not a diagram rdy can draw: a bound or the pinned crate's message.
    Invalid { reason: String },
    /// The pinned crate parsed the source but its drawing cannot be used.
    Failed { reason: String },
}

/// The headers of families rdy recognizes but does not draw, matched with their exact case.
const UNDRAWN_EXACT: [&str; 38] = [
    "C4Context",
    "C4Container",
    "C4Component",
    "C4Dynamic",
    "C4Deployment",
    "erDiagram",
    "gantt",
    "gitGraph",
    "journey",
    "pie",
    "info",
    "quadrantChart",
    "requirement",
    "requirementDiagram",
    "mindmap",
    "timeline",
    "zenuml",
    "sankey",
    "sankey-beta",
    "xychart",
    "xychart-beta",
    "block",
    "block-beta",
    "packet",
    "packet-beta",
    "kanban",
    "architecture",
    "architecture-beta",
    "radar-beta",
    "treemap",
    "treemap-beta",
    "swimlane-beta",
    "treeView-beta",
    "eventmodeling",
    "venn-beta",
    "cynefin-beta",
    "classDiagram-v2",
    "flowchart-elk",
];

/// The headers of families rdy recognizes but does not draw, matched in any case.
const UNDRAWN_ANY_CASE: [&str; 7] = [
    "ishikawa",
    "ishikawa-beta",
    "wardley-beta",
    "railroad-beta",
    "railroad-ebnf-beta",
    "railroad-abnf-beta",
    "railroad-peg-beta",
];

/// How a drawn family nests: flowchart subgraphs, class and state braces, or not at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Nesting {
    Subgraph,
    Braces,
    None,
}

/// Draws `source`, a diagram's resolved plain source, as SVG.
pub fn render(source: &str) -> Diagram {
    if source.len() > MAX_SOURCE_BYTES {
        return invalid(OVER_BYTES);
    }
    let src = source.strip_prefix('\u{feff}').unwrap_or(source);
    // MMDS JSON goes to the pinned crate's other frontend: rdy never sends it there
    if matches!(src.trim_start().as_bytes().first(), Some(b'{' | b'[')) {
        return invalid(UNKNOWN);
    }
    let family = mmdflux::detect_diagram(src);
    if family.is_none()
        && let Some(header) = undrawn_header(src)
    {
        return Diagram::Unsupported {
            header: header.to_string(),
        };
    }
    let nesting = match family {
        Some("flowchart") => Nesting::Subgraph,
        Some("class" | "state") => Nesting::Braces,
        _ => Nesting::None,
    };
    let (statements, depth) = measure(src, nesting);
    if statements > MAX_STATEMENTS {
        return invalid(OVER_STATEMENTS);
    }
    if depth > MAX_DEPTH {
        return invalid(OVER_DEPTH);
    }
    let config = RenderConfig {
        svg_theme: Some(SvgThemeConfig {
            mode: SvgThemeMode::Dynamic,
            ..Default::default()
        }),
        ..Default::default()
    };
    match mmdflux::render_diagram(src, OutputFormat::Svg, &config) {
        Ok(svg) if svg.len() > MAX_SVG_BYTES => Diagram::Failed {
            reason: OVER_SVG.to_string(),
        },
        Ok(svg) => Diagram::Svg(svg),
        Err(e) => match e.message.strip_prefix(PARSE_ERROR) {
            Some(reason) => invalid(reason),
            // an unknown or absent header: the pinned crate's detection refused it
            None if family.is_none() => Diagram::Invalid { reason: e.message },
            None => Diagram::Failed { reason: e.message },
        },
    }
}

fn invalid(reason: &str) -> Diagram {
    Diagram::Invalid {
        reason: reason.to_string(),
    }
}

/// Escapes a label for Mermaid source. Characters Mermaid reads as syntax become `#<code>;`
/// decimal codes; line breaks become spaces; every other character is kept.
pub fn encode_label(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    for c in label.chars() {
        match c {
            '"' | '#' | ';' | '<' | '>' | '[' | ']' | '(' | ')' | '{' | '}' | '|' | '`' => {
                out.push_str(&format!("#{};", c as u32))
            }
            '\n' | '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// The source after one complete front matter block: a first non-whitespace `---` line through
/// the first later line whose trimmed text is `---`. An unclosed block is not skipped.
fn after_front_matter(src: &str) -> &str {
    let Some(open) = src.trim_start().strip_prefix("---") else {
        return src;
    };
    let Some(newline) = open.find('\n') else {
        return src;
    };
    let mut rest = &open[newline + 1..];
    while !rest.is_empty() {
        let (line, next) = match rest.find('\n') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        if line.trim() == "---" {
            return next;
        }
        rest = next;
    }
    src
}

/// The header of a recognized family rdy does not draw, in its source spelling. Only the
/// header is read: after front matter and blank or `%%` comment lines, the first token of
/// ASCII letters, digits and hyphens.
fn undrawn_header(src: &str) -> Option<&str> {
    let line = after_front_matter(src)
        .lines()
        .map(str::trim_start)
        .find(|l| !l.is_empty() && !l.starts_with("%%"))?;
    let end = line
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .unwrap_or(line.len());
    let token = &line[..end];
    let known = UNDRAWN_EXACT.contains(&token)
        || UNDRAWN_ANY_CASE
            .iter()
            .any(|h| h.eq_ignore_ascii_case(token));
    known.then_some(token)
}

/// The length of the label code at the start of `bytes` (`#<decimal>;`, `&#<decimal>;`,
/// `&#x<hex>;`, `&lt;`, `&gt;`, `&amp;` or `&quot;`), if one is there.
fn code_len(bytes: &[u8]) -> Option<usize> {
    for named in [&b"&lt;"[..], b"&gt;", b"&amp;", b"&quot;"] {
        if bytes.starts_with(named) {
            return Some(named.len());
        }
    }
    let (start, hex) = if bytes.starts_with(b"&#x") {
        (3, true)
    } else if bytes.starts_with(b"&#") {
        (2, false)
    } else if bytes.starts_with(b"#") {
        (1, false)
    } else {
        return None;
    };
    let digits = bytes[start..]
        .iter()
        .take_while(|b| {
            if hex {
                b.is_ascii_hexdigit()
            } else {
                b.is_ascii_digit()
            }
        })
        .count();
    let end = start + digits;
    (digits > 0 && bytes.get(end) == Some(&b';')).then_some(end + 1)
}

/// Counts the statements of `src` after front matter, and its deepest nesting, in one pass.
///
/// A statement is a nonempty line or `;`-separated segment outside double-quoted strings.
/// Blank segments and `%%` comment lines do not count. A backslash escapes the next byte in a
/// string, and a string may span lines. A label code's `;` does not end a statement.
fn measure(src: &str, nesting: Nesting) -> (usize, usize) {
    let bytes = after_front_matter(src).as_bytes();
    let mut statements = 0;
    let mut depth: usize = 0;
    let mut deepest = 0;
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i == bytes.len() {
            break;
        }
        if bytes[i..].starts_with(b"%%") {
            // a comment runs to the end of its line
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;
        let mut quoted = false;
        while i < bytes.len() {
            let b = bytes[i];
            if quoted {
                match b {
                    b'\\' => i += 1,
                    b'"' => quoted = false,
                    _ => {}
                }
                i += 1;
                continue;
            }
            match b {
                b'"' => quoted = true,
                b'\n' | b';' => break,
                b'#' | b'&' => {
                    if let Some(n) = code_len(&bytes[i..]) {
                        i += n;
                        continue;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let text = trim(&bytes[start..i.min(bytes.len())]);
        // past the separator
        i += 1;
        if text.is_empty() {
            continue;
        }
        statements += 1;
        match nesting {
            Nesting::Subgraph => {
                let first = text.split(u8::is_ascii_whitespace).next().unwrap_or(&[]);
                if first.eq_ignore_ascii_case(b"subgraph") {
                    depth += 1;
                } else if text.eq_ignore_ascii_case(b"end") {
                    depth = depth.saturating_sub(1);
                }
            }
            Nesting::Braces => {
                if text.first() == Some(&b'}') {
                    depth = depth.saturating_sub(1);
                }
                // a brace inside a string the source never closes is not structure
                if !quoted && text.last() == Some(&b'{') {
                    depth += 1;
                }
            }
            Nesting::None => {}
        }
        deepest = deepest.max(depth);
    }
    (statements, deepest)
}

fn trim(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |e| e + 1);
    &bytes[start..end]
}
