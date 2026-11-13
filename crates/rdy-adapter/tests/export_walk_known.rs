//! The export walk through the adapter's uploader path (`Snapshot::write_stream`) charges only
//! what the upload holds (TASK-2329; p-VCW8D: "One batch, and one upload, hold at most ...
//! 1,000,000 object records and 1,000,000 dependency edges"). A long history whose snapshots
//! carry more than 1,000,000 edges in all no longer refuses an incremental upload that sends one
//! landing; the full export of the same tag is still over the bound, with core's text.
//!
//! Each landing's snapshot links to every block, so [`BLOCKS`] blocks and [`LANDINGS`] landings
//! carry more than 1,000,000 edges. The project is built with core calls in a closed store
//! first, then read through the adapter. The fixture takes about a minute in a debug build, so
//! the test runs on demand (`--ignored`); `rdy-engine/tests/export_walk_known.rs` pins the same
//! charging with small bounds on every run.

use rdy_adapter::stream::ObjectKey;
use rdy_adapter::{Hash, StoreHandle};
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest};
use rdy_model::Role;
use rdy_store::closure::{self, END_OF_LOG, WalkBounds};
use rdy_store::sync_stream::MAX_EDGES;
use serde_json::{Value, json};
use std::collections::BTreeSet;

const BLOCKS: usize = 10_000;
/// Landings after the first, before `v1.0.0`: with the first, their snapshots alone carry more
/// than [`MAX_EDGES`] links.
const LANDINGS: usize = 100;
const PER_DOC: usize = 500;

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

fn write(r: &mut Rdy, ops: Value) -> rdy_engine::WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "xwk".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy) {
    let p = r
        .propose(&session(Role::Writer), "work", "edit", Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
}

fn tag_hash(r: &Rdy, name: &str) -> Hash {
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1
}

#[test]
#[ignore = "about 60 s in a debug build (101 landings of 10,000 blocks): run with --ignored"]
fn xwk_adapter_incremental_upload_past_a_million_known_edges() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let mut ops = vec![];
    for d in 0..BLOCKS / PER_DOC {
        ops.push(
            json!({"op": "create", "type": "doc", "id": format!("$d{d}"),
            "fields": {"name": format!("d{d}"), "title": "D", "doc_type": "page"}}),
        );
    }
    for i in 0..BLOCKS {
        ops.push(
            json!({"op": "create", "type": "prose", "id": format!("$p{i}"),
            "parent": format!("$d{}", i / PER_DOC), "body": format!("Block {i}.")}),
        );
    }
    let res = write(&mut r, json!(ops));
    land(&mut r);
    for n in 0..=LANDINGS {
        if n == LANDINGS {
            r.tag(&session(Role::Approver), "v1.0.0", None, None)
                .unwrap();
        }
        let id = res.created[&format!("$p{n}")].clone();
        write(
            &mut r,
            json!([{"op": "update", "id": id, "body": format!("Edit {n}.")}]),
        );
        land(&mut r);
    }
    r.tag(&session(Role::Approver), "v2.0.0", None, None)
        .unwrap();
    // what the remote holds: v1.0.0's history, which it took over several uploads (one upload
    // of it whole is over the edge bound)
    let (v1, v2) = (tag_hash(&r, "v1.0.0"), tag_hash(&r, "v2.0.0"));
    let reader = r.store().reader();
    let unbounded = WalkBounds {
        records: u64::MAX,
        edges: u64::MAX,
    };
    let held =
        closure::closure_within(&reader, v1, &BTreeSet::new(), END_OF_LOG, unbounded).unwrap();
    let known: BTreeSet<ObjectKey> = held.records.into_iter().collect();
    let want = closure::closure_within(&reader, v2, &known, END_OF_LOG, unbounded).unwrap();
    // the walk of the incremental upload keeps the links of its emitted records only; a known
    // record's links are released once reached (before, every record walked kept them)
    let kept = closure::closure_kept_links_for_tests(&reader, v2, &known, END_OF_LOG).unwrap();
    let walked =
        closure::closure_kept_links_for_tests(&reader, v2, &BTreeSet::new(), END_OF_LOG).unwrap();
    eprintln!("links kept: {kept} with v1.0.0 known, {walked} with nothing known");
    assert!(kept < 20_000, "{kept} links kept");
    assert!(walked > MAX_EDGES, "{walked} links walked");
    drop(reader);
    r.close().unwrap();

    let handle = StoreHandle::open(dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    // the full export walks every snapshot's links: over the bound, with core's text
    let e = snap
        .write_stream("v2.0.0", &BTreeSet::new(), std::io::sink())
        .unwrap_err();
    assert_eq!(e.code, "E_LIMIT");
    assert_eq!(e.message, format!("stream exceeds {MAX_EDGES} graph edges"));
    // the incremental upload sends one landing's records: accepted, the records core's walk
    // finds with no bound, in the same order, with the same prerequisites
    let mut out = Vec::new();
    let report = snap.write_stream("v2.0.0", &known, &mut out).unwrap();
    assert_eq!(report.keys, want.records);
    assert_eq!(report.header.prerequisites, want.prerequisites);
    assert!(report.keys.len() < 50, "{} records", report.keys.len());
    assert!(report.keys.iter().all(|k| !known.contains(k)));
    assert_eq!(
        rdy_adapter::stream::check_batch(out.as_slice(), None).unwrap(),
        report
    );
}
