//! Store format records and the store2 layout (plan M2.0): format refs and FORMAT (fmt1), the
//! .rdy/store2 move and its stub (fmt1), and the one-time upgrade of M1 stores (upg1).
#![allow(unused_imports)]
use rdy_engine::{Rdy, RdyError, Scope};
use rdy_qa::fixtures::*;
use rdy_store::format::{Format, REF_LOG_FORMAT, REF_MIN_READER};
use rdy_store::{Store, StoreError};
use serde_json::json;
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
