//! The swarm journal: one entry per act, and the closed table of refusals an act may meet
//! (ledger O3). The journal keeps what each reader saw, so a check can compare it with the
//! logged moves (FW-IB2, D-Z0EMP).
use super::script::Route;
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
    /// A check act found no subject: no proposed decision or editorial change of the agent's own
    /// for a run or a request, no rule fixture to repair, or no check view to propose again.
    NothingToCheck,
    /// A reviewer's `check_submit` found no free independent check request.
    NoCheckRequest,
    /// The person's `accept_check_verdict`: no card in the queue offers a check verdict to accept.
    NoVerdictToAccept,
    /// The person's `ui_answer`: the approve card recommends no option (D-91DEW). A missing
    /// recommendation is never read as approve.
    NoRecommendedOption,
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

/// What a check act saw (D-JQD5X): each value the reply gave, when it gave one. Labels and hashes
/// are as the tools print them (`rq-<64 hex>`, 64 hex, `P-x@n`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckFacts {
    /// The check's name, such as `meaning_preservation`.
    pub check: Option<String>,
    /// The durable request (`rq-<64 hex>`).
    pub request: Option<String>,
    /// The case hash the request or the lease gave.
    pub case_hash: Option<String>,
    /// The REVIEW a verdict wrote.
    pub review: Option<String>,
    /// The verdict (`pass`, `concern`, `fail`).
    pub verdict: Option<String>,
    /// The ANSWER a person's acceptance wrote, by its option id `accept_verdict:<review>:<version>`.
    pub answer: Option<String>,
    /// The proposal version the act named (`P-x@n`).
    pub version: Option<String>,
}

/// What a `ui_answer` showed and sent (D-6M0AF): the card's form as the page gave it, and what the
/// HTTP answer was.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UiFacts {
    /// The source of the answer: always `ui`.
    pub source: &'static str,
    /// The item ref the form names.
    pub item: String,
    /// The card hash the form names.
    pub card_hash: String,
    /// The labels the form shows, in order.
    pub labels: Vec<String>,
    /// The option chosen, and its label.
    pub option: String,
    pub label: String,
    /// The HTTP status.
    pub status: u16,
    /// The landing the answer made, when it made one.
    pub landing: Option<String>,
}

/// How an M3 route attempt was made: an MCP tool or a CLI command (the person's explicit CLI
/// fallback lane).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Tool(&'static str),
    Cli(&'static str),
}

/// What an M3 route attempt met. `Pending` is the only state before a reply; the other three are
/// closed and distinct: a transport interruption is neither a refusal nor an acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// The call is sent and no reply came. Never journaled.
    Pending,
    /// The transport broke with the call in flight; the daemon may or may not have acted.
    Interrupted,
    /// The daemon replied with this error code.
    Refused { code: String },
    /// The daemon replied with success: its acknowledgement and the record ids the reply named.
    Acknowledged { ack: String, records: Vec<String> },
}

/// What a route attempt consumed or checked, as counts the reply gave.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RouteCounts {
    pub pages: usize,
    pub requests: usize,
    pub answers: usize,
    pub removals: usize,
    pub profile_refs: usize,
    pub links_checked: usize,
}

/// One attempted M3 route act (G2): who tried it, by which tool or CLI command, what the actor
/// observed and was shown, and what the daemon replied. The reply is set only by `acknowledged`,
/// `refused` or `interrupted`, from the reply: the attempt never holds a computed success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteAttempt {
    pub route: Route,
    /// One of `route.acts()`.
    pub act: &'static str,
    pub actor: String,
    pub run: String,
    pub via: Via,
    /// The ref and version the actor observed before the act.
    pub observed_ref: Option<String>,
    pub observed_version: Option<String>,
    /// The card hash shown to the person and the option chosen.
    pub card_hash: Option<String>,
    pub option: Option<String>,
    pub reply: Reply,
    pub counts: RouteCounts,
}

impl RouteAttempt {
    /// Starts an attempt, with no reply yet. `None` when `act` is not an act of `route`.
    pub fn begin(
        route: Route,
        act: &'static str,
        actor: &str,
        run: &str,
        via: Via,
    ) -> Option<RouteAttempt> {
        route.acts().contains(&act).then(|| RouteAttempt {
            route,
            act,
            actor: actor.to_string(),
            run: run.to_string(),
            via,
            observed_ref: None,
            observed_version: None,
            card_hash: None,
            option: None,
            reply: Reply::Pending,
            counts: RouteCounts::default(),
        })
    }

    /// The ref and version the actor observed.
    pub fn observed(mut self, r: &str, version: &str) -> RouteAttempt {
        self.observed_ref = Some(r.to_string());
        self.observed_version = Some(version.to_string());
        self
    }

    /// The card hash shown and the option chosen.
    pub fn shown(mut self, card_hash: &str, option: &str) -> RouteAttempt {
        self.card_hash = Some(card_hash.to_string());
        self.option = Some(option.to_string());
        self
    }

    /// The counts of what the act consumed and checked.
    pub fn counted(mut self, counts: RouteCounts) -> RouteAttempt {
        self.counts = counts;
        self
    }

    /// The daemon acknowledged. `None` for an empty acknowledgement: a reply without one is no
    /// acknowledgement.
    pub fn acknowledged(mut self, ack: &str, records: Vec<String>) -> Option<RouteAttempt> {
        if ack.is_empty() {
            return None;
        }
        self.reply = Reply::Acknowledged {
            ack: ack.to_string(),
            records,
        };
        Some(self)
    }

    /// The daemon refused with `code`.
    pub fn refused(mut self, code: &str) -> RouteAttempt {
        self.reply = Reply::Refused {
            code: code.to_string(),
        };
        self
    }

    /// The transport broke with the call in flight.
    pub fn interrupted(mut self) -> RouteAttempt {
        self.reply = Reply::Interrupted;
        self
    }

    pub fn is_pending(&self) -> bool {
        self.reply == Reply::Pending
    }

    pub fn is_acknowledged(&self) -> bool {
        matches!(self.reply, Reply::Acknowledged { .. })
    }

    pub fn is_interrupted(&self) -> bool {
        self.reply == Reply::Interrupted
    }

    /// The refusal code, when the daemon refused.
    pub fn refusal(&self) -> Option<&str> {
        match &self.reply {
            Reply::Refused { code } => Some(code),
            _ => None,
        }
    }

    /// Whether the closed table allows this attempt's outcome: an acknowledgement and an
    /// interruption always, a refusal only when `allowed` lists its code for the act, a pending
    /// attempt never.
    pub fn is_allowed(&self) -> bool {
        match &self.reply {
            Reply::Pending => false,
            Reply::Interrupted | Reply::Acknowledged { .. } => true,
            Reply::Refused { code } => allowed(self.act, code),
        }
    }
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
    /// The reply a check reads: for `land`, the `rdy --json approve` reply or an agent's `rdy_land`
    /// reply (its `assurance`, `landing` and `chosen`).
    pub reply: Option<serde_json::Value>,
    /// Set on a `Skipped` entry that found nothing to do (see [`Skip`]).
    pub skip: Option<Skip>,
    /// The message of a refusal (`Outcome::Refused`), as the daemon gave it: a check reads which
    /// change a refusal names.
    pub message: Option<String>,
    /// Set on the drain's `repair_dangling_ref`: the block the rewritten reference named, which a
    /// change the drain excluded created (ruling task-5-round2.1). `block` is the citing block.
    pub target: Option<String>,
    /// Set on a check act (`check_run`, `check_request`, `check_submit`, `accept_check_verdict`).
    pub check: Option<CheckFacts>,
    /// Set on a `ui_answer`.
    pub ui: Option<UiFacts>,
    /// Set on an M3 route attempt (`Entry::from_route`).
    pub route: Option<RouteAttempt>,
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
            check: None,
            ui: None,
            route: None,
        }
    }

    /// The entry of an M3 route attempt. `None` while the attempt is pending: nothing is
    /// journaled before a reply. An acknowledgement is `Ok`, a refusal is `Refused` with its
    /// code, an interruption is `Lost`; the three never map to each other.
    pub fn from_route(step: usize, attempt: RouteAttempt) -> Option<Entry> {
        let outcome = match &attempt.reply {
            Reply::Pending => return None,
            Reply::Interrupted => Outcome::Lost,
            Reply::Refused { code } => Outcome::Refused(code.clone()),
            Reply::Acknowledged { .. } => Outcome::Ok,
        };
        let mut e = Entry::new(&attempt.actor, step, attempt.act, outcome);
        e.block = attempt.observed_ref.clone();
        e.route = Some(attempt);
        Some(e)
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
    // D-ME2HD: a conflict, a blocking tension, an open plan item and a stable change with no
    // briefing are issues of a version that is not ready
    "E_PROPOSAL_NOT_READY",
    "E_ALT_UNCHOSEN",
    "E_ALT_CHOSEN",
    "E_ALREADY_LANDED",
    "E_VIEW_CLOSED",
    // E_STALE_BASE stays among the landing's write checks (D-ME2HD, O12); M3a Task 6 (D-TX3KH)
    // refuses a version whose block needs a new order key when its view changed or dropped
    // that block after the proposal ("propose again"). The person re-queues it.
    "E_STALE_BASE",
    // ap13 (D-ZWP7Q): an agent's `rdy_land` of a version whose stored verdict needs a person;
    // the drain falls back to the person's approval once
    "E_POLICY_HUMAN",
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
        // D-JQD5X: a run or a request names a subject its view holds, read in the same turn, so
        // it meets no refusal; a verdict on a case that changed after the lease is stale
        "check_submit" => &["E_REVIEW_STALE"],
        // the card moved between the queue read and the answer (a new verdict, a landing)
        "accept_check_verdict" => &["E_PROPOSAL_MOVED"],
        // D-6M0AF: the forced moved card's old hash; a policy landing of an imported status
        "ui_answer" => &["E_PROPOSAL_MOVED"],
        "ui_policy_land" => &["E_POLICY_HUMAN"],
        // G2, the M3 routes: each act has its own closed set. The wrong-role act is the one that
        // meets E_PERMISSION; an elicited decline is an answer, never a refusal
        "m3_relay_answer" => &["E_PROPOSAL_MOVED", "E_ITEM_MOVED"],
        "m3_wrong_role" => &["E_PERMISSION"],
        "m3_right_role" | "m3_elicit_first_rev" | "m3_elicit_second_rev" | "m3_moved_card" => {
            &["E_PROPOSAL_MOVED"]
        }
        "m3_relay_after_decline" => &["E_PROPOSAL_MOVED"],
        "m3_publication_check" | "m3_publication_output" => &["E_PUBLICATION_REFERENCE"],
        "m3_blob_remove" => &[
            "E_REDACTION_HUMAN",
            "E_REDACTION_PENDING",
            "E_REDACTION_STALE",
        ],
        // a view or a version whose changes conflict with canon has no change plan until the
        // conflict is resolved: its read refuses with E_CONFLICT_UNRESOLVED, never with a
        // write's E_STALE_BASE (ruling task-15 F3; the propose binds no plan then, and the
        // landing refuses the version as not ready)
        "plan" | "version_plan" => &["E_CONFLICT_UNRESOLVED"],
        // intent, review_next, tensions, inbox, read, brief, compact, and the plan answers, the queue
        // read and the deferral acceptance (plan_answer, pending, accept_deferrals), and a check
        // run or request (check_run, check_request), and a UI fixture's own writes (ui_fixture,
        // ui_site), an elicited decline and a real compact (m3_elicit_decline, m3_compact_real):
        // no refusal is allowed.
        _ => &[],
    };
    codes.contains(&code)
}
