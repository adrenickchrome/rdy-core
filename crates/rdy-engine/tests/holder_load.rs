//! Holder dump and load (D-WWAK7, D-69AY3, D-FR0C4, D-BPYN8): a holder dumps with no project
//! file, loads as a holder with no config or pack file and no canon, keeps its repo id, tags,
//! mode and every object hash, and is refused as a clone source. The fixtures copy real
//! projects' objects into a holder, so two tags can name divergent histories.
mod common;

use rdy_engine::clone::{CloneSource, clone};
use rdy_engine::*;
use rdy_model::*;
use rdy_store::dump::{DumpHeader, DumpItem, DumpReader, DumpWriter, RemovalFacts, RemovalTally};
use rdy_store::dump_chain::{BatchMarker, batch_end};
use rdy_store::holder::StoreMode;
use rdy_store::kinds;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::io::BufReader;
use std::path::Path;

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn write(r: &mut Rdy, ops: Value) -> WriteResult {
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

fn land(r: &mut Rdy, title: &str) {
    let p = r
        .propose(&writer(), "work", title, Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
}

fn tag(r: &mut Rdy, name: &str) {
    r.tag(&founder(), name, None, None).unwrap();
}

/// A project loaded from a dump of `r`: the same history, now free to diverge.
fn fork(r: &Rdy, root: &Path, name: &str) -> Rdy {
    let file = root.join(format!("{name}.rdydump"));
    r.read().dump(&file).unwrap();
    let dir = root.join(name);
    load::load(&file, &dir).unwrap();
    Rdy::open(&dir).unwrap()
}

/// `features` and the `assessment` store feature.
fn and_assessment(features: rdy_store::format::StoreFeatures) -> rdy_store::format::StoreFeatures {
    let mut f = features.features;
    f.extend(
        rdy_store::format::registered_features()
            .into_iter()
            .filter(|x| x.name == rdy_store::format::FEATURE_ASSESSMENT),
    );
    rdy_store::format::StoreFeatures::of(f)
}

const COPIED: [u8; 9] = [
    kinds::BLOCK,
    kinds::ASSESSMENT,
    kinds::CHANGE,
    kinds::PROPOSAL,
    kinds::LANDING,
    kinds::SNAPSHOT,
    kinds::BLOB_CHUNK,
    kinds::BLOB_MANIFEST,
    kinds::TAG,
];

fn objects(src: &Rdy) -> Vec<(u8, Vec<u8>)> {
    let mut out = vec![];
    src.store()
        .for_each_frame_from(ORIGIN, src.store().log_end(), |_, kind, _, payload, _| {
            if COPIED.contains(&rdy_store::codec::logical_kind(kind)) {
                out.push((kind, payload.to_vec()));
            }
            Ok(())
        })
        .unwrap();
    out
}

fn tag_refs(src: &Rdy, names: &[&str]) -> Vec<(String, Vec<u8>)> {
    src.store()
        .tags()
        .unwrap()
        .into_iter()
        .filter(|(n, _, _)| names.contains(&n.as_str()))
        .map(|(n, h, _)| (format!("tag/{n}"), h.0.to_vec()))
        .collect()
}

/// Two projects share a first landing (v1.0.0), then diverge: the left one lands v1.1.0, the
/// right one v2.0.0. Their objects and three tags fill a holder at `<root>/holder`, closed.
fn divergent_holder(root: &Path, repo_id: RepoId) -> std::path::PathBuf {
    let mut left = Rdy::init(&root.join("left"), "rdy-standard").unwrap();
    let w = write(
        &mut left,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "base"}
        ]),
    );
    let prose = BlockId::parse(&w.created["$p"]).unwrap();
    land(&mut left, "first");
    tag(&mut left, "v1.0.0");
    let mut right = fork(&left, root, "right");
    write(
        &mut left,
        json!([{"op": "update", "id": prose.to_string(), "body": "left"}]),
    );
    land(&mut left, "left");
    tag(&mut left, "v1.1.0");
    write(
        &mut right,
        json!([{"op": "update", "id": prose.to_string(), "body": "right"}]),
    );
    land(&mut right, "right");
    tag(&mut right, "v2.0.0");
    let dir = root.join("holder");
    let mut items = objects(&left);
    items.extend(objects(&right));
    let mut refs = tag_refs(&left, &["v1.0.0", "v1.1.0"]);
    refs.extend(tag_refs(&right, &["v2.0.0"]));
    let mut h = Rdy::create_holder(&dir, repo_id).unwrap();
    h.store_mut().write(items).unwrap();
    h.store_mut().set_refs(refs).unwrap();
    h.seal_open().unwrap();
    h.close().unwrap();
    dir
}

/// Every frame hash of `r`'s log of a copied kind.
fn object_hashes(r: &Rdy) -> BTreeSet<Hash> {
    let mut out = BTreeSet::new();
    r.store()
        .for_each_frame_from(ORIGIN, r.store().log_end(), |_, kind, hash, _, _| {
            if COPIED.contains(&rdy_store::codec::logical_kind(kind)) {
                out.insert(hash);
            }
            Ok(())
        })
        .unwrap();
    out
}

fn tag_pairs(r: &Rdy) -> Vec<(String, Hash)> {
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .map(|(n, h, _)| (n, h))
        .collect()
}

/// The project files and items of the dump `file`: the paths of its FILE records, and its
/// batches.
type Files = Vec<(String, Vec<u8>)>;

fn dump_items(file: &Path) -> (DumpHeader, Files, Vec<DumpItem>) {
    let f = BufReader::new(std::fs::File::open(file).unwrap());
    let (mut reader, _, header) = DumpReader::open(f).unwrap();
    let (mut files, mut batches) = (vec![], vec![]);
    while let Some(item) = reader.next().unwrap() {
        match item {
            DumpItem::File { path, bytes } => files.push((path, bytes)),
            b @ DumpItem::Batch(_) => batches.push(b),
        }
    }
    (header, files, batches)
}

fn assert_err(e: &RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e}"
    );
}

/// The entries below `dir`, relative, sorted: the target as a person would see it.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, at: &Path, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(at) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            out.push(p.strip_prefix(base).unwrap().display().to_string());
            if p.is_dir() {
                walk(base, &p, out);
            }
        }
    }
    let mut out = vec![];
    walk(dir, dir, &mut out);
    out.sort();
    out
}

#[test]
fn ha12_holder_dump_load_preserves_mode_identity_and_divergent_tags() {
    let root = tempfile::tempdir().unwrap();
    let repo_id = RepoId([7; 16]);
    let src_dir = divergent_holder(root.path(), repo_id);
    let src = Rdy::open_holder(&src_dir).unwrap();
    let dump = root.path().join("h.rdydump");
    let summary = src.read().dump(&dump).unwrap();
    assert!(summary.files.is_empty(), "{summary:?}");
    assert_eq!(
        summary.store_format,
        // the holder took landings, and each names its assessment
        rdy_store::format::DumpStore::Features(and_assessment(
            rdy_store::format::baseline_features()
        ))
    );

    let target = root.path().join("loaded");
    let loaded = load::load(&dump, &target).unwrap();
    assert!(loaded.files_written.is_empty() && loaded.files_kept.is_empty());
    assert_eq!(loaded.tags, 3);
    assert_eq!(loaded.views, 0);
    assert_eq!(loaded.next, "read the holder through the adapter at a tag");
    // no config, no pack directory, no canon: the store and its lock are all that stand
    assert!(!target.join(".rdy.yaml").exists());
    assert!(!target.join(".rdy/packs").exists());
    assert!(!target.join(".rdy/local.yaml").exists());

    let got = Rdy::open_holder(&target).unwrap();
    assert_eq!(got.store().mode().unwrap(), StoreMode::Holder);
    assert_eq!(
        got.store().reader().repo_id().unwrap(),
        Some(repo_id),
        "the repo id is the source's"
    );
    assert_eq!(got.store().reader().origin().unwrap(), None);
    assert_eq!(tag_pairs(&got), tag_pairs(&src), "the divergent tags");
    assert_eq!(
        object_hashes(&got),
        object_hashes(&src),
        "the original hashes"
    );
    assert_eq!(got.read().canon_head(), None, "a holder has no canon");
    // the loaded log is the source log byte for byte, so a second dump equals the first
    let again = root.path().join("again.rdydump");
    got.read().dump(&again).unwrap();
    let (a, b) = (dump_items(&dump), dump_items(&again));
    assert_eq!(a.0.log_end, b.0.log_end);
    assert_eq!(a.2, b.2);
    // a project opener never takes the holder as a project
    got.close().unwrap();
    assert!(Rdy::open(&target).is_err());
}

#[test]
fn ha12_holder_dump_has_no_project_files() {
    let root = tempfile::tempdir().unwrap();
    let src_dir = divergent_holder(root.path(), RepoId([3; 16]));
    // a project dump would fail or carry these: a pack entry that cannot be read as a file, one
    // whose name a dump refuses, and a stray config
    let packs = src_dir.join(".rdy/packs");
    std::fs::create_dir_all(packs.join("broken.yaml")).unwrap();
    std::fs::write(packs.join("bad name.yaml"), b"x: [").unwrap();
    std::fs::write(src_dir.join(".rdy.yaml"), b"pack: [").unwrap();
    std::fs::write(src_dir.join(".rdyignore"), b"*").unwrap();
    let src = Rdy::open_holder(&src_dir).unwrap();
    let dump = root.path().join("h.rdydump");
    let summary = src.read().dump(&dump).unwrap();
    assert!(summary.files.is_empty());
    let (_, files, batches) = dump_items(&dump);
    assert!(files.is_empty(), "zero FILE records: {files:?}");
    assert!(!batches.is_empty());
    // the same files stop a project dump, which shows the holder branch did not read them
    assert!(project_dump_files(&src_dir).is_err());
    // and the strays are not copied by the load
    let target = root.path().join("loaded");
    load::load(&dump, &target).unwrap();
    let names = |dir: &Path| -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    };
    assert_eq!(names(&target), [".rdy"]);
    assert_eq!(names(&target.join(".rdy")), ["store"]);
}

/// Whether a project dump of `dir` can read its files: it must not, beside these strays.
fn project_dump_files(dir: &Path) -> Result<Vec<(String, Vec<u8>)>, RdyError> {
    rdy_engine::dump::project_files(dir)
}

#[test]
fn ha12_holder_load_uses_holder_fsck() {
    let root = tempfile::tempdir().unwrap();
    let src_dir = divergent_holder(root.path(), RepoId([9; 16]));
    let src = Rdy::open_holder(&src_dir).unwrap();
    let dump = root.path().join("h.rdydump");
    src.read().dump(&dump).unwrap();
    let target = root.path().join("loaded");
    let loaded = load::load(&dump, &target).unwrap();
    // three tags on two histories: the holder check follows each tag's own chain, where a
    // project's check would look for every tag on one canon chain
    assert_eq!(loaded.fsck.versions, 3);
    assert_eq!(loaded.landings, 3, "the shared landing counts once");
    let source_fsck = src.fsck().unwrap();
    assert_eq!(
        serde_json::to_value(&loaded.fsck).unwrap(),
        serde_json::to_value(&source_fsck).unwrap()
    );
    let got = Rdy::open_holder(&target).unwrap();
    assert_eq!(
        serde_json::to_value(got.fsck().unwrap()).unwrap(),
        serde_json::to_value(&source_fsck).unwrap()
    );
}

#[test]
fn ha12_clone_refuses_holder_before_origin_changes() {
    let root = tempfile::tempdir().unwrap();
    let src_dir = divergent_holder(root.path(), RepoId([5; 16]));
    let src = Rdy::open_holder(&src_dir).unwrap();
    let before_end = src.store().log_end();
    let before_origin = src.store().reader().origin().unwrap();
    let dump = root.path().join("h.rdydump");
    src.read().dump(&dump).unwrap();
    let (message, fix) = (
        "a holder dump cannot be cloned as a project",
        "load the dump into a new holder directory",
    );

    for fork in [false, true] {
        // the dump-file route: the load runs, the step refuses, the target is taken back
        let into = root.path().join(format!("file-{fork}"));
        let e = clone(CloneSource::File(&dump), &into, fork).unwrap_err();
        assert_err(&e, E_BAD_INPUT, message, fix);
        assert!(!into.exists(), "the target is as it was: {:?}", tree(&into));
        // the already-open route: refused before the target or a pipe is touched
        let into = root.path().join(format!("state-{fork}"));
        let st = src.read();
        let e = clone(CloneSource::State(&st), &into, fork).unwrap_err();
        assert_err(&e, E_BAD_INPUT, message, fix);
        assert!(!into.exists());
    }
    // a target that stands with a file keeps it
    let into = root.path().join("occupied");
    std::fs::create_dir_all(&into).unwrap();
    std::fs::write(into.join("note.txt"), b"keep").unwrap();
    let e = clone(CloneSource::File(&dump), &into, false).unwrap_err();
    assert_err(&e, E_BAD_INPUT, message, fix);
    assert_eq!(tree(&into), ["note.txt"]);
    // the source holder's refs and log did not move
    assert_eq!(src.store().log_end(), before_end);
    assert_eq!(src.store().reader().origin().unwrap(), before_origin);
}

#[test]
fn ha12_holder_files_are_refused_and_target_is_restored() {
    let root = tempfile::tempdir().unwrap();
    let src_dir = divergent_holder(root.path(), RepoId([4; 16]));
    let src = Rdy::open_holder(&src_dir).unwrap();
    let plain = root.path().join("plain.rdydump");
    src.read().dump(&plain).unwrap();
    // the same batches, now with a project file ahead of them
    let (header, _, batches) = dump_items(&plain);
    let carrying = root.path().join("carrying.rdydump");
    {
        let out = std::fs::File::create(&carrying).unwrap();
        let mut w = DumpWriter::new(std::io::BufWriter::new(out), &header).unwrap();
        w.file(".rdy.yaml", b"pack: rdy-standard\n").unwrap();
        let mut facts = RemovalTally::default();
        for item in &batches {
            let DumpItem::Batch(b) = item else {
                unreachable!()
            };
            facts.observe(b).unwrap();
            w.batch(b).unwrap();
        }
        let (buf, _) = w.finish(facts.facts().unwrap()).unwrap();
        buf.into_inner().unwrap().sync_all().unwrap();
    }
    // a new directory is not left behind
    let target = root.path().join("loaded");
    let e = load::load(&carrying, &target).unwrap_err();
    assert_err(
        &e,
        E_DUMP,
        "a holder dump must not contain project files",
        "write a new dump from the holder",
    );
    assert!(!target.exists(), "{:?}", tree(&target));
    // an existing target keeps what it had, and gets no store and no file
    let occupied = root.path().join("occupied");
    std::fs::create_dir_all(occupied.join("notes")).unwrap();
    std::fs::write(occupied.join("notes/a.txt"), b"mine").unwrap();
    let before = tree(&occupied);
    let e = load::load(&carrying, &occupied).unwrap_err();
    assert_eq!(e.code, E_DUMP);
    assert_eq!(tree(&occupied), before);
    assert!(!occupied.join(".rdy.yaml").exists());
    // the same target then takes the honest dump
    let loaded = load::load(&plain, &occupied).unwrap();
    assert!(loaded.files_written.is_empty());
    assert_eq!(
        Rdy::open_holder(&occupied)
            .unwrap()
            .fsck()
            .unwrap()
            .versions,
        3
    );
}

/// An empty holder's dump with one byte appended to its `meta/store_mode` ref payload. The
/// ingest's decoder accepts the ref (it names the mode `holder`); the strict record decoder does
/// not. Hashes, digest and log end are recomputed, so the dump is whole. `file` adds a project
/// file ahead of the batch.
fn trailing_mode_dump(root: &Path, file: bool) -> std::path::PathBuf {
    let dir = root.join("empty-holder");
    if !dir.exists() {
        Rdy::create_holder(&dir, RepoId([9; 16]))
            .unwrap()
            .close()
            .unwrap();
    }
    let plain = root.join(if file {
        "empty-a.rdydump"
    } else {
        "empty-b.rdydump"
    });
    Rdy::open_holder(&dir).unwrap().read().dump(&plain).unwrap();
    let (mut header, _, items) = dump_items(&plain);
    assert_eq!(items.len(), 1, "an empty holder is one batch");
    let DumpItem::Batch(mut b) = items.into_iter().next().unwrap() else {
        unreachable!()
    };
    let mut changed = 0;
    for f in &mut b.frames {
        if let rdy_store::dump::Frame::Present {
            kind: kinds::REF_UPDATE,
            hash,
            payload,
        } = f
            && rdy_store::bounded::decode::<kinds::RefUpdate>(payload)
                .is_ok_and(|u| u.name == rdy_store::holder::REF_STORE_MODE)
        {
            payload.push(0);
            *hash = Hash::of(payload);
            changed += 1;
        }
    }
    assert_eq!(changed, 1);
    b.digest = kinds::batch_digest(b.frames.iter().map(|f| f.hash()));
    header.log_end = Location {
        segment: header.log_end.segment,
        offset: header.log_end.offset + 1,
    };
    // the one batch's marker names its new digest, and it ends one byte later
    assert_eq!(batch_end(&b), Some(header.log_end));
    header.chain.as_mut().unwrap().end_batch = BatchMarker::of(&b);
    let out = root.join(if file {
        "trailing-file.rdydump"
    } else {
        "trailing.rdydump"
    });
    let f = std::fs::File::create(&out).unwrap();
    let mut w = DumpWriter::new(std::io::BufWriter::new(f), &header).unwrap();
    if file {
        w.file(".rdy.yaml", b"pack: rdy-standard\n").unwrap();
    }
    w.batch(&b).unwrap();
    let (buf, _) = w.finish(RemovalFacts::default()).unwrap();
    buf.into_inner().unwrap().sync_all().unwrap();
    out
}

#[test]
fn ha12_holder_mode_is_the_ingests_even_when_the_ref_has_trailing_bytes() {
    let root = tempfile::tempdir().unwrap();
    // the honest form of the dump loads as a holder, so the probe below is a fair baseline
    let carrying = trailing_mode_dump(root.path(), true);
    let target = root.path().join("loaded");
    let e = load::load(&carrying, &target).unwrap_err();
    assert_err(
        &e,
        E_DUMP,
        "a holder dump must not contain project files",
        "write a new dump from the holder",
    );
    assert!(!target.exists(), "{:?}", tree(&target));
    // an existing target keeps what it had
    let occupied = root.path().join("occupied");
    std::fs::create_dir_all(occupied.join("notes")).unwrap();
    std::fs::write(occupied.join("notes/a.txt"), b"mine").unwrap();
    let before = tree(&occupied);
    let e = load::load(&carrying, &occupied).unwrap_err();
    assert_eq!(e.code, E_DUMP, "{e}");
    assert_eq!(tree(&occupied), before);
    assert!(!occupied.join(".rdy.yaml").exists());
    // the same dump without the file is a holder, as the ingest judged it
    let plain = trailing_mode_dump(root.path(), false);
    let loaded = load::load(&plain, &occupied).unwrap();
    assert!(loaded.files_written.is_empty());
    assert_eq!(loaded.next, "read the holder through the adapter at a tag");
    assert_eq!(
        Rdy::open_holder(&occupied).unwrap().store().mode().unwrap(),
        StoreMode::Holder
    );
}
