//! The actor and run grammars (D-ME2C0) and the USER fallback (D-Q8NFG).
//!
//! rdy checks every new session and every actor a person or agent names; it never checks stored
//! records, so nothing that reads or decodes the store calls this module. The grammar is closed
//! and byte-based: a supplied value is never trimmed, case-folded or aliased, and its length is
//! checked before its bytes are walked. The predicates do no I/O and read no clock; the
//! validators allocate only for a refusal, whose echo is bounded (`views::clip`).
use crate::error::{E_BAD_INPUT, RdyError};

/// The most bytes in one name: a human's, an agent's handle or name, or a CI job's.
pub const MAX_NAME_BYTES: usize = 64;
/// The most bytes in an actor: `agent:` plus two names and the slash between them.
pub const MAX_ACTOR_BYTES: usize = "agent:".len() + MAX_NAME_BYTES + 1 + MAX_NAME_BYTES;
/// The most bytes in a run.
pub const MAX_RUN_BYTES: usize = 64;

/// The bytes of a repository ID in a CI actor: lowercase hexadecimal.
const REPO_ID_BYTES: usize = 32;

/// Runs that name a scope or a reserved view, compared exactly (case-sensitive).
const RESERVED_RUNS: [&str; 4] = ["none", "canon", "inflight", "work"];
/// Every run beginning with this is reserved (re-affirmation views).
const RESERVED_RUN_PREFIX: &str = "reaffirm-";

const ACTOR_FIX: &str =
    "use human:<name>, agent:<name>, agent:<handle>/<name> or ci:<repo_id>/<name>";
const RUN_FIX: &str = "use 1 to 64 letters, digits, -, _ or ., with a letter or digit at each end; not none, work, canon, inflight or reaffirm-\u{2026}";

/// The fallback name when USER is absent or normalizes to nothing.
const UNKNOWN: &str = "unknown";
const HUMAN_PREFIX: &str = "human:";

fn is_name_end(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit()
}

fn is_name_byte(b: u8) -> bool {
    is_name_end(b) || matches!(b, b'-' | b'_' | b'.')
}

fn is_run_end(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

fn is_run_byte(b: u8) -> bool {
    is_run_end(b) || matches!(b, b'-' | b'_' | b'.')
}

/// 1 to `max` bytes of `byte`, with `end` bytes first and last.
fn plain(value: &str, max: usize, end: fn(u8) -> bool, byte: fn(u8) -> bool) -> bool {
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

/// A run: 1 to 64 bytes of letters, digits, `-`, `_` and `.`, with a letter or digit at each
/// end; not exactly `none`, `canon`, `inflight` or `work`, and not beginning `reaffirm-`.
pub fn is_run(value: &str) -> bool {
    plain(value, MAX_RUN_BYTES, is_run_end, is_run_byte)
        && !RESERVED_RUNS.contains(&value)
        && !value.starts_with(RESERVED_RUN_PREFIX)
}

/// The refusal of `value` as an actor: its bounded echo, and the form as the fix.
fn actor_refusal(value: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!(
            "the actor {:?} is not in rdy's actor form",
            crate::views::clip(value)
        ),
        ACTOR_FIX,
    )
}

/// The refusal of `value` as a run: its bounded echo, and the form as the fix.
fn run_refusal(value: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!(
            "the run {:?} is not in rdy's run form",
            crate::views::clip(value)
        ),
        RUN_FIX,
    )
}

/// `E_BAD_INPUT` unless `value` is in the actor form.
pub fn validate_actor(value: &str) -> Result<(), RdyError> {
    if is_actor(value) {
        return Ok(());
    }
    Err(actor_refusal(value))
}

/// `E_BAD_INPUT` unless `value` is in the run form.
pub fn validate_run(value: &str) -> Result<(), RdyError> {
    if is_run(value) {
        return Ok(());
    }
    Err(run_refusal(value))
}

/// The most bytes of a value that is not Unicode that its refusal decodes: enough for the
/// 80-scalar echo and its ellipsis, since each decoded scalar takes at most four bytes.
const ENV_ECHO_BYTES: usize = 4 * 81;

/// A session identity read from the environment, as `refusal` refuses it: `None` when the
/// variable is unset, the text when it is Unicode (checked later, with every supplied value),
/// and the refusal of its lossy, bounded decoding when it is not. Such a value is outside the
/// form (the replacement character is in no form), never an unset variable.
fn env_value(
    value: Option<&std::ffi::OsStr>,
    refusal: fn(&str) -> RdyError,
) -> Result<Option<String>, RdyError> {
    let Some(v) = value else {
        return Ok(None);
    };
    match v.to_str() {
        Some(text) => Ok(Some(text.to_string())),
        None => {
            let bytes = v.as_encoded_bytes();
            let head = &bytes[..bytes.len().min(ENV_ECHO_BYTES)];
            Err(refusal(&String::from_utf8_lossy(head)))
        }
    }
}

/// `RDY_ACTOR` as a session reads it (`std::env::var_os`): unset is `None`; a value that is not
/// Unicode is the actor refusal, never unset.
pub fn env_actor(value: Option<&std::ffi::OsStr>) -> Result<Option<String>, RdyError> {
    env_value(value, actor_refusal)
}

/// `RDY_RUN` as a session reads it (`std::env::var_os`): unset is `None`; a value that is not
/// Unicode is the run refusal, never unset.
pub fn env_run(value: Option<&std::ffi::OsStr>) -> Result<Option<String>, RdyError> {
    env_value(value, run_refusal)
}

/// The human actor made from USER when no actor is configured (D-Q8NFG): each scalar of USER
/// lower-cased if it is `A-Z`, kept if it is a name byte, else replaced by `-`; adjacent hyphens
/// fold into one; the first 64 bytes of that are kept, then `-`, `_` and `.` are trimmed from
/// both ends. An empty result, or no USER, is `unknown`. Always `human:<name>`, at most 70
/// bytes, and the scan stops once the 64-byte prefix is full: the rest of USER cannot change it.
pub fn fallback_actor(user: Option<&str>) -> String {
    let mut name = [0u8; MAX_NAME_BYTES];
    let mut len = 0;
    for c in user.unwrap_or("").chars() {
        if len == MAX_NAME_BYTES {
            break;
        }
        let b = match u8::try_from(c) {
            Ok(b) if b.is_ascii_uppercase() => b.to_ascii_lowercase(),
            Ok(b) if is_name_byte(b) => b,
            _ => b'-',
        };
        if b == b'-' && len > 0 && name[len - 1] == b'-' {
            continue;
        }
        name[len] = b;
        len += 1;
    }
    let kept = &name[..len];
    let start = kept.iter().position(|&b| is_name_end(b));
    let end = kept.iter().rposition(|&b| is_name_end(b));
    let trimmed = match (start, end) {
        (Some(s), Some(e)) => &kept[s..=e],
        _ => UNKNOWN.as_bytes(),
    };
    let mut out = String::with_capacity(HUMAN_PREFIX.len() + trimmed.len());
    out.push_str(HUMAN_PREFIX);
    for &b in trimmed {
        out.push(char::from(b));
    }
    out
}
