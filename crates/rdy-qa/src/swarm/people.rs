//! The simulated person. The person acts through a relaying agent session (`agent:relay`, run
//! `chat`, role `reviewer`) for every act that has a tool (FW-UX1, D-KQ4CA; F5, D-VF4RH). The
//! command line is the fallback, and only for this closed list:
//!
//! - `rdy approve`, relayed (`--relayed-by agent:relay`, the path FW-B9 gives; no MCP tool
//!   approves in M2) and, about one landing in five, direct (CLI parity);
//! - before a direct approve, `rdy pending` and the answer that accepts the version's deferral
//!   card, `rdy answer defer:<P-x@n> o1` (D-AAQ9Z, CLI parity; before a relayed approve the
//!   relay reads the queue with `rdy_pending` and relays the answer with `rdy_answer`);
//! - the working copy: `rdy checkout`, `rdy add`, `rdy commit`, `rdy update`, and, for the
//!   human's view `work`, which no agent may act in, `rdy plan --view work` and the answers to
//!   its open items, `rdy plan confirm|defer <items> --reason <why> --view work` (ruling task-15
//!   F1), then `rdy propose`.
//!
//! The person accepts a check verdict as blocking through the relay too (`rdy_pending`, then
//! `rdy_answer` with the card's `accept_verdict:` option; D-JQD5X QD2).
//!
//! The person lands the proposal at the front of a shared queue (the run orders it). A proposal
//! made before its view joined an alternative set it drops, and journals the drop as a skipped
//! `land` (`Skip::ProposedBeforeSet`) that names the proposal. Journal entries of the
//! relay carry the actor `agent:relay`; those of a direct command carry `human:qa`, the human
//! the project's user config names.
//!
//! ap13 (D-ZWP7Q "Disputes"): on a seeded draw of 1 in 8 of its steps (`dispute_draw`), after the
//! step's act, the person resolves `dismissed` one dispute it opened on an earlier step, if one is
//! open (`rdy dispute resolve`), and opens a dispute on a landing drawn from the seed's landings
//! and those the journal names (`rdy dispute open`). Both are direct commands of the person:
//! `dispute_open` entries name the landing in `block`, `dispute_resolve` entries the dispute, and
//! each `reply` is the command's. A queued proposal that an agent already landed under the
//! policy (an `Ok` `land` entry names it) leaves the queue unlanded.
use super::agent::{
    self, KINDS, KillGate, Link, Pace, Res, Where, advised_labels, inbox_labels, labels_of, spell,
};
use super::journal::{CheckFacts, Entry, Journal, Outcome, Skip};
use super::project::{CLAIMS_PER_AREA, DOC_PATHS, SeedBlocks, SwarmProject};
use super::run::{self, Watch};
use super::script::PersonAct;
use crate::daemon::human_command;
use crate::fixtures::output_within;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

/// A command's hang guard, not a speed check.
const CLI_BUDGET: Duration = Duration::from_secs(120);
/// The actor a direct command journals as (the user config `Served` writes names it).
const HUMAN: &str = "human:qa";
/// The relay as `rdy approve --relayed-by` names it: the relaying agent's own actor alone; the
/// relay records the session's run (D-PWB82).
const RELAY: &str = RELAY_ACTOR;
/// The relaying agent's actor and run: the relay runs as this session (D-HA7YZ).
const RELAY_ACTOR: &str = "agent:relay";
const RELAY_RUN: &str = "chat";
/// The prefix of a card's option that accepts a check verdict (`accept_verdict:<review>:<version>`).
pub const ACCEPT_PREFIX: &str = "accept_verdict:";
/// How many queued proposals the person looks at when it hunts for advice to accept: the queue
/// a fast run holds, so a late contradiction pair's proposal is seen (ruling task-5.2).
const ADVICE_LOOKS: usize = 24;

/// The proposals waiting for the person to land, oldest first.
pub type Queue = Arc<Mutex<Vec<String>>>;

/// What a run adds to the person (sg5): a wait before each act, and the reads around each
/// landing.
#[derive(Clone, Default)]
pub(crate) struct Hooks {
    /// Called with the step before each act; it returns when the act may run.
    pub pace: Option<Pace>,
    /// Where the landings' violations go. With it, the relay reads canon's blocking tensions
    /// before and after each landing, and each land entry names the proposal's view and the
    /// view's effective base before the landing (`Entry::view`, `Entry::base`).
    pub watch: Option<Watch>,
    /// How many acts the person has finished: the run paces the agents by it too.
    pub done: Option<Arc<AtomicUsize>>,
}

/// Starts the person: one thread, one relay session. It runs `acts` and returns.
pub fn spawn(
    p: &SwarmProject,
    acts: Vec<PersonAct>,
    journal: Journal,
    queue: Queue,
    gate: Arc<KillGate>,
) -> JoinHandle<()> {
    spawn_with(p, acts, journal, queue, gate, Hooks::default())
}

/// `spawn`, with the run's `hooks`.
pub(crate) fn spawn_with(
    p: &SwarmProject,
    acts: Vec<PersonAct>,
    journal: Journal,
    queue: Queue,
    gate: Arc<KillGate>,
    hooks: Hooks,
) -> JoinHandle<()> {
    let at = Where::of(p);
    let seed = p.seed.clone();
    let seed_landings = p.seed_landings.clone();
    std::thread::spawn(move || {
        let link = Link::connect(at, "agent:relay", "chat", "reviewer", None, journal, gate);
        let mut person = Person {
            link,
            seed,
            queue,
            checked_out: false,
            hooks,
            seed_landings,
            disputes: Vec::new(),
        };
        for (step, act) in acts.iter().enumerate() {
            if let Some(pace) = &person.hooks.pace {
                pace(step);
            }
            person.link.step = step;
            person.act(act);
            person.disputes();
            if let Some(done) = &person.hooks.done {
                done.fetch_add(1, Ordering::SeqCst);
            }
        }
    })
}

struct Person {
    link: Link,
    seed: SeedBlocks,
    queue: Queue,
    /// Whether `rdy checkout work` has run.
    checked_out: bool,
    hooks: Hooks,
    /// The landings that seeded the project: a dispute may name one.
    seed_landings: Vec<String>,
    /// The disputes the person opened and has not resolved, each with the step that opened it.
    disputes: Vec<(String, usize)>,
}

/// The salt of the dispute draw.
const DISPUTE: u64 = 0x6469_7370_7574_6500;

/// ap13: whether the person's step `step` of the run with `seed` makes its dispute acts: a seeded
/// draw of 1 in 8.
pub fn dispute_draw(seed: u64, step: usize) -> bool {
    agent::drawn(seed, DISPUTE, step as u64, 8)
}

/// Whether an `Ok` `land` entry names proposal `pref`: an agent landed it under the policy, or the
/// person did.
fn landed_already(entries: &[Entry], pref: &str) -> bool {
    entries
        .iter()
        .any(|x| x.act == "land" && x.outcome == Outcome::Ok && x.block.as_deref() == Some(pref))
}

/// Whether queued proposal `pref` was made before its view joined an alternative set (ruling
/// task-5-round1.2): the journal's `Ok` propose entry that names it comes before an `Ok`
/// `alt_create` entry that names its view, as the set's maker (`view`) or as the other member
/// (`block`). Such a proposal chose no member, and the person drops it. A proposal made after
/// the set (the winner's later work among them) goes to the gate.
pub(crate) fn proposed_before_set(entries: &[Entry], pref: &str) -> bool {
    let Some(at) = entries.iter().position(|x| {
        x.act == "propose" && x.outcome == Outcome::Ok && x.block.as_deref() == Some(pref)
    }) else {
        return false;
    };
    let Some(view) = entries[at].view.as_deref() else {
        return false;
    };
    entries[at + 1..].iter().any(|x| {
        x.act == "alt_create"
            && x.outcome == Outcome::Ok
            && (x.view.as_deref() == Some(view) || x.block.as_deref() == Some(view))
    })
}

/// The JSON a command printed: its stdout on success, its last stderr line on failure.
fn json_of(text: &str) -> Value {
    serde_json::from_str(text.trim())
        .or_else(|_| serde_json::from_str(text.trim().lines().last().unwrap_or("")))
        .unwrap_or_else(|_| Value::String(text.trim().to_string()))
}

/// `rdy --project <dir> --json <args>` on `link`'s project, as the person, or as the relaying
/// agent (`RDY_ACTOR=agent:relay`, `RDY_RUN=chat`) when `relay`. A command the daemon died under,
/// or could not start for, is `Lost`.
pub(crate) fn cli_in(link: &mut Link, act: &str, args: &[&str], relay: bool) -> Res {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let mut cmd = human_command(&link.at.dir, &link.at.env, &all);
    if relay {
        cmd.env("RDY_ACTOR", RELAY_ACTOR).env("RDY_RUN", RELAY_RUN);
    }
    let out = match output_within(&mut cmd, CLI_BUDGET) {
        Ok(o) => o,
        Err(e) => link.fail(act, &format!("hung: {e}")),
    };
    if out.status.success() {
        return Res::Ok(json_of(&String::from_utf8_lossy(&out.stdout)));
    }
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    let v = json_of(&err);
    match v["code"].as_str() {
        Some(code @ ("E_DAEMON_GONE" | "E_DAEMON_START")) => link.lost(act, code),
        Some(code) => Res::Refused(
            code.to_string(),
            v["message"].as_str().unwrap_or_default().to_string(),
        ),
        None => link.fail(act, &format!("failed: {}", err.trim())),
    }
}

/// Before the person approves `pref` (D-AAQ9Z): when the queue lists the version's deferral card
/// `defer:<pref>`, the person accepts it with option `o1` (the `accept_deferrals` act), as `who`
/// approves. Relayed, the relaying agent reads the queue with `rdy_pending` and relays the
/// answer with `rdy_answer` through `link`, naming the card it showed, the person's words and
/// the chat session (FW-UX1: every act with a tool goes through it). Direct, the person runs
/// `rdy pending` and `rdy answer defer:<pref> o1` (CLI parity). The read is a `pending` entry
/// whose reply is `{via}`, the tool or command that read it; the answer an `accept_deferrals`
/// entry whose reply is `{via, answer}`, with the answer's reply; each names `pref`. `false`
/// when the daemon died under a call: the approve waits.
pub(crate) fn accept_deferrals(link: &mut Link, who: &str, pref: &str, relayed: bool) -> bool {
    let card = format!("defer:{pref}");
    let mut e = Entry::new(who, link.step, "pending", Outcome::Ok);
    e.epoch = link.epoch;
    e.block = Some(pref.to_string());
    let via = if relayed {
        "rdy_pending"
    } else {
        "rdy pending"
    };
    e.reply = Some(json!({ "via": via }));
    // a card's budget cuts its text, never its hash
    let res = if relayed {
        link.raw("pending", "rdy_pending", json!({"session_ref": RELAY_RUN}))
    } else {
        cli_in(link, "pending", &["pending"], false)
    };
    let mut hash = None;
    let res = link.finish(e, "pending", res, |_, v| {
        hash = v["items"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|i| i["ref"] == card.as_str())
            .and_then(|i| i["card_hash"].as_str())
            .map(str::to_string);
    });
    if matches!(res, Res::Lost) {
        return false;
    }
    let Some(hash) = hash else { return true };
    let mut e = Entry::new(who, link.step, "accept_deferrals", Outcome::Ok);
    e.epoch = link.epoch;
    e.block = Some(pref.to_string());
    let (via, res) = if relayed {
        let args = json!({"ref": card, "option": "o1", "card_hash": hash,
            "evidence": format!("Accept the deferrals of {pref}."),
            "question": format!("Accept the deferred plan items of {pref}?"),
            "session_ref": RELAY_RUN});
        let res = link.raw("accept_deferrals", "rdy_answer", args);
        ("rdy_answer", res)
    } else {
        let args = ["answer", card.as_str(), "o1", "--card", hash.as_str()];
        ("rdy answer", cli_in(link, "accept_deferrals", &args, false))
    };
    let res = link.finish(e, "accept_deferrals", res, |en, v| {
        en.reply = Some(json!({"via": via, "answer": v}));
    });
    !matches!(res, Res::Lost)
}

/// Whether proposal `pref` landed, as `link`'s actor reads it (`rdy_read`), journaled as a
/// `land_found` entry that names the proposal, the view and the base `before` read. When it
/// landed with a landing no entry names yet, the entry's reply names the landing, and with
/// `watch` (the run's) the reads after a landing run on `before`: a landing whose reply the kill
/// cut is audited too. A read the kill cuts is made again on the restarted daemon (the link has
/// reconnected; a second loss fails the run), so `before` stays the read made before the
/// landing. A landing an entry names already (a version that landed with a later version's
/// landing, `E_ALREADY_LANDED`) is not named again: its base is the one read before it.
/// After `E_ALREADY_LANDED` (`refused`), `before` was read after the landing: a landing no
/// entry names then would be audited on the wrong base, so the run fails.
pub(crate) fn found_in(
    link: &mut Link,
    watch: Option<&Watch>,
    pref: &str,
    before: run::Before,
    refused: bool,
) -> bool {
    let (e, res) = loop {
        let mut e = link.entry(run::LAND_FOUND);
        e.block = Some(pref.to_string());
        e.view = Some(before.view.clone());
        e.base.clone_from(&before.base);
        let res = link.raw(run::LAND_FOUND, "rdy_read", json!({"id": pref}));
        if !matches!(res, Res::Lost) {
            break (e, res);
        }
        // journaled `Lost`; the link waits for the restart and reconnects
        link.finish(e, run::LAND_FOUND, res, |_, _| {});
    };
    let landing = match &res {
        Res::Ok(v) if v["proposal"]["state"] == "landed" => {
            v["proposal"]["landing"].as_str().map(str::to_string)
        }
        _ => None,
    };
    let named = landing.as_deref().is_some_and(|l| {
        link.journal.entries().iter().any(|x| {
            ((x.act == "land" && x.outcome == Outcome::Ok) || x.act == run::LAND_FOUND)
                && x.reply.as_ref().is_some_and(|r| r["landing"] == l)
        })
    });
    if let (Some(l), false, true) = (&landing, named, refused) {
        link.fail(
            run::LAND_FOUND,
            &format!(
                "found {pref} landed with landing {l}, which no entry names: the reads before it came after it"
            ),
        );
    }
    link.finish(e, run::LAND_FOUND, res, |en, _| {
        if let (Some(l), false) = (&landing, named) {
            en.reply = Some(json!({"landing": l}));
        }
    });
    let Some(l) = landing else { return false };
    if !named && let Some(w) = watch {
        run::after_land(link, w, before, &json!({"landing": l}));
    }
    true
}

impl Person {
    fn act(&mut self, act: &PersonAct) {
        // an act that can change a tension waits while an agent answers its plan; a landing
        // holds the landing turn instead, which every plan answer waits for (`KillGate`); a
        // working-copy edit answers a plan itself, so it takes its turns step by step
        let gate = self.link.gate.clone();
        let _act = (!matches!(
            act,
            PersonAct::Land { .. } | PersonAct::WcEdit | PersonAct::AcceptCheckVerdict
        ))
        .then(|| gate.act_turn());
        match act {
            PersonAct::Land { relayed } => self.land(*relayed),
            PersonAct::AcceptCheckVerdict => self.accept_check_verdict(),
            PersonAct::Dismiss => self.dismiss(),
            PersonAct::AcceptAdvice => self.accept_advice(),
            PersonAct::WcEdit => self.wc_edit(),
            PersonAct::InboxAck => self.inbox_ack(),
            PersonAct::Tensions => {
                self.tensions();
            }
        }
    }

    fn entry(&self, who: &str, act: &'static str) -> Entry {
        let mut e = Entry::new(who, self.link.step, act, Outcome::Ok);
        e.epoch = self.link.epoch;
        e
    }

    /// `rdy --project <dir> --json <args>` as the person. A command the daemon died under, or
    /// could not start for, is `Lost`.
    fn cli(&mut self, act: &str, args: &[&str]) -> Res {
        self.cli_as(act, args, false)
    }

    /// `cli`, run as the relaying agent (`RDY_ACTOR=agent:relay`, `RDY_RUN=chat`) when `relay`.
    fn cli_as(&mut self, act: &str, args: &[&str], relay: bool) -> Res {
        cli_in(&mut self.link, act, args, relay)
    }

    // ---- landing ----

    fn land(&mut self, relayed: bool) {
        let who = if relayed { "agent:relay" } else { HUMAN };
        let mut e = self.entry(who, "land");
        // the person's turn, from the pick to the approve: no agent act that decides from a read,
        // and no alternative set being made, runs while canon moves (fix round 1)
        let gate = self.link.gate.clone();
        let _turn = gate.landing_turn();
        let next = loop {
            let r = {
                let mut q = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
                (!q.is_empty()).then(|| q.remove(0))
            };
            match r {
                // made before its view joined a set: dropped, and the drop is journaled
                // an agent landed it under the policy (ap13): it leaves the queue
                Some(r) if landed_already(&self.link.journal.entries(), &r) => {}
                Some(r) if proposed_before_set(&self.link.journal.entries(), &r) => {
                    let mut d = self.entry(who, "land");
                    d.outcome = Outcome::Skipped;
                    d.skip = Some(Skip::ProposedBeforeSet);
                    d.block = Some(r);
                    self.link.journal.record(d);
                }
                r => break r,
            }
        };
        let Some(pref) = next else {
            e.outcome = Outcome::Skipped;
            self.link.journal.record(e);
            return;
        };
        e.block = Some(pref.clone());
        let before = match &self.hooks.watch {
            None => None,
            Some(_) => match run::before_land(&mut self.link, &pref) {
                Some(b) => {
                    e.view = Some(b.view.clone());
                    e.base.clone_from(&b.base);
                    Some(b)
                }
                // the daemon died under a read: the proposal waits at the head of the queue
                None => {
                    self.queue
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(0, pref);
                    return;
                }
            },
        };
        // the version's deferral card first (D-AAQ9Z): the approve waits for it
        if !accept_deferrals(&mut self.link, who, &pref, relayed) {
            self.queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(0, pref);
            return;
        }
        let evidence = format!("Approve {pref}.");
        let question = format!("Land {pref}?");
        let args: Vec<&str> = if relayed {
            vec![
                "approve",
                &pref,
                "--relayed-by",
                RELAY,
                "--evidence",
                &evidence,
                "--question",
                &question,
                "--session",
                "chat",
            ]
        } else {
            vec!["approve", &pref]
        };
        // the relay runs as the agent itself; the direct approve is the person's
        let res = self.cli_as("land", &args, relayed);
        let res = self.link.finish(e, "land", res, |en, v| {
            en.reply = Some(v.clone());
        });
        let found = match (before, &res) {
            (Some(b), Res::Ok(v)) => {
                if let Some(w) = &self.hooks.watch {
                    run::after_land(&mut self.link, w, b, v);
                }
                true
            }
            // the kill cut the reply, or an earlier cut reply landed it: whether it landed is
            // read now, and a landing found is audited like any other (fix round 1, finding 4)
            (Some(b), Res::Lost) => self.found(&pref, b, false),
            (Some(b), Res::Refused(code, _)) if code == "E_ALREADY_LANDED" => {
                self.found(&pref, b, true)
            }
            _ => false,
        };
        let mut q = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        match &res {
            Res::Ok(_) => {}
            // a newer version took its place, or it landed already: the ref is dead
            Res::Refused(code, _) if code == "E_PROPOSAL_MOVED" || code == "E_ALREADY_LANDED" => {}
            // the gate refused it for now: it waits behind the others
            Res::Refused(..) => q.push(pref),
            // the kill landed it: the ref is dead
            Res::Lost if found => {}
            // the kill may or may not have landed it: it goes first, and `E_ALREADY_LANDED`
            // answers if it did
            Res::Lost => q.insert(0, pref),
        }
    }

    /// `found_in` on the relay's link, with the run's hooks.
    fn found(&mut self, pref: &str, before: run::Before, refused: bool) -> bool {
        let watch = self.hooks.watch.clone();
        found_in(&mut self.link, watch.as_ref(), pref, before, refused)
    }

    // ---- disputes (ap13) ----

    /// On a drawn step (`dispute_draw`): resolves `dismissed` the oldest dispute the person opened
    /// on an earlier step, if one is open, then opens a dispute on a landing drawn from the seed's
    /// landings and the journal's. Both are the person's direct commands. A dispute shows and
    /// never blocks (D-ZWP7Q), so neither takes a turn.
    fn disputes(&mut self) {
        let step = self.link.step;
        if !dispute_draw(self.link.at.seed, step) {
            return;
        }
        if let Some(at) = self.disputes.iter().position(|(_, s)| *s < step) {
            let (id, _) = self.disputes.remove(at);
            let mut e = self.entry(HUMAN, "dispute_resolve");
            e.block = Some(id.clone());
            let args = ["dispute", "resolve", id.as_str(), "--outcome", "dismissed"];
            let res = self.cli("dispute_resolve", &args);
            self.link.finish(e, "dispute_resolve", res, |en, v| {
                en.reply = Some(v.clone());
            });
        }
        let mut landings = self.seed_landings.clone();
        for x in self.link.journal.entries() {
            let landed = (x.act == "land" && x.outcome == Outcome::Ok) || x.act == run::LAND_FOUND;
            if let Some(l) = x.reply.as_ref().and_then(|r| r["landing"].as_str())
                && landed
                && !landings.iter().any(|y| y == l)
            {
                landings.push(l.to_string());
            }
        }
        let pick = agent::seeded(self.link.at.seed, DISPUTE ^ 1, step as u64) as usize;
        let landing = landings[pick % landings.len()].clone();
        let reason = format!("The person disputes this landing at step {}.", spell(step));
        let mut e = self.entry(HUMAN, "dispute_open");
        e.block = Some(landing.clone());
        let args = [
            "dispute",
            "open",
            landing.as_str(),
            "--reason",
            reason.as_str(),
        ];
        let res = self.cli("dispute_open", &args);
        let mut opened = None;
        self.link.finish(e, "dispute_open", res, |en, v| {
            en.reply = Some(v.clone());
            opened = v["dispute"]["id"].as_str().map(str::to_string);
        });
        if let Some(id) = opened {
            self.disputes.push((id, step));
        }
    }

    // ---- through the relay ----

    /// The relay reads canon's tensions of one kind, the kind going round with the step, which
    /// keeps the read inside its budget in the deep tier (ruling task-5.5).
    fn tensions(&mut self) -> Option<Value> {
        let e = self.entry("agent:relay", "tensions");
        let kind = KINDS[self.link.step % KINDS.len()];
        let res = self.link.exec(
            e,
            "tensions",
            "rdy_tensions",
            json!({"scope": "canon", "kind": kind, "max_tokens": 100_000}),
            |en, v| en.seen = labels_of(&v["tensions"]),
        );
        let Res::Ok(v) = res else { return None };
        if v.get("omitted").is_some() {
            self.link.fail("tensions", "got a reply cut for its budget");
        }
        Some(v)
    }

    fn dismiss(&mut self) {
        // the relay lists the blocking edit collisions or premise shifts (in turn) of one agent
        // view (the view going round with the step; the narrow read keeps inside its budget in the
        // deep tier, ruling task-5.5) and dismisses the first with a change side of a writer
        // agent: it is a reviewer of another actor (D-DAFWP)
        let mut views: Vec<String> = self
            .link
            .journal
            .entries()
            .into_iter()
            .filter(|x| x.actor.starts_with("agent:a") && x.outcome == Outcome::Ok)
            .filter(|x| x.change.is_some())
            .filter_map(|x| x.view)
            .collect();
        views.sort();
        views.dedup();
        let step = self.link.step;
        let kind = ["edit_collision", "premise_shift"][step % 2];
        let mut args = json!({"severity": "blocking", "kind": kind, "max_tokens": 100_000});
        if !views.is_empty() {
            args["view"] = json!(views[step / 2 % views.len()]);
        }
        let e = self.entry("agent:relay", "tensions");
        let res = self
            .link
            .exec(e, "tensions", "rdy_tensions", args, |en, v| {
                en.seen = labels_of(&v["tensions"]);
            });
        let Res::Ok(v) = res else { return };
        if v.get("omitted").is_some() {
            self.link.fail("dismiss", "got a reply cut for its budget");
        }
        let target = v["tensions"].as_array().into_iter().flatten().find(|t| {
            t["sides"].as_array().into_iter().flatten().any(|s| {
                s["change"].is_string()
                    && s["actor"]
                        .as_str()
                        .is_some_and(|a| a.starts_with("agent:a"))
            })
        });
        let mut e = self.entry("agent:relay", "dismiss");
        let Some(id) = target.and_then(|t| t["id"].as_str()).map(str::to_string) else {
            e.outcome = Outcome::Skipped;
            self.link.journal.record(e);
            return;
        };
        e.block = Some(id.clone());
        let reason = format!("scripted dismissal {}", self.link.step);
        self.link.exec(
            e,
            "dismiss",
            "rdy_resolve",
            json!({"tension": id, "dismiss": {"reason": reason}}),
            |_, _| {},
        );
    }

    fn accept_advice(&mut self) {
        let queued: Vec<String> = self
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .take(ADVICE_LOOKS)
            .cloned()
            .collect();
        let mut found = None;
        for pref in queued {
            let mut e = self.entry("agent:relay", "read");
            e.block = Some(pref.clone());
            let mut digest = None;
            let res = self
                .link
                .exec(e, "read", "rdy_read", json!({"id": pref}), |en, v| {
                    let p = &v["proposal"];
                    en.seen = advised_labels(p);
                    let open = p["advised"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|t| t["accepted"] == false));
                    if open {
                        digest = p["advised_digest"].as_str().map(str::to_string);
                    }
                });
            match (res, digest) {
                (Res::Ok(_), Some(d)) => {
                    found = Some((pref, d));
                    break;
                }
                (Res::Lost, _) => return,
                _ => {}
            }
        }
        let mut e = self.entry("agent:relay", "accept_advice");
        let Some((pref, digest)) = found else {
            e.outcome = Outcome::Skipped;
            self.link.journal.record(e);
            return;
        };
        e.block = Some(pref.clone());
        let accept = json!({"proposal": pref, "advised_digest": digest,
            "evidence": format!("Accept the advice on {pref}."),
            "question": format!("Accept the reviewers' advice on {pref}?"),
            "session_ref": "chat"});
        self.link.exec(
            e,
            "accept_advice",
            "rdy_resolve",
            json!({"accept": accept}),
            |_, _| {},
        );
    }

    /// The person accepts a check verdict as blocking (D-JQD5X QD2), through the relay: the relay
    /// reads the queue (`rdy_pending`, a `pending` entry) and relays the answer that takes the
    /// first `accept_verdict:` option of the first card that offers one (`rdy_answer` with the
    /// card's hash and the person's words). The answer is an `accept_check_verdict` entry naming
    /// the version, the review, the option and the answer's reply. With no such card it is
    /// `Skipped` (`NoVerdictToAccept`). It holds the landing turn, as a landing does, so no
    /// landing moves the card between the read and the answer.
    fn accept_check_verdict(&mut self) {
        let gate = self.link.gate.clone();
        let _turn = gate.landing_turn();
        let mut e = self.entry("agent:relay", "pending");
        e.reply = Some(json!({"via": "rdy_pending"}));
        let mut found: Option<(String, String, String)> = None;
        let res = self.link.exec(
            e,
            "pending",
            "rdy_pending",
            json!({"session_ref": RELAY_RUN}),
            |_, v| {
                found = v["items"].as_array().into_iter().flatten().find_map(|i| {
                    let option = i["options"]
                        .as_array()?
                        .iter()
                        .filter_map(|o| o["id"].as_str())
                        .find(|id| id.starts_with(ACCEPT_PREFIX))?;
                    Some((
                        i["ref"].as_str()?.to_string(),
                        option.to_string(),
                        i["card_hash"].as_str()?.to_string(),
                    ))
                });
            },
        );
        if matches!(res, Res::Lost) {
            return;
        }
        let mut e = self.entry("agent:relay", "accept_check_verdict");
        let Some((pref, option, hash)) = found else {
            e.outcome = Outcome::Skipped;
            e.skip = Some(Skip::NoVerdictToAccept);
            self.link.journal.record(e);
            return;
        };
        e.block = Some(pref.clone());
        let review = option
            .strip_prefix(ACCEPT_PREFIX)
            .and_then(|rest| rest.split(':').next())
            .map(str::to_string);
        e.check = Some(CheckFacts {
            review,
            answer: Some(option.clone()),
            version: Some(pref.clone()),
            ..CheckFacts::default()
        });
        let args = json!({"ref": pref, "option": option, "card_hash": hash,
            "evidence": format!("Hold {pref} on this verdict."),
            "question": format!("Accept the check verdict on {pref} as blocking?"),
            "session_ref": RELAY_RUN});
        self.link
            .exec(e, "accept_check_verdict", "rdy_answer", args, |en, v| {
                en.reply = Some(v.clone());
            });
    }

    fn inbox_ack(&mut self) {
        let e = self.entry("agent:relay", "inbox_ack");
        self.link.exec(
            e,
            "inbox_ack",
            "rdy_inbox",
            json!({"person": true, "ack": true}),
            |en, v| {
                let entries = v["entries"].as_array().cloned().unwrap_or_default();
                en.seen = inbox_labels(&entries);
            },
        );
    }

    // ---- the working copy (the command line, FW-V2) ----

    /// A command of the working-copy flow, journaled as `act` by the human.
    fn wc(
        &mut self,
        act: &'static str,
        args: &[&str],
        fill: impl FnOnce(&mut Entry, &Value),
    ) -> Res {
        let e = self.entry(HUMAN, act);
        let res = self.cli(act, args);
        self.link.finish(e, act, res, fill)
    }

    /// Sets the body of block `id` in the working copy's file `rel`.
    fn edit_file(&self, rel: &str, id: &str, body: &str) {
        let path = self.link.at.dir.join(rel);
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            self.link
                .fail("wc_edit", &format!("cannot read {rel}: {e}"))
        });
        let mut list: Vec<serde_yaml_ng::Value> =
            serde_yaml_ng::from_str(&text).unwrap_or_else(|e| {
                self.link
                    .fail("wc_edit", &format!("cannot parse {rel}: {e}"))
            });
        let entry = list
            .iter_mut()
            .find(|x| x["id"].as_str() == Some(id))
            .unwrap_or_else(|| self.link.fail("wc_edit", &format!("{id} is not in {rel}")));
        entry
            .as_mapping_mut()
            .expect("an entry is a mapping")
            .insert("body".into(), serde_yaml_ng::Value::String(body.into()));
        let out = serde_yaml_ng::to_string(&list).expect("a list serializes");
        std::fs::write(&path, out).unwrap_or_else(|e| {
            self.link
                .fail("wc_edit", &format!("cannot write {rel}: {e}"))
        });
    }

    /// Edits one claim in the working copy, stages the file and commits. On `E_WC_STALE` it runs
    /// `rdy update` once and tries the commit once more. Then it answers the open items of
    /// `work`'s change plan (`answer_work_plan`) and proposes `work`.
    fn wc_edit(&mut self) {
        let gate = self.link.gate.clone();
        {
            let _act = gate.act_turn();
            if !self.wc_commit() {
                return;
            }
        }
        self.answer_work_plan();
        // the propose's own call can change a tension (a key change rdy adds)
        let _act = gate.act_turn();
        self.wc_propose();
    }

    /// The person's answers to `work`'s change plan (ruling task-15 F1: the CLI fallback path,
    /// FW-UX1), as an agent answers its own (`agent::answer_plan`): it reads the plan whole with
    /// `rdy plan --view work` (one `plan` entry per page, naming the view), then answers each open
    /// item that gates or warns by its kind (`agent::plan_answer_for`), one
    /// `rdy plan confirm|defer <items> --reason <why> --view work` for each act and reason (the
    /// command takes one of each), one `plan_answer` entry each, whose reply lists its answers
    /// (`{answers: [{item, kind, act, reason}]}`). It holds the plan turn throughout
    /// (`KillGate::plan_turn`). A view whose changes conflict with canon has no plan (the
    /// read's `E_CONFLICT_UNRESOLVED`), and a call the kill cuts ends it: the version proposed
    /// next is not ready, and the person's approve re-queues it.
    fn answer_work_plan(&mut self) {
        let gate = self.link.gate.clone();
        let _turn = gate.plan_turn();
        let all = agent::PLAN_ALL.to_string();
        let mut page: Option<String> = None;
        let mut open: Vec<(String, String)> = vec![];
        loop {
            let mut args = vec!["plan", "--max-tokens", all.as_str()];
            match &page {
                Some(next) => args.extend(["--page", next.as_str()]),
                None => args.extend(["--view", "work"]),
            }
            let mut read = None;
            let res = self.cli("plan", &args);
            let mut e = self.entry(HUMAN, "plan");
            e.view = Some("work".to_string());
            self.link.finish(e, "plan", res, |en, v| {
                en.reply = Some(json!({"counts": v["counts"]}));
                read = Some(v.clone());
            });
            let Some(read) = read else { return };
            for i in read["items"].as_array().into_iter().flatten() {
                if i["state"] == "open"
                    && let (Some(id), Some(kind)) = (i["id"].as_str(), i["kind"].as_str())
                {
                    open.push((id.to_string(), kind.to_string()));
                }
            }
            match read["next"].as_str() {
                Some(next) => page = Some(next.to_string()),
                None => break,
            }
        }
        // one command for each act and reason, in a fixed order
        let mut groups: BTreeMap<(&str, &str), Vec<(String, String)>> = BTreeMap::new();
        for (item, kind) in open {
            if let Some(answer) = agent::plan_answer_for(&kind) {
                groups.entry(answer).or_default().push((item, kind));
            }
        }
        for ((act, reason), items) in groups {
            let mut args = vec!["plan", act];
            args.extend(items.iter().map(|(item, _)| item.as_str()));
            args.extend(["--reason", reason, "--view", "work"]);
            let sent: Vec<Value> = items
                .iter()
                .map(|(item, kind)| json!({"item": item, "kind": kind, "act": act, "reason": reason}))
                .collect();
            let mut e = self.entry(HUMAN, "plan_answer");
            e.view = Some("work".to_string());
            // the entry names what it sent, refused or not
            e.reply = Some(json!({ "answers": sent }));
            let res = self.cli("plan_answer", &args);
            if !matches!(
                self.link.finish(e, "plan_answer", res, |_, _| {}),
                Res::Ok(_)
            ) {
                return;
            }
        }
    }

    /// The edit and the commit of `wc_edit`: `true` when the commit was made.
    fn wc_commit(&mut self) -> bool {
        let step = self.link.step;
        let area = step % DOC_PATHS.len();
        let claim = self.seed.claims
            [area * CLAIMS_PER_AREA + (step / DOC_PATHS.len()) % CLAIMS_PER_AREA]
            .clone();
        let rel = format!("{}.yaml", DOC_PATHS[area]);
        let body = format!("The person sets this rule in round {}.", spell(step));
        let decision = self.seed.decisions[area].clone();
        let message = format!("person edit {step}");
        if !self.checked_out {
            match self.wc("wc_checkout", &["checkout", "work"], |_, _| {}) {
                Res::Ok(_) => self.checked_out = true,
                _ => return false,
            }
        }
        for attempt in 0..2 {
            self.edit_file(&rel, &claim, &body);
            if !self.stage(&rel) {
                return false;
            }
            let res = self.wc(
                "wc_commit",
                &["commit", "-m", &message, "--implements", &decision],
                |en, v| {
                    en.change = v["change"].as_str().map(str::to_string);
                    en.block = Some(claim.clone());
                    en.view = Some("work".to_string());
                },
            );
            match res {
                Res::Ok(_) => return true,
                Res::Refused(code, _) if code == "E_WC_STALE" && attempt == 0 => {
                    // the view moved under the checkout: update, take the store's version of a
                    // file that changed on both sides, and make the edit again
                    match self.wc("wc_update", &["update"], |_, _| {}) {
                        Res::Ok(v) => self.take_theirs(&v),
                        _ => return false,
                    }
                }
                _ => return false,
            }
        }
        false
    }

    /// `rdy propose --view work`, with a summary; the version joins the person's queue.
    fn wc_propose(&mut self) {
        let step = self.link.step;
        let title = format!("person edit {step}");
        let mut made = None;
        let res = self.wc(
            "wc_propose",
            &[
                "propose",
                "--view",
                "work",
                "--title",
                &title,
                "--summary",
                &title,
            ],
            |en, v| {
                en.view = Some("work".to_string());
                en.block = v["ref"].as_str().map(str::to_string);
                en.seen = labels_of(&v["tensions"]);
                made = en.block.clone();
            },
        );
        if let (Res::Ok(_), Some(r)) = (res, made) {
            self.queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(r);
        }
    }

    /// `rdy add <file>`. A refusal the table allows ends the edit and is journaled as the
    /// commit's; any other fails the run.
    fn stage(&mut self, rel: &str) -> bool {
        let res = self.cli("wc_commit", &["add", rel]);
        if matches!(res, Res::Ok(_)) {
            return true;
        }
        let e = self.entry(HUMAN, "wc_commit");
        self.link.finish(e, "wc_commit", res, |_, _| {});
        false
    }

    /// After `rdy update`: for each file it reports as changed on both sides, the store's
    /// version (`<name>.theirs.yaml`) replaces the file.
    fn take_theirs(&self, update: &Value) {
        for c in update["conflicts"].as_array().into_iter().flatten() {
            let Some(path) = c["path"].as_str() else {
                continue;
            };
            let dir = &self.link.at.dir;
            let theirs = dir.join(path.replace(".yaml", ".theirs.yaml"));
            if theirs.exists() {
                std::fs::rename(&theirs, dir.join(path)).unwrap_or_else(|e| {
                    self.link.fail(
                        "wc_update",
                        &format!("cannot take {path}'s store version: {e}"),
                    )
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::agent;
    use crate::swarm::project;
    use crate::swarm::script::{Act, Config, Pick};

    /// A journal entry `act` of `actor`, `Ok`, in `view`, naming `block`.
    fn noted(actor: &str, act: &'static str, view: &str, block: &str) -> Entry {
        let mut e = Entry::new(actor, 0, act, Outcome::Ok);
        e.view = Some(view.to_string());
        e.block = Some(block.to_string());
        e
    }

    /// Fix round 2 (ruling task-5-round1.2): a queued proposal is dropped only when its propose
    /// entry comes before an `alt_create` entry that names its view, as the set's maker (`view`)
    /// or as the other member (`block`). A proposal made after the set, a proposal of a view in
    /// no set, and a set refused are no reason to drop it.
    #[test]
    fn sg5_a_proposal_is_dropped_only_when_made_before_its_views_set() {
        let set =
            |maker: &str, other: &str| noted(&format!("agent:{maker}"), "alt_create", maker, other);
        let before = noted("agent:a1", "propose", "a1", "P-before@1");
        let after = noted("agent:a1", "propose", "a1", "P-after@1");
        let elsewhere = noted("agent:a5", "propose", "a5", "P-a5@1");
        let mut refused = set("a5", "a6");
        refused.outcome = Outcome::Refused("E_ALT_MEMBER".to_string());
        let j = vec![before.clone(), elsewhere, refused, set("a0", "a1"), after];
        assert!(proposed_before_set(&j, "P-before@1"));
        assert!(!proposed_before_set(&j, "P-after@1"));
        assert!(!proposed_before_set(&j, "P-a5@1"), "a5's set was refused");
        assert!(!proposed_before_set(&j, "P-unknown@1"), "no propose entry");
        // the set's maker is the view too
        let j = vec![before, set("a1", "a3")];
        assert!(proposed_before_set(&j, "P-before@1"));
    }

    /// Fix round 2 (ruling task-5-round1.2): the person journals each proposal it drops as a
    /// skipped `land` (`ProposedBeforeSet`) that names the proposal, and lands the next one.
    #[test]
    fn sg5_the_person_journals_each_proposal_it_drops() {
        let cfg = Config {
            seed: 32,
            agents: 1,
            steps: 2,
            kill_at: None,
        };
        let p = project::build(&cfg);
        let (journal, gate) = (Journal::new(), Arc::new(KillGate::new()));
        journal.record(noted("agent:a1", "propose", "a1", "P-before@1"));
        journal.record(noted("agent:a0", "alt_create", "a0", "a1"));
        let link = Link::connect(
            Where::of(&p),
            "agent:relay",
            "chat",
            "reviewer",
            None,
            journal.clone(),
            gate.clone(),
        );
        let queue = Queue::default();
        queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push("P-before@1".to_string());
        let mut person = Person {
            link,
            seed: p.seed.clone(),
            queue: queue.clone(),
            checked_out: false,
            hooks: Hooks::default(),
            seed_landings: p.seed_landings.clone(),
            disputes: Vec::new(),
        };
        person.land(true);
        let lands: Vec<Entry> = journal
            .entries()
            .into_iter()
            .filter(|e| e.act == "land")
            .collect();
        let dropped = lands
            .iter()
            .find(|e| e.skip == Some(Skip::ProposedBeforeSet))
            .unwrap_or_else(|| panic!("no drop journaled: {lands:#?}"));
        assert_eq!(dropped.outcome, Outcome::Skipped);
        assert_eq!(dropped.block.as_deref(), Some("P-before@1"));
        assert!(
            queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
        );
    }

    /// Fix round 1, finding 4: a landing whose reply the kill cut is still audited. The relay
    /// reads whether the proposal landed; when it did, the reads after a landing run on what was
    /// read before it, and a `land_found` entry names the landing, the view and its base.
    #[test]
    fn sg5_a_landing_whose_reply_was_lost_is_found_and_audited() {
        let cfg = Config {
            seed: 31,
            agents: 1,
            steps: 2,
            kill_at: None,
        };
        let p = project::build(&cfg);
        let (journal, gate) = (Journal::new(), Arc::new(KillGate::new()));
        let write = Act::Write {
            target: Pick::Seed(0),
            stable: true,
        };
        let report = agent::spawn(
            &p,
            &cfg,
            0,
            vec![write, Act::Propose],
            journal.clone(),
            gate.clone(),
        )
        .join()
        .expect("the agent runs");
        let pref = report.proposals[0].clone();
        let watch: Watch = Arc::default();
        let link = Link::connect(
            Where::of(&p),
            "agent:relay",
            "chat",
            "reviewer",
            None,
            journal.clone(),
            gate.clone(),
        );
        let hooks = Hooks {
            pace: None,
            watch: Some(watch.clone()),
            done: None,
        };
        let mut person = Person {
            link,
            seed: p.seed.clone(),
            queue: Queue::default(),
            checked_out: false,
            hooks,
            seed_landings: p.seed_landings.clone(),
            disputes: Vec::new(),
        };
        // not landed yet: nothing is found
        let before = run::before_land(&mut person.link, &pref).expect("the reads before");
        assert!(!person.found(&pref, before, false));
        // landed by an approve whose reply nobody read
        let before = run::before_land(&mut person.link, &pref).expect("the reads before");
        let (ok, landed) = p.served.json(&["--json", "approve", &pref]);
        assert!(ok, "{landed}");
        assert!(person.found(&pref, before, false));
        let found: Vec<Entry> = journal
            .entries()
            .into_iter()
            .filter(|e| e.act == "land_found")
            .collect();
        assert_eq!(found.len(), 2, "{found:#?}");
        let last = &found[1];
        assert_eq!(last.block.as_deref(), Some(pref.as_str()));
        assert_eq!(last.view.as_deref(), Some("a0"));
        assert!(last.base.is_some(), "{last:?}");
        assert_eq!(
            last.reply.as_ref().map(|r| &r["landing"]),
            Some(&landed["landing"]),
            "{last:?}"
        );
        assert!(found[0].reply.is_none(), "{:?}", found[0]);
        // seed 11: a later approve of a version that landed with that landing (E_ALREADY_LANDED)
        // finds it again; the landing is named once, by the entry whose base was read before it
        let again = run::before_land(&mut person.link, &pref).expect("the reads before");
        assert!(person.found(&pref, again, true));
        let found: Vec<Entry> = journal
            .entries()
            .into_iter()
            .filter(|e| e.act == "land_found")
            .collect();
        assert_eq!(found.len(), 3, "{found:#?}");
        assert!(found[2].reply.is_none(), "{:?}", found[2]);
    }

    /// Adversarial review: the kill cuts the reads after a landing. The approve landed; the kill
    /// comes before the relay reads whether it landed. The relay reads again on the restarted
    /// daemon, so the landing is found with the base read before it, and the reads after it run
    /// (an O2 check is never left out in silence).
    #[test]
    fn sg5_a_landing_found_after_a_kill_cut_its_reads_is_still_audited() {
        let cfg = Config {
            seed: 33,
            agents: 1,
            steps: 2,
            kill_at: None,
        };
        let p = project::build(&cfg);
        let (journal, gate) = (Journal::new(), Arc::new(KillGate::with_kill()));
        let write = Act::Write {
            target: Pick::Seed(0),
            stable: true,
        };
        let report = agent::spawn(
            &p,
            &cfg,
            0,
            vec![write, Act::Propose],
            journal.clone(),
            gate.clone(),
        )
        .join()
        .expect("the agent runs");
        let pref = report.proposals[0].clone();
        let watch: Watch = Arc::default();
        let link = Link::connect(
            Where::of(&p),
            "agent:relay",
            "chat",
            "reviewer",
            None,
            journal.clone(),
            gate.clone(),
        );
        let hooks = Hooks {
            pace: None,
            watch: Some(watch.clone()),
            done: None,
        };
        let mut person = Person {
            link,
            seed: p.seed.clone(),
            queue: Queue::default(),
            checked_out: false,
            hooks,
            seed_landings: p.seed_landings.clone(),
            disputes: Vec::new(),
        };
        let before = run::before_land(&mut person.link, &pref).expect("the reads before");
        let (ok, landed) = p.served.json(&["--json", "approve", &pref]);
        assert!(ok, "{landed}");
        // the kill, and the run's restart: the relay's next call is lost, then it reconnects
        gate.killing();
        p.served.kill9();
        gate.restarted();
        assert!(
            person.found(&pref, before, false),
            "{:#?}",
            journal.entries()
        );
        let entries = journal.entries();
        let named: Vec<&Entry> = entries
            .iter()
            .filter(|e| e.act == "land_found" && e.reply.is_some())
            .collect();
        assert_eq!(named.len(), 1, "{entries:#?}");
        assert_eq!(
            named[0].reply.as_ref().map(|r| &r["landing"]),
            Some(&landed["landing"]),
            "{entries:#?}"
        );
        assert!(named[0].base.is_some(), "{:?}", named[0]);
        // the reads after the landing ran on the restarted daemon
        let at = entries
            .iter()
            .position(|e| e.act == "land_found" && e.reply.is_some())
            .expect("the entry");
        assert!(
            entries[at..]
                .iter()
                .any(|e| e.act == "tensions" && e.outcome == Outcome::Ok),
            "{entries:#?}"
        );
    }

    /// Adversarial review: `land_found` after `E_ALREADY_LANDED` read its `before` after the
    /// landing. When no entry names that landing yet, its audit would have no base read before it,
    /// so the run fails instead of auditing it on the wrong base.
    #[test]
    #[should_panic(expected = "which no entry names")]
    fn sg5_a_landing_first_found_after_it_landed_fails_the_run() {
        let cfg = Config {
            seed: 34,
            agents: 1,
            steps: 2,
            kill_at: None,
        };
        let p = project::build(&cfg);
        let (journal, gate) = (Journal::new(), Arc::new(KillGate::new()));
        let write = Act::Write {
            target: Pick::Seed(0),
            stable: true,
        };
        let report = agent::spawn(
            &p,
            &cfg,
            0,
            vec![write, Act::Propose],
            journal.clone(),
            gate.clone(),
        )
        .join()
        .expect("the agent runs");
        let pref = report.proposals[0].clone();
        let link = Link::connect(
            Where::of(&p),
            "agent:relay",
            "chat",
            "reviewer",
            None,
            journal.clone(),
            gate.clone(),
        );
        let hooks = Hooks {
            pace: None,
            watch: Some(Arc::default()),
            done: None,
        };
        let mut person = Person {
            link,
            seed: p.seed.clone(),
            queue: Queue::default(),
            checked_out: false,
            hooks,
            seed_landings: p.seed_landings.clone(),
            disputes: Vec::new(),
        };
        let (ok, landed) = p.served.json(&["--json", "approve", &pref]);
        assert!(ok, "{landed}");
        let after = run::before_land(&mut person.link, &pref).expect("the reads");
        person.found(&pref, after, true);
    }
}
