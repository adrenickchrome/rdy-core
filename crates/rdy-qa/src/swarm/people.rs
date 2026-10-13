//! The simulated person. The person acts through a relaying agent session (`agent:relay`, run
//! `chat`, role `reviewer`) for every act that has a tool (FW-UX1, D-KQ4CA; F5, D-VF4RH). The
//! command line is the fallback, and only for this closed list:
//!
//! - `rdy approve`, relayed (`--relayed-by agent:relay/chat`, the path FW-B9 gives; no MCP tool
//!   approves in M2) and, about one landing in five, direct (CLI parity);
//! - the working copy: `rdy checkout`, `rdy add`, `rdy commit`, `rdy update`, and `rdy propose`
//!   for the human's view `work`, which no agent may act in.
//!
//! The person lands the proposal at the front of a shared queue (the run orders it). A proposal
//! made before its view joined an alternative set it drops, and journals the drop as a skipped
//! `land` (`Skip::ProposedBeforeSet`) that names the proposal. Journal entries of the
//! relay carry the actor `agent:relay`; those of a direct command carry `human:qa`, the human
//! the project's user config names.
use super::agent::{KINDS, KillGate, Link, Pace, Res, Where, inbox_labels, labels_of, spell};
use super::journal::{Entry, Journal, Outcome, Skip};
use super::project::{CLAIMS_PER_AREA, DOC_PATHS, SeedBlocks, SwarmProject};
use super::run::{self, Watch};
use super::script::PersonAct;
use crate::daemon::human_command;
use crate::fixtures::output_within;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

/// A command's hang guard, not a speed check.
const CLI_BUDGET: Duration = Duration::from_secs(120);
/// The actor a direct command journals as (the user config `Served` writes names it).
const HUMAN: &str = "human:qa";
/// The relay as `rdy approve --relayed-by` names it.
const RELAY: &str = "agent:relay/chat";
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
    std::thread::spawn(move || {
        let link = Link::connect(at, "agent:relay", "chat", "reviewer", None, journal, gate);
        let mut person = Person {
            link,
            seed,
            queue,
            checked_out: false,
            hooks,
        };
        for (step, act) in acts.iter().enumerate() {
            if let Some(pace) = &person.hooks.pace {
                pace(step);
            }
            person.link.step = step;
            person.act(act);
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

impl Person {
    fn act(&mut self, act: &PersonAct) {
        match act {
            PersonAct::Land { relayed } => self.land(*relayed),
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
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let mut cmd = human_command(&self.link.at.dir, &self.link.at.env, &all);
        let out = match output_within(&mut cmd, CLI_BUDGET) {
            Ok(o) => o,
            Err(e) => self.link.fail(act, &format!("hung: {e}")),
        };
        if out.status.success() {
            return Res::Ok(json_of(&String::from_utf8_lossy(&out.stdout)));
        }
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        let v = json_of(&err);
        match v["code"].as_str() {
            Some(code @ ("E_DAEMON_GONE" | "E_DAEMON_START")) => self.link.lost(act, code),
            Some(code) => Res::Refused(
                code.to_string(),
                v["message"].as_str().unwrap_or_default().to_string(),
            ),
            None => self.link.fail(act, &format!("failed: {}", err.trim())),
        }
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
        let res = self.cli("land", &args);
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
            (Some(b), Res::Lost) => self.found(&pref, b),
            (Some(b), Res::Refused(code, _)) if code == "E_ALREADY_LANDED" => self.found(&pref, b),
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

    /// Whether proposal `pref` landed, as the relay reads it (`rdy_read`), journaled as a
    /// `land_found` entry that names the proposal, the view and the base `before` read. When it
    /// landed with a landing no entry names yet, the entry's reply names the landing, and with the
    /// run's hooks the reads after a landing run on `before`: a landing whose reply the kill cut
    /// is audited too. A landing an entry names already (a version that landed with a later
    /// version's landing, `E_ALREADY_LANDED`) is not named again: its base is the one read
    /// before it.
    fn found(&mut self, pref: &str, before: run::Before) -> bool {
        let mut e = self.entry("agent:relay", run::LAND_FOUND);
        e.block = Some(pref.to_string());
        e.view = Some(before.view.clone());
        e.base.clone_from(&before.base);
        let res = self
            .link
            .raw(run::LAND_FOUND, "rdy_read", json!({"id": pref}));
        let landing = match &res {
            Res::Ok(v) if v["proposal"]["state"] == "landed" => {
                v["proposal"]["landing"].as_str().map(str::to_string)
            }
            _ => None,
        };
        let named = landing.as_deref().is_some_and(|l| {
            self.link.journal.entries().iter().any(|x| {
                ((x.act == "land" && x.outcome == Outcome::Ok) || x.act == run::LAND_FOUND)
                    && x.reply.as_ref().is_some_and(|r| r["landing"] == l)
            })
        });
        self.link.finish(e, run::LAND_FOUND, res, |en, _| {
            if let (Some(l), false) = (&landing, named) {
                en.reply = Some(json!({"landing": l}));
            }
        });
        let Some(l) = landing else { return false };
        if !named && let Some(w) = self.hooks.watch.clone() {
            run::after_land(&mut self.link, &w, before, &json!({"landing": l}));
        }
        true
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
                .exec(e, "read", "rdy_read", json!({"id": pref}), |_, v| {
                    let p = &v["proposal"];
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
    /// `rdy update` once and tries the commit once more.
    fn wc_edit(&mut self) {
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
                _ => return,
            }
        }
        for attempt in 0..2 {
            self.edit_file(&rel, &claim, &body);
            if !self.stage(&rel) {
                return;
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
                Res::Ok(_) => break,
                Res::Refused(code, _) if code == "E_WC_STALE" && attempt == 0 => {
                    // the view moved under the checkout: update, take the store's version of a
                    // file that changed on both sides, and make the edit again
                    match self.wc("wc_update", &["update"], |_, _| {}) {
                        Res::Ok(v) => self.take_theirs(&v),
                        _ => return,
                    }
                }
                _ => return,
            }
        }
        let title = format!("person edit {step}");
        let mut made = None;
        let res = self.wc(
            "wc_propose",
            &["propose", "--view", "work", "--title", &title],
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
        };
        // not landed yet: nothing is found
        let before = run::before_land(&mut person.link, &pref).expect("the reads before");
        assert!(!person.found(&pref, before));
        // landed by an approve whose reply nobody read
        let before = run::before_land(&mut person.link, &pref).expect("the reads before");
        let (ok, landed) = p.served.json(&["--json", "approve", &pref]);
        assert!(ok, "{landed}");
        assert!(person.found(&pref, before));
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
        assert!(person.found(&pref, again));
        let found: Vec<Entry> = journal
            .entries()
            .into_iter()
            .filter(|e| e.act == "land_found")
            .collect();
        assert_eq!(found.len(), 3, "{found:#?}");
        assert!(found[2].reply.is_none(), "{:?}", found[2]);
    }
}
