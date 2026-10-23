//! rf5 (D-2WR0G, D-Z8P09): the input check of a decision's basis and times. It is an engine-only
//! check of the values a write or an import supplies, not a schema field kind and not a read-time
//! check: a stored value nobody sets again is never judged, and a dump load or a read of an old
//! value does not run it.
//!
//! - The four time fields (`decided_at`, `recorded_at`, `effective_from`, `review_by`) of a
//!   decision, when supplied, are a UTC time `YYYY-MM-DDTHH:MM:SS[.digits]Z` (or `+00:00` for the
//!   `Z`) or a date `YYYY-MM-DD`: a Gregorian date of years 0001 to 9999, hours 0 to 23, minutes
//!   and seconds 0 to 59, and at most 2000 characters. The accepted bytes are kept as written.
//! - A decision whose `authority_basis` is `delegated` names who ruled in a non-blank
//!   `attested_by`. rdy does not parse or judge the mandate.
use rdy_model::{Block, FieldValue, decision};
use rdy_schema::{Issue, Severity};
use std::collections::BTreeSet;

/// The decision time fields the check reads, in the order it reports them.
pub const TIME_FIELDS: [&str; 4] = ["decided_at", "recorded_at", "effective_from", "review_by"];
/// The most characters a time field value holds; a longer one fails the time check.
pub const MAX_TIME_CHARS: usize = 2000;
/// The fix of a time field that is no UTC time or date.
pub const TIME_FIX: &str =
    "write an RFC 3339 UTC time, such as 2026-10-21T15:10:58Z, or a date, such as 2026-10-05";
/// The authority basis that needs an attestation.
pub const DELEGATED: &str = "delegated";
/// The message of a delegated decision with no attestation.
pub const ATTESTED_BY_MESSAGE: &str =
    "field \"attested_by\" is required for authority_basis delegated";
/// The fix of a delegated decision with no attestation.
pub const ATTESTED_BY_FIX: &str = "name who ruled and under whose mandate in attested_by";

/// The message of a time field `name` that is no UTC time or date.
pub fn time_message(name: &str) -> String {
    format!("field \"{name}\" must be an RFC 3339 UTC time or a date")
}

/// The issues of decision `block` for the field names in `supplied`: an `E_FIELD_KIND` issue for
/// each supplied time field present on the block whose value is no UTC time or date, then an
/// `E_FIELD_REQUIRED` issue when the block's `authority_basis` is `delegated` and `attested_by`
/// is missing, not a string or blank. Empty for a block of another type. A create and an import
/// supply every field name they write; an update supplies the names its `fields` set (even to the
/// value already held); a type change to decision supplies the four time fields. A field an
/// update unsets is gone, so it is not checked.
pub fn validate_set(block: &Block, supplied: &BTreeSet<String>) -> Vec<Issue> {
    if block.block_type != decision::DECISION {
        return vec![];
    }
    let issue = |code: &'static str, message: String, fix: &str| Issue {
        severity: Severity::Error,
        code,
        block: Some(block.id.clone()),
        message,
        fix: fix.to_string(),
    };
    let mut out = vec![];
    for name in TIME_FIELDS {
        if !supplied.contains(name) {
            continue;
        }
        let Some(v) = block.fields.get(name) else {
            continue;
        };
        if !matches!(v, FieldValue::Str(s) if is_utc_time_or_date(s)) {
            out.push(issue("E_FIELD_KIND", time_message(name), TIME_FIX));
        }
    }
    if block.field_str("authority_basis") == Some(DELEGATED)
        && !block
            .field_str("attested_by")
            .is_some_and(|a| !a.trim().is_empty())
    {
        out.push(issue(
            "E_FIELD_REQUIRED",
            ATTESTED_BY_MESSAGE.to_string(),
            ATTESTED_BY_FIX,
        ));
    }
    out
}

/// Whether `s` is `YYYY-MM-DD` or `YYYY-MM-DDTHH:MM:SS[.digits]Z` (`+00:00` for the `Z`), a real
/// Gregorian date in years 0001 to 9999 with hours 0 to 23 and minutes and seconds 0 to 59, of at
/// most [`MAX_TIME_CHARS`] characters (counted before anything is parsed). Upper-case `T` and `Z`
/// only.
pub fn is_utc_time_or_date(s: &str) -> bool {
    if s.chars().take(MAX_TIME_CHARS + 1).count() > MAX_TIME_CHARS {
        return false;
    }
    let b = s.as_bytes();
    let num = |from: usize, len: usize| -> Option<u32> {
        let part = b.get(from..from + len)?;
        part.iter().try_fold(0u32, |n, c| {
            c.is_ascii_digit().then(|| n * 10 + u32::from(c - b'0'))
        })
    };
    let at = |i: usize, c: u8| b.get(i) == Some(&c);
    let (Some(y), Some(m), Some(d)) = (num(0, 4), num(5, 2), num(8, 2)) else {
        return false;
    };
    if !(at(4, b'-') && at(7, b'-')) || y == 0 || !(1..=12).contains(&m) {
        return false;
    }
    if d == 0 || d > days_in_month(y, m) {
        return false;
    }
    if b.len() == 10 {
        return true;
    }
    let (Some(hh), Some(mm), Some(ss)) = (num(11, 2), num(14, 2), num(17, 2)) else {
        return false;
    };
    if !(at(10, b'T') && at(13, b':') && at(16, b':')) || hh > 23 || mm > 59 || ss > 59 {
        return false;
    }
    let mut rest = &b[19..];
    if let Some(frac) = rest.strip_prefix(b".") {
        let digits = frac.iter().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 {
            return false;
        }
        rest = &frac[digits..];
    }
    rest == b"Z" || rest == b"+00:00"
}

fn days_in_month(y: u32, m: u32) -> u32 {
    match m {
        2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}
