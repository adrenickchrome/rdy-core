//! Atomic landing through the engine (plan M2a1, atom1): a kill before the marker leaves no trace
//! of the landing; a kill after the sync leaves all of it.
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use rdy_store::segment::CrashPoint;
use serde_json::json;
use std::path::Path;

fn child(test: &str, dir: &Path, point: &str) -> std::process::ExitStatus {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env("RDY_A1_CHILD", dir)
        .env("RDY_A1_POINT", point)
        .status()
        .unwrap()
}

/// A project whose work view holds a proposed decision; returns the proposal id.
fn proposal_with_a_decision(dir: &Path) -> String {
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Log", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}}
    ]))
    .unwrap();
    r.decide(
        &writer(),
        "work",
        DecisionInput {
            parent: res.created["$s"].clone(),
            statement: "A".into(),
            ..Default::default()
        },
    )
    .unwrap();
    r.propose(&writer(), "work", "with a decision").unwrap().id
}

const NAME: &str = "atom1_a_kill_during_a_landing_leaves_all_or_nothing";

/// Review Focus 4.
#[test]
fn atom1_a_kill_during_a_landing_leaves_all_or_nothing() {
    if let Ok(dir) = std::env::var("RDY_A1_CHILD") {
        let point = CrashPoint::parse(&std::env::var("RDY_A1_POINT").unwrap()).unwrap();
        let mut r = Rdy::open(Path::new(&dir)).unwrap();
        let pid = r.store().refs_with_prefix("test/pid").unwrap()[0].1.clone();
        r.store_mut().crash_at(point);
        let _ = r.land(&approver(), std::str::from_utf8(&pid).unwrap());
        unreachable!("the crash point aborts the process");
    }
    for (point, lands) in [
        (CrashPoint::BeforeMarker, false),
        (CrashPoint::BeforeSync, true),
        (CrashPoint::AfterSync, true),
        (CrashPoint::AfterIndex, true),
    ] {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("p");
        std::fs::create_dir_all(&dir).unwrap();
        let pid = proposal_with_a_decision(&dir);
        let (canon, head, n) = {
            let mut r = Rdy::open(&dir).unwrap();
            r.store_mut()
                .set_refs(vec![("test/pid".into(), pid.clone().into_bytes())])
                .unwrap();
            (
                r.store().canon_head().unwrap(),
                r.store().view_head("work").unwrap(),
                r.store().view("work").unwrap().changes.len(),
            )
        };
        assert!(!child(NAME, &dir, point.name()).success(), "{point:?}");
        let mut r = Rdy::open(&dir).unwrap();
        r.fsck().unwrap();
        let moved = r.store().canon_head().unwrap() != canon;
        assert_eq!(moved, lands, "{point:?}");
        if lands {
            let l = r.store().canon_head().unwrap().unwrap();
            let rec = r.store().landing(&l).unwrap();
            for c in &rec.changes {
                assert_eq!(r.store().landing_of(c).unwrap(), Some(l), "{point:?}");
            }
            assert_eq!(r.store().view("work").unwrap().base, Some(l));
            assert!(r.store().view("work").unwrap().changes.is_empty());
        } else {
            assert_eq!(r.store().view_head("work").unwrap(), head, "{point:?}");
            assert_eq!(r.store().view("work").unwrap().changes.len(), n);
            r.land(&approver(), &pid).unwrap();
        }
        check(&mut r, &Scope::Canon).unwrap();
    }
}
