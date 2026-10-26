//! Diagrams drawn as SVG: one pinned crate behind one module (D-FY4VF, D-KAGB2).
//!
//! [`render`] takes a diagram's resolved plain source and returns its SVG, or why it draws none.
//! The drawn families are the pinned crate's: flowchart (and its `graph` alias), class, sequence
//! and state. A recognized header of another family is source-only, and its body is never
//! read. Bounds are checked before the source reaches the pinned crate: bytes first, then a
//! linear count of statements and nesting depth. The count is not a second Mermaid parser: it
//! follows the pinned parser only where a comment, a quote or a line end decides what is
//! structure. Flowchart depth comes from the pinned grammar's own reading, which reads a
//! subgraph with no `end` again as nodes (see `flowchart`), and class and state depth from the
//! pinned line parsers' rules. Two flowchart shapes inside those bounds would hold the pinned
//! crate for minutes or hours, so they are refused too: a subgraph that no `end` closes (the
//! grammar reads its body again, and nested ones multiply), and an arrow over 256 bytes (its run
//! and its label; it spans one rank per dot or dash).
//!
//! A panic in the pinned crate is a drawing fault, never a crash, and prints nothing.
//!
//! The pinned crate reads six environment variables at run time. The binary removes them at
//! startup (`rdy_daemon::sys`); a library host must start without them (D-E8P8J).

use mmdflux::{OutputFormat, RenderConfig, SvgThemeConfig, SvgThemeMode};
use std::borrow::Cow;

mod flowchart;

/// The most bytes a source may have.
pub const MAX_SOURCE_BYTES: usize = 65536;
/// The most statements a drawn or unknown source may have.
pub const MAX_STATEMENTS: usize = 2048;
/// The deepest a drawn source may nest subgraphs or blocks.
pub const MAX_DEPTH: usize = 32;
/// The most bytes a drawn SVG may have.
pub const MAX_SVG_BYTES: usize = 4194304;
/// The most bytes one flowchart arrow may have: its run (dots, dashes or equals) and its label.
pub const MAX_ARROW_BYTES: usize = 256;

const OVER_BYTES: &str = "the source is over 65536 bytes";
const OVER_STATEMENTS: &str = "the source has over 2048 statements";
const OVER_DEPTH: &str = "the source nests subgraphs or blocks over 32 deep";
const OVER_SVG: &str = "the drawn diagram is over 4194304 bytes";
const UNCLOSED: &str = "a subgraph is not closed with end";
const OVER_ARROW: &str = "the source has an arrow over 256 bytes";
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
    let m = measure(src, family);
    if m.statements > MAX_STATEMENTS {
        return invalid(OVER_STATEMENTS);
    }
    if m.depth > MAX_DEPTH {
        return invalid(OVER_DEPTH);
    }
    if m.unclosed {
        return invalid(UNCLOSED);
    }
    if m.longest_arrow > MAX_ARROW_BYTES {
        return invalid(OVER_ARROW);
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
    quiet::catch(|| {
        #[cfg(test)]
        test_hook::maybe_panic();
        call()
    })
    .map_err(|()| Diagram::Failed {
        reason: LIBRARY_FAILED.to_string(),
    })
}

/// Catching a panic in the pinned crate without printing it.
///
/// A caught panic still runs the process's panic hook, which prints the message. So the first
/// guarded call wraps the hook that is there: the wrapper prints nothing for a panic on a thread
/// that is inside a guarded call, and passes every other panic to the hook it wrapped. A panic
/// on another thread, or after the call, prints as before. A hook set later replaces the
/// wrapper; then a guarded panic prints again, and is still caught.
mod quiet {
    use std::cell::Cell;
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::Once;

    thread_local! {
        /// Whether this thread is inside a guarded call.
        static GUARDED: Cell<bool> = const { Cell::new(false) };
    }

    static WRAP_HOOK: Once = Once::new();

    /// Puts back this thread's earlier guard state, also when the call unwinds.
    struct Restore(bool);

    impl Drop for Restore {
        fn drop(&mut self) {
            GUARDED.set(self.0);
        }
    }

    pub(crate) fn catch<T>(call: impl FnOnce() -> T) -> Result<T, ()> {
        WRAP_HOOK.call_once(|| {
            let wrapped = panic::take_hook();
            panic::set_hook(Box::new(move |info| {
                // a thread being torn down has no guard state: pass its panic on
                if !GUARDED.try_with(Cell::get).unwrap_or(false) {
                    wrapped(info);
                }
            }));
        });
        let _restore = Restore(GUARDED.replace(true));
        panic::catch_unwind(AssertUnwindSafe(call)).map_err(|_| ())
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

/// What the bounds read in a source.
struct Measure {
    statements: usize,
    depth: usize,
    /// A flowchart subgraph the pinned grammar attempts finds no `end`.
    unclosed: bool,
    /// The longest flowchart arrow, in bytes.
    longest_arrow: usize,
}

/// Counts the statements of `src` after front matter, its deepest nesting and, in a flowchart,
/// whether a subgraph finds no `end` and its longest arrow. The other families' pinned parsers
/// read each line once, so an unclosed block is read once and costs nothing more.
fn measure(src: &str, family: Family) -> Measure {
    let body = after_front_matter(src);
    let statements = segments(body.as_bytes(), family);
    let mut m = Measure {
        statements,
        depth: 0,
        unclosed: false,
        longest_arrow: 0,
    };
    match family {
        Family::Flowchart => {
            let r = flowchart::read(src);
            m.depth = r.depth;
            m.unclosed = r.unclosed;
            m.longest_arrow = r.longest_arrow;
        }
        Family::Class => m.depth = class_depth(body),
        Family::State => m.depth = state_depth(body),
        Family::Sequence | Family::Unknown => {}
    }
    m
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
fn segments(bytes: &[u8], family: Family) -> usize {
    let line_based = matches!(family, Family::Class | Family::State | Family::Sequence);
    let inline_comments = matches!(family, Family::Flowchart | Family::State | Family::Unknown);
    let mut statements = 0;
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
    }
    statements
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
/// The parser skips blank and `%%` lines up to its header. Then it cuts each trimmed line at
/// its first `%%`, even inside quotes, and tries its rules in order. A line that is `}` closes a
/// block, never below zero. A multi-line note's text runs to its `end note` line and is never
/// structure. Only a composite state declaration opens a block: see [`opens_state_block`].
fn state_depth(body: &str) -> usize {
    let mut depth: usize = 0;
    let mut deepest = 0;
    let mut header = true;
    let mut lines = body.lines();
    while let Some(line) = lines.next() {
        let text = before_comment(line.trim());
        if text.is_empty() || text.starts_with("%%") {
            continue;
        }
        if header {
            header = false;
            // a source with no header is refused by the parser; its lines still count here
            if text.to_ascii_lowercase().starts_with("statediagram") {
                continue;
            }
        }
        if text == "}" {
            depth = depth.saturating_sub(1);
        } else if opens_note_text(text) {
            for line in lines.by_ref() {
                if before_comment(line.trim()).eq_ignore_ascii_case("end note") {
                    break;
                }
            }
        } else if opens_state_block(text) {
            depth += 1;
            deepest = deepest.max(depth);
        }
    }
    deepest
}

/// Whether a state-diagram line, trimmed and cut at its comment, opens a composite state. The
/// pinned parser's earlier rules come first:
///
/// - a line that starts with `state` and whitespace is the only kind that can declare a state;
/// - a transition (`<from> --> <to>`, with an optional `: <label>`) declares none;
/// - unless `state` and a space start the line, `<id> : <description>` is a description;
/// - `state` must have an id after it: a `{` or an unclosed quote there declares nothing.
///
/// A declaration whose line ends with `{` opens the composite's block.
fn opens_state_block(text: &str) -> bool {
    let Some(rest) = keyword(text, "state") else {
        return false;
    };
    if !text.ends_with('{') || is_state_transition(text) {
        return false;
    }
    if !text.as_bytes()[..6].eq_ignore_ascii_case(b"state ") && is_state_description(text) {
        return false;
    }
    let rest = rest.trim_start();
    match rest.strip_prefix('"') {
        Some(quoted) => quoted.contains('"'),
        None => !rest.is_empty() && !rest.starts_with('{'),
    }
}

/// Whether the pinned state parser reads `text` as a transition: a `-->` with text before it and
/// a target after it, where the target is what is left before a description colon and a
/// `:::class` suffix.
fn is_state_transition(text: &str) -> bool {
    let Some(arrow) = text.find("-->") else {
        return false;
    };
    let rest = text[arrow + 3..].trim();
    if text[..arrow].trim().is_empty() || rest.is_empty() {
        return false;
    }
    let target = match description_colon(rest) {
        Some(colon) => rest[..colon].trim(),
        None => rest,
    };
    !without_class(target).is_empty()
}

/// Whether the pinned state parser reads `text` as `<id> : <description>`: an id with no space
/// that is not `[*]`, once its `:::class` suffix is cut, and a description that is not empty.
fn is_state_description(text: &str) -> bool {
    let Some(colon) = description_colon(text) else {
        return false;
    };
    let id = without_class(text[..colon].trim());
    !id.is_empty() && !id.contains(' ') && id != "[*]" && !text[colon + 1..].trim().is_empty()
}

/// The first `:` that is not part of a `:::class` annotation; the class name after `:::` runs
/// to the next `:` or space.
fn description_colon(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' {
            if bytes.get(i + 1) == Some(&b':') && bytes.get(i + 2) == Some(&b':') {
                i += 3;
                while i < bytes.len() && bytes[i] != b':' && bytes[i] != b' ' {
                    i += 1;
                }
                continue;
            }
            return Some(i);
        }
        i += 1;
    }
    None
}

/// An id without its `:::class` suffix.
fn without_class(id: &str) -> &str {
    match id.find(":::") {
        Some(at) => id[..at].trim(),
        None => id,
    }
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
        static BEFORE: Cell<Option<fn()>> = const { Cell::new(None) };
    }

    pub(crate) fn panic_at(n: usize) {
        PANIC_AT.set(n);
        CALLS.set(0);
        BEFORE.set(None);
    }

    /// Runs `action` inside the guarded call, just before the panic that [`panic_at`] set.
    pub(crate) fn before_panic(action: fn()) {
        BEFORE.set(Some(action));
    }

    pub(crate) fn maybe_panic() {
        let call = CALLS.get() + 1;
        CALLS.set(call);
        if PANIC_AT.get() == call {
            if let Some(action) = BEFORE.take() {
                action();
            }
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

    /// The subgraph nesting the pinned crate reads in `src`, from its own layout document, or
    /// None when it does not parse. Subgraph ids must be distinct, since the document merges
    /// subgraphs that share one.
    fn parsed_depth(src: &str) -> Option<usize> {
        let doc = mmdflux::materialize_diagram(src, &RenderConfig::default()).ok()?;
        let parent = |id: &str| {
            doc.subgraphs
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.parent.clone())
        };
        let mut deepest = 0;
        for s in &doc.subgraphs {
            let mut depth = 1;
            let mut up = s.parent.clone();
            while let Some(id) = up {
                depth += 1;
                up = parent(&id);
            }
            deepest = deepest.max(depth);
        }
        Some(deepest)
    }

    /// Lines for random flowcharts: subgraphs opened in every way the grammar allows, `end` in
    /// and out of labels and comments, every shape and link, and text the grammar refuses.
    const PIECES: [&str; 47] = [
        "subgraph S{n}\n",
        "subgraph S{n}[title {n}]\n",
        "subgraph S{n} \"t {n}\"\n",
        "subgraph \"t {n}\"\n",
        "subgraph S{n};",
        "A{n} subgraph S{n}\n",
        "subgraph S{n}\n\n",
        "subgraph S{n}[\"q]\n",
        "subgraph S-->{n}\n",
        "subgraph S{n}[(t]\n",
        "subgraph S{n}--x\n",
        "end\n",
        "end\n",
        "end\n",
        "end;",
        "END\n",
        "end %% c\n",
        "N{n}\n",
        "N{n}[label subgraph x end]\n",
        "N{n}[\"q subgraph end ] x\"]\n",
        "N{n}(\nend\nx)\n",
        "N{n}[\"x\nend\"]\n",
        "N{n}[\"y\"]\n",
        "N{n}[x\nsubgraph S{n}\nend]\n",
        "N{n} --> M{n}\n",
        "N{n} -->|subgraph end| M{n}\n",
        "N{n} -- subgraph end --> M{n}\n",
        "N{n} -.-> M{n}\n",
        "N{n} -. dotted end .-> M{n}\n",
        "N{n} == t ==> M{n}\n",
        "N{n} --x M{n}; P{n}\n",
        "%% subgraph end\n",
        "N{n} %% subgraph \"\n",
        "style N{n} fill:#f00\n",
        "direction LR\n",
        "click N{n} x\n",
        "N{n}((c)) --> M{n}([s])\n",
        "N{n}>a] & M{n}{d}\n",
        "N{n}[(cyl)] ~~~ M{n}{{hex}}\n",
        "N{n}@{ shape: doc } --> M{n}[/t\\]\n",
        "N{n}:::cls --> M{n}\n",
        "N{n}(\"x) subgraph S{n}\n",
        "\n",
        "  \t\n",
        "(\n",
        "]\n",
        "\"\n",
    ];

    const CASES: usize = 1000;

    /// Random flowcharts: the walk's bound is never below the depth the pinned crate parses,
    /// and equals it whenever the walk read the whole source and every subgraph it attempted
    /// closed.
    #[test]
    fn dgm1_flowchart_walk_matches_the_pinned_parser() {
        let mut seed: u64 = 0x5eed_d6a1;
        let mut next = |n: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        let (mut parsed, mut exact) = (0, 0);
        for case in 0..CASES {
            let mut src = String::from(if case % 2 == 0 {
                "flowchart TD\n"
            } else {
                "graph LR\n"
            });
            for n in 0..1 + next(24) {
                let piece = PIECES[next(PIECES.len() as u64) as usize];
                src.push_str(&piece.replace("{n}", &n.to_string()));
            }
            let (bound, whole) = flowchart::walk(&src);
            // the pinned crate's layout panics on some of these (an empty subgraph in another)
            let Ok(Some(depth)) = quiet::catch(|| parsed_depth(&src)) else {
                continue;
            };
            parsed += 1;
            assert!(bound >= depth, "bound {bound} < parsed {depth}:\n{src}");
            if whole {
                exact += 1;
                assert_eq!(bound, depth, "walked the whole source:\n{src}");
            }
        }
        // the corpus must exercise both paths
        assert!(
            parsed > 100 && exact > 100,
            "parsed {parsed}, exact {exact}"
        );
    }

    /// The flowchart walk does work in proportion to the text, whatever the text holds: each
    /// shape counts its operations (no timing), on inputs of about 60 KB built to make a scan
    /// read the same bytes again.
    #[test]
    fn dgm1_flowchart_walk_is_linear() {
        let fill = |head: &str, piece: &str, tail: &str| {
            let n = (60000 - head.len() - tail.len()) / piece.len();
            format!("{head}{}{tail}", piece.repeat(n))
        };
        let cases = [
            // a dotted link's start, then one long run of dots with no end
            fill("flowchart LR\nA -. ", ".", "X\n"),
            fill("flowchart LR\nA -. x", ".", "- B\n"),
            fill("flowchart LR\nA -. ", ". ", "X\n"),
            fill("flowchart LR\nA -- ", "-x", "\n"),
            fill("flowchart LR\nA == ", "=x", "\n"),
            // labelled links that find no end on their line, one after another
            fill("flowchart LR\n", "A -. x ", "\n"),
            fill("flowchart LR\n", "subgraph a-.b;", "\n"),
            // open subgraphs whose titles read differently as nodes
            fill("flowchart LR\n", "subgraph S[\"x]\n", ""),
            fill("flowchart LR\n", "subgraph S[\"x\" ]\n", ""),
            fill("flowchart LR\n", "subgraph S[(x]\n", ""),
            fill("flowchart LR\n", "subgraph S\nA[\"x]\n", ""),
            fill("flowchart LR\n", "subgraph S\n", "end\n"),
            // labels across lines, quotes and comments
            fill("flowchart LR\nA[\"", "x\n", ""),
            fill("flowchart LR\nA(", "x\n", ""),
            fill("flowchart LR\n", "A[\"x] ", "\n"),
            fill("flowchart LR\n", "A %% \"\n", ""),
            fill("flowchart LR\n", "(", ""),
            fill("flowchart LR\n", "A --> ", "B\n"),
        ];
        // open headers whose titles, read as nodes, all reach one closer and then the same
        // long rest of a statement: a chain of links, `&` nodes, or a class name
        let shared = |piece: &str, close: &str, rest: &str| {
            let n = (60000 - 20 - close.len()) / (piece.len() + rest.len());
            format!(
                "flowchart LR\n{}{close}{}\n",
                piece.repeat(n),
                rest.repeat(n)
            )
        };
        let shared_rests = [
            shared("subgraph S[(x]\n", "A[(z)]", " --> B"),
            shared("subgraph S[(x]\n", "A[(z)]", " & B"),
            shared("subgraph S[(x]\n", "A[(z)]:::", "c"),
            shared("subgraph S[(x]\n", "A[(z)]", " -- y --> B[q] & C"),
            shared("subgraph S[\"x]\n", "A[\"z\"]", " --> B"),
            shared("subgraph S[\"x]\n", "\"]", " ==> B"),
        ];
        let mut over = Vec::new();
        for src in cases.iter().chain(&shared_rests) {
            let (work, len) = flowchart::work(src);
            if work > 40 * len {
                over.push(format!(
                    "{work} operations for {len} bytes: {:?} .. {:?}",
                    &src[..60],
                    &src[src.len() - 40..]
                ));
            }
        }
        assert!(over.is_empty(), "{over:#?}");
        // the shared rest changes no depth: every open header still counts
        for n in [32, 33, 40] {
            let src = format!(
                "flowchart LR\n{}A[(z)]{}\n",
                "subgraph S[(x]\n".repeat(n),
                " --> B".repeat(n)
            );
            assert_eq!(flowchart::depth(&src), n, "{n} open headers");
        }
    }

    /// The variable that makes [`dgm1_guarded_panic_child`] run.
    const PANIC_CHILD: &str = "RDY_DIAGRAM_GUARDED_PANIC_CHILD";

    /// The child half of [`dgm1_a_guarded_panic_prints_nothing`]. Without its variable it does
    /// nothing.
    #[test]
    fn dgm1_guarded_panic_child() {
        if std::env::var_os(PANIC_CHILD).is_none() {
            return;
        }
        // while the guard is up, another thread panics: the hook still reports that one
        fn unrelated() {
            let joined = std::thread::spawn(|| panic!("an unrelated panic")).join();
            assert!(joined.is_err());
        }
        test_hook::panic_at(1);
        test_hook::before_panic(unrelated);
        let drawn = render("flowchart LR\n    A --> B\n");
        test_hook::panic_at(0);
        assert_eq!(
            drawn,
            Diagram::Failed {
                reason: LIBRARY_FAILED.to_string()
            }
        );
        // after the guard, a panic on this thread is reported again
        assert!(std::panic::catch_unwind(|| panic!("a panic after the guard")).is_err());
        println!("child rendered {drawn:?}");
    }

    /// A guarded panic prints nothing: its payload never reaches stderr. A panic on another
    /// thread while the guard is up, and a panic after the guard, still reach the hook that was
    /// there before.
    #[test]
    fn dgm1_a_guarded_panic_prints_nothing() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::dgm1_guarded_panic_child",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(PANIC_CHILD, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            stdout.contains("child rendered Failed"),
            "stdout:\n{stdout}"
        );
        assert!(
            !stderr.contains("a test panic in the pinned crate"),
            "stderr:\n{stderr}"
        );
        assert!(stderr.contains("an unrelated panic"), "stderr:\n{stderr}");
        assert!(
            stderr.contains("a panic after the guard"),
            "stderr:\n{stderr}"
        );
        assert_eq!(
            stderr.matches("panicked at").count(),
            2,
            "stderr:\n{stderr}"
        );
    }
}
