//! The store's open path: missing acknowledged frames (seg1), durable new directories (dur1) and
//! lock errors (lock1).
use rdy_qa::fixtures::*;
use rdy_store::{Store, StoreError};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn seg(store: &Path, n: u32) -> PathBuf {
    store.join("segments").join(format!("seg-{n:06}.log"))
}

#[test]
fn seg1_a_segment_after_a_gap_refuses_to_open() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let h = {
        let mut s = Store::create(&dir).unwrap();
        s.put_blob(b"acknowledged").unwrap()
    };
    // a later segment exists, but the one before it is gone
    std::fs::copy(seg(&dir, 0), seg(&dir, 2)).unwrap();
    let e = Store::create(&dir).err().expect("open must refuse");
    assert!(matches!(&e, StoreError::MissingFrames { .. }), "{e}");
    assert!(e.to_string().contains("seg-000001.log"), "{e}");
    // nothing was rebuilt or truncated: removing the stray segment gives a working store again
    std::fs::remove_file(seg(&dir, 2)).unwrap();
    assert_eq!(
        Store::create(&dir).unwrap().get_blob(&h).unwrap(),
        b"acknowledged"
    );
}

#[test]
fn seg1_an_index_ahead_of_the_log_refuses_instead_of_rolling_back() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let (first, second, cut) = {
        let mut s = Store::create(&dir).unwrap();
        let first = s.put_blob(b"first").unwrap();
        let cut = std::fs::metadata(seg(&dir, 0)).unwrap().len() as usize;
        let second = s.put_blob(b"second").unwrap();
        (first, second, cut)
    };
    let full = std::fs::read(seg(&dir, 0)).unwrap();
    // the second acknowledged write is gone from the log, cut at a frame boundary (not torn)
    std::fs::write(seg(&dir, 0), &full[..cut]).unwrap();
    let e = Store::create(&dir)
        .err()
        .expect("open must refuse, not reindex over the loss");
    assert!(matches!(&e, StoreError::MissingFrames { .. }), "{e}");
    assert!(e.to_string().contains("seg-000000.log"), "{e}");
    // the index was kept: restoring the segment brings both writes back
    std::fs::write(seg(&dir, 0), &full).unwrap();
    let s = Store::create(&dir).unwrap();
    assert_eq!(s.get_blob(&first).unwrap(), b"first");
    assert_eq!(s.get_blob(&second).unwrap(), b"second");
}

#[test]
fn seg1_rdy_open_names_the_missing_segment_in_its_fix() {
    let (dir, mut r) = project();
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "fields": {"title": "Kept", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    drop(r);
    // M2a1 (fmt2): the store is .rdy/store again
    let store = dir.path().join(".rdy/store");
    std::fs::copy(seg(&store, 0), seg(&store, 2)).unwrap();
    let e = rdy_engine::Rdy::open(dir.path())
        .err()
        .expect("open must refuse");
    assert_eq!(e.code, "E_IO", "{e}");
    assert!(e.fix.contains("seg-000001.log"), "{e}");
}

/// Review Focus 1: a guard for the new gap check (passes before and after the fix).
#[test]
fn seg1_quarantined_tails_are_not_segments() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let h = {
        let mut s = Store::create(&dir).unwrap();
        s.put_blob(b"kept").unwrap()
    };
    // an unacknowledged torn tail: the next open quarantines it as seg-000000.log.torn-<offset>
    let mut bytes = std::fs::read(seg(&dir, 0)).unwrap();
    bytes.extend([0u8; 50]);
    std::fs::write(seg(&dir, 0), &bytes).unwrap();
    drop(Store::create(&dir).unwrap());
    let torn = std::fs::read_dir(dir.join("segments"))
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".torn-")
        })
        .count();
    assert_eq!(torn, 1);
    assert_eq!(Store::create(&dir).unwrap().get_blob(&h).unwrap(), b"kept");
}

#[test]
fn dur1_opening_a_new_store_fsyncs_every_directory_it_created() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("a/b/store");
    let s = Store::create(&dir).unwrap();
    let synced: std::collections::BTreeSet<PathBuf> = s.durable_dirs().iter().cloned().collect();
    for want in [
        base.path().to_path_buf(),
        base.path().join("a"),
        base.path().join("a/b"),
        dir.clone(),
    ] {
        assert!(
            synced.contains(&want),
            "{} not fsynced: {synced:?}",
            want.display()
        );
    }
    drop(s);
    let s = Store::create(&dir).unwrap();
    assert!(
        s.durable_dirs().is_empty(),
        "a reopen creates nothing: {:?}",
        s.durable_dirs()
    );
}

#[test]
fn dur1_create_dir_durable_reports_the_parents_it_synced() {
    let base = tempfile::tempdir().unwrap();
    let deep = base.path().join("x/y");
    let got = rdy_store::fsutil::create_dir_durable(&deep).unwrap();
    assert_eq!(got, vec![base.path().to_path_buf(), base.path().join("x")]);
    assert!(deep.is_dir());
    assert!(
        rdy_store::fsutil::create_dir_durable(&deep)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn lock1_only_a_held_lock_is_e_locked() {
    let dir = Path::new("/projects/p/.rdy/store");
    assert!(matches!(
        rdy_store::lock_error(dir, std::fs::TryLockError::WouldBlock),
        StoreError::Locked(_)
    ));
    let e = rdy_store::lock_error(
        dir,
        std::fs::TryLockError::Error(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "locking is not supported on this file system",
        )),
    );
    assert!(matches!(&e, StoreError::Io(_)), "{e}");
    let e = rdy_engine::RdyError::from(e);
    assert_eq!(e.code, "E_IO", "{e}");
    assert!(e.message.contains("locking is not supported"), "{e}");
}

#[test]
fn lock1_a_second_open_is_still_e_locked() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let _held = Store::create(&dir).unwrap();
    let e = Store::create(&dir)
        .err()
        .expect("the second open is refused");
    assert!(matches!(e, StoreError::Locked(_)), "{e}");
}

#[test]
fn dur2_init_makes_the_config_and_its_directory_entry_durable() {
    let dir = tempfile::tempdir().unwrap();
    let (r, synced) =
        rdy_store::fsutil::record_syncs(|| rdy_engine::Rdy::init(dir.path(), "rdy-standard"));
    r.unwrap();
    let cfg_path = dir.path().join(".rdy.yaml");
    let cfg = synced
        .iter()
        .position(|p| *p == cfg_path)
        .unwrap_or_else(|| panic!(".rdy.yaml was not fsynced: {synced:?}"));
    assert!(
        synced[cfg + 1..].contains(&dir.path().to_path_buf()),
        "the project directory was not fsynced after .rdy.yaml was written: {synced:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&cfg_path).unwrap(),
        "pack: rdy-standard\n"
    );
    assert!(!dir.path().join("..rdy.yaml.tmp").exists());
}

#[test]
fn dur2_write_durable_replaces_a_file_then_syncs_it_and_its_directory() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("f.txt");
    std::fs::write(&p, "old").unwrap();
    let (r, synced) =
        rdy_store::fsutil::record_syncs(|| rdy_store::fsutil::write_durable(&p, b"new"));
    r.unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "new");
    assert_eq!(synced, vec![p.clone(), dir.path().to_path_buf()]);
}

/// Guard (passes once `record_syncs` exists): a directory fsync succeeds on this platform, through
/// `sync_all` (F_FULLFSYNC on macOS).
#[test]
fn dur3_sync_dir_flushes_a_directory_on_this_platform() {
    let dir = tempfile::tempdir().unwrap();
    let (r, synced) = rdy_store::fsutil::record_syncs(|| rdy_store::fsutil::sync_dir(dir.path()));
    r.unwrap();
    assert_eq!(synced, vec![dir.path().to_path_buf()]);
}

/// Probe for the final review: on macOS a directory fsync must cost a drive flush (about 4 ms),
/// not a plain fsync (about 0.03 ms).
#[test]
#[ignore = "probe: run by hand on macOS"]
fn dur3_probe_sync_dir_costs_a_drive_flush() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = vec![];
    for i in 0..21 {
        std::fs::write(dir.path().join(format!("f{i}")), b"x").unwrap();
        let s = Instant::now();
        rdy_store::fsutil::sync_dir(dir.path()).unwrap();
        t.push(s.elapsed());
    }
    t.sort();
    println!("sync_dir median {:?}", t[10]);
}
