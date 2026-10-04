//! Flat (M1-shaped) projects: name checks must not be O(touched × siblings).
//! The bounded tests run in debug builds; the ignored one measures in release:
//! cargo test -p rdy-engine --release --test perf_flat -- --ignored --nocapture
use rdy_engine::*;
use rdy_model::*;
use serde_json::json;
use std::time::{Duration, Instant};

fn agent() -> Session {
    Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
    }
}
fn founder() -> Session {
    Session {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
    }
}

/// One write creating `n` top-level docs (named by rdy from their titles), then propose and land.
fn flat(n: usize) -> (Duration, Duration, Duration) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let ops: Vec<_> = (0..n)
        .map(|i| json!({"op": "create", "type": "doc", "fields": {"title": format!("Doc {i}"), "doc_type": "overview"}}))
        .collect();
    let t = Instant::now();
    r.write(
        &agent(),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(json!(ops)).unwrap(),
            justification: None,
            message: "many".into(),
        },
    )
    .unwrap();
    let write = t.elapsed();
    let t = Instant::now();
    let p = r.propose(&agent(), "work", "many").unwrap();
    let propose = t.elapsed();
    let t = Instant::now();
    r.land(&founder(), &p.id).unwrap();
    let land = t.elapsed();
    (write, propose, land)
}

/// `n` docs from M1 (no stored name) committed straight to the store, then proposed; then
/// `lookups` path lookups at the top level. Returns (propose, all lookups).
fn nameless(n: usize, lookups: usize) -> (Duration, Duration) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let mut ids = std::collections::BTreeSet::new();
    while ids.len() < n {
        ids.insert(BlockId::new("d"));
    }
    let ops: Vec<Op> = ids
        .into_iter()
        .enumerate()
        .map(|(i, id)| Op::Create {
            block: Block {
                id,
                block_type: "doc".into(),
                parent: None,
                // valid keys (no trailing 0), as M1 produced
                order: format!("V{i:06}1"),
                fields: [
                    ("title".to_string(), FieldValue::Str(format!("Legacy {i}"))),
                    ("doc_type".to_string(), FieldValue::Str("overview".into())),
                ]
                .into_iter()
                .collect(),
                body: String::new(),
                links: Default::default(),
                pins: Default::default(),
            },
        })
        .collect();
    r.store_mut()
        .commit_to_view(
            "work",
            Change {
                view: "work".into(),
                ops,
                deps: vec![],
                provenance: agent().provenance(),
                justification: None,
                message: "raw".into(),
            },
        )
        .unwrap();
    r.invalidate();
    let t = Instant::now();
    r.propose(&agent(), "work", "legacy").unwrap();
    let propose = t.elapsed();
    let work = Scope::View("work".into());
    r.materialized(&work).unwrap();
    let t = Instant::now();
    for i in 0..lookups {
        let p = format!("legacy-{}", (i * 7) % n);
        r.resolve_path(&work, &p).unwrap();
    }
    (propose, t.elapsed())
}

#[test]
fn flat_two_thousand_docs_write_propose_land_is_fast() {
    let (write, propose, land) = flat(2000);
    println!("flat 2000: write {write:?}, propose {propose:?}, land {land:?}");
    assert!(
        write + propose + land < Duration::from_secs(5),
        "write {write:?} propose {propose:?} land {land:?}"
    );
}

#[test]
fn nameless_legacy_docs_propose_and_lookup_are_fast() {
    let (propose, lookups) = nameless(1000, 200);
    println!("nameless 1000: propose {propose:?}, 200 lookups {lookups:?}");
    assert!(propose < Duration::from_secs(1), "propose {propose:?}");
    assert!(lookups < Duration::from_millis(100), "lookups {lookups:?}");
}

#[test]
#[ignore]
fn flat_timings_release() {
    for n in [1000, 4000, 8000] {
        let (write, propose, land) = flat(n);
        println!("flat {n}: write {write:?}, propose {propose:?}, land {land:?}");
    }
    for n in [1000, 4000] {
        let (propose, lookups) = nameless(n, 100);
        println!(
            "nameless {n}: propose {propose:?}, one top-level lookup {:?}",
            lookups / 100
        );
    }
}
