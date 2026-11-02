//! The log's byte report and compact, on one project (D-4YRQ4).
use crate::{E_LIMIT, Rdy, RdyError};
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
