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
