//! The store lock judged by `rdy_store::fileid` (FW-XP1 D-1RXEA): a lock is its store's when the
//! held file and the file at `dir/lock` have one identity.

use rdy_store::fileid::FileId;
use rdy_store::{Store, StoreError, StoreLock};

#[test]
fn ps2_open_locked_takes_its_own_lock_after_the_store_dir_is_renamed() {
    let root = tempfile::tempdir().unwrap();
    let before = root.path().join("before");
    let after = root.path().join("after");
    std::fs::create_dir(&before).unwrap();
    Store::create(&before).unwrap().close().unwrap();
    let held = StoreLock::take(&before).unwrap();
    let id = FileId::lstat(&before.join("lock")).unwrap();

    std::fs::rename(&before, &after).unwrap();
    assert_eq!(
        FileId::lstat(&after.join("lock")).unwrap(),
        id,
        "a rename keeps the lock file's identity"
    );
    assert!(
        matches!(Store::open(&after), Err(StoreError::Locked(_))),
        "the renamed store is still locked"
    );
    let s = Store::open_locked(&after, &held).unwrap();
    s.close().unwrap();
    drop(held);
    Store::open(&after).unwrap().close().unwrap();
}

#[test]
fn ps2_open_locked_refuses_the_lock_of_another_store() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    Store::create(a.path()).unwrap().close().unwrap();
    Store::create(b.path()).unwrap().close().unwrap();
    let held = StoreLock::take(a.path()).unwrap();
    assert_ne!(
        FileId::lstat(&a.path().join("lock")).unwrap(),
        FileId::lstat(&b.path().join("lock")).unwrap()
    );
    match Store::open_locked(b.path(), &held) {
        Err(StoreError::Locked(p)) => assert_eq!(p, b.path()),
        Err(e) => panic!("expected Locked, got {e:?}"),
        Ok(_) => panic!("expected Locked, got a store"),
    }

    // A lock file replaced after the lock was taken is another file: the held lock is not its.
    std::fs::remove_file(a.path().join("lock")).unwrap();
    std::fs::write(a.path().join("lock"), b"").unwrap();
    assert!(matches!(
        Store::open_locked(a.path(), &held),
        Err(StoreError::Locked(_))
    ));
}
