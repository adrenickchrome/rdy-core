use rdy_schema::Issue;
use rdy_store::StoreError;
use serde::Serialize;
use std::fmt;

pub const E_LOCKED: &str = "E_LOCKED";
pub const E_NO_PROJECT: &str = "E_NO_PROJECT";
pub const E_CONFIG: &str = "E_CONFIG";
pub const E_PERMISSION: &str = "E_PERMISSION";
/// A dismissal of a blocking tension by the actor who wrote one of its sides (plan M2d2).
pub const E_SELF_REVIEW: &str = "E_SELF_REVIEW";
/// A review verdict fails its schema (plan M2e1).
pub const E_REVIEW_INVALID: &str = "E_REVIEW_INVALID";
/// A review's case hash is no longer current, or a verdict already settled the request (plan M2e1).
pub const E_REVIEW_STALE: &str = "E_REVIEW_STALE";
/// An agent session wrote into a view another session owns.
pub const E_VIEW_OWNER: &str = "E_VIEW_OWNER";
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
/// Two blocks would hold the same key (compared ignoring case).
pub const E_KEY_CLASH: &str = "E_KEY_CLASH";
/// A view of that name exists.
pub const E_VIEW_EXISTS: &str = "E_VIEW_EXISTS";
/// The view is closed: it takes no commit, include, exclude, undo (but the undo of its close)
/// or propose; or an approval, a landing or an advice acceptance names a proposal version that a
/// close of its view withdrew (D-K0SS1).
pub const E_VIEW_CLOSED: &str = "E_VIEW_CLOSED";
/// The proposal version named is not its latest.
pub const E_PROPOSAL_MOVED: &str = "E_PROPOSAL_MOVED";
/// An approval names a version whose exceptions have no confirm answer (F2); `corrected` holds
/// each exception's fresh card.
pub const E_EXCEPTION_OPEN: &str = "E_EXCEPTION_OPEN";
/// An answer names a queue item that changed or left the queue since its card was shown;
/// `corrected` holds the fresh card when the item has one.
pub const E_ITEM_MOVED: &str = "E_ITEM_MOVED";
/// Changes in the view depend on the change being excluded (each one is an issue).
pub const E_HAS_DEPENDENTS: &str = "E_HAS_DEPENDENTS";
/// A subset leaves out changes its members depend on (each one is an issue).
pub const E_DEPS_MISSING: &str = "E_DEPS_MISSING";
/// The version being landed carries conflicts it has not resolved (each one is an issue).
pub const E_CONFLICT_UNRESOLVED: &str = "E_CONFLICT_UNRESOLVED";
/// The landing would put blocking, undismissed tensions into canon (each one is an issue).
pub const E_BLOCKING_TENSION: &str = "E_BLOCKING_TENSION";
/// An undo would leave the view inconsistent (each blocking op is an issue).
pub const E_UNDO_BLOCKED: &str = "E_UNDO_BLOCKED";
/// An undo would bring back or remove work that has landed.
pub const E_UNDO_LANDING: &str = "E_UNDO_LANDING";
/// The session has no op of its own on the view that can be undone.
pub const E_NOTHING_TO_UNDO: &str = "E_NOTHING_TO_UNDO";
/// A page cursor is unknown, or what it pages (a briefing, a read, a search) moved since its first page.
pub const E_PAGE_STALE: &str = "E_PAGE_STALE";
/// A proposal from a member of an open alternative set names no chosen member (plan M2d1).
pub const E_ALT_UNCHOSEN: &str = "E_ALT_UNCHOSEN";
/// A view is not a member of the alternative set, or a member names another member (plan M2d1).
pub const E_ALT_MEMBER: &str = "E_ALT_MEMBER";
/// A proposal carries a change of a member that lost a chosen alternative set, and records no decision that supersedes or refines the choice (FW-AL1).
pub const E_ALT_CHOSEN: &str = "E_ALT_CHOSEN";
/// The intent has expired: it takes no renewal and no withdrawal (plan M2d1).
pub const E_INTENT_EXPIRED: &str = "E_INTENT_EXPIRED";
/// The fix of every `E_PAGE_STALE`, for a briefing, read and search cursor alike.
pub const PAGE_STALE_FIX: &str = "the read, briefing or search moved; start again without a cursor";

#[derive(Debug, Clone, Serialize)]
pub struct RdyError {
    pub code: &'static str,
    pub message: String,
    pub fix: String,
    pub issues: Vec<Issue>,
    /// A corrected payload the caller can resend, where rdy can compute one safely.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corrected: Option<serde_json::Value>,
    /// bwr1: a briefing refusal's ids to brief (`{"ids": [...]}`), replaced by the fresh
    /// briefing itself once a tool expands the error (`brief::expand_error`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub briefing: Option<serde_json::Value>,
}

impl RdyError {
    pub fn new(code: &'static str, message: impl Into<String>, fix: impl Into<String>) -> RdyError {
        RdyError {
            code,
            message: message.into(),
            fix: fix.into(),
            issues: vec![],
            corrected: None,
            briefing: None,
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
                    "the project store at {} is held by another rdy process (the daemon, or a command run with --no-daemon)",
                    p.display()
                ),
                "drop --no-daemon, or stop the daemon with rdy serve --stop",
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
            StoreError::DeletedSince(id) => RdyError::new(
                E_STALE_BASE,
                format!(
                    "{id} was deleted in canon after the base this view reads, so this write would bring it back without having seen that delete"
                ),
                format!(
                    "to keep {id}, re-create it from a view that reads canon (a new view, or one with nothing to land); otherwise drop the change that creates it"
                ),
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
            StoreError::ViewExists(v) => RdyError::new(
                E_VIEW_EXISTS,
                format!("view {v:?} exists"),
                format!("pick a free name, or fork {v} under another name"),
            ),
            StoreError::ViewClosed(v) => RdyError::new(
                E_VIEW_CLOSED,
                format!("view {v:?} is closed"),
                format!(
                    "reopen it with rdy undo --view {v} <close op> (rdy view show {v} lists the ops), or fork it"
                ),
            ),
            StoreError::ProposalMoved { stable, latest } => RdyError::new(
                E_PROPOSAL_MOVED,
                format!(
                    "proposal P-{} moved: its latest version is {}",
                    stable.short(),
                    latest.short()
                ),
                format!(
                    "review the latest version, then approve it as P-{}@<n>",
                    stable.short()
                ),
            ),
            StoreError::EmptyProposal(v) => RdyError::new(
                E_BAD_INPUT,
                format!("a proposal of view {v:?} needs at least one change"),
                format!("rdy view show {v} lists its changes; write to the view first"),
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
            StoreError::ParentCycle { message, fix } => RdyError::new(E_BAD_INPUT, message, fix),
            StoreError::Dump { at, what } => RdyError::new(
                E_DUMP,
                format!("the dump is damaged at byte {at}: {what}"),
                "dump the project again; rdy never repairs a dump",
            ),
            StoreError::Refused(message) => RdyError::new(
                E_BAD_INPUT,
                message,
                "read the current state again and retry against it",
            ),
            StoreError::UndoTooDeep { view, op } => RdyError::new(
                E_LIMIT,
                format!(
                    "view {view:?}: op {} ends a chain of more than 64 undos of undos",
                    op.short()
                ),
                format!(
                    "include or exclude the change directly instead: rdy view include|exclude {view} <change> (MCP: rdy_view include/exclude)"
                ),
            ),
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

/// This directory is not a working copy: no view is checked out here.
pub const E_WC_NOT_BOUND: &str = "E_WC_NOT_BOUND";
/// The working copy holds edits that the command would overwrite.
pub const E_WC_DIRTY: &str = "E_WC_DIRTY";
/// A file a person made sits where checkout wants to write.
pub const E_WC_OCCUPIED: &str = "E_WC_OCCUPIED";
/// The working copy is behind its view: update it first.
pub const E_WC_STALE: &str = "E_WC_STALE";
/// The working copy holds conflicts that are not resolved.
pub const E_WC_CONFLICT: &str = "E_WC_CONFLICT";
/// The files cannot be committed as they are (each problem is an issue).
pub const E_WC_INVALID: &str = "E_WC_INVALID";
/// Another rdy command is using this working copy.
pub const E_WC_LOCKED: &str = "E_WC_LOCKED";
/// Diagnostic: a supporting block (a decision, a question) was put in a doc file.
pub const E_WC_SUPPORTING: &str = "E_WC_SUPPORTING";
/// Diagnostic: a file holds a field or link that lives only in the store.
pub const E_WC_STORE_ONLY: &str = "E_WC_STORE_ONLY";
/// Diagnostic: a file in the working tree is not tracked.
pub const W_WC_UNTRACKED: &str = "W_WC_UNTRACKED";
/// Diagnostic: a block moved into a file that is not staged.
pub const W_WC_UNSTAGED_MOVE: &str = "W_WC_UNSTAGED_MOVE";
/// Diagnostic: the commit's write-back left a path as it was, because it holds what the commit
/// did not record (a file rdy does not track, or later edits).
pub const W_WC_KEPT: &str = "W_WC_KEPT";

/// The live site is not available to this session: it runs without a daemon, or the daemon's
/// site is off.
pub const E_SITE: &str = "E_SITE";
/// The live site is open to the network (`serve.site_lan`), or could not be limited to this
/// machine as asked.
pub const W_SITE_LAN: &str = "W_SITE_LAN";

/// A file that is not a dump this rdy reads, a damaged dump, or a loaded project that fails fsck.
pub const E_DUMP: &str = "E_DUMP";
