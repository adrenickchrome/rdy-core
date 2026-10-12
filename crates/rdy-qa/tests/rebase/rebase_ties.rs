//! sg2: order keys under rebase (spec §15.3 `rebase`, D-8EH6E). Views that append under one
//! parent from one base can end with equal order keys once they land; a tie is harmless when the
//! order it gives is stable and a later insert still lands where it was asked. Every state
//! passes the invariant checker, on canon and on each view.
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed, TestRunner};
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use serde_json::json;

fn canon() -> Scope {
    Scope::Canon
}

/// Canon and each view in `views` pass the checker.
fn all_valid(r: &Rdy, views: &[&str], when: &str) {
    if let Err(v) = check(r, &canon()) {
        panic!("canon broke {when}: {v:?}");
    }
    for view in views {
        if let Err(v) = check(r, &Scope::View((*view).into())) {
            panic!("view {view} broke {when}: {v:?}");
        }
    }
}

fn append(r: &mut Rdy, view: &str, section: &str, body: &str) -> String {
    let w = write_as(
        r,
        &as_agent(&format!("agent:{view}"), "r"),
        view,
        json!([{"op": "create", "type": "claim", "id": "$n", "parent": section, "body": body}]),
    )
    .expect("append");
    w.created["$n"].clone()
}

/// A project whose section holds one claim, "base"; returns it with the section and the claim.
fn seeded() -> (tempfile::TempDir, Rdy, String, String) {
    let (d, mut r) = project();
    let (_doc, section, claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").expect("land the seed");
    (d, r, section, claim)
}

#[test]
fn sg2_two_views_appending_under_one_parent_land_with_a_tie_or_distinct_keys() {
    let (_d, mut r, section, base) = seeded();
    let a = append(&mut r, "a", &section, "from a");
    let b = append(&mut r, "b", &section, "from b");
    all_valid(&r, &["a", "b"], "before any landing");
    land(&mut r, "a").expect("a lands");
    all_valid(&r, &["b"], "after a landed");
    land(&mut r, "b").expect("b lands after its rebase");
    all_valid(&r, &[], "after b landed");
    let section_id = BlockId::parse(&section).unwrap();
    let st = r.read();
    let m = st.materialized(&canon()).unwrap();
    let ids: Vec<String> = m
        .children(Some(&section_id))
        .iter()
        .map(|c| c.id.to_string())
        .collect();
    assert_eq!(ids.len(), 3, "canon lists all three claims: {ids:?}");
    for id in [&base, &a, &b] {
        assert!(ids.contains(id), "{id} is missing from {ids:?}");
    }
    assert_eq!(ids[0], base, "the seeded claim stays first: {ids:?}");
}

#[test]
fn sg2_a_tie_then_an_insert_between_stays_valid() {
    let (_d, mut r, section, base) = seeded();
    let a = append(&mut r, "a", &section, "from a");
    let b = append(&mut r, "b", &section, "from b");
    land(&mut r, "a").unwrap();
    land(&mut r, "b").unwrap();
    let section_id = BlockId::parse(&section).unwrap();
    let order_of = |r: &Rdy| -> Vec<String> {
        let st = r.read();
        let m = st.materialized(&canon()).unwrap();
        m.children(Some(&section_id))
            .iter()
            .map(|c| c.id.to_string())
            .collect()
    };
    let before = order_of(&r);
    assert_eq!(before.len(), 3, "{before:?}");
    let first_tied = before[1].clone();
    assert!(
        first_tied == a || first_tied == b,
        "the claim after the seeded one is a tied append: {before:?}"
    );
    let w = write_as(
        &mut r,
        &as_agent("agent:c", "r"),
        "c",
        json!([{"op": "create", "type": "claim", "id": "$n", "parent": section, "after": first_tied, "body": "between"}]),
    )
    .expect("insert after the first tied claim");
    let between = w.created["$n"].clone();
    all_valid(&r, &["c"], "after the insert in view c");
    land(&mut r, "c").expect("c lands");
    all_valid(&r, &[], "after the insert landed");
    let after = order_of(&r);
    assert_eq!(after.len(), 4, "{after:?}");
    // the insert is after the claim it names; the claims that were there keep their order
    let at = after.iter().position(|id| *id == between).unwrap();
    let place = after.iter().position(|id| *id == first_tied).unwrap();
    assert!(at > place, "the insert follows {first_tied}: {after:?}");
    let others: Vec<String> = after.iter().filter(|id| **id != between).cloned().collect();
    assert_eq!(others, before, "nothing else moved (base {base})");
    assert_eq!(order_of(&r), after, "the order is stable");
}

#[test]
fn sg2_rebase_preserves_relative_order_of_many_appends() {
    let config = Config {
        cases: cases(16),
        rng_seed: RngSeed::Fixed(0x71e5_0b17),
        failure_persistence: None,
        max_shrink_iters: 256,
        ..Config::default()
    };
    let mut runner = TestRunner::new(config);
    // each view appends 1 to 5 claims; the landing order is a shuffle of the views
    let strategy = proptest::collection::vec(1usize..=5, 2..=4).prop_flat_map(|counts| {
        let n = counts.len();
        (
            Just(counts),
            Just((0..n).collect::<Vec<_>>()).prop_shuffle(),
        )
    });
    let result = runner.run(&strategy, |(counts, order)| {
        rdy_model::seed_ids_for_tests(0x71e5_0000 + counts.iter().sum::<usize>() as u64);
        let outcome = std::panic::catch_unwind(|| {
            let (_d, mut r, section, _base) = seeded();
            let names: Vec<String> = (0..counts.len()).map(|i| format!("v{i}")).collect();
            let mut own: Vec<Vec<String>> = vec![];
            for (i, &n) in counts.iter().enumerate() {
                own.push(
                    (0..n)
                        .map(|j| append(&mut r, &names[i], &section, &format!("v{i} claim {j}")))
                        .collect(),
                );
            }
            let section_id = BlockId::parse(&section).unwrap();
            for &i in &order {
                land(&mut r, &names[i]).unwrap_or_else(|e| panic!("landing {}: {e:?}", names[i]));
                let views: Vec<&str> = names.iter().map(String::as_str).collect();
                all_valid(&r, &views, &format!("after landing {}", names[i]));
            }
            let st = r.read();
            let m = st.materialized(&canon()).unwrap();
            let ids: Vec<String> = m
                .children(Some(&section_id))
                .iter()
                .map(|c| c.id.to_string())
                .collect();
            assert_eq!(ids.len(), 1 + counts.iter().sum::<usize>(), "{ids:?}");
            for (i, mine) in own.iter().enumerate() {
                let seen: Vec<&String> = ids.iter().filter(|id| mine.contains(id)).collect();
                let want: Vec<&String> = mine.iter().collect();
                assert_eq!(
                    seen, want,
                    "view v{i}'s appends changed their relative order (landing order {order:?})"
                );
            }
        });
        rdy_model::unseed_ids_for_tests();
        outcome.map_err(|e| {
            let text = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "a panic".into());
            TestCaseError::fail(text)
        })
    });
    if let Err(e) = result {
        panic!("{e}");
    }
}
