//! Diagrams drawn as SVG: one pinned crate behind one module (D-FY4VF, D-KAGB2).
//!
//! [`render`] takes a diagram's resolved plain source and returns its SVG, or why it draws none.
//! The drawn families are the pinned crate's: flowchart (and its `graph` alias), class, sequence
//! and state. A recognized header of another family is source-only, and its body is never
//! read. Bounds are checked before the source reaches the pinned crate: bytes first, then a
//! linear count of statements and nesting depth. It is not a second Mermaid parser: it follows
//! the pinned parser only where a comment, a quote or a line end decides what is structure.
//!
//! The pinned crate reads six environment variables at run time. The binary removes them at
//! startup (`rdy_daemon::sys`); a library host must start without them (D-E8P8J).

use mmdflux::{OutputFormat, RenderConfig, SvgThemeConfig, SvgThemeMode};
use std::borrow::Cow;
use std::panic::{AssertUnwindSafe, catch_unwind};

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
/// The reason when the pinned crate panics on a source.
const LIBRARY_FAILED: &str = "the drawing library failed on this source";
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

/// A drawn family, as the pinned crate detects it, or none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    Flowchart,
    Class,
    State,
    Sequence,
    Unknown,
}

/// Draws `source`, a diagram's resolved plain source, as SVG.
pub fn render(source: &str) -> Diagram {
    if source.len() > MAX_SOURCE_BYTES {
        return invalid(OVER_BYTES);
    }
    let src = source.strip_prefix('\u{feff}').unwrap_or(source);
    // The pinned crate counts each front matter line as one byte longer than its text, which
    // is one byte short for a CRLF line: its slice then lands early, or inside a character and
    // panics (mmdflux 2.6.1 flowchart.rs:84). CRLF becomes LF; nothing else changes.
    let src: Cow<str> = if src.contains("\r\n") {
        Cow::Owned(src.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(src)
    };
    let src = src.as_ref();
    // MMDS JSON goes to the pinned crate's other frontend: rdy never sends it there
    if matches!(src.trim_start().as_bytes().first(), Some(b'{' | b'[')) {
        return invalid(UNKNOWN);
    }
    let detected = match guarded(|| mmdflux::detect_diagram(src)) {
        Ok(detected) => detected,
        Err(failed) => return failed,
    };
    if detected.is_none()
        && let Some(header) = undrawn_header(src)
    {
        return Diagram::Unsupported {
            header: header.to_string(),
        };
    }
    let family = match detected {
        Some("flowchart") => Family::Flowchart,
        Some("class") => Family::Class,
        Some("state") => Family::State,
        Some("sequence") => Family::Sequence,
        _ => Family::Unknown,
    };
    let (statements, depth) = measure(src, family);
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
    let drawn = match guarded(|| mmdflux::render_diagram(src, OutputFormat::Svg, &config)) {
        Ok(drawn) => drawn,
        Err(failed) => return failed,
    };
    match drawn {
        Ok(svg) if svg.len() > MAX_SVG_BYTES => Diagram::Failed {
            reason: OVER_SVG.to_string(),
        },
        Ok(svg) => Diagram::Svg(svg),
        Err(e) => match e.message.strip_prefix(PARSE_ERROR) {
            Some(reason) => invalid(reason),
            // an unknown or absent header: the pinned crate's detection refused it
            None if detected.is_none() => Diagram::Invalid { reason: e.message },
            None => Diagram::Failed { reason: e.message },
        },
    }
}

/// Runs one call into the pinned crate. A panic is a drawing fault, never a crash; its message
/// is not shown.
fn guarded<T>(call: impl FnOnce() -> T) -> Result<T, Diagram> {
    catch_unwind(AssertUnwindSafe(|| {
        #[cfg(test)]
        test_hook::maybe_panic();
        call()
    }))
    .map_err(|_| Diagram::Failed {
        reason: LIBRARY_FAILED.to_string(),
    })
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

/// Counts the statements of `src` after front matter, and its deepest nesting.
fn measure(src: &str, family: Family) -> (usize, usize) {
    let body = after_front_matter(src);
    let (statements, segment_depth) = segments(body.as_bytes(), family);
    let depth = match family {
        Family::Flowchart => segment_depth.max(subgraph_bound(body.as_bytes())),
        Family::Class => class_depth(body),
        Family::State => state_depth(body),
        Family::Sequence | Family::Unknown => 0,
    };
    (statements, depth)
}

/// Counts statements in one pass: nonempty lines or `;`-separated segments outside double-quoted
/// strings. Blank and comment-only segments do not count, and a label code's `;` ends no
/// statement. A lone CR ends a statement too, as the pinned flowchart grammar reads it.
///
/// A comment runs to the end of its line, and its text is never read, so a quote in it opens no
/// string. A segment that starts with `%%` is a comment in every family. In flowchart and state
/// diagrams a `%%` later in a segment, outside a string, starts one too, as the pinned parsers
/// read a comment after a statement; class and sequence lines keep it as text.
///
/// A string opens only where the pinned crate can close it, so a quote it reads as text hides
/// nothing. A flowchart string may span lines and closes at the next `"` that does not follow a
/// backslash; if there is none, the quote is text, and so is every later one. The other families
/// read line by line: a string closes on its own line, or the quote is text.
///
/// For a flowchart it also returns the depth by statements: a first token `subgraph` opens, a
/// statement that is `end` closes, both in any case.
fn segments(bytes: &[u8], family: Family) -> (usize, usize) {
    let line_based = matches!(family, Family::Class | Family::State | Family::Sequence);
    let inline_comments = matches!(family, Family::Flowchart | Family::State | Family::Unknown);
    let mut statements = 0;
    let mut depth: usize = 0;
    let mut deepest = 0;
    // a flowchart quote with no close turns strings off; a line-based one, for the rest of its line
    let mut strings = true;
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            if bytes[i] == b'\n' && line_based {
                strings = true;
            }
            i += 1;
        }
        if i == bytes.len() {
            break;
        }
        if bytes[i..].starts_with(b"%%") {
            i = line_end(bytes, i);
            continue;
        }
        let start = i;
        let mut comment = false;
        while i < bytes.len() {
            match bytes[i] {
                b'\n' | b'\r' | b';' => break,
                b'%' if inline_comments && bytes[i..].starts_with(b"%%") => {
                    comment = true;
                    break;
                }
                b'"' if strings => match close_quote(bytes, i + 1, line_based) {
                    Some(close) => {
                        i = close + 1;
                        continue;
                    }
                    None => strings = false,
                },
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
        let text = trim(&bytes[start..i]);
        if comment {
            i = line_end(bytes, i);
        } else {
            if bytes.get(i) == Some(&b'\n') && line_based {
                strings = true;
            }
            i += 1;
        }
        if text.is_empty() {
            continue;
        }
        statements += 1;
        if family == Family::Flowchart {
            let first = text.split(u8::is_ascii_whitespace).next().unwrap_or(&[]);
            if first.eq_ignore_ascii_case(b"subgraph") {
                depth += 1;
            } else if text.eq_ignore_ascii_case(b"end") {
                depth = depth.saturating_sub(1);
            }
            deepest = deepest.max(depth);
        }
    }
    (statements, deepest)
}

/// The index of the `\n` that ends the line holding `i`, or the end of `bytes`.
fn line_end(bytes: &[u8], i: usize) -> usize {
    bytes[i..]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(bytes.len(), |n| i + n)
}

/// The index of the `"` that closes a string whose text starts at `from`, as the pinned
/// flowchart grammar reads one: `\"` is an escaped quote. A line-based string stops at its
/// line end.
fn close_quote(bytes: &[u8], from: usize, line_based: bool) -> Option<usize> {
    let mut j = from;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' if bytes.get(j + 1) == Some(&b'"') => j += 2,
            b'"' => return Some(j),
            b'\n' if line_based => return None,
            _ => j += 1,
        }
    }
    None
}

/// An upper bound on the subgraph depth the pinned flowchart grammar can nest: the most `n` for
/// which `n` subgraph keywords come before some point and `n` end keywords after it.
///
/// The grammar opens each subgraph with the keyword `subgraph` and needs an `end` keyword to
/// close it, both in any case and with no word boundary after. A keyword can follow any byte
/// but an ASCII letter, digit or `_`, which would join it to an identifier. This bound reads
/// no labels, strings or comments, so nothing the grammar nests can hide from it; the statement
/// rule alone cannot know which text a label holds.
fn subgraph_bound(bytes: &[u8]) -> usize {
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

/// The deepest class-diagram nesting, line by line as the pinned class parser reads it. Only a
/// line whose trimmed text ends with `{` can open a namespace or class body, and only a line
/// that is `}` closes one; a line that starts with `%%` is a comment. Counting every line that
/// ends with `{` keeps the count at or above the parser's, whatever the line holds.
fn class_depth(body: &str) -> usize {
    let mut depth: usize = 0;
    let mut deepest = 0;
    for line in body.split('\n') {
        let text = line.trim();
        if text.is_empty() || text.starts_with("%%") {
            continue;
        }
        if text == "}" {
            depth = depth.saturating_sub(1);
        } else if text.ends_with('{') {
            depth += 1;
            deepest = deepest.max(depth);
        }
    }
    deepest
}

/// The deepest state-diagram nesting, line by line as the pinned state parser reads it.
///
/// The parser cuts each trimmed line at its first `%%`, even inside quotes. Then only a line
/// that is `}` closes a block, and only a `state` line that ends with `{` can open one, so a
/// brace that ends a description, a transition label or a note is text. A multi-line note's
/// text runs to its `end note` line and is never structure.
fn state_depth(body: &str) -> usize {
    let mut depth: usize = 0;
    let mut deepest = 0;
    let mut lines = body.split('\n');
    while let Some(line) = lines.next() {
        let text = before_comment(line.trim());
        if text.is_empty() {
            continue;
        }
        if text == "}" {
            depth = depth.saturating_sub(1);
        } else if opens_note_text(text) {
            for line in lines.by_ref() {
                if before_comment(line.trim()).eq_ignore_ascii_case("end note") {
                    break;
                }
            }
        } else if keyword(text, "state").is_some() && text.ends_with('{') {
            depth += 1;
            deepest = deepest.max(depth);
        }
    }
    deepest
}

/// A line's text before its first `%%`, as the pinned state parser cuts it.
fn before_comment(line: &str) -> &str {
    match line.find("%%") {
        Some(at) => line[..at].trim_end(),
        None => line,
    }
}

/// The text after an ASCII keyword that `line` starts with, in any case, when the keyword ends
/// the line or whitespace follows it.
fn keyword<'a>(line: &'a str, word: &str) -> Option<&'a str> {
    let head = line.as_bytes().get(..word.len())?;
    if !head.eq_ignore_ascii_case(word.as_bytes()) {
        return None;
    }
    let rest = &line[word.len()..];
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then_some(rest)
}

/// Whether a state-diagram line starts a multi-line note: `note right of <id>` or `note left of
/// <id>` with no `:`. The pinned parser then reads the next lines as its text.
fn opens_note_text(text: &str) -> bool {
    let Some(rest) = keyword(text, "note") else {
        return false;
    };
    // the parser needs `note` and one space, then trims
    if !text.as_bytes().get(4).is_some_and(|b| *b == b' ') {
        return false;
    }
    let rest = rest.trim();
    let Some(after) = keyword(rest, "right of").or_else(|| keyword(rest, "left of")) else {
        return false;
    };
    let after = after.trim();
    !after.is_empty() && !after.contains(':')
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

/// A test-only hook: makes the `n`th guarded call on this thread panic.
#[cfg(test)]
mod test_hook {
    use std::cell::Cell;

    thread_local! {
        static PANIC_AT: Cell<usize> = const { Cell::new(0) };
        static CALLS: Cell<usize> = const { Cell::new(0) };
    }

    pub(crate) fn panic_at(n: usize) {
        PANIC_AT.set(n);
        CALLS.set(0);
    }

    pub(crate) fn maybe_panic() {
        let call = CALLS.get() + 1;
        CALLS.set(call);
        if PANIC_AT.get() == call {
            panic!("a test panic in the pinned crate");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A panic in the pinned crate, in detection or in drawing, is a drawing fault with a fixed
    /// reason, never a crash.
    #[test]
    fn dgm1_a_library_panic_is_a_drawing_fault() {
        let src = "flowchart LR\n    A --> B\n";
        assert!(matches!(render(src), Diagram::Svg(_)));
        for call in [1, 2] {
            test_hook::panic_at(call);
            assert_eq!(
                render(src),
                Diagram::Failed {
                    reason: LIBRARY_FAILED.to_string()
                },
                "guarded call {call}"
            );
        }
        test_hook::panic_at(0);
        assert!(matches!(render(src), Diagram::Svg(_)));
    }
}
