//! sg4: the scripted agents act over MCP, the person acts through its relaying agent and the
//! command line, and every call is journaled. Each test builds a project, runs short scripts
//! and checks the journal and the store. The kill and the restart belong to the run (sg5).
use rdy_qa::mcp::McpClient;
use rdy_qa::swarm::agent::{self, AgentReport, KillGate};
use rdy_qa::swarm::journal::{Entry, Journal, Outcome, Reply, Skip};
use rdy_qa::swarm::m3::{self, CallHook, LAND_CARD, M3_FIXTURE, M3Hooks};
use rdy_qa::swarm::people;
use rdy_qa::swarm::project::{self, SwarmProject};
use rdy_qa::swarm::script::{
    Act, Config, M3_MAX_CALLS, PersonAct, Pick, Route, Scenario, UiStep, script,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SEED: u64 = 4;

fn cfg(agents: usize) -> Config {
    Config {
        seed: SEED,
        agents,
        steps: 12,
        kill_at: None,
    }
}

/// A project, an empty journal and a gate that never opens.
fn rig(agents: usize) -> (SwarmProject, Config, Journal, Arc<KillGate>) {
    let cfg = cfg(agents);
    (
        project::build(&cfg),
        cfg,
        Journal::new(),
        Arc::new(KillGate::new()),
    )
}

/// Runs agent `i` with `acts` to the end.
fn run(
    p: &SwarmProject,
    cfg: &Config,
    i: usize,
    acts: Vec<Act>,
    j: &Journal,
    g: &Arc<KillGate>,
) -> AgentReport {
    agent::spawn(p, cfg, i, acts, j.clone(), g.clone())
        .join()
        .unwrap_or_else(|e| panic!("agent {i} panicked: {e:?}"))
}

/// Runs the person's `acts` to the end over `queue`.
fn run_person(
    p: &SwarmProject,
    acts: Vec<PersonAct>,
    j: &Journal,
    queue: &Arc<Mutex<Vec<String>>>,
    g: &Arc<KillGate>,
) {
    people::spawn(p, acts, j.clone(), queue.clone(), g.clone())
        .join()
        .unwrap_or_else(|e| panic!("the person panicked: {e:?}"));
}

fn of(j: &Journal, actor: &str, act: &str) -> Vec<Entry> {
    j.entries()
        .into_iter()
        .filter(|e| e.actor == actor && e.act == act)
        .collect()
}

/// The person's relay: a session `agent:relay`, run `chat`, role `reviewer`.
fn relay(p: &SwarmProject) -> McpClient {
    let mut c = McpClient::start_proxy(&p.served);
    c.call(
        "rdy_session",
        json!({"actor": "agent:relay", "run": "chat", "role": "reviewer"}),
    )
    .ok();
    c
}

fn write_script(n: usize) -> Vec<Act> {
    let mut acts = vec![Act::Brief {
        target: Pick::Seed(0),
    }];
    acts.extend((0..n).map(|_| Act::Write {
        target: Pick::Seed(0),
        stable: true,
    }));
    acts
}

#[test]
fn sg4_an_agent_briefs_before_every_stable_write() {
    let (p, cfg, j, g) = rig(2);
    run(&p, &cfg, 0, write_script(3), &j, &g);
    let writes: Vec<Entry> = j
        .entries()
        .into_iter()
        .filter(|e| e.stable && e.outcome == Outcome::Ok)
        .collect();
    assert_eq!(writes.len(), 3, "{:#?}", j.entries());
    for e in &writes {
        assert!(
            e.brief.is_some(),
            "step {} wrote with no brief: {e:?}",
            e.step
        );
        assert!(e.change.is_some(), "{e:?}");
    }
    // each write had a briefing of its own just before it
    assert_eq!(of(&j, "agent:a0", "brief").len(), 4);
}

#[test]
fn sg4_a_stable_write_without_a_brief_is_refused() {
    let (p, cfg, j, g) = rig(2);
    run(
        &p,
        &cfg,
        0,
        vec![Act::WriteNoBrief {
            target: Pick::Seed(0),
        }],
        &j,
        &g,
    );
    let e = of(&j, "agent:a0", "write_no_brief");
    assert_eq!(e.len(), 1, "{:#?}", j.entries());
    assert_eq!(e[0].outcome, Outcome::Refused("E_BRIEF_REQUIRED".into()));
    assert!(e[0].brief.is_none());
}

#[test]
fn sg4_a_write_with_a_token_the_agent_itself_made_stale_is_refused() {
    let (p, cfg, j, g) = rig(2);
    run(
        &p,
        &cfg,
        0,
        vec![Act::WriteStale {
            target: Pick::Seed(0),
        }],
        &j,
        &g,
    );
    let e = of(&j, "agent:a0", "write_stale");
    assert_eq!(e.len(), 2, "{:#?}", j.entries());
    assert_eq!(e[0].outcome, Outcome::Ok);
    assert_eq!(e[1].outcome, Outcome::Refused("E_BRIEF_STALE".into()));
    assert_eq!(
        e[0].brief, e[1].brief,
        "the second write cites the first token"
    );
}

#[test]
fn sg4_two_agents_collide_and_both_writes_are_acknowledged() {
    let (p, cfg, j, g) = rig(2);
    let hs: Vec<_> = (0..2)
        .map(|i| {
            agent::spawn(
                &p,
                &cfg,
                i,
                vec![Act::Collide {
                    with: 1 - i,
                    target: Pick::Seed(0),
                }],
                j.clone(),
                g.clone(),
            )
        })
        .collect();
    for h in hs {
        h.join()
            .unwrap_or_else(|e| panic!("an agent panicked: {e:?}"));
    }
    let collides: Vec<Entry> = j
        .entries()
        .into_iter()
        .filter(|e| e.act == "collide")
        .collect();
    assert_eq!(collides.len(), 2, "{:#?}", j.entries());
    for e in &collides {
        assert_eq!(e.outcome, Outcome::Ok, "{e:?}");
        assert_eq!(e.block.as_deref(), Some(p.seed.claims[0].as_str()));
        assert!(
            e.base.is_some(),
            "a collide names the version it wrote from"
        );
    }
    assert_eq!(collides[0].base, collides[1].base, "one base");
    let mut changes: Vec<String> = collides.iter().filter_map(|e| e.change.clone()).collect();
    changes.sort();
    // the relay reads the tensions: one edit collision, whose sides are the two changes
    let mut r = relay(&p);
    let t = r
        .call(
            "rdy_tensions",
            json!({"kind": "edit_collision", "max_tokens": 100_000}),
        )
        .ok();
    let rows = t["tensions"].as_array().expect("tensions");
    assert_eq!(rows.len(), 1, "{t}");
    let mut sides: Vec<String> = rows[0]["sides"]
        .as_array()
        .expect("sides")
        .iter()
        .filter_map(|s| s["change"].as_str().map(str::to_string))
        .collect();
    sides.sort();
    assert_eq!(sides, changes, "{t}");
    // the later writer's reply listed it
    let queue = Arc::new(Mutex::new(Vec::new()));
    run_person(&p, vec![PersonAct::Tensions], &j, &queue, &g);
    let label = rows[0]["id"].as_str().expect("a label").to_string();
    let seen: Vec<Vec<String>> = collides.iter().map(|e| e.seen.clone()).collect();
    assert!(
        seen.iter().any(|s| s.contains(&label)),
        "{seen:?} lacks {label}"
    );
    // the person's tensions read is canon's, one kind at a time (ruling task-5.5, so the read
    // keeps inside its budget in the deep tier): the in-flight collision is not in it
    let read = &of(&j, "agent:relay", "tensions")[0];
    assert_eq!(read.outcome, Outcome::Ok, "{read:?}");
    assert!(!read.seen.contains(&label), "{read:?}");
}

/// Agents 0 and 1 update seed claim 0 from one base, and the person lands agent 0's proposal:
/// the claim is conflicted in agent 1's view.
fn conflicted_in_a1() -> (SwarmProject, Config, Journal, Arc<KillGate>) {
    let (p, cfg, j, g) = rig(2);
    let collide = |with| Act::Collide {
        with,
        target: Pick::Seed(0),
    };
    let report = run(&p, &cfg, 0, vec![collide(1), Act::Propose], &j, &g);
    run(&p, &cfg, 1, vec![collide(0)], &j, &g);
    let queue = Arc::new(Mutex::new(report.proposals.clone()));
    run_person(&p, vec![PersonAct::Land { relayed: true }], &j, &queue, &g);
    let mut r = relay(&p);
    let shown = r
        .call(
            "rdy_view",
            json!({"op": "show", "view": "a1", "max_tokens": 100_000}),
        )
        .ok();
    let claim = &p.seed.claims[0];
    assert!(
        shown["conflicts"]
            .as_array()
            .is_some_and(|cs| cs.iter().any(|c| c["block"] == claim.as_str())),
        "precondition: {claim} is conflicted in a1: {shown}"
    );
    (p, cfg, j, g)
}

/// Ruling task-5.3 (fast seed 2): the stale-briefing probe aims at a block its view does not hold
/// conflicted, read before the act: its second write names no base, which a conflicted block
/// refuses with `E_BAD_INPUT` before the briefing is looked at.
#[test]
fn sg4_a_stale_briefing_probe_aims_at_a_block_its_view_does_not_hold_conflicted() {
    let (p, cfg, j, g) = conflicted_in_a1();
    run(
        &p,
        &cfg,
        1,
        vec![Act::WriteStale {
            target: Pick::Seed(0),
        }],
        &j,
        &g,
    );
    let e = of(&j, "agent:a1", "write_stale");
    assert_eq!(e.len(), 2, "{:#?}", j.entries());
    assert_ne!(e[0].block.as_deref(), Some(p.seed.claims[0].as_str()));
    assert_eq!(e[0].outcome, Outcome::Ok);
    assert_eq!(e[1].outcome, Outcome::Refused("E_BRIEF_STALE".into()));
}

/// Ruling task-5.3: a write that names no base aims at the first candidate its view does not
/// hold conflicted; when there is none, the act is skipped (`NoUnconflictedTarget`).
#[test]
fn sg4_the_first_unconflicted_target_is_the_pick_or_else_a_seed_block() {
    let ids = ["c-1", "c-2", "c-3"];
    let conflicted: BTreeSet<String> = ["c-1".to_string(), "c-2".to_string()].into();
    assert_eq!(agent::first_unconflicted(&ids, &conflicted), Some(2));
    assert_eq!(agent::first_unconflicted(&ids[..2], &conflicted), None);
    assert_eq!(agent::first_unconflicted(&ids, &BTreeSet::new()), Some(0));
}

#[test]
fn sg4_the_relay_lands_a_proposal_with_a_relayed_approval() {
    let (p, cfg, j, g) = rig(2);
    let report = run(
        &p,
        &cfg,
        0,
        vec![
            Act::Brief {
                target: Pick::Seed(0),
            },
            Act::Write {
                target: Pick::Seed(0),
                stable: true,
            },
            Act::Propose,
        ],
        &j,
        &g,
    );
    assert_eq!(report.proposals.len(), 1, "{:#?}", j.entries());
    let queue = Arc::new(Mutex::new(report.proposals.clone()));
    run_person(&p, vec![PersonAct::Land { relayed: true }], &j, &queue, &g);
    let land = of(&j, "agent:relay", "land");
    assert_eq!(land.len(), 1, "{:#?}", j.entries());
    assert_eq!(land[0].outcome, Outcome::Ok, "{:?}", land[0]);
    // the journal keeps the real `rdy_answer` result; its landing fields sit under `landing`
    let reply = land[0].reply.as_ref().expect("the answer's reply");
    assert_eq!(reply["act"], "approve", "{reply}");
    assert_eq!(reply["assurance"], "relayed", "{reply}");
    let landed = people::landing_fields(reply);
    assert_eq!(landed["assurance"], "relayed", "{reply}");
    assert_eq!(landed["approved"], json!(report.proposals[0]), "{reply}");
    assert!(
        queue.lock().unwrap().is_empty(),
        "the landed ref left the queue"
    );
    // the log shows the landing, relayed by the person's agent
    let (ok, log) = p.served.json(&["--json", "log"]);
    assert!(ok, "{log}");
    assert_eq!(log[0]["landing"], landed["landing"], "{log}");
    assert_eq!(log[0]["assurance"], "relayed", "{log}");
    assert_eq!(log[0]["relayed_by"]["actor"], "agent:relay", "{log}");
}

#[test]
fn sg4_the_person_commits_from_the_working_copy_and_an_agent_reads_it() {
    let (p, cfg, j, g) = rig(2);
    let queue = Arc::new(Mutex::new(Vec::new()));
    run_person(
        &p,
        vec![PersonAct::WcEdit, PersonAct::Land { relayed: false }],
        &j,
        &queue,
        &g,
    );
    for act in ["wc_checkout", "wc_commit", "wc_propose"] {
        let e = of(&j, "human:qa", act);
        assert_eq!(e.len(), 1, "{act}: {:#?}", j.entries());
        assert_eq!(e[0].outcome, Outcome::Ok, "{act}: {:?}", e[0]);
    }
    let land = of(&j, "human:qa", "land");
    assert_eq!(land.len(), 1, "{:#?}", j.entries());
    assert_eq!(land[0].outcome, Outcome::Ok, "{:?}", land[0]);
    assert_eq!(
        land[0].reply.as_ref().expect("the reply")["assurance"],
        "local"
    );
    // an agent reads the claim in canon: the body is the person's text
    run(
        &p,
        &cfg,
        0,
        vec![Act::Read {
            target: Pick::Seed(0),
        }],
        &j,
        &g,
    );
    let mut c = McpClient::start_proxy(&p.served);
    c.call(
        "rdy_session",
        json!({"actor": "agent:a0", "run": "a0", "role": "reader"}),
    )
    .ok();
    let r = c
        .call(
            "rdy_read",
            json!({"id": p.seed.claims[0], "scope": "canon", "format": "yaml"}),
        )
        .ok();
    assert!(
        r.to_string()
            .contains("The person sets this rule in round a."),
        "{r}"
    );
}

#[test]
fn sg4_an_agent_inbox_reads_one_contiguous_range() {
    let (p, cfg, j, g) = rig(2);
    let collide = |with| Act::Collide {
        with,
        target: Pick::Seed(0),
    };
    let reads = || vec![Act::Inbox { ack: true }, Act::Inbox { ack: true }];
    // agent 1 first; agent 0 collides with it, so its own write opens the tension
    run(&p, &cfg, 1, [vec![collide(0)], reads()].concat(), &j, &g);
    run(&p, &cfg, 0, [vec![collide(1)], reads()].concat(), &j, &g);
    for actor in ["agent:a0", "agent:a1"] {
        let r = of(&j, actor, "inbox");
        assert_eq!(r.len(), 2, "{actor}: {:#?}", j.entries());
        let reads: Vec<_> = r
            .iter()
            .map(|e| e.inbox.clone().expect("an inbox read"))
            .collect();
        assert_eq!(
            reads[1].since, reads[0].cursor,
            "{actor}: the second read starts where the first ended"
        );
        assert!(reads.iter().all(|x| x.range == 0), "{actor}");
        assert!(!reads[0].since.is_empty());
    }
    // agent 0's reply listed the collision it opened, and its inbox names it
    let a0 = of(&j, "agent:a0", "collide");
    let label = a0[0]
        .seen
        .iter()
        .find(|l| l.starts_with("tn-"))
        .unwrap_or_else(|| panic!("agent 0's write listed no tension: {a0:?}"))
        .clone();
    let named = of(&j, "agent:a0", "inbox").into_iter().any(|e| {
        e.seen.contains(&label)
            && e.inbox.as_ref().is_some_and(|r| {
                r.entries.iter().any(|x| {
                    x["event"] == "tension_opened"
                        && x["ids"][0]
                            .as_str()
                            .is_some_and(|id| id.starts_with(&label[3..]))
                })
            })
    });
    assert!(named, "{label} is in no inbox read: {:#?}", j.entries());
}

/// A contradiction pair through the real flow (ruling task-4.2): agent 0 creates its claim and
/// proposes, the person lands the proposal, agent 1 then creates its claim against canon, and
/// reviewer agent 2 judges the pair. Returns the project, the journal, the reviewer's
/// submissions and the landed proposal's ref.
fn judged_pair(k: usize) -> (SwarmProject, Journal, Vec<Entry>, String) {
    let (p, cfg, j, g) = rig(4);
    let pair = |area| Act::Create {
        area,
        contra: Some(k),
        source: false,
    };
    let first = run(&p, &cfg, 0, vec![pair("payments"), Act::Propose], &j, &g);
    assert_eq!(first.proposals.len(), 1, "{:#?}", j.entries());
    let queue = Arc::new(Mutex::new(first.proposals.clone()));
    run_person(&p, vec![PersonAct::Land { relayed: true }], &j, &queue, &g);
    run(&p, &cfg, 1, vec![pair("payments")], &j, &g);
    // the reviewer leases requests until it has judged the pair
    run(&p, &cfg, 2, vec![Act::ReviewNext; 24], &j, &g);
    let submits = of(&j, "agent:a2", "review_submit");
    (p, j, submits, first.proposals[0].clone())
}

/// The semantic tension rows the relay reads.
fn semantic_rows(p: &SwarmProject) -> Vec<serde_json::Value> {
    let mut r = relay(p);
    let t = r
        .call(
            "rdy_tensions",
            json!({"kind": "semantic", "max_tokens": 100_000}),
        )
        .ok();
    t["tensions"].as_array().expect("tensions").clone()
}

/// The journal shows the first member's landing before the second member's create.
fn assert_landing_came_first(j: &Journal, landed: &str) {
    let entries = j.entries();
    let at = |f: &dyn Fn(&Entry) -> bool| entries.iter().position(f);
    let land =
        at(&|e| e.act == "land" && e.outcome == Outcome::Ok && e.block.as_deref() == Some(landed))
            .unwrap_or_else(|| panic!("no landing of {landed}: {entries:#?}"));
    let creates: Vec<(usize, &Entry)> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.act == "create" && e.outcome == Outcome::Ok)
        .collect();
    assert_eq!(creates.len(), 2, "{entries:#?}");
    assert_eq!(creates[0].1.actor, "agent:a0");
    assert_eq!(creates[1].1.actor, "agent:a1");
    assert!(
        creates[0].0 < land,
        "the first claim is written before it lands"
    );
    assert!(
        land < creates[1].0,
        "the landing comes before the second create"
    );
    // no hidden act: the second member never includes the first member's change
    assert!(entries.iter().all(|e| e.act != "include"), "{entries:#?}");
}

#[test]
fn sg4_a_reviewer_judges_a_contra_pair() {
    let (p, j, submits, landed) = judged_pair(2);
    assert_landing_came_first(&j, &landed);
    assert!(!submits.is_empty(), "{:#?}", j.entries());
    assert!(
        submits.iter().all(|e| e.outcome == Outcome::Ok),
        "{submits:?}"
    );
    let rows = semantic_rows(&p);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["judged"]["actor"], "agent:a2", "{rows:?}");
    // k = 2 is even: the verdict advises to block, with a reason, and gates nothing until a
    // person accepts it (the tension itself stays advisory)
    assert_eq!(
        rows[0]["judged"]["advice"], "scripted advice CONTRA-2",
        "{rows:?}"
    );
    assert_eq!(rows[0]["severity"], "advisory", "{rows:?}");
    // the submit's own reply carries the same advice: the executor sent severity blocking
    let label = rows[0]["id"].as_str().expect("a label").to_string();
    let judged: Vec<&Entry> = submits.iter().filter(|e| e.seen.contains(&label)).collect();
    assert_eq!(
        judged.len(),
        1,
        "the submit entry lists the tension: {submits:?}"
    );
    let reply = judged[0].reply.as_ref().expect("the submit reply");
    assert_eq!(reply["advice"], "scripted advice CONTRA-2", "{reply}");
    assert_eq!(reply["verdict"], "tension", "{reply}");
}

#[test]
fn sg4_a_reviewer_judges_an_odd_contra_pair_without_advice() {
    let (p, j, submits, landed) = judged_pair(3);
    assert_landing_came_first(&j, &landed);
    assert!(
        submits.iter().all(|e| e.outcome == Outcome::Ok),
        "{submits:?}"
    );
    let rows = semantic_rows(&p);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let judged = rows[0]["judged"].as_object().expect("a verdict");
    assert_eq!(judged["actor"], "agent:a2", "{rows:?}");
    assert_eq!(
        judged["citations"].as_array().map(Vec::len),
        Some(2),
        "{rows:?}"
    );
    // k = 3 is odd: the verdict is a tension with no advice to block
    assert!(!judged.contains_key("advice"), "{rows:?}");
    assert_eq!(rows[0]["severity"], "advisory", "{rows:?}");
    let label = rows[0]["id"].as_str().expect("a label").to_string();
    let reply = submits
        .iter()
        .find(|e| e.seen.contains(&label))
        .and_then(|e| e.reply.as_ref())
        .expect("the submit entry lists the tension");
    assert_eq!(reply["verdict"], "tension", "{reply}");
    assert!(reply.get("advice").is_none(), "{reply}");
}

#[test]
fn sg4_an_unexpected_refusal_panics_with_the_seed() {
    let (p, cfg, j, g) = rig(2);
    // an empty seed block id does not parse: the read is refused with E_BAD_INPUT, which the
    // table does not allow for a read
    let broken = SwarmProject {
        seed: project::SeedBlocks {
            claims: vec![String::new(); p.seed.claims.len()],
            ..p.seed.clone()
        },
        run_seed: p.run_seed,
        seed_landings: p.seed_landings.clone(),
        served: p.served,
    };
    let err = agent::spawn(
        &broken,
        &cfg,
        0,
        vec![Act::Read {
            target: Pick::Seed(0),
        }],
        j.clone(),
        g,
    )
    .join()
    .expect_err("the read must fail the run");
    let text = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| (*s).to_string()))
        .expect("a panic text");
    assert!(
        text.starts_with(&format!(
            "swarm seed {SEED}: agent:a0 step 0 (read) got E_BAD_INPUT"
        )),
        "{text}"
    );
    // the refusal is journaled before the panic
    let e = of(&j, "agent:a0", "read");
    assert_eq!(e[0].outcome, Outcome::Refused("E_BAD_INPUT".into()));
}

// ---- ruling task-4.1: preconditions come from state, before the call ----

/// The entry of `act` by agent `i`; panics unless there is exactly one.
fn only(j: &Journal, i: usize, act: &str) -> Entry {
    let mut e = of(j, &format!("agent:a{i}"), act);
    assert_eq!(e.len(), 1, "{act}: {:#?}", j.entries());
    e.remove(0)
}

fn assert_no_refusal(j: &Journal) {
    let refused: Vec<Entry> = j
        .entries()
        .into_iter()
        .filter(|e| matches!(e.outcome, Outcome::Refused(_)))
        .collect();
    assert!(refused.is_empty(), "{refused:#?}");
}

#[test]
fn sg4_a_propose_with_nothing_in_the_view_is_skipped_from_the_state() {
    let (p, cfg, j, g) = rig(2);
    run(&p, &cfg, 0, vec![Act::Propose], &j, &g);
    // the agent has written nothing, so its view does not exist: the daemon's E_NOT_FOUND is
    // the answer, and the journal shows it as the refusal it is
    let shown = only(&j, 0, "view_show");
    assert_eq!(
        (shown.outcome, shown.view.as_deref()),
        (Outcome::Refused("E_NOT_FOUND".into()), Some("a0"))
    );
    let e = only(&j, 0, "propose");
    assert_eq!(e.outcome, Outcome::Skipped, "{e:?}");
    assert_eq!(e.skip, Some(Skip::NothingToPropose));
    assert!(e.block.is_none(), "no proposal was made");
    let refused: Vec<&str> = j
        .entries()
        .iter()
        .filter(|e| matches!(e.outcome, Outcome::Refused(_)))
        .map(|e| e.act)
        .collect();
    assert_eq!(refused, ["view_show"], "no other call was refused");
}

#[test]
fn sg4_an_exclude_with_nothing_included_is_skipped() {
    let (p, cfg, j, g) = rig(2);
    run(&p, &cfg, 0, vec![Act::Exclude { forced: false }], &j, &g);
    let e = only(&j, 0, "exclude");
    assert_eq!(
        (e.outcome.clone(), e.skip),
        (Outcome::Skipped, Some(Skip::NothingToExclude))
    );
    assert_no_refusal(&j);
}

#[test]
fn sg4_an_include_goes_through_while_the_change_is_in_its_authors_view() {
    let (p, cfg, j, g) = rig(2);
    run(
        &p,
        &cfg,
        1,
        vec![Act::Create {
            area: "payments",
            contra: None,
            source: false,
        }],
        &j,
        &g,
    );
    run(
        &p,
        &cfg,
        0,
        vec![
            Act::Include {
                from: 1,
                forced: false,
            },
            Act::Exclude { forced: false },
        ],
        &j,
        &g,
    );
    // the include asked the author's view; the exclude then asked the agent's own
    let reads = of(&j, "agent:a0", "view_show");
    assert_eq!(reads.len(), 2, "{:#?}", j.entries());
    assert_eq!(reads[0].view.as_deref(), Some("a1"));
    assert_eq!(reads[1].view.as_deref(), Some("a0"));
    assert_eq!(only(&j, 0, "include").outcome, Outcome::Ok);
    assert_eq!(only(&j, 0, "exclude").outcome, Outcome::Ok);
    assert_no_refusal(&j);
}

/// Deep swarm ruling 1: the forced include sequence. The source writer creates its claim in its
/// source view `a<i>-in`; another writer includes that change into its include view `a<i>-ex`,
/// then excludes it, and the include view is left empty. Neither act touches the agents' own
/// views.
#[test]
fn ds1_the_forced_include_sequence_includes_then_excludes_the_source_claim() {
    let (p, cfg, j, g) = rig(2);
    run(
        &p,
        &cfg,
        1,
        vec![Act::Create {
            area: "payments",
            contra: None,
            source: true,
        }],
        &j,
        &g,
    );
    let created = only(&j, 1, "create");
    assert_eq!(created.outcome, Outcome::Ok);
    assert_eq!(created.view.as_deref(), Some("a1-in"));
    let change = created.change.clone().expect("the source change");
    run(
        &p,
        &cfg,
        0,
        vec![
            Act::Include {
                from: 1,
                forced: true,
            },
            Act::Exclude { forced: true },
        ],
        &j,
        &g,
    );
    let inc = only(&j, 0, "include");
    assert_eq!(inc.outcome, Outcome::Ok, "{:#?}", j.entries());
    assert_eq!(inc.view.as_deref(), Some("a0-ex"));
    assert_eq!(inc.reply, Some(json!({"included": [change]})));
    let exc = only(&j, 0, "exclude");
    assert_eq!(exc.outcome, Outcome::Ok, "{:#?}", j.entries());
    assert_eq!(exc.view.as_deref(), Some("a0-ex"));
    // the include read the source view, the exclude the include view
    let reads: Vec<Option<String>> = of(&j, "agent:a0", "view_show")
        .into_iter()
        .map(|e| e.view)
        .collect();
    assert_eq!(
        reads,
        [Some("a1-in".to_string()), Some("a0-ex".to_string())]
    );
    let mut relay = relay(&p);
    let shown = relay
        .call(
            "rdy_view",
            json!({"op": "show", "view": "a0-ex", "max_tokens": 100000}),
        )
        .ok();
    assert!(
        shown["changes"].as_array().is_none_or(Vec::is_empty),
        "{shown}"
    );
    assert_no_refusal(&j);
}

#[test]
fn sg4_an_include_of_a_change_that_landed_is_skipped_from_the_state() {
    let (p, cfg, j, g) = rig(2);
    let r = run(
        &p,
        &cfg,
        1,
        vec![
            Act::Create {
                area: "payments",
                contra: None,
                source: false,
            },
            Act::Propose,
        ],
        &j,
        &g,
    );
    let queue = Arc::new(Mutex::new(r.proposals.clone()));
    run_person(&p, vec![PersonAct::Land { relayed: true }], &j, &queue, &g);
    run(
        &p,
        &cfg,
        0,
        vec![Act::Include {
            from: 1,
            forced: false,
        }],
        &j,
        &g,
    );
    // it asked the author's view, which no longer holds the change
    assert_eq!(only(&j, 0, "view_show").view.as_deref(), Some("a1"));
    let e = only(&j, 0, "include");
    assert_eq!(
        (e.outcome.clone(), e.skip),
        (Outcome::Skipped, Some(Skip::NothingToInclude))
    );
    assert_no_refusal(&j);
}

#[test]
fn sg4_a_repin_waits_for_a_citation_that_changed() {
    // the cited block did not change: nothing to repin
    let (p, cfg, j, g) = rig(2);
    let cite = || Act::Cite {
        target: Pick::Seed(0),
    };
    run(&p, &cfg, 0, vec![cite(), Act::Repin], &j, &g);
    let e = only(&j, 0, "repin");
    assert_eq!(
        (e.outcome.clone(), e.skip),
        (Outcome::Skipped, Some(Skip::NothingToRepin))
    );
    assert_eq!(only(&j, 0, "suspects").outcome, Outcome::Ok);
    assert_no_refusal(&j);
    // the agent changes the cited block in its view: the citation is a suspect, and the
    // repin goes through
    let (p, cfg, j, g) = rig(2);
    let write = Act::Write {
        target: Pick::Seed(0),
        stable: true,
    };
    run(&p, &cfg, 0, vec![cite(), write, Act::Repin], &j, &g);
    let e = only(&j, 0, "repin");
    assert_eq!(
        (e.outcome.clone(), e.skip),
        (Outcome::Ok, None),
        "{:#?}",
        j.entries()
    );
    assert_no_refusal(&j);
}

#[test]
fn sg4_a_briefing_gate_probe_skips_a_closed_view() {
    let (p, cfg, j, g) = rig(2);
    // agent 1's own view exists and its owner closes it
    let mut c = McpClient::start_proxy(&p.served);
    c.call(
        "rdy_session",
        json!({"actor": "agent:a1", "run": "a1", "role": "writer"}),
    )
    .ok();
    let ops = json!([
        {"op": "create", "type": "doc", "id": "$d",
         "fields": {"name": "scratch-closed", "title": "Scratch", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "A note."}
    ]);
    c.call("rdy_write", json!({"ops": ops})).ok();
    c.call(
        "rdy_view",
        json!({"op": "close", "view": "a1", "reason": "test"}),
    )
    .ok();
    run(
        &p,
        &cfg,
        1,
        vec![Act::WriteNoBrief {
            target: Pick::Seed(0),
        }],
        &j,
        &g,
    );
    let e = only(&j, 1, "write_no_brief");
    assert_eq!(
        (e.outcome.clone(), e.skip),
        (Outcome::Skipped, Some(Skip::NoLiveView)),
        "{e:?}"
    );
    assert_no_refusal(&j);
}

/// Ruling task-4.1: each reason a seed can reach is reached by at least one seed in 1..=20, with
/// every act of the generated scripts run and no refusal outside the table. (`NoLiveView` needs
/// a closed view, which needs a landing: its test is above.)
#[test]
fn sg4_every_skip_reason_is_reached_by_some_seed() {
    let mut reasons: BTreeSet<String> = BTreeSet::new();
    for seed in 1..=20 {
        let cfg = Config {
            seed,
            agents: 4,
            steps: 24,
            kill_at: None,
        };
        let p = project::build(&cfg);
        let (j, g) = (Journal::new(), Arc::new(KillGate::new()));
        let hs: Vec<_> = script(&cfg)
            .agents
            .into_iter()
            .enumerate()
            .map(|(i, acts)| agent::spawn(&p, &cfg, i, acts, j.clone(), g.clone()))
            .collect();
        for h in hs {
            h.join()
                .unwrap_or_else(|e| panic!("seed {seed}: an agent panicked: {e:?}"));
        }
        reasons.extend(
            j.entries()
                .iter()
                .filter_map(|e| e.skip)
                .map(|r| format!("{r:?}")),
        );
    }
    for want in [
        Skip::NothingToPropose,
        Skip::NothingToRepin,
        Skip::NothingToInclude,
        Skip::NothingToExclude,
    ] {
        assert!(
            reasons.contains(&format!("{want:?}")),
            "{want:?} not in {reasons:?}"
        );
    }
}

// ---- finding 1: a lost daemon is accepted only in the kill window ----

/// Waits up to a minute for the journal to hold what `ok` looks for.
fn wait_for(j: &Journal, what: &str, ok: impl Fn(&[Entry]) -> bool) {
    let end = Instant::now() + Duration::from_secs(60);
    while !ok(&j.entries()) {
        assert!(
            Instant::now() < end,
            "timed out waiting for {what}: {:#?}",
            j.entries()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn reads(n: usize) -> Vec<Act> {
    vec![
        Act::Read {
            target: Pick::Seed(0),
        };
        n
    ]
}

fn panic_text(err: Box<dyn std::any::Any + Send>) -> String {
    err.downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| (*s).to_string()))
        .expect("a panic text")
}

#[test]
fn sg4_a_lost_daemon_in_a_run_that_kills_nothing_fails_with_the_seed() {
    let (p, cfg, j, g) = rig(2);
    let h = agent::spawn(&p, &cfg, 0, reads(5000), j.clone(), g);
    wait_for(&j, "three reads", |e| e.len() >= 3);
    p.served.kill9();
    let text = panic_text(h.join().expect_err("the agent must fail"));
    assert!(
        text.starts_with(&format!("swarm seed {SEED}: agent:a0 step ")),
        "{text}"
    );
    assert!(
        text.contains("lost the daemon in a run that kills nothing"),
        "{text}"
    );
    assert!(j.entries().iter().all(|e| e.outcome != Outcome::Lost));
}

/// Final review M5 (minors item 20): in a run that expects a kill, a daemon lost before the run
/// starts its kill is a product crash: the agent fails with the seed and journals no `Lost`.
#[test]
fn sg5_a_lost_daemon_before_the_run_starts_its_kill_fails_with_the_seed() {
    let (p, cfg, j, _) = rig(2);
    let g = Arc::new(KillGate::with_kill());
    let h = agent::spawn(&p, &cfg, 0, reads(5000), j.clone(), g);
    wait_for(&j, "three reads", |e| e.len() >= 3);
    p.served.kill9();
    let text = panic_text(h.join().expect_err("the agent must fail"));
    assert!(
        text.starts_with(&format!("swarm seed {SEED}: agent:a0 step ")),
        "{text}"
    );
    assert!(
        text.contains("lost the daemon before the run started its kill"),
        "{text}"
    );
    assert!(j.entries().iter().all(|e| e.outcome != Outcome::Lost));
}

#[test]
fn sg4_a_lost_daemon_after_the_restart_fails_with_the_seed() {
    let (p, cfg, j, _) = rig(2);
    let g = Arc::new(KillGate::with_kill());
    let h = agent::spawn(&p, &cfg, 0, reads(5000), j.clone(), g.clone());
    wait_for(&j, "three reads", |e| e.len() >= 3);
    // the kill window: the call is lost, the agent waits, then goes on in epoch 1
    g.killing();
    p.served.kill9();
    g.restarted();
    wait_for(&j, "a read after the restart", |e| {
        e.iter().any(|x| x.epoch == 1 && x.outcome == Outcome::Ok)
    });
    let lost: Vec<Entry> = j
        .entries()
        .into_iter()
        .filter(|e| e.outcome == Outcome::Lost)
        .collect();
    assert_eq!(lost.len(), 1, "{lost:#?}");
    assert_eq!(lost[0].epoch, 0);
    // a second death is not in the window
    p.served.kill9();
    let text = panic_text(h.join().expect_err("the agent must fail"));
    assert!(
        text.starts_with(&format!("swarm seed {SEED}: agent:a0 step ")),
        "{text}"
    );
    assert!(
        text.contains("lost the daemon after the restart (epoch 1)"),
        "{text}"
    );
    assert_eq!(
        j.entries()
            .iter()
            .filter(|e| e.outcome == Outcome::Lost)
            .count(),
        1,
        "no second Lost is journaled"
    );
}

// ---- O17 (D-JQD5X): a choosing version holds its choice decision ----

/// O17 with ap13: agent a0 holds a scratch change, and the seeded draw makes its first propose
/// scratch-only (`rdy_propose {changes}`), but its view is a member of an open set that another
/// agent made. The propose is refused with `E_ALT_UNCHOSEN`; a0 records its choice decision and
/// proposes again, choosing. A choosing version names the whole view: a list of the scratch
/// changes leaves the decision out, and that version is not ready (`E_ALT_DECISION`). So the
/// choosing version is ready: its reply carries the policy's verdict, which a not-ready version
/// never has.
#[test]
fn sg4_a_choice_after_a_scratch_only_draw_holds_its_decision() {
    let seed = (1..1_000)
        .find(|s| agent::scratch_only_draw(*s, 0, 0))
        .expect("a seed whose first propose of a0 is scratch-only");
    let cfg = Config {
        seed,
        agents: 2,
        steps: 12,
        kill_at: None,
    };
    let p = project::build(&cfg);
    let (j, g) = (Journal::new(), Arc::new(KillGate::new()));
    // view a1 exists before the set names it
    run(
        &p,
        &cfg,
        1,
        vec![Act::Write {
            target: Pick::Seed(1),
            stable: false,
        }],
        &j,
        &g,
    );
    // a0's propose answers its change plan in the plan turn before its call: while this act
    // turn is held, the call waits, so the set exists before it
    let hold = g.act_turn();
    let a0 = agent::spawn(
        &p,
        &cfg,
        0,
        vec![
            Act::Write {
                target: Pick::Seed(0),
                stable: false,
            },
            Act::Propose,
        ],
        j.clone(),
        g.clone(),
    );
    wait_for(&j, "a0's scratch write", |es| {
        es.iter()
            .any(|e| e.actor == "agent:a0" && e.act == "write" && e.outcome == Outcome::Ok)
    });
    let mut a1 = McpClient::start_proxy(&p.served);
    a1.call(
        "rdy_session",
        json!({"actor": "agent:a1", "run": "a1", "role": "writer"}),
    )
    .ok();
    let set = a1
        .call(
            "rdy_alternatives",
            json!({"op": "create", "title": "which rule wins", "members": [
                {"label": "a0", "view": "a0"}, {"label": "a1", "view": "a1"}]}),
        )
        .ok();
    let set = set["set"]["id"].as_str().expect("the set's id").to_string();
    drop(hold);
    a0.join()
        .unwrap_or_else(|e| panic!("agent a0 panicked: {e:?}"));
    let all = || format!("{:#?}", j.entries());
    let proposes = of(&j, "agent:a0", "propose");
    assert_eq!(proposes.len(), 2, "{}", all());
    assert_eq!(
        proposes[0].outcome,
        Outcome::Refused("E_ALT_UNCHOSEN".into()),
        "{}",
        all()
    );
    // the first attempt was the scratch-only draw: the seed draws it, a0 holds a scratch change
    // and knew no set
    let decided = of(&j, "agent:a0", "create");
    assert_eq!(decided.len(), 1, "the choice decision: {}", all());
    assert_eq!(decided[0].outcome, Outcome::Ok, "{}", all());
    let choice = &proposes[1];
    assert_eq!(choice.outcome, Outcome::Ok, "{}", all());
    let reply = choice.reply.as_ref().expect("the choosing propose's reply");
    assert_eq!(
        reply["alternatives"],
        json!({"set": set, "chosen": "a0"}),
        "{reply:#}"
    );
    assert!(
        reply["policy"]["verdict"].is_string(),
        "the choosing version is not ready: {reply:#}"
    );
}

// ---------------------------------------------------------------- mg8: chat across the M3 routes

/// The scenario of `route` for agent `agent` at step 0, as `m3_scenarios` describes one.
fn scenario(route: Route, agent: usize) -> Scenario {
    let (subject, slot) = route.subject();
    Scenario {
        route,
        subject,
        slot,
        agent,
        step: 0,
        partner: None,
        person: route.needs_person(),
        max_calls: M3_MAX_CALLS,
    }
}

/// Agent `i` reads a seed block at step 0, then runs the scenarios of `routes`.
fn run_m3(
    p: &SwarmProject,
    cfg: &Config,
    i: usize,
    routes: &[Route],
    j: &Journal,
    g: &Arc<KillGate>,
    hook: Option<CallHook>,
) -> AgentReport {
    let m3 = M3Hooks {
        scenarios: routes.iter().map(|r| scenario(*r, i)).collect(),
        before_call: hook,
    };
    let read = vec![Act::Read {
        target: Pick::Seed(0),
    }];
    agent::spawn_m3(p, cfg, i, read, j.clone(), g.clone(), m3)
        .join()
        .unwrap_or_else(|e| panic!("agent {i} panicked: {e:?}"))
}

/// The route attempts of `act`, in order.
fn attempts(j: &Journal, act: &str) -> Vec<Entry> {
    j.entries()
        .into_iter()
        .filter(|e| e.act == act && e.route.is_some())
        .collect()
}

/// Where the entries that `f` picks sit in the journal, in order (two entries may be equal, so
/// a check of order reads positions, never an entry's first match).
fn pos(j: &Journal, f: impl Fn(&Entry) -> bool) -> Vec<usize> {
    j.entries()
        .iter()
        .enumerate()
        .filter(|(_, e)| f(e))
        .map(|(k, _)| k)
        .collect()
}

/// The card hash an `Ok` `land_card` entry showed.
fn shown_hash(e: &Entry) -> Option<String> {
    e.reply.as_ref()?["shown"]["card_hash"]
        .as_str()
        .map(str::to_string)
}

/// The log, newest first.
fn log(p: &SwarmProject) -> Vec<Value> {
    let (ok, v) = p.served.json(&["--json", "log"]);
    assert!(ok, "{v}");
    v.as_array()
        .or_else(|| v["landings"].as_array())
        .unwrap_or_else(|| panic!("a log: {v}"))
        .clone()
}

/// The log row of landing `l`.
fn row(p: &SwarmProject, l: &str) -> Value {
    log(p)
        .into_iter()
        .find(|r| r["landing"] == l)
        .unwrap_or_else(|| panic!("no log row of {l}"))
}

/// The landing an acknowledged attempt names.
fn acked(e: &Entry) -> String {
    match &e.route.as_ref().expect("an attempt").reply {
        Reply::Acknowledged { ack, records } => {
            assert_eq!(records, &vec![ack.clone()], "{e:?}");
            ack.clone()
        }
        other => panic!("not acknowledged: {other:?} in {e:?}"),
    }
}

/// G3: the person's relayed landing reads the queue, shows the version's card, and sends the
/// person's words with `rdy_answer` and that card's hash. The log stores card evidence with the
/// hash shown (a command-line relay stores text) and the words quoted.
#[test]
fn mg8_primary_person_lands_through_rdy_answer() {
    let (p, cfg, j, g) = rig(2);
    let report = run(
        &p,
        &cfg,
        0,
        vec![
            Act::Brief {
                target: Pick::Seed(0),
            },
            Act::Write {
                target: Pick::Seed(0),
                stable: true,
            },
            Act::Propose,
        ],
        &j,
        &g,
    );
    let pref = report.proposals[0].clone();
    let queue = Arc::new(Mutex::new(vec![pref.clone()]));
    run_person(&p, vec![PersonAct::Land { relayed: true }], &j, &queue, &g);
    let cards: Vec<Entry> = of(&j, "agent:relay", LAND_CARD)
        .into_iter()
        .filter(|e| e.block.as_deref() == Some(pref.as_str()))
        .collect();
    assert_eq!(cards.len(), 1, "{:#?}", j.entries());
    let shown = cards[0].reply.as_ref().expect("the card read")["shown"].clone();
    assert_eq!(shown["ref"], json!(pref), "{shown}");
    assert!(
        shown["options"]
            .as_array()
            .is_some_and(|o| o.iter().any(|x| x == "approve")),
        "{shown}"
    );
    let hash = shown_hash(&cards[0]).expect("the hash shown");
    let land = of(&j, "agent:relay", "land");
    assert_eq!(land.len(), 1, "{:#?}", j.entries());
    assert_eq!(land[0].outcome, Outcome::Ok, "{:?}", land[0]);
    let card_at = pos(&j, |e| e.act == LAND_CARD);
    let land_at = pos(&j, |e| e.act == "land");
    assert!(card_at[0] < land_at[0], "the card came first");
    let reply = land[0].reply.as_ref().expect("the answer's reply");
    assert_eq!(reply["act"], "approve", "{reply}");
    let l = people::landing_fields(reply)["landing"]
        .as_str()
        .expect("a landing")
        .to_string();
    let r = row(&p, &l);
    assert_eq!(r["approved"], json!(pref), "{r:#}");
    assert_eq!(r["assurance"], "relayed", "{r:#}");
    assert_eq!(r["relayed_by"]["actor"], "agent:relay", "{r:#}");
    assert_eq!(r["evidence"]["kind"], "card", "{r:#}");
    assert_eq!(r["evidence"]["card_hash"], json!(hash), "{r:#}");
    assert_eq!(
        r["relay_evidence"]["quoted"],
        json!(format!("Approve {pref}.")),
        "{r:#}"
    );
    assert!(queue.lock().unwrap().is_empty());
}

/// G3: a card that moved after the person saw it is refused on its old hash and lands
/// nothing; the new card is read and shown before the next answer, which names the new hash.
#[test]
fn mg8_stale_card_is_presented_again_before_answer() {
    let (p, cfg, j, g) = rig(2);
    let landings = log(&p).len();
    run_m3(&p, &cfg, 0, &[Route::MovedCard], &j, &g, None);
    let tries = attempts(&j, "m3_moved_card");
    assert_eq!(tries.len(), 2, "{:#?}", j.entries());
    let (stale, fresh) = (&tries[0], &tries[1]);
    let (a, b) = (stale.route.as_ref().unwrap(), fresh.route.as_ref().unwrap());
    assert_eq!(
        stale.outcome,
        Outcome::Refused("E_PROPOSAL_MOVED".into()),
        "{stale:?}"
    );
    assert_ne!(a.observed_ref, b.observed_ref, "{a:?} {b:?}");
    assert_ne!(a.card_hash, b.card_hash, "{a:?} {b:?}");
    assert_eq!(fresh.outcome, Outcome::Ok, "{fresh:?}");
    // each answer names the hash of a card read and shown just before it
    let entries = j.entries();
    let cards = pos(&j, |e| e.act == LAND_CARD);
    let tried = pos(&j, |e| e.act == "m3_moved_card" && e.route.is_some());
    for t in &tried {
        let shown = cards
            .iter()
            .rfind(|c| *c < t && entries[**c].block == entries[*t].block)
            .unwrap_or_else(|| panic!("no card shown before {:?}", entries[*t]));
        assert_eq!(
            shown_hash(&entries[*shown]).as_ref(),
            entries[*t].route.as_ref().unwrap().card_hash.as_ref(),
            "{:?}",
            entries[*t]
        );
    }
    let fresh_card = cards
        .iter()
        .find(|c| entries[**c].block == fresh.block)
        .expect("the new card");
    assert!(tried[0] < *fresh_card, "shown after the refusal");
    // one landing: the new version, on the new card; the moved one never lands
    let l = acked(fresh);
    let rows = log(&p);
    assert_eq!(rows.len(), landings + 1, "{rows:#?}");
    let r = row(&p, &l);
    assert_eq!(r["approved"].as_str(), b.observed_ref.as_deref(), "{r:#}");
    assert_eq!(r["evidence"]["card_hash"].as_str(), b.card_hash.as_deref());
    assert!(
        rows.iter()
            .all(|r| r["approved"].as_str() != a.observed_ref.as_deref()),
        "{rows:#?}"
    );
    let found = of(&j, "agent:a0", "land_found");
    assert!(
        found.iter().any(
            |e| e.reply.as_ref().is_some_and(|x| x["landing"] == l.as_str()) && e.base.is_some()
        ),
        "{found:#?}"
    );
}

/// G3: the person answers through the eliciting client in both built revisions (local,
/// elicited evidence), declines once (nothing is recorded and the card stays open), and a
/// separate scripted answer is relayed after the decline. The local approval pages are the
/// simulated person's lane alone: no agent answers through them.
#[test]
fn mg8_elicitation_and_ui_use_the_real_person_lanes() {
    let (p, cfg, j, g) = rig(2);
    let landings = log(&p).len();
    run_m3(
        &p,
        &cfg,
        0,
        &[Route::ElicitationRevisions, Route::DeclineThenRelay],
        &j,
        &g,
        None,
    );
    for act in ["m3_elicit_first_rev", "m3_elicit_second_rev"] {
        let t = attempts(&j, act);
        assert_eq!(t.len(), 1, "{act}: {:#?}", j.entries());
        assert_eq!(t[0].outcome, Outcome::Ok, "{act}: {:?}", t[0]);
        let counts = t[0].route.as_ref().unwrap().counts;
        assert!(
            counts.requests >= 1 && counts.answers == counts.requests,
            "{act}: {counts:?}"
        );
        let r = row(&p, &acked(&t[0]));
        assert_eq!(r["assurance"], "local", "{act}: {r:#}");
        assert_eq!(r["evidence"]["kind"], "elicited", "{act}: {r:#}");
        assert!(r["relayed_by"].is_null(), "{act}: {r:#}");
    }
    // the decline is an answer that records nothing
    let declined = attempts(&j, "m3_elicit_decline");
    assert_eq!(declined.len(), 1, "{:#?}", j.entries());
    assert_eq!(declined[0].outcome, Outcome::Ok, "{:?}", declined[0]);
    assert_eq!(
        declined[0].reply,
        Some(json!({"state": "declined"})),
        "{:?}",
        declined[0]
    );
    assert_eq!(
        declined[0].route.as_ref().unwrap().reply,
        Reply::Acknowledged {
            ack: "declined".into(),
            records: vec![]
        }
    );
    // the card stays open and is shown again; the separate answer is relayed on it
    let after = attempts(&j, "m3_relay_after_decline");
    assert_eq!(after.len(), 1, "{:#?}", j.entries());
    assert_eq!(after[0].outcome, Outcome::Ok, "{:?}", after[0]);
    let entries = j.entries();
    let decline_at = pos(&j, |e| e.act == "m3_elicit_decline")[0];
    let again = entries[decline_at..]
        .iter()
        .find(|c| c.act == LAND_CARD && c.block == declined[0].block)
        .expect("the card read again after the decline");
    assert!(shown_hash(again).is_some(), "{again:?}");
    let pref = declined[0].block.clone().unwrap();
    let r = row(&p, &acked(&after[0]));
    assert_eq!(r["approved"], json!(pref), "{r:#}");
    assert_eq!(r["assurance"], "relayed", "{r:#}");
    assert_eq!(
        r["relay_evidence"]["quoted"],
        json!(format!("I have read {pref} now. Land it.")),
        "{r:#}"
    );
    assert_eq!(log(&p).len(), landings + 3, "two elicited, one relayed");
    // the local UI: the person's page answer lands, and only the person's driver answers there
    let queue = Arc::new(Mutex::new(Vec::new()));
    run_person(
        &p,
        vec![PersonAct::UiAnswer {
            step: UiStep::Drawn,
        }],
        &j,
        &queue,
        &g,
    );
    let ui: Vec<Entry> = j
        .entries()
        .into_iter()
        .filter(|e| e.act == "ui_answer")
        .collect();
    assert!(!ui.is_empty(), "{:#?}", j.entries());
    for e in &ui {
        assert_eq!(e.actor, "human:qa", "{e:?}");
        assert_eq!(e.ui.as_ref().map(|u| u.status), Some(303), "{e:?}");
    }
}

/// G3: an agent's `rdy_land` of a version its policy does not accept is refused
/// (`E_POLICY_HUMAN`), lands nothing and answers nothing for the person: the card stays as it
/// was. Only the person's own answer lands it, as a relayed approval, not a policy landing.
/// The wrong-role route is not run in the swarm (one person in one daemon); it says so.
#[test]
fn mg8_policy_refusal_does_not_become_a_person_approval() {
    let (p, cfg, j, g) = rig(2);
    let report = run(
        &p,
        &cfg,
        0,
        vec![
            Act::Brief {
                target: Pick::Seed(0),
            },
            Act::Write {
                target: Pick::Seed(0),
                stable: true,
            },
            Act::Propose,
        ],
        &j,
        &g,
    );
    let pref = report.proposals[0].clone();
    assert!(
        of(&j, "agent:a0", "land").is_empty(),
        "a stable edit is outside the policy's rule"
    );
    let before = log(&p);
    let mut r = relay(&p);
    let card = |r: &mut McpClient| {
        let q = r.call("rdy_pending", json!({"session_ref": "chat"})).ok();
        q["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["ref"] == pref.as_str())
            .map(|i| i["card_hash"].clone())
    };
    let shown = card(&mut r).expect("the person's card");
    let mut a0 = McpClient::start_proxy(&p.served);
    a0.call(
        "rdy_session",
        json!({"actor": "agent:a0", "run": "a0", "role": "writer"}),
    )
    .ok();
    let refused = a0.call("rdy_land", json!({"proposal": pref}));
    assert_eq!(refused.code(), Some("E_POLICY_HUMAN"), "{refused:?}");
    drop(a0);
    assert_eq!(log(&p), before, "the refusal landed nothing");
    assert_eq!(
        card(&mut r),
        Some(shown),
        "no answer was recorded on the card"
    );
    drop(r);
    let queue = Arc::new(Mutex::new(vec![pref.clone()]));
    run_person(&p, vec![PersonAct::Land { relayed: true }], &j, &queue, &g);
    let land = of(&j, "agent:relay", "land");
    assert_eq!(land.len(), 1, "{:#?}", j.entries());
    let l = people::landing_fields(land[0].reply.as_ref().unwrap())["landing"]
        .as_str()
        .unwrap()
        .to_string();
    let row = row(&p, &l);
    assert_eq!(row["assurance"], "relayed", "{row:#}");
    assert_eq!(row["approved_by"], "human:qa", "{row:#}");
    assert_eq!(row["relayed_by"]["actor"], "agent:relay", "{row:#}");
    // the wrong-role route: a visible gap, never a silent pass
    run_m3(&p, &cfg, 1, &[Route::WrongThenRightRole], &j, &g, None);
    for act in Route::WrongThenRightRole.acts() {
        let e = of(&j, "agent:a1", act);
        assert_eq!(e.len(), 1, "{act}: {:#?}", j.entries());
        assert_eq!(e[0].outcome, Outcome::Skipped, "{e:?}");
        assert_eq!(e[0].message.as_deref(), Some(m3::NOT_BUILT), "{e:?}");
    }
    assert_eq!(log(&p).len(), before.len() + 1);
}

/// G3: while another agent writes on the same daemon, the publication route checks canon,
/// then checks and writes every output of a held landing scope (kept for the oracle, every
/// local link resolving), and the compact route removes the subject's claim on the person's
/// approval and runs a real compact that counts the removed payload.
#[test]
fn mg8_publication_and_compact_run_on_the_shared_daemon() {
    let (p, cfg, j, g) = rig(2);
    let other = agent::spawn(&p, &cfg, 1, write_script(6), j.clone(), g.clone());
    let report = run_m3(
        &p,
        &cfg,
        0,
        &[Route::PublicationCheckOutput, Route::RealCompact],
        &j,
        &g,
        None,
    );
    other.join().expect("agent 1 runs");
    let writes: Vec<Entry> = of(&j, "agent:a1", "write");
    assert!(
        writes.iter().filter(|e| e.outcome == Outcome::Ok).count() >= 1,
        "{writes:#?}"
    );
    let checks = attempts(&j, "m3_publication_check");
    assert_eq!(checks.len(), 2, "{:#?}", j.entries());
    assert!(
        checks.iter().all(|e| e.outcome == Outcome::Ok),
        "{checks:#?}"
    );
    assert_eq!(checks[0].block.as_deref(), Some("canon"));
    assert!(
        checks[1]
            .block
            .as_deref()
            .is_some_and(|s| s.starts_with("landing:")),
        "{:?}",
        checks[1]
    );
    let outputs = attempts(&j, "m3_publication_output");
    assert_eq!(outputs.len(), 3, "{:#?}", j.entries());
    assert!(
        outputs.iter().all(|e| e.outcome == Outcome::Ok),
        "{outputs:#?}"
    );
    assert!(outputs.iter().all(|e| e.block == checks[1].block));
    let links: usize = outputs
        .iter()
        .map(|e| e.route.as_ref().unwrap().counts.links_checked)
        .sum();
    assert!(links > 0, "{outputs:#?}");
    assert_eq!(report.m3_outputs.len(), 1, "{report:?}");
    assert!(
        std::fs::read_dir(&report.m3_outputs[0]).is_ok_and(|mut d| d.next().is_some()),
        "the outputs stay for the oracle"
    );
    // the person's removal, approved on its card, then the real compact
    let removed = attempts(&j, "m3_blob_remove");
    assert_eq!(removed.len(), 1, "{:#?}", j.entries());
    assert_eq!(removed[0].outcome, Outcome::Ok, "{:?}", removed[0]);
    let r = row(&p, &acked(&removed[0]));
    assert_eq!(r["assurance"], "relayed", "{r:#}");
    let compact = attempts(&j, "m3_compact_real");
    assert_eq!(compact.len(), 1, "{:#?}", j.entries());
    assert_eq!(compact[0].outcome, Outcome::Ok, "{:?}", compact[0]);
    let report = compact[0].reply.as_ref().expect("the compact report");
    assert!(
        report["removed_bytes"].as_u64().is_some_and(|b| b > 0),
        "{report}"
    );
    assert_eq!(compact[0].route.as_ref().unwrap().counts.removals, 1);
    // the snapshots before and after it
    let snapshots: Vec<Entry> = of(&j, "agent:a0", M3_FIXTURE)
        .into_iter()
        .filter(|e| {
            e.reply
                .as_ref()
                .is_some_and(|r| r.get("removed_bytes").is_some())
        })
        .collect();
    assert_eq!(snapshots.len(), 2, "{snapshots:#?}");
    let snaps = pos(&j, |e| {
        e.act == M3_FIXTURE
            && e.reply
                .as_ref()
                .is_some_and(|r| r.get("removed_bytes").is_some())
    });
    let real = pos(&j, |e| e.act == "m3_compact_real")[0];
    assert!(snaps[0] < real && real < snaps[1], "{snaps:?} {real}");
}

/// G3: the kill cuts an answer whose landing the daemon made: the client never reads the
/// reply. The agent reconnects through the normal proxy and session, reads whether the
/// version landed, finds the landing, and never sends the answer again.
#[test]
fn mg8_reconnect_rebuilds_the_session_without_replaying_an_ack() {
    let cfg = cfg(2);
    let p = project::build(&cfg);
    let (j, g) = (Journal::new(), Arc::new(KillGate::with_kill()));
    let (asked, wait) = mpsc::channel::<()>();
    let (go, gone) = mpsc::channel::<()>();
    let (asked, gone) = (Mutex::new(asked), Mutex::new(gone));
    let hook: CallHook = Arc::new(move |_, act| {
        if act == "m3_relay_answer" {
            asked.lock().unwrap().send(()).unwrap();
            gone.lock().unwrap().recv().unwrap();
        }
    });
    let m3 = M3Hooks {
        scenarios: vec![scenario(Route::RelayedAnswer, 0)],
        before_call: Some(hook),
    };
    let read = vec![Act::Read {
        target: Pick::Seed(0),
    }];
    let h = agent::spawn_m3(&p, &cfg, 0, read, j.clone(), g.clone(), m3);
    wait.recv().expect("the scenario reaches its answer");
    // the card the agent showed is answered and lands; then the daemon dies before the agent's
    // own call is read
    let card = of(&j, "agent:a0", LAND_CARD).pop().expect("the card shown");
    let pref = card.block.clone().unwrap();
    let mut r = relay(&p);
    let v = r
        .call(
            "rdy_answer",
            json!({"ref": pref, "option": "approve", "card_hash": shown_hash(&card),
                "evidence": "Yes, land it.", "question": "Land it?", "session_ref": "chat"}),
        )
        .ok();
    let l = people::landing_fields(&v)["landing"]
        .as_str()
        .expect("a landing")
        .to_string();
    drop(r);
    g.killing();
    p.served.kill9();
    g.restarted();
    go.send(()).unwrap();
    h.join().expect("the agent recovers");
    let tries = attempts(&j, "m3_relay_answer");
    assert_eq!(
        tries.len(),
        1,
        "no answer is sent again: {:#?}",
        j.entries()
    );
    assert_eq!(tries[0].outcome, Outcome::Lost, "{:?}", tries[0]);
    assert_eq!(tries[0].route.as_ref().unwrap().reply, Reply::Interrupted);
    // the agent read the state on the restarted daemon and matched the landing
    let found = of(&j, "agent:a0", "land_found");
    let named: Vec<&Entry> = found
        .iter()
        .filter(|e| e.reply.as_ref().is_some_and(|x| x["landing"] == l.as_str()))
        .collect();
    assert_eq!(named.len(), 1, "{found:#?}");
    assert_eq!(named[0].epoch, 1, "{:?}", named[0]);
    assert!(named[0].base.is_some(), "{:?}", named[0]);
    let lost_at = pos(&j, |e| e.act == "m3_relay_answer")[0];
    let found_at = pos(&j, |e| {
        e.act == "land_found" && e.reply.as_ref().is_some_and(|x| x["landing"] == l.as_str())
    })[0];
    assert!(lost_at < found_at);
    let rows: Vec<Value> = log(&p)
        .into_iter()
        .filter(|r| r["approved"] == pref.as_str())
        .collect();
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(rows[0]["landing"], l.as_str());
}
