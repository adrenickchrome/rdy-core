//! The swarm run (sg5): the project, the agents and the person, one kill and restart, the freeze
//! and the drain.
//!
//! - The run builds the project and starts every agent and the person. It paces the person: act
//!   `k` of `n` waits until every agent has reached step `(k + 1) * steps / n`, and then the
//!   person's queue takes the proposals the agents made so far.
//! - When the furthest agent reaches `kill_at`, the run kills the daemon (`kill -9`) and opens the
//!   gate: the next client starts a new daemon.
//! - The freeze (no actor runs): each agent reads its inbox with `ack: true` until `more` is
//!   false; the relay reads the open and the dismissed tensions; the daemon stops; the project is
//!   copied to `frozen`.
//! - The drain (collision liveness, ledger O5): every live agent view with changes, oldest first,
//!   is proposed by its agent and landed by the person, through a closed table of resolutions,
//!   never a dismissal. Before the views, the reviewers judge the open review requests (at most
//!   `REVIEWS`). A change bound to a member that lost its set is excluded with what depends on
//!   it (FW-AL1: losers are discarded by design); the person accepts advice it is shown through
//!   the relay (ruling task-5.4).
//!
//! Around every landing the relay reads canon's blocking tensions before and after (ledger O2);
//! a landing whose reply the kill cut is found by a read and audited the same way. The person
//! lands on a cadence and the agents wait for the person's acts due before each step (ruling
//! task-5.2), and the person's approve and the agents' acts that decide from a read take turns
//! (`KillGate::landing_turn`).
//! The journal holds two markers of the actor `run`: `freeze` and `drain`.
use super::agent::{
    self, AgentReport, KINDS, KillGate, Link, Pace, Res, Where, inbox_labels, labels_of,
};
use super::journal::{Entry, InboxRead, Journal, Outcome as Met};
use super::oracle::{self, copy_tree};
use super::people::{self, Hooks, Queue};
use super::project::{self, CLAIMS_PER_AREA, SeedBlocks, SwarmProject};
use super::script::{AREAS, Config, Script, script};
use crate::daemon::{Served, human_command, short_tempdir};
use crate::fixtures::output_within;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The act of the journal marker where the freeze starts.
pub const FREEZE: &str = "freeze";
/// The act of the journal marker where the drain starts.
pub const DRAIN: &str = "drain";
/// The actor of the journal markers.
pub const MARKER: &str = "run";
/// The act of the relay's read, after a landing whose reply the kill cut, of whether the
/// proposal landed. When it did, the entry's reply names the landing.
pub const LAND_FOUND: &str = "land_found";
/// The most rounds the drain gives one view.
const ROUNDS: usize = 4;
/// The most review requests the drain's reviewer pass judges, all reviewers together (a hang
/// guard, ruling task-5.5).
const REVIEWS: usize = 2000;
/// How often the run looks at the journal.
const POLL: Duration = Duration::from_millis(10);
/// How long the person waits for the agents before the run fails (a hang guard).
const PACE_WAIT: Duration = Duration::from_secs(1800);
/// A command's hang guard, not a speed check.
const CLI_BUDGET: Duration = Duration::from_secs(120);
/// How long the run waits for a stopped daemon to exit (a hang guard).
const STOP_WAIT: Duration = Duration::from_secs(60);
/// The relay as `rdy approve --relayed-by` names it.
const RELAY: &str = "agent:relay/chat";
/// The budget of every listing the run reads whole.
const ALL: usize = 100_000;

/// The violations the landings made, as the run's landing checks word them.
pub(crate) type Watch = Arc<Mutex<Vec<String>>>;

/// What a run leaves for the checks.
pub struct Outcome {
    pub journal: Vec<Entry>,
    /// The landing ids, in the order the journal's `land` entries (and `land_found` entries, for
    /// a landing whose reply the kill cut) name them.
    pub landings: Vec<String>,
    /// The landings that seeded the project, oldest first: canon's chain before any actor ran.
    pub seed_landings: Vec<String>,
    /// Every open tension the relay's `rdy_tensions {max_tokens: 100000}` listed at the freeze,
    /// by label.
    pub reported: BTreeMap<String, Value>,
    /// The same with `status: "dismissed"`.
    pub dismissed: BTreeMap<String, Value>,
    /// The copy of the project taken at the freeze, with the daemon stopped.
    pub frozen: PathBuf,
    /// The project, drained, with the daemon stopped.
    pub dir: PathBuf,
    pub served: Served,
    /// What the run's landing checks found.
    pub violations: Vec<String>,
    /// Holds `frozen`.
    _frozen_dir: tempfile::TempDir,
}

impl Outcome {
    fn marker(&self, act: &str) -> usize {
        self.journal
            .iter()
            .position(|e| e.actor == MARKER && e.act == act)
            .unwrap_or(self.journal.len())
    }

    /// The entries before the freeze: what the actors did.
    pub fn before_freeze(&self) -> &[Entry] {
        &self.journal[..self.marker(FREEZE)]
    }

    /// The entries from the freeze on: the freeze's reads and the drain.
    pub fn after_freeze(&self) -> &[Entry] {
        &self.journal[self.marker(FREEZE)..]
    }

    /// The entries before the drain: what the frozen copy holds.
    pub fn before_drain(&self) -> &[Entry] {
        &self.journal[..self.marker(DRAIN)]
    }
}

/// What the relay read before a landing.
pub(crate) struct Before {
    /// The proposal's view.
    pub view: String,
    /// The view's effective base: the landing it read on.
    pub base: Option<String>,
    /// The proposal's `advised_digest` when a reviewer's advice on it is not yet accepted.
    advised: Option<String>,
    /// Canon's blocking tensions.
    canon: Value,
}

/// The relay's `rdy_tensions {scope: "canon", severity: "blocking"}`, whole. `None` when the
/// daemon died under it.
fn canon_blocking(link: &mut Link) -> Option<Value> {
    let e = link.entry("tensions");
    let args = json!({"scope": "canon", "severity": "blocking", "max_tokens": ALL});
    let res = link.exec(e, "tensions", "rdy_tensions", args, |en, v| {
        en.seen = labels_of(&v["tensions"]);
    });
    let Res::Ok(v) = res else { return None };
    if v.get("omitted").is_some() {
        link.fail("tensions", "got a reply cut for its budget");
    }
    Some(v)
}

/// Before landing `pref`: the relay reads the proposal's view, the view's effective base and
/// canon's blocking tensions. `None` when the daemon died under a read.
pub(crate) fn before_land(link: &mut Link, pref: &str) -> Option<Before> {
    let (mut view, mut advised) = (None, None);
    let mut e = link.entry("read");
    e.block = Some(pref.to_string());
    let res = link.exec(e, "read", "rdy_read", json!({"id": pref}), |_, v| {
        let p = &v["proposal"];
        view = p["view"].as_str().map(str::to_string);
        let open = p["advised"]
            .as_array()
            .is_some_and(|a| a.iter().any(|t| t["accepted"] == false));
        if open {
            advised = p["advised_digest"].as_str().map(str::to_string);
        }
    });
    if !matches!(res, Res::Ok(_)) {
        return None;
    }
    let view = view.unwrap_or_else(|| link.fail("read", &format!("got no view for {pref}")));
    let mut base = None;
    let mut e = link.entry("view_show");
    e.view = Some(view.clone());
    let args = json!({"op": "show", "view": view, "max_tokens": ALL});
    let res = link.exec(e, "view_show", "rdy_view", args, |_, v| {
        base = v["effective_base"]
            .as_str()
            .or(v["head"]["base"].as_str())
            .map(str::to_string);
    });
    if !matches!(res, Res::Ok(_)) {
        return None;
    }
    let canon = canon_blocking(link)?;
    Some(Before {
        view,
        base,
        advised,
        canon,
    })
}

/// After a landing (`reply`, the `rdy approve` reply): the relay reads canon's blocking tensions
/// again, and each one the landing put into canon is a violation.
pub(crate) fn after_land(link: &mut Link, watch: &Watch, before: Before, reply: &Value) {
    let Some(after) = canon_blocking(link) else {
        return;
    };
    let landing = reply["landing"].as_str().unwrap_or("?");
    let mut w = watch.lock().unwrap_or_else(PoisonError::into_inner);
    for t in oracle::new_blocking_canon(&before.canon, &after) {
        w.push(format!("landing {landing}: it put tension {t} into canon"));
    }
}

/// The run of `cfg` with the scripts its seed gives.
pub fn run(cfg: &Config) -> Outcome {
    run_script(cfg, script(cfg))
}

/// Waits until `progress` (the step every agent has reached) is `target` or more. Past
/// `PACE_WAIT` it fails with the seed (a hang guard).
fn wait_for(progress: &AtomicUsize, target: usize, seed: u64, who: &str) {
    let end = Instant::now() + PACE_WAIT;
    while progress.load(Ordering::SeqCst) < target {
        assert!(
            Instant::now() < end,
            "swarm seed {seed}: {who} waited for step {target} and the agents never reached it"
        );
        std::thread::sleep(POLL);
    }
}

/// The step the person's act `j` of `len` waits for: every agent has reached it.
fn due(j: usize, steps: usize, len: usize) -> usize {
    ((j + 1) * steps / len.max(1)).min(steps)
}

/// How many of the person's `len` acts are due before an agent's step `k`: those whose step
/// (`due`) is `k - 1` or less.
fn person_acts_before(k: usize, steps: usize, len: usize) -> usize {
    (0..len)
        .take_while(|j| k > 0 && due(*j, steps, len) < k)
        .count()
}

/// An agent's pace: step `k` waits until every agent has reached step `k - 1`, so the agents
/// stay within a step of each other and a reviewer still runs when a writer creates the claim it
/// judges; and until the person has finished the acts due before step `k`, so the person keeps
/// its cadence (ruling task-5.2: the run lands as it goes, and the first member of a
/// contradiction pair lands before the second member writes). Within a step they race.
fn agent_pacer(
    cfg: &Config,
    progress: &Arc<AtomicUsize>,
    person: &Arc<AtomicUsize>,
    len: usize,
) -> Pace {
    let (seed, steps) = (cfg.seed, cfg.steps);
    let (progress, person) = (progress.clone(), person.clone());
    Arc::new(move |k| {
        wait_for(&progress, k.saturating_sub(1), seed, "an agent");
        wait_for(
            &person,
            person_acts_before(k, steps, len),
            seed,
            "an agent (for the person)",
        );
    })
}

/// The stable part of a proposal ref `P-<stable>@<n>`, and its version.
fn version_of(pref: &str) -> (&str, u32) {
    match pref.rsplit_once('@') {
        Some((stable, n)) => (stable, n.parse().unwrap_or(0)),
        None => (pref, 0),
    }
}

/// The person's pace: before act `k` of `len`, it waits until every agent has reached step
/// `(k + 1) * steps / len`, then queues the agents' proposals made so far. The queue holds the
/// latest version of each proposal, oldest proposal first, except that a proposal of the view of
/// a contradiction pair's first marked claim goes first: the pair is reviewable only once that
/// claim has landed (ruling task-4.2).
fn pacer(
    cfg: &Config,
    s: &Script,
    progress: &Arc<AtomicUsize>,
    journal: &Journal,
    queue: &Queue,
) -> Pace {
    let (steps, seed, len) = (cfg.steps, cfg.seed, s.person.len());
    // the view of the first member's marked claim (agent.rs: `a<i>-k<k>`)
    let first: BTreeSet<String> = s
        .contra
        .iter()
        .enumerate()
        .map(|(k, (a, _))| format!("a{a}-k{}", agent::spell(k)))
        .collect();
    let (progress, journal, queue) = (progress.clone(), journal.clone(), queue.clone());
    let queued: Mutex<BTreeSet<String>> = Mutex::default();
    Arc::new(move |k| {
        wait_for(&progress, due(k, steps, len), seed, "the person");
        let mut seen = queued.lock().unwrap_or_else(PoisonError::into_inner);
        let mut q = queue.lock().unwrap_or_else(PoisonError::into_inner);
        for e in journal.entries() {
            if e.act != "propose" || e.outcome != Met::Ok || !e.actor.starts_with("agent:a") {
                continue;
            }
            let Some(r) = e.block else { continue };
            if !seen.insert(r.clone()) {
                continue;
            }
            let (stable, n) = version_of(&r);
            match q.iter().position(|x| version_of(x).0 == stable) {
                Some(at) if version_of(&q[at]).1 < n => q[at] = r,
                Some(_) => {}
                None if e.view.as_ref().is_some_and(|v| first.contains(v)) => q.insert(0, r),
                None => q.push(r),
            }
        }
    })
}

/// Stops the daemon and waits until it is gone.
fn stop(served: &Served, seed: u64) {
    served.stop();
    let end = Instant::now() + STOP_WAIT;
    while served.daemon_pid().is_some() {
        assert!(
            Instant::now() < end,
            "swarm seed {seed}: the daemon did not stop"
        );
        std::thread::sleep(POLL);
    }
}

/// The run of `cfg` with the scripts `s` (a directed run passes its own).
pub fn run_script(cfg: &Config, s: Script) -> Outcome {
    let journal = Journal::new();
    match catch_unwind(AssertUnwindSafe(|| run_with(cfg, s, &journal))) {
        Ok(o) => o,
        Err(p) => {
            let text = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_string()))
                .unwrap_or_else(|| "a panic".to_string());
            resume_unwind(Box::new(format!("{text}\n{}", tail(&journal.entries()))))
        }
    }
}

/// How many entries a failure shows.
const TAIL: usize = 40;

/// The last `TAIL` entries of `journal`, one line each, for a failure's text.
fn tail(journal: &[Entry]) -> String {
    let from = journal.len().saturating_sub(TAIL);
    let lines: Vec<String> = journal[from..]
        .iter()
        .map(|e| {
            format!(
                "  {} step {} {} {:?} view={} block={}",
                e.actor,
                e.step,
                e.act,
                e.outcome,
                e.view.as_deref().unwrap_or("-"),
                e.block.as_deref().unwrap_or("-")
            )
        })
        .collect();
    format!(
        "journal tail ({} entries):\n{}",
        journal.len(),
        lines.join("\n")
    )
}

fn run_with(cfg: &Config, s: Script, journal: &Journal) -> Outcome {
    let journal = journal.clone();
    let p = project::build(cfg);
    let at = Where::of(&p);
    let gate = Arc::new(if cfg.kill_at.is_some() {
        KillGate::with_kill()
    } else {
        KillGate::new()
    });
    let queue: Queue = Arc::default();
    let watch: Watch = Arc::default();
    let progress = Arc::new(AtomicUsize::new(0));
    let person_done = Arc::new(AtomicUsize::new(0));
    let agents: Vec<JoinHandle<AgentReport>> = s
        .agents
        .iter()
        .enumerate()
        .map(|(i, acts)| {
            let pace = Some(agent_pacer(cfg, &progress, &person_done, s.person.len()));
            agent::spawn_paced(
                &p,
                cfg,
                i,
                acts.clone(),
                journal.clone(),
                gate.clone(),
                pace,
            )
        })
        .collect();
    let hooks = Hooks {
        pace: Some(pacer(cfg, &s, &progress, &journal, &queue)),
        watch: Some(watch.clone()),
        done: Some(person_done.clone()),
    };
    let person = people::spawn_with(
        &p,
        s.person.clone(),
        journal.clone(),
        queue.clone(),
        gate.clone(),
        hooks,
    );
    let names: Vec<String> = (0..agents.len()).map(|i| format!("agent:a{i}")).collect();
    let mut kill_at = cfg.kill_at;
    loop {
        let mut step: HashMap<&str, usize> = HashMap::new();
        for e in journal.entries() {
            if let Some(n) = names.iter().find(|n| **n == e.actor) {
                let s = step.entry(n.as_str()).or_default();
                *s = (*s).max(e.step);
            }
        }
        let at_step: Vec<usize> = agents
            .iter()
            .zip(&names)
            .map(|(h, n)| {
                if h.is_finished() {
                    cfg.steps
                } else {
                    step.get(n.as_str()).copied().unwrap_or(0)
                }
            })
            .collect();
        let slowest = at_step.iter().copied().min().unwrap_or(cfg.steps);
        let furthest = at_step.iter().copied().max().unwrap_or(cfg.steps);
        progress.store(slowest, Ordering::SeqCst);
        if let Some(k) = kill_at
            && furthest >= k
        {
            p.served.kill9();
            gate.restarted();
            kill_at = None;
        }
        // a person that stopped (a failure) holds no agent back: the run then fails with its text
        if person.is_finished() {
            person_done.store(usize::MAX, Ordering::SeqCst);
        }
        if agents.iter().all(JoinHandle::is_finished) && person.is_finished() {
            break;
        }
        std::thread::sleep(POLL);
    }
    let reports: Vec<AgentReport> = agents
        .into_iter()
        .map(|h| h.join().unwrap_or_else(|e| resume_unwind(e)))
        .collect();
    person.join().unwrap_or_else(|e| resume_unwind(e));

    // the freeze: no actor runs
    journal.record(Entry::new(MARKER, cfg.steps, FREEZE, Met::Ok));
    for (i, rep) in reports.iter().enumerate() {
        freeze_inbox(&at, cfg, i, rep, &journal, &gate);
    }
    let mut relay = Link::connect(
        at.clone(),
        "agent:relay",
        "chat",
        "reviewer",
        None,
        journal.clone(),
        gate.clone(),
    );
    relay.step = cfg.steps;
    let reported = listing(&mut relay, json!({"max_tokens": ALL}));
    let dismissed = listing(
        &mut relay,
        json!({"status": "dismissed", "max_tokens": ALL}),
    );
    drop(relay);
    stop(&p.served, cfg.seed);
    let frozen_dir = short_tempdir();
    let frozen = frozen_dir.path().join("frozen");
    copy_tree(&at.dir, &frozen).unwrap_or_else(|e| {
        panic!(
            "swarm seed {}: cannot copy the project at the freeze: {e}",
            cfg.seed
        )
    });

    // the drain: the daemon starts again with its first client
    journal.record(Entry::new(MARKER, cfg.steps, DRAIN, Met::Ok));
    let order = drain_order(&reports, &journal.entries());
    let mut d = Drain::new(&at, cfg, &p.seed, &journal, &gate, &watch);
    let mut judged = 0;
    for i in (0..reports.len()).filter(|i| cfg.is_reviewer(*i)) {
        judged += d.reviews(i, REVIEWS - judged);
    }
    for v in &order {
        d.view(v);
    }
    drop(d);
    stop(&p.served, cfg.seed);

    let journal = journal.entries();
    let landings = journal
        .iter()
        .filter(|e| (e.act == "land" && e.outcome == Met::Ok) || e.act == LAND_FOUND)
        .filter_map(|e| e.reply.as_ref()?["landing"].as_str().map(str::to_string))
        .collect();
    let violations = watch.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let SwarmProject {
        served,
        seed_landings,
        ..
    } = p;
    Outcome {
        journal,
        landings,
        seed_landings,
        reported,
        dismissed,
        frozen,
        dir: at.dir,
        served,
        violations,
        _frozen_dir: frozen_dir,
    }
}

/// The relay's `rdy_tensions` with `args`: the rows by label, or `None` when the reply was cut
/// for its budget (`omitted`).
fn listing_once(relay: &mut Link, args: Value) -> Option<BTreeMap<String, Value>> {
    let e = relay.entry("tensions");
    let mut cut = false;
    let mut rows = BTreeMap::new();
    let res = relay.exec(e, "tensions", "rdy_tensions", args, |en, v| {
        en.seen = labels_of(&v["tensions"]);
        cut = v.get("omitted").is_some();
        rows = v["tensions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| Some((t["id"].as_str()?.to_string(), t.clone())))
            .collect();
    });
    if !matches!(res, Res::Ok(_)) {
        relay.fail("tensions", "lost the daemon at the freeze");
    }
    (!cut).then_some(rows)
}

/// The relay's `rdy_tensions` with `args`, whole, by label. When one read is cut for its budget
/// (the deep tier), the listing is read in parts that together hold every tension that is not a
/// read-time overlap: canon's (`scope: canon`) and each open view's (`view`), each kind apart,
/// and a view's kind still cut change by change (`involving`) (ruling task-5.5). A part still cut
/// fails the run with the seed.
fn listing(relay: &mut Link, args: Value) -> BTreeMap<String, Value> {
    if let Some(rows) = listing_once(relay, args.clone()) {
        return rows;
    }
    let mut scopes: Vec<Value> = vec![json!({"scope": "canon"})];
    let e = relay.entry("view_list");
    let mut views: Vec<String> = vec![];
    relay.exec(
        e,
        "view_list",
        "rdy_view",
        json!({"op": "list", "all": true}),
        |_, v| {
            views = v["views"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|r| r["closed"] != true)
                .filter_map(|r| r["name"].as_str().map(str::to_string))
                .collect();
        },
    );
    scopes.extend(views.iter().map(|v| json!({"view": v})));
    let mut out = BTreeMap::new();
    for scope in scopes {
        for kind in KINDS {
            let mut part = args.clone();
            for (k, v) in scope.as_object().into_iter().flatten() {
                part[k] = v.clone();
            }
            part["kind"] = json!(kind);
            if let Some(rows) = listing_once(relay, part.clone()) {
                out.extend(rows);
                continue;
            }
            // still cut: a view's part is read change by change (`involving`)
            let Some(view) = scope["view"].as_str() else {
                relay.fail(
                    "tensions",
                    &format!("got a reply cut for its budget, read in parts: {part}"),
                )
            };
            for c in view_changes(relay, view) {
                let mut one = part.clone();
                one.as_object_mut().map(|m| m.remove("view"));
                one["involving"] = json!(c);
                match listing_once(relay, one.clone()) {
                    Some(rows) => out.extend(rows),
                    None => relay.fail(
                        "tensions",
                        &format!("got a reply cut for its budget, read in parts: {one}"),
                    ),
                }
            }
        }
    }
    out
}

/// The changes view `view` holds, as the relay reads them (`rdy_view show`).
fn view_changes(relay: &mut Link, view: &str) -> Vec<String> {
    let mut e = relay.entry("view_show");
    e.view = Some(view.to_string());
    let mut out = vec![];
    relay.exec(
        e,
        "view_show",
        "rdy_view",
        json!({"op": "show", "view": view, "max_tokens": ALL}),
        |_, v| {
            out = v["changes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c["id"].as_str().map(str::to_string))
                .collect();
        },
    );
    out
}

/// Agent `i`'s session, as `agent::spawn` opens it.
fn agent_link(at: &Where, cfg: &Config, i: usize, journal: &Journal, gate: &Arc<KillGate>) -> Link {
    let name = format!("a{i}");
    let role = if cfg.is_reviewer(i) {
        "reviewer"
    } else {
        "writer"
    };
    let mut link = Link::connect(
        at.clone(),
        &format!("agent:{name}"),
        &name,
        role,
        Some(vec![AREAS[i % AREAS.len()].to_string()]),
        journal.clone(),
        gate.clone(),
    );
    link.step = cfg.steps;
    link
}

/// One inbox read of the freeze, journaled as `act` (its table row applies): from `since` (the
/// persisted cursor when `None`), with `ack: true`, whole. The entry's `InboxRead` names
/// `from` as its start and `range`. Returns what the call met, and on `Ok` the new cursor and
/// whether more entries wait.
fn inbox_read(
    link: &mut Link,
    act: &'static str,
    since: Option<&str>,
    from: &str,
    range: u32,
) -> (Res, String, bool) {
    let mut args = json!({"ack": true, "max_tokens": ALL});
    if let Some(s) = since {
        args["since"] = json!(s);
    }
    let (mut cursor, mut more) = (String::new(), false);
    let e = link.entry(act);
    let res = link.exec(e, act, "rdy_inbox", args, |en, v| {
        let entries = v["entries"].as_array().cloned().unwrap_or_default();
        cursor = v["cursor"].as_str().unwrap_or_default().to_string();
        more = v["more"] == true;
        en.seen = inbox_labels(&entries);
        en.inbox = Some(InboxRead {
            since: from.to_string(),
            cursor: cursor.clone(),
            entries,
            range,
        });
    });
    (res, cursor, more)
}

/// Agent `i` reads its inbox with `ack: true` until `more` is false, in the range its last read
/// was in. A cursor the kill left past the end of the log starts a new range from the cursor the
/// agent last acked, as the agent itself does.
fn freeze_inbox(
    at: &Where,
    cfg: &Config,
    i: usize,
    rep: &AgentReport,
    journal: &Journal,
    gate: &Arc<KillGate>,
) {
    let mut link = agent_link(at, cfg, i, journal, gate);
    let mut since = rep.last_cursor.clone();
    let mut range = rep.range;
    let mut res = inbox_read(&mut link, "inbox_resume", Some(&since), &since, range);
    loop {
        match res {
            (Res::Ok(_), cursor, more) => {
                if !more {
                    return;
                }
                since = cursor;
                res = inbox_read(&mut link, "inbox", Some(&since), &since, range);
            }
            // only the first read may meet a cursor past the end (the table allows it there)
            (Res::Refused(..), ..) => {
                range += 1;
                let from = rep.persisted.clone().unwrap_or_default();
                res = inbox_read(&mut link, "inbox", None, &from, range);
            }
            (Res::Lost, ..) => link.fail("inbox", "lost the daemon at the freeze"),
        }
    }
}

/// The agent views to drain, oldest first: each view by the first journal entry that wrote in
/// it (a fork, which no entry writes in, by its parent's), then by name.
fn drain_order(reports: &[AgentReport], journal: &[Entry]) -> Vec<String> {
    let first = |v: &str| {
        journal
            .iter()
            .position(|e| e.view.as_deref() == Some(v) && e.change.is_some())
    };
    let mut views: Vec<(usize, String)> = vec![];
    for (i, rep) in reports.iter().enumerate() {
        let own = format!("a{i}");
        for v in &rep.views {
            let key = first(v).or_else(|| first(&own)).unwrap_or(usize::MAX);
            views.push((key, v.clone()));
        }
    }
    views.sort();
    views.into_iter().map(|(_, v)| v).collect()
}

/// The agent a view belongs to: `a<i>` and its forks `a<i>-f<x>`.
fn owner_of(view: &str) -> usize {
    view.trim_start_matches('a')
        .split('-')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("view {view} is no agent's"))
}

/// The change a refusal names: the hex digits after `change `.
fn change_in(text: &str) -> Option<String> {
    let at = text.find("change ")? + "change ".len();
    let hex: String = text[at..]
        .chars()
        .take_while(char::is_ascii_hexdigit)
        .collect();
    (hex.len() >= 6).then_some(hex)
}

/// `text` without its `CONTRA-<k>` marker (and the space before it).
fn strip_marker(text: &str) -> String {
    let Some(at) = text.find("CONTRA-") else {
        return text.to_string();
    };
    let digits = text[at + "CONTRA-".len()..]
        .chars()
        .take_while(char::is_ascii_digit)
        .count();
    let end = at + "CONTRA-".len() + digits;
    let start = if text[..at].ends_with(' ') {
        at - 1
    } else {
        at
    };
    format!("{}{}", &text[..start], &text[end..])
}

/// A refusal of the gate: its code and the JSON `rdy approve` printed.
struct Refusal {
    code: String,
    body: Value,
}

/// The drain's sessions: the person's relay and each owning agent.
struct Drain {
    at: Where,
    cfg: Config,
    seed: SeedBlocks,
    journal: Journal,
    gate: Arc<KillGate>,
    watch: Watch,
    relay: Link,
    agents: HashMap<usize, Link>,
    /// The message of the last refusal, for a failure's text.
    said: String,
}

impl Drain {
    fn new(
        at: &Where,
        cfg: &Config,
        seed: &SeedBlocks,
        journal: &Journal,
        gate: &Arc<KillGate>,
        watch: &Watch,
    ) -> Drain {
        let mut relay = Link::connect(
            at.clone(),
            "agent:relay",
            "chat",
            "reviewer",
            None,
            journal.clone(),
            gate.clone(),
        );
        relay.step = cfg.steps;
        Drain {
            at: at.clone(),
            cfg: *cfg,
            seed: seed.clone(),
            journal: journal.clone(),
            gate: gate.clone(),
            watch: watch.clone(),
            relay,
            agents: HashMap::new(),
            said: String::new(),
        }
    }

    fn agent(&mut self, i: usize) -> &mut Link {
        let (at, cfg, journal, gate) = (&self.at, &self.cfg, &self.journal, &self.gate);
        self.agents
            .entry(i)
            .or_insert_with(|| agent_link(at, cfg, i, journal, gate))
    }

    /// The failure of a refusal outside the drain's table.
    fn no_rule(&self, v: &str, code: &str, kind: &str) -> ! {
        panic!(
            "swarm seed {}: view {v} met {code} ({kind}), which the drain has no rule for: {}",
            self.cfg.seed, self.said
        )
    }

    /// Lands view `v`, or finds it landed or closed. At most `ROUNDS` rounds of propose, land
    /// and resolve.
    fn view(&mut self, v: &str) {
        let i = owner_of(v);
        let mut last = "no refusal".to_string();
        for round in 0..ROUNDS {
            let Some(shown) = self.show(i, v) else {
                return;
            };
            let empty = shown["changes"].as_array().is_none_or(Vec::is_empty);
            // a member of a chosen or abandoned set is closed; a landed view holds nothing
            if shown["head"]["closed"] == true || empty {
                return;
            }
            let Some(pref) = self.propose(i, v, round, &shown) else {
                return;
            };
            match self.land(v, &pref) {
                None => return,
                Some(r) => {
                    last.clone_from(&r.code);
                    self.resolve(i, v, &r);
                }
            }
        }
        panic!(
            "swarm seed {}: view {v} did not land after {ROUNDS} rounds: {last}",
            self.cfg.seed
        );
    }

    /// `rdy_view show` of `v` as its agent; `None` for a view nobody wrote in.
    fn show(&mut self, i: usize, v: &str) -> Option<Value> {
        let link = self.agent(i);
        let mut e = link.entry("view_show");
        e.view = Some(v.to_string());
        let res = link.raw(
            "view_show",
            "rdy_view",
            json!({"op": "show", "view": v, "max_tokens": ALL}),
        );
        if matches!(&res, Res::Refused(code, _) if code == "E_NOT_FOUND") {
            e.outcome = Met::Refused("E_NOT_FOUND".to_string());
            link.journal.record(e);
            return None;
        }
        let mut shown = None;
        link.finish(e, "view_show", res, |_, r| shown = Some(r.clone()));
        let shown = shown.unwrap_or_else(|| link.fail("view_show", "lost the daemon in the drain"));
        if shown.get("omitted").is_some() {
            link.fail("view_show", "got a reply cut for its budget");
        }
        Some(shown)
    }

    /// The open alternative set `v` is a member of, and its member label, as the relay lists it.
    fn alternative(&mut self, v: &str) -> Option<(String, String)> {
        let e = self.relay.entry("alternatives");
        let mut sets = Value::Null;
        self.relay.exec(
            e,
            "alternatives",
            "rdy_alternatives",
            json!({"op": "list", "max_tokens": ALL}),
            |_, r| sets = r["sets"].clone(),
        );
        sets.as_array().into_iter().flatten().find_map(|s| {
            let member = s["members"]
                .as_array()?
                .iter()
                .find(|m| m["view"] == v && s["status"] == "open")?;
            Some((
                s["id"].as_str()?.to_string(),
                member["label"].as_str()?.to_string(),
            ))
        })
    }

    /// Agent `i` proposes view `v`, naming its set and member when the view is a member of an
    /// open set. Any refusal is outside the drain's table.
    fn propose(&mut self, i: usize, v: &str, round: usize, shown: &Value) -> Option<String> {
        let mut held: Vec<String> = shown["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["id"].as_str().map(str::to_string))
            .collect();
        loop {
            let mut args = json!({"view": v, "title": format!("{v} drain round {round}")});
            if let Some((set, member)) = self.alternative(v) {
                args["alternatives"] = json!({"set": set, "chosen": member});
            }
            let link = self.agent(i);
            let mut e = link.entry("propose");
            e.view = Some(v.to_string());
            let res = link.raw("propose", "rdy_propose", args);
            match &res {
                Res::Ok(r) => {
                    e.block = r["ref"].as_str().map(str::to_string);
                    e.seen = labels_of(&r["tensions"]);
                }
                Res::Refused(code, message) => {
                    e.outcome = Met::Refused(code.clone());
                    e.message = Some(message.clone());
                }
                Res::Lost => e.outcome = Met::Lost,
            }
            let made = e.block.clone();
            link.journal.record(e);
            match (res, made) {
                (Res::Ok(_), Some(p)) => return Some(p),
                // a change bound to a member that lost its set (FW-AL1: losers are discarded by
                // design): the agent excludes it with what depends on it, and proposes the rest.
                // `sg5_the_alternative_refusals_name_a_change_bound_to_a_loser` holds each such
                // refusal to a loser-bound change (ruling task-5.4)
                (Res::Refused(code, message), _)
                    if code == "E_ALT_MEMBER" || code == "E_ALT_CHOSEN" =>
                {
                    let bound = change_in(&message)
                        .and_then(|prefix| held.iter().find(|c| c.starts_with(&prefix)).cloned());
                    let Some(bound) = bound else {
                        self.said = message;
                        self.no_rule(v, &code, "propose, naming no change of the view")
                    };
                    let left = self.exclude(i, v, &bound, &code);
                    if left.is_empty() {
                        return None;
                    }
                    held = left;
                }
                (Res::Refused(code, message), _) => {
                    self.said = message;
                    self.no_rule(v, &code, "propose")
                }
                _ => link_fail(&self.cfg, v, "propose"),
            }
        }
    }

    /// Agent `i` excludes change `c` from view `v`, with what depends on it. Returns the changes
    /// the view still holds.
    fn exclude(&mut self, i: usize, v: &str, c: &str, code: &str) -> Vec<String> {
        let link = self.agent(i);
        let mut e = link.entry("exclude");
        e.view = Some(v.to_string());
        let args = json!({"op": "exclude", "view": v, "changes": [c], "cascade": true});
        let res = link.raw("exclude", "rdy_view", args);
        if let Res::Refused(got, message) = &res {
            e.outcome = Met::Refused(got.clone());
            e.message = Some(message.clone());
            link.journal.record(e);
            self.said.clone_from(message);
            self.no_rule(v, got, &format!("exclude, resolving {code}"));
        }
        // the reply names what left the view (`removed`, `cascaded`): the liveness check lists it
        link.finish(e, "exclude", res, |en, r| {
            en.seen = labels_of(&r["on_change"]["tensions"]);
            en.reply = Some(r.clone());
        });
        self.show(i, v)
            .and_then(|s| s["changes"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|c| c["id"].as_str().map(str::to_string))
            .collect()
    }

    /// Reviewer `i` judges every free review request, as its scripted `ReviewNext` does, until
    /// none is left: a contradiction pair whose second claim came late in the run is judged
    /// before its view is drained. The requests are those open at the drain's start and those
    /// its own judgements make; at most `left` of them, else the run fails (a hang guard).
    /// Returns how many it judged.
    fn reviews(&mut self, i: usize, left: usize) -> usize {
        for judged in 0..=left {
            let link = self.agent(i);
            let e = link.entry("review_next");
            let mut case: Option<Value> = None;
            link.exec(
                e,
                "review_next",
                "rdy_review_next",
                json!({"kind": "tension"}),
                |_, r| {
                    if r["request"].is_string() {
                        case = Some(r.clone());
                    }
                },
            );
            let Some(leased) = case else { return judged };
            if judged == left {
                panic!(
                    "swarm seed {}: the drain's reviewers judged {REVIEWS} review requests and more are open (a hang guard)",
                    self.cfg.seed
                );
            }
            let blocks = leased["case"]["blocks"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let args = agent::verdict_args(&leased["request"], &leased["case_hash"], &blocks);
            let e = link.entry("review_submit");
            link.exec(e, "review_submit", "rdy_review_submit", args, |en, r| {
                en.seen = r["tension"]
                    .as_str()
                    .map(str::to_string)
                    .into_iter()
                    .collect();
                en.reply = Some(r.clone());
            });
        }
        unreachable!("the loop returns or fails by its last round")
    }

    /// The person, shown a reviewer's advice on `pref`, accepts it through the relay, as the
    /// scripted `AcceptAdvice` does: a relayed person act of the drain, journaled as
    /// `accept_advice` (ruling task-5.4; the coverage check counts only the run's acts).
    fn accept(&mut self, pref: &str, digest: &str) {
        let mut e = self.relay.entry("accept_advice");
        e.block = Some(pref.to_string());
        let accept = json!({"proposal": pref, "advised_digest": digest,
            "evidence": format!("Accept the advice on {pref}."),
            "question": format!("Accept the reviewers' advice on {pref}?"),
            "session_ref": "chat"});
        let res = self.relay.exec(
            e,
            "accept_advice",
            "rdy_resolve",
            json!({"accept": accept}),
            |_, _| {},
        );
        // a moved proposal (the table's one refusal) meets the gate as moved too
        if matches!(res, Res::Lost) {
            link_fail(&self.cfg, pref, "accept_advice");
        }
    }

    /// The person lands `pref` through the relay. `None` when it landed; else the refusal. Advice
    /// the person is shown and has not accepted, the person accepts first (`accept`).
    fn land(&mut self, v: &str, pref: &str) -> Option<Refusal> {
        let mut before =
            before_land(&mut self.relay, pref).unwrap_or_else(|| link_fail(&self.cfg, v, "land"));
        if let Some(digest) = before.advised.take() {
            self.accept(pref, &digest);
        }
        let mut e = self.relay.entry("land");
        e.block = Some(pref.to_string());
        e.view = Some(before.view.clone());
        e.base.clone_from(&before.base);
        let evidence = format!("Approve {pref}.");
        let question = format!("Land {pref}?");
        let args = [
            "--json",
            "approve",
            pref,
            "--relayed-by",
            RELAY,
            "--evidence",
            &evidence,
            "--question",
            &question,
            "--session",
            "chat",
        ];
        let mut cmd = human_command(&self.at.dir, &self.at.env, &args);
        let out = output_within(&mut cmd, CLI_BUDGET)
            .unwrap_or_else(|err| panic!("swarm seed {}: rdy approve hung: {err}", self.cfg.seed));
        let ok = out.status.success();
        let text = String::from_utf8_lossy(if ok { &out.stdout } else { &out.stderr }).into_owned();
        let body = serde_json::from_str(text.trim())
            .or_else(|_| serde_json::from_str(text.trim().lines().last().unwrap_or("")))
            .unwrap_or_else(|_| Value::String(text.trim().to_string()));
        if ok {
            e.reply = Some(body.clone());
            self.journal.record(e);
            after_land(&mut self.relay, &self.watch, before, &body);
            return None;
        }
        let code = body["code"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "swarm seed {}: rdy approve {pref} failed: {body}",
                    self.cfg.seed
                )
            })
            .to_string();
        e.outcome = Met::Refused(code.clone());
        e.message = body["message"].as_str().map(str::to_string);
        e.reply = Some(body.clone());
        self.journal.record(e);
        self.said = body.to_string();
        Some(Refusal { code, body })
    }

    /// The closed table of resolutions (ledger O5). Anything outside it fails the run.
    fn resolve(&mut self, i: usize, v: &str, r: &Refusal) {
        let issues: Vec<Value> = r.body["issues"].as_array().cloned().unwrap_or_default();
        match r.code.as_str() {
            "E_PROPOSAL_MOVED" => {}
            "E_CONFLICT_UNRESOLVED" => {
                let blocks: BTreeSet<String> = issues
                    .iter()
                    .filter_map(|x| x["block"].as_str().map(str::to_string))
                    .collect();
                if blocks.is_empty() {
                    self.no_rule(v, &r.code, "no conflict listed");
                }
                let shown = self
                    .show(i, v)
                    .unwrap_or_else(|| link_fail(&self.cfg, v, "show"));
                for b in blocks {
                    let conflict = shown["conflicts"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|c| c["block"] == b.as_str())
                        .unwrap_or_else(|| self.no_rule(v, &r.code, "a conflict the view lacks"));
                    let canon = conflict["sides"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|s| !s["landed"].is_null() || s["change"].is_null())
                        .unwrap_or_else(|| {
                            self.no_rule(v, &r.code, "a conflict with no canon side")
                        });
                    let version = canon["version"].as_str().map(str::to_string);
                    let body = self.body(i, &b, "canon");
                    let ops = json!([{"op": "update", "id": b, "base": version, "body": body}]);
                    self.write(i, v, &b, ops, "write", &r.code, "conflict");
                }
            }
            "E_BLOCKING_TENSION" => {
                if issues.is_empty() {
                    self.no_rule(v, &r.code, "no tension listed");
                }
                for x in &issues {
                    let message = x["message"].as_str().unwrap_or_default();
                    let mut words = message.split_whitespace();
                    let label = words.next().unwrap_or_default().to_string();
                    let kind = words
                        .next()
                        .unwrap_or_default()
                        .trim_end_matches(':')
                        .to_string();
                    let row = self.tension(i, v, &label, &r.code, &kind);
                    self.resolve_tension(i, v, &row, &r.code, &kind);
                }
            }
            other => self.no_rule(v, other, "land"),
        }
    }

    /// The row of tension `label` as agent `i` lists the tensions of view `v` (landing forms
    /// included).
    fn tension(&mut self, i: usize, v: &str, label: &str, code: &str, kind: &str) -> Value {
        let link = self.agent(i);
        let e = link.entry("tensions");
        let mut rows = vec![];
        link.exec(
            e,
            "tensions",
            "rdy_tensions",
            json!({"view": v, "max_tokens": ALL}),
            |en, r| {
                en.seen = labels_of(&r["tensions"]);
                rows = r["tensions"].as_array().cloned().unwrap_or_default();
            },
        );
        rows.into_iter()
            .find(|t| t["id"] == label)
            .unwrap_or_else(|| self.no_rule(v, code, &format!("{kind}, not listed")))
    }

    /// The blocks of `row`'s sides that view `v` holds, each at its side's version.
    fn own_blocks(row: &Value, v: &str) -> Vec<(String, Option<String>)> {
        row["sides"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| {
                s["change"].is_string()
                    && s["views"]
                        .as_array()
                        .is_some_and(|vs| vs.iter().any(|x| x == v))
            })
            .flat_map(|s| s["blocks"].as_array().cloned().unwrap_or_default())
            .filter_map(|b| {
                Some((
                    b["block"].as_str()?.to_string(),
                    b["version"].as_str().map(str::to_string),
                ))
            })
            .collect()
    }

    fn resolve_tension(&mut self, i: usize, v: &str, row: &Value, code: &str, kind: &str) {
        let own = Drain::own_blocks(row, v);
        if own.is_empty() {
            self.no_rule(v, code, &format!("{kind}, with no side in the view"));
        }
        match kind {
            // the agent repins the citing block
            "premise_shift" => {
                for (b, _) in own {
                    let ops = json!([{"op": "repin", "id": b}]);
                    self.write(i, v, &b, ops, "repin", code, kind);
                }
            }
            // the agent updates the block on the new canon base
            "edit_collision" => {
                let canon: Option<(String, Option<String>)> = row["sides"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|s| s["change"].is_null())
                    .flat_map(|s| s["blocks"].as_array().cloned().unwrap_or_default())
                    .find_map(|b| {
                        Some((
                            b["block"].as_str()?.to_string(),
                            b["version"].as_str().map(str::to_string),
                        ))
                    });
                let Some((b, version)) = canon else {
                    self.no_rule(v, code, "edit_collision, with no canon side")
                };
                let body = self.body(i, &b, v);
                let ops = json!([{"op": "update", "id": b, "base": version, "body": body}]);
                self.write(i, v, &b, ops, "write", code, kind);
            }
            // the agent writes the update again, implementing the area's seed decision
            "unjustified_change" => {
                for (b, version) in own {
                    let body = self.body(i, &b, v);
                    let ops = json!([{"op": "update", "id": b, "base": version, "body": body}]);
                    self.write(i, v, &b, ops, "write", code, kind);
                }
            }
            // an accepted semantic tension: the agent rewrites its side without the marker
            "semantic" => {
                for (b, version) in own {
                    let body = strip_marker(&self.body(i, &b, v));
                    let ops = json!([{"op": "update", "id": b, "base": version, "body": body}]);
                    self.write(i, v, &b, ops, "write", code, kind);
                }
            }
            other => self.no_rule(v, code, other),
        }
    }

    /// The body of block `b` in `scope`, as agent `i` reads it.
    fn body(&mut self, i: usize, b: &str, scope: &str) -> String {
        let link = self.agent(i);
        let mut e = link.entry("read");
        e.block = Some(b.to_string());
        let mut text = String::new();
        link.exec(
            e,
            "read",
            "rdy_read",
            json!({"id": b, "scope": scope, "format": "yaml"}),
            |_, r| text = r["text"].as_str().unwrap_or_default().to_string(),
        );
        let yaml: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text).unwrap_or_else(|err| {
            link.fail("read", &format!("got yaml that does not parse: {err}"))
        });
        yaml["body"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| link.fail("read", &format!("got no body for {b} in {scope}")))
    }

    /// The area of block `b`: a seed claim's, else the area its agent created it in.
    fn area(&self, b: &str) -> usize {
        self.seed
            .claims
            .iter()
            .position(|c| c == b)
            .map(|k| k / CLAIMS_PER_AREA)
            .or_else(|| agent::area_of(b))
            .unwrap_or(0)
    }

    /// Agent `i` briefs `b` in view `v` and writes `ops` there, implementing the area's seed
    /// decision, journaled as `act`. A refusal is outside the drain's table (`code`, `kind`: the
    /// refusal it resolves).
    #[allow(clippy::too_many_arguments)]
    fn write(
        &mut self,
        i: usize,
        v: &str,
        b: &str,
        ops: Value,
        act: &'static str,
        code: &str,
        kind: &str,
    ) {
        let decision = self.seed.decisions[self.area(b)].clone();
        let link = self.agent(i);
        let mut token = String::new();
        let e = link.entry("brief");
        link.exec(
            e,
            "brief",
            "rdy_brief",
            json!({"ids": [b], "scope": v}),
            |en, r| {
                token = r["base"]["token"].as_str().unwrap_or_default().to_string();
                en.brief = Some(token.clone());
            },
        );
        let mut e = link.entry(act);
        e.view = Some(v.to_string());
        e.block = Some(b.to_string());
        e.base = ops[0]["base"].as_str().map(str::to_string);
        e.brief = Some(token.clone());
        e.stable = true;
        let args = json!({"view": v, "ops": ops, "brief": token, "implements": [decision]});
        let res = link.raw(act, "rdy_write", args);
        match &res {
            Res::Ok(r) => {
                e.change = r["change"].as_str().map(str::to_string);
                e.seen = labels_of(&r["on_change"]["tensions"]);
            }
            Res::Refused(c, _) => e.outcome = Met::Refused(c.clone()),
            Res::Lost => e.outcome = Met::Lost,
        }
        link.journal.record(e);
        match res {
            Res::Ok(_) => {}
            Res::Refused(c, message) => panic!(
                "swarm seed {}: view {v} met {c} ({kind}, resolving {code}), which the drain has no rule for: {message}",
                self.cfg.seed
            ),
            Res::Lost => link_fail(&self.cfg, v, act),
        }
    }
}

/// A lost daemon in the drain, which runs after the restart.
fn link_fail(cfg: &Config, v: &str, act: &str) -> ! {
    panic!(
        "swarm seed {}: view {v}: the daemon was lost in the drain ({act})",
        cfg.seed
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sg5_a_marker_and_the_space_before_it_are_stripped() {
        assert_eq!(
            strip_marker("Agent b forbids that settlement is at once. CONTRA-12"),
            "Agent b forbids that settlement is at once."
        );
        assert_eq!(strip_marker("CONTRA-3 first"), " first");
        assert_eq!(strip_marker("no marker"), "no marker");
    }

    #[test]
    fn sg5_a_refusal_names_its_change() {
        assert_eq!(
            change_in("change a9c63f7428a6 is as-66e20ea572a2 member a2's alternative").as_deref(),
            Some("a9c63f7428a6")
        );
        assert_eq!(change_in("no change here"), None);
    }

    /// Ruling task-5.2: an agent's step `k` waits for the person's acts due before it, and the
    /// person's act `j` waits for the step it is due at, so neither waits for the other.
    #[test]
    fn sg5_the_agents_wait_for_the_person_acts_due_before_their_step() {
        let (steps, len) = (40, 20);
        assert_eq!(due(0, steps, len), 2);
        assert_eq!(due(len - 1, steps, len), steps);
        assert_eq!(person_acts_before(0, steps, len), 0);
        assert_eq!(person_acts_before(2, steps, len), 0);
        assert_eq!(person_acts_before(3, steps, len), 1);
        assert_eq!(person_acts_before(steps, steps, len), len - 1);
        for k in 0..=steps {
            for j in 0..person_acts_before(k, steps, len) {
                assert!(due(j, steps, len) < k, "act {j} is due by step {k}");
            }
        }
    }

    #[test]
    fn sg5_a_view_belongs_to_the_agent_its_name_gives() {
        assert_eq!(owner_of("a3"), 3);
        assert_eq!(owner_of("a12-fb"), 12);
    }
}
