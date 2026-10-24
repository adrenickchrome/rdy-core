//! sg3: a swarm script is a pure function of its seed; it covers every act; outcomes are a closed table.
use rdy_qa::swarm::journal::{Outcome, allowed};
use rdy_qa::swarm::script::{Act, Config, PersonAct, direct_land_at, script};
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
        let p: BTreeSet<_> = s.person.iter().map(PersonAct::name).collect();
        for need in [
            "land",
            "dismiss",
            "accept_advice",
            "wc_edit",
            "inbox_ack",
            "tensions",
        ] {
            assert!(p.contains(need), "seed {seed}: the person lacks {need}");
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
    assert!(allowed("write_no_brief", "E_BRIEF_REQUIRED"));
    assert!(!allowed("write_no_brief", "E_STALE_BASE"));
    assert!(!allowed("write", "E_INTERNAL"));
    assert!(!allowed("tensions", "E_BAD_INPUT"));
    assert!(!allowed("no_such_act", "E_NOT_FOUND"));
    assert!(allowed("write", "E_BRIEF_STALE"));
    assert!(allowed("review_submit", "E_REVIEW_STALE"));
    assert!(allowed("inbox_resume", "E_BAD_INPUT"));
    assert!(!allowed("inbox", "E_BAD_INPUT"));
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
        assert_eq!(names.len(), 6, "seed {}: {names:?}", cfg.seed);
    }
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
                        Act::Include { from } => {
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
                let Act::Include { from } = a else {
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
                        Act::Include { from } => *from,
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
