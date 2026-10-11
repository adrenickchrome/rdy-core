use rdy_model::{BlockId, Hash};
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("index error: {0}")]
    Db(String),
    #[error("cannot decode object: {0}")]
    Decode(String),
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
}

pub(crate) fn db<E: std::fmt::Display>(e: E) -> StoreError {
    StoreError::Db(e.to_string())
}
