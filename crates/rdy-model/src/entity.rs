//! Entity helpers shared by the engine, the renderer and the servers: FW-EN1 (what an entity
//! is and how a name resolves), FW-EN2 (the `value` and `entity` types, aliases) and FW-EN3
//! (finding mentions in text).
//!
//! A word is a maximal run of Unicode alphanumerics and `_`, where `-` and `.` may join two
//! such runs (`ledger-service`, `v1.2`). A phrase is a text's words, lower-cased and joined by
//! one space, so case and spacing never matter.

use crate::{Block, FieldValue, RefMode, ref_spans};
use std::ops::Range;

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
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let end_of = |i: usize| chars.get(i).map_or(text.len(), |(b, _)| *b);
    let mut out = vec![];
    let mut i = 0;
    while i < chars.len() {
        if !is_word_char(chars[i].1) {
            i += 1;
            continue;
        }
        let start = i;
        loop {
            while i < chars.len() && is_word_char(chars[i].1) {
                i += 1;
            }
            let joins = i + 1 < chars.len()
                && matches!(chars[i].1, '-' | '.')
                && is_word_char(chars[i + 1].1);
            if !joins {
                break;
            }
            i += 1;
        }
        let (s, e) = (end_of(start), end_of(i));
        out.push(Word {
            start: s,
            end: e,
            folded: text[s..e].to_lowercase(),
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
/// shorter than [`MIN_MENTION_CHARS`] characters is skipped. Matches never overlap.
pub fn find_phrases(text: &str, known: &dyn Fn(&str) -> bool) -> Vec<(String, Range<usize>)> {
    let ws = words(text);
    let mut out = vec![];
    let mut i = 0;
    while i < ws.len() {
        let longest = MAX_NAME_WORDS.min(ws.len() - i);
        let hit = (1..=longest).rev().find_map(|n| {
            let p = ws[i..i + n]
                .iter()
                .map(|w| w.folded.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            (p.chars().count() >= MIN_MENTION_CHARS && known(&p)).then_some((n, p))
        });
        match hit {
            Some((n, p)) => {
                out.push((p, ws[i].start..ws[i + n - 1].end));
                i += n;
            }
            None => i += 1,
        }
    }
    out
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
/// when it has [`LITERAL_MIN_CHARS`] to [`LITERAL_MAX_CHARS`] characters and holds no `{{`.
/// An unmatched backtick is ignored. Repeats are kept.
pub fn code_literals(text: &str) -> Vec<String> {
    let mut out = vec![];
    for line in text.lines() {
        // Split on backticks: the odd-numbered parts lie between a pair, when a closing
        // backtick follows them.
        let parts: Vec<&str> = line.split('`').collect();
        for (i, inner) in parts.iter().enumerate() {
            if i % 2 == 0 || i + 1 >= parts.len() {
                continue;
            }
            let lit = inner.trim();
            let n = lit.chars().count();
            if (LITERAL_MIN_CHARS..=LITERAL_MAX_CHARS).contains(&n) && !lit.contains("{{") {
                out.push(lit.to_string());
            }
        }
    }
    out
}

/// `text` with every reference under `mode` replaced by spaces of the same byte length.
fn blank_refs(text: &str, mode: RefMode) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for span in ref_spans(text, mode) {
        out.push_str(&text[at..span.range.start]);
        out.push_str(&" ".repeat(span.range.len()));
        at = span.range.end;
    }
    out.push_str(&text[at..]);
    out
}

fn leaves(name: &str, v: &FieldValue, out: &mut Vec<(String, String)>) {
    match v {
        FieldValue::Str(s) if !s.is_empty() => {
            out.push((name.to_string(), blank_refs(s, RefMode::Strict)))
        }
        FieldValue::List(items) => {
            for (i, item) in items.iter().enumerate() {
                leaves(&format!("{name}[{i}]"), item, out);
            }
        }
        FieldValue::Map(m) => {
            for (k, item) in m {
                leaves(&format!("{name}.{k}"), item, out);
            }
        }
        _ => {}
    }
}

/// The text a mention scan reads in `b`, as (field name, text) pairs: the body first (unless
/// its mode is not strict: code and diagram bodies are skipped), then every string leaf of the
/// fields except `key`, a list item as `name[i]` and a map entry as `name.key`. References are
/// blanked, so one is never read as a mention.
pub fn text_sources(b: &Block) -> Vec<(String, String)> {
    let mut out = vec![];
    if RefMode::for_body(&b.block_type) == RefMode::Strict && !b.body.is_empty() {
        out.push(("body".to_string(), blank_refs(&b.body, RefMode::Strict)));
    }
    for (name, v) in &b.fields {
        if name != "key" {
            leaves(name, v, &mut out);
        }
    }
    out
}
