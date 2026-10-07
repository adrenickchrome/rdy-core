//! Block keys: short human names for blocks (`FW-S1`, `REQ-12`), stored in the engine field `key`.

use crate::BlockId;

/// The engine field that marks a doc's or folder's key as one rdy derived from its path
/// (FW-PR3): `true`, or absent. Such a key gets a new numeric suffix when the doc is proposed or
/// landed and another block holds it (K1); a key an agent sets is never changed.
pub const AUTO_FIELD: &str = "key_auto";

/// The longest key, in characters.
const MAX_LEN: usize = 32;

/// `None` when `text` is a valid key, else the reason in a few words.
///
/// Grammar: `[A-Za-z][A-Za-z0-9]*([-_.][A-Za-z0-9]+)*`, 1 to 32 characters, not a block id and
/// not starting with `P-` or `p-` (the proposal-id form).
pub fn problem(text: &str) -> Option<&'static str> {
    if text.is_empty() {
        return Some("a key cannot be empty");
    }
    if text.chars().count() > MAX_LEN {
        return Some("a key is at most 32 characters");
    }
    if !text.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return Some("a key starts with a letter");
    }
    if !text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Some("a key holds only letters, digits and - _ .");
    }
    let mut prev_sep = false;
    for c in text.chars() {
        let sep = matches!(c, '-' | '_' | '.');
        if sep && prev_sep {
            return Some("a key cannot hold two separators in a row");
        }
        prev_sep = sep;
    }
    if prev_sep {
        return Some("a key cannot end with a separator");
    }
    if BlockId::parse(text).is_ok() {
        return Some("a key cannot look like a block id");
    }
    if text.starts_with("P-") || text.starts_with("p-") {
        return Some("a key cannot start with P- (proposal ids use that form)");
    }
    None
}

/// The form keys are compared in: ASCII lowercase.
pub fn fold(text: &str) -> String {
    text.to_ascii_lowercase()
}

/// True for the characters a key uses between its parts.
fn is_sep(c: char) -> bool {
    matches!(c, '-' | '_' | '.')
}

/// `text` with every run of two or more separators replaced by its first one.
fn collapse_runs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_sep = false;
    for c in text.chars() {
        let sep = is_sep(c);
        if !(sep && prev_sep) {
            out.push(c);
        }
        prev_sep = sep;
    }
    out
}

/// `text` cut to at most `max` characters, with trailing separators trimmed.
fn cut(text: &str, max: usize) -> String {
    let cut: String = text.chars().take(max).collect();
    cut.trim_end_matches(is_sep).to_string()
}

/// The key a doc or folder at `path` gets when it is created without one (FW-PR3).
///
/// The rule, in order: split `path` on `/`, drop empty segments and lowercase each; while the
/// segments joined with `.` are longer than 32 characters and more than one remains, drop the
/// first (the leaf is the most telling part, and cutting the front avoids the parent folder's
/// own key); join with `.`, collapse each run of separators to its first one and trim trailing
/// ones; if the result is empty, does not start with a letter or starts with `p-`, prefix
/// `"{block_type}."`; cut to 32 characters and trim trailing separators. If the result is still
/// not a valid key, it is `block_type`. The result is lowercase, so it is never a block id.
pub fn from_path(path: &str, block_type: &str) -> String {
    let mut segments: Vec<String> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(fold)
        .collect();
    while segments.len() > 1 && segments.join(".").chars().count() > MAX_LEN {
        segments.remove(0);
    }
    let mut k = collapse_runs(&segments.join("."))
        .trim_end_matches(is_sep)
        .to_string();
    if !k.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) || k.starts_with("p-") {
        k = collapse_runs(&format!("{block_type}.{k}"))
            .trim_end_matches(is_sep)
            .to_string();
    }
    let k = cut(&k, MAX_LEN);
    if problem(&k).is_some() {
        return block_type.to_string();
    }
    k
}

/// `base` if `taken` does not hold it, else the first of `base-2`, `base-3`, … that is a valid
/// key and that `taken` does not hold. Each candidate fits 32 characters: `base` is cut to make
/// room for the suffix, and trailing separators are trimmed from the cut. Two rules keep every
/// candidate a valid key: after a stem `p` (any case) the suffix is `_{n}`, because `p-2` reads
/// as a proposal id; and a candidate that reads as a block id (a stem of 1 to 3 letters with a
/// 5-digit suffix, `ab-10000`) is skipped. `taken` is asked with the candidates as built
/// (lowercase for a key from `from_path`); callers compare keys folded.
pub fn unique(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_string();
    }
    (2u64..)
        .map(|n| {
            let suffix = format!("-{n}");
            let stem = cut(base, MAX_LEN.saturating_sub(suffix.len()));
            let sep = if stem.eq_ignore_ascii_case("p") {
                '_'
            } else {
                '-'
            };
            format!("{stem}{sep}{n}")
        })
        .find(|c| problem(c).is_none() && !taken(c))
        .expect("an unbounded range always finds a free candidate")
}
