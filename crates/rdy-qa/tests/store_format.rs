//! Store format records and the store2 layout (plan M2.0): format refs and FORMAT (fmt1), the
//! .rdy/store2 move and its stub (fmt1), and the one-time upgrade of M1 stores (upg1).
#![allow(unused_imports)]
use rdy_engine::{Rdy, RdyError, Scope};
use rdy_qa::fixtures::*;
use rdy_qa::invariants;
use rdy_store::format::{Format, REF_LOG_FORMAT, REF_MIN_READER};
use rdy_store::layout::STUB_TEXT;
use rdy_store::{Store, StoreError};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const CURRENT_TEXT: &str = "rdy store format\nlog_format 2\nmin_reader 2\n";

#[test]
fn fmt1_a_new_store_records_its_format_in_the_log_and_in_format() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let s = Store::open(&dir).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("FORMAT")).unwrap(),
        CURRENT_TEXT
    );
    assert_eq!(s.get_ref(REF_LOG_FORMAT).unwrap(), Some(b"2".to_vec()));
    assert_eq!(s.get_ref(REF_MIN_READER).unwrap(), Some(b"2".to_vec()));
    assert!(!s.open_report().upgraded, "a new store is not an upgrade");
    drop(s);
    assert!(Store::open(&dir).unwrap().open_report().marker_rule);
}

#[test]
fn fmt1_format_refs_survive_reindex() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::open(&dir).unwrap();
    s.put_blob(b"x").unwrap();
    s.reindex().unwrap();
    assert_eq!(s.get_ref(REF_MIN_READER).unwrap(), Some(b"2".to_vec()));
    assert_eq!(
        std::fs::read_to_string(dir.join("FORMAT")).unwrap(),
        CURRENT_TEXT
    );
}

#[test]
fn fmt1_a_store_from_a_newer_rdy_is_refused_before_anything_is_written() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    Store::open(&dir).unwrap().put_blob(b"x").unwrap();
    std::fs::write(
        dir.join("FORMAT"),
        "rdy store format\nlog_format 3\nmin_reader 3\n",
    )
    .unwrap();
    let seg = std::fs::read(dir.join("segments/seg-000000.log")).unwrap();
    let idx = std::fs::read(dir.join("index.redb")).unwrap();
    let e = Store::open(&dir).err().expect("a newer store is refused");
    assert!(matches!(&e, StoreError::Format { .. }), "{e}");
    let e = RdyError::from(e);
    assert_eq!(e.code, "E_STORE_FORMAT", "{e}");
    assert!(e.message.contains("store written by a newer rdy"), "{e}");
    assert!(e.fix.contains("upgrade rdy"), "{e}");
    assert_eq!(
        std::fs::read(dir.join("segments/seg-000000.log")).unwrap(),
        seg
    );
    assert_eq!(std::fs::read(dir.join("index.redb")).unwrap(), idx);
}

#[test]
fn fmt1_a_newer_min_reader_in_the_log_is_refused_without_format() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    Store::open(&dir)
        .unwrap()
        .set_refs(vec![(REF_MIN_READER.into(), b"3".to_vec())])
        .unwrap();
    std::fs::remove_file(dir.join("FORMAT")).unwrap();
    let e = Store::open(&dir)
        .err()
        .expect("the log says a newer rdy wrote it");
    assert!(matches!(&e, StoreError::Format { .. }), "{e}");
}

/// Review Focus 6.
#[test]
fn fmt1_a_lost_format_file_is_rewritten_from_the_log() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let frames = {
        let mut s = Store::open(&dir).unwrap();
        s.put_blob(b"one").unwrap();
        s.put_blob(b"two").unwrap();
        s.verify().unwrap()
    };
    std::fs::remove_file(dir.join("FORMAT")).unwrap();
    let s = Store::open(&dir).unwrap();
    let r = s.open_report().clone();
    assert!(r.marker_rule && !r.upgraded, "{r:?}");
    assert_eq!(
        std::fs::read_to_string(dir.join("FORMAT")).unwrap(),
        CURRENT_TEXT
    );
    assert_eq!(s.verify().unwrap(), frames, "no second format batch");
}

#[test]
fn upg1_an_m1_log_is_upgraded_once_by_a_marked_batch() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let blobs: Vec<_> = {
        let mut s = Store::open_legacy_for_tests(&dir).unwrap();
        (0..3u8).map(|i| s.put_blob(&[i; 9]).unwrap()).collect()
    };
    assert!(!dir.join("FORMAT").exists());
    assert!(!rdy_store::segment::log_has_marker(&dir.join("segments")).unwrap());
    let s = Store::open(&dir).unwrap();
    let r = s.open_report().clone();
    assert!(r.upgraded && !r.marker_rule, "{r:?}");
    for (i, h) in blobs.iter().enumerate() {
        assert_eq!(s.get_blob(h).unwrap(), vec![i as u8; 9]);
    }
    assert_eq!(s.get_ref(REF_LOG_FORMAT).unwrap(), Some(b"2".to_vec()));
    assert_eq!(
        std::fs::read_to_string(dir.join("FORMAT")).unwrap(),
        CURRENT_TEXT
    );
    assert_eq!(s.verify().unwrap(), 3 * 2 + 2);
    drop(s);
    let s = Store::open(&dir).unwrap();
    let r = s.open_report().clone();
    assert!(r.marker_rule && !r.upgraded, "{r:?}");
    assert_eq!(s.verify().unwrap(), 3 * 2 + 2);
}

#[test]
fn upg1_an_m1_torn_tail_is_repaired_by_the_m1_rule_before_the_upgrade() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let h = Store::open_legacy_for_tests(&dir)
        .unwrap()
        .put_blob(b"kept")
        .unwrap();
    let seg = dir.join("segments/seg-000000.log");
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes.extend([0u8; 50]);
    std::fs::write(&seg, &bytes).unwrap();
    let s = Store::open(&dir).unwrap();
    assert!(s.open_report().upgraded);
    assert_eq!(s.get_blob(&h).unwrap(), b"kept");
    s.verify().unwrap();
    assert!(
        std::fs::read_dir(dir.join("segments")).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".torn-"))
    );
}

/// An M1 log that holds both format refs as plain frames and no batch marker: what a kill (or a
/// power loss) during the upgrade's format batch leaves when the ref frames land and the marker
/// does not. Returns the blobs, and the index as it was before the refs were written.
fn m1_log_with_format_refs_and_no_marker(dir: &Path) -> (Vec<rdy_model::Hash>, Vec<u8>) {
    let blobs: Vec<_> = {
        let mut s = Store::open_legacy_for_tests(dir).unwrap();
        (0..3u8).map(|i| s.put_blob(&[i; 9]).unwrap()).collect()
    };
    let pre_index = std::fs::read(dir.join("index.redb")).unwrap();
    {
        let mut s = Store::open_legacy_for_tests(dir).unwrap();
        s.set_refs(vec![
            (REF_LOG_FORMAT.into(), b"2".to_vec()),
            (REF_MIN_READER.into(), b"2".to_vec()),
        ])
        .unwrap();
    }
    assert!(!dir.join("FORMAT").exists());
    assert!(!rdy_store::segment::log_has_marker(&dir.join("segments")).unwrap());
    (blobs, pre_index)
}

fn assert_finished_upgrade(dir: &Path, blobs: &[rdy_model::Hash]) {
    // an open that does not write must leave a store the next open accepts
    let s = Store::open(dir).unwrap();
    let r = s.open_report().clone();
    assert!(!r.marker_rule && r.upgraded, "{r:?}");
    assert!(
        rdy_store::segment::log_has_marker(&dir.join("segments")).unwrap(),
        "the open must leave a marker that acknowledges the log"
    );
    drop(s);
    let s = Store::open(dir).unwrap();
    let r = s.open_report().clone();
    assert!(r.marker_rule && !r.upgraded, "{r:?}");
    for (i, h) in blobs.iter().enumerate() {
        assert_eq!(s.get_blob(h).unwrap(), vec![i as u8; 9]);
    }
    assert_eq!(s.get_ref(REF_LOG_FORMAT).unwrap(), Some(b"2".to_vec()));
    assert_eq!(
        std::fs::read_to_string(dir.join("FORMAT")).unwrap(),
        CURRENT_TEXT
    );
    s.verify().unwrap();
}

/// Round 1, finding 1: both refs are in an M1-rule log with no marker; the open must not write
/// FORMAT alone, or the next open uses the marker rule on a log with no marker.
#[test]
fn upg1_format_refs_without_a_marker_get_a_marker_before_format_is_written() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let (blobs, _) = m1_log_with_format_refs_and_no_marker(&dir);
    assert_finished_upgrade(&dir, &blobs);
}

/// Round 1, finding 2: a kill before the upgrade's marker leaves the ref frames and no marker,
/// with the index from before the upgrade; the next open finishes the upgrade.
#[test]
fn upg1_a_kill_before_the_upgrade_marker_is_finished_by_the_next_open() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let (blobs, pre_index) = m1_log_with_format_refs_and_no_marker(&dir);
    std::fs::write(dir.join("index.redb"), pre_index).unwrap();
    assert_finished_upgrade(&dir, &blobs);
}

/// The same, with the index gone: it is rebuilt from the log, and nothing acknowledged is lost.
#[test]
fn upg1_a_kill_before_the_upgrade_marker_with_the_index_gone_loses_nothing() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let (blobs, _) = m1_log_with_format_refs_and_no_marker(&dir);
    std::fs::remove_file(dir.join("index.redb")).unwrap();
    assert_finished_upgrade(&dir, &blobs);
}

/// Every path under `dir` with its contents (None for directories).
fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.insert(p.clone(), None);
            out.extend(tree(&p));
        } else {
            out.insert(p.clone(), Some(std::fs::read(&p).unwrap()));
        }
    }
    out
}

#[test]
fn fmt1_a_new_project_keeps_its_store_in_store2_behind_a_stub() {
    let (dir, r) = project();
    let rdy = dir.path().join(".rdy");
    assert_eq!(
        STUB_TEXT,
        "this store was upgraded by a newer rdy (M2 or later); upgrade rdy\n"
    );
    assert!(rdy.join("store").is_file());
    assert_eq!(
        std::fs::read_to_string(rdy.join("store")).unwrap(),
        STUB_TEXT
    );
    for f in ["lock", "FORMAT", "index.redb", "segments"] {
        assert!(rdy.join("store2").join(f).exists(), "{f}");
    }
    assert_eq!(r.store().dir(), rdy.join("store2").as_path());
    let e = Rdy::open(dir.path())
        .err()
        .expect("the lock moved with the store");
    assert_eq!(e.code, "E_LOCKED", "{e}");
}

/// Review Focus 4.
#[test]
fn fmt1_an_m1_open_fails_on_the_stub_before_it_locks_or_writes() {
    let (dir, r) = project();
    drop(r);
    let rdy = dir.path().join(".rdy");
    let before = tree(&rdy);
    // M1's Store::open began with exactly these two calls, before its lock or any write
    let store = rdy.join("store");
    rdy_store::fsutil::create_dir_durable(&store).ok();
    rdy_store::fsutil::create_dir_durable(&store.join("segments"))
        .expect_err("no directory can be made inside the stub file");
    assert_eq!(tree(&rdy), before, "nothing under .rdy changed");
}

#[test]
fn fmt1_both_store_directories_are_refused_naming_the_newer() {
    let (dir, r) = project();
    drop(r);
    let rdy = dir.path().join(".rdy");
    // an old rdy ran after the upgrade: no stub, so it made .rdy/store again and wrote to it
    std::fs::remove_file(rdy.join("store")).unwrap();
    Store::open_legacy_for_tests(&rdy.join("store"))
        .unwrap()
        .put_blob(b"written by an old rdy")
        .unwrap();
    let now = std::time::SystemTime::now();
    let set = |p: PathBuf, t| {
        std::fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(t)
            .unwrap()
    };
    set(
        rdy.join("store2/segments/seg-000000.log"),
        now - std::time::Duration::from_secs(60),
    );
    set(rdy.join("store/segments/seg-000000.log"), now);
    let e = Rdy::open(dir.path()).err().expect("both are stores");
    assert_eq!(e.code, "E_STORE_FORMAT", "{e}");
    let old = rdy.join("store").display().to_string();
    let new = rdy.join("store2").display().to_string();
    assert!(e.message.contains(&new), "{e}");
    assert!(
        e.message.contains(&format!("{old} holds the newer log")),
        "{e}"
    );
    assert!(
        rdy.join("store").is_dir() && rdy.join("store2").is_dir(),
        "nothing moved"
    );
}

/// Final review (Important 5): an old rdy that ran after the stub was lost recreates `.rdy/store`
/// with an empty log. However new its segment's mtime, an empty store never holds the newer log,
/// and the message says it is empty.
#[test]
fn fmt1_an_empty_old_store_is_never_named_the_newer_log() {
    let (dir, r) = project();
    drop(r);
    let rdy = dir.path().join(".rdy");
    std::fs::remove_file(rdy.join("store")).unwrap();
    drop(Store::open_legacy_for_tests(&rdy.join("store")).unwrap());
    let now = std::time::SystemTime::now();
    let set = |p: PathBuf, t| {
        std::fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(t)
            .unwrap()
    };
    set(
        rdy.join("store2/segments/seg-000000.log"),
        now - std::time::Duration::from_secs(60),
    );
    set(rdy.join("store/segments/seg-000000.log"), now);
    let e = Rdy::open(dir.path()).err().expect("both are stores");
    assert_eq!(e.code, "E_STORE_FORMAT", "{e}");
    let old = rdy.join("store").display().to_string();
    let new = rdy.join("store2").display().to_string();
    assert!(
        e.message.contains(&format!("{new} holds the newer log")),
        "{e}"
    );
    assert!(e.message.contains(&format!("{old} is empty")), "{e}");
}

/// Final review (Minor 6): an M1 user who kept the store elsewhere behind a `.rdy/store` link is
/// told to replace the link, not just to move it aside.
#[test]
fn fmt1_a_symlinked_old_store_is_told_to_replace_the_link() {
    let (dir, r) = project();
    drop(r);
    let rdy = dir.path().join(".rdy");
    std::fs::remove_file(rdy.join("store")).unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), rdy.join("store")).unwrap();
    let e = Rdy::open(dir.path()).err().expect("a link is not the stub");
    assert_eq!(e.code, "E_STORE_FORMAT", "{e}");
    assert!(e.fix.contains("replace the link"), "{e}");
    assert!(
        std::fs::symlink_metadata(rdy.join("store"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "nothing moved"
    );
}

#[test]
fn fmt1_a_stub_without_store2_is_refused() {
    let (dir, r) = project();
    drop(r);
    let rdy = dir.path().join(".rdy");
    std::fs::rename(rdy.join("store2"), rdy.join("store2.elsewhere")).unwrap();
    let e = Rdy::open(dir.path()).err().expect("the store is missing");
    assert_eq!(e.code, "E_STORE_FORMAT", "{e}");
    assert!(e.message.contains("is missing"), "{e}");
}

#[test]
fn fmt1_a_missing_stub_is_rewritten() {
    let (dir, r) = project();
    drop(r);
    let rdy = dir.path().join(".rdy");
    std::fs::remove_file(rdy.join("store")).unwrap();
    Rdy::open(dir.path()).unwrap();
    assert_eq!(
        std::fs::read_to_string(rdy.join("store")).unwrap(),
        STUB_TEXT
    );
}

#[test]
fn fmt1_the_stub_is_written_durably() {
    let dir = tempfile::tempdir().unwrap();
    let (r, synced) = rdy_store::fsutil::record_syncs(|| Rdy::init(dir.path(), "rdy-standard"));
    r.unwrap();
    let rdy = dir.path().join(".rdy");
    let stub = synced
        .iter()
        .position(|p| *p == rdy.join("store"))
        .expect("stub synced");
    assert!(synced[stub + 1..].contains(&rdy), "{synced:?}");
}

#[test]
fn upg1_an_m1_project_upgrades_on_its_first_open() {
    let (dir, mut r) = m1_project();
    write(&mut r, "work", json!([{"op": "create", "type": "doc", "fields": {"title": "Kept", "doc_type": "glossary"}}])).unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v0.1.0", None, None).unwrap();
    write(&mut r, "work", json!([{"op": "create", "type": "doc", "fields": {"title": "Pending", "doc_type": "glossary"}}])).unwrap();
    let canon = r.store().canon_head().unwrap();
    let landings = r.store().landings().unwrap().len();
    let work = r.store().view_state("work").unwrap();
    drop(r);
    let rdy = dir.path().join(".rdy");
    assert!(
        rdy.join("store").is_dir() && !rdy.join("store2").exists(),
        "an M1 layout"
    );
    let mut r = Rdy::open(dir.path()).unwrap();
    let rep = r.store().open_report().clone();
    assert!(rep.upgraded && !rep.marker_rule, "{rep:?}");
    assert_eq!(
        std::fs::read_to_string(rdy.join("store")).unwrap(),
        STUB_TEXT
    );
    assert_eq!(
        Format::parse(&std::fs::read_to_string(rdy.join("store2/FORMAT")).unwrap()).unwrap(),
        Format::CURRENT
    );
    assert_eq!(r.store().canon_head().unwrap(), canon);
    assert_eq!(r.store().landings().unwrap().len(), landings);
    assert_eq!(r.store().view_state("work").unwrap(), work);
    assert!(matches!(
        r.resolve_scope("tag:v0.1.0").unwrap(),
        Scope::At(_)
    ));
    r.fsck().unwrap();
    invariants::check(&mut r, &Scope::Canon).unwrap();
    drop(r);
    let r = Rdy::open(dir.path()).unwrap();
    let rep = r.store().open_report();
    assert!(rep.marker_rule && !rep.upgraded, "{rep:?}");
}

#[test]
fn upg1_a_crash_between_the_rename_and_the_stub_finishes_on_the_next_open() {
    let (dir, mut r) = m1_project();
    write(&mut r, "work", json!([{"op": "create", "type": "doc", "fields": {"title": "Moved", "doc_type": "glossary"}}])).unwrap();
    drop(r);
    let rdy = dir.path().join(".rdy");
    std::fs::rename(rdy.join("store"), rdy.join("store2")).unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    assert!(r.store().open_report().upgraded);
    assert_eq!(
        std::fs::read_to_string(rdy.join("store")).unwrap(),
        STUB_TEXT
    );
}

/// Review Focus 4.
#[test]
fn upg1_an_m1_process_holding_the_store_blocks_the_upgrade() {
    let (dir, r) = m1_project();
    drop(r);
    let rdy = dir.path().join(".rdy");
    let held = std::fs::File::options()
        .write(true)
        .open(rdy.join("store/lock"))
        .unwrap();
    held.try_lock().unwrap();
    let e = Rdy::open(dir.path())
        .err()
        .expect("an M1 process holds the store");
    assert_eq!(e.code, "E_LOCKED", "{e}");
    assert!(
        rdy.join("store").is_dir() && !rdy.join("store2").exists(),
        "nothing moved"
    );
    drop(held);
    Rdy::open(dir.path()).unwrap();
}

#[test]
fn upg1_the_move_is_durable() {
    let (dir, r) = m1_project();
    drop(r);
    let rdy = dir.path().join(".rdy");
    let (r, synced) = rdy_store::fsutil::record_syncs(|| Rdy::open(dir.path()));
    r.unwrap();
    let stub = synced
        .iter()
        .position(|p| *p == rdy.join("store"))
        .expect("stub synced");
    assert!(
        synced[..stub].contains(&rdy),
        "the rename was synced before the stub: {synced:?}"
    );
    assert!(
        synced[stub + 1..].contains(&rdy),
        "the stub's entry was synced: {synced:?}"
    );
}
