#![cfg(unix)]
// Unix only: it reads file modes and owners. FW-XP1 Phase 4 ports it.
//! Private and settings replacements (`fsutil::write_private_durable`,
//! `fsutil::write_settings_durable`) and the privacy helpers moved into `pio`.

use rdy_store::fsutil::{
    PrivateWriteStage, with_private_write_probe_for_tests, write_durable, write_private_durable,
    write_settings_durable,
};
use rdy_store::pio;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// One probe call: the stage, the path, its mode and its length.
type Seen = (PrivateWriteStage, PathBuf, u32, u64);

#[test]
fn hsa2_private_bytes_are_private_before_write() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("credentials.yaml");
    // an old, wider file is replaced by a private one
    fs::write(&path, b"old").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let seen: std::rc::Rc<std::cell::RefCell<Vec<Seen>>> = Default::default();
    let log = seen.clone();
    let r = with_private_write_probe_for_tests(
        move |stage, p: &Path| {
            let m = fs::symlink_metadata(p)?;
            log.borrow_mut().push((
                stage,
                p.to_path_buf(),
                m.permissions().mode() & 0o777,
                m.len(),
            ));
            Ok(())
        },
        || write_private_durable(&path, b"secret bytes"),
    );
    r.unwrap();
    let seen = seen.borrow();
    let stages: Vec<PrivateWriteStage> = seen.iter().map(|s| s.0).collect();
    assert_eq!(
        stages,
        [
            PrivateWriteStage::Created,
            PrivateWriteStage::Written,
            PrivateWriteStage::Synced,
            PrivateWriteStage::Renamed,
            PrivateWriteStage::DirSynced,
        ]
    );
    let tmp = d.path().join(".credentials.yaml.tmp");
    // the temp file is 0600 and still empty when the write starts
    assert_eq!(seen[0].1, tmp);
    assert_eq!(
        seen[0].2, 0o600,
        "the temp is private before any byte is written"
    );
    assert_eq!(
        seen[0].3, 0,
        "no byte is written before the temp is private"
    );
    assert_eq!((seen[1].2, seen[1].3), (0o600, 12));
    assert_eq!(seen[3].1, path);
    assert_eq!(seen[3].2, 0o600);
    assert_eq!(seen[4].1, d.path());
    assert_eq!(fs::read(&path).unwrap(), b"secret bytes");
    assert_eq!(mode(&path), 0o600);
    assert!(pio::is_private_file(&fs::symlink_metadata(&path).unwrap()));
    assert_eq!(names(d.path()), ["credentials.yaml"]);
}

#[test]
fn hsa2_settings_writes_keep_the_target_mode_and_new_files_are_0600() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("config.yaml");
    write_settings_durable(&path, b"a: 1\n").unwrap();
    assert_eq!(mode(&path), 0o600, "a new settings file is 0600");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let temp_modes: std::rc::Rc<std::cell::RefCell<Vec<u32>>> = Default::default();
    let log = temp_modes.clone();
    with_private_write_probe_for_tests(
        move |stage, p: &Path| {
            if stage == PrivateWriteStage::Created {
                log.borrow_mut().push(mode(p));
            }
            Ok(())
        },
        || write_settings_durable(&path, b"a: 2\n"),
    )
    .unwrap();
    assert_eq!(*temp_modes.borrow(), [0o644]);
    assert_eq!(
        mode(&path),
        0o644,
        "an existing settings file keeps its mode"
    );
    assert_eq!(fs::read(&path).unwrap(), b"a: 2\n");
    // write_durable is unchanged: it sets no mode and calls no probe
    let plain = d.path().join("plain.txt");
    with_private_write_probe_for_tests(
        |_, _: &Path| -> io::Result<()> { panic!("write_durable calls no probe") },
        || write_durable(&plain, b"x"),
    )
    .unwrap();
    assert_eq!(fs::read(&plain).unwrap(), b"x");
}

#[test]
fn hsa2_a_probe_fault_stops_the_write_and_cleans_the_temp() {
    let faults = [
        (PrivateWriteStage::Created, false),
        (PrivateWriteStage::Written, false),
        (PrivateWriteStage::Synced, false),
        (PrivateWriteStage::Renamed, true),
        (PrivateWriteStage::DirSynced, true),
    ];
    for (fault, renamed) in faults {
        for private in [true, false] {
            let d = tempfile::tempdir().unwrap();
            let path = d.path().join("f.yaml");
            fs::write(&path, b"old bytes").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            let e = with_private_write_probe_for_tests(
                move |stage, _: &Path| {
                    if stage == fault {
                        Err(io::Error::other("injected"))
                    } else {
                        Ok(())
                    }
                },
                || {
                    if private {
                        write_private_durable(&path, b"new bytes")
                    } else {
                        write_settings_durable(&path, b"new bytes")
                    }
                },
            )
            .unwrap_err();
            assert_eq!(e.to_string(), "injected", "{fault:?}");
            let want: &[u8] = if renamed { b"new bytes" } else { b"old bytes" };
            assert_eq!(
                fs::read(&path).unwrap(),
                want,
                "{fault:?} private={private}"
            );
            assert_eq!(names(d.path()), ["f.yaml"], "{fault:?}: no temp is left");
        }
    }
}

#[test]
fn hsa2_a_nested_probe_is_refused_and_an_unwind_restores_the_thread() {
    let nested = std::panic::catch_unwind(|| {
        with_private_write_probe_for_tests(
            |_, _: &Path| Ok(()),
            || with_private_write_probe_for_tests(|_, _: &Path| Ok(()), || ()),
        )
    });
    assert!(nested.is_err(), "a nested probe is refused");
    // the unwind removed the outer probe: a new one installs and runs
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("x");
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let c = calls.clone();
    with_private_write_probe_for_tests(
        move |_, _: &Path| {
            c.set(c.get() + 1);
            Ok(())
        },
        || write_private_durable(&path, b"x"),
    )
    .unwrap();
    assert_eq!(calls.get(), 5);
    // the probe is per thread: another thread's write is not probed
    let path2 = d.path().join("y");
    with_private_write_probe_for_tests(
        |_, _: &Path| -> io::Result<()> { panic!("another thread's probe") },
        || {
            std::thread::spawn(move || write_private_durable(&path2, b"y"))
                .join()
                .unwrap()
        },
    )
    .unwrap();
}

#[test]
fn hsa2_pio_privacy_helpers_judge_files_by_owner_kind_and_mode() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("f");
    fs::write(&f, b"x").unwrap();
    fs::set_permissions(&f, fs::Permissions::from_mode(0o600)).unwrap();
    let m = fs::symlink_metadata(&f).unwrap();
    assert_eq!(pio::owner(&m), pio::own_uid());
    assert!(pio::is_private_file(&m));
    for wide in [0o640, 0o604, 0o644, 0o400, 0o700] {
        fs::set_permissions(&f, fs::Permissions::from_mode(wide)).unwrap();
        assert!(
            !pio::is_private_file(&fs::symlink_metadata(&f).unwrap()),
            "{wide:o}"
        );
    }
    // a directory is not a private file, even at 0600
    let dir = d.path().join("dir");
    pio::create_private_dir(&dir).unwrap();
    pio::make_private_dir(&dir).unwrap();
    assert!(pio::is_private_dir(&fs::symlink_metadata(&dir).unwrap()));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!pio::is_private_file(&fs::symlink_metadata(&dir).unwrap()));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    // open_no_follow refuses a link
    let link = d.path().join("link");
    std::os::unix::fs::symlink(&f, &link).unwrap();
    assert!(pio::open_no_follow(fs::OpenOptions::new().read(true), &link).is_err());
    let file = pio::open_no_follow(
        pio::private_file(fs::OpenOptions::new().write(true).create_new(true)),
        &d.path().join("new"),
    )
    .unwrap();
    pio::make_private_file(&file).unwrap();
    assert_eq!(mode(&d.path().join("new")), 0o600);
}

// Fix round 1: calls in an open directory.

#[test]
fn hsa2_pio_directory_calls_never_follow_a_link_at_the_entry() {
    use std::ffi::OsStr;
    let d = tempfile::tempdir().unwrap();
    let dir = pio::open_dir(d.path()).unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    fs::write(elsewhere.path().join("victim"), b"keep").unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), d.path().join("ldir")).unwrap();
    std::os::unix::fs::symlink(elsewhere.path().join("victim"), d.path().join("lfile")).unwrap();
    fs::write(d.path().join("plain"), b"plain").unwrap();
    // a link, or a file where a directory is opened, is a link refusal
    for name in ["ldir", "plain"] {
        let e = pio::open_dir_at(&dir, OsStr::new(name)).unwrap_err();
        assert!(pio::is_link_refusal(&e), "{name}: {e}");
    }
    let e = pio::open_read_at(&dir, OsStr::new("lfile")).unwrap_err();
    assert!(pio::is_link_refusal(&e), "{e}");
    let e = pio::open_read_at(&dir, OsStr::new("missing")).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::NotFound);
    assert!(!pio::is_link_refusal(&e));
    // create_new never follows a link at the entry
    let e = pio::create_private_at(&dir, OsStr::new("lfile")).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(elsewhere.path().join("victim")).unwrap(), b"keep");
    // one entry name only
    for bad in ["", ".", "..", "a/b"] {
        let e = pio::open_read_at(&dir, OsStr::new(bad)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
    }
    // a new directory, made 0700, then a file in it, renamed and removed through handles
    pio::create_private_dir_at(&dir, OsStr::new("sub")).unwrap();
    let e = pio::create_private_dir_at(&dir, OsStr::new("sub")).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
    let sub = pio::open_dir_at(&dir, OsStr::new("sub")).unwrap();
    fs::set_permissions(d.path().join("sub"), fs::Permissions::from_mode(0o500)).unwrap();
    pio::make_private_open_dir(&sub).unwrap();
    assert_eq!(mode(&d.path().join("sub")), 0o700);
    let f = pio::create_private_at(&sub, OsStr::new("a")).unwrap();
    pio::make_private_file(&f).unwrap();
    drop(f);
    // the directory's path changes; the handle still names the same directory
    fs::rename(d.path().join("sub"), d.path().join("moved")).unwrap();
    pio::rename_at(&sub, OsStr::new("a"), OsStr::new("b")).unwrap();
    assert_eq!(names(&d.path().join("moved")), ["b"]);
    let read = pio::open_read_at(&sub, OsStr::new("b")).unwrap();
    assert!(pio::is_private_file(&read.metadata().unwrap()));
    pio::remove_file_at(&sub, OsStr::new("b")).unwrap();
    assert!(names(&d.path().join("moved")).is_empty());
    // the mode tests
    for (m, shared, owner_only) in [
        (0o755, false, false),
        (0o700, false, true),
        (0o400, false, true),
        (0o775, true, false),
        (0o757, true, false),
        (0o1777, true, false),
        (0o640, false, false),
    ] {
        fs::set_permissions(d.path().join("plain"), fs::Permissions::from_mode(m)).unwrap();
        let meta = fs::symlink_metadata(d.path().join("plain")).unwrap();
        assert_eq!(pio::is_shared_writable(&meta), shared, "{m:o}");
        assert_eq!(pio::is_owner_only(&meta), owner_only, "{m:o}");
    }
}

#[test]
fn hsa2_a_replacement_in_a_directory_handle_ignores_later_path_changes() {
    use std::ffi::OsStr;
    let d = tempfile::tempdir().unwrap();
    fs::create_dir(d.path().join("a")).unwrap();
    fs::create_dir(d.path().join("b")).unwrap();
    fs::write(d.path().join("b/f.yaml"), b"b bytes").unwrap();
    let a = pio::open_dir(&d.path().join("a")).unwrap();
    // a is renamed aside and replaced by a link to b: the handle still writes into a
    fs::rename(d.path().join("a"), d.path().join("a-aside")).unwrap();
    std::os::unix::fs::symlink(d.path().join("b"), d.path().join("a")).unwrap();
    let a_path = d.path().join("a");
    let seen: std::rc::Rc<std::cell::RefCell<Vec<(PrivateWriteStage, PathBuf)>>> =
        Default::default();
    let log = seen.clone();
    with_private_write_probe_for_tests(
        move |stage, p: &Path| {
            log.borrow_mut().push((stage, p.to_path_buf()));
            Ok(())
        },
        || rdy_store::fsutil::replace_in_dir(&a, &a_path, OsStr::new("f.yaml"), b"new", None),
    )
    .unwrap();
    assert_eq!(fs::read(d.path().join("a-aside/f.yaml")).unwrap(), b"new");
    assert_eq!(mode(&d.path().join("a-aside/f.yaml")), 0o600);
    assert_eq!(fs::read(d.path().join("b/f.yaml")).unwrap(), b"b bytes");
    assert_eq!(names(&d.path().join("b")), ["f.yaml"]);
    assert_eq!(names(&d.path().join("a-aside")), ["f.yaml"]);
    // the probe is told the paths the handle had at open, for the record only
    let a_path = d.path().join("a");
    assert_eq!(
        *seen.borrow(),
        [
            (PrivateWriteStage::Created, a_path.join(".f.yaml.tmp")),
            (PrivateWriteStage::Written, a_path.join(".f.yaml.tmp")),
            (PrivateWriteStage::Synced, a_path.join(".f.yaml.tmp")),
            (PrivateWriteStage::Renamed, a_path.join("f.yaml")),
            (PrivateWriteStage::DirSynced, a_path.clone()),
        ]
    );
    // kept permissions are applied to the temp file before the bytes
    let keep = fs::Permissions::from_mode(0o640);
    let aside = pio::open_dir(&d.path().join("a-aside")).unwrap();
    rdy_store::fsutil::replace_in_dir(
        &aside,
        &d.path().join("a-aside"),
        OsStr::new("f.yaml"),
        b"kept",
        Some(&keep),
    )
    .unwrap();
    assert_eq!(mode(&d.path().join("a-aside/f.yaml")), 0o640);
    assert_eq!(fs::read(d.path().join("a-aside/f.yaml")).unwrap(), b"kept");
}
