//! m4bc3: a held batch that cannot be sealed reports what became of it. A confirmed rollback
//! cuts the log back to the batch's start, so the store is as before after reopen and reindex;
//! a rollback that cannot be proved is reported as such, the handle stays poisoned, and a
//! complete marked batch survives reopen and reindex. An abandoned batch reports its rollback
//! the same way.
use rdy_model::Hash;
use rdy_store::kinds;
use rdy_store::segment::Location;
use rdy_store::{Rollback, SealFailure, Store};

fn fresh() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::create(&d.path().join("s")).unwrap();
    (d, s)
}

fn reopen(dir: &tempfile::TempDir) -> Store {
    Store::open(&dir.path().join("s")).unwrap()
}

/// What a reopened, reindexed store holds durably: the log's end, every ref and the index's
/// derived rows.
#[derive(Debug, PartialEq)]
struct Durable {
    end: Location,
    refs: Vec<(String, Vec<u8>)>,
    frames: usize,
}

fn durable(s: &mut Store) -> Durable {
    s.reindex().unwrap();
    Durable {
        end: s.log_end(),
        refs: s.reader().refs_with_prefix("").unwrap(),
        frames: s.verify().unwrap(),
    }
}

/// A store with one sealed batch before the held batch under test, then the held batch: a blob
/// and a ref, appended and indexed but not sealed. Returns the store's durable state before the
/// held batch and the blob's hash.
fn with_held_batch(dir: &tempfile::TempDir, mut s: Store) -> (Store, Durable, Hash) {
    s.set_refs(vec![("t/before".into(), vec![1])]).unwrap();
    drop(s);
    let mut s = reopen(dir);
    let before = durable(&mut s);
    s.set_held(true).unwrap();
    let blob = s.put_blob(b"staged blob bytes").unwrap();
    s.set_refs(vec![("t/import".into(), vec![2])]).unwrap();
    assert_eq!(
        s.open_batch().map(<[_]>::len),
        Some(3),
        "chunk, manifest, ref"
    );
    (s, before, blob)
}

/// The seal of a held batch fails with `fault` armed and the rollback `rollback_fails`: returns
/// the failure.
fn seal_failing(s: &mut Store, fault: &str, rollback_fails: bool) -> SealFailure {
    match fault {
        "index" => s.fail_next_index_apply(),
        "sync" => s.fail_next_sync(),
        other => panic!("no fault {other}"),
    }
    if rollback_fails {
        s.fail_next_rollback();
    }
    s.seal_reporting(vec![]).unwrap_err()
}

#[test]
fn m4bc3_seal_index_or_sync_failure_with_confirmed_rollback_is_unchanged() {
    for fault in ["index", "sync"] {
        let (dir, s) = fresh();
        let (mut s, before, blob) = with_held_batch(&dir, s);
        let f = seal_failing(&mut s, fault, false);
        assert_eq!(
            f.rollback,
            Some(Rollback::Confirmed),
            "{fault}: {:?}",
            f.error
        );
        assert!(
            s.open_batch().is_none(),
            "{fault}: no batch is left to seal"
        );
        assert!(s.is_poisoned(), "{fault}: the index holds abandoned frames");
        assert!(
            s.put_blob(b"later").is_err(),
            "{fault}: writes wait for reopen"
        );
        drop(s);
        let mut s = reopen(&dir);
        assert_eq!(durable(&mut s), before, "{fault}: the store is as before");
        assert!(!s.has_blob(&blob).unwrap(), "{fault}: the blob is gone");
        assert_eq!(s.get_ref("t/import").unwrap(), None, "{fault}");
        assert!(
            s.open_report().quarantined.is_empty(),
            "{fault}: nothing torn"
        );
    }
}

#[test]
fn m4bc3_seal_index_failure_and_failed_rollback_is_indeterminate() {
    let (dir, s) = fresh();
    let (mut s, before, blob) = with_held_batch(&dir, s);
    let f = seal_failing(&mut s, "index", true);
    assert_eq!(f.rollback, Some(Rollback::Unproven), "{:?}", f.error);
    assert!(
        f.error.to_string().contains("injected index failure"),
        "{}",
        f.error
    );
    assert!(s.is_poisoned());
    assert!(
        s.put_blob(b"later").is_err(),
        "the handle refuses more writes"
    );
    drop(s);
    // the batch was marked and synced: it is the log's, through reopen and reindex
    let mut s = reopen(&dir);
    let after = durable(&mut s);
    assert!(after.end > before.end, "{after:?}");
    assert!(s.has_blob(&blob).unwrap(), "the marked batch survives");
    assert_eq!(s.get_ref("t/import").unwrap(), Some(vec![2]));
    s.reindex().unwrap();
    assert_eq!(
        s.get_ref("t/import").unwrap(),
        Some(vec![2]),
        "and a reindex"
    );
}

#[test]
fn m4bc3_seal_sync_failure_and_failed_rollback_is_indeterminate() {
    let (dir, s) = fresh();
    let (mut s, before, blob) = with_held_batch(&dir, s);
    let f = seal_failing(&mut s, "sync", true);
    assert_eq!(f.rollback, Some(Rollback::Unproven), "{:?}", f.error);
    assert!(s.is_poisoned());
    assert!(
        s.put_blob(b"later").is_err(),
        "the handle refuses more writes"
    );
    drop(s);
    // a failed sync does not prove the bytes were lost: the marker was written whole, so the
    // complete batch is the log's after reopen and reindex
    let mut s = reopen(&dir);
    let after = durable(&mut s);
    assert!(after.end > before.end, "{after:?}");
    assert!(s.has_blob(&blob).unwrap(), "the marked batch survives");
    assert_eq!(s.get_ref("t/import").unwrap(), Some(vec![2]));
}

#[test]
fn m4bc3_abandon_with_unproven_rollback_is_indeterminate() {
    // nothing open: nothing to cut, and the store keeps working
    let (_d, mut s) = fresh();
    s.set_held(true).unwrap();
    assert_eq!(s.abandon_open_batch(), Rollback::NothingOpen);
    assert!(!s.is_poisoned());

    // a seal refused before anything is appended leaves the batch open, as it was; an abandon
    // then cuts it back, confirmed
    let (dir, s) = fresh();
    let (mut s, before, blob) = with_held_batch(&dir, s);
    let open = s.open_batch().unwrap().to_vec();
    let f = s
        .seal_reporting(vec![(kinds::REDACTION, b"not the store's writer".to_vec())])
        .unwrap_err();
    assert_eq!(f.rollback, None, "{:?}", f.error);
    assert_eq!(
        s.open_batch(),
        Some(&open[..]),
        "the batch is open, as it was"
    );
    assert_eq!(s.abandon_open_batch(), Rollback::Confirmed);
    assert!(s.open_batch().is_none());
    assert!(
        s.is_poisoned(),
        "the index holds abandoned frames until reopen"
    );
    drop(s);
    let mut s = reopen(&dir);
    assert_eq!(durable(&mut s), before);
    assert!(!s.has_blob(&blob).unwrap());

    // the cut fails: the outcome is unproven, the handle refuses writes until reopen
    let (dir, s) = fresh();
    let (mut s, before, _) = with_held_batch(&dir, s);
    s.fail_next_rollback();
    assert_eq!(s.abandon_open_batch(), Rollback::Unproven);
    assert!(s.open_batch().is_none(), "nothing later may seal it");
    assert!(s.is_poisoned());
    assert!(s.set_refs(vec![("t/later".into(), vec![3])]).is_err());
    drop(s);
    // the batch has no marker: the reopen never takes it as acknowledged
    let mut s = reopen(&dir);
    let after = durable(&mut s);
    assert_eq!(after.refs, before.refs, "{after:?}");
    assert_eq!(after.frames, before.frames, "{after:?}");
}
