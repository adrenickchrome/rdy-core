//! The error of a sync stream (D-D2NPN): where reading or checking a stream failed, and what to
//! do about it. Shared by every stream reader, checker and take, and mapped to the engine's
//! public error there; it holds no reader state.
//!
//! Object records are numbered from 1; the header is record 0 and the trailer is record
//! `records + 1`. `offset` is the first byte of the failing frame; a failure before the magic is
//! record 0 at offset 0. `object` is the key of the failing record, when it is known.
use crate::bounded::RecordError;
use crate::object_refs::ObjectKey;
use std::fmt;

const E_BAD_RECORD: &str = "E_BAD_RECORD";
const E_UNRESOLVED_REF: &str = "E_UNRESOLVED_REF";
const E_STREAM_TRUNCATED: &str = "E_STREAM_TRUNCATED";
const E_STREAM_PURPOSE: &str = "E_STREAM_PURPOSE";
const E_STORE_FORMAT: &str = "E_STORE_FORMAT";
const E_LIMIT: &str = "E_LIMIT";
const E_IO: &str = "E_IO";

/// The limit label of a stream's total bytes.
pub const LIMIT_TOTAL_BYTES: &str = "2147483648 total bytes";
/// The limit label of a stream's object records.
pub const LIMIT_OBJECT_RECORDS: &str = "1000000 object records";
/// The limit label of a stream's dependency edges.
pub const LIMIT_GRAPH_EDGES: &str = "1000000 graph edges";
/// The limit label of a tag history's depth.
pub const LIMIT_HISTORY_DEPTH: &str = "100000 history depth";
/// The limit label of a header's prerequisites.
pub const LIMIT_PREREQUISITES: &str = "100000 prerequisites";
/// The limit label of a header's bytes.
pub const LIMIT_HEADER_BYTES: &str = "3300057 header bytes";
/// The limit label of the drops pending at once.
pub const LIMIT_PENDING_DROPS: &str = "1000000 pending drops";
/// The limit label of the dropped bytes pending at once.
pub const LIMIT_PENDING_DROPPED_BYTES: &str = "1073741824 pending dropped bytes";

/// Why a stream record is bad (E_BAD_RECORD). A closed list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    UnknownKind,
    HashMismatch,
    RecordDoesNotDecode,
    TrailingBytes,
    InvalidHeader,
    InvalidDroppedRecord,
    AmbiguousObjectKind,
    DependencyCycle,
    ObjectIsRepeated,
    NoBatchFiles,
    BatchHeaderDiffers,
    InputChangedDuringTake,
    InvalidRemovalProof,
    OutsideTagClosure,
}

impl Reason {
    /// The reason as the message writes it.
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::UnknownKind => "unknown kind",
            Reason::HashMismatch => "hash mismatch",
            Reason::RecordDoesNotDecode => "record does not decode",
            Reason::TrailingBytes => "trailing bytes",
            Reason::InvalidHeader => "invalid header",
            Reason::InvalidDroppedRecord => "invalid dropped record",
            Reason::AmbiguousObjectKind => "ambiguous object kind",
            Reason::DependencyCycle => "dependency cycle",
            Reason::ObjectIsRepeated => "object is repeated",
            Reason::NoBatchFiles => "no batch files",
            Reason::BatchHeaderDiffers => "batch header differs",
            Reason::InputChangedDuringTake => "input changed during take",
            Reason::InvalidRemovalProof => "invalid removal proof",
            Reason::OutsideTagClosure => "object is outside the tag closure",
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A stream failure: its code, message and fix, and where it happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamError {
    pub code: &'static str,
    pub message: String,
    pub fix: String,
    pub record: Option<u64>,
    pub offset: u64,
    pub object: Option<ObjectKey>,
}

impl StreamError {
    fn new(
        code: &'static str,
        message: String,
        fix: &str,
        record: Option<u64>,
        offset: u64,
    ) -> Self {
        StreamError {
            code,
            message,
            fix: fix.to_string(),
            record,
            offset,
            object: None,
        }
    }

    /// This error, naming `object` as the failing record's key.
    pub fn with_object(mut self, object: Option<ObjectKey>) -> Self {
        self.object = object;
        self
    }

    /// E_BAD_RECORD: record `record`, whose frame starts at byte `offset`, is bad for `reason`.
    pub fn bad_record(record: u64, offset: u64, reason: Reason) -> Self {
        Self::new(
            E_BAD_RECORD,
            format!("stream record {record} at byte {offset}: {reason}"),
            "request this stream again; run rdy fsck at its source",
            Some(record),
            offset,
        )
    }

    /// E_UNRESOLVED_REF: record `record` needs object `missing`, which the stream does not hold.
    pub fn unresolved(record: u64, offset: u64, missing: ObjectKey) -> Self {
        Self::new(
            E_UNRESOLVED_REF,
            format!("stream record {record} needs {missing}"),
            "send the missing object with its dependencies",
            Some(record),
            offset,
        )
    }

    /// E_STREAM_TRUNCATED: the trailer is missing, of another length, or does not match.
    pub fn truncated(record: Option<u64>, offset: u64) -> Self {
        Self::new(
            E_STREAM_TRUNCATED,
            "stream trailer is missing or does not match".to_string(),
            "request the complete stream again",
            record,
            offset,
        )
    }

    /// E_STREAM_PURPOSE: the header's purpose byte `actual` (printed in decimal) is not
    /// tag_history.
    pub fn purpose(record: Option<u64>, offset: u64, actual: u8) -> Self {
        Self::new(
            E_STREAM_PURPOSE,
            format!("stream purpose {actual} does not match tag_history"),
            "request a tag_history stream",
            record,
            offset,
        )
    }

    /// E_STORE_FORMAT: the stream's store format `actual` is not this build's `current`.
    pub fn store_format(record: Option<u64>, offset: u64, actual: u32, current: u32) -> Self {
        Self::new(
            E_STORE_FORMAT,
            format!("stream store format {actual} does not match {current}"),
            "write the stream with the pinned rdy build",
            record,
            offset,
        )
    }

    /// E_LIMIT: the stream exceeds `label`, a bound followed by its name (such as
    /// [`LIMIT_TOTAL_BYTES`] or a record limit's display).
    pub fn limit(record: Option<u64>, offset: u64, label: &str) -> Self {
        Self::new(
            E_LIMIT,
            format!("stream exceeds {label}"),
            "reduce the stream to the stated limit",
            record,
            offset,
        )
    }

    /// E_LIMIT: a bounded buffer could not be reserved.
    pub fn allocation(record: Option<u64>, offset: u64) -> Self {
        Self::new(
            E_LIMIT,
            "stream allocation failed".to_string(),
            "retry with more available memory",
            record,
            offset,
        )
    }

    /// E_IO: reading the stream failed with `error`.
    pub fn io(record: Option<u64>, offset: u64, error: &std::io::Error) -> Self {
        Self::new(
            E_IO,
            format!("stream input: {error}"),
            "check that the stream file can be read",
            record,
            offset,
        )
    }

    /// The stream error of record `record` at `offset` that failed its record check with `e`:
    /// `Kind` is an unknown kind, `Decode` a record that does not decode, `Limit` E_LIMIT with the
    /// limit's bound and name.
    pub fn from_record(record: u64, offset: u64, e: &RecordError) -> Self {
        match e {
            RecordError::Kind => Self::bad_record(record, offset, Reason::UnknownKind),
            RecordError::Decode => Self::bad_record(record, offset, Reason::RecordDoesNotDecode),
            RecordError::Limit { .. } => Self::limit(Some(record), offset, &e.to_string()),
        }
    }
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for StreamError {}
