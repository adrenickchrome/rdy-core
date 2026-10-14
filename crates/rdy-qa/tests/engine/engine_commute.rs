//! sg2: spec §15.2, changes to different blocks commute. Two to six updates, each to its own
//! claim in its own view from one canon, land in any order and leave one canon: the same bodies
//! and the same snapshot of block versions. Each order runs on its own fresh project, with the
//! same seeded ids.
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed, TestRunner};
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use serde_json::json;
use std::collections::BTreeMap;

/// The claims of the seed doc.
const CLAIMS: usize = 6;

/// Seeds this thread's block ids for one run; unseeds them on drop, even if the run panics.
struct SeededIds;

impl SeededIds {
    fn new(key: u64) -> SeededIds {
        rdy_model::seed_ids_for_tests(key);
        SeededIds
    }
}

impl Drop for SeededIds {
    fn drop(&mut self) {
        rdy_model::unseed_ids_for_tests();
    }
}

/// What a run leaves: the bodies of the claims, in claim order, and the canon snapshot's block
/// map (block id to version).
type Outcome = (Vec<String>, BTreeMap<String, String>);

/// One run: a project with a doc of six claims, one view per edit written from the seed's canon,
/// then the views landed in `order` (indexes into `edits`). Every landing leaves a valid canon.
fn run(key: u64, edits: &[(usize, String)], order: &[usize]) -> Outcome {
    let _ids = SeededIds::new(key);
    let (_dir, mut r) = project();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "commute", "title": "Commute", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}}),
    ];
    for i in 0..CLAIMS {
        ops.push(json!({"op": "create", "type": "claim", "id": format!("$c{i}"), "parent": "$s", "body": format!("c{i}")}));
    }
    let seeded = write(&mut r, "seed", json!(ops)).expect("seed the doc");
    land(&mut r, "seed").expect("land the seed");
    let claims: Vec<String> = (0..CLAIMS)
        .map(|i| seeded.created[&format!("$c{i}")].clone())
        .collect();
    for (k, (claim, body)) in edits.iter().enumerate() {
        let view = format!("e{k}");
        write_as(
            &mut r,
            &as_agent(&format!("agent:e{k}"), "r"),
            &view,
            json!([{"op": "update", "id": claims[*claim], "body": body}]),
        )
        .unwrap_or_else(|e| panic!("edit {k} of claim {claim}: {e:?}"));
    }
    for &k in order {
        let view = format!("e{k}");
        land(&mut r, &view).unwrap_or_else(|e| panic!("landing {view} (order {order:?}): {e:?}"));
        if let Err(v) = check(&r, &Scope::Canon) {
            panic!("canon broke after landing {view} (order {order:?}): {v:?}");
        }
    }
    let bodies = claims
        .iter()
        .map(|c| body_of(&r, &Scope::Canon, c))
        .collect();
    let snapshot = r
        .read()
        .materialized(&Scope::Canon)
        .expect("canon")
        .snapshot()
        .blocks
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    (bodies, snapshot)
}

/// A subset of the claims in random order (the edits), and two different orders to land them
/// in: two shuffles, the second turned by one when it equals the first (final review M9, minors
/// item 5: equal shuffles made the check compare a run with itself).
fn cases_strategy() -> impl Strategy<Value = (Vec<usize>, Vec<usize>, Vec<usize>)> {
    proptest::sample::subsequence((0..CLAIMS).collect::<Vec<_>>(), 2..=CLAIMS)
        .prop_shuffle()
        .prop_flat_map(|claims| {
            let n = claims.len();
            let order = Just((0..n).collect::<Vec<_>>()).prop_shuffle();
            (Just(claims), order.clone(), order)
        })
        .prop_map(|(claims, first, mut second)| {
            if second == first {
                second.rotate_left(1);
            }
            (claims, first, second)
        })
}

/// The two orders of every case differ.
#[test]
fn sg2_the_two_landing_orders_always_differ() {
    let mut runner = TestRunner::new(Config {
        cases: 256,
        rng_seed: RngSeed::Fixed(0x5921_c034),
        failure_persistence: None,
        ..Config::default()
    });
    runner
        .run(&cases_strategy(), |(_, first, second)| {
            prop_assert_ne!(first, second);
            Ok(())
        })
        .unwrap();
}

#[test]
fn sg2_changes_to_different_blocks_commute() {
    let config = Config {
        cases: cases(64),
        rng_seed: RngSeed::Fixed(0x5921_c033),
        failure_persistence: None,
        max_shrink_iters: 512,
        ..Config::default()
    };
    let mut runner = TestRunner::new(config);
    let result = runner.run(&cases_strategy(), |(claims, first, second)| {
        let edits: Vec<(usize, String)> = claims
            .iter()
            .enumerate()
            .map(|(k, &c)| (c, format!("edited {c} by e{k}")))
            .collect();
        let key = 0x5a2c_0000 + edits.len() as u64;
        let (bodies_a, snap_a) = run(key, &edits, &first);
        let (bodies_b, snap_b) = run(key, &edits, &second);
        for c in 0..CLAIMS {
            let want = edits
                .iter()
                .find(|(claim, _)| *claim == c)
                .map(|(_, b)| b.clone())
                .unwrap_or_else(|| format!("c{c}"));
            prop_assert_eq!(&bodies_a[c], &want, "order {:?}, claim {}", first, c);
            prop_assert_eq!(&bodies_b[c], &want, "order {:?}, claim {}", second, c);
        }
        prop_assert_eq!(
            snap_a,
            snap_b,
            "the snapshots differ between orders {:?} and {:?}",
            first,
            second
        );
        Ok(())
    });
    if let Err(e) = result {
        panic!("{e}");
    }
}
