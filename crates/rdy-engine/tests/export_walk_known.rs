//! The export walk charges only what the upload holds (TASK-2329; p-VCW8D: "One batch, and one
//! upload, hold at most 2,147,483,648 bytes, 1,000,000 object records and 1,000,000 dependency
//! edges"). A record the receiver claims to hold (`known`) and the edges it carries charge
//! nothing; an emitted record and every edge it carries, an edge to a known record too, charge
//! the bounds. With nothing known, everything the walk finds is charged, as before.
//!
//! The source has one doc of many prose blocks and a long history: each landing changes one
//! block, so each landing's snapshot links to every block. `v1` is tagged after most of it; `v2`
//! a few landings later. The receiver of the incremental upload holds `v1`'s closure.
mod common;

use rdy_engine::*;
use rdy_model::*;
use rdy_store::closure::{self, Closure, END_OF_LOG, WalkBounds};
use rdy_store::object_refs::{ObjectKey, refs_of};
use rdy_store::stream_error::StreamError;
use rdy_store::sync_stream::{MAX_EDGES, MAX_RECORDS, SYNC_KINDS};
use serde_json::json;
use std::collections::BTreeSet;

const BLOCKS: usize = 24;
const OLD_LANDINGS: usize = 8;
const NEW_LANDINGS: usize = 2;

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn write(r: &mut Rdy, ops: serde_json::Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "write to work".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy) {
    let p = r
        .propose(&writer(), "work", "batch", Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
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

/// The source: [`BLOCKS`] prose blocks landed, then [`OLD_LANDINGS`] landings of one block each
/// and tag `v1`, then [`NEW_LANDINGS`] more and tag `v2`. Returns the project and both tags.
fn source() -> (tempfile::TempDir, Rdy, Hash, Hash) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let mut ops = vec![json!({"op": "create", "type": "doc", "id": "$d",
        "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}})];
    for i in 0..BLOCKS {
        ops.push(
            json!({"op": "create", "type": "prose", "id": format!("$p{i}"), "parent": "$d",
            "body": format!("Block {i}.")}),
        );
    }
    let res = write(&mut r, json!(ops));
    land(&mut r);
    let ids: Vec<String> = (0..BLOCKS)
        .map(|i| res.created[&format!("$p{i}")].clone())
        .collect();
    let edit = |r: &mut Rdy, n: usize| {
        let id = &ids[n % BLOCKS];
        write(
            r,
            json!([{"op": "update", "id": id, "body": format!("Block edit {n}.")}]),
        );
        land(r);
    };
    for n in 0..OLD_LANDINGS {
        edit(&mut r, n);
    }
    r.tag(&founder(), "v1.0.0", None, None).unwrap();
    for n in OLD_LANDINGS..OLD_LANDINGS + NEW_LANDINGS {
        edit(&mut r, n);
    }
    r.tag(&founder(), "v2.0.0", None, None).unwrap();
    let (v1, v2) = (tag_hash(&r, "v1.0.0"), tag_hash(&r, "v2.0.0"));
    (dir, r, v1, v2)
}

/// The closure of `tag` less `known`, walked within `bounds`.
fn walk(
    r: &Rdy,
    tag: Hash,
    known: &BTreeSet<ObjectKey>,
    bounds: WalkBounds,
) -> Result<Closure, StreamError> {
    closure::closure_within(&r.store().reader(), tag, known, END_OF_LOG, bounds)
}

fn bounds(records: u64, edges: u64) -> WalkBounds {
    WalkBounds { records, edges }
}

/// The keys stored record `k` links to, resolved as the walk resolves them, no repeats: a typed
/// link is its key; an untyped one is the one portable object of its hash (kind 0 when none).
fn links_of(r: &Rdy, k: ObjectKey) -> BTreeSet<ObjectKey> {
    let (raw, payload) = r.store().get_kind_raw(k.kind, &k.hash).unwrap();
    refs_of(raw, &payload)
        .unwrap()
        .into_iter()
        .map(|l| match l.kind {
            Some(kind) => ObjectKey { kind, hash: l.hash },
            None => {
                let held: Vec<u8> = SYNC_KINDS
                    .iter()
                    .copied()
                    .filter(|kind| r.store().get_kind_raw(*kind, &l.hash).is_ok())
                    .collect();
                assert!(held.len() <= 1, "an ambiguous link in the fixture");
                ObjectKey {
                    kind: held.first().copied().unwrap_or(0),
                    hash: l.hash,
                }
            }
        })
        .collect()
}

/// The dependency edges `records` carry, summed (no record of the fixture is a placeholder, so
/// none names a removal proof).
fn edges_of(r: &Rdy, records: &[ObjectKey]) -> u64 {
    records.iter().map(|k| links_of(r, *k).len() as u64).sum()
}

fn assert_limit(e: StreamError, label: &str) {
    assert_eq!(e.code, "E_LIMIT");
    assert_eq!(e.message, format!("stream exceeds {label}"));
    assert_eq!(e.fix, "reduce the stream to the stated limit");
}

/// The fixture's closures: v2 in full, and v2 less v1's closure (what the remote holds).
struct Fixture {
    _dir: tempfile::TempDir,
    r: Rdy,
    v2: Hash,
    known: BTreeSet<ObjectKey>,
    full: Closure,
    incremental: Closure,
}

fn fixture() -> Fixture {
    let (dir, r, v1, v2) = source();
    let reader = r.store().reader();
    let held = closure::closure(&reader, v1, &BTreeSet::new()).unwrap();
    let known: BTreeSet<ObjectKey> = held.records.into_iter().collect();
    let full = closure::closure(&reader, v2, &BTreeSet::new()).unwrap();
    let incremental = closure::closure(&reader, v2, &known).unwrap();
    drop(reader);
    Fixture {
        _dir: dir,
        r,
        v2,
        known,
        full,
        incremental,
    }
}

#[test]
fn xwk_upload_bounds_are_the_constants() {
    assert_eq!(
        WalkBounds::UPLOAD,
        bounds(MAX_RECORDS, MAX_EDGES),
        "the product passes p-VCW8D's bounds"
    );
    assert_eq!((MAX_RECORDS, MAX_EDGES), (1_000_000, 1_000_000));
}

/// An incremental export past a long known history whose edges, all summed, pass the bound,
/// while its emitted records and their edges fit: accepted, with the same records, order and
/// prerequisites as with the upload's bounds.
#[test]
fn xwk_incremental_past_known_history_is_accepted() {
    let f = fixture();
    let emitted = &f.incremental.records;
    let records = emitted.len() as u64;
    let edges = edges_of(&f.r, emitted);
    let all_edges = edges_of(&f.r, &f.full.records);
    // the history is long: the walk meets far more edges than the upload holds
    assert!(
        all_edges > 3 * edges,
        "{all_edges} edges walked, {edges} sent"
    );
    assert!(f.full.records.len() > 3 * emitted.len());
    // the incremental upload holds only what v1's closure lacks
    assert!(emitted.iter().all(|k| !f.known.contains(k)));
    let tight = walk(&f.r, f.v2, &f.known, bounds(records, edges)).unwrap();
    assert_eq!(
        tight, f.incremental,
        "the same records, order and prerequisites"
    );
    // the full export of the same tag is over those bounds: the known history was the cost
    assert_limit(
        walk(&f.r, f.v2, &BTreeSet::new(), bounds(u64::MAX, edges)).unwrap_err(),
        "1000000 graph edges",
    );
    assert_limit(
        walk(&f.r, f.v2, &BTreeSet::new(), bounds(records, u64::MAX)).unwrap_err(),
        "1000000 object records",
    );
}

/// The emitted records alone over the record or the edge bound: refused, with today's text.
#[test]
fn xwk_emitted_over_a_bound_is_refused() {
    let f = fixture();
    let emitted = &f.incremental.records;
    let records = emitted.len() as u64;
    let edges = edges_of(&f.r, emitted);
    assert_eq!(
        walk(&f.r, f.v2, &f.known, bounds(records, u64::MAX)).unwrap(),
        f.incremental
    );
    assert_limit(
        walk(&f.r, f.v2, &f.known, bounds(records - 1, u64::MAX)).unwrap_err(),
        "1000000 object records",
    );
    assert_eq!(
        walk(&f.r, f.v2, &f.known, bounds(u64::MAX, edges)).unwrap(),
        f.incremental
    );
    let e = walk(&f.r, f.v2, &f.known, bounds(u64::MAX, edges - 1)).unwrap_err();
    assert!(
        e.object.is_some_and(|k| emitted.contains(&k)),
        "names the emitted record whose edges pass the bound"
    );
    assert_limit(e, "1000000 graph edges");
}

/// An edge from an emitted record to a known one is a dependency edge the upload holds: it is
/// charged. Without those edges the emitted records fit one bound well below the exact count,
/// yet that bound is refused.
#[test]
fn xwk_edge_to_a_known_record_is_charged() {
    let f = fixture();
    let emitted = &f.incremental.records;
    let inside: BTreeSet<ObjectKey> = emitted.iter().copied().collect();
    let mut to_known = 0u64;
    let mut to_emitted = 0u64;
    for k in emitted {
        for c in links_of(&f.r, *k) {
            if inside.contains(&c) {
                to_emitted += 1;
            } else {
                assert!(f.known.contains(&c), "every other target is known: {c:?}");
                to_known += 1;
            }
        }
    }
    // each new snapshot links to the unchanged blocks, which v1's closure holds
    assert!(
        to_known >= (BLOCKS - NEW_LANDINGS) as u64,
        "{to_known} edges to known"
    );
    let exact = to_emitted + to_known;
    assert_eq!(exact, edges_of(&f.r, emitted));
    assert_eq!(
        walk(&f.r, f.v2, &f.known, bounds(u64::MAX, exact)).unwrap(),
        f.incremental,
        "at the exact count"
    );
    assert_limit(
        walk(&f.r, f.v2, &f.known, bounds(u64::MAX, exact - 1)).unwrap_err(),
        "1000000 graph edges",
    );
    assert_limit(
        walk(&f.r, f.v2, &f.known, bounds(u64::MAX, to_emitted)).unwrap_err(),
        "1000000 graph edges",
    );
}

/// A full export (nothing known) charges every record and edge the walk finds, as before.
#[test]
fn xwk_full_export_charges_everything() {
    let f = fixture();
    let none = BTreeSet::new();
    let records = f.full.records.len() as u64;
    let edges = edges_of(&f.r, &f.full.records);
    assert!(f.full.prerequisites.is_empty());
    let exact = walk(&f.r, f.v2, &none, bounds(records, edges)).unwrap();
    assert_eq!(exact, f.full);
    assert_eq!(
        walk(&f.r, f.v2, &none, WalkBounds::UPLOAD).unwrap(),
        f.full,
        "the product's bounds give closure()"
    );
    assert_limit(
        walk(&f.r, f.v2, &none, bounds(records - 1, edges)).unwrap_err(),
        "1000000 object records",
    );
    assert_limit(
        walk(&f.r, f.v2, &none, bounds(records, edges - 1)).unwrap_err(),
        "1000000 graph edges",
    );
}
