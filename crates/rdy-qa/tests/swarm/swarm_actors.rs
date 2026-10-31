//! sg4: the scripted agents act over MCP, the person acts through its relaying agent and the
//! command line, and every call is journaled. Each test builds a project, runs short scripts
//! and checks the journal and the store. The kill and the restart belong to the run (sg5).
use rdy_qa::mcp::McpClient;
use rdy_qa::swarm::agent::{self, AgentReport, KillGate};
use rdy_qa::swarm::journal::{Entry, Journal, Outcome, Skip};
use rdy_qa::swarm::people;
use rdy_qa::swarm::project::{self, SwarmProject};
use rdy_qa::swarm::script::{Act, Config, PersonAct, Pick, script};
use serde_json::json;
use std::collections::BTreeSet;
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
    let reply = land[0].reply.as_ref().expect("the approve reply");
    assert_eq!(reply["assurance"], "relayed", "{reply}");
    assert_eq!(reply["approved"], json!(report.proposals[0]));
    assert!(
        queue.lock().unwrap().is_empty(),
        "the landed ref left the queue"
    );
    // the log shows the landing, relayed by the person's agent
    let (ok, log) = p.served.json(&["--json", "log"]);
    assert!(ok, "{log}");
    assert_eq!(log[0]["landing"], reply["landing"], "{log}");
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
