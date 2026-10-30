//! The name and actor grammars (D-ME2C0): closed and byte-based predicates that do no I/O and
//! read no clock. A supplied value is never trimmed, case-folded or aliased, and its length is
//! checked before its bytes are walked. The engine's session checks and the schema's authority
//! policy both read them here.

/// The most bytes in one name: a human's, an agent's handle or name, or a CI job's.
pub const MAX_NAME_BYTES: usize = 64;
/// The most bytes in an actor: `agent:` plus two names and the slash between them.
pub const MAX_ACTOR_BYTES: usize = "agent:".len() + MAX_NAME_BYTES + 1 + MAX_NAME_BYTES;

/// The bytes of a repository ID in a CI actor: lowercase hexadecimal.
const REPO_ID_BYTES: usize = 32;

#[doc(hidden)]
pub fn is_name_end(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit()
}

#[doc(hidden)]
pub fn is_name_byte(b: u8) -> bool {
    is_name_end(b) || matches!(b, b'-' | b'_' | b'.')
}

/// 1 to `max` bytes of `byte`, with `end` bytes first and last.
#[doc(hidden)]
pub fn plain(value: &str, max: usize, end: fn(u8) -> bool, byte: fn(u8) -> bool) -> bool {
    let b = value.as_bytes();
    match (b.first(), b.last()) {
        (Some(&first), Some(&last)) if b.len() <= max => {
            end(first) && end(last) && b.iter().all(|&c| byte(c))
        }
        _ => false,
    }
}

/// A name: 1 to 64 bytes of `a-z`, `0-9`, `-`, `_` and `.`, with a letter or digit at each end.
pub fn is_name(value: &str) -> bool {
    plain(value, MAX_NAME_BYTES, is_name_end, is_name_byte)
}

/// An actor: `human:<name>`, `agent:<name>`, `agent:<handle>/<name>` or
/// `ci:<repo_id>/<name>`, where the repo ID is 32 lowercase hexadecimal bytes (syntax only).
pub fn is_actor(value: &str) -> bool {
    if value.len() > MAX_ACTOR_BYTES {
        return false;
    }
    if let Some(name) = value.strip_prefix("human:") {
        is_name(name)
    } else if let Some(rest) = value.strip_prefix("agent:") {
        match rest.split_once('/') {
            Some((handle, name)) => is_name(handle) && is_name(name),
            None => is_name(rest),
        }
    } else if let Some(rest) = value.strip_prefix("ci:") {
        match rest.split_once('/') {
            Some((repo, name)) => {
                repo.len() == REPO_ID_BYTES
                    && repo
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    && is_name(name)
            }
            None => false,
        }
    } else {
        false
    }
}
