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
//! and its label; it spans one rank per dot or dash). The last refusal is the cost of the arrows
//! and groups (D-VACA9, D-NN3GQ): each arrow the source expands to costs its rank span, times a
//! weight when it has a label; each state note and each boundary between concurrent regions
//! costs one, as the pinned crate builds an arrow for each; and each group (a flowchart
//! subgraph, a composite state, a class namespace or a concurrent region) costs one. A source
//! that costs over [`MAX_ARROWS`] is not drawn. The flowchart walk gives the flowchart's cost,
//! and a line recognizer (see `arrows`) the other families'.
//!
//! The pinned crate draws from the drawing input, a copy of the source with three rewrites:
//! CRLF becomes LF; in a flowchart, each blank line that directly follows a subgraph header goes
//! (D-R7C3W); and top-level subgraphs that each set their own direction get hidden arrows
//! between them, so they stack along the diagram's direction (D-438Y2). The bounds read the
//! drawing input, hidden arrows included. A stacked source is drawn twice, without and with its
//! hidden arrows, and rdy keeps the stacked drawing only when its overlapping subgraph pairs
//! ([`layout`]) are a strict subset of the other's: it removes an overlap and adds none. Every
//! fallback shows the author's own bytes, which no rewrite reaches.
//!
//! A panic in the pinned crate is a drawing fault, never a crash, and prints nothing.
//!
//! The pinned crate reads six environment variables at run time. The binary removes them at
//! startup (`rdy_daemon::sys`); a library host must start without them (D-E8P8J).

use mmdflux::{OutputFormat, RenderConfig, SvgThemeConfig, SvgThemeMode};
use std::borrow::Cow;

mod arrows;
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
/// The most a drawn source's arrows and groups may cost in total (D-VACA9, D-R7C3W, D-NN3GQ;
/// fixed by D-0S2FW). Each arrow the source expands to costs its rank span (the pinned crate's
/// `minlen`), times [`LABELLED_ARROW_WEIGHT`] when it has a label. Each state note, each
/// boundary between concurrent regions and each group (a flowchart subgraph, a composite
/// state, a class namespace or a concurrent region) costs one.
pub const MAX_ARROWS: usize = 200;
/// What a labelled arrow's rank span is multiplied by in the arrow cost (D-VACA9).
pub const LABELLED_ARROW_WEIGHT: usize = 1;
/// The pinned drawing crate and its version, as the diagram cache keys a drawing (D-XM8MP).
pub const DRAWING_CRATE: &str = "mmdflux 2.6.1";

const OVER_BYTES: &str = "the source is over 65536 bytes";
const OVER_STATEMENTS: &str = "the source has over 2048 statements";
const OVER_DEPTH: &str = "the source nests subgraphs or blocks over 32 deep";
const OVER_SVG: &str = "the drawn diagram is over 4194304 bytes";
const UNCLOSED: &str = "a subgraph is not closed with end";
const OVER_ARROW: &str = "the source has an arrow over 256 bytes";
/// Every arrow cost saturates here, so no count or product grows past what the bound reads.
const ARROW_CAP: usize = MAX_ARROWS + 1;
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

impl Family {
    /// The family of the pinned crate's detected diagram type.
    fn of(detected: Option<&str>) -> Family {
        match detected {
            Some("flowchart") => Family::Flowchart,
            Some("class") => Family::Class,
            Some("state") => Family::State,
            Some("sequence") => Family::Sequence,
            _ => Family::Unknown,
        }
    }
}

/// Draws `source`, a diagram's resolved plain source, as SVG: its pre-draw refusals
/// ([`admit`]), then its drawing ([`Admitted::draw`]).
pub fn render(source: &str) -> Diagram {
    match admit(source) {
        Ok(a) => a.draw(),
        Err(why) => why,
    }
}

/// A source that passed every pre-draw refusal (D-XM8MP): its drawing input, ready to draw.
pub struct Admitted<'a> {
    /// The drawing input before any hidden arrow: owned when the CRLF or blank-line rule changed
    /// the source, else borrowed from it.
    drawing: Cow<'a, str>,
    /// The top-level subgraphs that hidden arrows stack, in order (see `flowchart`).
    stacked: Vec<String>,
    /// The pinned crate's detection found no diagram type.
    undetected: bool,
}

/// Every pre-draw refusal of `source`, in render's order; the source that passes, ready to draw.
pub fn admit(source: &str) -> Result<Admitted<'_>, Diagram> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(invalid(OVER_BYTES));
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
    // MMDS JSON goes to the pinned crate's other frontend: rdy never sends it there
    if matches!(src.trim_start().as_bytes().first(), Some(b'{' | b'[')) {
        return Err(invalid(UNKNOWN));
    }
    let detected = guarded(|| mmdflux::detect_diagram(&src))?;
    if detected.is_none()
        && let Some(header) = undrawn_header(&src)
    {
        return Err(Diagram::Unsupported {
            header: header.to_string(),
        });
    }
    let family = Family::of(detected);
    let undetected = detected.is_none();
    // the drawing input: the bounds and the pinned crate read it, and no fallback shows it
    let drawing = match (family, src) {
        (Family::Flowchart, Cow::Borrowed(s)) => flowchart::normalize_subgraph_blanks(s),
        (Family::Flowchart, Cow::Owned(s)) => {
            let changed = match flowchart::normalize_subgraph_blanks(&s) {
                Cow::Owned(n) => Some(n),
                Cow::Borrowed(_) => None,
            };
            Cow::Owned(changed.unwrap_or(s))
        }
        (_, src) => src,
    };
    let m = measure(&drawing, family);
    // each hidden arrow is one more statement
    let hidden = m.stacked.len().saturating_sub(1);
    if m.statements + hidden > MAX_STATEMENTS {
        return Err(invalid(OVER_STATEMENTS));
    }
    if m.depth > MAX_DEPTH {
        return Err(invalid(OVER_DEPTH));
    }
    if m.unclosed {
        return Err(invalid(UNCLOSED));
    }
    if m.longest_arrow > MAX_ARROW_BYTES {
        return Err(invalid(OVER_ARROW));
    }
    // the last pre-draw refusal (D-VACA9, D-NN3GQ): what the source's arrows and groups cost,
    // hidden arrows included
    if m.arrows > MAX_ARROWS {
        return Err(over_arrows());
    }
    Ok(Admitted {
        drawing,
        stacked: m.stacked,
        undetected,
    })
}

impl Admitted<'_> {
    /// The drawing input before any hidden arrow: the source after the BOM removal, CRLF to LF
    /// and, for the flowchart family, the blank-line rule. The diagram cache keys it.
    pub fn normalized(&self) -> &str {
        &self.drawing
    }

    /// Draws the admitted source: the base draw, and for a stacked source the stacked draw and
    /// the choice between them (D-ZJVRT). Never `Unsupported`.
    pub fn draw(self) -> Diagram {
        let base = draw(&self.drawing, self.undetected);
        if self.stacked.len() < 2 {
            return base;
        }
        // a stacked source draws twice: as written, and with its hidden arrows (ruling task-1)
        let stacked = draw(&stack(self.drawing, &self.stacked), false);
        choose(base, stacked)
    }
}

/// Draws the drawing input `src` in one guarded call. `undetected` is whether the pinned
/// crate's detection found no diagram type.
fn draw(src: &str, undetected: bool) -> Diagram {
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
            None if undetected => Diagram::Invalid { reason: e.message },
            None => Diagram::Failed { reason: e.message },
        },
    }
}

/// The drawing rdy keeps for a stacked source (D-438Y2, ruling task-1): the stacked drawing
/// only when its overlapping subgraph pairs are a strict subset of the base drawing's, so it
/// removes an overlap and adds none; else the base drawing. When one draw fails, the other is
/// kept if it drew; when both fail, the base result is kept. A drawing whose geometry cannot be
/// read keeps the base.
fn choose(base: Diagram, stacked: Diagram) -> Diagram {
    match (&base, &stacked) {
        (Diagram::Svg(b), Diagram::Svg(s)) => {
            let fewer = match (layout::overlap_set(b), layout::overlap_set(s)) {
                (Ok(b), Ok(s)) => s.len() < b.len() && s.is_subset(&b),
                _ => false,
            };
            if fewer { stacked } else { base }
        }
        (Diagram::Svg(_), _) => base,
        (_, Diagram::Svg(_)) => stacked,
        _ => base,
    }
}

/// The drawing input with a hidden arrow between each two top-level subgraphs in `ids`, in order
/// (D-438Y2): one line `<id> ~~~ <next id>` each, after an LF if the text does not end with one.
/// Fewer than two ids add nothing.
fn stack<'a>(drawing: Cow<'a, str>, ids: &[String]) -> Cow<'a, str> {
    if ids.len() < 2 {
        return drawing;
    }
    let mut out = drawing.into_owned();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    for pair in ids.windows(2) {
        out.push_str(&pair[0]);
        out.push_str(" ~~~ ");
        out.push_str(&pair[1]);
        out.push('\n');
    }
    Cow::Owned(out)
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

/// The refusal of a source whose arrows and groups cost more than [`MAX_ARROWS`] (D-VACA9,
/// D-NN3GQ).
fn over_arrows() -> Diagram {
    Diagram::Invalid {
        reason: format!(
            "the source has over {MAX_ARROWS} arrows and groups, counting a longer or labelled arrow as more than one"
        ),
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
    /// The top-level flowchart subgraphs that hidden arrows stack, in order (see `flowchart`).
    stacked: Vec<String>,
    /// What the arrows and groups cost (D-VACA9, D-NN3GQ), hidden arrows, state notes, region
    /// boundaries and groups included, saturated at `MAX_ARROWS + 1`.
    arrows: usize,
}

/// Counts the statements of `src` after front matter, its deepest nesting, what its arrows and
/// groups cost and, in a flowchart, whether a subgraph finds no `end` and its longest arrow. The other
/// families' pinned parsers read each line once, so an unclosed block is read once and costs
/// nothing more.
fn measure(src: &str, family: Family) -> Measure {
    let body = after_front_matter(src);
    let statements = segments(body.as_bytes(), family);
    let mut m = Measure {
        statements,
        depth: 0,
        unclosed: false,
        longest_arrow: 0,
        stacked: Vec::new(),
        arrows: 0,
    };
    match family {
        Family::Flowchart => {
            let r = flowchart::read(src);
            m.depth = r.depth;
            m.unclosed = r.unclosed;
            m.longest_arrow = r.longest_arrow;
            m.stacked = r.stacked;
            m.arrows = r.arrows;
        }
        Family::Class => {
            m.depth = class_depth(body);
            m.arrows = arrows::measure(src, family);
        }
        Family::State => {
            m.depth = state_depth(body);
            m.arrows = arrows::measure(src, family);
        }
        Family::Sequence => m.arrows = arrows::measure(src, family),
        Family::Unknown => {}
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
    state_note(text) == Some(true)
}

/// A state-diagram note line, as the pinned parser reads one (`try_parse_note`): `note right of
/// <id>` or `note left of <id>`, then `: <text>` on the same line, or text on the next lines up
/// to `end note`. Whether it is the multi-line kind, if it is a note.
fn state_note(text: &str) -> Option<bool> {
    // the parser needs `note` and one space, then trims
    if !text.as_bytes().get(..5)?.eq_ignore_ascii_case(b"note ") {
        return None;
    }
    let rest = text[5..].trim();
    let after = keyword(rest, "right of")
        .or_else(|| keyword(rest, "left of"))?
        .trim();
    match after.find(':') {
        Some(colon) => (!after[..colon].trim().is_empty()).then_some(false),
        None => (!after.is_empty()).then_some(true),
    }
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

/// A drawn diagram's geometry, read from the pinned crate's SVG (D-438Y2). `render` reads it to
/// choose between a stacked source's two drawings, and the geometry pin reads it to check them.
///
/// Every number is an integer count of hundredths: two decimals and an optional minus. Any
/// other form is an error with its text. The pinned crate draws inside one root
/// `<g transform="translate(DX,DY)">`, which opens before `<g class="clusters">`; the SVG holds
/// exactly one `transform=` attribute. DX is added to every x read, and DY to every y.
pub mod layout {
    use std::collections::BTreeSet;
    use std::fmt;

    /// A rectangle, in hundredths, in the root translate's frame.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Rect {
        pub x: i64,
        pub y: i64,
        pub w: i64,
        pub h: i64,
    }

    impl Rect {
        /// Whether `other` lies inside this one; its edges may touch.
        pub fn holds(&self, other: &Rect) -> bool {
            other.x >= self.x
                && other.y >= self.y
                && other.x + other.w <= self.x + self.w
                && other.y + other.h <= self.y + self.h
        }

        /// Whether the interiors intersect; touching edges do not.
        pub fn intersects(&self, other: &Rect) -> bool {
            self.x < other.x + other.w
                && other.x < self.x + self.w
                && self.y < other.y + other.h
                && other.y < self.y + self.h
        }

        /// Whether the point lies inside; a point on an edge is inside.
        pub fn has(&self, (x, y): (i64, i64)) -> bool {
            x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
        }
    }

    impl fmt::Display for Rect {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                f,
                "(x {}, y {}, width {}, height {})",
                show(self.x),
                show(self.y),
                show(self.w),
                show(self.h)
            )
        }
    }

    /// Hundredths as the SVG writes them.
    pub fn show(n: i64) -> String {
        let sign = if n < 0 { "-" } else { "" };
        format!("{sign}{}.{:02}", n.abs() / 100, n.abs() % 100)
    }

    /// A subgraph: `<g class="cluster" id="ID" data-id="ID"`, with the first
    /// `graph-subgraph-rect` rect and the first `graph-subgraph-text` text in its group.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Subgraph {
        pub id: String,
        pub rect: Rect,
        /// The title's anchor, if the group has a title.
        pub title: Option<(i64, i64)>,
    }

    /// A drawing's viewBox size, its root translate and its subgraphs, in source order.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Layout {
        pub size: (i64, i64),
        pub translate: (i64, i64),
        pub subgraphs: Vec<Subgraph>,
    }

    /// A number as an integer count of hundredths: two decimals, an optional minus.
    fn hundredths(text: &str) -> Result<i64, String> {
        let fail = || format!("not a number with two decimals: {text:?}");
        let (neg, digits) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        let (whole, frac) = digits.split_once('.').ok_or_else(fail)?;
        let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        if !all_digits(whole) || frac.len() != 2 || !all_digits(frac) || whole.len() > 12 {
            return Err(fail());
        }
        let n = whole.parse::<i64>().map_err(|_| fail())? * 100
            + frac.parse::<i64>().map_err(|_| fail())?;
        Ok(if neg { -n } else { n })
    }

    /// The tag that starts at `at`: from `<` through its `>`.
    fn tag_at(svg: &str, at: usize) -> Result<&str, String> {
        let end = svg[at..]
            .find('>')
            .ok_or_else(|| format!("an unclosed tag at {at}"))?;
        Ok(&svg[at..=at + end])
    }

    /// The value of attribute `name` in `tag`.
    fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
        let key = format!(" {name}=\"");
        let start = tag.find(&key)? + key.len();
        let end = tag[start..].find('"')? + start;
        Some(&tag[start..end])
    }

    fn number(tag: &str, name: &str) -> Result<i64, String> {
        hundredths(attr(tag, name).ok_or_else(|| format!("no {name}: {tag}"))?)
    }

    /// The first `<element` tag in `svg[from..to]` whose `data-svg-role` is `role`.
    fn first_with_role<'a>(
        svg: &'a str,
        from: usize,
        to: usize,
        element: &str,
        role: &str,
    ) -> Result<Option<&'a str>, String> {
        let open = format!("<{element} ");
        let mut at = from;
        while let Some(i) = svg[at..to].find(&open) {
            let tag = tag_at(svg, at + i)?;
            if attr(tag, "data-svg-role") == Some(role) {
                return Ok(Some(tag));
            }
            at += i + open.len();
        }
        Ok(None)
    }

    fn rect(tag: &str, (dx, dy): (i64, i64)) -> Result<Rect, String> {
        Ok(Rect {
            x: number(tag, "x")? + dx,
            y: number(tag, "y")? + dy,
            w: number(tag, "width")?,
            h: number(tag, "height")?,
        })
    }

    /// Reads a drawing's viewBox, root translate and subgraphs.
    pub fn read(svg: &str) -> Result<Layout, String> {
        let root = tag_at(svg, svg.find("<svg").ok_or("no svg element")?)?;
        let view = attr(root, "viewBox").ok_or_else(|| format!("no viewBox: {root}"))?;
        let parts: Vec<&str> = view.split(' ').collect();
        if !(parts.len() == 4 && parts[0] == "0" && parts[1] == "0") {
            return Err(format!("viewBox is not \"0 0 W H\": {view:?}"));
        }
        let size = (hundredths(parts[2])?, hundredths(parts[3])?);
        let transforms = svg.matches("transform=").count();
        if transforms != 1 {
            return Err(format!("the SVG holds {transforms} transforms, not one"));
        }
        let at = svg.find("transform=").unwrap_or(0);
        let g = svg[..at].rfind('<').ok_or("a transform outside a tag")?;
        let tag = tag_at(svg, g)?;
        let (dx, dy) = attr(tag, "transform")
            .and_then(|t| t.strip_prefix("translate("))
            .and_then(|t| t.strip_suffix(')'))
            .and_then(|t| t.split_once(','))
            .filter(|_| tag.starts_with("<g transform="))
            .ok_or_else(|| format!("not a root translate: {tag}"))?;
        let translate = (hundredths(dx)?, hundredths(dy)?);
        let clusters = svg
            .find("<g class=\"clusters\">")
            .ok_or("no clusters group")?;
        if g > clusters {
            return Err("the translate opens after the clusters".to_string());
        }
        let mut subgraphs = Vec::new();
        let mut from = 0;
        while let Some(i) = svg[from..].find("<g class=\"cluster\" id=\"") {
            let at = from + i;
            let tag = tag_at(svg, at)?;
            let id = attr(tag, "id").unwrap_or_default();
            if attr(tag, "data-id") != Some(id) {
                return Err(format!("a cluster's data-id is not its id: {tag}"));
            }
            // the cluster's group ends at its first closing tag
            let end = svg[at..].find("</g>").map_or(svg.len(), |e| at + e);
            let shape = first_with_role(svg, at, end, "rect", "graph-subgraph-rect")?
                .ok_or_else(|| format!("{id} has no rectangle"))?;
            let title = match first_with_role(svg, at, end, "text", "graph-subgraph-text")? {
                Some(text) => Some((
                    number(text, "x")? + translate.0,
                    number(text, "y")? + translate.1,
                )),
                None => None,
            };
            subgraphs.push(Subgraph {
                id: id.to_string(),
                rect: rect(shape, translate)?,
                title,
            });
            from = at + tag.len();
        }
        Ok(Layout {
            size,
            translate,
            subgraphs,
        })
    }

    /// The node rectangles of a drawing: each `<g class="node` tag that holds `data-id`, with
    /// the first `graph-node-shape` rect before the next node.
    pub fn nodes(svg: &str, layout: &Layout) -> Result<Vec<(String, Rect)>, String> {
        const NODE: &str = "<g class=\"node";
        let mut nodes = Vec::new();
        let mut from = 0;
        while let Some(i) = svg[from..].find(NODE) {
            let at = from + i;
            let tag = tag_at(svg, at)?;
            let next = svg[at + NODE.len()..]
                .find(NODE)
                .map_or(svg.len(), |n| at + NODE.len() + n);
            if let Some(id) = attr(tag, "data-id") {
                let shape = first_with_role(svg, at, next, "rect", "graph-node-shape")?
                    .ok_or_else(|| format!("node {id} has no rectangle"))?;
                nodes.push((id.to_string(), rect(shape, layout.translate)?));
            }
            from = at + tag.len();
        }
        Ok(nodes)
    }

    /// The subgraph pairs that overlap: their interiors intersect and neither holds the other.
    /// Each pair is its two ids in source order, and the pairs come in source order.
    pub fn overlapping(layout: &Layout) -> Vec<(&str, &str)> {
        let s = &layout.subgraphs;
        let mut out = Vec::new();
        for (i, a) in s.iter().enumerate() {
            for b in &s[i + 1..] {
                if a.rect.intersects(&b.rect) && !a.rect.holds(&b.rect) && !b.rect.holds(&a.rect) {
                    out.push((a.id.as_str(), b.id.as_str()));
                }
            }
        }
        out
    }

    /// The overlapping pairs of a drawing, each as its two ids in order.
    pub(crate) fn overlap_set(svg: &str) -> Result<BTreeSet<(String, String)>, String> {
        let layout = read(svg)?;
        Ok(overlapping(&layout)
            .into_iter()
            .map(|(a, b)| {
                let (a, b) = if a <= b { (a, b) } else { (b, a) };
                (a.to_string(), b.to_string())
            })
            .collect())
    }
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

    /// rdb1 (D-R7C3W): an arrow that starts but has no end on its line counts from its start to
    /// the line end, so 60,000 dots with no end are refused before the drawing call. Guarded
    /// call 2 (the draw) is set to panic: a refusal never reaches it. No timing.
    #[test]
    fn rdb1_unended_arrows_stop_before_drawing() {
        let dots = ".".repeat(60000);
        for start in ["-.", "< -."] {
            for end in ["", "\n", "\r\n"] {
                let src = format!("flowchart LR\nA {start} {dots}X{end}");
                assert!(src.len() <= MAX_SOURCE_BYTES);
                test_hook::panic_at(2);
                let got = render(&src);
                test_hook::panic_at(0);
                assert_eq!(got, invalid(OVER_ARROW), "{start:?}, ending {end:?}");
            }
        }
    }

    /// rdb1 (D-R7C3W): only the blank lines that directly follow a real subgraph header go. A
    /// blank line in a label, a title, a comment or front matter stays, and so does one after a
    /// node or `end`, or after a header that a `;` ends. Unchanged input comes back borrowed.
    #[test]
    fn rdb1_normalization_keeps_nonstructural_blank_lines() {
        use flowchart::normalize_subgraph_blanks as normalize;
        for src in [
            // multiline node labels
            "flowchart TD\nA[\"x\n\ny\"] --> B\n",
            "flowchart TD\nA(\nsubgraph X\n\nB)\n",
            "flowchart TD\nsubgraph S\nA[\"subgraph X\n\nz\"]\nend\n",
            // quoted titles
            "flowchart TD\nsubgraph \"subgraph X\n\nz\"\nA\nend\n",
            "flowchart TD\nsubgraph S[subgraph X\n\nz]\nA\nend\n",
            // comments
            "flowchart TD\n%% subgraph X\n\nA\n",
            "flowchart TD\nA %% subgraph X\n\nB\n",
            // front matter
            "---\ntitle: subgraph X\n\n---\nflowchart TD\nA\n",
            // a blank after a node or `end`
            "flowchart TD\nsubgraph X\nA\n\nend\n",
            "flowchart TD\nsubgraph X\nA\nend\n\nB\n",
            // a header that a `;` ends, then a line ending
            "flowchart TD\nsubgraph X;\nA\nend\n",
            "flowchart TD\nsubgraph X;\n\nA\nend\n",
        ] {
            let got = normalize(src);
            assert!(matches!(got, Cow::Borrowed(_)), "{src:?}");
            assert_eq!(got, src);
        }
        for (src, want) in [
            (
                "flowchart TD\nsubgraph X\n\n  \n\t\nA\nend\n",
                "flowchart TD\nsubgraph X\nA\nend\n",
            ),
            (
                "flowchart TD\nsubgraph X\n\nsubgraph Y\n \nA\nend\nend\n",
                "flowchart TD\nsubgraph X\nsubgraph Y\nA\nend\nend\n",
            ),
            (
                "flowchart TD\nsubgraph X\n\nA[\"p\n\nq\"]\n\nend\n",
                "flowchart TD\nsubgraph X\nA[\"p\n\nq\"]\n\nend\n",
            ),
            (
                "---\ntitle: x\n---\nflowchart TD\nsubgraph X\n\nA\nend\n",
                "---\ntitle: x\n---\nflowchart TD\nsubgraph X\nA\nend\n",
            ),
            (
                "flowchart TD\nsubgraph X\n%%{init: {}}%%\n\naccTitle: t\n\t\nA\nend\n",
                "flowchart TD\nsubgraph X\n%%{init: {}}%%\naccTitle: t\nA\nend\n",
            ),
            (
                "flowchart TD\rsubgraph X\r\r \rA\rend\r",
                "flowchart TD\rsubgraph X\rA\rend\r",
            ),
            ("flowchart TD\nsubgraph X\n  ", "flowchart TD\nsubgraph X\n"),
            // CR CR LF, once CRLF is LF: each blank line's whole CR LF goes
            (
                "flowchart TD\r\nsubgraph X\r\n  \r\n\r\nA\r\nend\r\n",
                "flowchart TD\r\nsubgraph X\r\nA\r\nend\r\n",
            ),
        ] {
            assert_eq!(normalize(src), want, "{src:?}");
        }
        assert_eq!(
            render("flowchart TD\nsubgraph X;\nA\nend\n"),
            invalid(UNCLOSED)
        );
    }

    /// What the base drew for `src`: the pinned crate's SVG, with no rewrite.
    fn base_svg(src: &str) -> Diagram {
        let config = RenderConfig {
            svg_theme: Some(SvgThemeConfig {
                mode: SvgThemeMode::Dynamic,
                ..Default::default()
            }),
            ..Default::default()
        };
        Diagram::Svg(mmdflux::render_diagram(src, OutputFormat::Svg, &config).expect("drawn"))
    }

    /// rdb1 (D-438Y2): separate top-level subgraphs that each set their own direction are
    /// stacked with hidden arrows in the drawing input only. Every other shape stays as it was.
    #[test]
    fn rdb1_hidden_arrows_only_in_the_drawing_input() {
        let four = include_str!("../tests/fixtures/diagram/four-panels.mmd");
        assert_eq!(flowchart::read(four).stacked, ["P1", "P2", "P3", "P4"]);
        let two =
            |a: &str, b: &str| format!("flowchart TB\nsubgraph P1\n{a}end\nsubgraph P2\n{b}end\n");
        let dir = "direction LR\n";
        assert_eq!(
            flowchart::read(&two(
                &format!("{dir}a1 --> a2\n"),
                &format!("{dir}b1 --> b2\n")
            ))
            .stacked,
            ["P1", "P2"]
        );
        let drawn = [
            // one subgraph
            format!("flowchart TB\nsubgraph P1\n{dir}a1 --> a2\nend\n"),
            // a top-level subgraph without its own direction
            two("a1 --> a2\n", &format!("{dir}b1 --> b2\n")),
            // a direction only in a nested subgraph
            two(
                &format!("subgraph Q\n{dir}a1 --> a2\nend\n"),
                &format!("{dir}b1 --> b2\n"),
            ),
            // an arrow between two subgraphs' nodes
            two(&format!("{dir}a1 --> b1\n"), &format!("{dir}b1 --> b2\n")),
            // a node id in two subgraphs
            two(&format!("{dir}a1 --> x\n"), &format!("{dir}x --> b2\n")),
            // an arrow to a subgraph id
            two(&format!("{dir}a1 --> P2\n"), &format!("{dir}b1 --> b2\n")),
            // an arrow at the top level
            format!(
                "{}t1 --> t2\n",
                two(&format!("{dir}a1 --> a2\n"), &format!("{dir}b1 --> b2\n"))
            ),
            // a header with only a quoted title
            format!(
                "flowchart TB\nsubgraph \"first\"\n{dir}a1 --> a2\nend\nsubgraph P2\n{dir}b1 --> b2\nend\n"
            ),
            // a nested header with only a quoted title: the pinned crate names it subGraph0, and
            // an arrow from another top-level subgraph reaches it
            two(
                &format!("{dir}subgraph \"inner\"\na1 --> a2\nend\n"),
                &format!("{dir}b1 --> subGraph0\n"),
            ),
        ];
        for src in &drawn {
            assert!(flowchart::read(src).stacked.is_empty(), "{src}");
            assert!(render(src) == base_svg(src), "{src}");
        }
        // an unclosed subgraph
        let unclosed = format!("flowchart TB\nsubgraph P1\n{dir}a1\nend\nsubgraph P2\n{dir}b1\n");
        assert!(flowchart::read(&unclosed).stacked.is_empty());
        assert_eq!(render(&unclosed), invalid(UNCLOSED));
    }

    /// The reviewer's source (rdb1, fix round 1): it meets the stack condition, and it drew with
    /// no overlap before the hidden arrows existed. With them, T1 and T2 overlap.
    const ADDS_AN_OVERLAP: &str = "flowchart TB\nsubgraph T1\ndirection LR\nsubgraph Q1\ndirection RL\nq4 --> q4b\nend\nn2 -- e --> n3\nend\nsubgraph T2\ndirection RL\nn5\nn6\nn7\nend\n";

    /// rdb1 (D-438Y2, ruling task-1): rdy keeps the stacked drawing only when it removes an
    /// overlap and adds none; else it keeps the base drawing, byte for byte.
    #[test]
    fn rdb1_the_stacked_drawing_must_remove_an_overlap() {
        let pairs = |d: &Diagram| match d {
            Diagram::Svg(svg) => layout::overlap_set(svg).expect("readable"),
            other => panic!("not drawn: {other:?}"),
        };
        // the reviewer's source: the base has no overlap and the stacked drawing adds one
        assert_eq!(flowchart::read(ADDS_AN_OVERLAP).stacked, ["T1", "T2"]);
        assert!(pairs(&base_svg(ADDS_AN_OVERLAP)).is_empty());
        assert!(!pairs(&stacked_svg(ADDS_AN_OVERLAP)).is_empty());
        assert!(render(ADDS_AN_OVERLAP) == base_svg(ADDS_AN_OVERLAP));
        // four-panels and four-cases: the stacked drawing removes every overlap
        for src in [
            include_str!("../tests/fixtures/diagram/four-panels.mmd"),
            include_str!("../tests/fixtures/diagram/four-cases.mmd"),
        ] {
            assert!(!pairs(&base_svg(src)).is_empty());
            assert!(render(src) == stacked_svg(src));
            assert!(render(src) != base_svg(src));
        }
        // one draw fails: the other is kept. Guarded call 2 draws the base, call 3 the stack.
        let four = include_str!("../tests/fixtures/diagram/four-panels.mmd");
        for (call, want) in [(2, stacked_svg(four)), (3, base_svg(four))] {
            test_hook::panic_at(call);
            let got = render(four);
            test_hook::panic_at(0);
            assert!(got == want, "guarded call {call} fails");
        }
        // the choice itself, on hand-built drawings
        let a = ("A", 0, 0, 10, 10);
        let b = ("B", 5, 5, 10, 10);
        let c = ("C", 100, 0, 10, 10);
        let c_on_a = ("C", 8, 0, 10, 10);
        let b_far = ("B", 50, 50, 10, 10);
        let b_on_c = ("B", 105, 5, 10, 10);
        let failed = |r: &str| Diagram::Failed {
            reason: r.to_string(),
        };
        for (name, base, stacked, keep_stacked) in [
            (
                "removes AB, adds none",
                drawn(&[a, b, c]),
                drawn(&[a, b_far, c]),
                true,
            ),
            (
                "removes AB, keeps BC",
                drawn(&[a, b, ("C", 10, 10, 10, 10)]),
                drawn(&[a, ("B", 15, 15, 10, 10), ("C", 10, 10, 10, 10)]),
                true,
            ),
            (
                "adds AC, removes AB",
                drawn(&[a, b, c]),
                drawn(&[a, b_far, c_on_a]),
                false,
            ),
            ("keeps AB", drawn(&[a, b, c]), drawn(&[a, b, c]), false),
            (
                "adds BC, keeps AB",
                drawn(&[a, b, c]),
                drawn(&[a, b, ("C", 12, 12, 10, 10)]),
                false,
            ),
            (
                "none either way",
                drawn(&[a, b_far, c]),
                drawn(&[a, b_on_c, ("C", 200, 0, 10, 10)]),
                false,
            ),
            ("adds AB", drawn(&[a, b_far, c]), drawn(&[a, b, c]), false),
            (
                "the stack is unreadable",
                drawn(&[a, b, c]),
                Diagram::Svg("<svg>".into()),
                false,
            ),
            ("the base fails", failed("base"), drawn(&[a, b, c]), true),
            (
                "the stack fails",
                drawn(&[a, b_far, c]),
                invalid("stacked"),
                false,
            ),
            ("both fail", failed("base"), invalid("stacked"), false),
        ] {
            let want = if keep_stacked { &stacked } else { &base }.clone();
            assert_eq!(choose(base, stacked), want, "{name}");
        }
    }

    /// What the pinned crate draws for the stacked drawing input of `src`.
    fn stacked_svg(src: &str) -> Diagram {
        let ids = flowchart::read(src).stacked;
        assert!(ids.len() >= 2, "not stacked: {src}");
        base_svg(&stack(Cow::Borrowed(src), &ids))
    }

    /// A hand-built drawing: one subgraph rectangle per `(id, x, y, width, height)`, in whole
    /// units, under the root translate `(10, -5)`.
    fn drawn(rects: &[(&str, i64, i64, i64, i64)]) -> Diagram {
        let mut svg = String::from(
            "<svg viewBox=\"0 0 1000.00 1000.00\"><g transform=\"translate(10.00,-5.00)\"><g class=\"clusters\">",
        );
        for (id, x, y, w, h) in rects {
            svg.push_str(&format!(
                "<g class=\"cluster\" id=\"{id}\" data-id=\"{id}\"><rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" data-svg-role=\"graph-subgraph-rect\" /></g>",
                layout::show((x - 10) * 100),
                layout::show((y + 5) * 100),
                layout::show(w * 100),
                layout::show(h * 100)
            ));
        }
        svg.push_str("</g></g></svg>");
        Diagram::Svg(svg)
    }

    /// rdb1 (D-438Y2): the one geometry function. Two subgraphs overlap when their interiors
    /// intersect and neither holds the other; an edge is inside.
    #[test]
    fn rdb1_overlap_geometry() {
        let pairs = |rects: &[(&str, i64, i64, i64, i64)]| {
            let Diagram::Svg(svg) = drawn(rects) else {
                unreachable!()
            };
            let l = layout::read(&svg).expect("readable");
            layout::overlapping(&l)
                .into_iter()
                .map(|(a, b)| format!("{a}{b}"))
                .collect::<Vec<_>>()
        };
        let none: Vec<String> = Vec::new();
        let outer = ("A", 0, 0, 100, 100);
        // nested, also with shared edges
        assert_eq!(pairs(&[outer, ("B", 10, 10, 20, 20)]), none, "nested");
        assert_eq!(
            pairs(&[outer, ("B", 0, 0, 100, 20)]),
            none,
            "nested on edges"
        );
        assert_eq!(
            pairs(&[("B", 10, 10, 20, 20), outer]),
            none,
            "nested, inner first"
        );
        // edge and corner contact
        assert_eq!(pairs(&[outer, ("B", 100, 0, 10, 10)]), none, "edge contact");
        assert_eq!(
            pairs(&[outer, ("B", 100, 100, 10, 10)]),
            none,
            "corner contact"
        );
        // disjoint
        assert_eq!(pairs(&[outer, ("B", 200, 200, 10, 10)]), none, "disjoint");
        // intersecting, also when one sticks out of the other by one unit
        assert_eq!(
            pairs(&[outer, ("B", 50, 50, 100, 10)]),
            ["AB"],
            "intersecting"
        );
        assert_eq!(
            pairs(&[outer, ("B", 0, 0, 101, 10)]),
            ["AB"],
            "out by one unit"
        );
        assert_eq!(
            pairs(&[outer, ("B", 90, 0, 20, 10), ("C", 95, 2, 10, 5)]),
            ["AB", "AC"],
            "pairs in source order; B holds C"
        );
        // the root translate is added before the comparison
        let l = layout::read(&match drawn(&[("A", 0, 0, 10, 10)]) {
            Diagram::Svg(svg) => svg,
            _ => unreachable!(),
        })
        .unwrap();
        assert_eq!(l.translate, (1000, -500));
        assert_eq!(
            l.subgraphs[0].rect,
            layout::Rect {
                x: 0,
                y: 0,
                w: 1000,
                h: 1000
            }
        );
        // a number in another form is an error with its text
        let odd = "<svg viewBox=\"0 0 10 10.00\"></svg>";
        assert_eq!(
            layout::read(odd),
            Err("not a number with two decimals: \"10\"".to_string())
        );
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

    /// The drawn fixtures of `tests/diagram.rs` (its DRAWN list), and four-cases.
    const RDB7_FIXTURES: [&str; 16] = [
        include_str!("../tests/fixtures/diagram/loop.mmd"),
        include_str!("../tests/fixtures/diagram/fan-in.mmd"),
        include_str!("../tests/fixtures/diagram/round-trip.mmd"),
        include_str!("../tests/fixtures/diagram/replay.mmd"),
        include_str!("../tests/fixtures/diagram/four-panels.mmd"),
        include_str!("../tests/fixtures/diagram/fan-out.mmd"),
        include_str!("../tests/fixtures/diagram/chain.mmd"),
        include_str!("../tests/fixtures/diagram/graph.mmd"),
        include_str!("../tests/fixtures/diagram/class.mmd"),
        include_str!("../tests/fixtures/diagram/sequence.mmd"),
        include_str!("../tests/fixtures/diagram/state.mmd"),
        include_str!("../tests/fixtures/diagram/labels.mmd"),
        include_str!("../tests/fixtures/diagram/routing.mmd"),
        include_str!("../tests/fixtures/diagram/diagonal-marker.mmd"),
        include_str!("../tests/fixtures/diagram/document-wave.mmd"),
        include_str!("../tests/fixtures/diagram/four-cases.mmd"),
    ];

    /// rdb7 (D-XM8MP): admission then drawing is render, for every drawn fixture and every
    /// pre-draw refusal; a refused source gives the same `Diagram` from `admit`.
    #[test]
    fn rdb7_admit_then_draw_equals_render() {
        for src in RDB7_FIXTURES {
            let admitted = admit(src).map(Admitted::draw);
            assert!(matches!(admitted, Ok(Diagram::Svg(_))), "{src}");
            assert!(admitted == Ok(render(src)), "{src}");
        }
        // the refusals of dgm1_unclosed_subgraphs_and_long_arrows_are_refused
        let mut refused: Vec<(String, Diagram)> = vec![];
        for k in [16, 18, 20, 22] {
            let src = format!(
                "flowchart LR\n{}A\n",
                (0..k)
                    .map(|i| format!("subgraph U{i}\n"))
                    .collect::<String>()
            );
            refused.push((src, invalid(UNCLOSED)));
        }
        for src in [
            "flowchart LR\nsubgraph S\nA --> B\n",
            "flowchart LR\nA --> B\nsubgraph S[\"x]\nC\n",
            "graph TD\nsubgraph S\nsubgraph T\nA\nend\n",
        ] {
            refused.push((src.to_string(), invalid(UNCLOSED)));
        }
        for dots in [16000, 60000] {
            refused.push((
                format!("flowchart LR\nA -. x{}- B\n", ".".repeat(dots)),
                invalid(OVER_ARROW),
            ));
        }
        refused.push((
            format!("flowchart LR\nA {}> B\n", "-".repeat(256)),
            invalid(OVER_ARROW),
        ));
        // the byte, statement and depth bounds
        refused.push((
            format!("flowchart LR\n    A --> B\n%% {}\n", "x".repeat(65536)),
            invalid(OVER_BYTES),
        ));
        refused.push((
            format!("flowchart LR{}\n", "\nA".repeat(2048)),
            invalid(OVER_STATEMENTS),
        ));
        refused.push((
            format!(
                "flowchart TD\n{}A --> B\n{}",
                (0..33)
                    .map(|i| format!("subgraph S{i}\n"))
                    .collect::<String>(),
                "end\n".repeat(33)
            ),
            invalid(OVER_DEPTH),
        ));
        // a family rdy does not draw, and the MMDS frontend
        refused.push((
            "pie\n  \"a\" : 1".to_string(),
            Diagram::Unsupported {
                header: "pie".to_string(),
            },
        ));
        refused.push(("{\"a\": 1}".to_string(), invalid(UNKNOWN)));
        for (src, want) in &refused {
            assert_eq!(&render(src), want, "{src}");
            match admit(src) {
                Err(got) => assert_eq!(&got, want, "{src}"),
                Ok(_) => panic!("admitted: {src}"),
            }
        }
        // a parse failure after admission is the drawing's result, not a refusal
        let bad = "flowchart LR\n A[";
        assert!(matches!(render(bad), Diagram::Invalid { .. }));
        assert_eq!(admit(bad).map(Admitted::draw), Ok(render(bad)));
    }

    /// rdb7 (D-XM8MP): `normalized` is the drawing input before any hidden arrow: the source
    /// after the BOM, CRLF and (for a flowchart) blank-line rules.
    #[test]
    fn rdb7_normalized_is_the_drawing_input_before_hidden_arrows() {
        let plain = "flowchart TB\nsubgraph S\nA --> B\nend\n";
        let messy = "\u{feff}flowchart TB\r\nsubgraph S\r\n\r\nA --> B\r\nend\r\n";
        let a = admit(plain).unwrap_or_else(|d| panic!("{d:?}"));
        let b = admit(messy).unwrap_or_else(|d| panic!("{d:?}"));
        assert_eq!(a.normalized(), plain);
        assert_eq!(b.normalized(), plain);
        let four = include_str!("../tests/fixtures/diagram/four-panels.mmd");
        let f = admit(four).unwrap_or_else(|d| panic!("{d:?}"));
        assert!(!f.normalized().contains("~~~"), "{}", f.normalized());
        assert_eq!(f.normalized(), four);
        // a class source keeps its blank lines
        let class = "classDiagram\nclass A {\n\n  +x int\n}\n";
        let c = admit(class).unwrap_or_else(|d| panic!("{d:?}"));
        assert_eq!(c.normalized(), class);
    }

    /// rdb7 (D-XM8MP): the cache key names the pinned crate as the workspace pins it.
    #[test]
    fn rdb7_drawing_crate_names_the_pin() {
        let manifest = include_str!("../../../Cargo.toml");
        let line = manifest
            .lines()
            .find(|l| l.starts_with("mmdflux = { version = \"="))
            .expect("the mmdflux pin");
        let version = line
            .trim_start_matches("mmdflux = { version = \"=")
            .split('"')
            .next()
            .unwrap();
        assert_eq!(DRAWING_CRATE, format!("mmdflux {version}"));
    }

    /// The arrow cost of `src` (D-VACA9) and how many of its expanded arrows have a label, as
    /// `admit`'s measure reads the drawing input. The flowchart's comes from its walk, the
    /// others' from the line recognizer; both agree with `measure`.
    fn cost(src: &str) -> (usize, usize) {
        let family = Family::of(mmdflux::detect_diagram(src));
        match family {
            Family::Flowchart => {
                let drawing = flowchart::normalize_subgraph_blanks(src);
                let r = flowchart::read(&drawing);
                assert_eq!(measure(&drawing, family).arrows, r.arrows, "{src}");
                (r.arrows, r.labelled)
            }
            Family::Unknown => (0, 0),
            family => {
                let t = arrows::tally(src, family);
                assert_eq!(measure(src, family).arrows, t.cost, "{src}");
                (t.cost, t.labelled)
            }
        }
    }

    /// A flowchart of `n` plain arrows, one per line: each costs 1.
    fn chain(n: usize) -> String {
        let mut s = String::from("flowchart LR\n");
        for i in 0..n {
            s.push_str(&format!("A{i} --> A{}\n", i + 1));
        }
        s
    }

    /// What `admit` refuses `src` with, or None when it admits it.
    fn refusal(src: &str) -> Option<Diagram> {
        admit(src).err()
    }

    /// `render` and `admit` of `src`, each with guarded call 2 (the draw; call 1 detects) set
    /// to panic: a source refused before its draw gives the same refusal from both.
    fn undrawn(src: &str) -> (Diagram, Option<Diagram>) {
        test_hook::panic_at(2);
        let rendered = render(src);
        test_hook::panic_at(2);
        let admitted = refusal(src);
        test_hook::panic_at(0);
        (rendered, admitted)
    }

    /// rdb6 (D-VACA9, D-R7C3W): every connector form of the closed tables costs its rank span
    /// (the pinned crate's `minlen`), once per expanded arrow, and a label marks the arrow.
    /// The small sources also draw.
    #[test]
    fn rdb6_every_connector_form_is_costed() {
        // (connector, span, labelled), between two flowchart nodes
        let flow: [(&str, usize, bool); 55] = [
            // solid: the dash run after the first dash; open, one less and at least one
            ("-->", 1, false),
            ("--->", 2, false),
            ("---->", 3, false),
            ("---", 1, false),
            ("----", 2, false),
            ("-----", 3, false),
            ("<-->", 1, false),
            ("<--", 1, false),
            ("<---", 2, false),
            ("x--x", 1, false),
            ("o--o", 1, false),
            ("--x", 1, false),
            ("--o", 1, false),
            ("x---", 2, false),
            ("- -->", 2, false),
            // thick: the equals run after the first equals sign
            ("==>", 1, false),
            ("===>", 2, false),
            ("===", 1, false),
            ("====", 2, false),
            ("=====", 3, false),
            ("<==>", 1, false),
            ("x==x", 1, false),
            ("<===", 2, false),
            // dotted: the dot run, open or not
            ("-.->", 1, false),
            ("-..->", 2, false),
            ("-...->", 3, false),
            ("-.-", 1, false),
            ("-..-", 2, false),
            ("<-.->", 1, false),
            ("o-.-o", 1, false),
            ("- .. ->", 2, false),
            // invisible: one
            ("~~~", 1, false),
            ("~~~~~", 1, false),
            ("~ ~ ~", 1, false),
            // a `|text|` label: the connector's own span
            ("-->|t|", 1, true),
            ("--->|t|", 2, true),
            ("---|t|", 1, true),
            ("----|t|", 2, true),
            ("==>|t|", 1, true),
            ("-..->|t|", 2, true),
            ("~~~|t|", 1, true),
            ("--> | t |", 1, true),
            ("<-->|t|", 1, true),
            // inline labels: the closing run's span
            ("-- t -->", 1, true),
            ("-- t --->", 2, true),
            ("-- t ---", 1, true),
            ("-- t ----", 2, true),
            ("<-- t -->", 1, true),
            ("== t ==>", 1, true),
            ("== t ===>", 2, true),
            ("== t ===", 1, true),
            ("-. t .->", 1, true),
            ("-. t ..->", 2, true),
            ("-. t -..->", 2, true),
            ("-. t .-", 1, true),
        ];
        for (connector, span, labelled) in flow {
            let src = format!("flowchart LR\nA {connector} B\n");
            assert_eq!(cost(&src), (span, usize::from(labelled)), "{src}");
            assert!(matches!(render(&src), Diagram::Svg(_)), "{src}");
        }
        // chains: each arrow in a chain counts (D-R7C3W)
        for (src, want) in [
            ("flowchart LR\nA --> B --> C\n", (2, 0)),
            ("flowchart LR\nA --> B ---> C\n", (3, 0)),
            ("flowchart LR\nA -- x --> B -.-> C ==>|y| D\n", (3, 2)),
            ("flowchart LR\nA--->B\n", (2, 0)),
            ("graph TD\nA-->B;B==>C", (2, 0)),
            ("flowchart LR\nA --> B\nB ---> C\nC -.-> A\n", (4, 0)),
        ] {
            assert_eq!(cost(src), want, "{src}");
            assert!(matches!(render(src), Diagram::Svg(_)), "{src}");
        }
        // class: every left marker, line and right marker is one relation
        for left in ["", "<|", "()", "*", "o", "<"] {
            for line in ["--", ".."] {
                for right in ["", "|>", "()", "*", "o", ">"] {
                    let src = format!("classDiagram\nA {left}{line}{right} B\n");
                    assert_eq!(cost(&src), (1, 0), "{src}");
                    assert!(matches!(render(&src), Diagram::Svg(_)), "{src}");
                }
            }
        }
        for (src, want) in [
            ("classDiagram\nA \"1\" --> \"many\" B\n", (1, 1)),
            ("classDiagram\nA \"1\" --> B\n", (1, 1)),
            ("classDiagram\nA --> \"*\" B\n", (1, 1)),
            ("classDiagram\nA --> B : uses\n", (1, 1)),
            ("classDiagram\nA --> B :\n", (1, 0)),
            ("classDiagram\nA \"\" --> \" \" B\n", (1, 0)),
            ("classDiagram\nA<|--B\n", (1, 0)),
            ("classDiagram\n`A x` --> `B y`\n", (1, 0)),
            ("classDiagram\nA --> B\nB ..> C\nC --* A\n", (3, 0)),
        ] {
            assert_eq!(cost(src), want, "{src}");
            assert!(matches!(render(src), Diagram::Svg(_)), "{src}");
        }
        // not a relation: the id runs on through the dashes
        assert_eq!(cost("classDiagram\nA-->B\n"), (0, 0));
        // state: one transition per line, `[*]` ends too; no chains
        for (src, want) in [
            ("stateDiagram-v2\n[*] --> A\nA --> B\nB --> [*]\n", (3, 0)),
            ("stateDiagram-v2\nA --> B : go\n", (1, 1)),
            ("stateDiagram-v2\nA:::c --> B:::d : go\n", (1, 1)),
            ("stateDiagram-v2\nA --> B :\n", (1, 0)),
            ("stateDiagram-v2\nA --> B --> C\n", (1, 0)),
            ("stateDiagram\nA --> B\n", (1, 0)),
        ] {
            assert_eq!(cost(src), want, "{src}");
            assert!(matches!(render(src), Diagram::Svg(_)), "{src}");
        }
        // sequence: all eight forms, with and without activation markers and text
        for form in ["-->>", "->>", "-->", "->", "--x", "-x", "--)", "-)"] {
            for (to, text, labelled) in [
                ("B", ": hi", true),
                ("+B", ": hi", true),
                ("B", ":", false),
                ("B", "", false),
            ] {
                let src = format!("sequenceDiagram\nA{form}{to}{text}\n");
                assert_eq!(cost(&src), (1, usize::from(labelled)), "{src}");
                assert!(matches!(render(&src), Diagram::Svg(_)), "{src}");
            }
            let deactivates = format!("sequenceDiagram\nA->>+B: in\nB{form}-A: out\n");
            assert_eq!(cost(&deactivates), (2, 2), "{deactivates}");
            assert!(
                matches!(render(&deactivates), Diagram::Svg(_)),
                "{deactivates}"
            );
        }
        // one message per line, the first form found
        assert_eq!(cost("sequenceDiagram\nA->>B: x->>y\n"), (1, 1));
        assert_eq!(cost("sequenceDiagram\nA-->>B-->>C\n"), (1, 0));
    }

    /// rdb6 (D-VACA9): a connector between two node groups costs once per pair of a source and
    /// a target, as the pinned crate expands it before layout. Repeated ids count each time,
    /// and the count saturates at `MAX_ARROWS + 1` without building the pairs.
    #[test]
    fn rdb6_groups_cost_expanded_arrows() {
        for (src, want) in [
            ("flowchart LR\nA & B --> C & D\n", (4, 0)),
            ("flowchart LR\nA & B --> C & D --> E\n", (6, 0)),
            ("flowchart LR\nA & A --> B & B\n", (4, 0)),
            ("flowchart LR\nA & B -->|y| C\n", (2, 2)),
            ("flowchart LR\nA & B -- y --> C & D\n", (4, 4)),
            ("flowchart LR\nA & B ----> C & D\n", (12, 0)),
            ("flowchart LR\nA & B ~~~ C\n", (2, 0)),
            ("flowchart LR\nA & B <--> C\n", (2, 0)),
            ("flowchart LR\nA:::x & B:::y --> C\n", (2, 0)),
        ] {
            assert_eq!(cost(src), want, "{src}");
            assert!(matches!(render(src), Diagram::Svg(_)), "{src}");
        }
        let group = |name: &str, n: usize| {
            (0..n)
                .map(|i| format!("{name}{i}"))
                .collect::<Vec<_>>()
                .join(" & ")
        };
        // 15 by 15 passes N; 10 by 20 is N; a thousand by a thousand saturates too
        let over = format!("flowchart LR\n{} --> {}\n", group("a", 15), group("b", 15));
        let at = format!("flowchart LR\n{} --> {}\n", group("a", 10), group("b", 20));
        let huge = format!(
            "flowchart LR\n{} --> {} --> {}\n",
            group("a", 1000),
            group("b", 1000),
            group("c", 1000)
        );
        assert!(huge.len() <= MAX_SOURCE_BYTES);
        assert_eq!(cost(&over).0, MAX_ARROWS + 1);
        assert_eq!(cost(&at).0, MAX_ARROWS);
        assert_eq!(cost(&huge).0, MAX_ARROWS + 1);
        for src in [&over, &huge] {
            assert_eq!(undrawn(src), (over_arrows(), Some(over_arrows())), "{src}");
        }
    }

    /// rdb6 (D-NN3GQ, D-VACA9): each group costs one: a flowchart subgraph, a composite state,
    /// a class namespace and a concurrent region; nested groups each count. Each state note that
    /// the pinned parser accepts and each boundary between concurrent regions (the nonempty
    /// regions of a composite, minus one) cost one too. A composite with k nonempty regions
    /// costs 1 + k + (k - 1); one with no divider has one implicit region, which adds nothing.
    /// Every count saturates at N + 1.
    #[test]
    fn rdb6_notes_regions_and_groups_cost_one_each() {
        // `k` blocks one inside another, each opened with `open` and its own `{i}` id
        let nested = |open: &str, close: &str, k: usize, inner: &str| {
            let opens: String = (0..k)
                .map(|i| open.replace("{i}", &i.to_string()))
                .collect();
            format!("{opens}{inner}{}", close.repeat(k))
        };
        let many = |head: &str, k: usize, one: &dyn Fn(usize) -> String| {
            let mut s = String::from(head);
            for i in 0..k {
                s.push_str(&one(i));
            }
            s
        };
        let cases: Vec<(String, usize)> = vec![
            // flowchart subgraphs: empty, with a node, with arrows, siblings, nested 8 deep
            ("flowchart LR\nsubgraph A\nend\n".into(), 1),
            ("flowchart LR\nsubgraph A[t]\nn\nend\n".into(), 1),
            ("flowchart LR\nsubgraph \"t\"\nn --> m\nend\n".into(), 2),
            ("flowchart LR\nsubgraph A\nn\nend\nsubgraph B\nn\nend\nsubgraph C\nend\n".into(), 3),
            (
                format!("flowchart TD\n{}", nested("subgraph S{i}\n", "end\n", 8, "n\n")),
                8,
            ),
            (
                format!("flowchart TD\n{}", nested("subgraph S{i}\n", "end\n", 8, "a --> b\n")),
                9,
            ),
            // a subgraph keyword in a label or a comment is no group
            ("flowchart LR\nA[\"subgraph X\"] --> B\n%% subgraph Y\n".into(), 1),
            // class namespaces: empty, nested, unclosed; a bad name or a class body holds none
            ("classDiagram\nnamespace N {\n}\n".into(), 1),
            ("classDiagram\nnamespace N {\n  class A\n}\n".into(), 1),
            ("classDiagram\nnamespace N {\n  namespace M {\n    class A\n  }\n}\n".into(), 2),
            ("classDiagram\nnamespace N {\n  A --> B\n".into(), 2),
            ("classDiagram\nnamespace a b {\n}\nnamespace {\n}\n".into(), 0),
            ("classDiagram\nclass A {\n  namespace N {\n}\n".into(), 0),
            (
                format!("classDiagram\n{}", nested("namespace N{i} {\n", "}\n", 8, "class A\n")),
                8,
            ),
            // state notes: single-line, multi-line, empty text; no state id is no note
            ("stateDiagram-v2\nnote right of A : t\n".into(), 1),
            ("stateDiagram-v2\nnote left of A :\n".into(), 1),
            ("stateDiagram-v2\nNOTE RIGHT OF A\n  t\nend note\n".into(), 1),
            ("stateDiagram-v2\nnote right of : t\nnote left of\nnote over A : t\n".into(), 0),
            ("stateDiagram-v2\nstate C {\n  note right of A : t\n}\n".into(), 2),
            // composite states: a child of any kind makes one; an empty body or one that only
            // divides makes none
            ("stateDiagram-v2\nstate C {\n  state A\n}\n".into(), 1),
            ("stateDiagram-v2\nstate C {\n  A --> B\n}\n".into(), 2),
            ("stateDiagram-v2\nstate C {\n  direction LR\n}\n".into(), 1),
            ("stateDiagram-v2\nstate C {\n  classDef c fill:#f00\n}\n".into(), 1),
            ("stateDiagram-v2\nstate C {\n}\nstate D {\n  --\n  --\n}\n".into(), 0),
            ("stateDiagram-v2\nstate C {\n  x y z\n  direction XY\n  %% A --> B\n}\n".into(), 0),
            ("stateDiagram-v2\nstate \"t\" as C {\n  state A\n}\n".into(), 1),
            ("stateDiagram-v2\nstate C {\n  state D {\n    state A\n  }\n}\n".into(), 2),
            (
                format!(
                    "stateDiagram-v2\n{}",
                    nested("state C{i} {\n", "}\n", 8, "state A\n")
                ),
                8,
            ),
            // concurrent regions: k nonempty regions cost 2k with the composite
            ("stateDiagram-v2\nstate C {\n  state A\n  --\n  state B\n}\n".into(), 4),
            (
                "stateDiagram-v2\nstate C {\n  state A\n  --\n  state B\n  --\n  state D\n}\n"
                    .into(),
                6,
            ),
            // a leading, trailing or repeated divider makes no region; one nonempty region
            // with a divider is a region
            ("stateDiagram-v2\nstate C {\n  --\n  state A\n}\n".into(), 2),
            ("stateDiagram-v2\nstate C {\n  state A\n  --\n}\n".into(), 2),
            (
                "stateDiagram-v2\nstate C {\n  --\n  state A\n  --\n  --\n  state B\n  --\n}\n"
                    .into(),
                4,
            ),
            // a region of skipped lines and comments is empty
            (
                "stateDiagram-v2\nstate C {\n  state A\n  --\n  x y z\n  %% t\n  --\n  state B\n}\n"
                    .into(),
                4,
            ),
            // a divider at the top level, or in a note's text, makes no region
            ("stateDiagram-v2\nstate A\n--\nstate B\n".into(), 0),
            (
                "stateDiagram-v2\nstate C {\n  state A\n  note right of A\n  --\n  end note\n}\n"
                    .into(),
                2,
            ),
            // regions nest: an inner composite is a statement of its own region
            (
                "stateDiagram-v2\nstate C {\n  state D {\n    A --> B\n    --\n    state E\n  }\n  --\n  F --> G\n}\n"
                    .into(),
                // C: 4 (two regions); D: 4 (two regions); two transitions
                10,
            ),
            // groups and notes saturate
            (many("flowchart LR\n", 1000, &|i| format!("subgraph S{i}\nend\n")), ARROW_CAP),
            (many("classDiagram\n", 1000, &|i| format!("namespace N{i} {{\n}}\n")), ARROW_CAP),
            (many("stateDiagram-v2\n", 1000, &|_| "note right of A : t\n".into()), ARROW_CAP),
            (
                many("stateDiagram-v2\nstate C {\n", 1000, &|i| format!("state A{i}\n--\n")),
                ARROW_CAP,
            ),
        ];
        for (src, want) in &cases {
            assert_eq!(cost(src).0, *want, "{src}");
            // below the cap, the pinned crate's layout input agrees
            if *want < ARROW_CAP
                && let Ok(Some((pinned, _))) = quiet::catch(|| pinned_cost(src))
            {
                assert_eq!(pinned, *want, "pinned: {src}");
            }
        }
    }

    /// rdb6 (D-VACA9, D-NN3GQ): text that only looks like a connector costs nothing: node
    /// labels, titles, front matter, comments, class members and annotations, state note text,
    /// classes and descriptions, and sequence titles, notes, boxes and message text. Only real
    /// connectors cost, and each group and each state note costs one: a note's text adds
    /// nothing.
    #[test]
    fn rdb6_labels_comments_and_members_add_no_arrows() {
        for (src, want) in [
            // flowchart: labels, titles, comments, front matter, keyword lines
            (
                "---\ntitle: A --> B\n---\nflowchart LR\nA[\"x --> y\"] --> B\n",
                1,
            ),
            ("flowchart LR\nA[\"a\n-->\nb\"] --> B[a ==> b]\n", 1),
            ("flowchart LR\nA(\"-->\") --> B{\"-.->\"}\n", 1),
            ("flowchart LR\nA{{\"x ==> y\"}} --> B([\"~~~\"])\n", 1),
            ("flowchart LR\n%% A --> B\nA --> B %% C --> D\n", 1),
            (
                "flowchart LR\nsubgraph S[\"a --> b\"]\nA --> B\nend\nsubgraph \"c --> d\"\nC\nend\n",
                // one arrow and two subgraphs
                3,
            ),
            ("flowchart LR\nA -->|\"x --> y\"| B\n", 1),
            ("flowchart LR\nA -- \"a ==> b\" --> B\n", 1),
            (
                "flowchart LR\nA --> B\nclick A \"x --> y\"\nstyle A fill:#f00\nclassDef c stroke:#333\nlinkStyle 0 stroke:#000\n",
                1,
            ),
            ("flowchart LR\nA\nB\nC\n", 0),
            // class: members, annotations, display labels, cardinalities, labels, comments
            (
                "---\ntitle: A --> B\n---\nclassDiagram\nclass A {\n  +go() --> B\n  A --> B\n  <<x --> y>>\n}\nA : +x --> y\n<<interface>> A\nclass B[\"x --> y\"]\nclass C <<a-->b>>\n%% A --> B\nA \"x-->y\" --> \"1\" B : a --> b\n",
                1,
            ),
            (
                "classDiagram\nnamespace N {\n  class A\n  A --> B\n}\ndirection LR\n",
                // one relation and one namespace
                2,
            ),
            // state: note text, classes, styles, descriptions, comments, declarations; each of
            // the two notes costs one, whatever its text holds
            (
                "stateDiagram-v2\nnote right of A : x --> y\nnote left of A\n  x --> y\n  A --> B\nend note\nclassDef c fill:#f00,stroke:#000\nclassDef x-->y fill:#f00\nclass A,B x-->y\nstyle A fill:#f00\nA : x ==> y\nstate \"x ==> y\" as S\nclick A \"x --> y\"\n%% A --> B\nA --> B %% C --> D\n",
                3,
            ),
            (
                "stateDiagram-v2\nnote right of A : -->\nnote left of A : x\nnote right of A\n  a --> b\n  c --> d\nend note\n",
                3,
            ),
            // sequence: titles, notes, boxes, participants, blocks, activation, message text
            (
                "sequenceDiagram\ntitle A->>B\nbox Aqua A->>B\nparticipant A as x->>y\nend\nactor C as c-->>d\nNote over A: x->>y\nNote right of A: x->>y\nloop x->>y\nA->>C: a->>b\nend\nalt a->>b\nC-->>A: c\nelse c->>d\nend\nautonumber\nactivate A\ndeactivate A\n%% A->>B\n",
                2,
            ),
        ] {
            assert_eq!(cost(src).0, want, "{src}");
        }
    }

    /// rdb6 (D-VACA9, D-XM8MP): a source over the arrow bound is refused before the pinned
    /// crate draws: guarded call 2 (the draw) is set to panic, and `admit` and `render` both
    /// give the cost reason. An earlier refusal wins over it, and an unsupported header with a
    /// hostile body stays unsupported.
    #[test]
    fn rdb6_over_budget_never_calls_the_drawing_library() {
        let lines = |head: &str, n: usize, line: &dyn Fn(usize) -> String| {
            let mut s = format!("{head}\n");
            for i in 0..n {
                s.push_str(&line(i));
                s.push('\n');
            }
            s
        };
        let over = [
            chain(MAX_ARROWS + 1),
            lines("classDiagram", MAX_ARROWS + 1, &|i| {
                format!("A{i} --> A{}", i + 1)
            }),
            lines("stateDiagram-v2", MAX_ARROWS + 1, &|i| {
                format!("S{i} --> S{}", i + 1)
            }),
            lines("sequenceDiagram", MAX_ARROWS + 1, &|i| {
                format!("A->>B: m{i}")
            }),
            // groups and notes with no arrow (D-NN3GQ)
            lines("flowchart LR", MAX_ARROWS + 1, &|i| {
                format!("subgraph G{i}\nn{i}\nend")
            }),
            lines("classDiagram", MAX_ARROWS + 1, &|i| {
                format!("namespace N{i} {{\nclass C{i}\n}}")
            }),
            lines("stateDiagram-v2", MAX_ARROWS + 1, &|i| {
                format!("note right of S{i} : n{i}")
            }),
        ];
        for src in &over {
            assert_eq!(undrawn(src), (over_arrows(), Some(over_arrows())), "{src}");
        }
        // each earlier refusal, with arrows over the bound too
        let many = chain(MAX_ARROWS + 1);
        let arrows = &many["flowchart LR\n".len()..];
        let nested = format!(
            "flowchart TD\n{}{arrows}{}",
            (0..33)
                .map(|i| format!("subgraph S{i}\n"))
                .collect::<String>(),
            "end\n".repeat(33)
        );
        for (src, want) in [
            (
                format!("{many}{}", "A\n".repeat(MAX_STATEMENTS)),
                invalid(OVER_STATEMENTS),
            ),
            (nested, invalid(OVER_DEPTH)),
            (
                format!("flowchart LR\nsubgraph S\n{arrows}"),
                invalid(UNCLOSED),
            ),
            (
                format!("{many}A {}> B\n", "-".repeat(256)),
                invalid(OVER_ARROW),
            ),
            (
                format!("{many}%% {}\n", "x".repeat(MAX_SOURCE_BYTES)),
                invalid(OVER_BYTES),
            ),
            (
                format!("pie\n{arrows}"),
                Diagram::Unsupported {
                    header: "pie".into(),
                },
            ),
        ] {
            assert_eq!(undrawn(&src), (want.clone(), Some(want)), "{}", &src[..80]);
        }
        // at the bound, the source is admitted
        assert!(refusal(&chain(MAX_ARROWS)).is_none());
    }

    /// rdb6 (D-VACA9): the cost adds no rescan. Chains, memo targets reached again, groups,
    /// failed inline labels and quoted connector text keep `dgm1_flowchart_walk_is_linear`'s
    /// bound of 40 operations a byte. It counts work, never time.
    #[test]
    fn rdb6_arrow_cost_keeps_linear_walk_work() {
        let fill = |head: &str, piece: &str, tail: &str| {
            let n = (60000 - head.len() - tail.len()) / piece.len();
            format!("{head}{}{tail}", piece.repeat(n))
        };
        let shared = |piece: &str, close: &str, rest: &str| {
            let n = (60000 - 20 - close.len()) / (piece.len() + rest.len());
            format!(
                "flowchart LR\n{}{close}{}\n",
                piece.repeat(n),
                rest.repeat(n)
            )
        };
        let cases = [
            // chains, one statement and one per line
            fill("flowchart LR\nA", " --> A", "\n"),
            fill("flowchart LR\nA", " ---> A", "\n"),
            fill("flowchart LR\nA", " -- x --> A", "\n"),
            fill("flowchart LR\nA", " -.-> A", "\n"),
            fill("flowchart LR\nA", " ==>|y| A", "\n"),
            fill("flowchart LR\n", "A --> B\n", ""),
            // groups, with products far past the bound
            fill("flowchart LR\nA", " & A", " --> B\n"),
            fill("flowchart LR\nA", " & A --> A", "\n"),
            fill("flowchart LR\n", "A & B --> C & D\n", ""),
            // memo targets reached again: titles read as nodes reach one chain
            shared("subgraph S[(x]\n", "A[(z)]", " --> B"),
            shared("subgraph S[(x]\n", "A[(z)]", " & B --> C"),
            shared("subgraph S[\"x]\n", "A[\"z\"]", " -->|y| B"),
            // failed inline labels and failed pipe labels
            fill("flowchart LR\n", "A -- x ", "\n"),
            fill("flowchart LR\n", "A == x ", "\n"),
            fill("flowchart LR\nA", " -->|x", "\n"),
            // quoted connector text
            fill("flowchart LR\n", "A[\"-->\"] --> ", "B\n"),
            fill("flowchart LR\n", "A -->|\"-->\"| ", "B\n"),
            fill("flowchart LR\n", "A[\"x --> y", "\"]\n"),
            // subgraphs, in a row and nested (D-NN3GQ)
            fill("flowchart LR\n", "subgraph S\nA --> B\nend\n", ""),
            fill("flowchart LR\n", "subgraph S\n", "A\n"),
        ];
        let mut over = Vec::new();
        for src in &cases {
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
        // the chains and groups are costed, saturated
        for src in &cases[..9] {
            assert_eq!(cost(src).0, MAX_ARROWS + 1, "{}", &src[..60]);
        }
    }

    /// Lines for random flowcharts with every connector form, labels, chains and groups. No
    /// `direction` (so no hidden arrows) and no arrow to a subgraph id (the pinned crate drops
    /// an arrow to an empty subgraph).
    const FLOW_PIECES: [&str; 32] = [
        "N{n} --> M{n}\n",
        "N{n} ---> M{n}\n",
        "N{n} --- M{n}\n",
        "N{n} ----- M{n}\n",
        "N{n} <--> M{n}\n",
        "N{n} x--o M{n}\n",
        "N{n} ==> M{n}\n",
        "N{n} ==== M{n}\n",
        "N{n} -.-> M{n}\n",
        "N{n} -...- M{n}\n",
        "N{n} ~~~ M{n}\n",
        "N{n} -->|t{n}| M{n}\n",
        "N{n} ---|t| M{n}\n",
        "N{n} -- t --> M{n}\n",
        "N{n} -- t ---- M{n}\n",
        "N{n} == t ===> M{n}\n",
        "N{n} -. t ..-> M{n}\n",
        "N{n} --> M{n} ---> P{n}\n",
        "N{n} & M{n} --> P{n} & Q{n}\n",
        "N{n} & N{n} -->|y| M{n} --> P{n}\n",
        "N{n}[\"a --> b\"] --> M{n}(c)\n",
        "N{n}; M{n} --> P{n}\n",
        "subgraph S{n}\nN{n} --> M{n}\nend\n",
        "subgraph S{n}[\"t --> u\"]\nN{n} -.-> M{n}\nend\n",
        "subgraph E{n}\nend\n",
        "subgraph O{n}\nsubgraph I{n}\nN{n} --> M{n}\nend\nend\n",
        "%% N{n} --> M{n}\n",
        "N{n} --> M{n} %% x --> y\n",
        "N{n}\n",
        "style N{n} fill:#f00\n",
        "N{n} -->|\"x --> y\"| M{n}\n",
        "\n",
    ];

    /// Lines for random class diagrams: relations with every marker, cardinalities and labels,
    /// class bodies, members, annotations and comments.
    const CLASS_PIECES: [&str; 26] = [
        "A{n} --> B{n}\n",
        "A{n} --> \"many\" B{n}\n",
        "A{n} \"\" -- B{n}\n",
        "A{n} <|-- B{n}\n",
        "A{n} *-- B{n}\n",
        "A{n} o-- B{n}\n",
        "A{n} ..> B{n}\n",
        "A{n} ..|> B{n}\n",
        "A{n} <--> B{n}\n",
        "A{n} -- B{n}\n",
        "A{n} .. B{n}\n",
        "A{n} \"1\" --> \"*\" B{n}\n",
        "A{n} --> B{n} : uses\n",
        "A{n} \"x\" <|-- B{n} : is\n",
        "class C{n} {\n  +go() --> B{n}\n  A{n} --> B{n}\n}\n",
        "class D{n}\n",
        "A{n} : +field --> x\n",
        "<<interface>> A{n}\n",
        "%% A{n} --> B{n}\n",
        "direction LR\n",
        "namespace N{n} {\n  A{n} --> B{n}\n}\n",
        "namespace E{n} {\n}\n",
        "namespace O{n} {\n  namespace I{n} {\n    class C{n}\n  }\n}\n",
        "A{n}-->B{n}\n",
        "x y z\n",
        "\n",
    ];

    /// Lines for random state diagrams: transitions, `[*]` ends, labels, notes, classes,
    /// descriptions, composite states, concurrent regions and comments. Each region holds a
    /// state, so the pinned crate links each pair of regions next to each other.
    const STATE_PIECES: [&str; 24] = [
        "A{n} --> B{n}\n",
        "[*] --> A{n}\n",
        "A{n} --> [*]\n",
        "A{n} --> B{n} : go{n}\n",
        "A{n}:::c --> B{n} : go\n",
        "A{n} --> B{n} :\n",
        "note right of A{n} : x --> y\n",
        "note left of A{n}\n  x --> y\nend note\n",
        "classDef c fill:#f00\n",
        "class A{n} c\n",
        "style A{n} fill:#0f0\n",
        "A{n} : x ==> y\n",
        "state \"x\" as S{n}\n",
        "state C{n} {\n  X{n} --> Y{n}\n}\n",
        "state E{n} {\n}\n",
        "state D{n} {\n  --\n}\n",
        "state P{n} {\n  state Q{n} {\n    X{n} --> Y{n}\n  }\n}\n",
        "state R{n} {\n  X{n} --> Y{n}\n  --\n  Z{n} --> W{n}\n}\n",
        "state K{n} {\n  --\n  state X{n}\n  --\n  --\n  note right of Y{n} : t\n  Y{n} --> Z{n}\n  --\n  x y z\n  --\n  state W{n} {\n    U{n} --> V{n}\n    --\n    T{n}: t\n  }\n}\n",
        "note right of A{n}\n  x\n  }\nend note\n",
        "%% A{n} --> B{n}\n",
        "A{n} --> B{n} %% C --> D\n",
        "direction LR\n",
        "\n",
    ];

    /// The cost and the labelled count of `src` as the pinned crate builds its layout input: the
    /// sum of `minlen` over every edge of its layout document (a missing `minlen` reads as 1),
    /// state notes' and concurrent regions' edges included, plus its subgraphs, regions included
    /// (D-NN3GQ); and the edges with a label, a head label or a tail label. None when the
    /// pinned crate does not lay the source out.
    fn pinned_cost(src: &str) -> Option<(usize, usize)> {
        use mmdflux::payload::Diagram as Payload;
        let json =
            mmdflux::render_diagram(src, OutputFormat::Mmds, &RenderConfig::default()).ok()?;
        let doc = mmdflux::mmds::parse_input(&json).ok()?;
        let registry = mmdflux::builtins::default_registry();
        let id = registry.detect(src)?;
        let payload = registry.create(id)?.parse(src).ok()?.into_payload().ok()?;
        let graph = match payload {
            Payload::Flowchart(g) | Payload::Class(g) | Payload::State(g) => g,
            Payload::Sequence(_) => return None,
        };
        let minlen: usize = doc.edges.iter().map(|e| e.minlen as usize).sum();
        let some = |l: &Option<String>| l.as_deref().is_some_and(|t| !t.trim().is_empty());
        let labelled = graph
            .edges
            .iter()
            .filter(|e| some(&e.label) || some(&e.head_label) || some(&e.tail_label))
            .count();
        Some((minlen + doc.subgraphs.len(), labelled))
    }

    /// rdb6 (D-VACA9, D-NN3GQ, review M4): on random flowchart, class and state sources, the
    /// cost with W = 1 is the sum of `minlen` over every edge the pinned crate lays out, notes'
    /// and regions' edges included, plus its subgraphs; the labelled count is its edges with a
    /// label. No edge is left out. Sequence has no layout document; its table pins hold.
    #[test]
    fn rdb6_cost_matches_the_pinned_layout_input() {
        assert_eq!(LABELLED_ARROW_WEIGHT, 1);
        let mut seed: u64 = 0x5eed_a660;
        let mut next = |n: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        let families: [(&[&str], [&str; 2]); 3] = [
            (&FLOW_PIECES, ["flowchart TD\n", "graph LR\n"]),
            (&CLASS_PIECES, ["classDiagram\n", "classDiagram\n"]),
            (&STATE_PIECES, ["stateDiagram-v2\n", "stateDiagram\n"]),
        ];
        for (pieces, headers) in families {
            let mut compared = 0;
            for case in 0..CASES {
                let mut src = String::from(headers[case % 2]);
                for n in 0..1 + next(24) {
                    let piece = pieces[next(pieces.len() as u64) as usize];
                    src.push_str(&piece.replace("{n}", &n.to_string()));
                }
                // the pinned crate's layout panics on some sources
                let Ok(Some(want)) = quiet::catch(|| pinned_cost(&src)) else {
                    continue;
                };
                compared += 1;
                assert_eq!(cost(&src), want, "{src}");
            }
            assert!(compared > 500, "{}: compared {compared}", headers[0]);
        }
    }
}
