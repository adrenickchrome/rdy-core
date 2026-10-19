//! The stateless batch check and the library stream export (D-D2NPN, D-DAGNB, D-WDKWQ).
//!
//! The batch format, its checks, its errors and its limits belong to core
//! (`rdy_engine::stream_check`, `rdy_engine::stream_export`); this module delegates and re-exports
//! the types those calls use, so a consumer names them from this crate alone. Nothing here keeps
//! state: a check opens no store and a new process can make each call, and an export reads only
//! the refs the snapshot captured. A host that wants independent batches from one ordered stream
//! splits it with [`StreamReader`] and [`StreamWriter`]; those re-exports claim no authorization
//! for a single batch and manage no files. The host owns its batch files, upload ids and
//! quarantine (D-WDKWQ); a checked batch is not a complete upload.

use crate::store::Snapshot;
use rdy_engine::{RdyError, stream_check, stream_export};
use rdy_model::RepoId;
use std::collections::BTreeSet;
use std::io::{Read, Write};

pub use rdy_engine::history_facts::ObjectPresence;
pub use rdy_store::object_refs::ObjectKey;
pub use rdy_store::stream_error::StreamError;
pub use rdy_store::sync_stream::{
    BatchReport, StreamHeader, StreamPurpose, StreamReader, StreamRecord, StreamTotals,
    StreamWriter,
};

/// Checks one complete batch read from `input` and returns its report: the header, the logical
/// key of every object record in wire order, and the trailer's totals. The keys come only after
/// the whole batch passes. With `expected_repo`, the header must name that repo
/// (`E_REPO_MISMATCH`); a new holder passes `None`. Errors, issues, limits and bytes are core's.
pub fn check_batch<R: Read>(
    input: R,
    expected_repo: Option<RepoId>,
) -> Result<BatchReport, RdyError> {
    stream_check::check_batch(input, expected_repo)
}

impl Snapshot {
    /// Writes the history of the captured tag `tag` (`1.2.0` finds `v1.2.0`) as one complete
    /// batch to `out`, less the objects in `known` (at most 100000), which the header names as
    /// prerequisites where a written record needs them. Reads only the snapshot's captured refs
    /// and log end. Owns no output path and does no network or rendering. The returned keys are
    /// in wire order. A tag the snapshot lacks is `E_NOT_FOUND` and nothing is written.
    pub fn write_stream<W: Write>(
        &self,
        tag: &str,
        known: &BTreeSet<ObjectKey>,
        out: W,
    ) -> Result<BatchReport, RdyError> {
        let (hash, _) = self.captured_tag(tag)?;
        stream_export::write(self.read_state(), hash, known, out)
    }
}
