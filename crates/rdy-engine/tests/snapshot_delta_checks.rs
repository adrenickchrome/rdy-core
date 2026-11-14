//! sdl3 (snapshot deltas, D-7K4Z0): the chain checks every boundary shares. A publication take
//! applies each uploaded landing's delta once, walking the chains oldest first; a project's
//! fsck, the holder check, a take and a load refuse a bent delta with the same text; and a
//! SNAPSHOT stored as anything but the canonical encoding of its map is refused before a take
//! or a load writes, with or without the feature.
//!
//! Until the delta writer ships, a landing writes its SNAPSHOT. The fixtures append each
//! landing's delta afterwards, which turns the source store's `snapshot_delta` feature on: its
//! closures then carry the deltas and no SNAPSHOT.
mod common;

use rdy_engine::publication_take::{
    PublicationRefusal, PublicationTarget, materializations_for_tests,
    root_snapshot_reuses_for_tests, take_publication,
};
use rdy_engine::*;
use rdy_model::*;
use rdy_store::closure::{END_OF_LOG, StoreSource, closure, write_batch};
use rdy_store::codec;
use rdy_store::format::stage_snapshot_delta_for_tests;
use rdy_store::kinds;
use rdy_store::snapshot::{folded_deltas_for_tests, record_chain_applications_for_tests};
use rdy_store::stream_error::StreamError;
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::StreamHeader;
use rdy_store::take_batch::{TagChoice, UploadFacts, UploadTarget};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};

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
            message: format!("sdl3 {view}"),
        },
    )
    .unwrap_or_else(|e| panic!("write to {view}: {e}"))
}

fn land(r: &mut Rdy, view: &str) -> Hash {
    let p = r
        .propose(&writer(), view, "sdl3", Some(common::TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e}"));
    common::confirm_exceptions(r, &approver(), &p);
    r.land(&approver(), &p.id).unwrap().parse().unwrap()
}

/// A project with `n` landings on canon (a doc, then one prose block per landing), tagged
/// `v1.0.0` at the last one. Returns the project's directory guard, the project, its landings
/// (oldest first) and the tag's hash.
fn project(n: usize) -> (tempfile::TempDir, Rdy, Vec<Hash>, Hash) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(&dir.path().join("src"), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "Prose 0."}
        ]),
    );
    let doc = w.created["$d"].clone();
    let mut landings = vec![land(&mut r, "work")];
    for i in 1..n {
        write(
            &mut r,
            "work",
            json!([{"op": "create", "type": "prose", "id": "$p", "parent": doc, "body": format!("Prose {i}.")}]),
        );
        landings.push(land(&mut r, "work"));
    }
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let tag = tag_hash(&r, "v1.0.0");
    (dir, r, landings, tag)
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

/// The delta to bend, by landing index, and how.
type Bend<'a> = Option<(usize, &'a dyn Fn(&mut SnapshotDelta))>;

/// Appends each canon landing's delta, oldest first, from its held SNAPSHOT; the delta of the
/// landing at index `bend.0` is changed by `bend.1` first (the next delta names the changed
/// one as its base). Returns the deltas' hashes in landing order.
fn add_deltas(r: &mut Rdy, bend: Bend<'_>) -> Vec<Hash> {
    let mut chain = r.store().landings().unwrap();
    chain.reverse();
    let mut items = vec![];
    let mut out = vec![];
    let mut prev: Option<(BTreeMap<BlockId, Hash>, Hash)> = None;
    for (i, (landing, rec)) in chain.into_iter().enumerate() {
        let map = r.store().get::<StateSnapshot>(&rec.state).unwrap().blocks;
        let empty = BTreeMap::new();
        let (before, base) = match &prev {
            Some((m, d)) => (m, Some(*d)),
            None => (&empty, None),
        };
        let mut d = diff_snapshots(landing, base, before, &map);
        if let Some((at, edit)) = &bend
            && *at == i
        {
            edit(&mut d);
        }
        items.push(codec::item_new(kinds::SNAPSHOT_DELTA, &d));
        out.push(d.id());
        prev = Some((map, d.id()));
    }
    r.store_mut().write(items).unwrap();
    r.invalidate();
    out
}

/// One batch file of tag `tag`'s whole closure in project `r`, with its store's features.
fn export(r: &Rdy, tag: Hash, path: &Path) {
    let reader = r.store().reader();
    let c = closure(&reader, tag, &BTreeSet::new()).unwrap();
    let header = StreamHeader::tag_history(
        reader.store_features().unwrap(),
        r.store().repo_id().unwrap().unwrap(),
        tag,
        c.prerequisites,
    );
    let mut source = StoreSource {
        reader: &reader,
        end: END_OF_LOG,
    };
    write_batch(
        &mut source,
        &header,
        &c.records,
        File::create(path).unwrap(),
    )
    .unwrap();
}

fn target(r: &Rdy, tag: Hash) -> PublicationTarget {
    let t = r.store().read_tag(&tag).unwrap();
    PublicationTarget {
        upload: UploadTarget {
            tag: t.name,
            landing: t.landing,
        },
        profile: "implicit:org".into(),
    }
}

fn holder(r: &Rdy, dir: &Path, name: &str) -> Rdy {
    Rdy::create_holder(&dir.join(name), r.store().repo_id().unwrap().unwrap()).unwrap()
}

fn publish(h: &mut Rdy, file: &Path, target: PublicationTarget) -> Result<(), RdyError> {
    take_publication(h, &[File::open(file).unwrap()], target, |_| Ok::<_, ()>(()))
        .map(|_| ())
        .map_err(|e| match e {
            PublicationRefusal::Core(e) => e,
            PublicationRefusal::Verdict(()) => panic!("the verdict refused"),
        })
}

fn incoming(_: &mut dyn ObjectSource, _: &UploadFacts) -> Result<TagChoice, StreamError> {
    Ok(TagChoice::Incoming)
}

/// The store's own take into holder `h`, with no publication check (its held batches off, as
/// the engine's take turns them off).
fn raw_take(
    h: &mut Rdy,
    file: &Path,
    target: &UploadTarget,
) -> Result<rdy_store::take_batch::BatchTaken, rdy_store::take_batch::TakeError<StreamError>> {
    h.store_mut().set_held(false).unwrap();
    let taken = h
        .store_mut()
        .take_batches(&[File::open(file).unwrap()], target, incoming);
    h.store_mut().set_held(true).unwrap();
    h.invalidate();
    taken
}

#[test]
fn sdl3_publication_check_folds_each_delta_once() {
    let _staged = stage_snapshot_delta_for_tests();
    let (dir, mut r, landings, tag) = project(200);
    let deltas = add_deltas(&mut r, None);
    assert_eq!(deltas.len(), 200);
    let file = dir.path().join("v1.rdysync");
    export(&r, tag, &file);
    let mut h = holder(&r, dir.path(), "holder");
    let start = (
        materializations_for_tests(),
        root_snapshot_reuses_for_tests(),
    );
    let applied = record_chain_applications_for_tests();
    let folded = folded_deltas_for_tests();
    publish(&mut h, &file, target(&r, tag)).unwrap();
    // no map read folds a chain again: not the check, and not the root's materialization
    assert_eq!(
        folded_deltas_for_tests() - folded,
        0,
        "no fold from an empty start"
    );
    // exactly one application of each delta: the chain is walked once, oldest first, and no
    // landing's map is folded from an empty start
    let got = applied.take();
    assert_eq!(got, deltas, "each delta once, in chain order");
    // the publication facts are resolved from the root the check built, as before
    assert_eq!(
        (
            materializations_for_tests(),
            root_snapshot_reuses_for_tests()
        ),
        (start.0 + 1, start.1 + 1),
        "one materialization, from the checked root"
    );
    // the holder holds no SNAPSHOT, and reads every landing as the source does
    assert!(
        !h.store()
            .has_kind(
                kinds::SNAPSHOT,
                &r.store().landing(&landings[0]).unwrap().state
            )
            .unwrap()
    );
    for l in [landings[0], landings[99], landings[199]] {
        assert_eq!(
            h.store().snapshot_at(&l).unwrap(),
            r.store().snapshot_at(&l).unwrap()
        );
    }
    h.store().check_holder().unwrap();
}

/// The texts of fsck, the holder check, a take and a load of the history of project `r`
/// (each the error's message, or None when it passed).
fn boundaries(r: &mut Rdy, dir: &Path, tag: Hash, name: &str) -> [Option<String>; 5] {
    let msg = |e: RdyError| e.message;
    // a project's fsck
    let fsck = r.fsck().err().map(msg);
    // a publication take
    let file = dir.join(format!("{name}.rdysync"));
    export(r, tag, &file);
    let mut h = holder(r, dir, &format!("{name}-pub"));
    let take = publish(&mut h, &file, target(r, tag)).err().map(msg);
    // the holder check, of a holder that took the history without the publication check
    let mut h = holder(r, dir, &format!("{name}-holder"));
    let t = target(r, tag).upload;
    raw_take(&mut h, &file, &t).unwrap();
    let held = h.store().check_holder().err().map(|e| e.to_string());
    // a load of each: the project's dump, and the holder's
    let load = |from: &Rdy, to: &str| {
        let dump = dir.join(format!("{to}.rdydump"));
        from.read().dump(&dump).unwrap();
        let into: PathBuf = dir.join(to);
        let r = rdy_engine::load::load(&dump, &into).err().map(msg);
        if r.is_some() {
            assert!(
                Rdy::open(&into).is_err(),
                "a refused load leaves no project"
            );
        }
        r
    };
    let loaded = load(r, &format!("{name}-load"));
    let loaded_holder = load(&h, &format!("{name}-load-holder"));
    [fsck, take, held, loaded, loaded_holder]
}

#[test]
fn sdl3_fsck_holder_take_and_load_agree() {
    let _staged = stage_snapshot_delta_for_tests();
    // a good delta history passes every boundary
    let (dir, mut r, _, tag) = project(3);
    add_deltas(&mut r, None);
    assert_eq!(
        boundaries(&mut r, dir.path(), tag, "good"),
        [None, None, None, None, None]
    );
    // a bent delta (landing 1's full is another map's) is refused by each, with one text
    let (dir, mut r, _, tag) = project(3);
    let deltas = add_deltas(
        &mut r,
        Some((1, &|d: &mut SnapshotDelta| d.full = Hash([7; 32]))),
    );
    let detail = format!(
        "snapshot delta {}: landing or full hash does not match",
        deltas[1]
    );
    let got = boundaries(&mut r, dir.path(), tag, "bent");
    for (i, m) in got.iter().enumerate() {
        let m = m.as_ref().unwrap_or_else(|| panic!("boundary {i} passed"));
        assert!(m.contains(&detail), "boundary {i}: {m}");
    }
    // a base that skips a landing: each refuses it the same way too
    let (dir, mut r, landings, tag) = project(3);
    let root_delta = {
        let state = r.store().landing(&landings[0]).unwrap().state;
        let map = r.store().get::<StateSnapshot>(&state).unwrap().blocks;
        diff_snapshots(landings[0], None, &BTreeMap::new(), &map).id()
    };
    let deltas = add_deltas(
        &mut r,
        Some((2, &move |d: &mut SnapshotDelta| d.base = Some(root_delta))),
    );
    let detail = format!(
        "snapshot delta {}: base does not match the previous landing",
        deltas[2]
    );
    let got = boundaries(&mut r, dir.path(), tag, "base");
    for (i, m) in got.iter().enumerate() {
        let m = m.as_ref().unwrap_or_else(|| panic!("boundary {i} passed"));
        assert!(m.contains(&detail), "boundary {i}: {m}");
    }
}

/// Three SNAPSHOT encodings of a two-block map that decode but are not its canonical bytes.
fn noncanonical(a: &BlockId, va: Hash, b: &BlockId, vb: Hash) -> Vec<(&'static str, Vec<u8>)> {
    assert!(a < b);
    let entry = |id: &BlockId, v: Hash, padded: bool| {
        let s = id.to_string();
        let mut e = if padded {
            vec![(s.len() as u8) | 0x80, 0x00]
        } else {
            vec![s.len() as u8]
        };
        e.extend(s.as_bytes());
        e.extend(v.0);
        e
    };
    let mut reordered = vec![2u8];
    reordered.extend(entry(b, vb, false));
    reordered.extend(entry(a, va, false));
    let mut duplicate = vec![2u8];
    duplicate.extend(entry(a, va, false));
    duplicate.extend(entry(a, va, false));
    let mut padded_len = vec![0x82u8, 0x00];
    padded_len.extend(entry(a, va, false));
    padded_len.extend(entry(b, vb, false));
    let mut padded_key = vec![2u8];
    padded_key.extend(entry(a, va, true));
    padded_key.extend(entry(b, vb, false));
    vec![
        ("reordered keys", reordered),
        ("duplicate keys", duplicate),
        ("a non-minimal map length", padded_len),
        ("a non-minimal key length", padded_key),
    ]
}

#[test]
fn sdl3_noncanonical_snapshot_refuses_before_take_or_load_writes() {
    for variant in 0..4 {
        // a fresh project each time: a landing on canon's head whose state is bytes of its map
        // that are not canonical, tagged `v2.0.0`. No feature: the take and the load still check
        // the bytes
        let (dir, mut r, landings, _) = project(2);
        let m = r
            .store()
            .get::<StateSnapshot>(&r.store().landing(&landings[1]).unwrap().state)
            .unwrap()
            .blocks;
        let mut it = m.iter();
        let (a, va) = it.next().unwrap();
        let (b, vb) = it.next().unwrap();
        let (what, bytes) = noncanonical(a, *va, b, *vb).swap_remove(variant);
        let state = r
            .store_mut()
            .write(vec![(kinds::SNAPSHOT, bytes.clone())])
            .unwrap()[0];
        let canonical = codec::decode_record::<StateSnapshot>(kinds::SNAPSHOT, &bytes)
            .map(|s| s.id())
            .ok();
        assert_ne!(Some(state), canonical, "{what}");
        let mut rec = r.store().landing(&landings[1]).unwrap();
        rec.read_identity = None;
        rec.prev = Some(landings[1]);
        // it lands nothing again: only its state differs from a landing a take accepts
        rec.changes = vec![];
        rec.state = state;
        rec.approved_by.at_ms += 1;
        let l = r.store_mut().put(kinds::LANDING, &rec).unwrap();
        let tag = r
            .store_mut()
            .put_tag(&Tag {
                name: "v2.0.0".into(),
                landing: l,
                message: None,
                tagged_by: Provenance {
                    actor: "human:founder".into(),
                    run: None,
                    role: Role::Approver,
                    at_ms: 1,
                },
            })
            .unwrap();
        r.invalidate();
        assert!(
            !r.store()
                .features()
                .has(rdy_store::format::FEATURE_SNAPSHOT_DELTA)
        );
        let detail = format!("snapshot {state}: bytes are not canonical");
        // the take refuses the batch before it writes
        let file = dir.path().join("bad.rdysync");
        export(&r, tag, &file);
        let mut h = holder(&r, dir.path(), "h");
        let end = h.store().log_end();
        let e = publish(&mut h, &file, target(&r, tag)).unwrap_err();
        assert_eq!(e.code, "E_BAD_RECORD", "{what}: {e:?}");
        assert!(e.message.ends_with(&detail), "{what}: {}", e.message);
        assert_eq!(
            e.fix,
            "request this stream again; run rdy fsck at its source"
        );
        assert_eq!(h.store().log_end(), end, "{what}: nothing written");
        // the store's own take refuses it the same way
        let e = raw_take(&mut h, &file, &target(&r, tag).upload).unwrap_err();
        assert!(format!("{e:?}").contains(&detail), "{what}: {e:?}");
        assert_eq!(h.store().log_end(), end);
        // a load refuses the dump before it writes the batch that holds the bytes
        let dump = dir.path().join("bad.rdydump");
        r.read().dump(&dump).unwrap();
        let into = dir.path().join("loaded");
        let e = rdy_engine::load::load(&dump, &into).unwrap_err();
        assert!(e.message.contains(&detail), "{what}: {}", e.message);
        assert!(Rdy::open(&into).is_err(), "{what}: no project left");
        // and the project's own fsck names the landing
        let e = r.fsck().unwrap_err();
        assert!(
            e.message
                .contains(&format!("landing {l}: snapshot {state} is not canonical")),
            "{what}: {}",
            e.message
        );
    }
}
