//! The swarm journal: one entry per act, and the closed table of refusals an act may meet
//! (ledger O3). The journal keeps what each reader saw, so a check can compare it with the
//! logged moves (FW-IB2, D-Z0EMP).
use std::sync::{Arc, Mutex, PoisonError};

/// What an act met.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    /// The daemon refused with this error code.
    Refused(String),
    /// The daemon died with the call in flight, between the kill and the restart. A lost call
    /// anywhere else fails the run with the seed.
    Lost,
    /// The act found nothing to do. A pick that resolved to nothing makes the actor read a seed
    /// block instead; an act whose precondition does not hold carries `Entry::skip`.
    Skipped,
}

/// Why an act found nothing to do and was `Skipped` before any call (the closed list of the
/// ruling on task 4, `NoUnconflictedTarget` of the ruling on task 5 and `ProposedBeforeSet` of
/// its round-1 ruling). The executor decides each from the state it read or the journal, never
/// from a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// The agent's view holds no change to propose.
    NothingToPropose,
    /// No claim of the agent cites a block that changed since it was pinned.
    NothingToRepin,
    /// No change of another agent's view is left to include.
    NothingToInclude,
    /// No included change of the agent is still in its view.
    NothingToExclude,
    /// The agent's view is closed, so a probe of the briefing gate has nowhere to write.
    NoLiveView,
    /// Every block a write that names no base could aim at is conflicted in the agent's view
    /// (ruling task-5.3): a write without a base is refused on a conflicted block.
    NoUnconflictedTarget,
    /// The person's `land`: the queued proposal (`Entry::block`) was made before its view joined
    /// an alternative set, so it chose no member (ruling task-5-round1.2). The person drops it
    /// and lands the next one; the view's later proposals choose or land on their own.
    ProposedBeforeSet,
}

/// One inbox read: the cursor passed, the cursor returned and the entries as the reply gave them.
#[derive(Debug, Clone, PartialEq)]
pub struct InboxRead {
    pub since: String,
    pub cursor: String,
    pub entries: Vec<serde_json::Value>,
    /// The agent's range number. It starts at 0 and grows by one when the agent must
    /// restart from its persisted cursor after a kill.
    pub range: u32,
}

/// One journaled act.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub actor: String,
    pub step: usize,
    pub act: &'static str,
    pub outcome: Outcome,
    pub change: Option<String>,
    pub view: Option<String>,
    pub block: Option<String>,
    pub base: Option<String>,
    pub brief: Option<String>,
    pub stable: bool,
    /// The daemon generation the call ran against: 0 before the kill, 1 after the restart.
    pub epoch: u32,
    /// The tension labels (`tn-<12 hex>`) the reply listed as open.
    pub seen: Vec<String>,
    /// Set for an inbox read.
    pub inbox: Option<InboxRead>,
    /// The reply a check reads: for `land`, the `rdy --json approve` reply (its `assurance`,
    /// `landing` and `chosen`).
    pub reply: Option<serde_json::Value>,
    /// Set on a `Skipped` entry that found nothing to do (see [`Skip`]).
    pub skip: Option<Skip>,
    /// The message of a refusal (`Outcome::Refused`), as the daemon gave it: a check reads which
    /// change a refusal names.
    pub message: Option<String>,
    /// Set on the drain's `repair_dangling_ref`: the block the rewritten reference named, which a
    /// change the drain excluded created (ruling task-5-round2.1). `block` is the citing block.
    pub target: Option<String>,
}

impl Entry {
    /// An entry with every optional field empty.
    pub fn new(actor: &str, step: usize, act: &'static str, outcome: Outcome) -> Entry {
        Entry {
            actor: actor.to_string(),
            step,
            act,
            outcome,
            change: None,
            view: None,
            block: None,
            base: None,
            brief: None,
            stable: false,
            epoch: 0,
            seen: Vec::new(),
            inbox: None,
            reply: None,
            skip: None,
            message: None,
            target: None,
        }
    }
}

/// The shared journal. Clones share one list.
#[derive(Debug, Clone, Default)]
pub struct Journal(Arc<Mutex<Vec<Entry>>>);

impl Journal {
    pub fn new() -> Journal {
        Journal::default()
    }

    pub fn record(&self, e: Entry) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(e);
    }

    pub fn entries(&self) -> Vec<Entry> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The entries from index `from` on, and the index after them: a reader that polls keeps the
    /// index and copies only what is new (final review M10).
    pub fn since(&self, from: usize) -> (Vec<Entry>, usize) {
        let all = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let from = from.min(all.len());
        (all[from..].to_vec(), all.len())
    }
}

const WRITE_CODES: &[&str] = &[
    "E_STALE_BASE",
    "E_BRIEF_STALE",
    "E_VIEW_CLOSED",
    "E_ALT_MEMBER",
];
const REPIN_CODES: &[&str] = &["E_STALE_BASE", "E_BRIEF_STALE", "E_VIEW_CLOSED"];
const VIEW_OP_CODES: &[&str] = &[
    "E_VIEW_CLOSED",
    "E_HAS_DEPENDENTS",
    "E_ALT_MEMBER",
    "E_NOT_FOUND",
];
const UNDO_CODES: &[&str] = &[
    "E_NOTHING_TO_UNDO",
    "E_UNDO_BLOCKED",
    "E_UNDO_LANDING",
    "E_VIEW_CLOSED",
];
const ALT_CREATE_CODES: &[&str] = &["E_ALT_MEMBER", "E_VIEW_CLOSED"];
const PROPOSE_CODES: &[&str] = &[
    "E_ALT_CHOSEN",
    "E_DEPS_MISSING",
    "E_ALT_UNCHOSEN",
    "E_ALT_MEMBER",
    "E_VIEW_CLOSED",
];
const LAND_CODES: &[&str] = &[
    "E_ALT_MEMBER",
    "E_PROPOSAL_MOVED",
    // D-ME2HD: a conflict or a blocking tension is an issue of a version that is not ready
    "E_PROPOSAL_NOT_READY",
    "E_ALT_UNCHOSEN",
    "E_ALT_CHOSEN",
    "E_ALREADY_LANDED",
    "E_VIEW_CLOSED",
    // D-ME2HD keeps E_STALE_BASE among the landing's write checks until M3b; M3a Task 6
    // (D-TX3KH) refuses a version whose block needs a new order key when its view changed or
    // dropped that block after the proposal ("propose again"). The person re-queues it.
    "E_STALE_BASE",
];
const WC_CODES: &[&str] = &["E_WC_STALE", "E_WC_CONFLICT"];

/// The closed table of refusals (ledger O3): may `act` meet the refusal `code`?
///
/// An act the table does not know allows nothing. `E_DAEMON_GONE` is not a refusal: the
/// journal records it as `Outcome::Lost`, and only between the kill and the restart.
pub fn allowed(act: &str, code: &str) -> bool {
    let codes: &[&str] = match act {
        "write" | "create" | "cite" => WRITE_CODES,
        "write_no_brief" => &["E_BRIEF_REQUIRED"],
        "write_stale" => &["E_BRIEF_STALE"],
        "collide" => &["E_STALE_BASE", "E_BRIEF_STALE", "E_VIEW_CLOSED"],
        "repin" => REPIN_CODES,
        "fork" | "include" | "exclude" => VIEW_OP_CODES,
        "undo" => UNDO_CODES,
        "alt_create" => ALT_CREATE_CODES,
        "propose" => PROPOSE_CODES,
        "review_submit" => &["E_REVIEW_STALE"],
        "land" => LAND_CODES,
        "dismiss" => &["E_NOT_FOUND"],
        "accept_advice" => &["E_PROPOSAL_MOVED"],
        "wc_checkout" | "wc_commit" | "wc_update" | "wc_propose" => WC_CODES,
        "inbox_resume" => &["E_BAD_INPUT"],
        // intent, review_next, tensions, inbox, read, brief: no refusal is allowed.
        _ => &[],
    };
    codes.contains(&code)
}
