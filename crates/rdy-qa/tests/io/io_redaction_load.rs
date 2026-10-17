//! rdx9: a redacted project moves through dump format 2 (plan Redaction, Task 9; D-3M81W: rdy
//! "drops their bytes and keeps their hashes"; D-69AY3: every released dump format stays
//! readable; D-FR0C4: a project moves with dump and load).
//!
//! The source holds the shared raw authorized batch (`redaction_fixture`), written while the
//! project is closed; the next open finishes the removal. A load, a clone of the open project and
//! a clone of its dump each read the one stream: every surviving hash, every absent payload with
//! its proof, the repo id and the origin stay.
#[path = "../../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

use rdy_engine::clone::{CloneSource, clone};
use rdy_engine::{E_DUMP, Rdy, load::load};
use rdy_model::Hash;
use rdy_model::redaction::RedactTarget;
use rdy_store::dump::{DumpItem, DumpReader, MAGIC, rec};
use rdy_store::format::{REF_STORE_FORMAT, STORE_FORMAT, encode_u32};
use rdy_store::kinds;
use rdy_store::meta::Origin;
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::Location;
use redaction_fixture::{FixtureOptions, ORIGIN, Proof, RequestSpec, authorized_batch_fixture};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const CANARY: &str = "CANARY removed body";
const HEAD: usize = 37;

/// One frame of a log as the record walker hands it out: its place, kind, hash and length, and
/// either its payload's hash or the apply and residue that authorize its absence.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Rec {
    loc: Location,
    kind: u8,
    hash: Hash,
    len: u32,
    proof: Result<Hash, (Hash, Option<Hash>)>,
}

fn records(r: &Rdy) -> Vec<Rec> {
    let store = r.store();
    let mut out = vec![];
    store
        .reader()
        .for_each_record_from(ORIGIN, store.log_end(), |loc, f, _| {
            out.push(match f {
                StoredFrame::Present {
                    kind,
                    hash,
                    payload,
                } => Rec {
                    loc,
                    kind,
                    hash,
                    len: payload.len() as u32,
                    proof: Ok(Hash::of(payload)),
                },
                StoredFrame::Dropped {
                    kind,
                    hash,
                    len,
                    apply,
                    residue,
                } => Rec {
                    loc,
                    kind,
                    hash,
                    len,
                    proof: Err((apply, residue)),
                },
            });
            Ok(())
        })
        .unwrap();
    out
}

/// Every segment file of the project's log, by name.
fn segments(project: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(project.join(".rdy/store/segments")).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with("seg-") && name.ends_with(".log") {
            out.insert(name, std::fs::read(&p).unwrap());
        }
    }
    out
}

/// Every path under `dir` with its bytes (None for a directory).
fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            }
            out.push(p);
        }
    }
    let mut paths = vec![];
    if dir.exists() {
        walk(dir, &mut paths);
    }
    paths
        .into_iter()
        .map(|p| {
            let rel = p.strip_prefix(dir).unwrap().to_path_buf();
            let bytes = (!p.is_dir()).then(|| std::fs::read(&p).unwrap());
            (rel, bytes)
        })
        .collect()
}

/// A project whose canon holds a doc, a canary prose block, a readable sibling and a blob, with
/// one applied request that removed every version of the canary block and the blob.
struct Redacted {
    dir: tempfile::TempDir,
    request: Hash,
    apply: Hash,
    blob: Hash,
}

fn redacted() -> Redacted {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = rdy_qa::fixtures::write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p1", "parent": "$d", "body": CANARY},
            {"op": "create", "type": "prose", "id": "$p2", "parent": "$d", "body": "Readable sibling."}
        ]),
    )
    .unwrap();
    rdy_qa::fixtures::land(&mut r, "work").unwrap();
    let mut bytes = format!("{CANARY} ").repeat(60_000).into_bytes();
    bytes.truncate((1 << 20) + 100); // two chunks
    let blob = r.store_mut().put_blob(&bytes).unwrap();
    let removed = rdy_model::BlockId::parse(&w.created["$p1"]).unwrap();
    r.close().unwrap();
    let f = authorized_batch_fixture(
        &dir.path().join(".rdy/store"),
        &[RequestSpec::new(
            "redact",
            vec![RedactTarget::AllVersions(removed), RedactTarget::Blob(blob)],
        )],
        &FixtureOptions::approved(Proof::local_human()),
    );
    // the open finishes the removal
    Rdy::open(dir.path()).unwrap().close().unwrap();
    Redacted {
        dir,
        request: f.requests[0].request,
        apply: f.applies[0].0,
        blob,
    }
}

fn origin_of(r: &Rdy) -> Option<Origin> {
    r.store().reader().origin().unwrap()
}

fn repo_id_of(r: &Rdy) -> rdy_model::RepoId {
    r.store().reader().repo_id().unwrap().unwrap()
}

/// The target `to` holds the source's log as a byte prefix of its own, and every source record
/// at its place: each surviving payload under its hash, each absent payload with its apply and
/// residue, and no byte of the canary.
fn keeps_the_source(src: &Path, src_recs: &[Rec], to: &Path, what: &str) {
    let (a, b) = (segments(src), segments(to));
    for (name, bytes) in &a {
        let got = &b[name];
        assert!(
            got.len() >= bytes.len() && got[..bytes.len()] == bytes[..],
            "{what}: {name} is not a prefix"
        );
    }
    for bytes in b.values() {
        assert!(
            !bytes.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()),
            "{what}: the log holds the removed text"
        );
    }
    let r = Rdy::open(to).unwrap();
    let got = records(&r);
    assert_eq!(&got[..src_recs.len()], src_recs, "{what}: every record");
    for rec in src_recs.iter().filter(|r| r.kind != kinds::BATCH_COMMIT) {
        assert!(
            r.store().has_kind(rec.kind, &rec.hash).unwrap(),
            "{what}: {} stays",
            rec.hash
        );
    }
    r.close().unwrap();
}

#[test]
fn rdx9_a_redacted_project_round_trips_and_clones() {
    let src = redacted();
    let a = Rdy::open(src.dir.path()).unwrap();
    let src_recs = records(&a);
    let absent: Vec<&Rec> = src_recs.iter().filter(|r| r.proof.is_err()).collect();
    let mut kinds_absent: Vec<u8> = absent.iter().map(|r| r.kind).collect();
    kinds_absent.dedup();
    assert_eq!(
        kinds_absent,
        [kinds::BLOCK, kinds::CHANGE, kinds::BLOB_CHUNK],
        "{absent:?}"
    );
    assert!(
        absent
            .iter()
            .all(|r| r.proof.as_ref().unwrap_err().0 == src.apply)
    );
    assert!(matches!(
        a.store().reader().get_raw(&src.blob),
        Ok((kinds::BLOB_MANIFEST, _))
    ));
    let id = repo_id_of(&a);
    assert_eq!(origin_of(&a), None, "a new project has no origin");
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("p.rdydump");
    let d = a.read().dump(&file).unwrap();
    assert_eq!((d.dump_format, d.store_format), (2, 13));
    let bytes = std::fs::read(&file).unwrap();
    assert!(
        !bytes.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()),
        "the dump holds the removed text"
    );
    // load: the log byte for byte, the repo id kept, no origin
    let loaded = scratch.path().join("loaded");
    let s = load(&file, &loaded).unwrap();
    assert_eq!((s.dump_format, s.store_format), (2, STORE_FORMAT));
    assert_eq!(segments(&loaded), segments(src.dir.path()));
    keeps_the_source(src.dir.path(), &src_recs, &loaded, "load");
    let l = Rdy::open(&loaded).unwrap();
    assert_eq!(repo_id_of(&l), id);
    assert_eq!(origin_of(&l), None);
    assert_eq!(
        l.store()
            .reader()
            .redaction_of(absent[0].kind, &absent[0].hash)
            .unwrap(),
        Some(src.request)
    );
    l.close().unwrap();
    // a clone of the open project and a clone of its dump: the log, the repo id, the origin
    let want = {
        let st = a.read();
        Origin {
            fork: false,
            repo_id: id,
            tag: st.versions().unwrap().last().map(|v| v.name.clone()),
            landing: st.canon_head(),
        }
    };
    let st = a.read();
    let local = scratch.path().join("local");
    clone(CloneSource::State(&st), &local, false).unwrap();
    drop(st);
    let filed = scratch.path().join("filed");
    clone(CloneSource::File(&file), &filed, false).unwrap();
    for (to, what) in [(&local, "local clone"), (&filed, "file clone")] {
        keeps_the_source(src.dir.path(), &src_recs, to, what);
        let c = Rdy::open(to).unwrap();
        assert_eq!(repo_id_of(&c), id, "{what}");
        assert_eq!(origin_of(&c), Some(want.clone()), "{what}");
        c.close().unwrap();
    }
    a.close().unwrap();
}

/// A frozen fixture's path.
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/dump")
        .join(name)
}

#[test]
fn rdx9_all_released_dump_pairs_still_load() {
    for (name, store_format) in [
        ("v1-s10.rdydump", 10),
        ("v1-s11.rdydump", 11),
        ("v1-s12.rdydump", 12),
    ] {
        let to = tempfile::tempdir().unwrap();
        let dir = to.path().join("p");
        let s = load(&fixture(name), &dir).unwrap();
        assert_eq!((s.dump_format, s.store_format), (1, store_format), "{name}");
        assert_eq!(s.fsck.unlogged_moves, 0, "{name}");
        let (mut rd, _, _) = DumpReader::open(std::fs::File::open(fixture(name)).unwrap()).unwrap();
        let r = Rdy::open(&dir).unwrap();
        let mut n = 0;
        while let Some(item) = rd.next().unwrap() {
            if let DumpItem::Batch(b) = item {
                for f in &b.frames {
                    assert!(f.present_payload().is_some(), "{name}: a format-1 frame");
                    assert!(
                        r.store().has_kind(f.kind(), f.hash()).unwrap(),
                        "{name}: kind {} {} is gone",
                        f.kind(),
                        f.hash()
                    );
                    n += 1;
                }
            }
        }
        assert!(n > 0, "{name}");
        assert_eq!(
            r.store().get_ref(REF_STORE_FORMAT).unwrap(),
            Some(encode_u32(STORE_FORMAT)),
            "{name}: the upgrade batch names this rdy's store format"
        );
        r.fsck().unwrap();
        r.close().unwrap();
        rdy_qa::reindex_check::reindex_reproduces(&dir)
            .unwrap_or_else(|v| panic!("{name}: {v:#?}"));
    }
}

/// The records of a dump after its prelude, as (kind, payload).
fn dump_records(good: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut at = 12usize;
    let mut out = vec![];
    while at < good.len() {
        let len = u32::from_le_bytes(good[at..at + 4].try_into().unwrap()) as usize;
        out.push((good[at + 4], good[at + HEAD..at + HEAD + len].to_vec()));
        at += HEAD + len;
    }
    out
}

/// A dump of format 2 of hash-valid `recs`, its trailer made true again.
fn forge(recs: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let n = recs.len() - 1;
    let mut chain = vec![];
    for (k, p) in &recs[..n] {
        chain.push(*k);
        chain.extend_from_slice(&(p.len() as u64).to_le_bytes());
        chain.extend_from_slice(&Hash::of(p).0);
    }
    let count = |f: &dyn Fn(u8) -> bool| recs[..n].iter().filter(|(k, _)| f(*k)).count() as u64;
    let mut trailer = count(&|k| k == rec::BATCH).to_le_bytes().to_vec();
    trailer.extend_from_slice(&count(&|k| k < rec::HEADER || k == rec::DROPPED).to_le_bytes());
    trailer.extend_from_slice(&(count(&|k| k == rec::FILE) as u32).to_le_bytes());
    trailer.extend_from_slice(&Hash::of(&chain).0);
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&2u32.to_le_bytes());
    for (k, p) in recs[..n].iter().chain([&(rec::TRAILER, trailer)]) {
        out.extend_from_slice(&(p.len() as u32).to_le_bytes());
        out.push(*k);
        out.extend_from_slice(&Hash::of(p).0);
        out.extend_from_slice(p);
    }
    out
}

#[test]
fn rdx9_a_forged_drop_preserves_the_target() {
    let src = redacted();
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("p.rdydump");
    let a = Rdy::open(src.dir.path()).unwrap();
    a.read().dump(&file).unwrap();
    a.close().unwrap();
    let good = std::fs::read(&file).unwrap();
    let recs = dump_records(&good);
    let i = recs
        .iter()
        .position(|(k, p)| *k == rec::DROPPED && p[0] == kinds::BLOCK)
        .unwrap();
    let j = (0..i).rev().find(|j| recs[*j].0 == rec::BATCH).unwrap();
    type Bend = fn(&mut Vec<(u8, Vec<u8>)>, usize, usize);
    let cases: [(&str, Bend, &str); 5] = [
        (
            "length",
            |r, i, _| {
                r[i].1.pop();
            },
            "dropped frame has the wrong length",
        ),
        (
            "kind",
            |r, i, _| r[i].1[0] = kinds::TAG,
            "dropped frame has an invalid kind",
        ),
        (
            "residue",
            |r, i, _| r[i].1[69] = 2,
            "dropped frame has an invalid residue",
        ),
        (
            "apply",
            |r, i, _| r[i].1[37] ^= 1,
            "dropped frame has no valid authorization",
        ),
        (
            "offset",
            |r, _, j| r[j].1[4..12].copy_from_slice(&(u64::MAX - 40).to_le_bytes()),
            "dropped frame does not match its authorized span",
        ),
    ];
    for (name, bend, want) in cases {
        let mut r = recs.clone();
        bend(&mut r, i, j);
        let forged = scratch.path().join(format!("{name}.rdydump"));
        std::fs::write(&forged, forge(&r)).unwrap();
        // a target that holds a file of its own
        let to = scratch.path().join(format!("to-{name}"));
        std::fs::create_dir(&to).unwrap();
        std::fs::write(to.join("keep.txt"), b"mine").unwrap();
        let before = tree(&to);
        let e = load(&forged, &to).unwrap_err();
        assert_eq!(e.code, E_DUMP, "{name}: {e:?}");
        assert!(
            e.message
                .starts_with(&format!("{} is damaged at byte ", forged.display()))
                && e.message.ends_with(&format!(": {want}")),
            "{name}: {}",
            e.message
        );
        assert_eq!(e.fix, "dump the project again; rdy never repairs a dump");
        assert_eq!(tree(&to), before, "{name}: the target is as it was");
        // a clone of the forged dump refuses it the same way
        let c = scratch.path().join(format!("clone-{name}"));
        let e = clone(CloneSource::File(&forged), &c, false).unwrap_err();
        assert_eq!(e.code, E_DUMP, "{name}: clone");
        assert!(
            e.message.ends_with(&format!(": {want}")),
            "{name}: clone: {}",
            e.message
        );
        assert!(!c.exists(), "{name}: the clone wrote nothing");
    }
    // and the unforged stream, through the same helper, loads
    let same = scratch.path().join("same.rdydump");
    std::fs::write(&same, forge(&recs)).unwrap();
    assert_eq!(std::fs::read(&same).unwrap(), good);
    load(&same, &scratch.path().join("ok")).unwrap();
}

#[test]
fn rdx9_a_drop_claim_over_the_frame_bound_preserves_the_target() {
    let src = redacted();
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("p.rdydump");
    let a = Rdy::open(src.dir.path()).unwrap();
    a.read().dump(&file).unwrap();
    a.close().unwrap();
    let good = std::fs::read(&file).unwrap();
    let recs = dump_records(&good);
    let i = recs
        .iter()
        .position(|(k, p)| *k == rec::DROPPED && p[0] == kinds::BLOCK)
        .unwrap();
    let at = 12 + recs[..i].iter().map(|(_, p)| HEAD + p.len()).sum::<usize>();
    // the dropped block claims one byte more than any stored payload holds
    let mut r = recs.clone();
    r[i].1[33..37].copy_from_slice(&(64u32 * 1024 * 1024 + 1).to_le_bytes());
    let forged = scratch.path().join("claim.rdydump");
    std::fs::write(&forged, forge(&r)).unwrap();
    let to = scratch.path().join("to");
    std::fs::create_dir(&to).unwrap();
    std::fs::write(to.join("keep.txt"), b"mine").unwrap();
    let before = tree(&to);
    let e = load(&forged, &to).unwrap_err();
    assert_eq!(e.code, E_DUMP, "{e:?}");
    assert_eq!(
        e.message,
        format!(
            "{} is damaged at byte {at}: a dropped frame claims 67108865 bytes; a frame holds at most 67108864",
            forged.display()
        )
    );
    assert_eq!(
        e.fix,
        "the dump is damaged or was not written by rdy; dump the source again"
    );
    assert_eq!(tree(&to), before, "the target is as it was");
    // a clone of the forged dump refuses it the same way, and writes nothing
    let c = scratch.path().join("clone");
    let e = clone(CloneSource::File(&forged), &c, false).unwrap_err();
    assert_eq!(e.code, E_DUMP);
    assert!(
        e.message
            .ends_with(": a dropped frame claims 67108865 bytes; a frame holds at most 67108864"),
        "clone: {}",
        e.message
    );
    assert!(!c.exists(), "the clone wrote nothing");
}

/// The value of `const <name>: &str = "<value>";` in a guard's source.
fn guard_const(source: &str, name: &str) -> String {
    let at = source
        .find(&format!("const {name}: &str = \""))
        .unwrap_or_else(|| panic!("{name} is gone from its guard"));
    let rest = &source[at..];
    let start = rest.find('"').unwrap() + 1;
    let len = rest[start..].find('"').unwrap();
    rest[start..start + len].to_string()
}

/// One released fixture's regular guard: its source, the fixture's stem, the names of its two
/// checksum constants, its generator's signature and the store format its generator pins.
type Guard = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
);

#[test]
fn rdx9_fixture_generators_and_bytes_are_unchanged() {
    // each released fixture against the checksum its regular guard holds; each generator stays
    // ignored, and this test never runs one
    let guards: [Guard; 3] = [
        (
            include_str!("io_dump_fixture.rs"),
            "v1-s10",
            "FIXTURE_BLAKE3",
            "EXPECT_BLAKE3",
            "fn dl8_write_the_v1_fixture()",
            None,
        ),
        (
            include_str!("io_dump_fixture_s11.rs"),
            "v1-s11",
            "FIXTURE_S11_BLAKE3",
            "EXPECT_S11_BLAKE3",
            "fn di2_write_the_v1_s11_fixture()",
            Some("assert_eq!(STORE_FORMAT, 11"),
        ),
        (
            include_str!("io_dump_fixture_s12.rs"),
            "v1-s12",
            "FIXTURE_BLAKE3",
            "EXPECT_BLAKE3",
            "fn ec13_write_the_v1_s12_fixture()",
            Some("assert_eq!(STORE_FORMAT, 12"),
        ),
    ];
    for (source, stem, dump_const, expect_const, generator, pinned) in guards {
        let dump = std::fs::read(fixture(&format!("{stem}.rdydump"))).unwrap();
        assert_eq!(
            Hash::of(&dump).to_string(),
            guard_const(source, dump_const),
            "{stem}: the dump's bytes"
        );
        let expect = std::fs::read(fixture(&format!("{stem}.expect.json"))).unwrap();
        assert_eq!(
            Hash::of(&expect).to_string(),
            guard_const(source, expect_const),
            "{stem}: the expect file's bytes"
        );
        assert!(
            source.contains(&format!("#[test]\n#[ignore]\n{generator}")),
            "{stem}: the generator stays ignored"
        );
        if let Some(pinned) = pinned {
            assert!(
                source.contains(pinned),
                "{stem}: the generator keeps its store format"
            );
        }
        // the frozen header: dump format 1 and its own store format
        let (_, format, h) = DumpReader::open(&dump[..]).unwrap();
        assert_eq!(format, 1, "{stem}");
        assert_eq!(h.store_format.to_string(), stem[4..], "{stem}");
    }
}
