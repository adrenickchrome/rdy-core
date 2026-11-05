//! The store lock judged by `rdy_store::fileid` (FW-XP1 D-1RXEA): a lock is its store's when the
//! held file and the file at `dir/lock` have one identity.

use rdy_store::fileid::FileId;
use rdy_store::{Store, StoreError, StoreLock};

// Unix only: a store directory moves while its lock is held. Windows refuses to move a directory
// while a file in it is open; its load closes the loading store under start.lock instead.
#[cfg(unix)]
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

/// The identity of the object a name or a handle reaches: a rename keeps it, a copy and a
/// replacement are other files, a hard link is the same file with two links.
#[test]
fn wx2_rename_keeps_identity_and_copy_changes_it() {
    use rdy_store::fileid::{self, same_file};
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("a");
    let b = d.path().join("b");
    std::fs::write(&a, b"one").unwrap();
    let id = FileId::lstat(&a).unwrap();
    let handle = std::fs::File::open(&a).unwrap();
    assert_eq!(FileId::of_open(&handle, &a).unwrap(), id);
    assert_eq!(FileId::of_file(&handle).unwrap(), id);
    assert_eq!(fileid::links_of(&handle, &a).unwrap(), 1);
    assert_eq!(fileid::change_stamp_of(&handle, &a).unwrap().id, id);

    std::fs::rename(&a, &b).unwrap();
    assert_eq!(
        FileId::lstat(&b).unwrap(),
        id,
        "a rename keeps the identity"
    );
    assert_eq!(FileId::of_open(&handle, &b).unwrap(), id);

    let copy = d.path().join("copy");
    std::fs::copy(&b, &copy).unwrap();
    assert_ne!(FileId::lstat(&copy).unwrap(), id, "a copy is another file");
    assert!(!same_file(&b, &copy));

    let hard = d.path().join("hard");
    std::fs::hard_link(&b, &hard).unwrap();
    assert_eq!(
        FileId::lstat(&hard).unwrap(),
        id,
        "a hard link is the same file"
    );
    assert!(same_file(&b, &hard));
    assert_eq!(fileid::links_of(&handle, &b).unwrap(), 2);
    std::fs::remove_file(&hard).unwrap();
    assert_eq!(fileid::links_of(&handle, &b).unwrap(), 1);

    // a replacement renamed over the name is another file; the old handle keeps the old one
    let tmp = d.path().join("tmp");
    std::fs::write(&tmp, b"two").unwrap();
    let replacement = FileId::lstat(&tmp).unwrap();
    std::fs::rename(&tmp, &b).unwrap();
    assert_eq!(FileId::lstat(&b).unwrap(), replacement);
    assert_ne!(replacement, id);
    assert_eq!(FileId::of_file(&handle).unwrap(), id);

    // a directory has an identity of its own
    let dir_id = FileId::lstat(d.path()).unwrap();
    assert_ne!(dir_id, id);
    assert_eq!(FileId::lstat(d.path()).unwrap(), dir_id);
}

/// A failed identity read is an I/O error that names the path, of the failed call's kind.
#[test]
fn wx2_identity_failure_is_io() {
    let d = tempfile::tempdir().unwrap();
    let missing = d.path().join("missing");
    let e = FileId::lstat(&missing).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
    let prefix = format!(
        "could not read the file identity of {}: ",
        missing.display()
    );
    let text = e.to_string();
    assert!(text.starts_with(&prefix), "{text}");
    assert!(text.len() > prefix.len(), "the OS error follows: {text}");
    let store = StoreError::from(e);
    assert!(matches!(store, StoreError::Io(_)), "{store:?}");
    assert_eq!(store.to_string(), format!("i/o error: {text}"));
    assert!(!rdy_store::fileid::same_file(&missing, &missing));
}
