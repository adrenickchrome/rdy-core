//! Store format (plan M2.0 fmt1, plan M2a1 fmt2): one number, in the log and mirrored in FORMAT;
//! a store of any other format is refused before anything is written; the store is .rdy/store.
use rdy_engine::{Rdy, RdyError};
use rdy_qa::fixtures::*;
use rdy_store::format::{REF_STORE_FORMAT, STORE_FORMAT};
use rdy_store::segment::SegmentLog;
use rdy_store::{Store, StoreError, kinds};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const CURRENT_TEXT: &str = "rdy store format\nstore_format 3\n";

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
fn fmt1_a_new_store_records_its_format_in_the_log_and_in_format() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let s = Store::open(&dir).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("FORMAT")).unwrap(),
        CURRENT_TEXT
    );
    assert_eq!(s.get_ref(REF_STORE_FORMAT).unwrap(), Some(b"3".to_vec()));
    assert_eq!(s.verify().unwrap(), 1, "one format batch of one ref");
    drop(s);
    let s = Store::open(&dir).unwrap();
    assert_eq!(s.verify().unwrap(), 1, "no second format batch");
    assert!(s.open_report().quarantined.is_empty());
}

#[test]
fn fmt1_format_refs_survive_reindex() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::open(&dir).unwrap();
    s.put_blob(b"x").unwrap();
    s.reindex().unwrap();
    assert_eq!(s.get_ref(REF_STORE_FORMAT).unwrap(), Some(b"3".to_vec()));
    assert_eq!(
        std::fs::read_to_string(dir.join("FORMAT")).unwrap(),
        CURRENT_TEXT
    );
}

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
    assert_eq!(
        std::fs::read_to_string(dir.join("FORMAT")).unwrap(),
        CURRENT_TEXT
    );
    assert_eq!(s.verify().unwrap(), frames, "no second format batch");
    assert_eq!(s.get_blob(&Store::blob_hash(b"one")).unwrap(), b"one");
}

#[test]
fn fmt1_an_unreadable_format_file_names_its_fix() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    drop(Store::open(&dir).unwrap());
    std::fs::write(dir.join("FORMAT"), "not a format file\n").unwrap();
    let e = RdyError::from(Store::open(&dir).err().expect("unreadable"));
    assert_eq!(e.code, "E_STORE_FORMAT", "{e}");
    assert!(e.message.contains("is unreadable"), "{e}");
    assert!(e.fix.contains("delete it and reopen"), "{e}");
}

/// Review Focus 3: the one refusal, whatever told us the store is another build's.
fn refused(dir: &Path, what: &str) -> RdyError {
    let e = Store::open(dir)
        .err()
        .expect("a store of another format is refused");
    assert!(matches!(&e, StoreError::Format { .. }), "{e}");
    let e = RdyError::from(e);
    assert_eq!(e.code, "E_STORE_FORMAT", "{e}");
    assert_eq!(
        e.message,
        format!(
            "this store was written by a different rdy build: {} {what}, and this rdy uses store format {STORE_FORMAT}",
            dir.display()
        )
    );
    assert_eq!(e.fix, "re-create it with this rdy");
    e
}

#[test]
fn fmt2_a_store_of_a_newer_format_is_refused_before_anything_is_opened() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    Store::open(&dir).unwrap().put_blob(b"x").unwrap();
    std::fs::write(dir.join("FORMAT"), "rdy store format\nstore_format 4\n").unwrap();
    let before = tree(&dir);
    refused(&dir, "has store format 4");
    assert_eq!(tree(&dir), before, "nothing under the store changed");
}

#[test]
fn fmt2_an_m2_0_store_is_refused() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    Store::open(&dir).unwrap().put_blob(b"x").unwrap();
    std::fs::write(
        dir.join("FORMAT"),
        "rdy store format\nlog_format 2\nmin_reader 2\n",
    )
    .unwrap();
    let before = tree(&dir);
    refused(&dir, "has a FORMAT file with no store_format line");
    assert_eq!(tree(&dir), before);
}

#[test]
fn fmt2_a_log_that_says_another_format_is_refused_without_format() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    Store::open(&dir)
        .unwrap()
        .set_refs(vec![(REF_STORE_FORMAT.into(), b"2".to_vec())])
        .unwrap();
    std::fs::remove_file(dir.join("FORMAT")).unwrap();
    let seg = std::fs::read(dir.join("segments/seg-000000.log")).unwrap();
    refused(&dir, "has store format 2");
    assert_eq!(
        std::fs::read(dir.join("segments/seg-000000.log")).unwrap(),
        seg
    );
    assert!(!dir.join("FORMAT").exists());
}

/// A marked log with no format ref: frames, no format ref, no FORMAT. The index is built to find
/// that out; the log is untouched.
#[test]
fn fmt2_a_log_with_frames_and_no_store_format_is_refused() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    std::fs::create_dir_all(dir.join("segments")).unwrap();
    {
        let mut log = SegmentLog::open(&dir.join("segments"), u64::MAX).unwrap();
        log.append_batch(&[(
            kinds::BLOB_CHUNK,
            b"written before the format existed".to_vec(),
        )])
        .unwrap();
        log.sync().unwrap();
    }
    let seg = std::fs::read(dir.join("segments/seg-000000.log")).unwrap();
    refused(&dir, "has a log with no store format");
    assert_eq!(
        std::fs::read(dir.join("segments/seg-000000.log")).unwrap(),
        seg
    );
    assert!(!dir.join("FORMAT").exists());
}

#[test]
fn fmt2_a_project_whose_store_is_not_a_directory_is_refused() {
    let (dir, r) = project();
    drop(r);
    let rdy = dir.path().join(".rdy");
    std::fs::remove_dir_all(rdy.join("store")).unwrap();
    // what an M2.0 project holds at .rdy/store
    std::fs::write(
        rdy.join("store"),
        "this store was upgraded by a newer rdy (M2 or later); upgrade rdy\n",
    )
    .unwrap();
    let before = tree(&rdy);
    let e = Rdy::open(dir.path())
        .err()
        .expect("the stub is another build's store");
    assert_eq!(e.code, "E_STORE_FORMAT", "{e}");
    assert!(
        e.message.contains(&format!(
            "{} is not a directory",
            rdy.join("store").display()
        )),
        "{e}"
    );
    assert_eq!(e.fix, "re-create it with this rdy");
    assert_eq!(tree(&rdy), before, "nothing under .rdy changed");
}

#[test]
fn fmt1_a_new_project_keeps_its_store_in_dot_rdy_store() {
    let (dir, r) = project();
    let rdy = dir.path().join(".rdy");
    for f in ["lock", "FORMAT", "index.redb", "segments"] {
        assert!(rdy.join("store").join(f).exists(), "{f}");
    }
    assert!(!rdy.join("store2").exists());
    assert_eq!(r.store().dir(), rdy.join("store").as_path());
    let e = Rdy::open(dir.path()).err().expect("the store is locked");
    assert_eq!(e.code, "E_LOCKED", "{e}");
}

/// A missing store directory under an intact .rdy is created, durably (dur1).
#[test]
fn fmt1_a_missing_store_directory_is_created_durably() {
    let (dir, r) = project();
    drop(r);
    let rdy = dir.path().join(".rdy");
    std::fs::remove_dir_all(rdy.join("store")).unwrap();
    let (r, synced) = rdy_store::fsutil::record_syncs(|| Rdy::open(dir.path()));
    r.unwrap();
    assert!(synced.contains(&rdy), "{synced:?}");
    assert!(rdy.join("store/FORMAT").exists());
}
