//! The log's byte report and compact, on one project (D-4YRQ4).
use crate::engine::Engine;
use crate::{E_LIMIT, E_PERMISSION, Rdy, RdyError, Session};
use rdy_store::compact::CompactError;
pub use rdy_store::compact::{CompactReport, KindBytes, LogBytes};

impl From<CompactError> for RdyError {
    fn from(e: CompactError) -> Self {
        match e {
            CompactError::Store(e) => e.into(),
            CompactError::Limit(message) => {
                RdyError::new(E_LIMIT, message, "report this as a bug in rdy")
            }
        }
    }
}

impl Rdy {
    /// The bytes of the log's sealed frames, in total and by kind. It reads frame headers and
    /// removed spans only.
    pub fn log_bytes(&self) -> Result<LogBytes, RdyError> {
        Ok(self.store.log_bytes()?)
    }

    /// Punches the removed payloads of the log, or with `dry_run` only reports. It takes no
    /// approval, appends no frame and publishes nothing; a punch that fails is a note in the
    /// report.
    pub fn compact(&self, dry_run: bool) -> Result<CompactReport, RdyError> {
        Ok(self.store.compact(dry_run)?)
    }
}

/// `rdy compact` and `rdy_compact` (D-4YRQ4): the writer's guard, then the report as JSON. A role
/// that cannot write is refused, with or without `dry_run`, before the store is touched. The call
/// is a serialized read: it seals no batch and publishes nothing.
pub fn run_compact(
    engine: &dyn Engine,
    session: &Session,
    dry_run: bool,
) -> Result<serde_json::Value, RdyError> {
    if !session.role.can_write() {
        return Err(RdyError::new(
            E_PERMISSION,
            "role Reader cannot compact",
            "start the session as a writer",
        ));
    }
    engine.read_serialized(Box::new(move |r| {
        let report = r.compact(dry_run)?;
        Ok(serde_json::to_value(report).expect("a report serializes"))
    }))
}
