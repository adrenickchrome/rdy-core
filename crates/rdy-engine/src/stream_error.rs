//! The engine's public error for a sync stream (D-D2NPN). The store's [`StreamError`] says where
//! reading or checking a stream failed; this module maps it, once, to [`RdyError`]. Batch check,
//! export and take all call [`to_rdy`] instead of keeping a mapping each.
use crate::error::RdyError;
use rdy_schema::{Issue, Severity};

pub use rdy_store::stream_error::{Reason, StreamError};

/// `e` as the engine's error: the code, message and fix are kept, and one issue (severity
/// Error, no block, the same code and fix) names the position as
/// `record {record} at byte {offset}; object {object}`. A missing record or object prints
/// `none`; an object prints its kind in decimal, a colon and its full hash.
pub fn to_rdy(e: StreamError) -> RdyError {
    let record = e
        .record
        .map_or_else(|| "none".to_string(), |n| n.to_string());
    let object = e
        .object
        .map_or_else(|| "none".to_string(), |key| key.to_string());
    let issue = Issue {
        severity: Severity::Error,
        code: e.code,
        block: None,
        message: format!("record {record} at byte {}; object {object}", e.offset),
        fix: e.fix.clone(),
    };
    let mut out = RdyError::new(e.code, e.message, e.fix);
    out.issues.push(issue);
    out
}
