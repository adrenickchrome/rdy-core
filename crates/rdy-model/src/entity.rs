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

/// One key of a name or a text phrase (D-YXBXH): the phrase itself, or the phrase with one
/// plural ending dropped from its last word ([`name_keys`]). Two phrases match when their key
/// sets meet.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct NameKey(pub String);

/// The least Unicode scalar values a last word keeps when a key drops its final `es` or `s`.
pub const MIN_STEM_CHARS: usize = 3;

/// The keys of `p`, a [`phrase`], sorted, each once, as slices of `p`: `p` without one final
/// `es` of its last word when at least [`MIN_STEM_CHARS`] scalars remain in that word; `p`
/// without one final `s` when its last word does not end in `ss` and as many remain; then `p`.
/// No key is stemmed again, and the earlier words never change. Each key is a prefix of the
/// next, so this order is the sorted order. An empty phrase has no key.
pub fn phrase_keys(p: &str) -> impl Iterator<Item = &str> {
    let last = p.rsplit(' ').next().unwrap_or_default();
    let n = last.chars().count();
    let es = (last.ends_with("es") && n >= MIN_STEM_CHARS + 2).then(|| &p[..p.len() - 2]);
    let s = (last.ends_with('s') && !last.ends_with("ss") && n > MIN_STEM_CHARS)
        .then(|| &p[..p.len() - 1]);
    [es, s, (!p.is_empty()).then_some(p)].into_iter().flatten()
}

/// The keys of `text`: [`phrase_keys`] of its [`phrase`]. At most three, sorted, each once;
/// none for a text with no words.
pub fn name_keys(text: &str) -> Vec<NameKey> {
    let p = phrase(text);
    phrase_keys(&p).map(|k| NameKey(k.to_string())).collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn keys(text: &str) -> Vec<String> {
        name_keys(text).into_iter().map(|k| k.0).collect()
    }

    fn meet(a: &str, b: &str) -> bool {
        let b = name_keys(b);
        name_keys(a).iter().any(|k| b.contains(k))
    }

    #[test]
    fn nb1_name_keys_fold_only_the_last_word() {
        for (a, b) in [
            ("Conflict", "Conflicts"),
            ("Pass", "Passes"),
            ("Release", "Releases"),
            ("merge  CONFLICT", "Merge Conflicts"),
        ] {
            assert!(meet(a, b) && meet(b, a), "{a} / {b}");
        }
        assert_eq!(keys("Passes"), ["pass", "passe", "passes"]);
        assert_eq!(keys("Releases"), ["releas", "release", "releases"]);
        // a final `ss` keeps its `s`
        assert_eq!(keys("Process"), ["process"]);
        assert!(!meet("Process", "Proces"));
        assert!(meet("Process", "Processes"));
        // the accepted match: `news` without its `s` is `new`
        assert_eq!(keys("News"), ["new", "news"]);
        assert!(meet("News", "new"));
        // a remnant of two scalars does not count
        assert_eq!(keys("Bus"), ["bus"]);
        assert_eq!(keys("Yes"), ["yes"]);
        assert_eq!(keys("Uses"), ["use", "uses"]);
        assert!(!meet("Bus", "bu") && !meet("Uses", "us"));
        // three Unicode scalars, not bytes
        assert_eq!(keys("ÑUS"), ["ñus"]);
        assert_eq!(keys("Cafés"), ["café", "cafés"]);
        // no recursive stripping: a derived key is never stripped again
        assert_eq!(keys("Glasses"), ["glass", "glasse", "glasses"]);
        assert!(!meet("Glasses", "glas"));
        assert!(!meet("Releases", "relea"));
        // earlier words never change
        assert_eq!(
            keys("Rules Engines"),
            ["rules engin", "rules engine", "rules engines"]
        );
        assert!(!meet("Rules Engines", "rule engine"));
        // tokenized and joined as `phrase` does
        assert_eq!(keys("  Merge -- conflicts! "), keys("merge conflicts"));
        assert!(name_keys("").is_empty() && name_keys("--").is_empty());
        for t in ["Passes", "Glasses", "News", "a", "Process", "x y zes"] {
            let k = keys(t);
            assert!(k.len() <= 3, "{t}: {k:?}");
            assert!(k.windows(2).all(|w| w[0] < w[1]), "{t}: sorted, no repeat");
            assert!(k.contains(&phrase(t)), "{t}: the phrase itself");
        }
    }

    #[test]
    fn nb1_names_preserve_unicode_spans_and_longest_match() {
        let names = [
            "İstanbul office",
            "merge conflict",
            "conflict resolution",
            "conflict",
            "alpha beta gamma delta epsilon zeta eta theta",
            "one two three four five six seven eight nine",
            "ledger",
        ];
        let known: BTreeSet<NameKey> = names.iter().flat_map(|n| name_keys(n)).collect();
        // a window is already a phrase: its keys come from it as it is, never re-tokenized (a
        // folded `İ` holds a combining mark that is no word character)
        let is_known = |p: &str| phrase_keys(p).any(|k| known.contains(&NameKey(k.to_string())));
        let text = "İSTANBUL Offices: Merge Conflicts resolution, then conflicts. \
                    Alpha beta gamma delta epsilon zeta eta thetas; \
                    one two three four five six seven eight nine. merge\0conflicts ledgers";
        let at = |needle: &str, from: usize| {
            let s = from + text[from..].find(needle).unwrap();
            s..s + needle.len()
        };
        let ist = at("İSTANBUL Offices", 0);
        let merge = at("Merge Conflicts", 0);
        let then = at("conflicts", at("then ", 0).end);
        let greek = at("Alpha beta gamma delta epsilon zeta eta thetas", 0);
        let blanked = at("conflicts", at("merge\0", 0).end);
        let ledgers = at("ledgers", 0);
        assert_eq!(
            find_phrases(text, &is_known),
            vec![
                ("i\u{307}stanbul offices".to_string(), ist),
                ("merge conflicts".to_string(), merge),
                ("conflicts".to_string(), then),
                (
                    "alpha beta gamma delta epsilon zeta eta thetas".to_string(),
                    greek
                ),
                ("conflicts".to_string(), blanked),
                ("ledgers".to_string(), ledgers),
            ],
            "a ninth word is never part of a name; a blanked reference ends a window"
        );
    }
}
