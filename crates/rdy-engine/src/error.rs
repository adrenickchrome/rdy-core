use rdy_schema::Issue;
use rdy_store::StoreError;
use serde::Serialize;
use std::fmt;

pub const E_LOCKED: &str = "E_LOCKED";
pub const E_NO_PROJECT: &str = "E_NO_PROJECT";
pub const E_CONFIG: &str = "E_CONFIG";
pub const E_PERMISSION: &str = "E_PERMISSION";
pub const E_BAD_INPUT: &str = "E_BAD_INPUT";
pub const E_VALIDATION: &str = "E_VALIDATION";
pub const E_UNKNOWN_REF: &str = "E_UNKNOWN_REF";
pub const E_UNKNOWN_BLOCK: &str = "E_UNKNOWN_BLOCK";
pub const E_ALREADY_EXISTS: &str = "E_ALREADY_EXISTS";
pub const E_STALE_BASE: &str = "E_STALE_BASE";
pub const E_FROZEN: &str = "E_FROZEN";
pub const E_HAS_CHILDREN: &str = "E_HAS_CHILDREN";
pub const E_NOT_FOUND: &str = "E_NOT_FOUND";
pub const E_IO: &str = "E_IO";
pub const E_RESERVED_VIEW: &str = "E_RESERVED_VIEW";
pub const E_TAG_EXISTS: &str = "E_TAG_EXISTS";
pub const E_BAD_TAG_NAME: &str = "E_BAD_TAG_NAME";
pub const E_VERSION_ORDER: &str = "E_VERSION_ORDER";
pub const E_BAD_NAME: &str = "E_BAD_NAME";
pub const E_PATH_CLASH: &str = "E_PATH_CLASH";
pub const E_FILE_FORMAT: &str = "E_FILE_FORMAT";
pub const E_ALREADY_LANDED: &str = "E_ALREADY_LANDED";
/// No order key fits between two siblings within `MAX_ORDER_KEY_LEN`.
pub const E_ORDER_DEPTH: &str = "E_ORDER_DEPTH";
/// The store was written by a newer rdy, or its FORMAT file is unreadable.
pub const E_STORE_FORMAT: &str = "E_STORE_FORMAT";
/// A tool panicked.
pub const E_INTERNAL: &str = "E_INTERNAL";
/// Input over a hard limit.
pub const E_LIMIT: &str = "E_LIMIT";

#[derive(Debug, Clone, Serialize)]
pub struct RdyError {
    pub code: &'static str,
    pub message: String,
    pub fix: String,
    pub issues: Vec<Issue>,
    /// A corrected payload the caller can resend, where rdy can compute one safely.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corrected: Option<serde_json::Value>,
}

impl RdyError {
    pub fn new(code: &'static str, message: impl Into<String>, fix: impl Into<String>) -> RdyError {
        RdyError {
            code,
            message: message.into(),
            fix: fix.into(),
            issues: vec![],
            corrected: None,
        }
    }

    pub fn with_corrected(mut self, v: serde_json::Value) -> RdyError {
        self.corrected = Some(v);
        self
    }
}

impl fmt::Display for RdyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} (fix: {})", self.code, self.message, self.fix)
    }
}

impl std::error::Error for RdyError {}

impl From<StoreError> for RdyError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::Locked(p) => RdyError::new(
                E_LOCKED,
                format!(
                    "the project store at {} is open in another rdy process",
                    p.display()
                ),
                "close the other rdy process (or MCP server) and retry",
            ),
            StoreError::HeldByReader(p) => RdyError::new(
                E_LOCKED,
                format!(
                    "the project store at {} is still open in this process through a read state of an earlier open",
                    p.display()
                ),
                "drop every read state (and store reader) of the earlier open, then reopen",
            ),
            StoreError::StaleBase {
                id,
                expected,
                actual: Some(actual),
            } => RdyError::new(
                E_STALE_BASE,
                format!(
                    "{id} changed since you read it: your base is {}, current is {}",
                    expected.short(),
                    actual.short()
                ),
                format!("re-read {id} (rdy_context) and retry with base={actual}"),
            ),
            StoreError::StaleBase {
                id,
                expected,
                actual: None,
            } => RdyError::new(
                E_STALE_BASE,
                format!(
                    "{id} was deleted since you read it (your base was {})",
                    expected.short()
                ),
                format!("{id} was deleted since you read it; recreate it or drop this op"),
            ),
            StoreError::Frozen(id) => RdyError::new(
                E_FROZEN,
                format!("{id} is an accepted or rejected decision and cannot change"),
                format!("write a new decision that supersedes or revokes {id}"),
            ),
            StoreError::UnknownBlock(id) => RdyError::new(
                E_UNKNOWN_BLOCK,
                format!("no block {id}"),
                "check the id with rdy_read or rdy_overview (rdy ls on the command line)",
            ),
            StoreError::AlreadyExists(id) => RdyError::new(
                E_ALREADY_EXISTS,
                format!("block {id} already exists"),
                "omit id to let rdy generate one",
            ),
            StoreError::NotFound(h) => RdyError::new(
                E_NOT_FOUND,
                format!("object {} not found", h.short()),
                "check the hash",
            ),
            StoreError::UnknownView(v) => RdyError::new(
                E_NOT_FOUND,
                format!("no view {v:?}"),
                "check the view name with rdy status",
            ),
            StoreError::Conflict { id } => RdyError::new(
                E_STALE_BASE,
                format!("conflict on {id}"),
                format!(
                    "re-read {id} (rdy_context) and retry your change against its current version"
                ),
            ),
            StoreError::MissingFrames { segment, detail } => RdyError::new(
                E_IO,
                format!("the store's log is missing acknowledged writes: {detail}"),
                format!(
                    "restore {} from backup, then reopen; rdy will not rebuild the index over the loss",
                    segment.display()
                ),
            ),
            StoreError::Format { message, fix } => RdyError::new(E_STORE_FORMAT, message, fix),
            StoreError::Io(e) if e.kind() == std::io::ErrorKind::InvalidData => RdyError::new(
                E_IO,
                format!("i/o error: {e}"),
                "the store's log is damaged where the message says: move that segment aside or restore it from backup, then reopen",
            ),
            e @ (StoreError::Io(_) | StoreError::Db(_) | StoreError::Decode(_)) => RdyError::new(
                E_IO,
                e.to_string(),
                "run rdy fsck; if the index is damaged run rdy reindex",
            ),
        }
    }
}
