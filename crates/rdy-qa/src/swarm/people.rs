//! The simulated person. The person acts through a relaying agent session (`agent:relay`, run
//! `chat`, role `reviewer`) for every act that has a tool (FW-UX1, D-KQ4CA; F5, D-VF4RH). The
//! command line is the fallback, and only for this closed list:
//!
//! - `rdy approve`, relayed (`--relayed-by agent:relay/chat`, the path FW-B9 gives; no MCP tool
//!   approves in M2) and, about one landing in five, direct (CLI parity);
//! - the working copy: `rdy checkout`, `rdy add`, `rdy commit`, `rdy update`, and `rdy propose`
//!   for the human's view `work`, which no agent may act in.
//!
//! The person lands the oldest unlanded proposal of a shared queue. Journal entries of the
//! relay carry the actor `agent:relay`; those of a direct command carry `human:qa`, the human
//! the project's user config names.
use super::agent::{KillGate, Link, Res, Where, inbox_labels, labels_of, spell};
use super::journal::{Entry, Journal, Outcome};
use super::project::{CLAIMS_PER_AREA, DOC_PATHS, SeedBlocks, SwarmProject};
use super::script::PersonAct;
use crate::daemon::human_command;
use crate::fixtures::output_within;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

/// A command's hang guard, not a speed check.
const CLI_BUDGET: Duration = Duration::from_secs(120);
/// The actor a direct command journals as (the user config `Served` writes names it).
const HUMAN: &str = "human:qa";
/// The relay as `rdy approve --relayed-by` names it.
const RELAY: &str = "agent:relay/chat";
/// How many queued proposals the person looks at when it hunts for advice to accept.
const ADVICE_LOOKS: usize = 3;

/// The proposals waiting for the person to land, oldest first.
pub type Queue = Arc<Mutex<Vec<String>>>;

/// Starts the person: one thread, one relay session. It runs `acts` and returns.
pub fn spawn(
    p: &SwarmProject,
    acts: Vec<PersonAct>,
    journal: Journal,
    queue: Queue,
    gate: Arc<KillGate>,
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
        };
        for (step, act) in acts.iter().enumerate() {
            person.link.step = step;
            person.act(act);
        }
    })
}

struct Person {
    link: Link,
    seed: SeedBlocks,
    queue: Queue,
    /// Whether `rdy checkout work` has run.
    checked_out: bool,
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
            Some("E_DAEMON_GONE" | "E_DAEMON_START") => Res::Lost,
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
        let next = {
            let mut q = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
            (!q.is_empty()).then(|| q.remove(0))
        };
        let Some(pref) = next else {
            e.outcome = Outcome::Skipped;
            self.link.journal.record(e);
            return;
        };
        e.block = Some(pref.clone());
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
        let mut q = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        match &res {
            Res::Ok(_) => {}
            // a newer version took its place, or it landed already: the ref is dead
            Res::Refused(code, _) if code == "E_PROPOSAL_MOVED" || code == "E_ALREADY_LANDED" => {}
            // the gate refused it for now: it waits behind the others
            Res::Refused(..) => q.push(pref),
            // the kill may or may not have landed it: it goes first, and `E_ALREADY_LANDED`
            // answers if it did
            Res::Lost => q.insert(0, pref),
        }
    }

    // ---- through the relay ----

    fn tensions(&mut self) -> Option<Value> {
        let e = self.entry("agent:relay", "tensions");
        let res = self.link.exec(
            e,
            "tensions",
            "rdy_tensions",
            json!({"max_tokens": 100_000}),
            |en, v| en.seen = labels_of(&v["tensions"]),
        );
        let Res::Ok(v) = res else { return None };
        if v.get("omitted").is_some() {
            self.link.fail("tensions", "got a reply cut for its budget");
        }
        Some(v)
    }

    fn dismiss(&mut self) {
        // the relay lists the blocking tensions and dismisses the first with a change side of a
        // writer agent: it is a reviewer of another actor (D-DAFWP)
        let e = self.entry("agent:relay", "tensions");
        let res = self.link.exec(
            e,
            "tensions",
            "rdy_tensions",
            json!({"severity": "blocking", "max_tokens": 100_000}),
            |en, v| en.seen = labels_of(&v["tensions"]),
        );
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
