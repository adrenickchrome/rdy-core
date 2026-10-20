//! Entity helpers shared by the engine, the renderer and the servers: FW-EN1 (what an entity
//! is and how a name resolves), FW-EN2 (the `value` and `entity` types, aliases) and FW-EN3
//! (finding mentions in text).
//!
//! A word is a maximal run of Unicode alphanumerics and `_`, where `-` and `.` may join two
//! such runs (`ledger-service`, `v1.2`). A phrase is a text's words, lower-cased and joined by
//! one space, so case and spacing never matter.

use crate::{Block, FieldValue, RefMode, RefSpan, ref_spans};
use std::ops::{ControlFlow, Range};

pub const VALUE: &str = "value";
pub const ENTITY: &str = "entity";
pub const ALIASES: &str = "aliases";
pub const KIND: &str = "kind";
/// A mention must have at least this many characters.
pub const MIN_MENTION_CHARS: usize = 3;
/// The most words in one name that a mention scan tries.
pub const MAX_NAME_WORDS: usize = 8;
/// The most aliases one block contributes.
pub const MAX_ALIASES: usize = 64;
/// A code literal has at least this many characters.
pub const LITERAL_MIN_CHARS: usize = 2;
/// A code literal has at most this many characters.
pub const LITERAL_MAX_CHARS: usize = 64;
/// Fields that hold metadata, not text: a mention scan never reads them (the key, a decision's
/// kind, status, ruling basis, attestation and dates, a code or diagram block's `lang`, a doc's
/// type and lifecycle). The engine also skips every field of kind `enum` in the pack.
pub const METADATA_FIELDS: &[&str] = &[
    "key",
    "kind",
    "lang",
    "doc_type",
    "lifecycle",
    "status",
    "authority_basis",
    "reversibility",
    "attested_by",
    "decided_at",
    "recorded_at",
    "effective_from",
    "review_by",
];

/// One word of a text: its byte range and its case-folded form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    pub start: usize,
    pub end: usize,
    pub folded: String,
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The words of `text`, in order: runs of `[\w]+([-.][\w]+)*`.
pub fn words(text: &str) -> Vec<Word> {
    let mut out = vec![];
    let mut it = text.char_indices().peekable();
    while let Some((start, c)) = it.next() {
        if !is_word_char(c) {
            continue;
        }
        let mut end = start + c.len_utf8();
        loop {
            while let Some(&(i, c)) = it.peek() {
                if !is_word_char(c) {
                    break;
                }
                end = i + c.len_utf8();
                it.next();
            }
            // a `-` or `.` joins this run to a next one that starts right after it
            let mut look = it.clone();
            match (look.next(), look.next()) {
                (Some((_, '-' | '.')), Some((_, n))) if is_word_char(n) => {
                    it.next();
                }
                _ => break,
            }
        }
        out.push(Word {
            start,
            end,
            folded: text[start..end].to_lowercase(),
        });
    }
    out
}

/// The phrase of `text`: its folded words joined by one space.
pub fn phrase(text: &str) -> String {
    words(text)
        .into_iter()
        .map(|w| w.folded)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The phrases of `text` that `known` accepts, left to right, longest first at each word
/// (windows of [`MAX_NAME_WORDS`] words down to one), each with its byte range. A phrase
/// shorter than [`MIN_MENTION_CHARS`] characters is skipped. Matches never overlap. A `'\0'`
/// between two words (a blanked reference) ends a window: no phrase spans it.
pub fn find_phrases(text: &str, known: &dyn Fn(&str) -> bool) -> Vec<(String, Range<usize>)> {
    let mut out = vec![];
    visit_phrases(text, known, &mut |p, range| {
        out.push((p.to_string(), range))
    });
    out
}

/// Calls `visit` with each phrase [`find_phrases`] gives, in the same order, as it is found:
/// nothing is collected.
pub fn visit_phrases(
    text: &str,
    known: &dyn Fn(&str) -> bool,
    visit: &mut dyn FnMut(&str, Range<usize>),
) {
    let ws = words(text);
    // the window of n words is the first n words of the longest window: one join per word,
    // and each shorter window is a prefix of it (`ends[n - 1]` bytes, `chars[n - 1]` chars)
    let mut joined = String::new();
    let mut ends = [0usize; MAX_NAME_WORDS];
    let mut chars = [0usize; MAX_NAME_WORDS];
    let mut i = 0;
    while i < ws.len() {
        let mut longest = MAX_NAME_WORDS.min(ws.len() - i);
        if let Some(k) =
            (1..longest).find(|&k| text[ws[i + k - 1].end..ws[i + k].start].contains('\0'))
        {
            longest = k;
        }
        joined.clear();
        let mut count = 0;
        for (n, w) in ws[i..i + longest].iter().enumerate() {
            if n > 0 {
                joined.push(' ');
                count += 1;
            }
            joined.push_str(&w.folded);
            count += w.folded.chars().count();
            ends[n] = joined.len();
            chars[n] = count;
        }
        let hit = (1..=longest)
            .rev()
            .find(|&n| chars[n - 1] >= MIN_MENTION_CHARS && known(&joined[..ends[n - 1]]));
        match hit {
            Some(n) => {
                visit(&joined[..ends[n - 1]], ws[i].start..ws[i + n - 1].end);
                i += n;
            }
            None => i += 1,
        }
    }
}

/// The `aliases` of `b`: its string items, trimmed, non-empty, without case-insensitive
/// repeats, in order, at most [`MAX_ALIASES`].
pub fn aliases(b: &Block) -> Vec<&str> {
    let Some(FieldValue::List(items)) = b.fields.get(ALIASES) else {
        return vec![];
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut out = vec![];
    for item in items {
        let Some(a) = item.as_str().map(str::trim).filter(|a| !a.is_empty()) else {
            continue;
        };
        if seen.insert(a.to_lowercase()) {
            out.push(a);
            if out.len() == MAX_ALIASES {
                break;
            }
        }
    }
    out
}

/// What a `value` block reads as: its `value`, then its `unit` (`5 ms`, `50%`). `None` for any
/// other type and for an empty value.
pub fn value_text(b: &Block) -> Option<String> {
    if b.block_type != VALUE {
        return None;
    }
    let value = b
        .field_str("value")
        .map(str::trim)
        .filter(|v| !v.is_empty())?;
    match b.field_str("unit").map(str::trim).filter(|u| !u.is_empty()) {
        Some(u) if u.chars().next().is_some_and(char::is_alphanumeric) => {
            Some(format!("{value} {u}"))
        }
        Some(u) => Some(format!("{value}{u}")),
        None => Some(value.to_string()),
    }
}

/// The code literals of `text`: per line, each pair of backticks, its inner text trimmed, kept
/// when it has [`LITERAL_MIN_CHARS`] to [`LITERAL_MAX_CHARS`] characters and holds no `{{` and
/// no `'\0'` (a reference that [`text_sources`] blanked).
/// An unmatched backtick is ignored. Repeats are kept.
pub fn code_literals(text: &str) -> Vec<String> {
    code_literal_spans(text)
        .into_iter()
        .map(|r| text[r].to_string())
        .collect()
}

/// The byte ranges in `text` of its [`code_literals`], in the same order: each covers the
/// trimmed inner text of a pair of backticks on one line.
pub fn code_literal_spans(text: &str) -> Vec<Range<usize>> {
    let mut out = vec![];
    for (at, line) in lines_at(text) {
        // backticks pair up in order on a line: the first with the second, the third with the
        // fourth; a last unpaired one is ignored
        let ticks: Vec<usize> = line.match_indices('`').map(|(i, _)| i).collect();
        for &[open, close] in ticks.as_chunks::<2>().0 {
            let inner = &line[open + 1..close];
            let lit = inner.trim();
            let n = lit.chars().count();
            if (LITERAL_MIN_CHARS..=LITERAL_MAX_CHARS).contains(&n)
                && !lit.contains("{{")
                && !lit.contains('\0')
            {
                let start = at + open + 1 + (inner.len() - inner.trim_start().len());
                out.push(start..start + lit.len());
            }
        }
    }
    out
}

/// The lines of `text` as [`str::lines`] gives them, each with its byte offset in `text`.
pub fn lines_at(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut at = 0;
    text.split_inclusive('\n').map(move |raw| {
        let start = at;
        at += raw.len();
        let line = match raw.strip_suffix('\n') {
            Some(l) => l.strip_suffix('\r').unwrap_or(l),
            None => raw,
        };
        (start, line)
    })
}

/// `text` with every reference under `mode` replaced by `'\0'`s of the same byte length, so a
/// phrase never spans one (see [`find_phrases`]).
fn blank_refs(text: &str, mode: RefMode) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for span in ref_spans(text, mode) {
        out.push_str(&text[at..span.range.start]);
        out.push_str(&"\0".repeat(span.range.len()));
        at = span.range.end;
    }
    out.push_str(&text[at..]);
    out
}

/// How a text source's references are written: `render(text, mode)` is the text with every
/// reference under `mode` replaced.
type RenderRefs<'a> = &'a dyn Fn(&str, RefMode) -> String;

/// The text a mention scan reads in `b`, as (field name, text) pairs: the body first (unless
/// its mode is not strict: code and diagram bodies are skipped), then every string leaf of the
/// fields except the [`METADATA_FIELDS`], a list item as `name[i]` and a map entry as
/// `name.key`. References are blanked, so one is never read as a mention.
pub fn text_sources(b: &Block) -> Vec<(String, String)> {
    sources_with(b, &blank_refs)
}

/// The same sources as [`text_sources`], for a reader: each reference shows as its bare token,
/// the way `rdy_read` shows one ([`render_refs`]), never blanked. The mention scan must not use
/// this: it would read a reference's id as a mention.
pub fn shown_sources(b: &Block) -> Vec<(String, String)> {
    sources_with(b, &|text, mode| {
        render_refs(text, mode, &|span| span.token.clone())
    })
}

/// `text` with each reference under `mode` replaced by what `resolve` gives for its span, and
/// `\{{` shown as `{{`. A `RefMode::Off` text is returned as it is. `rdy_read` and the review
/// case files both render text this way.
pub fn render_refs(text: &str, mode: RefMode, resolve: &dyn Fn(&RefSpan) -> String) -> String {
    if mode == RefMode::Off {
        return text.to_string();
    }
    let plain = |s: &str| s.replace("\\{{", "{{");
    let mut out = String::new();
    let mut last = 0;
    for span in ref_spans(text, mode) {
        out.push_str(&plain(&text[last..span.range.start]));
        out.push_str(&resolve(&span));
        last = span.range.end;
    }
    out.push_str(&plain(&text[last..]));
    out
}

fn sources_with(b: &Block, render: RenderRefs<'_>) -> Vec<(String, String)> {
    let mut out = vec![];
    visit_text_sources(b, &mut |name, text, mode| {
        out.push((name.to_string(), render(text, mode)));
        ControlFlow::Continue(())
    });
    out
}

/// Calls `visit` with each text source of `b`, borrowed, as (source name, stored text, mode),
/// in the order of [`text_sources`]: the body first when its mode is strict (code and diagram
/// bodies are skipped), then each non-empty string leaf of the fields except the
/// [`METADATA_FIELDS`], by field name, a list item as `name[i]` and a map entry as `name.key`.
/// The text is as stored: references are not blanked. A `Break` stops before the next source.
pub fn visit_text_sources(
    b: &Block,
    visit: &mut dyn FnMut(&str, &str, RefMode) -> ControlFlow<()>,
) {
    let mode = RefMode::for_body(&b.block_type);
    if mode == RefMode::Strict && !b.body.is_empty() && visit("body", &b.body, mode).is_break() {
        return;
    }
    for (name, v) in &b.fields {
        if !METADATA_FIELDS.contains(&name.as_str()) && visit_leaves(name, v, visit).is_break() {
            return;
        }
    }
}

/// Calls `visit` with each non-empty string leaf of `v`, labelled `name`, `name[i]` or
/// `name.key`, in order; every field text is [`RefMode::Strict`]. A `Break` stops the walk and
/// is returned.
pub fn visit_leaves(
    name: &str,
    v: &FieldValue,
    visit: &mut dyn FnMut(&str, &str, RefMode) -> ControlFlow<()>,
) -> ControlFlow<()> {
    match v {
        FieldValue::Str(s) if !s.is_empty() => visit(name, s, RefMode::Strict),
        FieldValue::List(items) => {
            for (i, item) in items.iter().enumerate() {
                visit_leaves(&format!("{name}[{i}]"), item, visit)?;
            }
            ControlFlow::Continue(())
        }
        FieldValue::Map(m) => {
            for (k, item) in m {
                visit_leaves(&format!("{name}.{k}"), item, visit)?;
            }
            ControlFlow::Continue(())
        }
        _ => ControlFlow::Continue(()),
    }
}

/// The field a text source's name comes from: `steps[0]` and `cells.a` are `steps` and `cells`.
pub fn source_field(name: &str) -> &str {
    name.find(['[', '.']).map_or(name, |i| &name[..i])
}
