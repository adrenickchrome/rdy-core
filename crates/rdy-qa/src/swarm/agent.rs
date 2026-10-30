//! The scripted agents: one thread and one `rdy mcp` proxy process per agent (ledger F1: code,
//! not a model). Each act of the script is one MCP tool call, plus the calls it needs: a
//! briefing before every write to stable content, and the second call of a two-call act.
//!
//! Journal conventions (Task 5 reads them):
//! - Every tool call writes one `Entry`; the session, the subscription and the first inbox read
//!   that start an agent are setup and write none. A briefing that comes before a write is an
//!   entry of act `brief`, and a second write after `E_BRIEF_STALE` is a second entry.
//! - A write names the change it made in `change`, the block it wrote or created in `block`,
//!   and, for an update, the version it was made from in `base`.
//! - Before a `propose` of a live view the agent answers its change plan (`answer_plan`): a
//!   `plan` entry per page read and one `plan_answer` entry, each naming the view in `view`.
//! - `propose`, `land`, `accept_advice`, `version_plan`, `pending` and `accept_deferrals` name
//!   the proposal ref `P-x@n` in `block`; an `Ok` `propose` entry's `reply` is
//!   `{alternatives}`, the choice it made (null: none). `alt_create` names the agent's view in
//!   `view` and the other member's view in `block`. An `Ok` `include` entry's `reply` is
//!   `{included: [<change>]}`. A `land` the person skips because the proposal was made before
//!   its view's set names that proposal too. `dismiss` names the tension label in `block`. No
//!   other act puts anything but a block there.
//! - A pick that resolves to nothing is journaled `Skipped` after a read of a seed block. An act
//!   that finds nothing to do is journaled `Skipped` with a `Skip` reason and no call of its own
//!   (ruling task-4.1): `propose`, `repin`, `include`, `exclude` and `write_no_brief` read the
//!   state they need first (`view_show`, `suspects` entries) and decide from it. A refusal of
//!   an actual call is only ever checked against the closed table (`allowed`); a message text
//!   never turns a refusal into a skip.
//! - `Lost` is journaled only between the kill and the restart: the run expects a kill
//!   (`KillGate::with_kill`), has started it (`KillGate::killing`), and the actor still runs on
//!   the first daemon (epoch 0). Any other lost daemon fails the run with the seed.
//! - A contradiction pair is written through the real flow (ruling task-4.2): the first
//!   member's claim is created, proposed and landed by the person, then the second member
//!   creates its claim against canon. The executor adds no act of its own for this. Each
//!   member writes its marked claim in a view of its own, `a<i>-k<k>` (k spelled), and its next
//!   `Propose` proposes that view (fix round 1, ruling task-5.2): the claim lands, or is shown
//!   with its advice, apart from the agent's other work, and no alternative set closes it.
//! - A `ReviewNext` act judges up to `REVIEW_BATCH` free requests, one `review_next` and one
//!   `review_submit` entry each, so the reviewers keep up with the queue in a run.
use super::journal::{Entry, InboxRead, Journal, Outcome, Skip, allowed};
use super::project::{SeedBlocks, SwarmProject};
use super::script::{AREAS, Act, Config, Pick, SEED_BLOCKS};
use crate::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{
    Arc, Condvar, Mutex, OnceLock, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Each call's budget: a hang guard, not a speed check (the budget of `conc1_`).
pub const CALL_BUDGET: Duration = Duration::from_secs(30);
/// How long an actor waits for the run to restart the daemon after a kill (a hang guard).
const RESTART_WAIT: Duration = Duration::from_secs(180);
/// How often an actor tries to open a session on a proxy before the run fails.
const OPEN_TRIES: usize = 8;
/// The tension kinds, as `rdy_tensions {kind}` names them: a read of canon takes one, in turn.
pub(crate) const KINDS: [&str; 10] = [
    "edit_collision",
    "fork",
    "premise_shift",
    "area_overlap",
    "duplicate_definition",
    "unjustified_change",
    "deprecated_reference",
    "value_disagreement",
    "restatement_drift",
    "semantic",
];
/// The most review requests one `ReviewNext` act judges (ruling task-5.2: the reviewers keep up
/// with the queue in a run, so a contradiction pair is judged before the run ends).
pub const REVIEW_BATCH: usize = 16;

/// The daemon generation, shared by the run and every actor. The run calls `restarted` once the
/// killed daemon is gone and the next client may start a new one.
///
/// It also holds the landing turn: an act that decides from a read of the agent's view (a
/// precondition, ruling task-4.1) holds a read turn from that read to its call, and the person's
/// `rdy approve` holds the write turn, so canon never moves between an agent's read and the call
/// that relies on it.
///
/// And it holds the plan turn: an agent that answers its change plan holds it alone, from the
/// plan read to the answers (`plan_turn`), and every other act of an actor that can change a
/// tension holds a shared turn (`act_turn`), so no item the read listed changes before the
/// answers name it (D-ME2HD: a tension item's id moves with its sides). A landing needs none: an
/// agent answers inside its propose, which holds the read turn. An actor takes the landing turn
/// before the plan turn, never the other way. Every other call races freely.
#[derive(Default)]
pub struct KillGate {
    epoch: Mutex<u32>,
    changed: Condvar,
    expects_kill: bool,
    /// Set by the run just before it kills the daemon (`killing`).
    killing: AtomicBool,
    turn: RwLock<()>,
    plan: RwLock<()>,
}

impl KillGate {
    /// A gate for a run that never kills the daemon: no call may be lost.
    pub fn new() -> KillGate {
        KillGate::default()
    }

    /// A gate for a run that kills the daemon once (`Config::kill_at` is set) and then calls
    /// `restarted`. A call is lost, and journaled `Lost`, only between those two.
    pub fn with_kill() -> KillGate {
        KillGate {
            expects_kill: true,
            ..KillGate::default()
        }
    }

    /// Whether the run kills the daemon once.
    pub fn expects_kill(&self) -> bool {
        self.expects_kill
    }

    /// The run is about to kill the daemon: from now until `restarted`, a lost call is the
    /// scripted kill's (final review M5).
    pub fn killing(&self) {
        self.killing.store(true, Ordering::SeqCst);
    }

    /// Whether the run has started its kill (`killing`).
    pub fn is_killing(&self) -> bool {
        self.killing.load(Ordering::SeqCst)
    }

    /// The person's turn to land: no act that decides from a read runs while it is held.
    pub fn landing_turn(&self) -> RwLockWriteGuard<'_, ()> {
        self.turn.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// An agent's turn for an act that decides from a read: no landing runs while it is held.
    pub fn read_turn(&self) -> RwLockReadGuard<'_, ()> {
        self.turn.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// An agent's turn to answer its change plan: no other act that can change a tension runs
    /// while it is held.
    pub fn plan_turn(&self) -> RwLockWriteGuard<'_, ()> {
        self.plan.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// An actor's turn for an act that can change a tension: no plan is answered while it is
    /// held.
    pub fn act_turn(&self) -> RwLockReadGuard<'_, ()> {
        self.plan.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// The daemon was killed and may start again: the generation moves to its next number and
    /// every waiting actor goes on.
    pub fn restarted(&self) {
        *self.epoch.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        self.changed.notify_all();
    }

    /// The daemon generation: 0 until `restarted`, then 1.
    pub fn epoch(&self) -> u32 {
        *self.epoch.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits until the run has restarted the daemon. Past `RESTART_WAIT` it panics with the
    /// run's `seed`: the run never restarted it.
    pub fn wait_restarted(&self, seed: u64) {
        self.wait_restarted_for(seed, RESTART_WAIT);
    }

    fn wait_restarted_for(&self, seed: u64, limit: Duration) {
        let end = Instant::now() + limit;
        let mut epoch = self.epoch.lock().unwrap_or_else(PoisonError::into_inner);
        while *epoch == 0 {
            let left = end.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "swarm seed {seed}: the daemon was killed and the run never restarted it"
            );
            epoch = self
                .changed
                .wait_timeout(epoch, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// What an agent leaves behind when its script ends.
#[derive(Debug, Clone, Default)]
pub struct AgentReport {
    /// The agent's views: its own and the forks it made.
    pub views: BTreeSet<String>,
    /// The proposal refs `P-x@n` it made, oldest first.
    pub proposals: Vec<String>,
    /// The cursor its last inbox read returned.
    pub last_cursor: String,
    /// The cursor of its last read that acked: where a read starts again when `last_cursor` is
    /// past the end of the log after a kill. `None` when the agent does not know it: no ack yet,
    /// or an ack whose reply the kill cut (`persisted_after`).
    pub persisted: Option<String>,
    /// The range of its last inbox read (`InboxRead::range`).
    pub range: u32,
}

/// What a call met.
pub(crate) enum Res {
    Ok(Value),
    Refused(String, String),
    Lost,
}

/// Where the project lives, for a thread that cannot borrow the `SwarmProject`.
#[derive(Clone)]
pub(crate) struct Where {
    pub dir: PathBuf,
    pub env: Vec<(String, String)>,
    pub seed: u64,
}

impl Where {
    pub fn of(p: &SwarmProject) -> Where {
        Where {
            dir: p.served.dir.path().to_path_buf(),
            env: p.served.env_pairs(),
            seed: p.run_seed,
        }
    }
}

/// One actor's connection: a proxy process with a session, the journal and the gate. It runs a
/// call, journals it, checks a refusal against the closed table, and reconnects after a kill.
pub(crate) struct Link {
    pub at: Where,
    pub actor: String,
    run: String,
    role: &'static str,
    areas: Option<Vec<String>>,
    pub journal: Journal,
    pub gate: Arc<KillGate>,
    client: McpClient,
    /// The daemon generation the calls run against.
    pub epoch: u32,
    pub step: usize,
    /// Set when the actor reconnected after a kill and has not yet been told.
    recovered: bool,
}

/// The text of a caught panic.
fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<String>()
        .cloned()
        .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_string()))
        .unwrap_or_else(|| "a panic".to_string())
}

impl Link {
    /// Starts a proxy and opens the session on it. A proxy that does not come up, or a session
    /// the daemon does not open, is tried again (the daemon may be starting).
    pub fn connect(
        at: Where,
        actor: &str,
        run: &str,
        role: &'static str,
        areas: Option<Vec<String>>,
        journal: Journal,
        gate: Arc<KillGate>,
    ) -> Link {
        let client = Link::open(&at, actor, run, role, &areas);
        Link {
            epoch: gate.epoch(),
            at,
            actor: actor.to_string(),
            run: run.to_string(),
            role,
            areas,
            journal,
            gate,
            client,
            step: 0,
            recovered: false,
        }
    }

    fn open(
        at: &Where,
        actor: &str,
        run: &str,
        role: &str,
        areas: &Option<Vec<String>>,
    ) -> McpClient {
        let mut last = String::new();
        for _ in 0..OPEN_TRIES {
            let started = catch_unwind(AssertUnwindSafe(|| {
                McpClient::start_proxy_at(&at.dir, &at.env)
            }));
            let mut client = match started {
                Ok(c) => c,
                Err(p) => {
                    last = panic_text(&*p);
                    std::thread::sleep(Duration::from_millis(300));
                    continue;
                }
            };
            let mut args = json!({"actor": actor, "run": run, "role": role});
            if let Some(a) = areas {
                args["areas"] = json!(a);
            }
            let opened = catch_unwind(AssertUnwindSafe(|| {
                let r = client.call_within("rdy_session", args, CALL_BUDGET);
                if r.is_ok() {
                    // the notification is best effort; the inbox is the reliable path
                    client.subscribe("rdy://inbox");
                }
                r
            }));
            match opened {
                Ok(Reply::Ok(_)) => return client,
                Ok(other) => last = format!("{other:?}"),
                Err(p) => last = panic_text(&*p),
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        panic!(
            "swarm seed {}: {actor} could not open a session: {last}",
            at.seed
        );
    }

    /// The panic of an unexpected result.
    pub fn fail(&self, act: &str, what: &str) -> ! {
        panic!(
            "swarm seed {}: {} step {} ({act}) {what}",
            self.at.seed, self.actor, self.step
        )
    }

    /// One tool call, with nothing journaled.
    pub fn raw(&mut self, act: &str, tool: &str, args: Value) -> Res {
        let c = &mut self.client;
        let started = Instant::now();
        match catch_unwind(AssertUnwindSafe(|| c.call_within(tool, args, CALL_BUDGET))) {
            Ok(Reply::Ok(v)) => Res::Ok(v),
            Ok(Reply::ToolError(v)) => Res::Refused(
                v["code"].as_str().unwrap_or("?").to_string(),
                v["message"].as_str().unwrap_or("").to_string(),
            ),
            Ok(Reply::RpcError(e)) => {
                let gone = e["data"]["code"] == "E_DAEMON_GONE"
                    || e["message"]
                        .as_str()
                        .is_some_and(|m| m.starts_with("E_DAEMON_GONE"));
                if gone {
                    self.lost(act, "E_DAEMON_GONE")
                } else {
                    self.fail(act, &format!("got a protocol error: {e}"))
                }
            }
            Err(p) => {
                let text = panic_text(&*p);
                // a dead proxy: the daemon went away and the proxy with it. It either refuses
                // the write, or exits with the request unanswered, which the client reports
                // as a missing reply at once (a hang takes the whole budget).
                let exited =
                    text.contains("no reply to request") && started.elapsed() < CALL_BUDGET / 2;
                if text.contains("closed its input") || exited {
                    self.lost(act, &text)
                } else {
                    self.fail(act, &format!("failed: {text}"))
                }
            }
        }
    }

    /// A call the daemon's death cut off. Accepted only in the kill window (`may_lose`); any
    /// other lost daemon is a product crash or an unplanned exit, and fails the run.
    pub fn lost(&self, act: &str, why: &str) -> Res {
        if may_lose(self.gate.expects_kill(), self.gate.is_killing(), self.epoch) {
            Res::Lost
        } else {
            let when = if !self.gate.expects_kill() {
                "in a run that kills nothing".to_string()
            } else if !self.gate.is_killing() {
                "before the run started its kill".to_string()
            } else {
                format!("after the restart (epoch {})", self.epoch)
            };
            self.fail(act, &format!("lost the daemon {when}: {why}"))
        }
    }

    /// Waits for the run to restart the daemon, then opens a new proxy and session.
    pub fn recover(&mut self) {
        self.gate.wait_restarted(self.at.seed);
        self.epoch = self.gate.epoch();
        self.client = Link::open(&self.at, &self.actor, &self.run, self.role, &self.areas);
        self.recovered = true;
    }

    /// Whether the actor reconnected since the last time this was asked.
    pub fn take_recovered(&mut self) -> bool {
        std::mem::take(&mut self.recovered)
    }

    /// A call that writes one journal entry (see `finish`).
    pub fn exec(
        &mut self,
        e: Entry,
        rules: &'static str,
        tool: &str,
        args: Value,
        fill: impl FnOnce(&mut Entry, &Value),
    ) -> Res {
        let res = self.raw(e.act, tool, args);
        self.finish(e, rules, res, fill)
    }

    /// Journals what a call met. `e` holds the act and its fixed fields; the outcome and the
    /// epoch are set here, and `fill` adds what an `Ok` reply names. A refusal is checked against
    /// the closed table for `rules` (the act whose row applies); one outside the table panics
    /// with the seed, whatever its message says. After `Lost` the actor waits for the restart and
    /// reconnects.
    pub fn finish(
        &mut self,
        mut e: Entry,
        rules: &'static str,
        res: Res,
        fill: impl FnOnce(&mut Entry, &Value),
    ) -> Res {
        e.epoch = self.epoch;
        let (outcome, bad) = judge(rules, &res);
        e.outcome = outcome;
        match &res {
            Res::Ok(v) => fill(&mut e, v),
            Res::Refused(_, message) => e.message = Some(message.clone()),
            Res::Lost => {}
        }
        let act = e.act;
        self.journal.record(e);
        if let Some(what) = bad {
            self.fail(act, &what);
        }
        if matches!(res, Res::Lost) {
            self.recover();
        }
        res
    }

    /// The `Entry` of an act at the current step.
    pub fn entry(&self, act: &'static str) -> Entry {
        let mut e = Entry::new(&self.actor, self.step, act, Outcome::Ok);
        e.epoch = self.epoch;
        e
    }
}

/// Whether a call may be lost: the run kills the daemon once, it has started the kill
/// (`KillGate::killing`), and the actor still runs on the daemon that dies (epoch 0). A lost
/// daemon before the kill starts is a product crash (final review M5).
fn may_lose(expects_kill: bool, killing: bool, epoch: u32) -> bool {
    expects_kill && killing && epoch == 0
}

/// The cursor the daemon persisted for the agent after an inbox read, as far as the agent knows:
/// `cursor` after an acknowledged `ack: true` read, else `old`. `None` is unknown (no ack yet,
/// or an ack whose reply the kill cut: the daemon may or may not have persisted it).
fn persisted_after(
    old: Option<String>,
    ack: bool,
    res: &Res,
    cursor: Option<String>,
) -> Option<String> {
    match res {
        Res::Ok(_) if ack => cursor,
        Res::Lost if ack => None,
        _ => old,
    }
}

/// The budget of a whole plan read (`PLAN_MAX_TOKENS`); a cut read goes on with its `next`.
pub(crate) const PLAN_ALL: usize = 100_000;

/// The act and the reason an agent answers an open plan item of `kind` with (D-ME2HD; the acts
/// `allowed_acts` gives each kind, `plan/compute.rs`): `confirm` for a confirm item, `defer` for
/// an update, a rejustify and a re-affirm item, `confirm` for a tension item. `None` for a code
/// item (`update_implementation`): it never gates, so it stays open.
pub(crate) fn plan_answer_for(kind: &str) -> Option<(&'static str, &'static str)> {
    match kind {
        "confirm" => Some(("confirm", "swarm: checked")),
        "update" | "rejustify" | "reaffirm" => Some(("defer", "swarm: later")),
        "tension" => Some(("confirm", "swarm: accepted")),
        _ => None,
    }
}

/// Answers each open item of `view`'s change plan that gates or warns (D-ME2HD, D-45RG6), in one
/// `rdy_plan` call, by its kind (`plan_answer_for`). It reads the plan whole first: one `plan`
/// entry per page, naming the view. The answers are one `plan_answer` entry naming the view,
/// whose reply lists each answer with its item's kind (`{answers: [{item, kind, act, reason}]}`).
/// It holds the plan turn throughout (`KillGate::plan_turn`), so the items it read are the items
/// it answers. With nothing to answer it makes no second call. A view whose changes conflict
/// with canon has no plan (the read's `E_CONFLICT_UNRESOLVED`, a row of the table), and a call
/// the kill cuts ends it too: the version proposed next is not ready, and the drain answers its
/// plan once it resolves it.
pub(crate) fn answer_plan(link: &mut Link, view: &str) {
    // from the read to the answers, no other act changes a tension (`KillGate::plan_turn`)
    let gate = link.gate.clone();
    let _turn = gate.plan_turn();
    let mut open: Vec<(String, String)> = vec![];
    let mut args = json!({"view": view, "max_tokens": PLAN_ALL});
    loop {
        let mut e = link.entry("plan");
        e.view = Some(view.to_string());
        let mut page = None;
        link.exec(e, "plan", "rdy_plan", args, |en, v| {
            en.reply = Some(json!({"counts": v["counts"]}));
            page = Some(v.clone());
        });
        let Some(page) = page else { return };
        for i in page["items"].as_array().into_iter().flatten() {
            if i["state"] == "open"
                && let (Some(id), Some(kind)) = (i["id"].as_str(), i["kind"].as_str())
            {
                open.push((id.to_string(), kind.to_string()));
            }
        }
        match page["next"].as_str() {
            Some(next) => args = json!({"page": next, "max_tokens": PLAN_ALL}),
            None => break,
        }
    }
    let sent: Vec<Value> = open
        .iter()
        .filter_map(|(item, kind)| {
            let (act, reason) = plan_answer_for(kind)?;
            Some(json!({"item": item, "kind": kind, "act": act, "reason": reason}))
        })
        .collect();
    if sent.is_empty() {
        return;
    }
    let answers: Vec<Value> = sent
        .iter()
        .map(|a| json!({"item": a["item"], "act": a["act"], "reason": a["reason"]}))
        .collect();
    let mut e = link.entry("plan_answer");
    e.view = Some(view.to_string());
    // the entry names what it sent, refused or not
    e.reply = Some(json!({ "answers": sent }));
    link.exec(
        e,
        "plan_answer",
        "rdy_plan",
        json!({"view": view, "answers": answers, "max_tokens": PLAN_ALL}),
        |_, _| {},
    );
}

/// The tension labels of a proposal's advised entries (`rdy_read P-x@n`'s `proposal.advised`):
/// open semantic tensions of the event-derived set.
pub(crate) fn advised_labels(proposal: &Value) -> Vec<String> {
    proposal["advised"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["tension"].as_str().map(str::to_string))
        .collect()
}

/// What a call met, as the journal records it, and the failure text when it is a refusal the
/// closed table for `rules` does not allow.
fn judge(rules: &str, res: &Res) -> (Outcome, Option<String>) {
    match res {
        Res::Ok(_) => (Outcome::Ok, None),
        Res::Refused(code, message) => (
            Outcome::Refused(code.clone()),
            (!allowed(rules, code)).then(|| format!("got {code}: {message}")),
        ),
        Res::Lost => (Outcome::Lost, None),
    }
}

/// The tension labels a reply lists as open: a write, view-op or propose reply's new tensions on
/// the actor's changes, or a listing's rows. A read-time intent overlap and the landing form of an
/// open proposal (a row with `proposal`) are left out: neither is a tension of the event-derived
/// set, so neither is ever logged (sg5).
pub(crate) fn labels_of(rows: &Value) -> Vec<String> {
    rows.as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["status"].as_str().is_none_or(|s| s == "open"))
        .filter(|t| t["read_time"] != true && t.get("proposal").is_none())
        .filter_map(|t| t["id"].as_str().map(str::to_string))
        .collect()
}

/// The tension label of an inbox `tension_opened` entry: `tn-` and the first 12 digits of the
/// tension's id (the first of the entry's `ids`).
pub(crate) fn inbox_labels(entries: &[Value]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| e["event"] == "tension_opened")
        .filter_map(|e| e["ids"][0].as_str())
        .map(|id| format!("tn-{}", &id[..id.len().min(12)]))
        .collect()
}

/// Letters for a number (`0` is `a`, `26` is `ba`): the generated texts hold no digits, which
/// the value detectors would read.
pub(crate) fn spell(mut n: usize) -> String {
    let mut out = Vec::new();
    loop {
        out.push((b'a' + (n % 26) as u8) as char);
        n /= 26;
        if n == 0 {
            break;
        }
    }
    out.iter().rev().collect()
}

/// The text of a claim that agent `name` writes in pass `pass` to cite `target`: each agent's
/// own words, so two citations of one block never share a normative text (a
/// `duplicate_definition`).
fn cite_text(name: &str, pass: usize, target: &str) -> String {
    format!(
        "Agent {name} builds on {{{{{target}}}}} in pass {}.",
        spell(pass)
    )
}

/// The area of every claim an agent created, by block id (a block id is unique in a project, and
/// a process may run several projects). An agent writing another agent's claim needs it for the
/// decision it implements.
fn area_registry() -> &'static Mutex<HashMap<String, usize>> {
    static R: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    R.get_or_init(Mutex::default)
}

pub(crate) fn area_of(block: &str) -> Option<usize> {
    area_registry()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(block)
        .copied()
}

/// What a read of a view found.
enum Shown {
    /// The daemon died under the read; the actor has reconnected and the act ends.
    Lost,
    /// No view of that name exists.
    Missing,
    /// The view: whether it is closed, the changes it includes that have not landed, and the
    /// blocks it holds conflicted.
    View {
        closed: bool,
        changes: Vec<String>,
        conflicts: BTreeSet<String>,
    },
}

/// The index of the first of `ids` that is not in `conflicted`, the blocks a view holds
/// conflicted: the target of a write that names no base (ruling task-5.3). `None` when every one
/// is conflicted.
pub fn first_unconflicted(ids: &[&str], conflicted: &BTreeSet<String>) -> Option<usize> {
    ids.iter().position(|id| !conflicted.contains(*id))
}

/// A block an act aims at, resolved.
struct Target {
    id: String,
    area: usize,
    /// The change of another agent that made the block, when the block came by an include.
    included: Option<String>,
}

/// A claim this agent created.
struct Own {
    id: String,
    area: usize,
    change: String,
}

struct Agent {
    link: Link,
    i: usize,
    view: String,
    seed: SeedBlocks,
    // inbox
    last_cursor: String,
    /// The cursor of the last read that acked, if one did: where a restart without a usable
    /// cursor begins.
    persisted: Option<String>,
    range: u32,
    resume: bool,
    // what this agent holds
    own: Vec<Own>,
    /// The claims this agent made that cite a block, with the block each cites.
    cites: Vec<(String, String)>,
    /// Included changes this agent's claims cite from: a body reference is no dependency, so
    /// excluding one would leave the reference with nothing to point at.
    pinned: BTreeSet<String>,
    included: Vec<String>,
    /// Changes of other agents that landed before this agent included them.
    dead: BTreeSet<String>,
    gone: BTreeSet<String>,
    /// Every change this agent's writes made.
    mine: BTreeSet<String>,
    wrote: bool,
    /// The prose of the scratch doc and the change that made it.
    scratch: Option<(String, String)>,
    alt_set: Option<String>,
    /// The views of this agent's marked claims that it has not yet proposed, oldest first.
    contra_views: Vec<String>,
    forks: usize,
    counter: usize,
    report: AgentReport,
}

/// Starts agent `i`: one thread, one proxy, the session `{actor: "agent:a<i>", run: "a<i>"}`
/// with the area of `i % 3`. The thread runs `acts` and returns the report.
pub fn spawn(
    p: &SwarmProject,
    cfg: &Config,
    i: usize,
    acts: Vec<Act>,
    journal: Journal,
    gate: Arc<KillGate>,
) -> JoinHandle<AgentReport> {
    spawn_paced(p, cfg, i, acts, journal, gate, None)
}

/// A wait before each step of an actor, given the step: it returns when the step may run.
pub(crate) type Pace = Arc<dyn Fn(usize) + Send + Sync>;

/// `spawn`, with a wait before each step (`pace`): a run keeps its agents within a step of
/// each other (sg5), so a reviewer still runs when a writer creates the claim it judges.
pub(crate) fn spawn_paced(
    p: &SwarmProject,
    cfg: &Config,
    i: usize,
    acts: Vec<Act>,
    journal: Journal,
    gate: Arc<KillGate>,
    pace: Option<Pace>,
) -> JoinHandle<AgentReport> {
    let at = Where::of(p);
    let seed = p.seed.clone();
    let cfg = *cfg;
    std::thread::spawn(move || {
        let name = format!("a{i}");
        let role = if cfg.is_reviewer(i) {
            "reviewer"
        } else {
            "writer"
        };
        let area = AREAS[i % AREAS.len()].to_string();
        let link = Link::connect(
            at,
            &format!("agent:{name}"),
            &name,
            role,
            Some(vec![area]),
            journal,
            gate,
        );
        let mut a = Agent::new(link, i, seed);
        a.start_inbox();
        for (step, act) in acts.iter().enumerate() {
            if let Some(pace) = &pace {
                pace(step);
            }
            a.link.step = step;
            a.act(act);
            if a.link.take_recovered() {
                a.resume = true;
                a.resync();
            }
        }
        a.report.last_cursor = a.last_cursor.clone();
        a.report.persisted = a.persisted.clone();
        a.report.range = a.range;
        a.report
    })
}

impl Agent {
    /// An agent in its starting state: view `a<i>`, nothing written, the inbox not yet read.
    fn new(link: Link, i: usize, seed: SeedBlocks) -> Agent {
        let name = format!("a{i}");
        let mut report = AgentReport::default();
        report.views.insert(name.clone());
        Agent {
            link,
            i,
            view: name,
            seed,
            last_cursor: String::new(),
            persisted: None,
            range: 0,
            resume: false,
            own: Vec::new(),
            cites: Vec::new(),
            pinned: BTreeSet::new(),
            included: Vec::new(),
            dead: BTreeSet::new(),
            gone: BTreeSet::new(),
            mine: BTreeSet::new(),
            wrote: false,
            scratch: None,
            alt_set: None,
            contra_views: Vec::new(),
            forks: 0,
            counter: 0,
            report,
        }
    }
}

impl Agent {
    /// Reads the inbox once to start it, and keeps the cursor it returns. A daemon that dies
    /// under the read is waited for, and the read is made again.
    fn start_inbox(&mut self) {
        for _ in 0..3 {
            match self.link.raw("start", "rdy_inbox", json!({})) {
                Res::Ok(v) => {
                    self.last_cursor = v["cursor"].as_str().unwrap_or_default().to_string();
                    return;
                }
                Res::Refused(code, message) => {
                    self.link.fail("start", &format!("got {code}: {message}"));
                }
                Res::Lost => {
                    self.link.recover();
                    self.link.take_recovered();
                }
            }
        }
        self.link
            .fail("start", "lost the daemon three times before the first call");
    }

    fn name(&self) -> String {
        spell(self.i)
    }

    /// After a kill the agent does not know whether the view op in flight (an undo, an exclude,
    /// an include) took effect. It reads its view and drops what the view no longer holds.
    /// The read is a journaled `view_show` (final review M7): a view the kill left unmade reads
    /// as empty, and any other refusal fails the run through the closed table.
    fn resync(&mut self) {
        if !self.wrote && self.included.is_empty() {
            return;
        }
        let view = self.view.clone();
        let held: BTreeSet<String> = match self.show(&view) {
            Shown::Lost => return,
            Shown::Missing => BTreeSet::new(),
            Shown::View { changes, .. } => changes.into_iter().collect(),
        };
        self.included.retain(|c| held.contains(c));
        self.gone = self
            .own
            .iter()
            .map(|o| o.change.clone())
            .chain(self.mine.iter().cloned())
            .filter(|c| !held.contains(c))
            .collect();
        if let Some((_, change)) = &self.scratch
            && self.gone.contains(change)
        {
            self.scratch = None;
        }
    }

    fn act(&mut self, act: &Act) {
        // an act that decides from a read of the agent's view holds the read turn from the read
        // to its call: a landing between them would make the read stale (fix round 1)
        // an alternative set is made in that turn too: the person never lands while a set is
        // made and not yet journaled
        let reads_first = matches!(
            act,
            Act::Propose
                | Act::Include { .. }
                | Act::Exclude
                | Act::WriteNoBrief { .. }
                | Act::WriteStale { .. }
                | Act::Write { stable: false, .. }
                | Act::AltCreate { .. }
                | Act::Tensions
        );
        let gate = self.link.gate.clone();
        let _turn = reads_first.then(|| gate.read_turn());
        // a propose answers the plan in its own turn first, then takes this one for its call
        let _act = (!matches!(act, Act::Propose)).then(|| gate.act_turn());
        match act {
            Act::Brief { target } => self.brief_act(target),
            Act::Write { target, stable } => self.write_act(target, *stable),
            Act::WriteNoBrief { target } => self.write_no_brief(target),
            Act::WriteStale { target } => self.write_stale(target),
            Act::Create { area, contra } => self.create(area, *contra),
            Act::Collide { with: _, target } => self.collide(target),
            Act::Cite { target } => self.cite(target),
            Act::Repin => self.repin(),
            Act::Fork => self.fork(),
            Act::Include { from } => self.include(*from),
            Act::Exclude => self.exclude(),
            Act::Undo => self.undo(),
            Act::Intent { area } => self.intent(area),
            Act::AltCreate { with } => self.alt_create(*with),
            Act::Propose => self.propose(),
            Act::ReviewNext => self.review_next(),
            Act::Tensions => self.tensions(),
            Act::Inbox { ack } => self.inbox(*ack),
            Act::Read { target } => self.read_act(target),
        }
    }

    // ---- picks ----

    /// The claims of `own` that are still in the view.
    fn own_alive(&self) -> impl Iterator<Item = &Own> {
        self.own.iter().filter(|o| !self.gone.contains(&o.change))
    }

    fn resolve(&self, pick: &Pick) -> Option<Target> {
        match pick {
            Pick::Seed(n) => {
                let (id, area) = self.seed.pick(*n);
                Some(Target {
                    id: id.to_string(),
                    area,
                    included: None,
                })
            }
            Pick::Own(n) => self.own_alive().nth(*n).map(|o| Target {
                id: o.id.clone(),
                area: o.area,
                included: None,
            }),
            Pick::Other { agent, nth } => {
                let who = format!("agent:a{agent}");
                self.link
                    .journal
                    .entries()
                    .into_iter()
                    .filter(|e| e.actor == who && matches!(e.act, "create" | "cite"))
                    .filter(|e| e.outcome == Outcome::Ok)
                    .filter(|e| {
                        e.change
                            .as_ref()
                            .is_some_and(|c| self.included.contains(c) && !self.gone.contains(c))
                    })
                    .filter_map(|e| e.block.zip(e.change))
                    .nth(*nth)
                    .and_then(|(id, change)| {
                        area_of(&id).map(|area| Target {
                            id,
                            area,
                            included: Some(change),
                        })
                    })
            }
        }
    }

    /// A pick that resolved to nothing: the agent reads a seed block instead.
    fn skipped(&mut self, act: &'static str) {
        let (id, _) = self.seed.pick(self.link.step);
        let id = id.to_string();
        let mut e = self.link.entry(act);
        e.block = Some(id.clone());
        e.outcome = Outcome::Skipped;
        match self.link.raw(act, "rdy_read", json!({"id": id})) {
            Res::Ok(_) => {}
            Res::Refused(code, message) => {
                self.link.fail(act, &format!("got {code}: {message}"));
            }
            Res::Lost => e.outcome = Outcome::Lost,
        }
        let lost = e.outcome == Outcome::Lost;
        self.link.journal.record(e);
        if lost {
            self.link.recover();
        }
    }

    /// The act found nothing to do. It is journaled `Skipped` with its reason and makes no call:
    /// the caller decided from a read of the state (`show`, `rdy_suspects`).
    fn skip(&mut self, act: &'static str, why: Skip) {
        let mut e = self.view_entry(act);
        e.outcome = Outcome::Skipped;
        e.skip = Some(why);
        self.link.journal.record(e);
    }

    /// `rdy_view {op: "show"}` of `view`, journaled as a `view_show` entry: what the executor
    /// reads before an act whose precondition is a fact about a view.
    fn show(&mut self, view: &str) -> Shown {
        let mut e = self.link.entry("view_show");
        e.view = Some(view.to_string());
        let args = json!({"op": "show", "view": view, "max_tokens": 100_000});
        let res = self.link.raw("view_show", "rdy_view", args);
        // a view nobody has written to does not exist: that is the answer, not a fault
        if matches!(&res, Res::Refused(code, _) if code == "E_NOT_FOUND") {
            e.outcome = Outcome::Refused("E_NOT_FOUND".to_string());
            self.link.journal.record(e);
            return Shown::Missing;
        }
        let mut shown = None;
        let res = self
            .link
            .finish(e, "view_show", res, |_, v| shown = Some(v.clone()));
        let (Res::Ok(_), Some(v)) = (res, shown) else {
            return Shown::Lost;
        };
        if v.get("omitted").is_some() {
            self.link
                .fail("view_show", "got a reply cut for its budget");
        }
        let ids = |key: &str, field: &str| -> Vec<String> {
            v[key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c[field].as_str().map(str::to_string))
                .collect()
        };
        Shown::View {
            closed: v["head"]["closed"].as_bool().unwrap_or(false),
            changes: ids("changes", "id"),
            conflicts: ids("conflicts", "block").into_iter().collect(),
        }
    }

    fn view_entry(&self, act: &'static str) -> Entry {
        let mut e = self.link.entry(act);
        e.view = Some(self.view.clone());
        e
    }

    fn decision(&self, area: usize) -> String {
        self.seed.decisions[area].clone()
    }

    // ---- briefing and writing ----

    /// `rdy_brief` on `ids`: the token and the version of each covered block. `None` after a
    /// kill (the act ends).
    fn brief_call(&mut self, ids: &[String]) -> Option<(String, Vec<Value>)> {
        self.brief_in(ids, None)
    }

    /// `brief_call` in view `scope` (the session's view when `None`).
    fn brief_in(&mut self, ids: &[String], scope: Option<&str>) -> Option<(String, Vec<Value>)> {
        let mut e = self.link.entry("brief");
        e.block = ids.first().cloned();
        let mut token = String::new();
        let mut blocks = Vec::new();
        let mut args = json!({"ids": ids});
        if let Some(v) = scope {
            args["scope"] = json!(v);
        }
        let res = self.link.exec(e, "brief", "rdy_brief", args, |en, v| {
            token = v["base"]["token"].as_str().unwrap_or_default().to_string();
            blocks = v["base"]["blocks"].as_array().cloned().unwrap_or_default();
            en.brief = Some(token.clone());
            en.seen = labels_of(&v["inflight"]["tensions"]);
        });
        matches!(res, Res::Ok(_)).then_some((token, blocks))
    }

    /// Briefs `ids`, then writes `ops` citing the token, as `entry.act` (the table row `rules`
    /// applies). On `E_BRIEF_STALE` it briefs again and writes once more. Returns the last
    /// reply. An entry whose view is not the session's (a marked claim's own view, which the
    /// write makes) briefs canon and writes in that view.
    fn briefed_write(
        &mut self,
        mut entry: Entry,
        rules: &'static str,
        ids: &[String],
        ops: Value,
        area: usize,
        retry: bool,
    ) -> Res {
        entry.stable = true;
        let mut tries = 0;
        // a marked claim's own view does not exist before its write, which makes it from canon:
        // the briefing reads canon
        let other = entry.view.clone().filter(|v| *v != self.view);
        let scope = other.as_ref().map(|_| "canon");
        loop {
            let Some((token, blocks)) = self.brief_in(ids, scope) else {
                return Res::Lost;
            };
            let mut ops = ops.clone();
            // an update names the version it was made from: the daemon refuses a block that two
            // sides changed unless the write says which version it edited
            if blocks.len() == 1 && ops[0]["op"] == "update" {
                let version = blocks[0]["version"].as_str().map(str::to_string);
                entry.base.clone_from(&version);
                if let Some(v) = version {
                    ops[0]["base"] = json!(v);
                }
            }
            entry.brief = Some(token.clone());
            let mut args = json!({"ops": ops, "brief": token, "implements": [self.decision(area)]});
            if let Some(v) = &other {
                args["view"] = json!(v);
            }
            let res = self.write_call(entry.clone(), rules, args);
            if retry
                && tries == 0
                && matches!(&res, Res::Refused(code, _) if code == "E_BRIEF_STALE")
            {
                tries += 1;
                continue;
            }
            return res;
        }
    }

    /// One `rdy_write`; an `Ok` reply names the change and the tensions it opened.
    fn write_call(&mut self, e: Entry, rules: &'static str, args: Value) -> Res {
        let res = self.link.exec(e, rules, "rdy_write", args, |en, v| {
            en.change = v["change"].as_str().map(str::to_string);
            en.seen = labels_of(&v["on_change"]["tensions"]);
            if let Some(id) = v["created"].as_object().and_then(|m| m.values().next()) {
                en.block = id.as_str().map(str::to_string);
            }
        });
        if let Res::Ok(v) = &res {
            self.wrote = true;
            if let Some(c) = v["change"].as_str() {
                self.mine.insert(c.to_string());
            }
        }
        res
    }

    fn brief_act(&mut self, target: &Pick) {
        let Some(t) = self.resolve(target) else {
            return self.skipped("brief");
        };
        let section = self.seed.sections[t.area].clone();
        let mut e = self.link.entry("brief");
        e.block = Some(t.id);
        self.link.exec(
            e,
            "brief",
            "rdy_brief",
            json!({"ids": [section]}),
            |en, v| {
                en.brief = v["base"]["token"].as_str().map(str::to_string);
                en.seen = labels_of(&v["inflight"]["tensions"]);
            },
        );
    }

    fn body(&mut self, what: &str) -> String {
        self.counter += 1;
        format!(
            "Agent {} {what} in pass {}.",
            self.name(),
            spell(self.counter)
        )
    }

    fn write_act(&mut self, target: &Pick, stable: bool) {
        if !stable {
            return self.scratch_write();
        }
        let Some(t) = self.resolve(target) else {
            return self.skipped("write");
        };
        let ops = json!([{"op": "update", "id": t.id, "body": self.body("reworks this rule")}]);
        let mut e = self.view_entry("write");
        e.block = Some(t.id.clone());
        self.briefed_write(e, "write", std::slice::from_ref(&t.id), ops, t.area, true);
    }

    /// A write to content that is not stable: the agent's own scratch doc, created at the first
    /// such write and updated after.
    fn scratch_write(&mut self) {
        let mut e = self.view_entry("write");
        e.stable = false;
        let body = self.body("jots a note");
        match self.scratch.clone() {
            None => {
                let name = format!("scratch-{}-{}", self.view, spell(self.counter));
                let ops = json!([
                    {"op": "create", "type": "doc", "id": "$d",
                     "fields": {"name": name, "title": "Scratch", "doc_type": "overview"}},
                    {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": body}
                ]);
                let mut made = None;
                let res = self
                    .link
                    .exec(e, "write", "rdy_write", json!({"ops": ops}), |en, v| {
                        en.change = v["change"].as_str().map(str::to_string);
                        en.block = v["created"]["$p"].as_str().map(str::to_string);
                        en.seen = labels_of(&v["on_change"]["tensions"]);
                        made = en.block.clone().zip(en.change.clone());
                    });
                if matches!(res, Res::Ok(_)) {
                    self.wrote = true;
                    if let Some((_, change)) = &made {
                        self.mine.insert(change.clone());
                    }
                    self.scratch = made;
                }
            }
            Some((id, _)) => {
                // the update names no base: a scratch block conflicted in the view is refused,
                // so the agent reads its view first (ruling task-5.3)
                let view = self.view.clone();
                let conflicted = match self.show(&view) {
                    Shown::Lost => return,
                    Shown::Missing => BTreeSet::new(),
                    Shown::View { conflicts, .. } => conflicts,
                };
                if first_unconflicted(&[id.as_str()], &conflicted).is_none() {
                    return self.skip("write", Skip::NoUnconflictedTarget);
                }
                e.block = Some(id.clone());
                let ops = json!([{"op": "update", "id": id, "body": body}]);
                self.write_call(e, "write", json!({"ops": ops}));
            }
        }
    }

    fn write_no_brief(&mut self, target: &Pick) {
        let Some(t) = self.resolve(target) else {
            return self.skipped("write_no_brief");
        };
        // the probe needs a live view: a closed view is refused before the briefing is looked
        // at. A view that does not exist yet is live: the write would create it.
        let view = self.view.clone();
        match self.show(&view) {
            Shown::Lost => return,
            Shown::View { closed: true, .. } => {
                return self.skip("write_no_brief", Skip::NoLiveView);
            }
            Shown::Missing | Shown::View { .. } => {}
        }
        let ops = json!([{"op": "update", "id": t.id, "body": self.body("reworks this rule")}]);
        let mut e = self.view_entry("write_no_brief");
        e.block = Some(t.id);
        e.stable = true;
        let args = json!({"ops": ops, "implements": [self.decision(t.area)]});
        if matches!(self.write_call(e, "write_no_brief", args), Res::Ok(_)) {
            self.link.fail(
                "write_no_brief",
                "landed a stable write that cites no briefing",
            );
        }
    }

    fn write_stale(&mut self, target: &Pick) {
        let Some(t) = self.resolve(target) else {
            return self.skipped("write_stale");
        };
        // the second write names no base, which a block conflicted in the view refuses: the
        // agent reads its view and aims at the pick, or else the first seed block, that its view
        // does not hold conflicted (ruling task-5.3)
        let view = self.view.clone();
        let conflicted = match self.show(&view) {
            Shown::Lost => return,
            Shown::Missing => BTreeSet::new(),
            Shown::View { conflicts, .. } => conflicts,
        };
        let mut candidates = vec![t];
        for n in 0..SEED_BLOCKS {
            let (id, area) = self.seed.pick(n);
            candidates.push(Target {
                id: id.to_string(),
                area,
                included: None,
            });
        }
        let ids: Vec<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
        let Some(k) = first_unconflicted(&ids, &conflicted) else {
            return self.skip("write_stale", Skip::NoUnconflictedTarget);
        };
        let t = candidates.swap_remove(k);
        let Some((token, blocks)) = self.brief_call(std::slice::from_ref(&t.id)) else {
            return;
        };
        let base = blocks
            .first()
            .and_then(|b| b["version"].as_str().map(str::to_string));
        let mut e = self.view_entry("write_stale");
        e.block = Some(t.id.clone());
        e.stable = true;
        e.brief = Some(token.clone());
        e.base.clone_from(&base);
        let ops = json!([{"op": "update", "id": t.id, "base": base,
            "body": self.body("reworks this rule")}]);
        let args = json!({"ops": ops, "brief": token, "implements": [self.decision(t.area)]});
        // the first write may meet what any write meets; only the second is held to the table row
        if !matches!(
            self.write_call(e.clone(), "write", args.clone()),
            Res::Ok(_)
        ) {
            return;
        }
        let mut args = args;
        args["ops"] =
            json!([{"op": "update", "id": t.id, "body": self.body("reworks this rule again")}]);
        if matches!(self.write_call(e, "write_stale", args), Res::Ok(_)) {
            self.link.fail(
                "write_stale",
                "landed a second write on a briefing the first write made stale",
            );
        }
    }

    fn create(&mut self, area: &str, contra: Option<usize>) {
        let k = AREAS.iter().position(|a| *a == area).unwrap_or(0);
        let text = match contra {
            Some(c) => {
                let verb = if self.i.is_multiple_of(2) {
                    "requires"
                } else {
                    "forbids"
                };
                format!(
                    "Agent {} {verb} that settlement is at once. CONTRA-{c}",
                    self.name()
                )
            }
            None => self.body("adds a rule"),
        };
        let ops = json!([{"op": "create", "type": "claim", "id": "$n",
            "parent": self.seed.sections[k], "body": text}]);
        let mut e = self.view_entry("create");
        // a marked claim goes in a view of its own, which the agent's next `Propose` proposes
        let own_view = contra.map(|c| format!("{}-k{}", self.view, spell(c)));
        if let Some(v) = &own_view {
            e.view = Some(v.clone());
        }
        let section = self.seed.sections[k].clone();
        let res = self.briefed_write(e, "create", &[section], ops, k, true);
        match (own_view, &res) {
            (None, _) => self.take_created(&res, k, None),
            (Some(_), Res::Refused(..)) => {}
            // a write the kill cut may have made the view: the drain and the next `Propose` look
            (Some(v), _) => {
                if let Res::Ok(r) = &res
                    && let Some(id) = r["created"]["$n"].as_str()
                {
                    area_registry()
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(id.to_string(), k);
                }
                self.report.views.insert(v.clone());
                self.contra_views.push(v);
            }
        }
    }

    /// Notes the claim a create or cite made (`$n` or `$y` in the reply's `created`).
    fn take_created(&mut self, res: &Res, area: usize, cites: Option<&str>) {
        let Res::Ok(v) = res else { return };
        let (Some(id), Some(change)) = (
            v["created"]
                .as_object()
                .and_then(|m| m.values().next())
                .and_then(Value::as_str),
            v["change"].as_str(),
        ) else {
            return;
        };
        area_registry()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_string(), area);
        self.own.push(Own {
            id: id.to_string(),
            area,
            change: change.to_string(),
        });
        if let Some(target) = cites {
            self.cites.push((id.to_string(), target.to_string()));
        }
    }

    fn collide(&mut self, target: &Pick) {
        let Some(t) = self.resolve(target) else {
            return self.skipped("collide");
        };
        let ops =
            json!([{"op": "update", "id": t.id, "body": self.body("fixes this rule its own way")}]);
        let mut e = self.view_entry("collide");
        e.block = Some(t.id.clone());
        self.briefed_write(e, "collide", std::slice::from_ref(&t.id), ops, t.area, true);
    }

    fn cite(&mut self, target: &Pick) {
        let Some(t) = self.resolve(target) else {
            return self.skipped("cite");
        };
        self.counter += 1;
        let body = cite_text(&self.name(), self.counter, &t.id);
        let ops = json!([{"op": "create", "type": "claim", "id": "$y",
            "parent": self.seed.sections[t.area], "body": body}]);
        let e = self.view_entry("cite");
        let section = self.seed.sections[t.area].clone();
        let res = self.briefed_write(e, "cite", &[section], ops, t.area, true);
        self.take_created(&res, t.area, Some(&t.id));
        if let (Res::Ok(_), Some(change)) = (&res, t.included) {
            self.pinned.insert(change);
        }
    }

    fn repin(&mut self) {
        // a repin has something to do for a claim of this agent whose cited block changed since
        // the claim pinned it; `rdy_suspects` names those, with the reason `changed`
        let alive: Vec<String> = self
            .cites
            .iter()
            .filter(|(id, _)| self.own_alive().any(|o| o.id == *id))
            .map(|(id, _)| id.clone())
            .collect();
        if alive.is_empty() {
            return self.skip("repin", Skip::NothingToRepin);
        }
        let mut changed: BTreeSet<String> = BTreeSet::new();
        let e = self.link.entry("suspects");
        let res = self.link.exec(
            e,
            "suspects",
            "rdy_suspects",
            json!({"max_tokens": 100_000}),
            |_, v| {
                changed.extend(
                    v["items"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|s| s["reason"] == "changed")
                        .filter_map(|s| s["block"].as_str().map(str::to_string)),
                );
            },
        );
        let Res::Ok(reply) = res else { return };
        if reply.get("omitted").is_some() {
            self.link.fail("suspects", "got a reply cut for its budget");
        }
        let Some(id) = alive.into_iter().rev().find(|id| changed.contains(id)) else {
            return self.skip("repin", Skip::NothingToRepin);
        };
        let area = area_of(&id).unwrap_or(0);
        let ops = json!([{"op": "repin", "id": id}]);
        let mut e = self.view_entry("repin");
        e.block = Some(id.clone());
        self.briefed_write(e, "repin", &[id], ops, area, true);
    }
}

/// The `as-` set id (12 hex digits) a refusal names, if any.
fn set_in(text: &str) -> Option<String> {
    let at = text.find("as-")?;
    let id: String = text[at..].chars().take(3 + 12).collect();
    (id.len() == 15 && id[3..].chars().all(|c| c.is_ascii_hexdigit())).then_some(id)
}

/// The arguments of `rdy_review_submit` for a leased case (ledger O4). When both blocks of the
/// case hold the same `CONTRA-<k>` marker the verdict is `tension`, with one citation per block.
/// For an even `k` it advises blocking: `severity: "blocking"` and a `reason`. For an odd `k` it
/// advises nothing: no `severity` and no `reason` are sent. Any other case is `no_tension`.
pub(crate) fn verdict_args(request: &Value, case_hash: &Value, blocks: &[Value]) -> Value {
    let markers: Vec<Option<usize>> = blocks
        .iter()
        .map(|b| {
            let text: String = b["text"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| t["text"].as_str())
                .collect::<Vec<_>>()
                .join(" ");
            marker(&text)
        })
        .collect();
    let mut args = json!({"request": request, "case_hash": case_hash});
    match markers.as_slice() {
        [Some(a), Some(b)] if a == b => {
            let k = *a;
            args["verdict"] = json!("tension");
            args["citations"] = json!(
                blocks
                    .iter()
                    .map(|b| json!({"block": b["id"],
                        "note": format!("these two cannot both hold: CONTRA-{k}")}))
                    .collect::<Vec<_>>()
            );
            if k % 2 == 0 {
                args["severity"] = json!("blocking");
                args["reason"] = json!(format!("scripted advice CONTRA-{k}"));
            }
        }
        _ => args["verdict"] = json!("no_tension"),
    }
    args
}

/// The `CONTRA-<k>` marker in `text`: `k`.
fn marker(text: &str) -> Option<usize> {
    let at = text.find("CONTRA-")? + "CONTRA-".len();
    let digits: String = text[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// The views that belong to an alternative set some agent created, per the journal. A change
/// written in one stays bound to its set, whatever view includes it, and the including view
/// cannot land.
fn views_in_sets(journal: &Journal) -> BTreeSet<String> {
    journal
        .entries()
        .iter()
        .filter(|e| e.act == "alt_create" && e.outcome == Outcome::Ok)
        .flat_map(|e| e.view.iter().chain(e.block.iter()).cloned())
        .collect()
}

impl Agent {
    fn fork(&mut self) {
        if !self.wrote {
            return self.skipped("fork");
        }
        self.forks += 1;
        let name = format!("{}-f{}", self.view, spell(self.forks));
        let e = self.view_entry("fork");
        let args = json!({"op": "fork", "from": self.view, "name": name});
        if matches!(
            self.link.exec(e, "fork", "rdy_view", args, |_, _| {}),
            Res::Ok(_)
        ) {
            self.report.views.insert(name);
        }
    }

    fn include(&mut self, from: usize) {
        let who = format!("agent:a{from}");
        let theirs = format!("a{from}");
        let entries = self.link.journal.entries();
        let in_a_set = views_in_sets(&self.link.journal);
        let candidates: Vec<String> = entries
            .into_iter()
            .rev()
            .filter(|_| from != self.i && !in_a_set.contains(&theirs))
            .filter(|e| e.actor == who && e.outcome == Outcome::Ok)
            .filter(|e| e.view.as_deref() == Some(theirs.as_str()))
            // only a claim the agent created: it depends on nothing but canon. A claim that cites
            // a block only its author's view holds would leave the including view's proposal
            // with a reference to nothing in canon, and a change that updates a block drags in
            // the changes it depends on, which may be bound to an alternative set
            .filter(|e| e.act == "create")
            .filter_map(|e| e.change)
            .filter(|c| !self.included.contains(c) && !self.dead.contains(c))
            .collect();
        if candidates.is_empty() {
            return self.skip("include", Skip::NothingToInclude);
        }
        // the change must still be in its author's view: one that landed is in canon, and an
        // include of it is refused
        let changes = match self.show(&theirs) {
            Shown::Lost => return,
            Shown::Missing => Vec::new(),
            Shown::View { changes, .. } => changes,
        };
        let live = candidates.iter().find(|c| changes.contains(c)).cloned();
        for c in candidates {
            if !changes.contains(&c) {
                self.dead.insert(c);
            }
        }
        match live {
            Some(change) => self.include_change(change),
            None => self.skip("include", Skip::NothingToInclude),
        }
    }

    /// `rdy_view include` of one change of another agent's view.
    fn include_change(&mut self, change: String) {
        let e = self.view_entry("include");
        let args = json!({"op": "include", "changes": [change]});
        let named = change.clone();
        if let Res::Ok(_) = self.link.exec(e, "include", "rdy_view", args, |en, v| {
            en.seen = labels_of(&v["on_change"]["tensions"]);
            // the change it included, which a coverage check reads (fix round 2)
            en.reply = Some(json!({"included": [named]}));
        }) {
            self.gone.remove(&change);
            if !self.included.contains(&change) {
                self.included.push(change);
            }
        }
    }

    fn exclude(&mut self) {
        let Some(change) = self
            .included
            .iter()
            .rev()
            .find(|c| !self.pinned.contains(*c))
            .cloned()
        else {
            return self.skip("exclude", Skip::NothingToExclude);
        };
        // the change must still be in the agent's view: one that landed has left it
        let view = self.view.clone();
        match self.show(&view) {
            Shown::Lost => return,
            Shown::View { changes, .. } if changes.contains(&change) => {}
            // a closed view is refused by the call, and the table allows that refusal
            Shown::View { closed: true, .. } => {}
            Shown::Missing | Shown::View { .. } => {
                self.included.retain(|c| *c != change);
                return self.skip("exclude", Skip::NothingToExclude);
            }
        }
        let e = self.view_entry("exclude");
        let args = json!({"op": "exclude", "changes": [change]});
        let res = self.link.exec(e, "exclude", "rdy_view", args, |en, v| {
            en.seen = labels_of(&v["on_change"]["tensions"]);
        });
        if matches!(res, Res::Ok(_)) {
            self.included.retain(|c| *c != change);
        }
    }

    fn undo(&mut self) {
        if !self.wrote {
            return self.skipped("undo");
        }
        let e = self.view_entry("undo");
        let res = self
            .link
            .exec(e, "undo", "rdy_view", json!({"op": "undo"}), |en, v| {
                en.seen = labels_of(&v["on_change"]["tensions"]);
            });
        let Res::Ok(v) = res else { return };
        let list = |key: &str| -> Vec<String> {
            v[key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c.as_str().map(str::to_string))
                .collect()
        };
        for c in list("excluded") {
            self.included.retain(|x| *x != c);
            self.gone.insert(c);
        }
        for c in list("included") {
            self.gone.remove(&c);
            if !self.mine.contains(&c) && !self.included.contains(&c) {
                self.included.push(c);
            }
        }
        if let Some((_, change)) = &self.scratch
            && self.gone.contains(change)
        {
            self.scratch = None;
        }
    }

    fn intent(&mut self, area: &str) {
        let e = self.view_entry("intent");
        let args = json!({"op": "post", "area": area,
            "angle": format!("settle the {area} rules"), "ttl": "30m"});
        self.link.exec(e, "intent", "rdy_intent", args, |_, _| {});
    }

    fn alt_create(&mut self, with: usize) {
        let who = format!("agent:a{with}");
        let other_wrote = self
            .link
            .journal
            .entries()
            .iter()
            .any(|e| e.actor == who && e.outcome == Outcome::Ok && e.change.is_some());
        if with == self.i || !self.wrote || !other_wrote {
            return self.skipped("alt_create");
        }
        let other = format!("a{with}");
        let mut e = self.view_entry("alt_create");
        e.block = Some(other.clone());
        let args = json!({"op": "create", "title": "which rule wins", "members": [
            {"label": self.view, "view": self.view}, {"label": other, "view": other}]});
        if let Res::Ok(v) = self
            .link
            .exec(e, "alt_create", "rdy_alternatives", args, |_, _| {})
        {
            self.alt_set = v["set"]["id"].as_str().map(str::to_string);
        }
    }

    /// Proposes the oldest view of a marked claim not yet proposed, else the agent's view. A live
    /// view's change plan is answered first (`answer_plan`), so the version binds the answers.
    fn propose(&mut self) {
        let (contra, view, closed) = loop {
            let contra = self.contra_views.first().cloned();
            let view = contra.clone().unwrap_or_else(|| self.view.clone());
            let (nothing, closed) = match self.show(&view) {
                Shown::Lost => return,
                Shown::Missing => (true, false),
                Shown::View {
                    closed: false,
                    changes,
                    ..
                } => (changes.is_empty(), false),
                // a closed view is refused by the call, and the table allows that refusal
                Shown::View { .. } => (false, true),
            };
            match (nothing, &contra) {
                (false, _) => break (contra, view, closed),
                // a marked claim the kill cut never made its view: the agent's own view is next
                (true, Some(_)) => {
                    self.contra_views.remove(0);
                }
                (true, None) => return self.skip("propose", Skip::NothingToPropose),
            }
        };
        if !closed {
            answer_plan(&mut self.link, &view);
        }
        // the propose's own call can change a tension (a key change rdy adds)
        let gate = self.link.gate.clone();
        let _act = gate.act_turn();
        let title = format!("{view} step {}", self.link.step);
        let mut args = json!({"title": title, "summary": title, "view": view});
        if contra.is_none()
            && let Some(set) = &self.alt_set
        {
            args["alternatives"] = json!({"set": set, "chosen": self.view});
        }
        for attempt in 0..2 {
            let mut e = self.view_entry("propose");
            e.view = Some(view.clone());
            let mut made = None;
            // the entry names the choice it made (null: none), which the person reads
            let chose = args["alternatives"].clone();
            let res = self
                .link
                .exec(e, "propose", "rdy_propose", args.clone(), |en, v| {
                    en.block = v["ref"].as_str().map(str::to_string);
                    en.seen = labels_of(&v["tensions"]);
                    en.reply = Some(json!({"alternatives": chose}));
                    made = en.block.clone();
                });
            match res {
                Res::Ok(_) => {
                    if let Some(r) = made
                        && !self.report.proposals.contains(&r)
                    {
                        self.report.proposals.push(r);
                    }
                    if contra.is_some() {
                        self.contra_views.remove(0);
                    }
                }
                // the view is a member of a set: the refusal names it, and the agent chooses
                Res::Refused(code, message)
                    if code == "E_ALT_UNCHOSEN" && attempt == 0 && contra.is_none() =>
                {
                    if let Some(set) = set_in(&message) {
                        args["alternatives"] = json!({"set": set, "chosen": self.view});
                        self.alt_set = Some(set);
                        continue;
                    }
                }
                // the set is not open any more: the agent forgets it and, as the refusal's fix
                // says ("only an open set takes a choice: propose without alternatives"),
                // proposes again at once without a choice, so a winner's later work reaches the
                // gate in the run (fix round 2, ruling task-5-round1.2)
                Res::Refused(code, _) if code == "E_ALT_MEMBER" && contra.is_none() => {
                    self.alt_set = None;
                    if attempt == 0
                        && let Some(a) = args.as_object_mut()
                        && a.remove("alternatives").is_some()
                    {
                        continue;
                    }
                }
                _ => {}
            }
            return;
        }
    }

    /// Judges up to `REVIEW_BATCH` free review requests: each a `rdy_review_next` and a
    /// `rdy_review_submit` on the case it leased. It stops when none is free.
    fn review_next(&mut self) {
        for _ in 0..REVIEW_BATCH {
            if !self.review_one() {
                return;
            }
        }
    }

    /// One review: whether a case was leased and judged.
    fn review_one(&mut self) -> bool {
        let e = self.link.entry("review_next");
        let mut case: Option<Value> = None;
        let res = self.link.exec(
            e,
            "review_next",
            "rdy_review_next",
            json!({"kind": "tension"}),
            |_, v| {
                if v["request"].is_string() {
                    case = Some(v.clone());
                }
            },
        );
        let (Res::Ok(_), Some(leased)) = (res, case) else {
            return false;
        };
        let blocks = leased["case"]["blocks"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let args = verdict_args(&leased["request"], &leased["case_hash"], &blocks);
        let e = self.link.entry("review_submit");
        let res = self
            .link
            .exec(e, "review_submit", "rdy_review_submit", args, |en, v| {
                en.seen = v["tension"]
                    .as_str()
                    .map(str::to_string)
                    .into_iter()
                    .collect();
                en.reply = Some(v.clone());
            });
        !matches!(res, Res::Lost)
    }

    /// `rdy_tensions` on the agent's own in-flight work: the tensions involving the last change
    /// its view holds (`involving`, read from the view first), which keeps the read inside its
    /// budget in the deep tier (ruling task-5.5). With no change in flight (no view, an empty one,
    /// or a closed one) the agent reads canon's tensions of one kind, the kind going round with
    /// the step.
    fn tensions(&mut self) {
        let view = self.view.clone();
        // a closed view's changes are not in flight (a member that lost its set)
        let last = match self.show(&view) {
            Shown::Lost => return,
            Shown::Missing | Shown::View { closed: true, .. } => None,
            Shown::View { changes, .. } => changes.last().cloned(),
        };
        let args = match last {
            Some(c) => json!({"involving": c, "max_tokens": 100_000}),
            None => {
                let kind = KINDS[self.link.step % KINDS.len()];
                json!({"scope": "canon", "kind": kind, "max_tokens": 100_000})
            }
        };
        let e = self.link.entry("tensions");
        let res = self
            .link
            .exec(e, "tensions", "rdy_tensions", args, |en, v| {
                en.seen = labels_of(&v["tensions"]);
            });
        if let Res::Ok(v) = res
            && v.get("omitted").is_some()
        {
            self.link.fail("tensions", "got a reply cut for its budget");
        }
    }

    fn inbox(&mut self, ack: bool) {
        let resume = std::mem::take(&mut self.resume);
        let act = if resume { "inbox_resume" } else { "inbox" };
        let since = self.last_cursor.clone();
        let range = self.range;
        let mut cursor = None;
        let e = self.link.entry(act);
        let args = json!({"since": since, "ack": ack, "max_tokens": 100_000});
        let res = self.link.exec(e, act, "rdy_inbox", args, |en, v| {
            let entries = v["entries"].as_array().cloned().unwrap_or_default();
            let c = v["cursor"].as_str().unwrap_or_default().to_string();
            en.seen = inbox_labels(&entries);
            en.inbox = Some(InboxRead {
                since: since.clone(),
                cursor: c.clone(),
                entries,
                range,
            });
            cursor = Some(c);
        });
        self.persisted = persisted_after(self.persisted.take(), ack, &res, cursor.clone());
        match res {
            Res::Ok(_) => self.last_cursor = cursor.unwrap_or_default(),
            // the kill dropped a batch this agent had read: the cursor is past the end of the
            // log. It reads from the cursor the daemon persisted, and that starts a new range.
            // An unknown persisted cursor is journaled as an empty `since`: the range's start
            // cannot be checked (final review M7)
            Res::Refused(code, _) if resume && code == "E_BAD_INPUT" => {
                self.range += 1;
                let since = self.persisted.clone().unwrap_or_default();
                let range = self.range;
                let mut cursor = None;
                let e = self.link.entry("inbox");
                let res = self.link.exec(e, "inbox", "rdy_inbox", json!({}), |en, v| {
                    let entries = v["entries"].as_array().cloned().unwrap_or_default();
                    let c = v["cursor"].as_str().unwrap_or_default().to_string();
                    en.seen = inbox_labels(&entries);
                    en.inbox = Some(InboxRead {
                        since: since.clone(),
                        cursor: c.clone(),
                        entries,
                        range,
                    });
                    cursor = Some(c);
                });
                if matches!(res, Res::Ok(_))
                    && let Some(c) = cursor
                {
                    self.last_cursor = c;
                }
            }
            _ => {}
        }
    }

    fn read_act(&mut self, target: &Pick) {
        let Some(t) = self.resolve(target) else {
            return self.skipped("read");
        };
        let mut e = self.link.entry("read");
        e.block = Some(t.id.clone());
        self.link
            .exec(e, "read", "rdy_read", json!({"id": t.id}), |_, _| {});
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::project;

    fn block(id: &str, text: &str) -> Value {
        json!({"id": id, "text": [{"text": text}]})
    }

    fn panic_message(f: impl FnOnce()) -> String {
        let err = catch_unwind(AssertUnwindSafe(f)).expect_err("the call must panic");
        panic_text(&*err)
    }

    #[test]
    fn sg4_an_even_contra_pair_advises_blocking_with_a_reason() {
        let blocks = [block("b-1", "x CONTRA-2"), block("b-2", "y CONTRA-2")];
        let args = verdict_args(&json!("rq-1"), &json!("h"), &blocks);
        assert_eq!(args["verdict"], "tension");
        assert_eq!(args["severity"], "blocking");
        assert_eq!(args["reason"], "scripted advice CONTRA-2");
        assert_eq!(args["citations"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            (&args["request"], &args["case_hash"]),
            (&json!("rq-1"), &json!("h"))
        );
    }

    #[test]
    fn sg4_an_odd_contra_pair_sends_no_severity_and_no_reason() {
        let blocks = [block("b-1", "x CONTRA-3"), block("b-2", "y CONTRA-3")];
        let args = verdict_args(&json!("rq-1"), &json!("h"), &blocks);
        assert_eq!(args["verdict"], "tension");
        assert_eq!(args["citations"].as_array().map(Vec::len), Some(2));
        let sent = args.as_object().expect("an object");
        assert!(!sent.contains_key("severity"), "{args}");
        assert!(!sent.contains_key("reason"), "{args}");
    }

    #[test]
    fn sg4_a_case_without_one_shared_marker_is_no_tension() {
        for blocks in [
            [block("b-1", "x CONTRA-2"), block("b-2", "y CONTRA-4")],
            [block("b-1", "x CONTRA-2"), block("b-2", "y")],
            [block("b-1", "x"), block("b-2", "y")],
        ] {
            let args = verdict_args(&json!("rq-1"), &json!("h"), &blocks);
            assert_eq!(args["verdict"], "no_tension", "{args}");
            let sent = args.as_object().expect("an object");
            assert!(!sent.contains_key("severity") && !sent.contains_key("citations"));
        }
    }

    /// Ruling task-4.1: the five refusals that were once skipped by their message are refusals
    /// again. Each one is outside the closed table, so the run fails with them.
    #[test]
    fn sg4_a_refusal_is_checked_against_the_table_and_never_skipped_by_its_message() {
        let rows = [
            ("write_no_brief", "E_VIEW_CLOSED", "view \"a0\" is closed"),
            (
                "propose",
                "E_BAD_INPUT",
                "view \"a0\" has no changes to propose",
            ),
            (
                "repin",
                "E_BAD_INPUT",
                "c-1 already pins it; nothing to repin",
            ),
            ("include", "E_BAD_INPUT", "change 1 already landed in 2"),
            ("exclude", "E_BAD_INPUT", "change 1 is not in view \"a0\""),
        ];
        for (act, code, message) in rows {
            let res = Res::Refused(code.to_string(), message.to_string());
            let (outcome, bad) = judge(act, &res);
            assert_eq!(outcome, Outcome::Refused(code.to_string()), "{act}");
            assert_eq!(bad, Some(format!("got {code}: {message}")), "{act}");
        }
        // a refusal the table lists is journaled and does not fail the run
        let res = Res::Refused("E_VIEW_CLOSED".into(), "closed".into());
        assert_eq!(judge("propose", &res).1, None);
    }

    #[test]
    fn sg4_a_lost_call_is_accepted_only_in_the_kill_window() {
        assert!(
            may_lose(true, true, 0),
            "the kill has started and has not been survived"
        );
        assert!(!may_lose(true, true, 1), "after the restart");
        assert!(!may_lose(false, false, 0), "no kill is expected");
        assert!(!may_lose(false, false, 1));
        assert!(KillGate::with_kill().expects_kill());
        assert!(!KillGate::new().expects_kill());
    }

    /// Final review M5 (minors item 20): before the run starts its kill, a lost daemon is a
    /// product crash, not the scripted kill.
    #[test]
    fn sg5_a_lost_call_before_the_kill_starts_fails_the_run() {
        assert!(!may_lose(true, false, 0), "the kill has not started");
        let gate = KillGate::with_kill();
        assert!(!gate.is_killing());
        gate.killing();
        assert!(gate.is_killing());
        gate.restarted();
        assert_eq!(gate.epoch(), 1);
    }

    /// Final review M7 (minors item 17): after an ack whose reply the kill cut, the agent does
    /// not know the cursor the daemon persisted.
    #[test]
    fn sg5_a_lost_ack_leaves_the_persisted_cursor_unknown() {
        let old = Some("1.2.0".to_string());
        let new = Some("1.9.0".to_string());
        let ok = Res::Ok(json!({}));
        assert_eq!(persisted_after(old.clone(), true, &ok, new.clone()), new);
        assert_eq!(persisted_after(old.clone(), false, &ok, new.clone()), old);
        assert_eq!(persisted_after(old.clone(), true, &Res::Lost, None), None);
        assert_eq!(persisted_after(old.clone(), false, &Res::Lost, None), old);
        let refused = Res::Refused("E_BAD_INPUT".into(), "past the end".into());
        assert_eq!(persisted_after(old.clone(), true, &refused, None), old);
    }

    /// Final review M7 (minors item 15): a proposal read names the advised tensions it saw.
    #[test]
    fn sg5_a_proposal_read_names_its_advised_tensions() {
        let p = json!({"advised": [
            {"tension": "tn-aaa", "accepted": false},
            {"tension": "tn-bbb", "accepted": true},
        ]});
        assert_eq!(
            advised_labels(&p),
            vec!["tn-aaa".to_string(), "tn-bbb".to_string()]
        );
        assert!(advised_labels(&json!({})).is_empty());
    }

    #[test]
    fn sg4_the_wait_for_a_restart_names_the_seed_and_ends_at_the_restart() {
        let gate = KillGate::with_kill();
        let text = panic_message(|| gate.wait_restarted_for(31, Duration::from_millis(50)));
        assert!(
            text.starts_with("swarm seed 31: the daemon was killed and the run never restarted"),
            "{text}"
        );
        gate.restarted();
        gate.wait_restarted_for(31, Duration::from_millis(50));
        assert_eq!(gate.epoch(), 1);
    }

    /// sg5 (seed 1): a listing's read-time overlaps and landing forms are not tensions of the
    /// event-derived set, so `seen` leaves them out and the log check reads only logged kinds.
    #[test]
    fn sg5_seen_labels_leave_out_read_time_overlaps_and_landing_forms() {
        let rows = json!([
            {"id": "tn-1", "status": "open"},
            {"id": "tn-2", "status": "open", "read_time": true},
            {"id": "tn-3", "status": "open", "proposal": "P-abc@2"},
            {"id": "tn-4", "status": "dismissed"},
            {"id": "tn-5"},
        ]);
        assert_eq!(
            labels_of(&rows),
            vec!["tn-1".to_string(), "tn-5".to_string()]
        );
    }

    /// sg5 (seed 1): two agents that cite one block write different texts, so their claims never
    /// make a `duplicate_definition` of the same normative text, which no drain rule resolves.
    #[test]
    fn sg5_two_agents_cite_one_block_in_different_words() {
        let (a, b) = (cite_text("a", 1, "c-X"), cite_text("b", 1, "c-X"));
        assert_ne!(a, b);
        assert_ne!(cite_text("a", 1, "c-X"), cite_text("a", 2, "c-X"));
        for text in [a, b] {
            assert!(text.contains("{{c-X}}"), "{text}");
            assert!(!text.chars().any(|c| c.is_ascii_digit()), "{text}");
        }
    }

    /// A propose on a view with nothing in it (a write that was undone) is a refusal outside the
    /// table when the executor does not check first: it fails with the seed.
    #[test]
    fn sg4_a_forced_propose_on_an_empty_view_fails_the_run_with_the_seed() {
        let cfg = Config {
            seed: 11,
            agents: 2,
            steps: 12,
            kill_at: None,
        };
        let p = project::build(&cfg);
        let journal = Journal::new();
        let mut link = Link::connect(
            Where::of(&p),
            "agent:a0",
            "a0",
            "writer",
            Some(vec!["payments".to_string()]),
            journal.clone(),
            Arc::new(KillGate::new()),
        );
        let ops = json!([
            {"op": "create", "type": "doc", "id": "$d",
             "fields": {"name": "scratch-forced", "title": "Scratch", "doc_type": "overview"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "A note."}
        ]);
        let e = link.entry("write");
        assert!(matches!(
            link.exec(e, "write", "rdy_write", json!({"ops": ops}), |_, _| {}),
            Res::Ok(_)
        ));
        let e = link.entry("undo");
        assert!(matches!(
            link.exec(e, "undo", "rdy_view", json!({"op": "undo"}), |_, _| {}),
            Res::Ok(_)
        ));
        link.step = 3;
        let text = panic_message(|| {
            let e = link.entry("propose");
            link.exec(
                e,
                "propose",
                "rdy_propose",
                json!({"title": "empty"}),
                |_, _| {},
            );
        });
        assert!(
            text.starts_with("swarm seed 11: agent:a0 step 3 (propose) got E_BAD_INPUT"),
            "{text}"
        );
        let last = journal.entries().pop().expect("an entry");
        assert_eq!(
            (last.act, last.outcome),
            ("propose", Outcome::Refused("E_BAD_INPUT".into()))
        );
    }

    /// A read of a view nobody has written to is a refusal the daemon answered: the journal
    /// records `E_NOT_FOUND` for it, never an `Ok` the oracles would count as a read.
    #[test]
    fn sg4_a_show_of_a_missing_view_is_journaled_as_a_refusal() {
        let cfg = Config {
            seed: 12,
            agents: 2,
            steps: 12,
            kill_at: None,
        };
        let p = project::build(&cfg);
        let journal = Journal::new();
        let link = Link::connect(
            Where::of(&p),
            "agent:a0",
            "a0",
            "writer",
            Some(vec!["payments".to_string()]),
            journal.clone(),
            Arc::new(KillGate::new()),
        );
        let mut agent = Agent::new(link, 0, p.seed.clone());
        assert!(matches!(agent.show("nobody-wrote-here"), Shown::Missing));
        let last = journal.entries().pop().expect("an entry");
        assert_eq!(
            (last.act, last.outcome, last.view.as_deref()),
            (
                "view_show",
                Outcome::Refused("E_NOT_FOUND".into()),
                Some("nobody-wrote-here")
            )
        );
    }

    /// Final review M7 (minors item 16): the read a resync makes after a kill is journaled and
    /// held to the closed table, like every other call.
    #[test]
    fn sg5_a_resync_journals_its_read_of_the_view() {
        let cfg = Config {
            seed: 13,
            agents: 2,
            steps: 12,
            kill_at: None,
        };
        let p = project::build(&cfg);
        let journal = Journal::new();
        let link = Link::connect(
            Where::of(&p),
            "agent:a0",
            "a0",
            "writer",
            Some(vec!["payments".to_string()]),
            journal.clone(),
            Arc::new(KillGate::new()),
        );
        let mut agent = Agent::new(link, 0, p.seed.clone());
        // a write the kill may have cut: the view may not exist
        agent.wrote = true;
        agent.resync();
        let last = journal
            .entries()
            .pop()
            .expect("the resync's read is journaled");
        assert_eq!(
            (last.act, last.outcome, last.view.as_deref()),
            (
                "view_show",
                Outcome::Refused("E_NOT_FOUND".into()),
                Some("a0")
            )
        );
    }

    /// Final review M7 (minors item 15): a briefing names the open tensions it lists, so the
    /// log check holds them to the log too.
    #[test]
    fn sg5_a_briefing_names_the_tensions_it_lists() {
        let cfg = Config {
            seed: 14,
            agents: 2,
            steps: 12,
            kill_at: None,
        };
        let p = project::build(&cfg);
        let journal = Journal::new();
        let link = Link::connect(
            Where::of(&p),
            "agent:a0",
            "a0",
            "writer",
            Some(vec!["payments".to_string()]),
            journal.clone(),
            Arc::new(KillGate::new()),
        );
        let mut agent = Agent::new(link, 0, p.seed.clone());
        let claim = p.seed.claims[0].clone();
        let (token, _) = agent
            .brief_call(std::slice::from_ref(&claim))
            .expect("a briefing");
        // an update of a stable claim with no justification opens `unjustified_change`
        let ops = json!([{"op": "update", "id": claim, "body": "The ledger rule first changes."}]);
        let e = agent.link.entry("write");
        let res = agent.write_call(e, "write", json!({"ops": ops, "brief": token}));
        assert!(matches!(res, Res::Ok(_)));
        let opened = journal.entries().pop().expect("the write").seen;
        assert!(!opened.is_empty(), "the write opened a tension");
        agent
            .brief_call(std::slice::from_ref(&claim))
            .expect("a briefing");
        let briefed = journal.entries().pop().expect("the briefing");
        assert_eq!(briefed.act, "brief");
        for label in &opened {
            assert!(
                briefed.seen.contains(label),
                "{label} in {:?}",
                briefed.seen
            );
        }
    }
}
