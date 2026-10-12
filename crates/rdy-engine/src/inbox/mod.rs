//! The inbox (plan M2e2): what concerns one session, computed at read time from the log, the
//! published state and the session's persisted subscription.
use rdy_store::segment::Location;
use serde::Serialize;

pub mod inflight;
pub mod sub;

/// One inbox entry: its kind, the full ids it names, one line for a person or an agent, and the batch it came from (not serialized).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub event: &'static str,
    pub ids: Vec<String>,
    pub line: String,
    #[serde(skip)]
    pub seq: Location,
}
