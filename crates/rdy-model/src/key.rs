//! Block keys: short human names for blocks (`FW-S1`, `REQ-12`), stored in the engine field `key`.

use crate::BlockId;

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
