//! The stateless batch check and the library stream export (D-D2NPN, D-DAGNB, D-WDKWQ).
//!
//! The batch format, its checks, its errors and its limits belong to core
//! (`rdy_engine::stream_check`, `rdy_engine::stream_export`); this module delegates and re-exports
//! the types those calls use, so a consumer names them from this crate alone. Nothing here keeps
//! state of its own: a check opens no store and a new process can make each call, and an export
//! reads only the refs the snapshot captured. The one exception is a pull a caller prepares
//! ([`Snapshot::prepare_pull`], [`Snapshot::prepare_pull_with_base`]): the caller owns that value
//! for one operation, and dropping it releases what it kept. A host that wants independent batches from one ordered stream
//! splits it with [`StreamReader`] and [`StreamWriter`]; those re-exports claim no authorization
//! for a single batch and manage no files. The host owns its batch files, upload ids and
//! quarantine (D-WDKWQ); a checked batch is not a complete upload.

use crate::store::Snapshot;
pub use rdy_engine::canon_state_export::PreparedCanonState;
pub use rdy_engine::hub_http::BodyEncoding;
pub use rdy_engine::stream_export::{EncodedHistory, KeyPage, PreparedPull};
use rdy_engine::{RdyError, stream_check, stream_export};
use rdy_model::{Hash, RepoId};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::sync::Arc;

pub use rdy_engine::history_facts::ObjectPresence;
pub use rdy_store::object_refs::ObjectKey;
pub use rdy_store::stream_error::StreamError;
pub use rdy_store::sync_stream::{
    BatchReport, CANON_STATE_FORMAT, HISTORY_FORMAT, StreamHeader, StreamPurpose, StreamReader,
    StreamRecord, StreamTotals, StreamWriter,
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

    /// Writes the canon history (`purpose` canon_history, `root` a landing) or the view history
    /// (view_history, `root` a view op) of this snapshot as one complete format-4 batch to `out`
    /// (D-KE83X), less the objects of `known` (at most 100000) and the link closure of `base`, a
    /// landing or view op the receiver holds. It is core's export (`write_history`), read up to
    /// the snapshot's log position: a hub's tests and tools make their pushes with it. A root
    /// written after the snapshot is `E_UNRESOLVED_REF`, and nothing is written.
    pub fn write_history<W: Write>(
        &self,
        purpose: StreamPurpose,
        root: Hash,
        base: Option<Hash>,
        known: &BTreeSet<ObjectKey>,
        out: W,
    ) -> Result<BatchReport, RdyError> {
        stream_export::write_history(self.read_state(), purpose, root, base, known, out)
    }

    /// Gives a page of at most `limit` keys (1 to 100000) of the canon or view history of
    /// `root`, less the link closure of `base`, in the order [`Snapshot::write_history`] writes
    /// them, after the key `after` (from the start when None). `next` is the page's last key
    /// when more keys follow, else None. A `base` the snapshot does not hold counts as none. It
    /// reads only the snapshot's captured refs and log end, keeps no state and does no network.
    /// A root the snapshot does not hold, or an `after` outside the history, is
    /// `E_UNRESOLVED_REF`; another purpose is `E_STREAM_PURPOSE`; a `limit` out of range is
    /// `E_LIMIT`.
    pub fn history_page(
        &self,
        purpose: StreamPurpose,
        root: Hash,
        base: Option<Hash>,
        after: Option<ObjectKey>,
        limit: usize,
    ) -> Result<KeyPage, RdyError> {
        stream_export::history_page(self.read_state(), purpose, root, base, after, limit)
    }

    /// Writes the records that `keys` name of the canon or view history of `root` as one
    /// complete format-4 batch with that purpose and root, in the order
    /// [`Snapshot::write_history`] would write that set. The header names the root as a
    /// prerequisite when the batch does not hold it. A landing carries its one snapshot delta,
    /// which the batch holds though `keys` omit it; a named delta travels only with its landing.
    /// A repeated key counts once. It reads only
    /// the snapshot's captured refs and log end, owns no output path, keeps no state and does
    /// no network. A key outside the root's history, or a root the snapshot does not hold, is
    /// `E_UNRESOLVED_REF` and nothing is written; more than 100000 keys is `E_LIMIT`; another
    /// purpose is `E_STREAM_PURPOSE`.
    pub fn write_history_keys<W: Write>(
        &self,
        purpose: StreamPurpose,
        root: Hash,
        keys: &[ObjectKey],
        out: W,
    ) -> Result<BatchReport, RdyError> {
        stream_export::write_history_keys(self.read_state(), purpose, root, keys, out)
    }

    /// Prepares one caller-owned first pull of the canon history (`purpose` canon_history,
    /// `root` a landing) or view history (view_history, `root` a view op) of this snapshot: it
    /// walks the history once and keeps that closure with a share of the snapshot's captured
    /// state. Its pages and gets read the closure and walk nothing again, so a pull of many gets
    /// uses the one canon tip and set of view heads that this snapshot captured. The caller
    /// owns the value for the one operation: nothing is cached in the process or on disk, and
    /// dropping it releases the captured state. A root the snapshot does not hold is
    /// `E_UNRESOLVED_REF`; another purpose is `E_STREAM_PURPOSE`. A pull above a base uses
    /// [`Snapshot::prepare_pull_with_base`]; a request that no operation owns uses
    /// [`Snapshot::history_page`] and [`Snapshot::write_history_keys`], which keep their walks.
    pub fn prepare_pull(
        &self,
        purpose: StreamPurpose,
        root: Hash,
    ) -> Result<PreparedPull, RdyError> {
        self.prepare_pull_with_base(purpose, root, None)
    }

    /// [`Snapshot::prepare_pull`] less the link closure of `base`, a landing or view op the
    /// receiver holds: one incremental pull. It walks the history once with that base, as
    /// [`Snapshot::history_page`] walks it: a base the snapshot does not hold counts as none, and
    /// the later attachments of the base's records still travel. Its pages are that walk's keys
    /// in its order; a get names keys of that selection only (another key is `E_UNRESOLVED_REF`
    /// and nothing is written), and its batch holds exactly those keys.
    pub fn prepare_pull_with_base(
        &self,
        purpose: StreamPurpose,
        root: Hash,
        base: Option<Hash>,
    ) -> Result<PreparedPull, RdyError> {
        PreparedPull::with_base(Arc::clone(self.shared_state()), purpose, root, base)
    }

    /// Prepares one caller-owned read of the state of the canon tip `root` (a landing) of this
    /// snapshot (stream format 5, D-GGTYA): the root landing, its map read through the store's
    /// map read (no walk of the history closure), the SNAPSHOT made from that map, the blocks it
    /// names, the tip's delta and its attachments, with a share of the snapshot's captured state.
    /// Its pages and gets read that one selection. The caller owns the value for the one
    /// operation; dropping it releases the captured state. A root the snapshot does not hold is
    /// `E_UNRESOLVED_REF`.
    pub fn prepare_canon_state(&self, root: Hash) -> Result<PreparedCanonState, RdyError> {
        PreparedCanonState::new(Arc::clone(self.shared_state()), root)
    }

    /// [`Snapshot::prepare_pull`] for a hub's get replies: its gets come back as the bytes that
    /// go on the wire, framed as one zstd frame when the caller accepts it.
    pub fn prepare_pull_reply(
        &self,
        purpose: StreamPurpose,
        root: Hash,
    ) -> Result<PullReply, RdyError> {
        self.prepare_pull_reply_with_base(purpose, root, None)
    }

    /// [`Snapshot::prepare_pull_with_base`] for a hub's get replies, as
    /// [`Snapshot::prepare_pull_reply`].
    pub fn prepare_pull_reply_with_base(
        &self,
        purpose: StreamPurpose,
        root: Hash,
        base: Option<Hash>,
    ) -> Result<PullReply, RdyError> {
        Ok(PullReply {
            pull: self.prepare_pull_with_base(purpose, root, base)?,
        })
    }
}

/// One prepared pull's replies: a [`PreparedPull`] that gives each get as an [`EncodedHistory`].
/// The caller owns it for the one operation, as it owns a prepared pull.
#[derive(Debug)]
pub struct PullReply {
    pull: PreparedPull,
}

impl PullReply {
    /// A page of at most `limit` keys after `after`, as [`PreparedPull::page`].
    pub fn page(&self, after: Option<ObjectKey>, limit: usize) -> Result<KeyPage, RdyError> {
        self.pull.page(after, limit)
    }

    /// The batch of exactly the records `keys` name as a get reply, as
    /// [`PreparedPull::write_keys_encoded`]: one zstd frame when `accept_zstd` and the frame is
    /// smaller than the batch, else the batch's own bytes. The report counts the batch's raw
    /// bytes; `wire_bytes` counts the reply's. Nothing is encoded for a source record that fails
    /// its checks.
    pub fn write_keys_encoded(
        &self,
        keys: &[ObjectKey],
        accept_zstd: bool,
    ) -> Result<EncodedHistory, RdyError> {
        self.pull.write_keys_encoded(keys, accept_zstd)
    }
}
