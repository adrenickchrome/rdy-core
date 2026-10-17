use rdy_model::{BlockId, Hash};
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// An I/O failure. `From<io::Error>` turns a missing payload a read path carried inside an
    /// `io::Error` (`segment::UnauthorizedHole`) into `RedactionCorrupt` instead.
    #[error("i/o error: {0}")]
    Io(#[source] std::io::Error),
    #[error("index error: {0}")]
    Db(String),
    #[error("cannot decode object: {0}")]
    Decode(String),
    /// A write the store's checks refuse (a verdict, a semantic tension version or an inbox
    /// subscription that breaks a rule); nothing was written. Not damage: `Decode` is a stored
    /// object or ref that does not decode.
    #[error("{0}")]
    Refused(String),
    #[error("object {0} not found")]
    NotFound(Hash),
    #[error("store at {0} is locked by another process")]
    Locked(PathBuf),
    /// The store's lock was free but its index is still open: a `StoreReader` (or a state built
    /// on one) of an earlier open in this process outlived its store.
    #[error("store at {0} is still open through a reader of an earlier open in this process")]
    HeldByReader(PathBuf),
    #[error("stale base for {id}: change was based on {expected}, current is {actual:?}")]
    StaleBase {
        id: BlockId,
        expected: Hash,
        actual: Option<Hash>,
    },
    #[error("unknown block {0}")]
    UnknownBlock(BlockId),
    #[error("block {0} already exists")]
    AlreadyExists(BlockId),
    #[error("unknown view {0:?}")]
    UnknownView(String),
    #[error("view {0:?} exists")]
    ViewExists(String),
    #[error("view {0:?} is closed")]
    ViewClosed(String),
    /// The proposal version named is not its stable's latest (`proposal/<stable>`).
    #[error("proposal {stable} moved to version {latest}")]
    ProposalMoved { stable: Hash, latest: Hash },
    /// A proposal version with no changes: it would land as an empty landing, again and again.
    #[error("a proposal of view {0:?} needs at least one change")]
    EmptyProposal(String),
    #[error("conflict on block {id}")]
    Conflict { id: BlockId },
    #[error("block {0} is a frozen decision and cannot change")]
    Frozen(BlockId),
    /// A create of an id canon removed after the base the writer read: the write never saw that
    /// delete, so it does not undo it.
    #[error("block {0} was deleted in canon after the base this write read")]
    DeletedSince(BlockId),
    #[error("acknowledged writes are missing from the log ({}): {detail}", .segment.display())]
    MissingFrames { segment: PathBuf, detail: String },
    /// The store's format is not one this rdy can open (written by a newer rdy, or its FORMAT
    /// file is unreadable).
    #[error("{message}")]
    Format { message: String, fix: String },
    /// An undo that ends a chain of more than `MAX_UNDO_DEPTH` undos of undos: the store is
    /// sound, the walk refuses to follow it.
    #[error("view {view:?}: op {op} ends a chain of more than 64 undos of undos")]
    UndoTooDeep { view: String, op: Hash },
    /// A write that combines moves from different changes (a fork's or a landing's rebase)
    /// would make two blocks each other's ancestors; nothing was written.
    #[error("{message}")]
    ParentCycle { message: String, fix: String },
    /// A file that is not a dump this rdy reads, or a damaged dump: `at` is the byte offset of
    /// the record that failed, `what` says how (`crate::dump`).
    #[error("dump damaged at byte {at}: {what}")]
    Dump { at: u64, what: String },
    /// The object's content was removed by redaction `request` (E_REDACTED): a raw read of its
    /// payload, a blob with a removed chunk, or a write that would bring an erased hash back.
    #[error("Content removed by redaction {request}.")]
    Redacted { request: Hash },
    /// A reader captured a redaction epoch older than the store's (E_REDACTED_STATE).
    #[error("This read state predates content removal.")]
    RedactedState,
    /// A content removal without a person's approval (E_REDACTION_HUMAN).
    #[error("Content removal needs a person's approval.")]
    RedactionHuman,
    /// An authorized removal that has not finished (E_REDACTION_PENDING).
    #[error("Content removal was authorized but has not finished.")]
    RedactionPending,
    /// The spans or sources a request binds changed since it was prepared (E_REDACTION_STALE).
    #[error("The redaction report changed after the request was recorded.")]
    RedactionStale,
    /// A stored or prepared redaction record that does not authorize the payload at this frame
    /// location (E_REDACTION_CORRUPT): stored proof damage names the offending frame, a malformed
    /// preparation names its claimed span.
    #[error("The redaction record does not authorize the missing payload at {segment}:{offset}.")]
    RedactionCorrupt { segment: u32, offset: u64 },
    /// A redaction over one of the fixed bounds (E_REDACTION_LIMIT); `limit` is one of
    /// `crate::redaction::limit::ALL`.
    #[error("The redaction exceeds {limit}.")]
    RedactionLimit { limit: &'static str },
    /// A read projection (a stub or a residual change) offered to a writer (E_BAD_INPUT).
    #[error("A read projection cannot be written.")]
    ProjectedWrite,
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> StoreError {
        if let Some(crate::segment::UnauthorizedHole(at)) = e
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<crate::segment::UnauthorizedHole>())
        {
            return StoreError::corrupt_at(*at);
        }
        StoreError::Io(e)
    }
}

impl StoreError {
    /// The corrupt-span refusal at frame location `at`.
    pub(crate) fn corrupt_at(at: crate::segment::Location) -> StoreError {
        StoreError::RedactionCorrupt {
            segment: at.segment,
            offset: at.offset,
        }
    }
}

pub(crate) fn db<E: std::fmt::Display>(e: E) -> StoreError {
    StoreError::Db(e.to_string())
}
