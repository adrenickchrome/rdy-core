//! sg3: a swarm script is a pure function of its seed; it covers every act; outcomes are a closed table.
use rdy_qa::swarm::journal::{Entry, Outcome, Reply, RouteAttempt, RouteCounts, Via, allowed};
use rdy_qa::swarm::project;
use rdy_qa::swarm::script::{
    Act, CHECK_MIN_STEPS, Config, EXCLUDE_STEP, HoldStep, INCLUDE_SOURCE_STEP, INCLUDE_STEP,
    M3_MAX_BYTES, M3_MAX_CALLS, M3_MAX_MESSAGES, M3_PARTNER_GAP, MIN_STEPS, PersonAct, Route,
    RuleStep, SUBJECT_CLAIMS, Subject, UiStep, advice_steps, check_plan, direct_land_at,
    include_plan, m3_scenarios, person_weights, reviewer_weights, script, ui_plan, writer_weights,
};
use std::collections::BTreeSet;

fn names(acts: &[Act]) -> BTreeSet<&'static str> {
    acts.iter().map(Act::name).collect()
}

#[test]
fn sg3_the_same_seed_gives_the_same_script() {
    let a = script(&Config::fast(7));
    let b = script(&Config::fast(7));
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
    assert_ne!(format!("{a:?}"), format!("{:?}", script(&Config::fast(8))));
}

#[test]
fn sg3_every_agent_script_has_the_required_acts() {
    for seed in 1..=5 {
        let s = script(&Config::fast(seed));
        assert_eq!(s.agents.len(), 8);
        for (i, acts) in s.agents.iter().enumerate() {
            assert_eq!(acts.len(), 40);
            assert_eq!(acts[0].name(), "brief", "seed {seed} agent {i}");
            let n = names(acts);
            let need: &[&str] = if Config::fast(seed).is_reviewer(i) {
                &["review_next", "inbox"]
            } else {
                &["collide", "cite", "propose", "inbox"]
            };
            for need in need {
                assert!(n.contains(need), "seed {seed} agent {i} lacks {need}");
            }
            if !Config::fast(seed).is_reviewer(i) {
                assert!(
                    !n.contains("review_next"),
                    "seed {seed}: writer {i} must not review"
                );
            }
        }
        assert!(names(&s.agents[0]).contains("alt_create"));
        assert!(names(&s.agents[1]).contains("alt_create"));
        // mh7: writer 0 holds the compact act, and no other agent does
        assert!(names(&s.agents[0]).contains("compact"), "seed {seed}");
        for (i, acts) in s.agents.iter().enumerate().skip(1) {
            assert!(!names(acts).contains("compact"), "seed {seed} agent {i}");
        }
        let p: BTreeSet<_> = s.person.iter().map(PersonAct::name).collect();
        for need in [
            "land",
            "dismiss",
            "accept_advice",
            "wc_edit",
            "inbox_ack",
            "tensions",
            "accept_check_verdict",
            "ui_answer",
        ] {
            assert!(p.contains(need), "seed {seed}: the person lacks {need}");
        }
        // D-JQD5X: the check acts, writers' and a reviewer's
        let all: BTreeSet<&str> = s.agents.iter().flat_map(|a| names(a)).collect();
        for need in ["check_run", "check_request", "check_submit"] {
            assert!(all.contains(need), "seed {seed}: no agent holds {need}");
        }
        for (i, acts) in s.agents.iter().enumerate() {
            let n = names(acts);
            if Config::fast(seed).is_reviewer(i) {
                assert!(
                    !n.contains("check_run") && !n.contains("check_request"),
                    "seed {seed}: reviewer {i} must not run or request a check"
                );
            } else {
                assert!(
                    !n.contains("check_submit"),
                    "seed {seed}: writer {i} must not claim a check"
                );
            }
        }
        assert!(
            !s.contra.is_empty(),
            "seed {seed}: no contradiction pair for the reviewers"
        );
    }
}

#[test]
fn sg3_the_union_of_scripts_covers_every_act() {
    let mut all = BTreeSet::new();
    for seed in 1..=5 {
        for acts in script(&Config::fast(seed)).agents {
            all.extend(names(&acts));
        }
    }
    assert_eq!(all, Act::ALL_NAMES.iter().copied().collect::<BTreeSet<_>>());
    for act in ["check_run", "check_request", "check_submit"] {
        assert!(all.contains(act), "no script holds {act}");
    }
}

#[test]
fn sg3_the_kill_falls_inside_the_run() {
    for seed in 1..=20 {
        let c = Config::fast(seed);
        let k = c.kill_at.unwrap();
        assert!((10..30).contains(&k), "seed {seed}: kill at {k}");
    }
}

#[test]
fn sg3_the_outcome_table_is_closed() {
    // D-ME2HD: a conflict or a blocking tension refuses a landing as an issue of a version
    // that is not ready, never as the outer code
    assert!(allowed("land", "E_PROPOSAL_NOT_READY"));
    assert!(!allowed("land", "E_BLOCKING_TENSION"));
    assert!(!allowed("land", "E_CONFLICT_UNRESOLVED"));
    // D-ME2HD, M3a Task 6: a landing whose re-keyed block changed in its view after the proposal
    assert!(allowed("land", "E_STALE_BASE"));
    assert!(allowed("write_no_brief", "E_BRIEF_REQUIRED"));
    assert!(!allowed("write_no_brief", "E_STALE_BASE"));
    assert!(!allowed("write", "E_INTERNAL"));
    assert!(!allowed("tensions", "E_BAD_INPUT"));
    assert!(!allowed("no_such_act", "E_NOT_FOUND"));
    // mh7 (D-4YRQ4): a compact is a serialized read of a writer's session; it meets no
    // semantic refusal, only the seeded kill
    for code in [
        "E_PERMISSION",
        "E_LIMIT",
        "E_STALE_BASE",
        "E_BAD_INPUT",
        "E_STORE_READ_ONLY",
        "E_VIEW_CLOSED",
    ] {
        assert!(!allowed("compact", code), "compact allows {code}");
    }
    assert!(allowed("write", "E_BRIEF_STALE"));
    assert!(allowed("review_submit", "E_REVIEW_STALE"));
    assert!(allowed("inbox_resume", "E_BAD_INPUT"));
    assert!(!allowed("inbox", "E_BAD_INPUT"));
    // D-JQD5X: a verdict on a case that changed after its lease is stale; a run or a request
    // names a subject its view holds, so it meets no refusal; the card can move under an
    // acceptance
    assert!(allowed("check_submit", "E_REVIEW_STALE"));
    assert!(!allowed("check_submit", "E_SELF_REVIEW"));
    assert!(!allowed("check_run", "E_BAD_INPUT"));
    assert!(!allowed("check_request", "E_BAD_INPUT"));
    assert!(allowed("accept_check_verdict", "E_PROPOSAL_MOVED"));
    assert!(!allowed("accept_check_verdict", "E_BAD_INPUT"));
    assert_eq!(
        format!("{:?}", Outcome::Refused("E_X".into())),
        "Refused(\"E_X\")"
    );
}

#[test]
fn sg3_a_contra_pair_shares_one_area_and_one_marker() {
    for seed in 1..=20 {
        let s = script(&Config::fast(seed));
        assert!(!s.contra.is_empty(), "seed {seed}");
        // Every Create that carries a marker, with its agent and area.
        let mut marked: Vec<(usize, usize, &'static str)> = Vec::new();
        for (i, acts) in s.agents.iter().enumerate() {
            for act in acts {
                if let Act::Create {
                    area,
                    contra: Some(k),
                    ..
                } = act
                {
                    marked.push((i, *k, *area));
                }
            }
        }
        for (k, (a, b)) in s.contra.iter().enumerate() {
            let of_k: Vec<_> = marked.iter().filter(|(_, kk, _)| *kk == k).collect();
            assert_eq!(
                of_k.len(),
                2,
                "seed {seed}: pair {k} must have exactly two marked Creates"
            );
            let agents: BTreeSet<usize> = of_k.iter().map(|(i, _, _)| *i).collect();
            assert_eq!(agents, BTreeSet::from([*a, *b]), "seed {seed}: pair {k}");
            assert_eq!(
                of_k[0].2, of_k[1].2,
                "seed {seed}: pair {k} must share one area"
            );
        }
        // No marker outside the pair list.
        assert!(
            marked.iter().all(|(_, k, _)| *k < s.contra.len()),
            "seed {seed}"
        );
        // One Create per marker per agent: an agent in two pairs holds two Creates, each with its own k.
        for i in 0..s.agents.len() {
            let ks: Vec<usize> = marked
                .iter()
                .filter(|(a, _, _)| *a == i)
                .map(|(_, k, _)| *k)
                .collect();
            let uniq: BTreeSet<usize> = ks.iter().copied().collect();
            assert_eq!(
                ks.len(),
                uniq.len(),
                "seed {seed}: agent {i} repeats a marker"
            );
        }
    }
}

/// Ruling task-4.2: a contradiction pair goes through the real flow. The first member (`.0`)
/// creates its claim early and proposes after it; the second member (`.1`) creates its claim
/// after five eighths of the steps, so the first member's landing can come before it, and before
/// the last quarter, then proposes it before the last two steps, so the person is shown the
/// reviewers' advice in the run (ruling task-5.2). Agents 0 and 1 (one alternative set) are not
/// paired while other writers exist.
#[test]
fn sg3_a_contra_pair_is_written_through_the_real_flow() {
    let configs = (1..=20)
        .flat_map(|seed| {
            [
                Config::fast(seed),
                Config {
                    seed,
                    agents: 4,
                    steps: 24,
                    kill_at: None,
                },
                Config {
                    seed,
                    agents: 6,
                    steps: 30,
                    kill_at: None,
                },
            ]
        })
        .collect::<Vec<_>>();
    for cfg in configs {
        let s = script(&cfg);
        let writers = (0..cfg.agents).filter(|&i| !cfg.is_reviewer(i)).count();
        for (k, (first, second)) in s.contra.iter().enumerate() {
            let at = |agent: usize, want: &dyn Fn(&Act) -> bool| -> Vec<usize> {
                (0..cfg.steps)
                    .filter(|&i| want(&s.agents[agent][i]))
                    .collect()
            };
            let marked = |a: &Act| matches!(a, Act::Create { contra: Some(c), .. } if *c == k);
            let created_first = at(*first, &marked);
            let created_second = at(*second, &marked);
            let ctx = format!(
                "seed {} agents {} pair {k} {:?}",
                cfg.seed,
                cfg.agents,
                (first, second)
            );
            assert_eq!((created_first.len(), created_second.len()), (1, 1), "{ctx}");
            let (c1, c2) = (created_first[0], created_second[0]);
            assert!(
                c1 < cfg.steps / 4,
                "{ctx}: the first claim comes early ({c1})"
            );
            assert!(
                (cfg.steps / 2 + cfg.steps / 8..cfg.steps - cfg.steps / 4).contains(&c2),
                "{ctx}: the second claim comes after five eighths and before the last quarter ({c2})"
            );
            let proposes = at(*first, &|a| matches!(a, Act::Propose));
            assert!(
                proposes.iter().any(|&p| c1 < p && p < c2),
                "{ctx}: the first member proposes between the two claims: {proposes:?}"
            );
            let second_proposes = at(*second, &|a| matches!(a, Act::Propose));
            assert!(
                second_proposes.iter().any(|&p| c2 < p && p < cfg.steps - 2),
                "{ctx}: the second member proposes its claim before the last two steps: {second_proposes:?}"
            );
            if writers > 2 {
                assert_ne!(
                    (*first, *second),
                    (0, 1),
                    "{ctx}: agents 0 and 1 share an alternative set"
                );
            }
        }
    }
}

/// The forced advice sequence: the second member of pair 0 writes its marked claim, the first
/// reviewer judges every free request, then the writer proposes. Each comes two steps after the
/// last, so the step barrier orders them, after the kill and after the person's last landing
/// before the tail, and the person's tail `AcceptAdvice` waits until the proposal has ended.
#[test]
fn sg3_the_forced_advice_sequence_comes_before_the_tail_acceptance() {
    let configs = (1..=20).flat_map(|seed| [Config::fast(seed), Config::deep(seed)]);
    for cfg in configs {
        let s = script(&cfg);
        let a = advice_steps(&cfg).expect("the fast and deep tiers hold the sequence");
        let ctx = format!("seed {} steps {}", cfg.seed, cfg.steps);
        let (first, second) = s.contra[0];
        assert_eq!(
            s.agents[second][a.claim],
            Act::AdviceClaim {
                first,
                area: match &s.agents[first].iter().find(|x| matches!(
                    x,
                    Act::Create {
                        contra: Some(0),
                        ..
                    }
                )) {
                    Some(Act::Create { area, .. }) => area,
                    other => panic!("{ctx}: the first claim is {other:?}"),
                },
            },
            "{ctx}"
        );
        assert_eq!(s.agents[second][a.propose], Act::AdviceProposal, "{ctx}");
        let reviewer = (0..cfg.agents).find(|&i| cfg.is_reviewer(i)).unwrap();
        assert_eq!(s.agents[reviewer][a.review], Act::ReviewAll, "{ctx}");
        assert_eq!((a.claim + 2, a.review + 2), (a.review, a.propose), "{ctx}");
        assert!(cfg.kill_at.is_some_and(|k| k < a.claim), "{ctx}");
        let len = s.person.len();
        // the person's pace (`run::due`): act j waits for step (j + 1) * steps / len
        let due = |j: usize| (j + 1) * cfg.steps / len;
        assert_eq!(s.person[len - 2], PersonAct::AcceptAdvice, "{ctx}");
        assert!(a.propose < due(len - 2), "{ctx}");
        let last_land = (0..len - 2)
            .rev()
            .find(|&j| s.person[j].name() == "land")
            .unwrap();
        assert!(due(last_land) < a.propose, "{ctx}");
    }
}

/// Ruling task-5.2: the person lands on a cadence (every other act, `steps / 2` acts), ends by
/// accepting the advice it is shown and landing, and does each kind of act at least once.
#[test]
fn sg3_the_person_lands_on_a_cadence_and_ends_by_accepting_advice() {
    for cfg in (1..=20).flat_map(|seed| {
        [
            Config::fast(seed),
            Config::deep(seed),
            Config {
                seed,
                agents: 4,
                steps: 24,
                kill_at: None,
            },
        ]
    }) {
        let p = script(&cfg).person;
        assert_eq!(p.len(), cfg.steps / 2, "seed {}", cfg.seed);
        for (i, act) in p.iter().enumerate().take(p.len() - 2) {
            if i % 2 == 0 {
                assert_eq!(act.name(), "land", "seed {} act {i}", cfg.seed);
            }
        }
        assert_eq!(p[p.len() - 2], PersonAct::AcceptAdvice, "seed {}", cfg.seed);
        assert_eq!(p[p.len() - 1].name(), "land", "seed {}", cfg.seed);
        let names: BTreeSet<_> = p[..p.len() - 2].iter().map(PersonAct::name).collect();
        // the forced check verdict acceptance (D-JQD5X) and the forced UI answers (D-6M0AF) are
        // two more kinds, in the scripts that hold the forced sequences (the fast and deep tiers)
        let forced = 2 * usize::from(check_plan(&cfg).is_some());
        assert_eq!(names.len(), 6 + forced, "seed {}: {names:?}", cfg.seed);
    }
}

/// ui11 (D-6M0AF; system/quality-gate tb-A4DAV): every fast and deep script holds one forced
/// `GET`/`POST` answer and one forced moved card, on fixed odd acts due before the kill can come,
/// and the person's other draws keep their weights beside one more of weight 1. The check acts
/// keep their places. A script too short for the forced sequences holds and draws no `UiAnswer`.
#[test]
fn ui11_seeded_scripts_include_real_ui_answers() {
    let due = |j: usize, steps: usize, len: usize| ((j + 1) * steps / len).min(steps);
    assert_eq!(
        person_weights(),
        [
            ("dismiss", 20),
            ("accept_advice", 20),
            ("wc_edit", 15),
            ("inbox_ack", 15),
            ("tensions", 10),
            ("ui_answer", 1),
        ]
    );
    for seed in 1..=20 {
        for cfg in [Config::fast(seed), Config::deep(seed)] {
            let ctx = format!("seed {seed} steps {}", cfg.steps);
            let s = script(&cfg);
            let plan = ui_plan(&cfg).unwrap_or_else(|| panic!("{ctx}: no forced UI acts"));
            assert_eq!(ui_plan(&cfg), Some(plan), "{ctx}: a pure function");
            let len = s.person.len();
            assert_eq!(
                s.person[plan.moved],
                PersonAct::UiAnswer {
                    step: UiStep::Moved
                },
                "{ctx}"
            );
            assert_eq!(
                s.person[plan.forced],
                PersonAct::UiAnswer {
                    step: UiStep::Forced
                },
                "{ctx}"
            );
            let count = |step: UiStep| {
                s.person
                    .iter()
                    .filter(|a| **a == PersonAct::UiAnswer { step })
                    .count()
            };
            assert_eq!(count(UiStep::Moved), 1, "{ctx}");
            assert_eq!(count(UiStep::Forced), 1, "{ctx}");
            for j in [plan.moved, plan.forced] {
                assert_eq!(j % 2, 1, "{ctx}: not a landing act");
                assert!(j < len - 2, "{ctx}: before the tail");
                let k = cfg.kill_at.expect("a kill");
                assert!(due(j, cfg.steps, len) + 1 < k, "{ctx}: due before the kill");
            }
            // a working-copy edit keeps an early act, so rg15 still reads a work version's plan
            assert_eq!(s.person[5], PersonAct::WcEdit, "{ctx}");
            // the forced check sequences keep their acts
            let check = check_plan(&cfg).unwrap_or_else(|| panic!("{ctx}: no check acts"));
            assert_eq!(
                s.person[check.accept],
                PersonAct::AcceptCheckVerdict,
                "{ctx}"
            );
            assert_ne!(check.accept, plan.moved, "{ctx}");
            assert_ne!(check.accept, plan.forced, "{ctx}");
        }
    }
    let short = Config {
        seed: 3,
        agents: 8,
        steps: 24,
        kill_at: None,
    };
    assert_eq!(ui_plan(&short), None);
    assert!(
        !script(&short)
            .person
            .iter()
            .any(|a| a.name() == "ui_answer"),
        "no draw without the forced sequences"
    );
}

/// ck10 (D-JQD5X, D-ME2HD; system/quality-gate tb-A4DAV): a script is still a pure function of
/// its seed, and holds the check acts. Every fast and deep script holds one forced rule
/// failure and repair (`CheckRun` with a rule step, by one writer, in that order) and one
/// request, concern, person acceptance and new version, ordered by the run's step indexes and
/// pacing: the reviewer's step waits until the writer has finished its request, the person's act
/// until the reviewer has finished its concern, the new version until the person has acted. The
/// reviewer is a reviewer, so never the writer. Each check act weighs 1 and every older weight
/// keeps its value. The fast script stays 40 steps with its kill window.
#[test]
fn ck10_scripts_include_check_and_person_acts() {
    // the same seed gives the same script, forced acts included
    assert_eq!(
        format!("{:?}", script(&Config::fast(7))),
        format!("{:?}", script(&Config::fast(7)))
    );
    assert_eq!(check_plan(&Config::fast(7)), check_plan(&Config::fast(7)));
    // the run's pacing: person act j waits for step (j + 1) * steps / len
    let due = |j: usize, steps: usize, len: usize| ((j + 1) * steps / len).min(steps);
    for seed in 1..=20 {
        for cfg in [Config::fast(seed), Config::deep(seed)] {
            let s = script(&cfg);
            let ctx = format!("seed {seed} steps {}", cfg.steps);
            let p = check_plan(&cfg).unwrap_or_else(|| panic!("{ctx}: no forced checks"));
            assert!(!cfg.is_reviewer(p.rule_agent), "{ctx}: {p:?}");
            assert!(!cfg.is_reviewer(p.hold_agent), "{ctx}: {p:?}");
            assert!(cfg.is_reviewer(p.reviewer), "{ctx}: {p:?}");
            assert_ne!(p.reviewer, p.hold_agent, "{ctx}: another actor reviews");
            let at = |agent: usize, step: usize| &s.agents[agent][step];
            assert_eq!(
                at(p.rule_agent, p.rule_fail),
                &Act::CheckRun {
                    rule: Some(RuleStep::Fail)
                },
                "{ctx}"
            );
            assert_eq!(
                at(p.rule_agent, p.rule_repair),
                &Act::CheckRun {
                    rule: Some(RuleStep::Repair)
                },
                "{ctx}"
            );
            assert!(p.rule_fail < p.rule_repair, "{ctx}: {p:?}");
            assert_eq!(
                at(p.hold_agent, p.request),
                &Act::CheckRequest {
                    hold: Some(HoldStep::Request)
                },
                "{ctx}"
            );
            assert_eq!(at(p.reviewer, p.submit), &Act::CheckSubmit, "{ctx}");
            assert_eq!(
                at(p.hold_agent, p.new_version),
                &Act::CheckRequest {
                    hold: Some(HoldStep::NewVersion)
                },
                "{ctx}"
            );
            let len = s.person.len();
            assert_eq!(s.person[p.accept], PersonAct::AcceptCheckVerdict, "{ctx}");
            assert_eq!(p.accept % 2, 1, "{ctx}: not a landing act");
            assert!(p.accept < len - 2, "{ctx}: before the tail");
            // the order, by step indexes and barriers alone
            assert!(p.submit >= p.request + 2, "{ctx}: {p:?}");
            assert!(due(p.accept, cfg.steps, len) > p.submit, "{ctx}: {p:?}");
            assert!(
                p.new_version > due(p.accept, cfg.steps, len),
                "{ctx}: {p:?}"
            );
            // each forced sequence once: only the forced acts carry a step
            let forced: usize = s
                .agents
                .iter()
                .flatten()
                .filter(|a| {
                    matches!(
                        a,
                        Act::CheckRun { rule: Some(_) } | Act::CheckRequest { hold: Some(_) }
                    )
                })
                .count();
            assert_eq!(forced, 4, "{ctx}");
            let accepts = s
                .person
                .iter()
                .filter(|a| **a == PersonAct::AcceptCheckVerdict)
                .count();
            assert_eq!(accepts, 1, "{ctx}");
        }
        // the fast script keeps its length and its kill window
        let cfg = Config::fast(seed);
        assert!(script(&cfg).agents.iter().all(|a| a.len() == 40));
        assert!((10..30).contains(&cfg.kill_at.unwrap()));
    }
    // a script too short for them holds no forced checks
    for steps in [24, 30, CHECK_MIN_STEPS - 1] {
        let cfg = Config {
            seed: 3,
            agents: 8,
            steps,
            kill_at: None,
        };
        assert_eq!(check_plan(&cfg), None, "{steps} steps");
        assert!(!script(&cfg).person.contains(&PersonAct::AcceptCheckVerdict));
    }
    // each check act weighs 1; the older weights keep their values
    assert_eq!(
        writer_weights(),
        [
            ("brief", 5),
            ("write", 18),
            ("write_no_brief", 3),
            ("write_stale", 3),
            ("create", 8),
            ("collide", 6),
            ("cite", 6),
            ("repin", 3),
            ("fork", 3),
            ("include", 3),
            ("exclude", 2),
            ("undo", 3),
            ("intent", 3),
            ("alt_create", 3),
            ("propose", 8),
            ("tensions", 5),
            ("inbox", 5),
            ("read", 10),
            ("check_run", 1),
            ("check_request", 1),
        ]
    );
    assert_eq!(
        reviewer_weights(),
        [
            ("brief", 5),
            ("review_next", 40),
            ("inbox", 10),
            ("tensions", 10),
            ("read", 10),
            ("check_submit", 1),
        ]
    );
}

/// Fix round 2 (ruling task-5-round1.1): the generator draws `AltCreate` and `Include` as Task 3
/// drew them. Any writer may make a set with any other agent, and any writer may include from
/// any other agent, agents 0 and 1 included. Over the fast and deep scripts of 20 seeds, each of
/// these draws comes at least once.
#[test]
fn sg3_any_writer_makes_a_set_or_includes_as_task_3_drew() {
    let (mut set_beyond_0_and_1, mut include_by_0_or_1, mut include_from_0_or_1) =
        (false, false, false);
    for seed in 1..=20 {
        for cfg in [Config::fast(seed), Config::deep(seed)] {
            for (me, acts) in script(&cfg).agents.iter().enumerate() {
                for act in acts {
                    match act {
                        Act::AltCreate { with } => {
                            assert_ne!(*with, me, "seed {seed}: a{me} {act:?}");
                            set_beyond_0_and_1 |= me >= 2 || *with >= 2;
                        }
                        Act::Include { from, .. } => {
                            assert_ne!(*from, me, "seed {seed}: a{me} {act:?}");
                            include_by_0_or_1 |= me < 2;
                            include_from_0_or_1 |= *from < 2;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    assert!(set_beyond_0_and_1, "no set made beyond agents 0 and 1");
    assert!(include_by_0_or_1, "no include by agent 0 or 1");
    assert!(include_from_0_or_1, "no include from agent 0 or 1");
}

/// Ruling task-5-round1.1: at least one fast seed's script lets a set bind an included change
/// (msw20) and lands after the set can be chosen. An agent includes, then it makes a set or is
/// named in one, and the person lands after that set exists. The person's act `j` of `len` runs
/// after step `(j + 1) * steps / len` (the run's pacing).
#[test]
fn sg3_a_fast_seed_includes_before_a_set_and_lands_after_it() {
    let reaches = |seed: u64| {
        let cfg = Config::fast(seed);
        let s = script(&cfg);
        let len = s.person.len();
        let last_land = s
            .person
            .iter()
            .enumerate()
            .filter(|(_, a)| a.name() == "land")
            .map(|(j, _)| (j + 1) * cfg.steps / len)
            .max()
            .unwrap_or(0);
        // an include from an agent outside the includer's set: a change of the set partner is
        // a member's own work, not a change the set binds (msw20)
        s.agents.iter().enumerate().any(|(m, acts)| {
            acts.iter().enumerate().any(|(included, a)| {
                let Act::Include { from, .. } = a else {
                    return false;
                };
                s.agents.iter().enumerate().any(|(maker, theirs)| {
                    theirs.iter().enumerate().any(|(j, a)| {
                        let Act::AltCreate { with } = a else {
                            return false;
                        };
                        let partner = match (maker == m, *with == m) {
                            (true, _) => *with,
                            (_, true) => maker,
                            _ => return false,
                        };
                        *from != partner && included < j && j < last_land
                    })
                })
            })
        })
    };
    let seeds: Vec<u64> = (1..=6).filter(|s| reaches(*s)).collect();
    assert!(
        !seeds.is_empty(),
        "no fast seed includes before a set and lands after it"
    );
}

/// Final review M8 (minors item 3): each reviewer holds at least three `ReviewNext` acts.
#[test]
fn sg3_each_reviewer_holds_at_least_three_review_next() {
    for seed in 1..=20 {
        for cfg in [Config::fast(seed), Config::deep(seed)] {
            for (i, acts) in script(&cfg).agents.iter().enumerate() {
                if !cfg.is_reviewer(i) {
                    continue;
                }
                let n = acts.iter().filter(|a| matches!(a, Act::ReviewNext)).count();
                assert!(n >= 3, "seed {seed} reviewer {i}: {n} review_next");
            }
        }
    }
}

/// Final review M8 (minors item 2): a reviewer never writes, so `Collide`, `Include` and
/// `AltCreate` name a writer other than the agent itself.
#[test]
fn sg3_a_partner_is_always_another_writer() {
    for seed in 1..=20 {
        for cfg in [Config::fast(seed), Config::deep(seed)] {
            for (me, acts) in script(&cfg).agents.iter().enumerate() {
                for act in acts {
                    let other = match act {
                        Act::Collide { with, .. } | Act::AltCreate { with } => *with,
                        Act::Include { from, .. } => *from,
                        _ => continue,
                    };
                    assert!(
                        other != me && !cfg.is_reviewer(other),
                        "seed {seed} a{me}: {act:?} names a reviewer or itself"
                    );
                }
            }
        }
    }
}

/// Final review M4 (minors item 1): every person script lands direct (`rdy approve` with no
/// relay, the CLI parity path) at a fixed even act, and still lands through the relay.
#[test]
fn sg3_every_person_script_lands_direct_at_a_fixed_act() {
    for cfg in (1..=20).flat_map(|seed| {
        [
            Config::fast(seed),
            Config::deep(seed),
            Config {
                seed,
                agents: 4,
                steps: 24,
                kill_at: None,
            },
        ]
    }) {
        let p = script(&cfg).person;
        let at = direct_land_at(p.len());
        assert_eq!(at % 2, 0, "seed {}: a landing act", cfg.seed);
        assert!(at < p.len() - 2, "seed {}: before the tail", cfg.seed);
        assert_eq!(
            p[at],
            PersonAct::Land { relayed: false },
            "seed {}",
            cfg.seed
        );
        assert!(
            p.contains(&PersonAct::Land { relayed: true }),
            "seed {}",
            cfg.seed
        );
    }
}

#[test]
fn sg3_a_wc_step_name_that_is_not_listed_allows_no_refusal() {
    for step in ["wc_checkout", "wc_commit", "wc_update", "wc_propose"] {
        assert!(allowed(step, "E_WC_STALE"), "{step}");
        assert!(allowed(step, "E_WC_CONFLICT"), "{step}");
        assert!(!allowed(step, "E_NOT_FOUND"), "{step}");
    }
    assert!(!allowed("wc_comit", "E_WC_STALE"));
    assert!(!allowed("wc_", "E_WC_CONFLICT"));
}

/// Deep swarm ruling 1, item 3: every script that holds the forced check sequences (40 steps or
/// more, with a reviewer) also holds the forced include sequence, the same for the same seed. A
/// source writer creates the claim; another writer includes it two steps later and excludes it
/// one step after that. Neither is agent 0 or 1 (the scripted alternative set) or a reviewer. Its
/// steps share no index with a forced check step, and they come before the kill window.
#[test]
fn ds1_scripts_hold_the_forced_include_sequence() {
    // the pacing orders them: the include waits until the source's whole step is done
    const { assert!(INCLUDE_STEP >= INCLUDE_SOURCE_STEP + 2 && EXCLUDE_STEP > INCLUDE_STEP) };
    assert_eq!(
        format!("{:?}", script(&Config::deep(2))),
        format!("{:?}", script(&Config::deep(2)))
    );
    assert_eq!(
        include_plan(&Config::fast(2)),
        include_plan(&Config::fast(2))
    );
    let forced = |a: &Act| {
        matches!(
            a,
            Act::Create { source: true, .. }
                | Act::Include { forced: true, .. }
                | Act::Exclude { forced: true }
        )
    };
    for seed in 1..=20 {
        for cfg in [Config::fast(seed), Config::deep(seed)] {
            let s = script(&cfg);
            let ctx = format!("seed {seed} steps {}", cfg.steps);
            let p = include_plan(&cfg).unwrap_or_else(|| panic!("{ctx}: no forced include"));
            let c = check_plan(&cfg).unwrap_or_else(|| panic!("{ctx}: no forced checks"));
            for who in [p.source, p.writer] {
                assert!(who >= 2 && !cfg.is_reviewer(who), "{ctx}: {p:?}");
            }
            assert_ne!(p.source, p.writer, "{ctx}: {p:?}");
            assert!(
                matches!(
                    &s.agents[p.source][INCLUDE_SOURCE_STEP],
                    Act::Create {
                        source: true,
                        contra: None,
                        ..
                    }
                ),
                "{ctx}: {:?}",
                s.agents[p.source][INCLUDE_SOURCE_STEP]
            );
            assert_eq!(
                s.agents[p.writer][INCLUDE_STEP],
                Act::Include {
                    from: p.source,
                    forced: true
                },
                "{ctx}"
            );
            assert_eq!(
                s.agents[p.writer][EXCLUDE_STEP],
                Act::Exclude { forced: true },
                "{ctx}"
            );
            // once, and nowhere else
            let n: usize = s
                .agents
                .iter()
                .map(|acts| acts.iter().filter(|a| forced(a)).count())
                .sum();
            assert_eq!(n, 3, "{ctx}: forced include acts");
            // no overlap with the forced check placements, which keep their acts
            let steps = [INCLUDE_SOURCE_STEP, INCLUDE_STEP, EXCLUDE_STEP];
            for check in [
                c.rule_fail,
                c.rule_repair,
                c.request,
                c.new_version,
                c.submit,
            ] {
                assert!(!steps.contains(&check), "{ctx}: {c:?}");
            }
            assert_eq!(
                s.agents[c.rule_agent][c.rule_fail],
                Act::CheckRun {
                    rule: Some(RuleStep::Fail)
                },
                "{ctx}"
            );
            assert_eq!(s.agents[c.reviewer][c.submit], Act::CheckSubmit, "{ctx}");
            // before the kill window
            let kill = cfg.kill_at.expect("a kill");
            assert!(EXCLUDE_STEP < kill, "{ctx}: kill at {kill}");
        }
    }
    // a script with no forced checks holds no forced include either
    let short = Config {
        seed: 3,
        agents: 8,
        steps: MIN_STEPS,
        kill_at: None,
    };
    assert_eq!(include_plan(&short), None);
    assert!(
        script(&short)
            .agents
            .iter()
            .all(|acts| !acts.iter().any(&forced))
    );
}

/// nb10 (M3e): answering the neighbor items by a group needs no new act, no new refusal row and
/// no new random draw. The act names, a script's reproducibility and the closed table of
/// refusals are those of M3d: the plan answers still allow no refusal.
#[test]
fn nb10_swarm_grouping_needs_no_new_act_or_outcome() {
    assert_eq!(Act::ALL_NAMES.len(), 23);
    assert_eq!(
        Act::ALL_NAMES,
        [
            "brief",
            "write",
            "write_no_brief",
            "write_stale",
            "create",
            "collide",
            "cite",
            "repin",
            "fork",
            "include",
            "exclude",
            "undo",
            "intent",
            "alt_create",
            "propose",
            "review_next",
            "tensions",
            "inbox",
            "read",
            "check_run",
            "check_request",
            "check_submit",
            "compact",
        ]
    );
    for seed in [1, 7, 20] {
        for cfg in [Config::fast(seed), Config::deep(seed)] {
            assert_eq!(
                format!("{:?}", script(&cfg)),
                format!("{:?}", script(&cfg)),
                "seed {seed}"
            );
        }
    }
    // the plan answers, grouped or by id, and the plan reads meet only what they met before
    for code in [
        "E_PLAN_ITEM",
        "E_PLAN_OPEN",
        "E_BAD_INPUT",
        "E_LIMIT",
        "E_STALE_BASE",
    ] {
        assert!(!allowed("plan_answer", code), "plan_answer allows {code}");
    }
    assert!(allowed("plan", "E_CONFLICT_UNRESOLVED"));
    assert!(!allowed("plan", "E_PLAN_ITEM"));
    assert!(!allowed("version_plan", "E_PLAN_ITEM"));
}

/// Every fast and deep config of seeds 1 to 20.
fn both_tiers() -> impl Iterator<Item = Config> {
    (1..=20).flat_map(|seed| [Config::fast(seed), Config::deep(seed)])
}

/// mg7 (G2): every fast and deep seed forces every M3 route once, with no draw to leave one out.
/// The seeded project holds a small independent subject for each, and each route owns a claim.
#[test]
fn mg7_seeded_scripts_force_every_m3_route() {
    for cfg in both_tiers() {
        let sc = m3_scenarios(&cfg);
        let routes: Vec<Route> = sc.iter().map(|s| s.route).collect();
        assert_eq!(routes, Route::ALL.to_vec(), "seed {} {cfg:?}", cfg.seed);
        let subjects: BTreeSet<Subject> = sc.iter().map(|s| s.subject).collect();
        assert_eq!(subjects, Subject::ALL.into_iter().collect(), "{cfg:?}");
        // no two routes share a claim of one subject
        let slots: BTreeSet<(Subject, usize)> = sc.iter().map(|s| (s.subject, s.slot)).collect();
        assert_eq!(slots.len(), Route::ALL.len(), "{cfg:?}");
        assert!(sc.iter().all(|s| s.slot < SUBJECT_CLAIMS), "{cfg:?}");
        // the scenarios are a pure function of the config
        assert_eq!(sc, m3_scenarios(&cfg), "{cfg:?}");
    }
    // the compact route belongs to writer 0, which holds the compact act
    for cfg in both_tiers() {
        let real = m3_scenarios(&cfg)
            .into_iter()
            .find(|s| s.route == Route::RealCompact)
            .expect("the compact route");
        assert_eq!(real.agent, 0, "{cfg:?}");
    }
    // a script too short for the forced sequences holds no scenario
    let short = Config {
        seed: 3,
        agents: 4,
        steps: 24,
        kill_at: None,
    };
    assert!(m3_scenarios(&short).is_empty());
    // the scripts themselves keep their acts: the descriptors add no act and no person act
    assert_eq!(Act::ALL_NAMES.len(), 23);
    // the project seeds one subject each, with a distinct claim per route
    let p = project::build(&Config::fast(1));
    assert_eq!(p.seed.subjects.len(), Subject::ALL.len());
    let mut ids = BTreeSet::new();
    for subject in Subject::ALL {
        let seed = p.seed.subject(subject);
        assert_eq!(seed.subject, subject);
        assert_eq!(seed.claims.len(), SUBJECT_CLAIMS);
        for id in seed.claims.iter().chain([&seed.doc, &seed.section]) {
            assert!(!id.is_empty() && ids.insert(id.clone()), "{subject:?} {id}");
        }
    }
    assert_eq!(ids.len(), Subject::ALL.len() * (SUBJECT_CLAIMS + 2));
}

/// mg7 (G2): a scenario fits the run: after the kill, inside the steps, off the advice steps, on
/// a writer, with a reviewer two steps later where it needs one; and within the bounds.
#[test]
fn mg7_m3_scenarios_fit_both_tiers() {
    assert_eq!(M3_MAX_CALLS, 32);
    assert_eq!((M3_MAX_MESSAGES, M3_MAX_BYTES), (256, 1 << 20));
    for cfg in both_tiers() {
        let advice = advice_steps(&cfg).expect("an advice sequence");
        let reserved = [advice.claim, advice.review, advice.propose];
        let kill = cfg.kill_at.expect("a kill");
        let mut used = BTreeSet::new();
        for s in m3_scenarios(&cfg) {
            let at = format!("seed {} {:?}", cfg.seed, s.route);
            assert!(!cfg.is_reviewer(s.agent) && s.agent < cfg.agents, "{at}");
            assert!(
                s.step > kill && s.step + 1 < cfg.steps,
                "{at}: step {}",
                s.step
            );
            assert!(!reserved.contains(&s.step), "{at}");
            assert!(used.insert((s.agent, s.step)), "{at}: a taken step");
            assert_eq!(s.max_calls, M3_MAX_CALLS, "{at}");
            assert_eq!(s.person, s.route.needs_person(), "{at}");
            match s.partner {
                Some((reviewer, step)) => {
                    assert!(s.route.needs_partner(), "{at}");
                    assert!(cfg.is_reviewer(reviewer), "{at}");
                    assert_eq!(step, s.step + M3_PARTNER_GAP, "{at}");
                    assert!(step + 1 < cfg.steps && !reserved.contains(&step), "{at}");
                }
                None => assert!(!s.route.needs_partner(), "{at}"),
            }
            assert_eq!((s.subject, s.slot), s.route.subject(), "{at}");
        }
    }
    // the draw is its own: the agents' and the person's scripts are those of the seed alone
    let a = script(&Config::fast(9));
    let _ = m3_scenarios(&Config::fast(9));
    assert_eq!(format!("{a:?}"), format!("{:?}", script(&Config::fast(9))));
}

fn attempt(route: Route, act: &'static str) -> RouteAttempt {
    RouteAttempt::begin(route, act, "agent:a2", "r1", Via::Tool("rdy_answer")).expect("an act")
}

/// mg7 (G2): a begun attempt has no reply and is never journaled; an interruption, a refusal
/// and an acknowledgement are three states that map to three outcomes.
#[test]
fn mg7_journal_cannot_confuse_attempt_with_acknowledgement() {
    let begun = attempt(Route::RelayedAnswer, "m3_relay_answer")
        .observed("c-ABCDE", "P-x@1")
        .shown("a".repeat(64).as_str(), "approve")
        .counted(RouteCounts {
            pages: 2,
            requests: 1,
            answers: 1,
            ..RouteCounts::default()
        });
    assert!(begun.is_pending() && !begun.is_acknowledged() && !begun.is_interrupted());
    assert_eq!(begun.refusal(), None);
    assert!(!begun.is_allowed());
    assert!(
        Entry::from_route(4, begun.clone()).is_none(),
        "nothing before a reply"
    );
    // an act of another route is not an attempt of this one
    assert!(
        RouteAttempt::begin(Route::MovedCard, "m3_relay_answer", "a", "r", Via::Cli("x")).is_none()
    );
    // an empty acknowledgement is no acknowledgement
    assert!(begun.clone().acknowledged("", vec![]).is_none());

    let acked = begun
        .clone()
        .acknowledged("ack-1", vec!["a-1".into()])
        .expect("an acknowledgement");
    let refused = begun.clone().refused("E_PROPOSAL_MOVED");
    let lost = begun.clone().interrupted();
    assert!(acked.is_acknowledged() && !refused.is_acknowledged() && !lost.is_acknowledged());
    assert_eq!(refused.refusal(), Some("E_PROPOSAL_MOVED"));
    assert_eq!(lost.refusal(), None);
    assert_ne!(acked.reply, refused.reply);
    assert_ne!(refused.reply, lost.reply);
    assert_ne!(acked.reply, lost.reply);
    assert!(acked.is_allowed() && refused.is_allowed() && lost.is_allowed());
    assert!(!begun.refused("E_INTERNAL").is_allowed());

    let ok = Entry::from_route(4, acked).expect("a journaled attempt");
    assert_eq!(ok.outcome, Outcome::Ok);
    let no = Entry::from_route(4, refused).expect("a journaled refusal");
    assert_eq!(no.outcome, Outcome::Refused("E_PROPOSAL_MOVED".into()));
    let gone = Entry::from_route(4, lost).expect("a journaled interruption");
    assert_eq!(gone.outcome, Outcome::Lost);
    // the entry keeps every fact the attempt had
    let a = ok.route.expect("the attempt");
    assert_eq!((a.actor.as_str(), a.run.as_str()), ("agent:a2", "r1"));
    assert_eq!(a.via, Via::Tool("rdy_answer"));
    assert_eq!(a.observed_ref.as_deref(), Some("c-ABCDE"));
    assert_eq!(a.observed_version.as_deref(), Some("P-x@1"));
    assert_eq!(a.option.as_deref(), Some("approve"));
    assert_eq!(
        (a.counts.pages, a.counts.requests, a.counts.answers),
        (2, 1, 1)
    );
    assert_eq!(
        a.reply,
        Reply::Acknowledged {
            ack: "ack-1".into(),
            records: vec!["a-1".into()]
        }
    );
    assert_eq!(ok.act, "m3_relay_answer");
}

/// mg7 (G2): each new act has its own closed refusal set; any other code, and any act the table
/// does not know, is refused.
#[test]
fn mg7_outcomes_are_closed_for_each_new_act() {
    let sets: [(&str, &[&str]); 13] = [
        ("m3_relay_answer", &["E_PROPOSAL_MOVED", "E_ITEM_MOVED"]),
        ("m3_wrong_role", &["E_PERMISSION"]),
        ("m3_right_role", &["E_PROPOSAL_MOVED"]),
        ("m3_elicit_first_rev", &["E_PROPOSAL_MOVED"]),
        ("m3_elicit_second_rev", &["E_PROPOSAL_MOVED"]),
        ("m3_elicit_decline", &[]),
        ("m3_relay_after_decline", &["E_PROPOSAL_MOVED"]),
        ("m3_moved_card", &["E_PROPOSAL_MOVED"]),
        ("m3_publication_check", &["E_PUBLICATION_REFERENCE"]),
        ("m3_publication_output", &["E_PUBLICATION_REFERENCE"]),
        (
            "m3_blob_remove",
            &[
                "E_REDACTION_HUMAN",
                "E_REDACTION_PENDING",
                "E_REDACTION_STALE",
            ],
        ),
        ("m3_compact_real", &[]),
        ("m3_unlisted", &[]),
    ];
    let probe = [
        "E_PROPOSAL_MOVED",
        "E_ITEM_MOVED",
        "E_PERMISSION",
        "E_PUBLICATION_REFERENCE",
        "E_REDACTION_HUMAN",
        "E_REDACTION_PENDING",
        "E_REDACTION_STALE",
        "E_INTERNAL",
        "E_BAD_INPUT",
        "E_STALE_BASE",
        "E_LIMIT",
        "E_DAEMON_GONE",
        "E_ELICITATION_UNAVAILABLE",
    ];
    for (act, codes) in sets {
        for code in probe {
            assert_eq!(allowed(act, code), codes.contains(&code), "{act} {code}");
        }
    }
    // the sets cover exactly the acts the routes name
    let named: BTreeSet<&str> = Route::ALL.iter().flat_map(|r| r.acts()).copied().collect();
    let tabled: BTreeSet<&str> = sets
        .iter()
        .map(|(a, _)| *a)
        .filter(|a| *a != "m3_unlisted")
        .collect();
    assert_eq!(named, tabled);
    // an attempt is allowed a refusal only by its own act's set
    let wrong = attempt(Route::WrongThenRightRole, "m3_wrong_role");
    assert!(wrong.clone().refused("E_PERMISSION").is_allowed());
    assert!(!wrong.refused("E_PROPOSAL_MOVED").is_allowed());
    // the earlier tables are unchanged
    assert!(allowed("ui_answer", "E_PROPOSAL_MOVED") && !allowed("ui_answer", "E_PERMISSION"));
}
