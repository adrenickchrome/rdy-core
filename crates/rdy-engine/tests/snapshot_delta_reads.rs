//! sdl2 (snapshot deltas, D-RP6SM): every state read of the engine goes through the store's one
//! map read (`rdy_store::snapshot`). On a history whose landings read their maps by folding
//! their deltas, each scope reads what it read from the held SNAPSHOTs: canon, a historical
//! landing, a tag, pinned and floating views, a fork, replay, undo, a view's plan, the gate,
//! consequences and tensions. A stream's records fold to the same state as the store, and a
//! publication take materializes its root once, from the snapshot its check built. A removed
//! version reads as its stub after a cache hit.
//!
//! Until the delta writer ships, a landing writes its SNAPSHOT. The fixture appends each
//! landing's delta and turns on the store's test hook that reads a landing with a delta by the
//! fold, as if its SNAPSHOT were not held (`fold_held_snapshots_for_tests`).
mod common;

use rdy_engine::publication_take::{
    PublicationTarget, materializations_for_tests, root_snapshot_reuses_for_tests, take_publication,
};
use rdy_engine::redaction::RedactInput;
use rdy_engine::render_source::materialize_landing_for_tests;
use rdy_engine::*;
use rdy_model::*;
use rdy_store::codec;
use rdy_store::format::stage_snapshot_delta_for_tests;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::segment::Location;
use rdy_store::snapshot::{RecordSnapshots, folded_deltas_for_tests, read_snapshot};
use rdy_store::sync_stream::StreamRecord;
use rdy_store::take_batch::UploadTarget;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: format!("sdl2 {view}"),
        },
    )
    .unwrap_or_else(|e| panic!("write to {view}: {e}"))
}

fn propose(r: &mut Rdy, view: &str) -> ProposalInfo {
    r.propose(&writer(), view, "sdl2", Some(common::TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e}"))
}

fn land(r: &mut Rdy, view: &str) -> Hash {
    let p = propose(r, view);
    common::confirm_exceptions(r, &approver(), &p);
    r.land(&approver(), &p.id).unwrap().parse().unwrap()
}

/// The shared small fixture: three landings on canon (create, change and add, delete), a tag on
/// the second, a view pinned at the first with a change of its own, a floating view and a fork
/// of the pinned view.
struct Fx {
    _dir: tempfile::TempDir,
    r: Rdy,
    /// Canon's landings, oldest first.
    landings: Vec<Hash>,
    /// The pinned view's change.
    pinned_change: Hash,
    x: BlockId,
    y: BlockId,
}

fn fixture() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$x", "parent": "$d", "body": "The x text."},
            {"op": "create", "type": "prose", "id": "$y", "parent": "$d", "body": "The y text."}
        ]),
    );
    let (doc, x, y) = (
        w.created["$d"].clone(),
        w.created["$x"].clone(),
        w.created["$y"].clone(),
    );
    let l0 = land(&mut r, "work");
    write(
        &mut r,
        "work",
        json!([
            {"op": "update", "id": x, "body": "The x text, changed."},
            {"op": "create", "type": "prose", "id": "$z", "parent": doc, "body": "The z text."}
        ]),
    );
    let l1 = land(&mut r, "work");
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    write(&mut r, "work", json!([{"op": "delete", "id": y}]));
    let l2 = land(&mut r, "work");
    r.view_create(&writer(), "pinned", Some(&format!("landing:{l0}")), true)
        .unwrap();
    let c = write(
        &mut r,
        "pinned",
        json!([{"op": "update", "id": y, "body": "The y text, pinned."}]),
    );
    r.view_create(&writer(), "floating", None, false).unwrap();
    r.view_fork(&writer(), "pinned", "forked").unwrap();
    Fx {
        _dir: dir,
        r,
        landings: vec![l0, l1, l2],
        pinned_change: c.change.parse().unwrap(),
        x: BlockId::parse(&x).unwrap(),
        y: BlockId::parse(&y).unwrap(),
    }
}

/// Appends the delta of each canon landing that has none, oldest first, each from its held
/// SNAPSHOT (a raw typed read, not the map read under test). Returns how many it wrote.
fn add_deltas(r: &mut Rdy) -> usize {
    let mut chain = r.store().landings().unwrap();
    chain.reverse();
    let mut items = vec![];
    let mut prev: Option<(BTreeMap<BlockId, Hash>, Hash)> = None;
    for (landing, rec) in chain {
        let map = r.store().get::<StateSnapshot>(&rec.state).unwrap().blocks;
        let delta = match r.store().snapshot_delta_of(&landing).unwrap() {
            Some((h, _)) => h,
            None => {
                let empty = BTreeMap::new();
                let (before, base) = match &prev {
                    Some((m, d)) => (m, Some(*d)),
                    None => (&empty, None),
                };
                let d = diff_snapshots(landing, base, before, &map);
                items.push(codec::item_new(kinds::SNAPSHOT_DELTA, &d));
                d.id()
            }
        };
        prev = Some((map, delta));
    }
    let n = items.len();
    if n > 0 {
        r.store_mut().write(items).unwrap();
        r.invalidate();
    }
    n
}

/// Reads with nothing cached: the store's map cache and the engine's built states are dropped.
fn cold(r: &mut Rdy) {
    r.store().clear_map_cache_for_tests();
    r.invalidate();
}

/// The deltas `f` folds on this thread, and its result.
fn folds<T>(f: impl FnOnce() -> T) -> (u64, T) {
    let start = folded_deltas_for_tests();
    let out = f();
    (folded_deltas_for_tests() - start, out)
}

/// Each block of `m`, by id: its version and its body.
fn blocks(m: &Materialized) -> Vec<(BlockId, Hash, String)> {
    let snap = m.snapshot();
    snap.blocks
        .iter()
        .map(|(id, v)| {
            (
                id.clone(),
                *v,
                m.get(id).map(|b| b.body.clone()).unwrap_or_default(),
            )
        })
        .collect()
}

/// What every read scope of the fixture gives, as text.
fn facts(fx: &Fx) -> Vec<(String, String)> {
    let st = fx.r.read();
    let mut out = vec![];
    let mut push = |name: &str, v: String| out.push((name.to_string(), v));
    let scopes = [
        ("canon", Scope::Canon),
        ("landing 0", Scope::At(fx.landings[0])),
        ("landing 1", Scope::At(fx.landings[1])),
        ("tag", st.resolve_scope("tag:v1.0.0").unwrap()),
        ("pinned", Scope::View("pinned".into())),
        ("floating", Scope::View("floating".into())),
        ("forked", Scope::View("forked".into())),
    ];
    for (name, scope) in &scopes {
        let m = st.materialized(scope).unwrap();
        push(name, format!("{:?}", blocks(&m)));
        push(
            &format!("{name} conflicts"),
            format!("{:?}", st.conflicts(scope).unwrap()),
        );
    }
    let t = st.tensions().unwrap();
    push(
        "tensions",
        format!("{} {:?}", t.len(), t.iter().collect::<Vec<_>>()),
    );
    push(
        "consequences",
        format!(
            "{:?}",
            st.consequences(&Scope::View("pinned".into()), &fx.pinned_change, None)
                .unwrap()
        ),
    );
    push(
        "plan",
        format!("{:?}", st.view_plan("pinned").map(|p| p.items)),
    );
    out
}

#[test]
fn sdl2_all_state_scopes_read_delta_only_history() {
    let _staged = stage_snapshot_delta_for_tests();
    let mut fx = fixture();
    let before = facts(&fx);
    assert_eq!(add_deltas(&mut fx.r), 3);
    fx.r.store().fold_held_snapshots_for_tests(true);

    // every read scope gives what the held SNAPSHOTs gave, now by folding the deltas
    cold(&mut fx.r);
    let (n, after) = folds(|| facts(&fx));
    assert!(n > 0, "the reads folded deltas");
    assert_eq!(after, before);
    // a warm read folds nothing more
    let (n, again) = folds(|| facts(&fx));
    assert_eq!(again, before);
    assert_eq!(n, 0, "warm reads fold no delta");

    // each landing's map is its SNAPSHOT's, folded from the first landing
    for (i, l) in fx.landings.iter().enumerate() {
        cold(&mut fx.r);
        let rec = fx.r.store().landing(l).unwrap();
        let held = fx.r.store().get::<StateSnapshot>(&rec.state).unwrap();
        let (n, got) = folds(|| fx.r.store().snapshot_at(l).unwrap());
        assert_eq!((n, got), (i as u64 + 1, held));
    }
    // replay reads its target's state by the fold
    fx.r.view_create(
        &writer(),
        "replayed",
        Some(&format!("landing:{}", fx.landings[0])),
        true,
    )
    .unwrap();
    cold(&mut fx.r);
    let (n, res) = folds(|| {
        fx.r.view_replay(&writer(), "replayed", &[fx.pinned_change.to_string()])
    });
    let res = res.unwrap();
    assert!(n > 0, "replay folded");
    assert_eq!(res.changes, 1);
    let m =
        fx.r.read()
            .materialized(&Scope::View("replayed".into()))
            .unwrap();
    assert_eq!(m.get(&fx.y).unwrap().body, "The y text, pinned.");

    // undo reads the state its inverse needs by the fold
    cold(&mut fx.r);
    let (n, undone) = folds(|| fx.r.undo(&writer(), "replayed", None, false));
    undone.unwrap();
    assert!(n > 0, "undo folded");
    let m =
        fx.r.read()
            .materialized(&Scope::View("replayed".into()))
            .unwrap();
    assert_eq!(m.get(&fx.y).unwrap().body, "The y text.");

    // the gate and the landing's consequences read canon by the fold
    write(
        &mut fx.r,
        "work",
        json!([{"op": "update", "id": fx.x.to_string(), "body": "The x text, gated."}]),
    );
    let p = propose(&mut fx.r, "work");
    cold(&mut fx.r);
    let version: Hash = p.id.parse().unwrap();
    let (n, c) = folds(|| fx.r.read().landing_consequences(&version, None));
    c.unwrap();
    assert!(n > 0, "the landing's consequences folded");
    common::confirm_exceptions(&mut fx.r, &approver(), &p);
    cold(&mut fx.r);
    let (n, landed) = folds(|| fx.r.land(&approver(), &p.id));
    let landed: Hash = landed.unwrap().parse().unwrap();
    assert!(n > 0, "the gate folded");
    // the new landing has its SNAPSHOT and no delta yet: it reads its SNAPSHOT
    assert_eq!(fx.r.store().landings().unwrap()[0].0, landed);
    let m = fx.r.read().materialized(&Scope::Canon).unwrap();
    assert_eq!(m.get(&fx.x).unwrap().body, "The x text, gated.");
    assert_eq!(add_deltas(&mut fx.r), 1);
    cold(&mut fx.r);
    let (n, m) = folds(|| fx.r.read().materialized(&Scope::Canon).unwrap());
    assert_eq!(n, 4, "canon folds all four landings");
    assert_eq!(m.get(&fx.x).unwrap().body, "The x text, gated.");
}

/// The fixture's records as a stream source holds them: every object frame of the kinds a
/// tag's history carries, with no SNAPSHOT, so each landing's map is its deltas'.
fn delta_records(r: &Rdy) -> BTreeMap<ObjectKey, StreamRecord> {
    const COPIED: [u8; 7] = [
        kinds::BLOCK,
        kinds::CHANGE,
        kinds::PROPOSAL,
        kinds::LANDING,
        kinds::SNAPSHOT_DELTA,
        kinds::BLOB_CHUNK,
        kinds::BLOB_MANIFEST,
    ];
    let mut out = BTreeMap::new();
    r.store()
        .for_each_frame_from(ORIGIN, r.store().log_end(), |_, raw, hash, payload, _| {
            let kind = codec::logical_kind(raw);
            if COPIED.contains(&kind) {
                let key = ObjectKey { kind, hash };
                let encoding = codec::split(raw, payload).unwrap().version;
                out.entry(key).or_insert(StreamRecord::Present {
                    key,
                    encoding,
                    payload: payload.to_vec(),
                });
            }
            Ok(())
        })
        .unwrap();
    out
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
fn sdl2_stream_and_store_materialization_are_equal() {
    let _staged = stage_snapshot_delta_for_tests();
    let mut fx = fixture();
    add_deltas(&mut fx.r);
    fx.r.store().fold_held_snapshots_for_tests(true);
    let records = delta_records(&fx.r);
    assert!(!records.keys().any(|k| k.kind == kinds::SNAPSHOT));
    for l in &fx.landings {
        cold(&mut fx.r);
        let store_map = fx.r.store().snapshot_at(l).unwrap();
        let store_state = fx.r.read().materialized(&Scope::At(*l)).unwrap();
        // the stream's own association, from the typed delta records it holds
        let mut src = records.clone();
        let (n, stream_map) =
            folds(|| read_snapshot(&mut RecordSnapshots::new(&mut src), None, l).unwrap());
        assert!(n > 0, "the stream folded");
        assert_eq!(stream_map, store_map);
        // the engine's source-backed materialization (a render's and a take's read)
        let mut src = records.clone();
        let m = materialize_landing_for_tests(&mut src, *l).unwrap();
        assert_eq!(blocks(&m), blocks(&store_state));
        assert_eq!(m.snapshot_id(), store_state.snapshot_id());
    }

    // a publication take materializes its root once, from the snapshot its check built
    fx.r.store().fold_held_snapshots_for_tests(false);
    let v1 = tag_hash(&fx.r, "v1.0.0");
    let l1 = fx.r.store().read_tag(&v1).unwrap().landing;
    let mut bytes = Vec::new();
    stream_export::write(&fx.r.read(), v1, &BTreeSet::new(), &mut bytes).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let repo = fx.r.store().repo_id().unwrap().unwrap();
    let mut h = Rdy::create_holder(&dir.path().join("holder"), repo).unwrap();
    let path = dir.path().join("v1.rdysync");
    std::fs::write(&path, &bytes).unwrap();
    let start = (
        materializations_for_tests(),
        root_snapshot_reuses_for_tests(),
    );
    let got = take_publication(
        &mut h,
        &[File::open(&path).unwrap()],
        PublicationTarget {
            upload: UploadTarget {
                tag: "v1.0.0".into(),
                landing: l1,
            },
            profile: "implicit:org".into(),
        },
        |_| Ok::<_, ()>(()),
    )
    .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(
        (
            materializations_for_tests(),
            root_snapshot_reuses_for_tests()
        ),
        (start.0 + 1, start.1 + 1),
        "one materialization, from the checked root"
    );
    assert_eq!(got.facts.landing, l1);
    // the holder reads the root as the source does
    assert_eq!(
        h.store().snapshot_at(&l1).unwrap(),
        fx.r.store().snapshot_at(&l1).unwrap()
    );
}

/// Removes every version of block `target` through the engine's own landing path, which renews
/// the engine's caches for the new removal epoch.
fn remove(r: &mut Rdy, target: &BlockId) {
    let req = r
        .redact_in(
            &writer(),
            "redact",
            RedactInput {
                targets: vec![target.to_string()],
                reason_class: "secret".into(),
                reason: "a leaked text".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(req.redaction.is_some());
    let p = r
        .propose(&writer(), "redact", "Remove the text", Some("Removes it."))
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
}

#[test]
fn sdl2_removed_versions_read_as_stubs_after_cache_hits() {
    let _staged = stage_snapshot_delta_for_tests();
    let mut fx = fixture();
    // warm the map cache: canon and a historical landing, each read twice
    cold(&mut fx.r);
    for _ in 0..2 {
        let m =
            fx.r.read()
                .materialized(&Scope::At(fx.landings[1]))
                .unwrap();
        assert_eq!(m.get(&fx.x).unwrap().body, "The x text, changed.");
        fx.r.read().materialized(&Scope::Canon).unwrap();
    }
    assert!(fx.r.store().map_cache_stats_for_tests().0 > 0);
    let version = fx.r.store().snapshot_at(&fx.landings[1]).unwrap().blocks[&fx.x];

    // the removal report has no kind-30 rows before the report task: remove first, then
    // append the deltas
    remove(&mut fx.r, &fx.x);

    // the cached maps hold version hashes only: each removed version reads as its stub, with
    // its original version hash, in every scope that reads it
    let stubs = |fx: &Fx| {
        for scope in [Scope::At(fx.landings[0]), Scope::At(fx.landings[1])] {
            let m = fx.r.read().materialized(&scope).unwrap();
            let b = m.get(&fx.x).unwrap();
            assert!(b.is_redacted(), "{scope:?}: {}", b.body);
            assert_eq!(b.body, "Content removed.", "{scope:?}");
            assert!(!b.body.contains("x text"), "{scope:?}");
        }
        let m =
            fx.r.read()
                .materialized(&Scope::At(fx.landings[1]))
                .unwrap();
        assert_eq!(m.snapshot().blocks[&fx.x], version);
    };
    stubs(&fx);
    // the removal's epoch emptied the cache
    let (n, map) = folds(|| fx.r.store().snapshot_at(&fx.landings[1]).unwrap());
    assert_eq!(n, 0, "a held SNAPSHOT, no delta yet");
    assert_eq!(map.blocks[&fx.x], version);

    // with deltas, the folded maps of the warm cache serve stubs too
    assert_eq!(add_deltas(&mut fx.r), 4, "three landings and the removal's");
    // a reopen: no engine state or map built before is kept
    let root = fx._dir.path().to_path_buf();
    let r = std::mem::replace(
        &mut fx.r,
        Rdy::init(&root.join("spare"), "rdy-standard").unwrap(),
    );
    r.close().unwrap();
    fx.r = Rdy::open(&root).unwrap();
    fx.r.store().fold_held_snapshots_for_tests(true);
    let (n, _) = folds(|| stubs(&fx));
    assert!(n > 0, "folded");
    let (n, _) = folds(|| stubs(&fx));
    assert_eq!(n, 0, "cache hits");
    let (n, _) = folds(|| fx.r.store().snapshot_at(&fx.landings[1]).unwrap());
    assert_eq!(n, 0);
    assert!(fx.r.store().get_block(&version).unwrap().is_redacted());
}
