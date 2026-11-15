//! sdl6 (SD1 Task 6; D-7K4Z0, D-918YB, D-8T7PA): every public path that brings a pre-delta
//! history into this rdy keeps every identity. A released store's writer open (the registered
//! `snapshot_delta` upgrade), a released dump's load and a released stream's take into a new
//! holder each keep every old object under its old key with its exact raw kind and payload bytes
//! (a removed one as its placeholder), every landing, tag, signature subject, block pin and relay
//! evidence (all inside those bytes), the old SNAPSHOT objects, and each landing's materialized
//! map. What they add is one SNAPSHOT_DELTA per landing, under its own new key: an addition,
//! never a substitute under an old key.
use rdy_engine::Rdy;
use rdy_engine::load::load;
use rdy_engine::maintenance::store_dir;
use rdy_engine::stream_take::take;
use rdy_model::signature::Signature;
use rdy_model::{BlockId, Hash, LandingRecord, SnapshotDelta, StateSnapshot, Tag};
use rdy_qa::snapshot_delta_fixture as fx;
use rdy_store::dump::{DumpItem, DumpReader, Frame};
use rdy_store::format::FEATURE_SNAPSHOT_DELTA;
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::Location;
use rdy_store::sync_stream::{StreamReader, StreamRecord};
use rdy_store::take_batch::UploadTarget;
use rdy_store::{OpenPolicy, Store, codec, kinds};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

/// One object as a log or a file holds it: its raw kind and payload, or a removal's
/// placeholder (raw kind and original length).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Held {
    Present(u8, Vec<u8>),
    Dropped(u8, u32),
}

/// Every object a history holds, by logical kind and hash.
type Objects = BTreeMap<(u8, Hash), Held>;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// Every portable object of the store's log.
fn store_objects(s: &Store) -> Objects {
    let mut out = Objects::new();
    s.reader()
        .for_each_record_from(ORIGIN, s.log_end(), |_, f, _| {
            let (raw, hash, held) = match f {
                StoredFrame::Present {
                    kind,
                    hash,
                    payload,
                } => (kind, hash, Held::Present(kind, payload.to_vec())),
                StoredFrame::Dropped {
                    kind, hash, len, ..
                } => (kind, hash, Held::Dropped(kind, len)),
            };
            let k = codec::logical_kind(raw);
            if rdy_store::object_refs::is_portable(k) {
                out.insert((k, hash), held);
            }
            Ok(())
        })
        .unwrap();
    out
}

/// Every portable object of the dump file `p`.
fn dump_objects(p: &Path) -> Objects {
    let bytes = std::fs::read(p).unwrap();
    let (mut r, _, _) = DumpReader::open(&bytes[..]).unwrap();
    let mut out = Objects::new();
    while let Some(item) = r.next().unwrap() {
        let DumpItem::Batch(b) = item else { continue };
        for f in b.frames {
            let (raw, hash, held) = match f {
                Frame::Present {
                    kind,
                    hash,
                    payload,
                } => (kind, hash, Held::Present(kind, payload)),
                Frame::Dropped {
                    kind, hash, len, ..
                } => (kind, hash, Held::Dropped(kind, len)),
            };
            let k = codec::logical_kind(raw);
            if rdy_store::object_refs::is_portable(k) {
                out.insert((k, hash), held);
            }
        }
    }
    out
}

/// Every record of the stream `bytes`, with its header's features.
fn stream_objects(bytes: &[u8]) -> (Objects, rdy_store::sync_stream::StreamHeader) {
    let (mut r, header) = StreamReader::open(bytes).unwrap();
    let mut out = Objects::new();
    while let Some(rec) = r.next().unwrap() {
        let k = rec.key();
        let held = match &rec {
            StreamRecord::Present { payload, .. } => Held::Present(rec.raw_kind(), payload.clone()),
            StreamRecord::Dropped { original_len, .. } => {
                Held::Dropped(rec.raw_kind(), *original_len)
            }
        };
        out.insert((k.kind, k.hash), held);
    }
    r.finish().unwrap();
    (out, header)
}

fn names(s: &Store) -> Vec<String> {
    s.features()
        .features
        .iter()
        .map(|f| f.name.clone())
        .collect()
}

fn decode<T: serde::de::DeserializeOwned + 'static>(held: &Held) -> T {
    match held {
        Held::Present(raw, payload) => codec::decode_record(*raw, payload).unwrap(),
        Held::Dropped(..) => panic!("a removed object has no payload"),
    }
}

/// The landings, tags and signatures of `old`, decoded from their bytes.
fn typed_facts(old: &Objects) -> (Vec<(Hash, LandingRecord)>, Vec<Tag>, Vec<Hash>) {
    let mut landings = vec![];
    let mut tags = vec![];
    let mut subjects = vec![];
    for ((k, h), held) in old {
        match *k {
            kinds::LANDING => landings.push((*h, decode::<LandingRecord>(held))),
            kinds::TAG => tags.push(decode::<Tag>(held)),
            kinds::SIGNATURE => subjects.push(decode::<Signature>(held).subject),
            _ => {}
        }
    }
    (landings, tags, subjects)
}

/// The block map each landing of `old` in `of` materializes, from its held SNAPSHOT.
fn old_maps(old: &Objects, of: &BTreeSet<Hash>) -> BTreeMap<Hash, BTreeMap<BlockId, Hash>> {
    let (landings, ..) = typed_facts(old);
    landings
        .into_iter()
        .filter(|(h, _)| of.contains(h))
        .map(|(h, l)| {
            let snap = old
                .get(&(kinds::SNAPSHOT, l.state))
                .unwrap_or_else(|| panic!("landing {h}: its old SNAPSHOT {}", l.state));
            (h, decode::<StateSnapshot>(snap).blocks)
        })
        .collect()
}

/// `new`, the store `s`'s objects after a path brought in the pre-delta history `old`, keeps
/// every identity of `old` and adds only one delta per landing.
fn assert_kept(what: &str, old: &Objects, s: &Store) {
    let new = store_objects(s);
    assert!(
        !old.keys().any(|(k, _)| *k == kinds::SNAPSHOT_DELTA),
        "{what}: the source is pre-delta"
    );
    // every old object under its old key, with its raw kind and bytes (a placeholder as one)
    for (k, held) in old {
        assert_eq!(new.get(k), Some(held), "{what}: {}:{}", k.0, k.1);
    }
    // what was added: one delta for each landing, each under a new key
    let added: BTreeSet<(u8, Hash)> = new
        .keys()
        .filter(|k| !old.contains_key(k))
        .copied()
        .collect();
    assert!(
        added.iter().all(|(k, _)| *k == kinds::SNAPSHOT_DELTA),
        "{what}: only deltas are added: {added:?}"
    );
    let (landings, tags, subjects) = typed_facts(old);
    assert!(!landings.is_empty(), "{what}: the source has landings");
    // the project's landings get a delta each; the record of a verified foreign-removal proof
    // is not one of them and gets none (`StoreReader::project_landings`)
    let project: BTreeSet<Hash> = s
        .reader()
        .project_landings()
        .unwrap()
        .into_iter()
        .map(|(h, _)| h)
        .collect();
    assert_eq!(added.len(), project.len(), "{what}: one delta per landing");
    // the old snapshots stay held
    let snapshots = old.keys().filter(|(k, _)| *k == kinds::SNAPSHOT).count();
    assert!(snapshots > 0, "{what}: the source holds its snapshots");
    let maps = old_maps(old, &project);
    for (l, rec) in &landings {
        // the landing record reads back as it was
        assert_eq!(&s.landing(l).unwrap(), rec, "{what}: landing {l}");
        if !project.contains(l) {
            assert_eq!(
                s.reader().snapshot_delta_of(l).unwrap(),
                None,
                "{what}: proof record {l} has no delta"
            );
            continue;
        }
        // its delta is new, names it, and folds to the landing's old state
        let (d, _) = s
            .reader()
            .snapshot_delta_of(l)
            .unwrap()
            .unwrap_or_else(|| panic!("{what}: landing {l} has a delta"));
        assert!(added.contains(&(kinds::SNAPSHOT_DELTA, d)), "{what}: {d}");
        let delta: SnapshotDelta = s.get_typed(kinds::SNAPSHOT_DELTA, &d).unwrap();
        assert_eq!((delta.landing, delta.full), (*l, rec.state), "{what}: {d}");
        // the materialized map is the old snapshot's
        assert_eq!(
            &s.snapshot_at(l).unwrap().blocks,
            &maps[l],
            "{what}: map of {l}"
        );
    }
    // tags and signature subjects read back from the new store as they were
    assert_eq!(
        tags.len(),
        old.keys().filter(|(k, _)| *k == kinds::TAG).count()
    );
    for ((k, h), held) in old.iter().filter(|((k, _), _)| *k == kinds::TAG) {
        let got: Tag = s.get_typed(*k, h).unwrap();
        assert_eq!(got, decode::<Tag>(held), "{what}: tag {h}");
    }
    for ((k, h), _) in old.iter().filter(|((k, _), _)| *k == kinds::SIGNATURE) {
        let g: Signature = s.get_typed(*k, h).unwrap();
        assert!(subjects.contains(&g.subject), "{what}: signature {h}");
    }
    assert!(
        names(s).iter().any(|n| n == FEATURE_SNAPSHOT_DELTA),
        "{what}: the result lists the feature"
    );
}

/// The pre-delta store fixtures this rdy's releases wrote.
const STORES: [&str; 3] = ["feature-1", "figure-dark-1", "foreign-copy-1"];

#[test]
fn sdl6_pre_delta_store_upgrade_keeps_every_identity() {
    for name in STORES {
        let from = fixtures().join("store").join(name);
        // the source as this rdy reads it without the step: a RequireCurrent open
        let (_b0, old_root) = rdy_qa::foreign_fixture::project_copy(&from);
        let old = {
            let s =
                Store::open_with_policy(&store_dir(&old_root), OpenPolicy::RequireCurrent).unwrap();
            assert!(
                !names(&s).iter().any(|n| n == FEATURE_SNAPSHOT_DELTA),
                "{name}: the fixture is pre-delta"
            );
            store_objects(&s)
        };
        // the writer open: the project's open upgrades it
        let (_b1, root) = rdy_qa::foreign_fixture::project_copy(&from);
        let r = Rdy::open(&root).unwrap();
        assert_kept(name, &old, r.store());
        r.close().unwrap();
        // the result reopens current, and its checks pass
        let s = Store::open_with_policy(&store_dir(&root), OpenPolicy::RequireCurrent).unwrap();
        s.check_objects().unwrap();
    }
}

/// The released dump-format-4 full dumps (pre-delta): a project's and a holder's.
const DUMPS: [&str; 4] = [
    "v4-f1-e0.rdydump",
    "v4-f1-e1.rdydump",
    "v4-f1-e1-foreign-copy.rdydump",
    "v4-f1-e1-ck-holder.rdydump",
];

#[test]
fn sdl6_pre_delta_dump_load_keeps_every_identity() {
    for name in DUMPS {
        let file = fixtures().join("dump").join(name);
        let old = dump_objects(&file);
        let t = tempfile::tempdir().unwrap();
        let to = t.path().join("p");
        load(&file, &to).unwrap();
        let s = Store::open_with_policy(&store_dir(&to), OpenPolicy::RequireCurrent).unwrap();
        assert_kept(name, &old, &s);
        s.check_objects().unwrap();
    }
}

/// The released tag-upload sync batches (pre-delta) this rdy takes. Format 1 (`v1-s15`) is
/// refused before any record, and a link delivery (`v3-f1-e1-link-delivery`) holds no root tag
/// to take; each has its own pin.
const STREAMS: [&str; 3] = [
    "v2-f1-e0.rdysync",
    "v2-f1-e1-ck.rdysync",
    "v2-f1-e1.rdysync",
];

#[test]
fn sdl6_pre_delta_take_keeps_every_identity() {
    for name in STREAMS {
        let path = fixtures().join("sync").join(name);
        let bytes = std::fs::read(&path).unwrap();
        let (old, header) = stream_objects(&bytes);
        assert!(
            !header.features.has(FEATURE_SNAPSHOT_DELTA),
            "{name}: the stream is pre-delta"
        );
        let root_tag: Tag = decode(
            old.get(&(header.root().kind, header.root().hash))
                .unwrap_or_else(|| panic!("{name}: the batch holds its root")),
        );
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("holder");
        let mut h = Rdy::create_holder(&root, header.repo_id).unwrap();
        assert!(
            names(h.store()).iter().any(|n| n == FEATURE_SNAPSHOT_DELTA),
            "a new holder lists the feature"
        );
        take(
            &mut h,
            &[File::open(&path).unwrap()],
            UploadTarget {
                tag: root_tag.name.clone(),
                landing: root_tag.landing,
            },
        )
        .unwrap();
        assert_kept(name, &old, h.store());
        h.store().check_holder().unwrap();
    }
}

/// A converted store whose source manifest has no `object_encoding` (the `v4-f1-e0` dumps name
/// no feature) lists it beside `snapshot_delta`: its deltas are encoding-1 records. So its own
/// dump names every feature its records use, and loads again with the same objects.
#[test]
fn sdl6_a_converted_featureless_store_dumps_and_loads_again() {
    let file = fixtures().join("dump").join("v4-f1-e0.rdydump");
    let t = tempfile::tempdir().unwrap();
    let to = t.path().join("p");
    load(&file, &to).unwrap();
    let s = Store::open_with_policy(&store_dir(&to), OpenPolicy::RequireCurrent).unwrap();
    for f in [
        rdy_store::format::FEATURE_OBJECT_ENCODING,
        FEATURE_SNAPSHOT_DELTA,
    ] {
        assert!(s.features().has(f), "{f}");
    }
    let before = store_objects(&s);
    drop(s);
    let out = t.path().join("again.rdydump");
    let r = Rdy::open(&to).unwrap();
    r.read().dump(&out).unwrap();
    r.close().unwrap();
    let again = t.path().join("again");
    load(&out, &again).unwrap_or_else(|e| panic!("a load of the converted store's dump: {e:?}"));
    let s = Store::open_with_policy(&store_dir(&again), OpenPolicy::RequireCurrent).unwrap();
    assert_eq!(store_objects(&s), before);
}

// ---------------------------------------------------------------------------------------------
// sdl7 (SD1 Task 7): deterministic bytes on every target, the long history and the fixtures
// ---------------------------------------------------------------------------------------------

/// A fixed hash: the BLAKE3 of `text`.
fn fixed(text: &str) -> Hash {
    Hash::of(text.as_bytes())
}

/// The fixed version of block `i` at generation `g`.
fn version(i: usize, g: usize) -> Hash {
    fixed(&format!("version {i}.{g}"))
}

fn map_of(pairs: &[(usize, usize)]) -> BTreeMap<BlockId, Hash> {
    pairs
        .iter()
        .map(|&(i, g)| (fx::block_id(i), version(i, g)))
        .collect()
}

/// One named delta of the golden, with the maps it joins.
struct Case {
    name: &'static str,
    delta: SnapshotDelta,
}

/// The delta of `landing` over `prev`, built by hand from the rule (the first sets the whole
/// map; a later one sets the changed or new pairs and drops the removed ids), and checked
/// against the product's `diff_snapshots` for the same maps.
fn rule_delta(
    landing: &str,
    base: Option<&Case>,
    prev: &BTreeMap<BlockId, Hash>,
    next: &BTreeMap<BlockId, Hash>,
) -> SnapshotDelta {
    let set: Vec<(BlockId, Hash)> = next
        .iter()
        .filter(|(k, v)| prev.get(*k) != Some(*v))
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    let drop: Vec<BlockId> = prev
        .keys()
        .filter(|k| !next.contains_key(*k))
        .cloned()
        .collect();
    let delta = SnapshotDelta {
        landing: fixed(landing),
        base: base.map(|c| c.delta.id()),
        full: rdy_model::snapshot_map_id(next),
        set,
        drop,
    };
    assert_eq!(
        delta,
        rdy_model::diff_snapshots(delta.landing, delta.base, prev, next),
        "{landing}: the rule and the product's diff agree"
    );
    delta
}

/// The golden's deltas: root, update, drop, no-op, fork, repeated full state, the serialization
/// length boundaries (a list of 127 and 128 entries: the count's varint grows by a byte) and a
/// map built in reverse insertion order.
fn golden_cases() -> Vec<Case> {
    let mut cases: Vec<Case> = vec![];
    let empty = BTreeMap::new();
    // root
    let m0 = map_of(&[(0, 0), (1, 0), (2, 0)]);
    cases.push(Case {
        name: "root_one",
        delta: rule_delta("root one", None, &empty, &map_of(&[(0, 0)])),
    });
    let root = rule_delta("landing 1", None, &empty, &m0);
    cases.push(Case {
        name: "root_three",
        delta: root,
    });
    // update: one changed, one new
    let m1 = map_of(&[(0, 0), (1, 1), (2, 0), (3, 0)]);
    let upd = rule_delta("landing 2", Some(&cases[1]), &m0, &m1);
    cases.push(Case {
        name: "update",
        delta: upd,
    });
    // drop
    let m2 = map_of(&[(0, 0), (1, 1), (3, 0)]);
    let drp = rule_delta("landing 3", Some(&cases[2]), &m1, &m2);
    cases.push(Case {
        name: "drop",
        delta: drp,
    });
    // set and drop together
    let m3 = map_of(&[(0, 2), (1, 1), (4, 0)]);
    let both = rule_delta("landing 4", Some(&cases[3]), &m2, &m3);
    cases.push(Case {
        name: "set_and_drop",
        delta: both,
    });
    // no-op: the same map, a delta of its own
    let noop = rule_delta("landing 5", Some(&cases[4]), &m3, &m3);
    assert!(noop.set.is_empty() && noop.drop.is_empty());
    cases.push(Case {
        name: "noop",
        delta: noop,
    });
    // fork: two landings on the same base, with different landings and different sets
    let fork_a = rule_delta(
        "fork a",
        Some(&cases[4]),
        &m3,
        &map_of(&[(0, 2), (1, 1), (4, 1)]),
    );
    let fork_b = rule_delta(
        "fork b",
        Some(&cases[4]),
        &m3,
        &map_of(&[(0, 2), (1, 3), (4, 0)]),
    );
    assert_eq!(fork_a.base, fork_b.base);
    cases.push(Case {
        name: "fork_a",
        delta: fork_a,
    });
    cases.push(Case {
        name: "fork_b",
        delta: fork_b,
    });
    // repeated full state: the map of the root again, two landings later; the same `full` under
    // a different landing and a different delta
    let x = m0.clone();
    let y = map_of(&[(0, 5), (1, 5), (2, 5)]);
    let to_y = rule_delta("repeat 2", Some(&cases[1]), &x, &y);
    let to_y_case = Case {
        name: "repeat_to_y",
        delta: to_y,
    };
    let back = rule_delta("repeat 3", Some(&to_y_case), &y, &x);
    assert_eq!(back.full, cases[1].delta.full, "the root's full map again");
    assert_ne!(back.id(), cases[1].delta.id());
    cases.push(to_y_case);
    cases.push(Case {
        name: "repeat_back_to_x",
        delta: back,
    });
    // length boundaries: 127 and 128 set entries (the count's varint is one byte, then two), and
    // 127 and 128 drops
    for (name, n) in [("set_127", 127usize), ("set_128", 128)] {
        let next: BTreeMap<BlockId, Hash> =
            (0..n).map(|i| (fx::block_id(i), version(i, 0))).collect();
        cases.push(Case {
            name,
            delta: rule_delta(name, None, &empty, &next),
        });
    }
    for (name, n) in [("drop_127", 127usize), ("drop_128", 128)] {
        let prev: BTreeMap<BlockId, Hash> =
            (0..n).map(|i| (fx::block_id(i), version(i, 0))).collect();
        let mut parent = Case {
            name: "unused",
            delta: rule_delta(name, None, &empty, &prev),
        };
        parent.delta.landing = fixed(&format!("{name} parent"));
        cases.push(Case {
            name,
            delta: rule_delta(name, Some(&parent), &prev, &empty),
        });
    }
    // reverse insertion order: the same 128 pairs inserted last to first give the same map,
    // so the same delta bytes as `set_128`
    let mut reversed = BTreeMap::new();
    for i in (0..128).rev() {
        reversed.insert(fx::block_id(i), version(i, 0));
    }
    let rev = rule_delta("set_128", None, &empty, &reversed);
    assert_eq!(
        rev,
        cases.iter().find(|c| c.name == "set_128").unwrap().delta,
        "reverse insertion changes nothing"
    );
    cases
}

/// The stored bytes of a delta: raw kind and payload (the encoding prefix included).
fn stored(delta: &SnapshotDelta) -> (u8, Vec<u8>) {
    codec::item_new(kinds::SNAPSHOT_DELTA, delta)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The small recipe the golden pins whole: three blocks, three landings, one set per later
/// landing. Every landing, state and delta id, and each delta's bytes.
const GOLDEN_RECIPE: fx::Recipe = fx::Recipe {
    blocks: 3,
    landings: 3,
    sets: 1,
};

fn golden_document() -> Value {
    let cases: Vec<Value> = golden_cases()
        .iter()
        .map(|c| {
            let (raw, payload) = stored(&c.delta);
            json!({
                "name": c.name,
                "raw_kind": raw,
                "landing": c.delta.landing.to_string(),
                "base": c.delta.base.map(|b| b.to_string()),
                "full": c.delta.full.to_string(),
                "set": c.delta.set.len(),
                "drop": c.delta.drop.len(),
                "payload_len": payload.len(),
                "payload_hex": hex(&payload),
                "delta": Hash::of(&payload).to_string(),
            })
        })
        .collect();
    let h = GOLDEN_RECIPE.build();
    let chain: Vec<Value> = (0..h.landings.len())
        .map(|n| {
            let delta: SnapshotDelta = codec::decode_record(
                kinds::SNAPSHOT_DELTA | 0x80,
                &h.items
                    .iter()
                    .find(|(k, p)| {
                        *k == (kinds::SNAPSHOT_DELTA | 0x80) && Hash::of(p) == h.deltas[n]
                    })
                    .unwrap()
                    .1,
            )
            .unwrap();
            json!({
                "landing": h.landings[n].to_string(),
                "state": h.states[n].to_string(),
                "delta": h.deltas[n].to_string(),
                "delta_payload_hex": hex(&stored(&delta).1),
            })
        })
        .collect();
    json!({
        "format": "snapshot-delta-encoding-v1",
        "raw_kind": 30 | 0x80,
        "encoding": 1,
        "cases": cases,
        "recipe": {"blocks": 3, "landings": 3, "sets": 1, "landings_in_order": chain},
    })
}

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/snapshot-delta/encoding-v1.json")
}

/// The wire form of a delta, laid out by hand from the postcard rules: a two-byte little-endian
/// encoding version, the landing's 32 bytes, an option byte and the base's 32 bytes, the full
/// hash's 32 bytes, a varint count and each pair's id (a varint length and its bytes) and
/// version bytes, then a varint count of dropped ids.
#[test]
fn sdl7_a_small_delta_has_the_hand_laid_out_bytes() {
    let d = SnapshotDelta {
        landing: fixed("hand landing"),
        base: None,
        full: fixed("hand full"),
        set: vec![(fx::block_id(0), version(0, 0))],
        drop: vec![],
    };
    let mut want = vec![1u8, 0];
    want.extend(fixed("hand landing").0);
    want.push(0);
    want.extend(fixed("hand full").0);
    want.push(1);
    want.push(9);
    want.extend(b"blk-00000");
    want.extend(version(0, 0).0);
    want.push(0);
    assert_eq!(stored(&d), (30 | 0x80, want.clone()));
    assert_eq!(d.id(), Hash::of(&want));
    // with a base the option byte is 1 and the base follows
    let with_base = SnapshotDelta {
        base: Some(fixed("hand base")),
        ..d
    };
    let mut want = vec![1u8, 0];
    want.extend(fixed("hand landing").0);
    want.push(1);
    want.extend(fixed("hand base").0);
    want.extend(fixed("hand full").0);
    want.push(1);
    want.push(9);
    want.extend(b"blk-00000");
    want.extend(version(0, 0).0);
    want.push(0);
    assert_eq!(stored(&with_base).1, want);
}

/// The bytes and the BLAKE3 ids of the canonical deltas are the same on every target: this test
/// compares each stored payload, in full, with the committed hex golden, and every platform's
/// gate runs it (macOS arm64, Linux amd64, Windows 11 x64). It never compares two runs on one
/// machine. Rewrite the golden once with `RDY_SD1_WRITE_GOLDEN=1`, then commit it.
#[test]
fn sdl7_delta_bytes_are_identical_on_all_targets() {
    let now = golden_document();
    if std::env::var_os("RDY_SD1_WRITE_GOLDEN").is_some() {
        std::fs::create_dir_all(golden_path().parent().unwrap()).unwrap();
        std::fs::write(
            golden_path(),
            serde_json::to_string_pretty(&now).unwrap() + "\n",
        )
        .unwrap();
    }
    let text = std::fs::read_to_string(golden_path())
        .unwrap_or_else(|e| panic!("{}: {e}", golden_path().display()));
    // the committed text is read as bytes of this repository on this target: a checkout that
    // changed its line endings would change the hex only if it changed the file
    assert!(!text.contains('\r'), "the golden has no carriage return");
    let want: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(want["raw_kind"], json!(158));
    assert_eq!(want["encoding"], json!(1));
    let cases = want["cases"].as_array().unwrap();
    let live = now["cases"].as_array().unwrap();
    assert_eq!(cases.len(), live.len());
    for (w, n) in cases.iter().zip(live) {
        assert_eq!(w, n, "{}: stored bytes or id moved", w["name"]);
        // the id is the hash of those exact bytes
        let payload: Vec<u8> = (0..w["payload_hex"].as_str().unwrap().len() / 2)
            .map(|i| {
                u8::from_str_radix(&w["payload_hex"].as_str().unwrap()[2 * i..2 * i + 2], 16)
                    .unwrap()
            })
            .collect();
        assert_eq!(payload.len() as u64, w["payload_len"].as_u64().unwrap());
        assert_eq!(Hash::of(&payload).to_string(), w["delta"].as_str().unwrap());
        // the bytes decode, name the golden's own fields, and re-encode to themselves
        let delta: SnapshotDelta = codec::decode_record(158, &payload).unwrap();
        assert_eq!(delta.landing.to_string(), w["landing"].as_str().unwrap());
        assert_eq!(delta.full.to_string(), w["full"].as_str().unwrap());
        assert_eq!(delta.set.len() as u64, w["set"].as_u64().unwrap());
        assert_eq!(delta.drop.len() as u64, w["drop"].as_u64().unwrap());
        assert_eq!(stored(&delta), (158, payload));
    }
    assert_eq!(
        want["recipe"], now["recipe"],
        "the small recipe's whole history"
    );
    // the length boundary shows in the bytes: a 127-entry and a 128-entry set differ by the
    // entry itself and by the count's second varint byte
    let len = |name: &str| {
        cases.iter().find(|c| c["name"] == json!(name)).unwrap()["payload_len"]
            .as_u64()
            .unwrap()
    };
    let entry = 1 + 9 + 32;
    assert_eq!(len("set_128") - len("set_127"), entry + 1);
    assert_eq!(len("drop_128") - len("drop_127"), 1 + 9 + 1);
}

/// The recipe's own maps and the product's fold agree on every landing of a small history, and
/// the history is the one the golden pins.
#[test]
fn sdl7_recipe_deltas_fold_to_the_independent_maps() {
    let r = fx::Recipe {
        blocks: 12,
        landings: 9,
        sets: 3,
    };
    let h = r.build();
    assert_eq!(h.landings.len(), 9);
    assert_eq!(h.deltas.len(), 9);
    assert_eq!(h.objects, fx::object_count(&r));
    let mut folded: BTreeMap<BlockId, Hash> = BTreeMap::new();
    let mut deltas = h.items.iter().filter(|(k, _)| *k == 158);
    for n in 0..9 {
        let (_, payload) = deltas.next().unwrap();
        let d: SnapshotDelta = codec::decode_record(158, payload).unwrap();
        assert_eq!(d.id(), h.deltas[n]);
        assert_eq!(d.landing, h.landings[n]);
        rdy_model::fold_snapshot_delta(&mut folded, &d).unwrap();
        assert_eq!(folded, r.expected_map(n), "landing {n}");
        assert_eq!(
            rdy_model::snapshot_map_id(&folded),
            h.states[n],
            "landing {n}"
        );
    }
    assert_eq!(folded, h.final_map);
    // the same recipe twice gives the same bytes
    assert_eq!(r.build().items, h.items);
    // a legacy history of the recipe has the same landings' maps in SNAPSHOTs and no delta
    let legacy = r.build_legacy();
    assert!(legacy.deltas.is_empty());
    assert_eq!(legacy.states, h.states);
    assert_eq!(
        legacy
            .items
            .iter()
            .filter(|(k, _)| *k == kinds::SNAPSHOT)
            .count(),
        9
    );
}

/// The recipe's arithmetic for the fixed workloads, without building one: the long history is
/// 1,049,600 logical entries and a small delta stream; the perf recipes keep their fixed sizes.
#[test]
fn sdl7_recipe_arithmetic_is_fixed() {
    assert_eq!(fx::logical_entries(&fx::LONG), 1_049_600);
    assert!(fx::logical_entries(&fx::LONG) > 1_000_000);
    // the delta stream stays well below a million objects and edges
    assert!(
        fx::object_count(&fx::LONG) < 20_000,
        "{}",
        fx::object_count(&fx::LONG)
    );
    assert_eq!(fx::delta_entries(&fx::LONG), 1024 + 1024);
    assert_eq!(fx::delta_entries(&fx::PERF_READ), 10_000 + 999 * 10);
    assert_eq!(fx::logical_entries(&fx::PERF_UPGRADE), 100_000_000);
}

// ---- the long history ----

use rdy_store::closure::{self, END_OF_LOG, StoreSource};
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::StreamHeader;

/// The fixed repo id of the long-history project and its holders.
fn long_repo() -> rdy_model::RepoId {
    rdy_model::RepoId([7; 16])
}

/// The closure of TAG `tag` in `store`, less `known`, as one batch file's bytes, with its
/// closure and the header it carries.
fn export_batch(
    store: &Store,
    tag: Hash,
    known: &BTreeSet<ObjectKey>,
) -> (Vec<u8>, closure::Closure, StreamHeader) {
    let reader = store.reader();
    let c = closure::closure(&reader, tag, known).unwrap();
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        long_repo(),
        tag,
        c.prerequisites.clone(),
    );
    let mut source = StoreSource {
        reader: &reader,
        end: END_OF_LOG,
    };
    let bytes = closure::write_batch(&mut source, &header, &c.records, Vec::new())
        .unwrap()
        .0;
    (bytes, c, header)
}

/// What a holder keeps of the long history, read back: every landing's full identity and map
/// against the recipe's independent maps, with the holder's own check passing first.
fn verify_long_holder(h: &Rdy, hist: &fx::History, upto: usize) {
    h.store().check_holder().unwrap();
    let r = h.store().reader();
    // an independent running map: the recipe's rule, never the product's fold
    let mut running = hist.recipe.expected_map(0);
    for n in 0..=upto {
        if n > 0 {
            for i in hist.recipe.set_numbers(n) {
                running.insert(fx::block_id(i), hist.recipe.version(i, n));
            }
        }
        let l = hist.landings[n];
        let rec = r.landing(&l).unwrap();
        assert_eq!(rec.state, hist.states[n], "landing {n}: its full identity");
        assert_eq!(
            rec.state,
            rdy_model::snapshot_map_id(&running),
            "landing {n}"
        );
        let got = r.snapshot_at(&l).unwrap();
        assert_eq!(got.id(), rec.state, "landing {n}: the read map's identity");
        assert!(got.blocks == running, "landing {n}: the read map");
        let (d, _) = r.snapshot_delta_of(&l).unwrap().unwrap();
        assert_eq!(d, hist.deltas[n], "landing {n}: its delta");
    }
}

/// D-7K4Z0 and the capacity bound: a history of 1,025 landings with 1,024 entries in every full
/// map is 1,049,600 logical snapshot entries, over the million that a full-SNAPSHOT history would
/// carry as links. As deltas it stays near twenty thousand objects, and a full upload with
/// nothing known to the receiver is accepted; the holder check passes and every landing keeps its
/// full identity. The incremental export keeps the export-walk fix: it emits only the new
/// landing's records and carries the held base delta as a typed prerequisite. The history goes
/// in as one assembled source batch and one take batch per holder; no durable full snapshot
/// exists in it.
#[test]
fn sdl7_upload_past_one_million_snapshot_entries_is_accepted() {
    let hist = fx::LONG.build();
    assert_eq!(fx::logical_entries(&fx::LONG), 1_049_600);
    assert!(hist.objects < 20_000, "{} objects", hist.objects);
    assert_eq!(hist.deltas.len(), 1025);
    assert!(
        hist.items
            .iter()
            .all(|(k, _)| (*k & 0x7f) != kinds::SNAPSHOT),
        "no durable full snapshot"
    );
    let last = fx::LONG.landings - 1;
    let t = tempfile::tempdir().unwrap();
    // one assembled source batch
    let mut src = Store::create(&t.path().join("src")).unwrap();
    let before = src.log_end();
    assert_eq!(hist.write_into(&mut src).unwrap(), hist.objects);
    assert!(src.log_end() > before);
    let v1 = src.put_tag(&hist.tag("v1.0.0", last - 1)).unwrap();
    let v2 = src.put_tag(&hist.tag("v2.0.0", last)).unwrap();

    // a full upload, nothing known to the receiver: one take batch
    let (bytes, c, header) = export_batch(&src, v2, &BTreeSet::new());
    assert!(
        header.prerequisites.is_empty(),
        "a full upload needs nothing"
    );
    assert!(c.records.len() < 20_000, "{} records", c.records.len());
    assert!(
        c.records
            .iter()
            .filter(|k| k.kind == kinds::SNAPSHOT_DELTA)
            .count()
            == 1025
    );
    let file = t.path().join("full.rdysync");
    std::fs::write(&file, &bytes).unwrap();
    let mut full = Rdy::create_holder(&t.path().join("full-holder"), long_repo()).unwrap();
    let outcome = take(
        &mut full,
        &[File::open(&file).unwrap()],
        UploadTarget {
            tag: "v2.0.0".into(),
            landing: hist.landings[last],
        },
    )
    .unwrap();
    let _ = outcome;
    verify_long_holder(&full, &hist, last);
    assert_eq!(
        full.store().reader().project_landings().unwrap().len(),
        1025
    );
    // the final map is the recipe's
    let tip = full
        .store()
        .reader()
        .snapshot_at(&hist.landings[last])
        .unwrap();
    assert!(tip.blocks == hist.final_map);

    // the incremental upload over v1.0.0's history: the export-walk fix still charges only what
    // it emits, and the held base delta travels as a typed prerequisite
    let (b1, c1, _) = export_batch(&src, v1, &BTreeSet::new());
    let known: BTreeSet<ObjectKey> = c1.records.iter().copied().collect();
    let (b2, c2, h2) = export_batch(&src, v2, &known);
    assert!(
        c2.records.len() < 12,
        "{} records emitted",
        c2.records.len()
    );
    assert!(c2.records.iter().all(|k| !known.contains(k)));
    let base_delta = ObjectKey {
        kind: kinds::SNAPSHOT_DELTA,
        hash: hist.deltas[last - 1],
    };
    assert!(
        h2.prerequisites.contains(&base_delta),
        "the held base delta is a typed prerequisite: {:?}",
        h2.prerequisites
    );
    let kept =
        closure::closure_kept_links_for_tests(&src.reader(), v2, &known, END_OF_LOG).unwrap();
    assert!(kept < 100, "{kept} links kept");
    let (f1, f2) = (t.path().join("v1.rdysync"), t.path().join("v2-inc.rdysync"));
    std::fs::write(&f1, &b1).unwrap();
    std::fs::write(&f2, &b2).unwrap();
    let mut inc = Rdy::create_holder(&t.path().join("inc-holder"), long_repo()).unwrap();
    take(
        &mut inc,
        &[File::open(&f1).unwrap()],
        UploadTarget {
            tag: "v1.0.0".into(),
            landing: hist.landings[last - 1],
        },
    )
    .unwrap();
    take(
        &mut inc,
        &[File::open(&f2).unwrap()],
        UploadTarget {
            tag: "v2.0.0".into(),
            landing: hist.landings[last],
        },
    )
    .unwrap();
    verify_long_holder(&inc, &hist, last);
}

// ---- the frozen SD1 fixtures ----

/// Each project landing with its delta id and its map: what a reader of the store sees.
fn delta_facts(s: &Store) -> Vec<(Hash, Hash, BTreeMap<BlockId, Hash>)> {
    let r = s.reader();
    r.project_landings()
        .unwrap()
        .into_iter()
        .map(|(l, _)| {
            let (d, _) = r
                .snapshot_delta_of(&l)
                .unwrap()
                .unwrap_or_else(|| panic!("landing {l} has a delta"));
            (l, d, r.snapshot_at(&l).unwrap().blocks)
        })
        .collect()
}

/// The native store fixture of the feature, its dump and its sync batch each roundtrip: a
/// project open reads the deltas the log holds (no upgrade); a reindex that rebuilds every
/// derived table from the log gives the same deltas and maps; a dump and a load carry every
/// object and delta; the committed dump loads to the objects of the native store; and a dump of
/// the loaded store loads again to the same.
#[test]
fn sdl7_delta_fixture_roundtrips_and_reindexes() {
    let from = fixtures().join("store/snapshot-delta-1");
    let (_keep, root) = rdy_qa::foreign_fixture::project_copy(&from);
    let mut r = Rdy::open(&root).unwrap();
    assert!(names(r.store()).iter().any(|n| n == FEATURE_SNAPSHOT_DELTA));
    let before = delta_facts(r.store());
    assert_eq!(before.len(), 3, "the fixture's three landings");
    assert!(before.iter().all(|(_, _, m)| !m.is_empty()));
    let objects = store_objects(r.store());
    assert!(
        !objects.keys().any(|(k, _)| *k == kinds::SNAPSHOT),
        "a current-writer history holds no SNAPSHOT"
    );
    assert_eq!(
        objects
            .keys()
            .filter(|(k, _)| *k == kinds::SNAPSHOT_DELTA)
            .count(),
        3
    );
    // the log is the source: a reindex gives the same table and maps
    r.reindex().unwrap();
    assert_eq!(delta_facts(r.store()), before, "after a reindex");
    r.fsck().unwrap();
    // a dump and its load
    let t = tempfile::tempdir().unwrap();
    let out = t.path().join("one.rdydump");
    r.read().dump(&out).unwrap();
    r.close().unwrap();
    let to = t.path().join("loaded");
    load(&out, &to).unwrap();
    let s = Store::open_with_policy(&store_dir(&to), OpenPolicy::RequireCurrent).unwrap();
    assert_eq!(store_objects(&s), objects, "a load carries every object");
    assert_eq!(delta_facts(&s), before, "and every delta and map");
    s.check_objects().unwrap();
    drop(s);
    // the committed dump of the same project loads to the same objects, deltas and maps
    let committed = fixtures().join("dump/v4-f1-e1-snapshot-delta.rdydump");
    let again = t.path().join("committed");
    load(&committed, &again).unwrap();
    let s = Store::open_with_policy(&store_dir(&again), OpenPolicy::RequireCurrent).unwrap();
    assert_eq!(store_objects(&s), objects, "the committed dump");
    assert_eq!(delta_facts(&s), before);
    // a second generation: dump the loaded project, load it, and nothing moved
    drop(s);
    let r2 = Rdy::open(&again).unwrap();
    let second = t.path().join("two.rdydump");
    r2.read().dump(&second).unwrap();
    r2.close().unwrap();
    assert_eq!(dump_objects(&second), dump_objects(&committed));
    let third = t.path().join("third");
    load(&second, &third).unwrap();
    let s = Store::open_with_policy(&store_dir(&third), OpenPolicy::RequireCurrent).unwrap();
    assert_eq!(delta_facts(&s), before, "the second generation");
    // the committed stream of the last tag takes into a holder with the same deltas
    let sync = fixtures().join("sync/v2-f1-e1-snapshot-delta.rdysync");
    let bytes = std::fs::read(&sync).unwrap();
    let (held, header) = stream_objects(&bytes);
    assert!(header.features.has(FEATURE_SNAPSHOT_DELTA));
    assert!(!held.keys().any(|(k, _)| *k == kinds::SNAPSHOT));
    let tag: Tag = decode(held.get(&(header.root().kind, header.root().hash)).unwrap());
    let mut h = Rdy::create_holder(&t.path().join("holder"), header.repo_id).unwrap();
    take(
        &mut h,
        &[File::open(&sync).unwrap()],
        UploadTarget {
            tag: tag.name.clone(),
            landing: tag.landing,
        },
    )
    .unwrap();
    h.store().check_holder().unwrap();
    for (l, d, m) in &before {
        if let Some((hd, _)) = h.store().reader().snapshot_delta_of(l).unwrap() {
            assert_eq!(&hd, d);
            assert_eq!(&h.store().reader().snapshot_at(l).unwrap().blocks, m);
        }
    }
    // the take brought the last tag's whole history: all three landings
    assert_eq!(h.store().reader().project_landings().unwrap().len(), 3);
}

/// The inventory text of the files under `dir`: one line each, `path size blake3`, in path
/// order (the same form `store_snapshot_delta_fixture` pins for the SD1 store).
fn inventory_text(dir: &Path) -> String {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                let bytes = std::fs::read(&p).unwrap();
                let rel = p
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(format!("{rel} {} {}\n", bytes.len(), Hash::of(&bytes)));
            }
        }
    }
    let mut lines = vec![];
    walk(dir, dir, &mut lines);
    lines.concat()
}

/// The first-use members that predate snapshot deltas never change: every file of the three
/// native store fixtures a released or released-candidate rdy froze (`feature-1`,
/// `figure-dark-1`, `foreign-copy-1`), and the dump pairs of `foreign_copy` and `figure_dark`.
/// Opening, loading or taking one writes only into copies (`sdl6_` tests), never into the
/// committed bytes, expect files or digests.
#[test]
fn sdl7_legacy_first_use_members_remain_immutable() {
    const STORE_INVENTORIES: [(&str, &str); 3] = [
        (
            "feature-1",
            "7c34c1362beaba05eda0592a7072d439dd5cad38823b5aaff961cf6a5b6bf523",
        ),
        (
            "figure-dark-1",
            "dfcd815f8d99e3272ca9efc1a63bef6ec6dbc26f5b92bc47d029c95232fdc65e",
        ),
        (
            "foreign-copy-1",
            "74853c3668027fcde980ca069459fd92bab1c5e3f3ae1b4e78c012ee4c1cb9a1",
        ),
    ];
    for (name, want) in STORE_INVENTORIES {
        let text = inventory_text(&fixtures().join("store").join(name));
        assert_eq!(Hash::of(text.as_bytes()).to_string(), want, "store/{name}");
        assert!(
            !text.contains("index.redb"),
            "store/{name}: no derived index"
        );
    }
    const DUMP_MEMBERS: [(&str, &str); 4] = [
        (
            "v4-f1-e1-foreign-copy.rdydump",
            "6aca7c40fefdff88db530fdbce96355f38e068c85e65ba4e8e627b9977a78bbf",
        ),
        (
            "v4-f1-e1-foreign-copy.expect.json",
            "1d74a9af451d64935717c941b16fb656888f740ff655bb6a681ef00b0b72afd3",
        ),
        (
            "v4-f1-e1-figure-dark.rdydump",
            "a64524b4ae80f73235ae9b06b773b0e15cfe4ca34be859b81bdeb33474969aa0",
        ),
        (
            "v4-f1-e1-figure-dark.expect.json",
            "de7494771ae5abb3ee975a270b222d0e22b63eff45077bba96fc7ceb9f50f7ba",
        ),
    ];
    for (name, want) in DUMP_MEMBERS {
        let bytes = std::fs::read(fixtures().join("dump").join(name)).unwrap();
        assert_eq!(Hash::of(&bytes).to_string(), want, "dump/{name}");
    }
    // a use of each fixture the way the SD1 paths use it leaves the committed store untouched:
    // every store member opens as a copy, and the source inventory is as it was after
    for (name, _) in STORE_INVENTORIES {
        let from = fixtures().join("store").join(name);
        let before = inventory_text(&from);
        let (_keep, root) = rdy_qa::foreign_fixture::project_copy(&from);
        Rdy::open(&root).unwrap().close().unwrap();
        assert_eq!(
            inventory_text(&from),
            before,
            "store/{name} after a copy's open"
        );
    }
}

/// A small streamed legacy project (the form the large upgrade fixture takes) is a legacy store
/// in every sense, and the writer's open upgrades it to exactly the deltas the recipe builds by
/// its own rule: same ids, same maps, one batch, and the project checks clean. The recipe's
/// maps and the delta bytes are made without the product's fold or diff.
#[test]
fn sdl7_streamed_legacy_fixture_upgrades_to_the_recipe_deltas() {
    let recipe = fx::Recipe {
        blocks: 30,
        landings: 11,
        sets: 4,
    };
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("p");
    let facts = fx::legacy_project(&root, recipe, 4);
    assert_eq!(facts.landings.len(), 11);
    assert_eq!(facts.batches, 3, "eleven landings, four to a batch");
    let want = recipe.build();
    // the legacy landings are the delta history's landings plus the SNAPSHOT instead: the state
    // ids agree, the landing ids too (the records name the same state and proposals)
    assert_eq!(facts.states, want.states);
    assert_eq!(facts.landings, want.landings);
    // the legacy store holds no feature and no delta
    {
        let s = Store::open_with_policy(&store_dir(&root), OpenPolicy::RequireCurrent).unwrap();
        assert!(!names(&s).iter().any(|n| n == FEATURE_SNAPSHOT_DELTA));
        let objects = store_objects(&s);
        assert!(!objects.keys().any(|(k, _)| *k == kinds::SNAPSHOT_DELTA));
        assert_eq!(
            objects
                .keys()
                .filter(|(k, _)| *k == kinds::SNAPSHOT)
                .count(),
            11
        );
    }
    let report = rdy_engine::maintenance::run(&root, false).unwrap();
    assert!(report.changed && report.checked);
    assert_eq!(report.steps, ["snapshot_delta".to_string()]);
    assert!(report.batch.is_some() && report.backup.is_some());
    let r = Rdy::open(&root).unwrap();
    let s = r.store();
    let reader = s.reader();
    for (n, l) in want.landings.iter().enumerate() {
        let (d, _) = reader.snapshot_delta_of(l).unwrap().unwrap();
        assert_eq!(
            d, want.deltas[n],
            "landing {n}: the upgrade's delta is the recipe's"
        );
        assert!(reader.snapshot_at(l).unwrap().blocks == recipe.expected_map(n));
    }
    r.fsck().unwrap();
    s.check_objects().unwrap();
    // one upgrade: a second run changes nothing
    drop(reader);
    r.close().unwrap();
    let again = rdy_engine::maintenance::run(&root, false).unwrap();
    assert!(!again.changed);
}

/// The bench-only fixture of the large upgrade (D-918YB's scale): 10,000 legacy landings, each
/// with 10,000 entries and one changed entry per later landing, streamed in batches of fifty
/// landings; it holds gigabytes of full SNAPSHOTs, so it never runs in the fast gate. Run it on
/// the bench machine: `cargo test -p rdy-qa --release --test io sdl7_large_store_upgrade_bench_fixture -- --ignored`.
/// It builds the legacy project once, upgrades it with the writer's open, and verifies every
/// landing's full hash and the final map. The timed measurement is the perf row
/// `snapshot_delta_upgrade_large`, which copies a pristine source for each run.
#[test]
#[ignore = "bench only: builds 10,000 legacy landings of 10,000 entries (gigabytes of SNAPSHOTs)"]
fn sdl7_large_store_upgrade_bench_fixture() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("p");
    let facts = fx::legacy_project(&root, fx::PERF_UPGRADE, 50);
    assert_eq!(facts.landings.len(), 10_000);
    let report = rdy_engine::maintenance::run(&root, false).unwrap();
    assert!(report.changed && report.checked);
    let r = Rdy::open(&root).unwrap();
    let reader = r.store().reader();
    for (n, l) in facts.landings.iter().enumerate() {
        let rec = reader.landing(l).unwrap();
        assert_eq!(rec.state, facts.states[n], "landing {n}: its full identity");
        assert!(
            reader.snapshot_delta_of(l).unwrap().is_some(),
            "landing {n}"
        );
    }
    let last = facts.landings.last().unwrap();
    assert!(reader.snapshot_at(last).unwrap().blocks == facts.final_map);
}

/// sdl4 (final review F1, D-918YB): a verified foreign-removal proof's landing carries no
/// snapshot delta by design, so a removal report reads full maps for the project's own landings
/// only (`StoreReader::project_landings`, as fsck and upgrade classify them). On the frozen
/// foreign-copy dump, a dry-run removal of a local document reports instead of failing with
/// `expected exactly one snapshot delta`, and the proof landing still counts for references
/// and quotes as any landing does.
#[test]
fn sdl4_foreign_proof_landings_leave_local_removals_working() {
    let file = fixtures()
        .join("dump")
        .join("v4-f1-e1-foreign-copy.rdydump");
    let t = tempfile::tempdir().unwrap();
    let to = t.path().join("p");
    load(&file, &to).unwrap();
    let s = Store::open_with_policy(&store_dir(&to), OpenPolicy::RequireCurrent).unwrap();
    let reader = s.reader();
    let project: BTreeSet<Hash> = reader
        .project_landings()
        .unwrap()
        .into_iter()
        .map(|(h, _)| h)
        .collect();
    let mut held = BTreeSet::new();
    reader
        .for_each_object_of(kinds::LANDING, |h| {
            held.insert(h);
            Ok(())
        })
        .unwrap();
    assert!(
        held.len() > project.len(),
        "the fixture holds a foreign-removal proof landing outside the project's landings"
    );
    drop(reader);
    drop(s);
    let r = Rdy::open(&to).unwrap();
    let report = r
        .read()
        .redaction_report(&rdy_engine::redaction::RedactInput {
            targets: vec!["d-9XVJA".into()],
            reason_class: "secret".into(),
            reason: "review probe".into(),
            dry_run: true,
        })
        .unwrap_or_else(|e| panic!("dry-run removal of a local document: {e}"));
    assert!(
        report
            .versions
            .iter()
            .any(|(id, _)| id.as_str() == "d-9XVJA"),
        "the report selects the local document: {:?}",
        report.versions
    );
}
