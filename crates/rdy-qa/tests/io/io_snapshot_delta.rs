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
use rdy_store::dump::{DumpItem, DumpReader, Frame};
use rdy_store::format::FEATURE_SNAPSHOT_DELTA;
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::Location;
use rdy_store::sync_stream::{StreamReader, StreamRecord};
use rdy_store::take_batch::UploadTarget;
use rdy_store::{OpenPolicy, Store, codec, kinds};
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
