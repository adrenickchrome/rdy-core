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
    #[error("store at {0} is already open in another process")]
    Locked(PathBuf),
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
    #[error("conflict on block {id}")]
    Conflict { id: BlockId },
    #[error("block {0} is a frozen decision and cannot change")]
    Frozen(BlockId),
    #[error("acknowledged writes are missing from the log ({}): {detail}", .segment.display())]
    MissingFrames { segment: PathBuf, detail: String },
}

pub(crate) fn db<E: std::fmt::Display>(e: E) -> StoreError {
    StoreError::Db(e.to_string())
}
