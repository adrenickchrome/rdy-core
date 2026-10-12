//! sg4: the scripted agents act over MCP, the person acts through its relaying agent and the
//! command line, and every call is journaled. Each test builds a project, runs short scripts
//! and checks the journal and the store. The kill and the restart belong to the run (sg5).
use rdy_qa::mcp::McpClient;
use rdy_qa::swarm::agent::{self, AgentReport, KillGate};
use rdy_qa::swarm::journal::{Entry, Journal, Outcome};
use rdy_qa::swarm::people;
use rdy_qa::swarm::project::{self, SwarmProject};
use rdy_qa::swarm::script::{Act, Config, PersonAct, Pick};
use serde_json::json;
use std::sync::{Arc, Mutex};

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
    // the later writer's reply listed it; the person's tensions read lists it too
    let queue = Arc::new(Mutex::new(Vec::new()));
    run_person(&p, vec![PersonAct::Tensions], &j, &queue, &g);
    let label = rows[0]["id"].as_str().expect("a label").to_string();
    let seen: Vec<Vec<String>> = collides.iter().map(|e| e.seen.clone()).collect();
    assert!(
        seen.iter().any(|s| s.contains(&label)),
        "{seen:?} lacks {label}"
    );
    assert!(of(&j, "agent:relay", "tensions")[0].seen.contains(&label));
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

#[test]
fn sg4_a_reviewer_judges_a_contra_pair() {
    let (p, cfg, j, g) = rig(4);
    for i in 0..2 {
        run(
            &p,
            &cfg,
            i,
            vec![Act::Create {
                area: "payments",
                contra: Some(2),
            }],
            &j,
            &g,
        );
    }
    let creates = j
        .entries()
        .into_iter()
        .filter(|e| e.act == "create")
        .count();
    assert_eq!(creates, 2, "{:#?}", j.entries());
    // the reviewer leases requests until it has judged the pair
    run(&p, &cfg, 2, vec![Act::ReviewNext; 24], &j, &g);
    let submits = of(&j, "agent:a2", "review_submit");
    assert!(!submits.is_empty(), "{:#?}", j.entries());
    assert!(
        submits.iter().all(|e| e.outcome == Outcome::Ok),
        "{submits:?}"
    );
    let mut r = relay(&p);
    let t = r
        .call(
            "rdy_tensions",
            json!({"kind": "semantic", "max_tokens": 100_000}),
        )
        .ok();
    let rows = t["tensions"].as_array().expect("tensions");
    assert_eq!(rows.len(), 1, "{t}");
    assert_eq!(rows[0]["judged"]["actor"], "agent:a2", "{t}");
    // k = 2 is even: the verdict advises to block, with a reason
    assert_eq!(
        rows[0]["judged"]["advice"], "scripted advice CONTRA-2",
        "{t}"
    );
    assert!(
        submits.iter().any(|e| e
            .seen
            .contains(&rows[0]["id"].as_str().unwrap().to_string())),
        "the submit entry lists the tension"
    );
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
