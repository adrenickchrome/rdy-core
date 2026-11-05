//! Private and settings replacements (`fsutil::write_private_durable`,
//! `fsutil::write_settings_durable`) and the privacy helpers moved into `pio`.
//!
//! Its file modes and owners go through `rdy_store::pio` (FW-XP1 D-1RXEA); the expected modes are
//! Unix modes, which FW-XP1 Phase 4 ports with the Windows privacy rules.

#[cfg(unix)]
use rdy_store::fsutil::write_durable;
use rdy_store::fsutil::{
    PrivateWriteStage, with_private_write_probe_for_tests, write_private_durable,
    write_settings_durable,
};
use rdy_store::pio;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    pio::mode_of(&fs::symlink_metadata(path).unwrap()) & 0o777
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
#[cfg(unix)]
type Seen = (PrivateWriteStage, PathBuf, u32, u64);

// Unix modes: the Windows witnesses are the wx4_ pins below.
#[cfg(unix)]
#[test]
fn hsa2_private_bytes_are_private_before_write() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("credentials.yaml");
    // an old, wider file is replaced by a private one
    fs::write(&path, b"old").unwrap();
    fs::set_permissions(&path, pio::permissions(0o644)).unwrap();
    let seen: std::rc::Rc<std::cell::RefCell<Vec<Seen>>> = Default::default();
    let log = seen.clone();
    let r = with_private_write_probe_for_tests(
        move |stage, p: &Path| {
            let m = fs::symlink_metadata(p)?;
            log.borrow_mut()
                .push((stage, p.to_path_buf(), pio::mode_of(&m) & 0o777, m.len()));
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

// Unix modes: the Windows witnesses are the wx4_ pins below.
#[cfg(unix)]
#[test]
fn hsa2_settings_writes_keep_the_target_mode_and_new_files_are_0600() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("config.yaml");
    write_settings_durable(&path, b"a: 1\n").unwrap();
    assert_eq!(mode(&path), 0o600, "a new settings file is 0600");
    fs::set_permissions(&path, pio::permissions(0o644)).unwrap();
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
            #[cfg(unix)]
            fs::set_permissions(&path, pio::permissions(0o600)).unwrap();
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

// Unix modes: the Windows witnesses are the wx4_ pins below.
#[cfg(unix)]
#[test]
fn hsa2_pio_privacy_helpers_judge_files_by_owner_kind_and_mode() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("f");
    fs::write(&f, b"x").unwrap();
    fs::set_permissions(&f, pio::permissions(0o600)).unwrap();
    let m = fs::symlink_metadata(&f).unwrap();
    assert_eq!(pio::owner(&m), pio::own_uid());
    assert!(pio::is_private_file(&m));
    for wide in [0o640, 0o604, 0o644, 0o400, 0o700] {
        fs::set_permissions(&f, pio::permissions(wide)).unwrap();
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
    fs::set_permissions(&dir, pio::permissions(0o600)).unwrap();
    assert!(!pio::is_private_file(&fs::symlink_metadata(&dir).unwrap()));
    fs::set_permissions(&dir, pio::permissions(0o700)).unwrap();
    // open_no_follow refuses a link
    let link = d.path().join("link");
    pio::symlink(&f, &link).unwrap();
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

// Unix modes: the Windows witnesses are the wx4_ pins below.
#[cfg(unix)]
#[test]
fn hsa2_pio_directory_calls_never_follow_a_link_at_the_entry() {
    use std::ffi::OsStr;
    let d = tempfile::tempdir().unwrap();
    let dir = pio::open_dir(d.path()).unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    fs::write(elsewhere.path().join("victim"), b"keep").unwrap();
    pio::symlink(elsewhere.path(), d.path().join("ldir")).unwrap();
    pio::symlink(elsewhere.path().join("victim"), d.path().join("lfile")).unwrap();
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
        fs::set_permissions(d.path().join("plain"), pio::permissions(m)).unwrap();
        let meta = fs::symlink_metadata(d.path().join("plain")).unwrap();
        assert_eq!(pio::is_shared_writable(&meta), shared, "{m:o}");
        assert_eq!(pio::is_owner_only(&meta), owner_only, "{m:o}");
    }
    // all users may write the directory, and its sticky bit is unset (ruling gate-g3)
    let dir = d.path().join("modes");
    fs::create_dir(&dir).unwrap();
    for (m, unsticky) in [
        (0o755, false),
        (0o775, false),
        (0o777, true),
        (0o757, true),
        (0o1777, false),
        (0o1757, false),
        (0o700, false),
    ] {
        fs::set_permissions(&dir, pio::permissions(m)).unwrap();
        let meta = fs::symlink_metadata(&dir).unwrap();
        assert_eq!(pio::is_world_writable_unsticky(&meta), unsticky, "{m:o}");
    }
    fs::set_permissions(&dir, pio::permissions(0o700)).unwrap();
}

/// The umask check a config edit runs before it makes a directory: a probe file made with mode
/// 0o700 tells whether the umask keeps that mode whole, and is removed again. It never follows
/// a link and never touches what already stands at its name.
// Unix modes: the Windows witnesses are the wx4_ pins below.
#[cfg(unix)]
#[test]
fn hsa2_pio_the_umask_check_keeps_nothing_and_follows_no_link() {
    use std::ffi::OsStr;
    let d = tempfile::tempdir().unwrap();
    let dir = pio::open_dir(d.path()).unwrap();
    // the answer agrees with the mode a file made 0o700 gets under this process's umask
    let made = pio::file_mode(fs::OpenOptions::new().write(true).create_new(true), 0o700)
        .open(d.path().join("witness"))
        .unwrap();
    let whole = pio::mode_of(&made.metadata().unwrap()) & 0o777 == 0o700;
    drop(made);
    fs::remove_file(d.path().join("witness")).unwrap();
    assert_eq!(
        pio::umask_keeps_0700_at(&dir, OsStr::new(".probe.tmp")).unwrap(),
        whole
    );
    assert!(names(d.path()).is_empty(), "the probe file is removed");
    // a link or a file at the name: AlreadyExists, the link not followed, nothing changed
    let elsewhere = tempfile::tempdir().unwrap();
    pio::symlink(elsewhere.path().join("victim"), d.path().join("lnk")).unwrap();
    fs::write(d.path().join("plain"), b"keep").unwrap();
    for name in ["lnk", "plain"] {
        let e = pio::umask_keeps_0700_at(&dir, OsStr::new(name)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{name}");
    }
    assert!(
        names(elsewhere.path()).is_empty(),
        "the link is not followed"
    );
    assert_eq!(fs::read(d.path().join("plain")).unwrap(), b"keep");
    assert_eq!(names(d.path()), ["lnk", "plain"]);
    // one entry name only
    let e = pio::umask_keeps_0700_at(&dir, OsStr::new("a/b")).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    // the new-directory probe stage reaches the test probe, and its error is returned
    let seen = with_private_write_probe_for_tests(
        |stage, path: &Path| {
            assert_eq!(
                (stage, path),
                (PrivateWriteStage::DirMade, Path::new("/x/y"))
            );
            Err(io::Error::other("probe"))
        },
        || rdy_store::fsutil::probe_dir_made(Path::new("/x/y")),
    );
    assert_eq!(seen.unwrap_err().to_string(), "probe");
}

// Unix modes: the Windows witnesses are the wx4_ pins below.
#[cfg(unix)]
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
    pio::symlink(d.path().join("b"), d.path().join("a")).unwrap();
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
    let keep = pio::permissions(0o640);
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

// Task 4 (FW-XP1 Phase 4): privacy and no-follow on every platform. Each pin runs on Unix with
// modes and on Windows with security descriptors: a private object there is this account's, with
// a protected DACL that holds one allow ACE, for this account, and no inherited access.

/// The privacy of a private object that rdy made: this account's, and no other account's.
const PRIVATE: pio::Privacy = pio::Privacy {
    mine: true,
    private: true,
    owner_only: true,
    shared_writable: false,
    open_to_all: false,
};

/// `path` opened for reading, never through a link (a directory opens too).
fn open_read(path: &Path) -> io::Result<fs::File> {
    pio::open_no_follow(fs::OpenOptions::new().read(true), path)
}

/// The privacy of what `path` names, read from a handle.
fn privacy_at(path: &Path) -> pio::Privacy {
    pio::privacy(&open_read(path).unwrap()).unwrap()
}

/// Lets every account write `path` and, for a directory, remove or replace its entries, and
/// hands that access to what is made in it: mode 0o777 (a directory) or 0o666 on Unix; on
/// Windows inheritable full access for Everyone and for Users.
fn widen(path: &Path) {
    #[cfg(unix)]
    {
        let mode = if path.is_dir() { 0o777 } else { 0o666 };
        fs::set_permissions(path, pio::permissions(mode)).unwrap();
    }
    #[cfg(windows)]
    {
        let inherit = if path.is_dir() { "(OI)(CI)" } else { "" };
        for sid in ["*S-1-1-0", "*S-1-5-32-545"] {
            run(
                "icacls",
                &[
                    path.as_os_str(),
                    "/grant".as_ref(),
                    format!("{sid}:{inherit}(F)").as_ref(),
                ],
            );
        }
    }
}

/// Runs `program` with `args` and checks that it succeeded.
#[cfg(windows)]
fn run(program: &str, args: &[&std::ffi::OsStr]) {
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{program} {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A link at `link` to the file or directory `target`: a symbolic link; on Windows a file or
/// directory symbolic link (`mklink`, `mklink /D`).
fn link(target: &Path, link: &Path) {
    #[cfg(unix)]
    pio::symlink(target, link).unwrap();
    #[cfg(windows)]
    {
        let mut args: Vec<&std::ffi::OsStr> = vec!["/C".as_ref(), "mklink".as_ref()];
        if target.is_dir() {
            args.push("/D".as_ref());
        }
        args.extend([link.as_os_str(), target.as_os_str()]);
        run("cmd", &args);
    }
}

/// On Windows: `f`'s owner is this account, and its DACL is protected and holds exactly the one
/// ACE that rdy gives a private object of `kind`.
#[cfg(windows)]
fn assert_only_the_account_ace(f: &fs::File, kind: pio::ObjectKind, what: &str) {
    let me = pio::current_account().unwrap();
    let s = pio::security_of(f).unwrap();
    assert_eq!(s.owner, Some(me), "{what}: the owner");
    assert!(s.protected, "{what}: no inherited access");
    assert_eq!(
        s.dacl,
        Some(vec![pio::Ace::private(me, kind)]),
        "{what}: the ACEs"
    );
}

#[test]
fn wx4_private_objects_have_only_the_account_ace() {
    let d = tempfile::tempdir().unwrap();
    // a new private directory, and an existing one opened to all and made private
    let new_dir = d.path().join("run");
    pio::create_private_dir(&new_dir).unwrap();
    let old_dir = d.path().join("old");
    fs::create_dir(&old_dir).unwrap();
    widen(&old_dir);
    assert!(!privacy_at(&old_dir).owner_only, "the fixture is open");
    pio::make_private_dir(&old_dir).unwrap();
    pio::make_private_dir(&old_dir).unwrap();
    for dir in [&new_dir, &old_dir] {
        let f = open_read(dir).unwrap();
        assert_eq!(pio::privacy(&f).unwrap(), PRIVATE, "{}", dir.display());
        #[cfg(windows)]
        assert_only_the_account_ace(&f, pio::ObjectKind::Dir, &dir.display().to_string());
    }
    // a new private file (the lock's options), private before make_private_file, and after it
    let lock = new_dir.join("daemon.lock");
    let f = pio::open_no_follow(
        pio::private_file(
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false),
        ),
        &lock,
    )
    .unwrap();
    assert_eq!(pio::privacy(&f).unwrap(), PRIVATE, "a new private file");
    pio::make_private_file(&f).unwrap();
    assert_eq!(pio::privacy(&f).unwrap(), PRIVATE, "made private again");
    // an existing file opened to all keeps its access through a private open (the log's
    // options), and make_private_file narrows it to this account
    let log = d.path().join("daemon.log");
    fs::write(&log, b"older run\n").unwrap();
    widen(&log);
    let f = pio::open_no_follow(
        pio::private_file(fs::OpenOptions::new().append(true).create(true)),
        &log,
    )
    .unwrap();
    let before = pio::privacy(&f).unwrap();
    assert!(
        before.mine && !before.private && before.shared_writable,
        "{before:?}"
    );
    pio::make_private_file(&f).unwrap();
    for (path, f) in [(&lock, open_read(&lock).unwrap()), (&log, f)] {
        assert_eq!(pio::privacy(&f).unwrap(), PRIVATE, "{}", path.display());
        #[cfg(windows)]
        assert_only_the_account_ace(&f, pio::ObjectKind::File, &path.display().to_string());
    }
    assert_eq!(fs::read(&log).unwrap(), b"older run\n", "the bytes stay");
}

#[test]
fn wx4_inheritance_and_foreign_owner_are_refused() {
    let d = tempfile::tempdir().unwrap();
    // a directory that hands every account access to what is made in it
    let open = d.path().join("open");
    fs::create_dir(&open).unwrap();
    widen(&open);
    let p = privacy_at(&open);
    assert!(
        p.mine && !p.private && !p.owner_only && p.shared_writable && p.open_to_all,
        "{p:?}"
    );
    // a plain file made in it gets that access: it is not private, and others may write it
    let plain = open.join("plain");
    fs::write(&plain, b"plain").unwrap();
    #[cfg(unix)]
    fs::set_permissions(&plain, pio::permissions(0o666)).unwrap();
    let p = privacy_at(&plain);
    assert!(
        p.mine && !p.private && !p.owner_only && p.shared_writable && p.open_to_all,
        "{p:?}"
    );
    // a private directory and a private file made in it take none of it
    let run = open.join("run");
    pio::create_private_dir(&run).unwrap();
    assert_eq!(privacy_at(&run), PRIVATE, "a private directory");
    let file = open.join("private");
    let f = pio::open_no_follow(
        pio::private_file(fs::OpenOptions::new().write(true).create_new(true)),
        &file,
    )
    .unwrap();
    assert_eq!(pio::privacy(&f).unwrap(), PRIVATE, "a private file");
    // the plain file, made private, keeps none of the access it was given
    let f = pio::open_no_follow(fs::OpenOptions::new().read(true).write(true), &plain).unwrap();
    pio::make_private_file(&f).unwrap();
    assert_eq!(
        pio::privacy(&f).unwrap(),
        PRIVATE,
        "the plain file made private"
    );
    #[cfg(windows)]
    {
        let s = pio::security_of(&f).unwrap();
        assert!(s.protected, "the inherited access is gone");
        assert_eq!(s.dacl.map(|a| a.len()), Some(1));
    }
    // a directory and a file of another account are never made private, and stay as they are
    #[cfg(unix)]
    let (theirs_dir, theirs_file) = (PathBuf::from("/"), PathBuf::from("/etc/hosts"));
    #[cfg(windows)]
    let (theirs_dir, theirs_file) = {
        let root = PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot is set"));
        (root.clone(), root.join(r"System32\drivers\etc\hosts"))
    };
    let before = privacy_at(&theirs_dir);
    assert!(
        !before.mine,
        "{} is another account's",
        theirs_dir.display()
    );
    let e = pio::make_private_dir(&theirs_dir).unwrap_err();
    #[cfg(windows)]
    assert_eq!(
        e.to_string(),
        format!("{} is not private to this account", theirs_dir.display())
    );
    #[cfg(unix)]
    assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
    assert_eq!(privacy_at(&theirs_dir), before, "unchanged");
    let f = open_read(&theirs_file).unwrap();
    let before = pio::privacy(&f).unwrap();
    assert!(
        !before.mine,
        "{} is another account's",
        theirs_file.display()
    );
    let e = pio::make_private_file(&f).unwrap_err();
    #[cfg(windows)]
    {
        let text = e.to_string().to_lowercase();
        assert!(
            text.ends_with(r"\system32\drivers\etc\hosts is not private to this account"),
            "{e}"
        );
    }
    #[cfg(unix)]
    assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
    assert_eq!(pio::privacy(&f).unwrap(), before, "unchanged");
}

#[test]
fn wx4_run_entries_refuse_every_reparse_tag() {
    let d = tempfile::tempdir().unwrap();
    let run = d.path().join("run");
    pio::create_private_dir(&run).unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let victim = elsewhere.path().join("victim");
    fs::write(&victim, b"keep").unwrap();
    let victim_dir = elsewhere.path().join("victim-dir");
    fs::create_dir(&victim_dir).unwrap();
    widen(&victim);
    widen(&victim_dir);
    let (file_before, dir_before) = (privacy_at(&victim), privacy_at(&victim_dir));
    // a link to a file, a link to a directory and, on Windows, a junction: a mount-point
    // reparse point. The provider's own tests cover the other reparse tags.
    link(&victim, &run.join("lfile"));
    link(&victim_dir, &run.join("ldir"));
    #[cfg(windows)]
    run_mklink_junction(&victim_dir, &run.join("junction"));
    let entries: &[&str] = if cfg!(windows) {
        &["lfile", "ldir", "junction"]
    } else {
        &["lfile", "ldir"]
    };
    for &name in entries {
        let at = run.join(name);
        let opens: [(&str, fs::OpenOptions); 4] = [
            ("read", {
                let mut o = fs::OpenOptions::new();
                o.read(true);
                o
            }),
            ("write", {
                let mut o = fs::OpenOptions::new();
                o.write(true);
                o
            }),
            ("append and create", {
                let mut o = fs::OpenOptions::new();
                o.append(true).create(true);
                o
            }),
            ("read, write and create", {
                let mut o = fs::OpenOptions::new();
                o.read(true).write(true).create(true).truncate(false);
                o
            }),
        ];
        for (how, mut opts) in opens {
            let e = pio::open_no_follow(&mut opts, &at).unwrap_err();
            #[cfg(windows)]
            assert_eq!(
                e.to_string(),
                format!(
                    "{} is a reparse point; rdy will not follow it",
                    at.display()
                ),
                "{name}, {how}"
            );
            #[cfg(unix)]
            let _ = (e, how);
        }
        // a private create never follows the entry: one of its own is refused as existing, and
        // one that may open what stands there is refused as a link
        let e = pio::open_no_follow(
            pio::private_file(fs::OpenOptions::new().write(true).create_new(true)),
            &at,
        )
        .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{name}: {e}");
        let e = pio::open_no_follow(
            pio::private_file(
                fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false),
            ),
            &at,
        )
        .unwrap_err();
        #[cfg(windows)]
        assert_eq!(
            e.to_string(),
            format!(
                "{} is a reparse point; rdy will not follow it",
                at.display()
            ),
            "{name}"
        );
        #[cfg(unix)]
        let _ = e;
        // on Windows a directory is made private only when it is no reparse point
        #[cfg(windows)]
        {
            let e = pio::make_private_dir(&at).unwrap_err();
            assert_eq!(
                e.to_string(),
                format!(
                    "{} is a reparse point; rdy will not follow it",
                    at.display()
                ),
                "{name}"
            );
        }
    }
    // nothing was followed: the targets keep their bytes, entries and access
    assert_eq!(fs::read(&victim).unwrap(), b"keep");
    assert!(names(&victim_dir).is_empty());
    assert_eq!(names(elsewhere.path()), ["victim", "victim-dir"]);
    assert_eq!(
        (privacy_at(&victim), privacy_at(&victim_dir)),
        (file_before, dir_before)
    );
}

/// A junction (a mount-point reparse point) at `link` to the directory `target`.
#[cfg(windows)]
fn run_mklink_junction(target: &Path, link: &Path) {
    run(
        "cmd",
        &[
            "/C".as_ref(),
            "mklink".as_ref(),
            "/J".as_ref(),
            link.as_os_str(),
            target.as_os_str(),
        ],
    );
}

#[test]
fn wx4_private_temp_is_private_at_creation() {
    // a directory that hands every account access to what is made in it
    let d = tempfile::tempdir().unwrap();
    let open = d.path().join("open");
    fs::create_dir(&open).unwrap();
    widen(&open);
    // the private file constructor, which the future MCP token is to use (none is made here):
    // private when it is made, before make_private_file and before a byte is written
    let token = open.join("token");
    let mut f = pio::open_no_follow(
        pio::private_file(fs::OpenOptions::new().write(true).create_new(true)),
        &token,
    )
    .unwrap();
    assert_eq!(f.metadata().unwrap().len(), 0, "nothing written yet");
    assert_eq!(pio::privacy(&f).unwrap(), PRIVATE, "private at creation");
    #[cfg(windows)]
    assert_only_the_account_ace(&f, pio::ObjectKind::File, "the token");
    io::Write::write_all(&mut f, b"secret").unwrap();
    drop(f);
    assert_eq!(privacy_at(&token), PRIVATE);
    assert_eq!(fs::read(&token).unwrap(), b"secret");
    // the private temp of a durable private write: private and empty when it is made
    let target = open.join("credentials.yaml");
    let seen: std::rc::Rc<std::cell::RefCell<Vec<(pio::Privacy, u64)>>> = Default::default();
    let log = seen.clone();
    with_private_write_probe_for_tests(
        move |stage, p: &Path| {
            if stage == PrivateWriteStage::Created {
                let f = open_read(p)?;
                log.borrow_mut()
                    .push((pio::privacy(&f)?, f.metadata()?.len()));
            }
            Ok(())
        },
        || write_private_durable(&target, b"secret bytes"),
    )
    .unwrap();
    assert_eq!(*seen.borrow(), [(PRIVATE, 0)]);
    // a replacement keeps the private descriptor of the temp, not the access of what it replaced
    let wide = open.join("config.yaml");
    fs::write(&wide, b"old").unwrap();
    widen(&wide);
    write_private_durable(&wide, b"new").unwrap();
    for path in [&target, &wide] {
        let f = open_read(path).unwrap();
        assert_eq!(pio::privacy(&f).unwrap(), PRIVATE, "{}", path.display());
        #[cfg(windows)]
        assert_only_the_account_ace(&f, pio::ObjectKind::File, &path.display().to_string());
    }
    assert_eq!(fs::read(&wide).unwrap(), b"new");
    assert_eq!(names(&open), ["config.yaml", "credentials.yaml", "token"]);
}
