//! The swarm run (sg5): the project, the agents and the person, one kill and restart, the freeze
//! and the drain.
//!
//! - The run builds the project and starts every agent and the person. It paces the person: act
//!   `k` of `n` waits until every agent has reached step `(k + 1) * steps / n`, and then the
//!   person's queue takes the proposals the agents made so far. A proposal of a contradiction
//!   pair's first claim goes to the front of the queue; a proposal that chooses a set member,
//!   and a set winner's later proposal, go right after the queued first claims.
//! - When the furthest agent reaches `kill_at`, the run kills the daemon (`kill -9`) and opens the
//!   gate: the next client starts a new daemon.
//! - The freeze (no actor runs): each agent reads its inbox with `ack: true` until `more` is
//!   false; the relay reads the open and the dismissed tensions; the daemon stops; the project is
//!   copied to `frozen`.
//! - The drain (collision liveness, ledger O5): every live agent view with changes, oldest first
//!   (the member views of a set still open come first, so the set is chosen before a view holding
//!   a change bound to a member proposes), is proposed by its agent and landed by the person,
//!   through a closed table of resolutions, never a dismissal. Before the views, the reviewers
//!   judge the open review requests (at most `REVIEWS`). A change bound to a member that lost its
//!   set is excluded with what depends on it: its dependents by deps, and a change that cites a
//!   block such an excluded change created (FW-AL1: losers are discarded by design); the person
//!   accepts advice it is shown through the relay (ruling task-5.4). A version that is not ready
//!   for open change-plan items has its plan answered by its agent, which proposes again.
//! - Before every approve, of the run and of the drain, the person accepts the version's
//!   deferral card when the queue lists one (D-AAQ9Z), and the relay reads the version's change
//!   plan (`version_plan`), so a check sees what the gate read.
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
use crate::daemon::{Served, human_command, short_tempdir_in};
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
/// The relay as `rdy approve --relayed-by` names it: the relaying agent's own actor alone; the
/// relay records the session's run (D-PWB82).
const RELAY: &str = RELAY_ACTOR;
/// The relaying agent's actor and run: the relay runs as this session (D-HA7YZ).
const RELAY_ACTOR: &str = "agent:relay";
const RELAY_RUN: &str = "chat";
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

/// Before landing `pref`: the relay reads the proposal's view, the view's effective base,
/// canon's blocking tensions and the version's change plan (a `version_plan` entry naming `pref`,
/// whose reply holds the plan's `counts`: what the gate reads; a version that conflicts with
/// canon has none, and the landing refuses it). `None` when the daemon died under a read.
pub(crate) fn before_land(link: &mut Link, pref: &str) -> Option<Before> {
    let (mut view, mut advised) = (None, None);
    let mut e = link.entry("read");
    e.block = Some(pref.to_string());
    let res = link.exec(e, "read", "rdy_read", json!({"id": pref}), |en, v| {
        let p = &v["proposal"];
        en.seen = agent::advised_labels(p);
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
    let mut e = link.entry("version_plan");
    e.block = Some(pref.to_string());
    let args = json!({"proposal": pref, "max_tokens": ALL});
    let res = link.exec(e, "version_plan", "rdy_plan", args, |en, v| {
        en.reply = Some(json!({"counts": v["counts"]}));
    });
    if matches!(res, Res::Lost) {
        return None;
    }
    Some(Before {
        view,
        base,
        advised,
        canon,
    })
}

/// After a landing (`reply`, the `rdy approve` reply): the relay reads canon's blocking tensions
/// again, and each one the landing put into canon is a violation. A read the kill cuts is made
/// again on the restarted daemon: only the person lands, and it holds its landing turn, so canon
/// has not moved. A second loss fails the run (the check is never left out).
pub(crate) fn after_land(link: &mut Link, watch: &Watch, before: Before, reply: &Value) {
    let after = match canon_blocking(link) {
        Some(after) => after,
        None => canon_blocking(link)
            .unwrap_or_else(|| link.fail("tensions", "lost the daemon twice after a landing")),
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

/// Where the pacer puts a proposal it has not queued before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// A proposal of a contradiction pair's first marked claim: at the front.
    FirstClaim,
    /// A proposal that chooses a set member, or a set winner's later work: after every queued
    /// first-claim proposal, before the rest.
    Ahead,
    /// Any other proposal: at the back.
    Behind,
}

/// Queues proposal `r` at `place`; a newer version of a queued proposal takes its place, and an
/// older one is dropped. `ranks` holds the place of each proposal the pacer queued, by stable id.
/// A first claim goes after the first claims at the front; a proposal `Ahead` goes after every
/// queued first claim and every earlier one `Ahead`, so it never passes a first claim (final
/// review M8); the rest go to the back. Each place keeps its proposals oldest first.
fn enqueue(q: &mut Vec<String>, r: String, place: Place, ranks: &mut BTreeMap<String, Place>) {
    let (stable, n) = version_of(&r);
    if let Some(at) = q.iter().position(|x| version_of(x).0 == stable) {
        if version_of(&q[at]).1 < n {
            q[at] = r;
        }
        return;
    }
    ranks.insert(stable.to_string(), place);
    let rank = |x: &String| ranks.get(version_of(x).0).copied();
    let at = match place {
        Place::FirstClaim => q
            .iter()
            .take_while(|x| rank(x) == Some(Place::FirstClaim))
            .count(),
        Place::Ahead => q
            .iter()
            .rposition(|x| matches!(rank(x), Some(Place::FirstClaim | Place::Ahead)))
            .map_or(0, |p| p + 1),
        Place::Behind => q.len(),
    };
    q.insert(at, r);
}

/// The person's pace: before act `k` of `len`, it waits until every agent has reached step
/// `(k + 1) * steps / len`, then queues the agents' proposals made since its last act (it reads
/// the journal from a cursor). The queue holds the latest version of each proposal, oldest
/// proposal first, except that a proposal of the view of a contradiction pair's first marked
/// claim goes first (the pair is reviewable only once that claim has landed, ruling task-4.2),
/// and a proposal that chooses a member or a set winner's later work goes right after the queued
/// first claims (`enqueue`).
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
    // how far the pacer has read the journal, and the views that won a set by a landing so far
    let read: Mutex<(usize, BTreeSet<String>)> = Mutex::default();
    // the place of each proposal queued so far, by stable id
    let placed: Mutex<BTreeMap<String, Place>> = Mutex::default();
    Arc::new(move |k| {
        wait_for(&progress, due(k, steps, len), seed, "the person");
        let mut seen = queued.lock().unwrap_or_else(PoisonError::into_inner);
        let mut ranks = placed.lock().unwrap_or_else(PoisonError::into_inner);
        let mut q = queue.lock().unwrap_or_else(PoisonError::into_inner);
        let mut read = read.lock().unwrap_or_else(PoisonError::into_inner);
        let (entries, next) = journal.since(read.0);
        read.0 = next;
        let won = &mut read.1;
        won.extend(
            entries
                .iter()
                .filter(|e| e.act == "land" && e.outcome == Met::Ok)
                .filter_map(|e| e.reply.as_ref()?["chosen"]["member"].as_str())
                .map(str::to_string),
        );
        for e in entries {
            if e.act != "propose" || e.outcome != Met::Ok || !e.actor.starts_with("agent:a") {
                continue;
            }
            let Some(r) = e.block else { continue };
            if !seen.insert(r.clone()) {
                continue;
            }
            // a proposal that chooses a member, or a set's winner's later work, goes first, so
            // the run chooses sets and lands a winner's later work (fix round 2, ruling
            // task-5-round1)
            let chooses = e
                .reply
                .as_ref()
                .is_some_and(|x| !x["alternatives"].is_null());
            let first_up = e
                .view
                .as_ref()
                .is_some_and(|v| first.contains(v) || won.contains(v))
                || chooses;
            let place = if e.view.as_ref().is_some_and(|v| first.contains(v)) {
                Place::FirstClaim
            } else if first_up {
                Place::Ahead
            } else {
                Place::Behind
            };
            enqueue(&mut q, r, place, &mut ranks);
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
    // each agent's furthest step, read from the journal's new entries only
    let mut step: HashMap<&str, usize> = HashMap::new();
    let mut cursor = 0;
    loop {
        let (fresh, next) = journal.since(cursor);
        cursor = next;
        for e in fresh {
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
            gate.killing();
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
    let frozen_dir = short_tempdir_in(project::run_parent());
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
    let mut d = Drain::new(&at, cfg, &p.seed, &journal, &gate, &watch, order.clone());
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
/// and a view's kind still cut change by change (`involving`) (ruling task-5.5). A listing of one
/// view (`args` names a `view`) is read in that view's parts alone, and its change-by-change
/// parts keep the view filter. A part still cut fails the run with the seed.
fn listing(relay: &mut Link, args: Value) -> BTreeMap<String, Value> {
    if let Some(rows) = listing_once(relay, args.clone()) {
        return rows;
    }
    let one_view = args["view"].as_str().map(str::to_string);
    let scopes: Vec<Value> = match &one_view {
        Some(v) => vec![json!({"view": v})],
        None => {
            let mut scopes = vec![json!({"scope": "canon"})];
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
            scopes
        }
    };
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
                if one_view.is_none() {
                    one.as_object_mut().map(|m| m.remove("view"));
                }
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

/// The block and the target of a dangling reference that a refusal names first:
/// `<block> references <target>, which does not exist in canon` (the gate's `E_DANGLING_REF`).
pub fn dangling_in(text: &str) -> Option<(String, String)> {
    let (block, rest) = text.split_once(" references ")?;
    let (target, _) = rest.split_once(", which does not exist in canon")?;
    let block = block.rsplit(' ').next()?;
    (!block.is_empty() && !target.is_empty() && !target.contains(' '))
        .then(|| (block.to_string(), target.to_string()))
}

/// `text` with each reference to `target` written as plain words: its link words, or `name` (the
/// target's last name) when it has none (ruling task-5-round2.1). Other references and escapes
/// stay as written.
fn unref(text: &str, target: &str, name: &str) -> String {
    let mut out = String::new();
    let mut last = 0;
    for span in rdy_model::ref_spans(text, rdy_model::RefMode::Strict) {
        if span.token != target {
            continue;
        }
        out.push_str(&text[last..span.range.start]);
        out.push_str(span.display.as_deref().unwrap_or(name));
        last = span.range.end;
    }
    out.push_str(&text[last..]);
    out
}

/// What a reader calls a block (`yaml`, as `rdy_read` gives it): the target's last name of ruling
/// task-5-round2.1. Its title, else its `name` field, else its text's first sentence, with each
/// reference as its link words (dropped when it has none: an id is never a name), no CONTRA
/// marker, and no final full stop.
fn name_in(yaml: &serde_yaml_ng::Value) -> String {
    let field = |k: &str| {
        yaml["fields"][k]
            .as_str()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
    };
    if let Some(said) = field("title").or_else(|| field("name")) {
        return said;
    }
    let body = yaml["body"].as_str().unwrap_or_default();
    let mut text = String::new();
    let mut last = 0;
    for span in rdy_model::ref_spans(body, rdy_model::RefMode::Strict) {
        text.push_str(&body[last..span.range.start]);
        text.push_str(span.display.as_deref().unwrap_or(""));
        last = span.range.end;
    }
    text.push_str(&body[last..]);
    let text = strip_marker(&text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let first = text.split(". ").next().unwrap_or_default();
    first.trim_end_matches('.').trim().to_string()
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

/// What an alternative refusal at the drain's propose names, read against the sets the relay
/// lists (ruling task-5.4).
#[derive(Debug, PartialEq, Eq)]
enum Named {
    /// A member that lost a chosen set (`E_ALT_CHOSEN`): the drain excludes the change.
    Lost,
    /// Members of open sets (`E_ALT_MEMBER`), as (set id, member view): the landing must choose,
    /// so the drain lands the set's member first (the gate's fix: "propose from <member> to
    /// choose it") and excludes nothing (the settle act, ruling task-5-round3.1).
    Open(Vec<(String, String)>),
    /// Neither, by the listing.
    Unknown,
}

/// The members of `set` as (label, view).
fn members_of(set: &Value) -> Vec<(String, String)> {
    set["members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            Some((
                m["label"].as_str()?.to_string(),
                m["view"].as_str()?.to_string(),
            ))
        })
        .collect()
}

/// What the refusal `message` names, read against `sets` (`rdy_alternatives list`): a member
/// that the listing shows lost a chosen set (`belongs to member <label>, which lost set <id>`),
/// else the open sets' members it names (`<id> member <label>'s alternative`, or one of the
/// open sets a change is bound in, `<id> member <label>,` or `;`).
fn alt_named(message: &str, sets: &Value) -> Named {
    let sets = sets.as_array().map(Vec::as_slice).unwrap_or_default();
    let lost = sets.iter().filter(|s| s["status"] == "chosen").any(|s| {
        let (Some(id), Some(won)) = (s["id"].as_str(), s["chosen"]["member"].as_str()) else {
            return false;
        };
        members_of(s).iter().any(|(label, view)| {
            view != won && message.contains(&format!("member {label}, which lost set {id} ("))
        })
    });
    if lost {
        return Named::Lost;
    }
    let mut open = vec![];
    for s in sets.iter().filter(|s| s["status"] == "open") {
        let Some(id) = s["id"].as_str() else { continue };
        for (label, view) in members_of(s) {
            let at = format!("{id} member {label}");
            let named = message.match_indices(&at).any(|(k, _)| {
                matches!(
                    message[k + at.len()..].chars().next(),
                    Some('\'' | ',' | ';')
                )
            });
            if named {
                open.push((id.to_string(), view));
            }
        }
    }
    if open.is_empty() {
        Named::Unknown
    } else {
        Named::Open(open)
    }
}

/// The member views the drain lands, in turn, to settle the open set `set`: `bound` first, then
/// the others in the drain's `order` (a member outside it last), leaving out each view in
/// `busy` (the drain is landing it now, up its call chain).
fn settle_order(set: &Value, bound: &str, order: &[String], busy: &[String]) -> Vec<String> {
    let members: Vec<String> = members_of(set).into_iter().map(|(_, v)| v).collect();
    let mut out: Vec<String> = vec![];
    let ranked = order.iter().filter(|v| members.contains(v));
    let rest = members.iter().filter(|v| !order.contains(v));
    for v in std::iter::once(&bound.to_string())
        .chain(ranked)
        .chain(rest)
    {
        if members.contains(v) && !busy.contains(v) && !out.contains(v) {
            out.push(v.clone());
        }
    }
    out
}

/// What the drain's propose came to.
enum Proposed {
    /// The proposal's ref, and the tensions its reply lists as refusing its landing (`refuses`),
    /// by label.
    Ref(String, BTreeSet<String>),
    /// The view holds nothing more.
    Empty,
    /// The drain settled an open set first: the view proposes again.
    Again,
}

/// The labels of the tensions a propose reply lists (`rows`, its `tensions`) as refusing the
/// version's landing (`refuses: true`).
fn refusing_of(rows: &Value) -> BTreeSet<String> {
    rows.as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["refuses"] == true)
        .filter_map(|t| t["id"].as_str().map(str::to_string))
        .collect()
}

/// Whether the listed tension `row` fits the drain row for `kind`, the kind the refusal's issue
/// names: the row is of that kind, and a `semantic` row is one a person accepted (D-WMCFW: only
/// an accepted semantic tension blocks). Else why not.
fn row_fits(row: &Value, kind: &str) -> Result<(), String> {
    if row["kind"] != kind {
        return Err(format!("listed as {}", row["kind"]));
    }
    if kind == "semantic" && row["judged"]["accepted"].is_null() {
        return Err("listed with no accepted advice".to_string());
    }
    Ok(())
}

/// The issue codes of a version that is not ready that the drain resolves (`Drain::resolve`).
const RESOLVED_ISSUES: [&str; 4] = [
    "E_CONFLICT_UNRESOLVED",
    "E_BLOCKING_TENSION",
    "E_PLAN_OPEN",
    "E_BRIEF_REQUIRED",
];

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
    /// The drain's order: oldest view first.
    order: Vec<String>,
    /// The views the drain is landing now, outermost first.
    busy: Vec<String>,
    /// The open sets the drain has settled for a view that waited on them.
    settling: BTreeSet<String>,
}

impl Drain {
    fn new(
        at: &Where,
        cfg: &Config,
        seed: &SeedBlocks,
        journal: &Journal,
        gate: &Arc<KillGate>,
        watch: &Watch,
        order: Vec<String>,
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
            order,
            busy: vec![],
            settling: BTreeSet::new(),
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
            "swarm seed {}: view {v} met {code} ({kind}), which the drain has no rule for\n  the daemon said: {}",
            self.cfg.seed, self.said
        )
    }

    /// Lands view `v`, or finds it landed or closed. At most `ROUNDS` rounds of propose, land
    /// and resolve; a propose that first settles an open set (`Proposed::Again`) is no round.
    fn view(&mut self, v: &str) {
        self.busy.push(v.to_string());
        self.rounds(v);
        self.busy.pop();
    }

    fn rounds(&mut self, v: &str) {
        let i = owner_of(v);
        let mut last = "no refusal".to_string();
        let mut round = 0;
        while round < ROUNDS {
            let Some(shown) = self.show(i, v) else {
                return;
            };
            let empty = shown["changes"].as_array().is_none_or(Vec::is_empty);
            // a member of a chosen or abandoned set is closed; a landed view holds nothing
            if shown["head"]["closed"] == true || empty {
                return;
            }
            let (pref, refusing) = match self.propose(i, v, round, &shown) {
                Proposed::Ref(pref, refusing) => (pref, refusing),
                Proposed::Empty => return,
                Proposed::Again => continue,
            };
            match self.land(v, &pref) {
                None => return,
                Some(r) => {
                    last.clone_from(&r.code);
                    self.resolve(i, v, &pref, &refusing, &r);
                }
            }
            round += 1;
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

    /// Every alternative set, chosen and abandoned ones too, as the relay lists them.
    fn sets(&mut self) -> Value {
        let e = self.relay.entry("alternatives");
        let mut sets = Value::Null;
        self.relay.exec(
            e,
            "alternatives",
            "rdy_alternatives",
            json!({"op": "list", "all": true, "max_tokens": ALL}),
            |_, r| sets = r["sets"].clone(),
        );
        sets
    }

    /// Whether the relay lists set `id` as open.
    fn is_open(&mut self, id: &str) -> bool {
        self.sets()
            .as_array()
            .into_iter()
            .flatten()
            .any(|s| s["id"] == id && s["status"] == "open")
    }

    /// Settles each open set in `named` (set id, bound member view) for view `v`, which waits on
    /// it: the drain lands the set's members (`settle_order`) until one landing chooses it. The
    /// run fails when the set stays open, or when the drain meets it a second time.
    fn settle(&mut self, v: &str, code: &str, named: &[(String, String)], sets: &Value) {
        for (id, bound) in named {
            if !self.settling.insert(id.clone()) {
                self.no_rule(v, code, &format!("propose, waiting again on open set {id}"));
            }
            let set = sets
                .as_array()
                .into_iter()
                .flatten()
                .find(|s| s["id"] == id.as_str())
                .cloned()
                .unwrap_or(Value::Null);
            for m in settle_order(&set, bound, &self.order, &self.busy) {
                if !self.is_open(id) {
                    break;
                }
                self.view(&m);
            }
            if self.is_open(id) {
                self.no_rule(
                    v,
                    code,
                    &format!("propose, waiting on open set {id}, which no member's landing chose"),
                );
            }
        }
    }

    /// The open alternative set `v` is a member of, and its member label, as the relay lists it.
    fn alternative(&mut self, v: &str) -> Option<(String, String)> {
        let sets = self.sets();
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
    /// open set. The refusals of the drain's table are answered here; any other fails the run.
    fn propose(&mut self, i: usize, v: &str, round: usize, shown: &Value) -> Proposed {
        let mut held: Vec<String> = shown["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["id"].as_str().map(str::to_string))
            .collect();
        // each dangling reference is repaired once: a second refusal for it fails the run
        let mut repaired: BTreeSet<(String, String)> = BTreeSet::new();
        loop {
            let title = format!("{v} drain round {round}");
            let mut args = json!({"view": v, "title": title, "summary": title});
            if let Some((set, member)) = self.alternative(v) {
                args["alternatives"] = json!({"set": set, "chosen": member});
            }
            let link = self.agent(i);
            let mut e = link.entry("propose");
            e.view = Some(v.to_string());
            let res = link.raw("propose", "rdy_propose", args);
            let mut refusing = BTreeSet::new();
            match &res {
                Res::Ok(r) => {
                    e.block = r["ref"].as_str().map(str::to_string);
                    e.seen = labels_of(&r["tensions"]);
                    refusing = refusing_of(&r["tensions"]);
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
                (Res::Ok(_), Some(p)) => return Proposed::Ref(p, refusing),
                (Res::Refused(code, message), _)
                    if code == "E_ALT_MEMBER" || code == "E_ALT_CHOSEN" =>
                {
                    let sets = self.sets();
                    let named = alt_named(&message, &sets);
                    self.said.clone_from(&message);
                    match named {
                        // a change bound to a member that lost its set (FW-AL1: losers are
                        // discarded by design), as the listing shows: the agent excludes it with
                        // what depends on it, and proposes the rest (ruling task-5.4)
                        Named::Lost if code == "E_ALT_CHOSEN" => {
                            let bound = change_in(&message).and_then(|prefix| {
                                held.iter().find(|c| c.starts_with(&prefix)).cloned()
                            });
                            let Some(bound) = bound else {
                                self.no_rule(v, &code, "propose, naming no change of the view")
                            };
                            let left = self.exclude(i, v, &bound, &code);
                            if left.is_empty() {
                                return Proposed::Empty;
                            }
                            held = left;
                        }
                        // a change bound to a member of a set still open: the landing must
                        // choose (ruling task-5-round1.1). The ruled drain row and its settle act
                        // (ruling task-5-round3.1): the drain excludes nothing; it lands the
                        // set's bound member first, out of oldest-first order, and the view
                        // proposes again without using a round. Drain-only, never coverage.
                        // `sg5_each_alternative_refusal_is_a_ruled_row` holds each such refusal to
                        // a set open at that propose that a landing chose before the next one
                        Named::Open(named) if code == "E_ALT_MEMBER" => {
                            self.settle(v, &code, &named, &sets);
                            return Proposed::Again;
                        }
                        _ => self.no_rule(
                            v,
                            &code,
                            "propose, naming no member that lost a listed chosen set or belongs to a listed open one",
                        ),
                    }
                }
                // a block of the view cites a block that a change the drain excluded above
                // created. A citation is no dep (`exclude --cascade` follows deps), so the citing
                // change stays: the agent rewrites the citing block without the reference and
                // proposes again (ruling task-5-round2.1). Any other E_VALIDATION fails the run.
                (Res::Refused(code, message), _) if code == "E_VALIDATION" => {
                    let found = dangling_in(&message)
                        .filter(|(_, target)| self.excluded_creates(v).contains(target))
                        .filter(|pair| repaired.insert(pair.clone()));
                    let Some((block, target)) = found else {
                        self.said = message;
                        self.no_rule(v, &code, "propose")
                    };
                    self.said = message;
                    self.repair(i, v, &block, &target);
                }
                (Res::Refused(code, message), _) => {
                    self.said = message;
                    self.no_rule(v, &code, "propose")
                }
                _ => link_fail(&self.cfg, v, "propose"),
            }
        }
    }

    /// Agent `i` rewrites `block` in view `v` without its reference to `target`, a block that a
    /// change the drain excluded from `v` created: one `rdy_write` update with `base`, the
    /// reference written as its link words or as the target's last name (`last_name`). Journaled
    /// as the drain act `repair_dangling_ref` naming `block` and `target` (ruling
    /// task-5-round2.1). A reference outside the body is outside the row.
    fn repair(&mut self, i: usize, v: &str, block: &str, target: &str) {
        let body = self.body(i, block, v);
        // the target's last name is needed only for a reference with no link words
        let wordless = rdy_model::ref_spans(&body, rdy_model::RefMode::Strict)
            .iter()
            .any(|s| s.token == target && s.display.is_none());
        let name = if wordless {
            self.last_name(i, v, target).unwrap_or_else(|| {
                self.no_rule(
                    v,
                    "E_VALIDATION",
                    &format!("propose: no view holds {target} to name it by"),
                )
            })
        } else {
            String::new()
        };
        let fixed = unref(&body, target, &name);
        if fixed == body {
            self.no_rule(
                v,
                "E_VALIDATION",
                &format!("propose: {block}'s body holds no reference to {target}"),
            );
        }
        let ops = json!([{"op": "update", "id": block, "body": fixed}]);
        self.write_noted(
            i,
            v,
            block,
            ops,
            "repair_dangling_ref",
            "E_VALIDATION",
            "a dangling reference",
            Some(target),
        );
    }

    /// The target's last name: agent `i` reads `target` in a view other than `v` that still holds
    /// it (a view that included or created the change that created it, by the journal: the
    /// losing member's view keeps a loser-bound change) and names it as a reader would
    /// (`name_in`). `None` when no such view holds it.
    fn last_name(&mut self, i: usize, v: &str, target: &str) -> Option<String> {
        let journal = self.journal.entries();
        let made: BTreeSet<String> = journal
            .iter()
            .filter(|e| e.outcome == Met::Ok && e.block.as_deref() == Some(target))
            .filter_map(|e| e.change.clone())
            .collect();
        let mut views: Vec<String> = vec![];
        let included = journal.iter().filter(|e| {
            e.act == "include"
                && e.outcome == Met::Ok
                && e.reply.as_ref().is_some_and(|r| {
                    r["included"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|c| c.as_str().is_some_and(|c| made.contains(c)))
                })
        });
        let creating = journal.iter().filter(|e| {
            e.outcome == Met::Ok && e.change.as_ref().is_some_and(|c| made.contains(c))
        });
        for w in included.chain(creating).filter_map(|e| e.view.clone()) {
            if w != v && !views.contains(&w) {
                views.push(w);
            }
        }
        for w in views {
            let link = self.agent(i);
            let mut e = link.entry("read");
            e.view = Some(w.clone());
            e.block = Some(target.to_string());
            let res = link.raw(
                "read",
                "rdy_read",
                json!({"id": target, "scope": w, "format": "yaml"}),
            );
            match res {
                Res::Ok(r) => {
                    link.journal.record(e);
                    let text = r["text"].as_str().unwrap_or_default();
                    let yaml: serde_yaml_ng::Value =
                        serde_yaml_ng::from_str(text).unwrap_or_else(|err| {
                            panic!(
                                "swarm seed {}: {target} in {w}: yaml that does not parse: {err}",
                                self.cfg.seed
                            )
                        });
                    let name = name_in(&yaml);
                    if !name.is_empty() {
                        return Some(name);
                    }
                }
                Res::Refused(code, message) => {
                    e.outcome = Met::Refused(code);
                    e.message = Some(message);
                    link.journal.record(e);
                }
                Res::Lost => link_fail(&self.cfg, v, "read"),
            }
        }
        None
    }

    /// The blocks that the changes the drain excluded from view `v` created or wrote, as the
    /// journal's entries of those changes name them.
    fn excluded_creates(&self, v: &str) -> BTreeSet<String> {
        let journal = self.journal.entries();
        let drained = journal
            .iter()
            .position(|e| e.actor == MARKER && e.act == DRAIN)
            .unwrap_or(journal.len());
        let excluded: BTreeSet<String> = journal[drained..]
            .iter()
            .filter(|e| e.act == "exclude" && e.outcome == Met::Ok)
            .filter(|e| e.view.as_deref() == Some(v))
            .filter_map(|e| e.reply.as_ref())
            .flat_map(|r| {
                ["removed", "cascaded"].into_iter().flat_map(move |k| {
                    r[k].as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|c| c.as_str().map(str::to_string))
                })
            })
            .collect();
        journal
            .iter()
            .filter(|e| e.outcome == Met::Ok)
            .filter(|e| e.change.as_ref().is_some_and(|c| excluded.contains(c)))
            .filter_map(|e| e.block.clone())
            .collect()
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
        // the version's deferral card, as the person accepts it in the run (D-AAQ9Z)
        if !people::accept_deferrals(&mut self.relay, "agent:relay", pref, true) {
            link_fail(&self.cfg, v, "accept_deferrals");
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
        // the relaying agent runs the approve as itself (D-HA7YZ)
        let mut cmd = human_command(&self.at.dir, &self.at.env, &args);
        cmd.env("RDY_ACTOR", RELAY_ACTOR).env("RDY_RUN", RELAY_RUN);
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

    /// The closed table of resolutions (ledger O5). Anything outside it fails the run. A
    /// version that is not ready (`E_PROPOSAL_NOT_READY`, D-ME2HD) is resolved only when each of
    /// its issues is a conflict, a blocking tension, open change-plan items (`E_PLAN_OPEN`) or a
    /// stable change with no briefing (`E_BRIEF_REQUIRED`): the conflicts first, when it has
    /// any; the tensions on a later round. Each `E_BRIEF_REQUIRED` issue's block is briefed and
    /// written again with the token. Then, when an issue is `E_PLAN_OPEN` or the round wrote,
    /// the agent answers the view's plan as it does before its own propose
    /// (`agent::answer_plan`), so the next propose binds the answers. Any other issue code
    /// fails the run, as an outer `E_VALIDATION` did. The conflict and tension issues carry no
    /// block or tension id, so the drain reads them from the version (`pref`), the tensions its
    /// propose reply lists as refusing (`refusing`) and the view's listing, never from the
    /// issues' text.
    fn resolve(&mut self, i: usize, v: &str, pref: &str, refusing: &BTreeSet<String>, r: &Refusal) {
        let issues: Vec<Value> = r.body["issues"].as_array().cloned().unwrap_or_default();
        match r.code.as_str() {
            "E_PROPOSAL_MOVED" => {}
            "E_PROPOSAL_NOT_READY" => {
                let codes: Vec<String> = issues
                    .iter()
                    .map(|x| x["code"].as_str().unwrap_or_default().to_string())
                    .collect();
                if codes.is_empty() {
                    self.no_rule(v, &r.code, "no issue listed");
                }
                if let Some(other) = codes
                    .iter()
                    .find(|c| !RESOLVED_ISSUES.contains(&c.as_str()))
                {
                    self.no_rule(v, other, "land");
                }
                let has = |code: &str| codes.iter().any(|c| c == code);
                let mut wrote = false;
                if has("E_CONFLICT_UNRESOLVED") {
                    let version = self.version(i, v, pref, &r.code);
                    self.resolve_conflicts(i, v, &version, "E_CONFLICT_UNRESOLVED");
                    wrote = true;
                } else if has("E_BLOCKING_TENSION") {
                    let version = self.version(i, v, pref, &r.code);
                    self.resolve_tensions(i, v, &version, refusing, "E_BLOCKING_TENSION");
                    wrote = true;
                }
                for x in issues.iter().filter(|x| x["code"] == "E_BRIEF_REQUIRED") {
                    let Some(b) = x["block"].as_str() else {
                        self.no_rule(v, "E_BRIEF_REQUIRED", "an issue with no block")
                    };
                    self.rebrief(i, v, b);
                    wrote = true;
                }
                if wrote || has("E_PLAN_OPEN") {
                    agent::answer_plan(self.agent(i), v);
                }
            }
            other => self.no_rule(v, other, "land"),
        }
    }

    /// A stable change of view `v` that records no briefing (`E_BRIEF_REQUIRED`, D-VF4RH, O3):
    /// agent `i` briefs its block `b` in `v` and writes the block again with that token, its
    /// body as `v` holds it, on the version the briefing gives.
    fn rebrief(&mut self, i: usize, v: &str, b: &str) {
        let body = self.body(i, b, v);
        let ops = json!([{"op": "update", "id": b, "body": body}]);
        self.write(
            i,
            v,
            b,
            ops,
            "write",
            "E_BRIEF_REQUIRED",
            "a change with no briefing",
        );
    }

    /// Version `pref` as agent `i` reads it (`rdy_read`, whole): its `proposal`. A cut reply
    /// fails the run.
    fn version(&mut self, i: usize, v: &str, pref: &str, code: &str) -> Value {
        let link = self.agent(i);
        let mut e = link.entry("read");
        e.block = Some(pref.to_string());
        let mut got = None;
        link.exec(
            e,
            "read",
            "rdy_read",
            json!({"id": pref, "max_tokens": ALL}),
            |_, r| got = Some(r.clone()),
        );
        let Some(got) = got else {
            link_fail(&self.cfg, v, "read")
        };
        if got.get("omitted").is_some() || got.get("truncated").is_some() {
            self.no_rule(
                v,
                code,
                &format!("{pref}, read with a reply cut for its budget"),
            );
        }
        got["proposal"].clone()
    }

    /// The conflicts of `version` (its `conflicts`, in block-id order): agent `i` writes each
    /// block from canon's side. None listed fails the run.
    fn resolve_conflicts(&mut self, i: usize, v: &str, version: &Value, code: &str) {
        let mut conflicts: Vec<Value> =
            version["conflicts"].as_array().cloned().unwrap_or_default();
        if conflicts.is_empty() {
            self.no_rule(v, code, "no conflict listed");
        }
        conflicts.sort_by(|a, b| {
            a["block"]
                .as_str()
                .unwrap_or_default()
                .cmp(b["block"].as_str().unwrap_or_default())
        });
        for conflict in conflicts {
            let b = conflict["block"]
                .as_str()
                .unwrap_or_else(|| self.no_rule(v, code, "a conflict with no block"))
                .to_string();
            let canon = conflict["sides"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|s| !s["landed"].is_null() || s["change"].is_null())
                .unwrap_or_else(|| self.no_rule(v, code, "a conflict with no canon side"));
            let base = canon["version"].as_str().map(str::to_string);
            let body = self.body(i, &b, "canon");
            let ops = json!([{"op": "update", "id": b, "base": base, "body": body}]);
            self.write(i, v, &b, ops, "write", code, "conflict");
        }
    }

    /// The blocking tensions `version`'s landing would put into canon, as agent `i` lists the
    /// open blocking tensions of view `v`: the rows with a change side, each change side one of
    /// the version's changes, and no intent side (a side with neither is canon), that the propose
    /// reply lists as refusing (`refusing`: the landing form), by id. The view also lists the
    /// in-flight form of each live write of those changes, which the reply lists beside the
    /// landing form as information and which does not refuse: a view that wrote a block again
    /// since still lists each earlier write as against canon, and resolving that row would write
    /// from canon's version, a base the view no longer holds (`E_STALE_BASE`). Each is resolved
    /// by its kind, once per block:
    /// a row whose blocks in the view an earlier row's resolution already wrote is skipped. None
    /// selected fails the run.
    fn resolve_tensions(
        &mut self,
        i: usize,
        v: &str,
        version: &Value,
        refusing: &BTreeSet<String>,
        code: &str,
    ) {
        let changes: BTreeSet<String> = version["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["change"].as_str().map(str::to_string))
            .collect();
        let args = json!({"view": v, "severity": "blocking", "status": "open", "max_tokens": ALL});
        let rows = listing(self.agent(i), args);
        // the listing is by label: its order is the rows' id order
        let selected: Vec<Value> = rows
            .into_values()
            .filter(|row| row["id"].as_str().is_some_and(|id| refusing.contains(id)))
            .filter(|row| {
                let sides = row["sides"].as_array().cloned().unwrap_or_default();
                let mut of_change = sides.iter().filter_map(|s| s["change"].as_str()).peekable();
                of_change.peek().is_some()
                    && of_change.all(|c| changes.contains(c))
                    && sides.iter().all(|s| s["intent"].is_null())
            })
            .collect();
        if selected.is_empty() {
            self.no_rule(v, code, "no blocking tension of the version listed");
        }
        // the view lists one in-flight form per change that wrote a block: the first row's
        // write or repin settles the block for every later row on it
        let mut settled: BTreeSet<String> = BTreeSet::new();
        for row in selected {
            let kind = row["kind"].as_str().unwrap_or_default().to_string();
            if let Err(why) = row_fits(&row, &kind) {
                self.no_rule(v, code, &format!("{kind}, {why}"));
            }
            let own: Vec<String> = Drain::own_blocks(&row, v)
                .into_iter()
                .map(|(b, _)| b)
                .collect();
            if !own.is_empty() && own.iter().all(|b| settled.contains(b)) {
                continue;
            }
            self.resolve_tension(i, v, &row, code, &kind);
            settled.extend(own);
        }
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
        self.write_noted(i, v, b, ops, act, code, kind, None);
    }

    /// `write`, with the entry naming `target` (a repair's dangling reference). An update op with
    /// no `base` key takes the version the briefing gives for `b` in `v`.
    #[allow(clippy::too_many_arguments)]
    fn write_noted(
        &mut self,
        i: usize,
        v: &str,
        b: &str,
        mut ops: Value,
        act: &'static str,
        code: &str,
        kind: &str,
        target: Option<&str>,
    ) {
        let decision = self.seed.decisions[self.area(b)].clone();
        let link = self.agent(i);
        let mut token = String::new();
        let mut version = Value::Null;
        let e = link.entry("brief");
        link.exec(
            e,
            "brief",
            "rdy_brief",
            json!({"ids": [b], "scope": v}),
            |en, r| {
                token = r["base"]["token"].as_str().unwrap_or_default().to_string();
                version = r["base"]["blocks"][0]["version"].clone();
                en.brief = Some(token.clone());
            },
        );
        if ops[0]["op"] == "update" && ops[0].get("base").is_none() {
            ops[0]["base"] = version;
        }
        let mut e = link.entry(act);
        e.view = Some(v.to_string());
        e.block = Some(b.to_string());
        e.target = target.map(str::to_string);
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
                "swarm seed {}: view {v} met {c} ({kind}), which the drain has no rule for\n  while resolving {code}; the daemon said: {message}",
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

    /// Final review M8 (minors items 23 and 39): a proposal that chooses a set member, or a
    /// winner's later work, never passes a queued contra first claim; each place keeps its
    /// proposals oldest first.
    #[test]
    fn sg5_a_choosing_proposal_never_passes_a_queued_first_claim() {
        let mut q: Vec<String> = vec![];
        let mut ranks = BTreeMap::new();
        enqueue(&mut q, "P-o1@1".into(), Place::Behind, &mut ranks);
        enqueue(&mut q, "P-f1@1".into(), Place::FirstClaim, &mut ranks);
        enqueue(&mut q, "P-c1@1".into(), Place::Ahead, &mut ranks);
        assert_eq!(q, ["P-f1@1", "P-c1@1", "P-o1@1"]);
        enqueue(&mut q, "P-f2@1".into(), Place::FirstClaim, &mut ranks);
        enqueue(&mut q, "P-c2@1".into(), Place::Ahead, &mut ranks);
        assert_eq!(q, ["P-f1@1", "P-f2@1", "P-c1@1", "P-c2@1", "P-o1@1"]);
        // a refused first claim waits at the back: a choosing proposal still comes after it
        q.retain(|x| x != "P-f1@1");
        q.push("P-f1@1".into());
        enqueue(&mut q, "P-c3@1".into(), Place::Ahead, &mut ranks);
        assert_eq!(q.last().map(String::as_str), Some("P-c3@1"));
        // a newer version takes the old one's place; an older one is dropped
        enqueue(&mut q, "P-o1@2".into(), Place::Behind, &mut ranks);
        enqueue(&mut q, "P-c1@0".into(), Place::Ahead, &mut ranks);
        assert!(q.contains(&"P-o1@2".to_string()) && !q.contains(&"P-o1@1".to_string()));
        assert!(q.contains(&"P-c1@1".to_string()) && !q.contains(&"P-c1@0".to_string()));
    }

    #[test]
    fn sg5_a_marker_and_the_space_before_it_are_stripped() {
        assert_eq!(
            strip_marker("Agent b forbids that settlement is at once. CONTRA-12"),
            "Agent b forbids that settlement is at once."
        );
        assert_eq!(strip_marker("CONTRA-3 first"), " first");
        assert_eq!(strip_marker("no marker"), "no marker");
    }

    /// Adversarial review: the drain picks its row by the kind the refusal's issue names, and
    /// holds the listed row to it: the listed kind must be that kind, and the `semantic` row is
    /// only for a semantic tension a person accepted (D-WMCFW: only an accepted one blocks).
    #[test]
    fn sg5_the_drain_resolves_only_the_tensions_a_propose_reply_lists_as_refusing() {
        let reply = json!([
            {"id": "tn-000000000001", "refuses": true},
            {"id": "tn-000000000002", "refuses": false},
            {"id": "tn-000000000003"},
            {"id": "tn-000000000004", "refuses": true},
        ]);
        let got: Vec<String> = refusing_of(&reply).into_iter().collect();
        assert_eq!(got, ["tn-000000000001", "tn-000000000004"]);
        assert!(refusing_of(&Value::Null).is_empty());
    }

    #[test]
    fn sg5_the_drain_holds_a_blocking_tension_row_to_its_kind() {
        let accepted = json!({"kind": "semantic", "judged": {"accepted": {"person": "p"}}});
        let unaccepted = json!({"kind": "semantic", "judged": {"advice": "block"}});
        assert_eq!(row_fits(&accepted, "semantic"), Ok(()));
        assert!(row_fits(&unaccepted, "semantic").is_err());
        assert!(row_fits(&json!({"kind": "semantic"}), "semantic").is_err());
        assert_eq!(
            row_fits(&json!({"kind": "premise_shift"}), "premise_shift"),
            Ok(())
        );
        assert!(row_fits(&json!({"kind": "edit_collision"}), "premise_shift").is_err());
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
    fn sg5_a_refusal_names_its_dangling_reference() {
        assert_eq!(
            dangling_in("c-NGH43 references c-18JGF, which does not exist in canon"),
            Some(("c-NGH43".to_string(), "c-18JGF".to_string()))
        );
        assert_eq!(dangling_in("c-NGH43 is too deep"), None);
    }

    /// Ruling task-5-round2.1: the repair replaces each reference to the target with its link
    /// words, or with the target's last name when it has none, and keeps every other reference
    /// and escape as written.
    #[test]
    fn sg5_a_repair_writes_the_reference_as_plain_words() {
        let name = "Agent a four adds a rule";
        assert_eq!(
            unref("Agent b builds on {{c-0MKAA}} in pass c.", "c-0MKAA", name),
            "Agent b builds on Agent a four adds a rule in pass c."
        );
        assert_eq!(
            unref(
                "See {{ c-0MKAA | the rule }} and {{c-1AAAA}}.",
                "c-0MKAA",
                name
            ),
            "See the rule and {{c-1AAAA}}."
        );
        assert_eq!(
            unref("\\{{c-0MKAA}} {{c-0MKAA|x}} {{c-0MKAA}}", "c-0MKAA", name),
            "\\{{c-0MKAA}} x Agent a four adds a rule"
        );
        assert_eq!(unref("no reference", "c-0MKAA", name), "no reference");
    }

    /// The target's last name: its title, else its name field, else its text's first sentence,
    /// with references as their link words (dropped when they have none) and no CONTRA marker.
    #[test]
    fn sg5_a_targets_last_name_is_what_a_reader_calls_it() {
        let yaml = |s: &str| serde_yaml_ng::from_str::<serde_yaml_ng::Value>(s).unwrap();
        assert_eq!(
            name_in(&yaml(
                "type: doc\nfields:\n  title: Setup guide\nbody: ''\n"
            )),
            "Setup guide"
        );
        assert_eq!(
            name_in(&yaml("type: claim\nfields:\n  name: settle-rule\n")),
            "settle-rule"
        );
        assert_eq!(
            name_in(&yaml(
                "type: claim\nbody: Agent d forbids that settlement is at once. CONTRA-3\n"
            )),
            "Agent d forbids that settlement is at once"
        );
        assert_eq!(
            name_in(&yaml(
                "type: claim\nbody: 'Agent e builds on {{c-1AAAA}} in pass b. More.'\n"
            )),
            "Agent e builds on in pass b"
        );
        assert_eq!(
            name_in(&yaml("type: claim\nbody: 'See {{c-1AAAA|the rule}}'\n")),
            "See the rule"
        );
    }

    /// Ruling task-5.4 (fix round 1 of the promoted review): the drain reads an alternative
    /// refusal at its propose against the sets the relay lists. It excludes a change only when
    /// the refusal names a member that the listing shows lost a chosen set. A change bound to a
    /// member of an open set is never excluded: the drain lands that member first.
    #[test]
    fn sg5_an_alternative_refusal_is_read_against_the_listed_sets() {
        let sets = json!([
            {"id": "as-111111111111", "status": "open", "members": [
                {"label": "a0", "view": "a0"}, {"label": "a1", "view": "a1"}]},
            {"id": "as-222222222222", "status": "chosen",
             "chosen": {"member": "a3", "landing": "abc"}, "members": [
                {"label": "a2", "view": "a2"}, {"label": "a3", "view": "a3"}]},
            {"id": "as-333333333333", "status": "open", "members": [
                {"label": "a4", "view": "a4"}, {"label": "a5", "view": "a5-fb"}]}
        ]);
        let open = |pairs: &[(&str, &str)]| {
            Named::Open(
                pairs
                    .iter()
                    .map(|(s, m)| ((*s).to_string(), (*m).to_string()))
                    .collect(),
            )
        };
        assert_eq!(
            alt_named(
                "change abcdef123456 is as-111111111111 member a0's alternative",
                &sets
            ),
            open(&[("as-111111111111", "a0")])
        );
        // the member is named by its label, and the drain lands its view
        assert_eq!(
            alt_named(
                "change abcdef123456 is bound in 2 open sets: as-111111111111 member a1, as-333333333333 member a5; one proposal chooses in one set only",
                &sets
            ),
            open(&[("as-111111111111", "a1"), ("as-333333333333", "a5-fb")])
        );
        // a label that only starts another label names nothing
        assert_eq!(
            alt_named(
                "change abcdef123456 is as-111111111111 member a01's alternative",
                &sets
            ),
            Named::Unknown
        );
        assert_eq!(
            alt_named(
                "change abcdef123456 belongs to member a2, which lost set as-222222222222 (chosen by landing 1234, which recorded no decision)",
                &sets
            ),
            Named::Lost
        );
        // the winner, or a member of a set the listing shows open, did not lose
        assert_eq!(
            alt_named(
                "change abcdef123456 belongs to member a3, which lost set as-222222222222 (chosen by landing 1234, which recorded no decision)",
                &sets
            ),
            Named::Unknown
        );
        assert_eq!(
            alt_named(
                "change abcdef123456 belongs to member a0, which lost set as-111111111111 (chosen by landing 1234, which recorded no decision)",
                &sets
            ),
            Named::Unknown
        );
        assert_eq!(alt_named("no set here", &sets), Named::Unknown);
    }

    /// The members the drain lands to settle an open set: the bound member first, then the
    /// others in the drain's order. A view the drain is landing now (up its call chain) is left
    /// out: it cannot land before the view that waits on it.
    #[test]
    fn sg5_an_open_set_is_settled_by_its_bound_member_first() {
        let set = json!({"id": "as-111111111111", "status": "open", "members": [
            {"label": "a0", "view": "a0"}, {"label": "a1", "view": "a1"},
            {"label": "a2", "view": "a2"}]});
        let order: Vec<String> = ["a2", "a1", "a0"].map(String::from).to_vec();
        assert_eq!(settle_order(&set, "a0", &order, &[]), ["a0", "a2", "a1"]);
        assert_eq!(
            settle_order(&set, "a1", &order, &["a2".into()]),
            ["a1", "a0"]
        );
        let busy: Vec<String> = ["a0", "a1", "a2"].map(String::from).to_vec();
        assert!(settle_order(&set, "a0", &order, &busy).is_empty());
    }

    #[test]
    fn sg5_a_view_belongs_to_the_agent_its_name_gives() {
        assert_eq!(owner_of("a3"), 3);
        assert_eq!(owner_of("a12-fb"), 12);
    }
}
