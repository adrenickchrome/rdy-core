//! ap7 (D-ZWP7Q, O26; TASK-545): the adapter's `Snapshot::disputes` lists each dispute of the
//! landings it is given, with its resolution, in input order and oldest first within a landing.
//! The project is built with core calls and the store's own writers in a closed store first.

use rdy_adapter::history::{Dispute, DisputeFacts, DisputeOutcome, DisputeResolution};
use rdy_adapter::{Hash, StoreHandle};
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest};
use rdy_model::{Provenance, Role};
use serde_json::{Value, json};

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

fn person(at_ms: i64) -> Provenance {
    Provenance {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
        at_ms,
    }
}

fn write(r: &mut Rdy, name: &str, ops: Value) {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: name.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "ap7".into(),
        },
    )
    .unwrap();
}

/// Lands one doc `name`; returns the landing.
fn land(r: &mut Rdy, name: &str) -> Hash {
    write(
        r,
        name,
        json!([{"op": "create", "type": "doc", "fields": {"name": name, "title": name, "doc_type": "page"}}]),
    );
    let p = r
        .propose(&session(Role::Writer), name, name, Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id)
        .unwrap()
        .parse()
        .unwrap()
}

fn dispute(landing: Hash, reason: &str, at_ms: i64) -> Dispute {
    Dispute {
        landing,
        opened_by: person(at_ms),
        reason: reason.into(),
        at_ms,
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    /// Three landings: two with disputes, one with none.
    l1: Hash,
    l2: Hash,
    l3: Hash,
    /// `l1`'s disputes, oldest first, and the resolution of the first.
    l1_disputes: [Hash; 2],
    l1_resolution: Hash,
    l2_dispute: Hash,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let (l1, l2, l3) = (
        land(&mut r, "one"),
        land(&mut r, "two"),
        land(&mut r, "three"),
    );
    // `l1`'s younger dispute is written first, so log order and age differ
    let younger = r
        .store_mut()
        .put_dispute(&dispute(l1, "younger", 20), None)
        .unwrap()
        .0;
    let older = r
        .store_mut()
        .put_dispute(&dispute(l1, "older", 10), None)
        .unwrap()
        .0;
    let (l1_resolution, _) = r
        .store_mut()
        .put_dispute_resolution(
            &DisputeResolution {
                dispute: older,
                outcome: DisputeOutcome::Upheld,
                by: person(30),
                correction: Some(l3),
            },
            None,
        )
        .unwrap();
    let l2_dispute = r
        .store_mut()
        .put_dispute(&dispute(l2, "only one", 15), None)
        .unwrap()
        .0;
    r.seal_open().unwrap();
    r.close().unwrap();
    Fixture {
        dir,
        l1,
        l2,
        l3,
        l1_disputes: [older, younger],
        l1_resolution,
        l2_dispute,
    }
}

#[test]
fn ap7_snapshot_disputes_lists_each_dispute_with_its_resolution() {
    let f = fixture();
    let handle = StoreHandle::open(f.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    // input order, then oldest first within a landing; a landing with no dispute gives no row,
    // and a repeated landing counts once
    let rows: Vec<DisputeFacts> = snap.disputes(&[f.l2, f.l3, f.l1, f.l2]).unwrap();
    assert_eq!(
        rows.iter().map(|d| d.hash).collect::<Vec<_>>(),
        [f.l2_dispute, f.l1_disputes[0], f.l1_disputes[1]]
    );
    assert_eq!(rows[0].record.landing, f.l2);
    assert_eq!(rows[0].record.reason, "only one");
    assert!(rows[0].resolution.is_none());
    // the older dispute of `l1` is resolved; the younger is open
    let (rid, res) = rows[1].resolution.clone().expect("a resolution");
    assert_eq!(rid, f.l1_resolution);
    assert_eq!(res.dispute, f.l1_disputes[0]);
    assert_eq!(res.outcome, DisputeOutcome::Upheld);
    assert_eq!(res.correction, Some(f.l3));
    assert_eq!(rows[1].record.reason, "older");
    assert_eq!(rows[2].record.reason, "younger");
    assert!(rows[2].resolution.is_none());
    // no landings, or none with a dispute, give nothing
    assert!(snap.disputes(&[]).unwrap().is_empty());
    assert!(snap.disputes(&[f.l3]).unwrap().is_empty());
    handle.close().unwrap();
}

#[test]
fn ap7_snapshot_disputes_refuses_more_than_max_keys() {
    let f = fixture();
    let handle = StoreHandle::open(f.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    // the input counts before its repeats collapse (as `landings` does)
    let many = vec![f.l3; 100_001];
    let e = snap.disputes(&many).unwrap_err();
    assert_eq!(e.code, "E_LIMIT", "{e:?}");
    assert_eq!(e.message, "landing facts query exceeds 100000 hashes");
    // the limit itself is read
    assert!(snap.disputes(&many[..100_000]).unwrap().is_empty());
    handle.close().unwrap();
}
