//! sg3: a swarm script is a pure function of its seed; it covers every act; outcomes are a closed table.
use rdy_qa::swarm::journal::{Outcome, allowed};
use rdy_qa::swarm::script::{Act, Config, PersonAct, script};
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
    assert!(allowed("land", "E_BLOCKING_TENSION"));
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
/// creates its claim early and proposes after it; the second member (`.1`) creates its claim in
/// the last quarter, so the first member's landing can come before it. Agents 0 and 1 (one
/// alternative set) are not paired while other writers exist.
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
                c2 >= cfg.steps - cfg.steps / 4,
                "{ctx}: the second claim comes late ({c2})"
            );
            let proposes = at(*first, &|a| matches!(a, Act::Propose));
            assert!(
                proposes.iter().any(|&p| c1 < p && p < c2),
                "{ctx}: the first member proposes between the two claims: {proposes:?}"
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
