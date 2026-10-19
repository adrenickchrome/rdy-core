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
/// A sync stream record is bad (D-D2NPN; `stream_error`).
pub const E_BAD_RECORD: &str = "E_BAD_RECORD";
/// A sync stream record needs an object the stream does not hold.
pub const E_UNRESOLVED_REF: &str = "E_UNRESOLVED_REF";
/// A sync stream's trailer is missing or does not match.
pub const E_STREAM_TRUNCATED: &str = "E_STREAM_TRUNCATED";
/// A sync stream's purpose is not the one asked for.
pub const E_STREAM_PURPOSE: &str = "E_STREAM_PURPOSE";
/// A sync stream or batch belongs to another repo.
pub const E_REPO_MISMATCH: &str = "E_REPO_MISMATCH";
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

/// A redaction target that is not a block id, `id@full-version-hash` or `blob:full-manifest-hash`,
/// or that names nothing removable (plan Redaction). Built by [`redaction_target`].
pub const E_REDACTION_TARGET: &str = "E_REDACTION_TARGET";
/// A redaction over one of its fixed bounds. Built by [`redaction_limit`].
pub const E_REDACTION_LIMIT: &str = "E_REDACTION_LIMIT";
/// A redaction without a valid reason class or a reason of 1 to 4096 UTF-8 bytes.
pub const E_REDACTION_REASON: &str = "E_REDACTION_REASON";
/// The report a redaction request binds changed after the request was recorded.
pub const E_REDACTION_STALE: &str = "E_REDACTION_STALE";
/// A content removal without a person's approval: policy and agent-only approval refuse.
pub const E_REDACTION_HUMAN: &str = "E_REDACTION_HUMAN";
/// An authorized content removal that has not finished.
pub const E_REDACTION_PENDING: &str = "E_REDACTION_PENDING";
/// A missing payload that no stored redaction record authorizes.
pub const E_REDACTION_CORRUPT: &str = "E_REDACTION_CORRUPT";
/// A read state taken before a content removal.
pub const E_REDACTED_STATE: &str = "E_REDACTED_STATE";
/// Content a redaction removed.
pub const E_REDACTED: &str = "E_REDACTED";
/// An output budget too small for the redaction warning.
pub const E_REDACTION_BUDGET: &str = "E_REDACTION_BUDGET";

/// The fix of `E_REDACTION_TARGET`.
pub const REDACTION_TARGET_FIX: &str =
    "Use a block id, id@full-version-hash, or blob:full-manifest-hash.";
/// The fix of `E_REDACTION_LIMIT`.
pub const REDACTION_LIMIT_FIX: &str = "Select fewer targets, then run the dry run again.";
/// The message of `E_REDACTION_REASON`.
pub const REDACTION_REASON_MESSAGE: &str = "The redaction needs a reason and a reason class.";
/// The fix of `E_REDACTION_REASON`.
pub const REDACTION_REASON_FIX: &str =
    "Use secret, personal, legal, or retraction, with a reason of 1 to 4096 UTF-8 bytes.";
/// The fix of `E_REDACTION_STALE`.
pub const REDACTION_STALE_FIX: &str =
    "Record a new request, show its report, and ask for approval again.";
/// The fix of `E_REDACTION_HUMAN`.
pub const REDACTION_HUMAN_FIX: &str =
    "Show the redaction card and relay the person's answer, or approve from the CLI as a human.";
/// The fix of `E_REDACTION_PENDING`.
pub const REDACTION_PENDING_FIX: &str =
    "Close the project and reopen it to finish the removal. Do not delete the log.";
/// The fix of `E_REDACTION_CORRUPT`.
pub const REDACTION_CORRUPT_FIX: &str = "Keep the store unchanged and inspect it with rdy fsck.";
/// The fix of `E_REDACTED_STATE`.
pub const REDACTED_STATE_FIX: &str = "Read the project again.";
/// The fix of `E_REDACTED`.
pub const REDACTED_FIX: &str = "Read the redaction details in the source history.";
/// The message of `E_REDACTION_BUDGET`.
pub const REDACTION_BUDGET_MESSAGE: &str =
    "The output budget is too small for the redaction warning.";
/// The fix of `E_REDACTION_BUDGET`.
pub const REDACTION_BUDGET_FIX: &str =
    "Increase max_tokens, or propose fewer redaction requests at once.";

/// The causes of `E_REDACTION_TARGET`, exactly.
pub const REDACTION_CAUSE_INVALID: &str = "invalid selector";
pub const REDACTION_CAUSE_UNKNOWN: &str = "unknown target";
pub const REDACTION_CAUSE_OTHER_BLOCK: &str = "version belongs to another block";
pub const REDACTION_CAUSE_NOT_BLOB: &str = "target is not a blob manifest";
pub const REDACTION_CAUSE_METADATA: &str = "metadata is not content";

/// The redaction warnings, in their report order; each has an empty fix.
pub const W_REDACTION_COPIES: &str = "W_REDACTION_COPIES";
pub const W_REDACTION_INVENTORY: &str = "W_REDACTION_INVENTORY";
pub const W_REDACTION_QUOTES: &str = "W_REDACTION_QUOTES";
pub const W_REDACTION_REUSE: &str = "W_REDACTION_REUSE";
/// Only with a nonempty quarantine list.
pub const W_REDACTION_QUARANTINE: &str = "W_REDACTION_QUARANTINE";
/// Only for reason class secret.
pub const W_REDACTION_SECRET: &str = "W_REDACTION_SECRET";
pub const W_REDACTION_COPIES_MESSAGE: &str = "This removes bytes from this store. It cannot erase exports, renders, dumps, clones, published files, working copies, backups, or filesystem snapshots.";
pub const W_REDACTION_INVENTORY_MESSAGE: &str = "Dumps, clones, exports, renders, and published versions may hold copies. Their locations are unknown to rdy.";
pub const W_REDACTION_QUOTES_MESSAGE: &str = "Quote matches use complete source strings. Partial quotes may still hold content. Snapshot issue lists and summaries, review findings, tension advice, and answer notes are not erased.";
pub const W_REDACTION_REUSE_MESSAGE: &str = "This store will refuse these object and chunk hashes on later writes. This can block an identical restore or a file that shares a removed chunk.";
pub const W_REDACTION_QUARANTINE_MESSAGE: &str = "Quarantine files may hold the content. Redaction does not remove them. A person can run rdy fsck --clean-quarantine.";
pub const W_REDACTION_SECRET_MESSAGE: &str =
    "Rotate or revoke the secret. Redaction does not undo its exposure.";

/// A public publication profile whose selection references blocks outside it (D-0VCQ5): its
/// output is blocked.
pub const E_PUBLICATION_REFERENCE: &str = "E_PUBLICATION_REFERENCE";
/// An org publication profile whose selection references blocks outside it (D-0VCQ5): a warning
/// on its output.
pub const W_PUBLICATION_REFERENCE: &str = "W_PUBLICATION_REFERENCE";

/// `E_REDACTION_TARGET` for `target` (its normalized spelling, or `[]` for no targets) and one of
/// the `REDACTION_CAUSE_*` texts.
pub fn redaction_target(target: &str, cause: &str) -> RdyError {
    RdyError::new(
        E_REDACTION_TARGET,
        format!("Cannot redact {target}: {cause}."),
        REDACTION_TARGET_FIX,
    )
}

/// `E_REDACTION_LIMIT` for one of `rdy_store::redaction::limit::ALL`.
pub fn redaction_limit(limit: &str) -> RdyError {
    RdyError::new(
        E_REDACTION_LIMIT,
        format!("The redaction exceeds {limit}."),
        REDACTION_LIMIT_FIX,
    )
}

/// `E_REDACTION_REASON`.
pub fn redaction_reason() -> RdyError {
    RdyError::new(
        E_REDACTION_REASON,
        REDACTION_REASON_MESSAGE,
        REDACTION_REASON_FIX,
    )
}

/// `E_REDACTION_BUDGET`.
pub fn redaction_budget() -> RdyError {
    RdyError::new(
        E_REDACTION_BUDGET,
        REDACTION_BUDGET_MESSAGE,
        REDACTION_BUDGET_FIX,
    )
}

/// A signing act with no configured key: `signing.key` (a person) or `signing.agent_key` (an
/// agent) is unset. Built by [`signing_key_missing`].
pub const E_SIGNING_KEY_MISSING: &str = "E_SIGNING_KEY_MISSING";
/// A signing key of a type other than `ssh-ed25519` and `sk-ssh-ed25519@openssh.com`. Built by
/// [`signing_key_type`].
pub const E_SIGNING_KEY_TYPE: &str = "E_SIGNING_KEY_TYPE";
/// The caller's SSH agent could not sign; the cause is one of [`NoAgentCause`]. Built by
/// [`no_agent`].
pub const E_NO_AGENT: &str = "E_NO_AGENT";
/// A stored signature that is malformed or fails its cryptographic check. Built by
/// [`sig_invalid`].
pub const E_SIG_INVALID: &str = "E_SIG_INVALID";
/// A valid signature whose statement does not match its subject. Built by
/// [`sig_object_mismatch`].
pub const E_SIG_OBJECT_MISMATCH: &str = "E_SIG_OBJECT_MISMATCH";
/// A valid signature by a key that the allowed-signers file does not trust for its principal and
/// namespace. Built by [`sig_unknown_key`].
pub const W_SIG_UNKNOWN_KEY: &str = "W_SIG_UNKNOWN_KEY";
/// An act saved without its automatic signatures. Built by [`signing_failed`].
pub const W_SIGNING_FAILED: &str = "W_SIGNING_FAILED";

/// The fix of `E_SIGNING_KEY_TYPE`.
pub const SIGNING_KEY_TYPE_FIX: &str = "set an ssh-ed25519 or sk-ssh-ed25519@openssh.com key";
/// The fix of `E_NO_AGENT`.
pub const NO_AGENT_FIX: &str = "check SSH_AUTH_SOCK and the configured key, then try again";
/// The fix of `E_SIG_INVALID`.
pub const SIG_INVALID_FIX: &str = "check the source of this signature; it may be damaged or forged";
/// The fix of `E_SIG_OBJECT_MISMATCH`.
pub const SIG_OBJECT_MISMATCH_FIX: &str = "run rdy sign for this subject, or refuse the record";
/// The fix of `W_SIG_UNKNOWN_KEY`.
pub const SIG_UNKNOWN_KEY_FIX: &str = "add the key and principal to signing.allowed_signers";
/// The fix of `W_SIGNING_FAILED`.
pub const SIGNING_FAILED_FIX: &str = "fix the signing error, then run rdy sign";
/// The fix of the signing `E_LIMIT` ([`signing_limit`]).
pub const SIGNING_LIMIT_FIX: &str = "reduce the input to the stated limit";

/// Why the caller's SSH agent could not sign: the closed list of `E_NO_AGENT` causes. Every
/// surface reports one of these texts and never an agent's own comment, path or error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NoAgentCause {
    /// The calling process has no `SSH_AUTH_SOCK`.
    Unset,
    /// The agent socket could not be reached, or it closed before a whole reply.
    ConnectionFailed,
    /// The agent holds no identity for the selected key.
    NotHeld,
    /// The agent answered with a failure (reply type 5, 30 or 102).
    Refused,
    /// The agent's reply is malformed, of the wrong type, or over a decoded bound.
    InvalidReply,
    /// The agent's reply packet is over 262144 bytes.
    ReplyTooLarge,
    /// The caller cancelled the request.
    Cancelled,
}

impl NoAgentCause {
    /// Every cause, in the order the error table lists them.
    pub const ALL: [NoAgentCause; 7] = [
        NoAgentCause::Unset,
        NoAgentCause::ConnectionFailed,
        NoAgentCause::NotHeld,
        NoAgentCause::Refused,
        NoAgentCause::InvalidReply,
        NoAgentCause::ReplyTooLarge,
        NoAgentCause::Cancelled,
    ];

    /// The exact cause text.
    pub fn as_str(self) -> &'static str {
        match self {
            NoAgentCause::Unset => "SSH_AUTH_SOCK is not set",
            NoAgentCause::ConnectionFailed => "connection failed",
            NoAgentCause::NotHeld => "key is not held",
            NoAgentCause::Refused => "request refused",
            NoAgentCause::InvalidReply => "invalid reply",
            NoAgentCause::ReplyTooLarge => "reply exceeds 262144 bytes",
            NoAgentCause::Cancelled => "request cancelled",
        }
    }

    /// The cause whose exact text is `text`; `None` for any other text.
    pub fn parse(text: &str) -> Option<NoAgentCause> {
        NoAgentCause::ALL.into_iter().find(|c| c.as_str() == text)
    }
}

impl fmt::Display for NoAgentCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `E_SIGNING_KEY_MISSING` for `setting`: exactly `signing.key` or `signing.agent_key`.
pub fn signing_key_missing(setting: &str) -> RdyError {
    RdyError::new(
        E_SIGNING_KEY_MISSING,
        format!("{setting} is not set"),
        format!("set {setting} in the user config or .rdy/local.yaml"),
    )
}

/// `E_SIGNING_KEY_TYPE` for the key `key` (its fingerprint) of type `key_type`.
pub fn signing_key_type(key: &str, key_type: &str) -> RdyError {
    RdyError::new(
        E_SIGNING_KEY_TYPE,
        format!("{key} has unsupported signing key type {key_type}"),
        SIGNING_KEY_TYPE_FIX,
    )
}

/// `E_NO_AGENT` with one of the closed causes.
pub fn no_agent(cause: NoAgentCause) -> RdyError {
    RdyError::new(
        E_NO_AGENT,
        format!("the SSH agent could not sign: {cause}"),
        NO_AGENT_FIX,
    )
}

/// `E_SIG_INVALID` for the signature record `hash`.
pub fn sig_invalid(hash: &str) -> RdyError {
    RdyError::new(
        E_SIG_INVALID,
        format!("signature {hash} is invalid"),
        SIG_INVALID_FIX,
    )
}

/// `E_SIG_OBJECT_MISMATCH` for the signature record `hash`.
pub fn sig_object_mismatch(hash: &str) -> RdyError {
    RdyError::new(
        E_SIG_OBJECT_MISMATCH,
        format!("signature {hash} does not match its subject"),
        SIG_OBJECT_MISMATCH_FIX,
    )
}

/// `W_SIG_UNKNOWN_KEY` for `key` (a fingerprint), `principal` and `namespace`.
pub fn sig_unknown_key(key: &str, principal: &str, namespace: &str) -> RdyError {
    RdyError::new(
        W_SIG_UNKNOWN_KEY,
        format!("key {key} is not trusted for {principal} in {namespace}"),
        SIG_UNKNOWN_KEY_FIX,
    )
}

/// `W_SIGNING_FAILED`, embedding the exact code and message of the first signing error.
pub fn signing_failed(first: &RdyError) -> RdyError {
    RdyError::new(
        W_SIGNING_FAILED,
        format!(
            "the act was saved without signatures: {}: {}",
            first.code, first.message
        ),
        SIGNING_FAILED_FIX,
    )
}

/// `E_LIMIT` for a signing input over `limit`: the numeric limit and its unit, such as
/// `16384 bytes`, `1024 identities` or `4096 signatures per subject`.
pub fn signing_limit(limit: &str) -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("signing input exceeds {limit}"),
        SIGNING_LIMIT_FIX,
    )
}

/// `E_BAD_INPUT` for a configured key selector of `setting` that is neither a public-key line nor
/// a canonical SHA256 fingerprint.
pub fn signing_selector(setting: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("{setting} is not a public key or SHA256 fingerprint"),
        format!("set {setting} to a public key line or its SHA256 fingerprint"),
    )
}

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
            StoreError::Redacted { request } => RdyError::new(
                E_REDACTED,
                format!("Content removed by redaction {request}."),
                REDACTED_FIX,
            ),
            StoreError::RedactedState => RdyError::new(
                E_REDACTED_STATE,
                "This read state predates content removal.",
                REDACTED_STATE_FIX,
            ),
            StoreError::RedactionHuman => RdyError::new(
                E_REDACTION_HUMAN,
                "Content removal needs a person's approval.",
                REDACTION_HUMAN_FIX,
            ),
            StoreError::RedactionPending => RdyError::new(
                E_REDACTION_PENDING,
                "Content removal was authorized but has not finished.",
                REDACTION_PENDING_FIX,
            ),
            StoreError::RedactionStale => RdyError::new(
                E_REDACTION_STALE,
                "The redaction report changed after the request was recorded.",
                REDACTION_STALE_FIX,
            ),
            StoreError::RedactionCorrupt { segment, offset } => RdyError::new(
                E_REDACTION_CORRUPT,
                format!(
                    "The redaction record does not authorize the missing payload at {segment}:{offset}."
                ),
                REDACTION_CORRUPT_FIX,
            ),
            StoreError::RedactionLimit { limit } => redaction_limit(limit),
            StoreError::ProjectedWrite => RdyError::new(
                E_BAD_INPUT,
                "A read projection cannot be written.",
                "Write a new block version from source content.",
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

/// The session actor is not in the hub's `human:`, `agent:` or `ci:` form, an agent session
/// claims a human actor, or a human claim is not bound to the install's account handle. Built by
/// [`actor_unbound`].
pub const E_ACTOR_UNBOUND: &str = "E_ACTOR_UNBOUND";
/// A hub command names no remote, and none is the share remote or the only remote.
pub const E_REMOTE_REQUIRED: &str = "E_REMOTE_REQUIRED";
/// A second share remote would be added.
pub const E_REMOTE_EXISTS: &str = "E_REMOTE_EXISTS";
/// A hub request destination outside its install's origin fence. Built by [`upstream_origin`].
pub const E_UPSTREAM_ORIGIN: &str = "E_UPSTREAM_ORIGIN";
/// A hub token that fails the token form check.
pub const E_UPSTREAM_TOKEN: &str = "E_UPSTREAM_TOKEN";
/// The install has no usable sign-in. Built by [`upstream_auth`].
pub const E_UPSTREAM_AUTH: &str = "E_UPSTREAM_AUTH";
/// A hub reply outside the client contract.
pub const E_UPSTREAM_PROTOCOL: &str = "E_UPSTREAM_PROTOCOL";
/// A hub request that failed before the hub could act on it.
pub const E_UPSTREAM_TRANSPORT: &str = "E_UPSTREAM_TRANSPORT";
/// The hub refused the request.
pub const E_UPSTREAM_REFUSED: &str = "E_UPSTREAM_REFUSED";
/// A hub request whose reply was lost after it was sent: it may have completed.
pub const E_UPSTREAM_UNCERTAIN: &str = "E_UPSTREAM_UNCERTAIN";
/// A sign-in without a matching confirmation of its host card.
pub const E_UPSTREAM_CONFIRM_HOST: &str = "E_UPSTREAM_CONFIRM_HOST";
/// The configured CA file cannot be read or holds no usable certificate.
pub const E_UPSTREAM_CA: &str = "E_UPSTREAM_CA";
/// The SSH agent's proof for a key binding fails its check.
pub const E_UPSTREAM_KEY_PROOF: &str = "E_UPSTREAM_KEY_PROOF";
/// The hub refused the request's actor claim.
pub const E_UPSTREAM_ACTOR_CLAIM: &str = "E_UPSTREAM_ACTOR_CLAIM";
/// The account's hub handle changed. Built by [`upstream_handle_changed`].
pub const W_UPSTREAM_HANDLE_CHANGED: &str = "W_UPSTREAM_HANDLE_CHANGED";
/// The local credential was removed but the hub did not confirm the sign-out. Built by
/// [`upstream_signout_local`].
pub const W_UPSTREAM_SIGNOUT_LOCAL: &str = "W_UPSTREAM_SIGNOUT_LOCAL";

/// The message of `E_ACTOR_UNBOUND`.
pub const ACTOR_UNBOUND_MESSAGE: &str = "the session actor is not a hub actor";
/// The fix of `E_ACTOR_UNBOUND`.
pub const ACTOR_UNBOUND_FIX: &str =
    "set an actor in the hub's human, agent or ci form, then try again";
/// The message of `E_REMOTE_REQUIRED`.
pub const REMOTE_REQUIRED_MESSAGE: &str = "no remote was selected";
/// The fix of `E_REMOTE_REQUIRED`.
pub const REMOTE_REQUIRED_FIX: &str = "pass --remote, or configure one share remote or one remote";
/// The message of `E_REMOTE_EXISTS`.
pub const REMOTE_EXISTS_MESSAGE: &str = "a share remote already exists";
/// The fix of `E_REMOTE_EXISTS`.
pub const REMOTE_EXISTS_FIX: &str = "change its mode before adding another share remote";
/// The fix of `E_UPSTREAM_ORIGIN`.
pub const UPSTREAM_ORIGIN_FIX: &str =
    "use the install origin, or add its destination to hub.installs.pinned_origins";
/// The message of `E_UPSTREAM_TOKEN`.
pub const UPSTREAM_TOKEN_MESSAGE: &str = "the hub token has an invalid form";
/// The fix of `E_UPSTREAM_TOKEN`.
pub const UPSTREAM_TOKEN_FIX: &str = "sign in to this install again";
/// The fix of `E_UPSTREAM_AUTH`.
pub const UPSTREAM_AUTH_FIX: &str = "start a new sign-in for this install";
/// The message of `E_UPSTREAM_PROTOCOL`.
pub const UPSTREAM_PROTOCOL_MESSAGE: &str = "the hub reply does not match the client contract";
/// The fix of `E_UPSTREAM_PROTOCOL`.
pub const UPSTREAM_PROTOCOL_FIX: &str = "check that this rdy version supports the hub's API";
/// The message of `E_UPSTREAM_TRANSPORT`.
pub const UPSTREAM_TRANSPORT_MESSAGE: &str = "the hub request failed";
/// The fix of `E_UPSTREAM_TRANSPORT`.
pub const UPSTREAM_TRANSPORT_FIX: &str =
    "check the install address, network and CA setting, then try again";
/// The message of `E_UPSTREAM_REFUSED`.
pub const UPSTREAM_REFUSED_MESSAGE: &str = "the hub refused the request";
/// The fix of `E_UPSTREAM_REFUSED`.
pub const UPSTREAM_REFUSED_FIX: &str = "check the account's access on the hub before trying again";
/// The message of `E_UPSTREAM_UNCERTAIN`.
pub const UPSTREAM_UNCERTAIN_MESSAGE: &str = "the hub request may have completed";
/// The fix of `E_UPSTREAM_UNCERTAIN`.
pub const UPSTREAM_UNCERTAIN_FIX: &str = "check its state on the hub before trying again";
/// The message of `E_UPSTREAM_CONFIRM_HOST`.
pub const UPSTREAM_CONFIRM_HOST_MESSAGE: &str = "the install host has not been confirmed";
/// The fix of `E_UPSTREAM_CONFIRM_HOST`.
pub const UPSTREAM_CONFIRM_HOST_FIX: &str =
    "show the sign-in card, then relay the answer with its card_hash";
/// The message of `E_UPSTREAM_CA`.
pub const UPSTREAM_CA_MESSAGE: &str = "the configured CA file cannot be used";
/// The fix of `E_UPSTREAM_CA`.
pub const UPSTREAM_CA_FIX: &str =
    "set ca_file to an absolute path to a valid PEM certificate bundle in user config";
/// The message of `E_UPSTREAM_KEY_PROOF`.
pub const UPSTREAM_KEY_PROOF_MESSAGE: &str = "the agent's key proof is invalid";
/// The fix of `E_UPSTREAM_KEY_PROOF`.
pub const UPSTREAM_KEY_PROOF_FIX: &str =
    "check the configured public key and SSH agent, then bind again";
/// The message of `E_UPSTREAM_ACTOR_CLAIM`.
pub const UPSTREAM_ACTOR_CLAIM_MESSAGE: &str = "the hub refused the actor claim";
/// The fix of `E_UPSTREAM_ACTOR_CLAIM`.
pub const UPSTREAM_ACTOR_CLAIM_FIX: &str =
    "check the current account handle and session actor, then try again";
/// The message of `W_UPSTREAM_SIGNOUT_LOCAL`.
pub const UPSTREAM_SIGNOUT_LOCAL_MESSAGE: &str =
    "the local credential was removed; hub sign-out was not confirmed";
/// The fix of `W_UPSTREAM_SIGNOUT_LOCAL`.
pub const UPSTREAM_SIGNOUT_LOCAL_FIX: &str = "end the credential on the hub if it is still active";

/// `E_ACTOR_UNBOUND`.
pub fn actor_unbound() -> RdyError {
    RdyError::new(E_ACTOR_UNBOUND, ACTOR_UNBOUND_MESSAGE, ACTOR_UNBOUND_FIX)
}

/// `E_REMOTE_REQUIRED`.
pub fn remote_required() -> RdyError {
    RdyError::new(
        E_REMOTE_REQUIRED,
        REMOTE_REQUIRED_MESSAGE,
        REMOTE_REQUIRED_FIX,
    )
}

/// `E_REMOTE_EXISTS`.
pub fn remote_exists() -> RdyError {
    RdyError::new(E_REMOTE_EXISTS, REMOTE_EXISTS_MESSAGE, REMOTE_EXISTS_FIX)
}

/// `E_UPSTREAM_ORIGIN` for `install`, a canonical origin.
pub fn upstream_origin(install: &str) -> RdyError {
    RdyError::new(
        E_UPSTREAM_ORIGIN,
        format!("the request destination is not trusted for {install}"),
        UPSTREAM_ORIGIN_FIX,
    )
}

/// `E_UPSTREAM_TOKEN`. It never names the token.
pub fn upstream_token() -> RdyError {
    RdyError::new(E_UPSTREAM_TOKEN, UPSTREAM_TOKEN_MESSAGE, UPSTREAM_TOKEN_FIX)
}

/// `E_UPSTREAM_AUTH` for `install`, a canonical origin.
pub fn upstream_auth(install: &str) -> RdyError {
    RdyError::new(
        E_UPSTREAM_AUTH,
        format!("sign-in is required for {install}"),
        UPSTREAM_AUTH_FIX,
    )
}

/// `E_UPSTREAM_PROTOCOL`. It never quotes the reply.
pub fn upstream_protocol() -> RdyError {
    RdyError::new(
        E_UPSTREAM_PROTOCOL,
        UPSTREAM_PROTOCOL_MESSAGE,
        UPSTREAM_PROTOCOL_FIX,
    )
}

/// `E_UPSTREAM_TRANSPORT`. It never quotes the library error.
pub fn upstream_transport() -> RdyError {
    RdyError::new(
        E_UPSTREAM_TRANSPORT,
        UPSTREAM_TRANSPORT_MESSAGE,
        UPSTREAM_TRANSPORT_FIX,
    )
}

/// `E_UPSTREAM_REFUSED`. It never quotes the hub's own message.
pub fn upstream_refused() -> RdyError {
    RdyError::new(
        E_UPSTREAM_REFUSED,
        UPSTREAM_REFUSED_MESSAGE,
        UPSTREAM_REFUSED_FIX,
    )
}

/// `E_UPSTREAM_UNCERTAIN`.
pub fn upstream_uncertain() -> RdyError {
    RdyError::new(
        E_UPSTREAM_UNCERTAIN,
        UPSTREAM_UNCERTAIN_MESSAGE,
        UPSTREAM_UNCERTAIN_FIX,
    )
}

/// `E_UPSTREAM_CONFIRM_HOST`.
pub fn upstream_confirm_host() -> RdyError {
    RdyError::new(
        E_UPSTREAM_CONFIRM_HOST,
        UPSTREAM_CONFIRM_HOST_MESSAGE,
        UPSTREAM_CONFIRM_HOST_FIX,
    )
}

/// `E_UPSTREAM_CA`. It never names the file or quotes its parser.
pub fn upstream_ca() -> RdyError {
    RdyError::new(E_UPSTREAM_CA, UPSTREAM_CA_MESSAGE, UPSTREAM_CA_FIX)
}

/// `E_UPSTREAM_KEY_PROOF`.
pub fn upstream_key_proof() -> RdyError {
    RdyError::new(
        E_UPSTREAM_KEY_PROOF,
        UPSTREAM_KEY_PROOF_MESSAGE,
        UPSTREAM_KEY_PROOF_FIX,
    )
}

/// `E_UPSTREAM_ACTOR_CLAIM`.
pub fn upstream_actor_claim() -> RdyError {
    RdyError::new(
        E_UPSTREAM_ACTOR_CLAIM,
        UPSTREAM_ACTOR_CLAIM_MESSAGE,
        UPSTREAM_ACTOR_CLAIM_FIX,
    )
}

/// `W_UPSTREAM_HANDLE_CHANGED` from the validated handle `old` to the validated handle `new`, as
/// a warning issue.
pub fn upstream_handle_changed(old: &str, new: &str) -> Issue {
    Issue {
        severity: rdy_schema::Severity::Warning,
        code: W_UPSTREAM_HANDLE_CHANGED,
        block: None,
        message: format!("the hub handle changed from {old} to {new}"),
        fix: format!("use human:{new} for this install"),
    }
}

/// `W_UPSTREAM_SIGNOUT_LOCAL`, as a warning issue.
pub fn upstream_signout_local() -> Issue {
    Issue {
        severity: rdy_schema::Severity::Warning,
        code: W_UPSTREAM_SIGNOUT_LOCAL,
        block: None,
        message: UPSTREAM_SIGNOUT_LOCAL_MESSAGE.to_string(),
        fix: UPSTREAM_SIGNOUT_LOCAL_FIX.to_string(),
    }
}
