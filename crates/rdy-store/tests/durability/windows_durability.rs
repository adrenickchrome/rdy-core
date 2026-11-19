//! Durable directory flushes and renames (`rdy_store::pio`, D-FPKPS) and the `fsutil` writes that
//! use them (FW-XP1 Phase 4). The pins run on every platform:
//! - a directory flush is the platform's real call or an error, never a success made up;
//! - a rename makes the platform's write-through call and never copies;
//! - a failed flush is never acknowledged;
//! - a failed replacement leaves each target whole, with its old bytes or its new bytes.
//!
//! A test backend (`pio::on_durable`) sees each durable step just before and just after its
//! platform call. An error it gives fails the step: before the call, the call is not made; after
//! it, the call was made. Injected errors are OS error numbers, so a message is pinned by its
//! number, never by an OS's translation.

use rdy_store::error::StoreError;
use rdy_store::fsutil::{self, PrivateWriteStage};
use rdy_store::pio::{self, Boundary, DurableStep};
use std::cell::RefCell;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The flags of every rename's platform call. Windows: MoveFileExW with MOVEFILE_REPLACE_EXISTING
/// (0x1) and MOVEFILE_WRITE_THROUGH (0x8). Unix's rename takes none.
#[cfg(windows)]
const RENAME_FLAGS: u32 = 0x1 | 0x8;
#[cfg(unix)]
const RENAME_FLAGS: u32 = 0;

/// MoveFileExW's MOVEFILE_COPY_ALLOWED: a move that may copy and delete. No rename sets it.
const COPY_ALLOWED: u32 = 0x2;

/// ERROR_ACCESS_DENIED and ERROR_INVALID_FUNCTION on Windows: a flush refused, and a file system
/// that cannot flush. (On Unix these numbers are EIO and EPERM: failed calls too.)
const ACCESS_DENIED: i32 = 5;
const INVALID_FUNCTION: i32 = 1;

/// The OS's answer to a move across volumes: ERROR_NOT_SAME_DEVICE on Windows, EXDEV on Unix.
#[cfg(windows)]
const CROSS_DEVICE: i32 = 17;
#[cfg(unix)]
const CROSS_DEVICE: i32 = 18;

/// What a backend saw: each step and its boundary, in order.
type Seen = Vec<(DurableStep, Boundary)>;

/// Runs `run` with a durable backend on this thread that records each step and fails each step for
/// which `fail` gives an error. The backend is removed again, even if `run` panics.
fn with_backend<T>(
    mut fail: impl FnMut(&DurableStep, Boundary) -> Option<io::Error> + 'static,
    run: impl FnOnce() -> T,
) -> (T, Seen) {
    struct Off;
    impl Drop for Off {
        fn drop(&mut self) {
            pio::on_durable(None);
        }
    }
    let seen: Rc<RefCell<Seen>> = Rc::default();
    let log = seen.clone();
    pio::on_durable(Some(Box::new(move |step: &DurableStep, at: Boundary| {
        log.borrow_mut().push((step.clone(), at));
        fail(step, at).map_or(Ok(()), Err)
    })));
    let off = Off;
    let out = run();
    drop(off);
    let seen = seen.borrow().clone();
    (out, seen)
}

/// [`with_backend`] with a backend that fails nothing.
fn watched<T>(run: impl FnOnce() -> T) -> (T, Seen) {
    with_backend(|_, _| None, run)
}

fn flush(path: &Path) -> DurableStep {
    DurableStep::Flush(path.to_path_buf())
}

/// The file names of a rename step's two paths and its flags.
fn rename_names(step: &DurableStep) -> Option<(String, String, u32)> {
    match step {
        DurableStep::Rename { from, to, flags } => Some((name_of(from), name_of(to), *flags)),
        DurableStep::Flush(_) => None,
    }
}

fn name_of(p: &Path) -> String {
    p.file_name().unwrap().to_string_lossy().into_owned()
}

/// Every rename step of `seen` at the boundary `at`, as [`rename_names`] gives it.
fn renames(seen: &Seen, at: Boundary) -> Vec<(String, String, u32)> {
    seen.iter()
        .filter(|(_, b)| *b == at)
        .filter_map(|(s, _)| rename_names(s))
        .collect()
}

/// The text of `e` as the store shows it (E_IO: `i/o error: {detail}`).
fn store_text(e: io::Error) -> String {
    StoreError::from(e).to_string()
}

/// The names and bytes of every entry of `dir`, sorted by name.
fn contents(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut all: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let bytes = if e.file_type().unwrap().is_file() {
                fs::read(e.path()).unwrap()
            } else {
                b"<dir>".to_vec()
            };
            (e.file_name().to_string_lossy().into_owned(), bytes)
        })
        .collect();
    all.sort();
    all
}

/// One way of `fsutil` to replace the file at a path with bytes.
type Writer = fn(&Path, &[u8]) -> io::Result<()>;

/// The ways `fsutil` replaces a file, each named for messages.
fn writers() -> Vec<(&'static str, Writer)> {
    vec![
        ("write_durable", fsutil::write_durable),
        ("write_private_durable", fsutil::write_private_durable),
        ("write_settings_durable", fsutil::write_settings_durable),
        ("write_durable_batch", |p: &Path, b: &[u8]| {
            fsutil::write_durable_batch(&[(p.to_path_buf(), b)], &[]).map_err(|(_, e)| e)
        }),
    ]
}

#[test]
fn wx6_directory_flush_is_real_or_io() {
    let root = tempfile::tempdir().unwrap();
    let d = root.path();
    // the real call runs between the two boundaries and succeeds on a directory of this account's
    let ((), seen) = watched(|| pio::sync_dir(d).unwrap());
    assert_eq!(
        seen,
        [(flush(d), Boundary::Before), (flush(d), Boundary::After)]
    );
    let dir = pio::open_dir(d).unwrap();
    let ((), seen) = watched(|| pio::sync_open_dir(&dir, d).unwrap());
    assert_eq!(
        seen,
        [(flush(d), Boundary::Before), (flush(d), Boundary::After)]
    );
    // fsutil's flush is pio's, and it records the directory once the flush is done
    let ((r, synced), seen) = watched(|| fsutil::record_syncs(|| fsutil::sync_dir(d)));
    r.unwrap();
    assert_eq!(synced, [d.to_path_buf()]);
    assert_eq!(
        seen,
        [(flush(d), Boundary::Before), (flush(d), Boundary::After)]
    );

    // a directory that is not there is an error, never a success
    let missing = d.join("missing");
    for e in [
        pio::sync_dir(&missing).unwrap_err(),
        fsutil::sync_dir(&missing).unwrap_err(),
    ] {
        assert_eq!(e.kind(), io::ErrorKind::NotFound, "{e}");
        let shown = format!("could not sync directory {}: ", missing.display());
        assert!(e.to_string().starts_with(&shown), "{e}");
    }

    // an answer of access denied, or of a file system that cannot flush, is that error, with its
    // number, before the call or after it: never a success
    type Flush = Box<dyn Fn() -> io::Result<()>>;
    let path = d.to_path_buf();
    let calls: Vec<(&str, Flush)> = vec![
        ("pio::sync_dir", Box::new(move || pio::sync_dir(&path))),
        (
            "pio::sync_open_dir",
            Box::new({
                let path = d.to_path_buf();
                move || pio::sync_open_dir(&pio::open_dir(&path)?, &path)
            }),
        ),
        ("fsutil::sync_dir", {
            let path = d.to_path_buf();
            Box::new(move || fsutil::sync_dir(&path))
        }),
    ];
    for code in [ACCESS_DENIED, INVALID_FUNCTION] {
        for at in [Boundary::Before, Boundary::After] {
            for (name, call) in &calls {
                let ((r, synced), seen) = with_backend(
                    move |_, b| (b == at).then(|| io::Error::from_raw_os_error(code)),
                    || fsutil::record_syncs(call),
                );
                let e = r.unwrap_err();
                let os = io::Error::from_raw_os_error(code);
                let text = format!("could not sync directory {}: {os}", d.display());
                assert_eq!(e.to_string(), text, "{name}, error {code}, {at:?}");
                assert!(text.ends_with(&format!("(os error {code})")), "{text}");
                assert_eq!(e.kind(), os.kind(), "{name}");
                assert_eq!(store_text(e), format!("i/o error: {text}"), "{name}");
                assert!(
                    synced.is_empty(),
                    "{name}: a failed flush is never recorded"
                );
                // before the call, the call is not made
                let want = match at {
                    Boundary::Before => vec![(flush(d), Boundary::Before)],
                    Boundary::After => vec![(flush(d), Boundary::Before), (flush(d), at)],
                };
                assert_eq!(seen, want, "{name}, {at:?}");
            }
        }
    }

    // Windows: a directory that this account may not add an entry to cannot be flushed: the
    // platform's own ERROR_ACCESS_DENIED, never a success
    #[cfg(windows)]
    {
        let locked = d.join("locked");
        fs::create_dir(&locked).unwrap();
        deny_adding_entries(&locked);
        if let Some(why) = adding_allowed(&locked) {
            // The deny entry is not in force for this account (a privilege or an ACL of the
            // host overrides it), so the precondition of the failure path does not hold. A
            // flush is then the other half of "real or io": the real call, made and recorded.
            eprintln!("wx6: the deny entry is not in force ({why}); the flush must be real");
            let ((), seen) = watched(|| pio::sync_dir(&locked).unwrap());
            assert_eq!(
                seen,
                [
                    (flush(&locked), Boundary::Before),
                    (flush(&locked), Boundary::After)
                ]
            );
            let dir = pio::open_dir(&locked).unwrap();
            let ((), seen) = watched(|| pio::sync_open_dir(&dir, &locked).unwrap());
            assert_eq!(
                seen,
                [
                    (flush(&locked), Boundary::Before),
                    (flush(&locked), Boundary::After)
                ]
            );
            let (r, synced) = fsutil::record_syncs(|| fsutil::sync_dir(&locked));
            r.unwrap();
            assert_eq!(synced, std::slice::from_ref(&locked));
            drop(dir);
            allow_adding_entries(&locked);
            return;
        }
        let e = pio::sync_dir(&locked).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
        assert!(e.to_string().ends_with("(os error 5)"), "{e}");
        let dir = pio::open_dir(&locked).unwrap();
        let e = pio::sync_open_dir(&dir, &locked).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
        let shown = format!("could not sync directory {}: ", locked.display());
        assert!(e.to_string().starts_with(&shown), "{e}");
        assert!(e.to_string().ends_with("(os error 5)"), "{e}");
        let (r, synced) = fsutil::record_syncs(|| fsutil::sync_dir(&locked));
        assert!(r.is_err() && synced.is_empty());
        drop(dir);
        allow_adding_entries(&locked);
        pio::sync_dir(&locked).unwrap();
    }
}

/// Denies this account the rights to add a file or a directory to `dir` (an explicit deny entry,
/// which applies to an administrator too).
#[cfg(windows)]
fn deny_adding_entries(dir: &Path) {
    icacls(dir, &["/deny", &format!("{}:(WD,AD)", user())]);
}

/// Removes the deny entry of [`deny_adding_entries`].
#[cfg(windows)]
fn allow_adding_entries(dir: &Path) {
    icacls(dir, &["/remove:d", &user()]);
}

/// Probes whether this account can still add an entry to `dir`: it tries to create a file and a
/// directory in it. `None` when both are refused with access denied (the deny entry is in force);
/// otherwise the reason, and any entry the probe made is removed again.
#[cfg(windows)]
fn adding_allowed(dir: &Path) -> Option<String> {
    let file = dir.join("probe-file");
    let sub = dir.join("probe-dir");
    let made_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file)
        .map(drop);
    let made_dir = fs::create_dir(&sub);
    let denied =
        |r: &io::Result<()>| matches!(r, Err(e) if e.raw_os_error() == Some(ACCESS_DENIED));
    if denied(&made_file) && denied(&made_dir) {
        return None;
    }
    if made_file.is_ok() {
        fs::remove_file(&file).unwrap();
    }
    if made_dir.is_ok() {
        fs::remove_dir(&sub).unwrap();
    }
    Some(format!(
        "as {}, creating a file gave {made_file:?} and a directory {made_dir:?}",
        user()
    ))
}

/// This process's account, as its token's SID (`*S-1-5-...`, which icacls takes): the account the
/// deny entry must name. `USERNAME` is not used: a process started for another account can inherit
/// it from its parent, and a deny entry for the parent's account does not stop this one.
#[cfg(windows)]
fn user() -> String {
    let out = std::process::Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .unwrap();
    assert!(out.status.success(), "whoami: {out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    let sid = text.trim().rsplit(',').next().unwrap().trim_matches('"');
    assert!(sid.starts_with("S-1-"), "whoami: {text}");
    format!("*{sid}")
}

#[cfg(windows)]
fn icacls(path: &Path, args: &[&str]) {
    let out = std::process::Command::new("icacls")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "icacls {args:?}: {out:?}");
}

#[test]
fn wx6_rename_uses_write_through() {
    let root = tempfile::tempdir().unwrap();
    let d = root.path();
    fs::write(d.join("a"), b"new").unwrap();
    fs::write(d.join("b"), b"old").unwrap();
    // a durable rename: one platform call with the write-through flags, replacing the target
    let ((), seen) = watched(|| pio::rename_durable(&d.join("a"), &d.join("b")).unwrap());
    let want = ("a".to_string(), "b".to_string(), RENAME_FLAGS);
    assert_eq!(
        renames(&seen, Boundary::Before),
        std::slice::from_ref(&want)
    );
    assert_eq!(renames(&seen, Boundary::After), [want]);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(RENAME_FLAGS & COPY_ALLOWED, 0, "a rename never copies");
    assert_eq!(fs::read(d.join("b")).unwrap(), b"new");
    assert!(!d.join("a").exists());
    // between two directories
    fs::create_dir(d.join("sub")).unwrap();
    let ((), seen) = watched(|| pio::rename_durable(&d.join("b"), &d.join("sub/c")).unwrap());
    assert_eq!(
        renames(&seen, Boundary::After),
        [("b".to_string(), "c".to_string(), RENAME_FLAGS)]
    );
    assert_eq!(fs::read(d.join("sub/c")).unwrap(), b"new");
    // an anchored rename too
    let dir = pio::open_dir(d).unwrap();
    fs::write(d.join("x"), b"x").unwrap();
    let ((), seen) = watched(|| pio::rename_at(&dir, OsStr::new("x"), OsStr::new("y")).unwrap());
    assert_eq!(
        renames(&seen, Boundary::After),
        [("x".to_string(), "y".to_string(), RENAME_FLAGS)]
    );
    assert_eq!(fs::read(d.join("y")).unwrap(), b"x");

    // every fsutil replacement renames with those flags, then flushes the directory
    for (name, write) in writers() {
        let target = d.join(format!("{name}.txt"));
        fs::write(&target, b"old").unwrap();
        let ((), seen) = watched(|| write(&target, b"new").unwrap());
        let tmp = format!(".{name}.txt.tmp");
        assert_eq!(
            renames(&seen, Boundary::After),
            [(tmp, format!("{name}.txt"), RENAME_FLAGS)],
            "{name}"
        );
        let last_rename = seen
            .iter()
            .rposition(|(s, _)| rename_names(s).is_some())
            .unwrap();
        assert_eq!(
            seen[last_rename + 1..],
            [(flush(d), Boundary::Before), (flush(d), Boundary::After)],
            "{name}: the directory is flushed after the rename"
        );
        assert_eq!(fs::read(&target).unwrap(), b"new", "{name}");
    }
    // a batch renames every file, then flushes each directory once
    let items: Vec<(PathBuf, &[u8])> = vec![
        (d.join("p.txt"), b"p"),
        (d.join("sub/q.txt"), b"q"),
        (d.join("r.txt"), b"r"),
    ];
    let ((), seen) = watched(|| fsutil::write_durable_batch(&items, &[]).unwrap());
    let after = renames(&seen, Boundary::After);
    assert_eq!(after.len(), 3, "{seen:?}");
    assert!(after.iter().all(|(_, _, f)| *f == RENAME_FLAGS));
    let flushes: Vec<_> = seen
        .iter()
        .filter(|(s, b)| matches!(s, DurableStep::Flush(_)) && *b == Boundary::After)
        .collect();
    assert_eq!(flushes.len(), 2, "{seen:?}");
}

#[test]
fn wx6_failed_flush_never_acknowledges() {
    let root = tempfile::tempdir().unwrap();
    let d = root.path();
    let injected = || io::Error::from_raw_os_error(ACCESS_DENIED);
    let text = format!(
        "could not sync directory {}: {}",
        d.display(),
        io::Error::from_raw_os_error(ACCESS_DENIED)
    );
    for at in [Boundary::Before, Boundary::After] {
        // each replacement: an error, the directory never recorded as synced
        for (name, write) in writers() {
            let target = d.join(format!("{name}.txt"));
            let ((r, synced), _) = with_backend(
                move |s, b| (matches!(s, DurableStep::Flush(_)) && b == at).then(injected),
                || fsutil::record_syncs(|| write(&target, b"new")),
            );
            let e = r.unwrap_err();
            assert_eq!(e.to_string(), text, "{name}, {at:?}");
            assert!(!synced.contains(&d.to_path_buf()), "{name}: {synced:?}");
        }
        // a private write's probe never sees the directory synced
        let stages: Rc<RefCell<Vec<PrivateWriteStage>>> = Rc::default();
        let log = stages.clone();
        let target = d.join("secret.txt");
        let (r, _) = with_backend(
            move |s, b| (matches!(s, DurableStep::Flush(_)) && b == at).then(injected),
            || {
                fsutil::with_private_write_probe_for_tests(
                    move |stage, _| {
                        log.borrow_mut().push(stage);
                        Ok(())
                    },
                    || fsutil::write_private_durable(&target, b"new"),
                )
            },
        );
        assert_eq!(r.unwrap_err().to_string(), text);
        assert!(stages.borrow().contains(&PrivateWriteStage::Renamed));
        assert!(!stages.borrow().contains(&PrivateWriteStage::DirSynced));
        // a batch: the second directory's flush fails, and the error names that directory
        let sub = d.join("sub");
        fs::create_dir_all(&sub).unwrap();
        let items: Vec<(PathBuf, &[u8])> = vec![(d.join("a.txt"), b"a"), (sub.join("b.txt"), b"b")];
        let failing = sub.clone();
        let ((r, synced), _) = with_backend(
            move |s, b| (*s == DurableStep::Flush(failing.clone()) && b == at).then(injected),
            || fsutil::record_syncs(|| fsutil::write_durable_batch(&items, &[])),
        );
        let (path, e) = r.unwrap_err();
        assert_eq!(path, sub);
        assert!(
            e.to_string()
                .starts_with(&format!("could not sync directory {}: ", sub.display())),
            "{e}"
        );
        assert!(!synced.contains(&sub), "{synced:?}");
        // a new directory: its parent's failed flush is an error, never a directory made durable
        let new = d.join(format!("new-{at:?}/deeper"));
        let ((r, synced), _) = with_backend(
            move |s, b| (matches!(s, DurableStep::Flush(_)) && b == at).then(injected),
            || fsutil::record_syncs(|| fsutil::create_dir_durable(&new)),
        );
        let e = r.unwrap_err();
        assert!(
            e.to_string().starts_with("could not sync directory "),
            "{e}"
        );
        assert!(synced.is_empty(), "{synced:?}");
    }
}

#[test]
fn wx6_failed_replace_keeps_a_recoverable_copy() {
    let root = tempfile::tempdir().unwrap();
    let injected = || io::Error::from_raw_os_error(ACCESS_DENIED);
    for at in [Boundary::Before, Boundary::After] {
        for (name, write) in writers() {
            // each case in a directory of its own, so every entry in it is this case's
            let d = root.path().join(format!("{name}-{at:?}"));
            fs::create_dir(&d).unwrap();
            let target = d.join("t.txt");
            fs::write(&target, b"old bytes").unwrap();
            let (r, seen) = with_backend(
                move |s, b| (rename_names(s).is_some() && b == at).then(injected),
                || write(&target, b"new bytes"),
            );
            let e = r.unwrap_err();
            assert!(
                e.to_string()
                    .contains(&format!("(os error {ACCESS_DENIED})")),
                "{name}: {e}"
            );
            // the target is whole: its old bytes when the rename was not made, its new bytes when
            // it was; every other entry holds whole bytes too, never a part
            let want: &[u8] = match at {
                Boundary::Before => b"old bytes",
                Boundary::After => b"new bytes",
            };
            assert_eq!(fs::read(&target).unwrap(), want, "{name}, {at:?}");
            for (entry, bytes) in contents(&d) {
                assert!(
                    bytes == b"old bytes" || bytes == b"new bytes",
                    "{name}, {at:?}: {entry} holds {bytes:?}"
                );
            }
            // nothing is flushed as if the replacement were done
            let last = seen.last().unwrap();
            assert!(rename_names(&last.0).is_some(), "{name}: {seen:?}");
            // the next write succeeds and leaves no temp file
            write(&target, b"next").unwrap();
            assert_eq!(contents(&d), [("t.txt".to_string(), b"next".to_vec())]);
        }
        // a batch: the second rename fails; the first file is new, the others keep their old
        // bytes, and no temp file stays
        let d = root.path().join(format!("batch-{at:?}"));
        fs::create_dir(&d).unwrap();
        let names = ["a.txt", "b.txt", "c.txt"];
        for n in names {
            fs::write(d.join(n), b"old bytes").unwrap();
        }
        let items: Vec<(PathBuf, &[u8])> = names
            .iter()
            .map(|n| (d.join(n), &b"new bytes"[..]))
            .collect();
        let (r, _) = with_backend(
            move |s, b| {
                let second = rename_names(s).is_some_and(|(_, to, _)| to == "b.txt");
                (second && b == at).then(injected)
            },
            || fsutil::write_durable_batch(&items, &[]),
        );
        let (path, _) = r.unwrap_err();
        assert_eq!(path, d.join("b.txt"));
        let second: &[u8] = match at {
            Boundary::Before => b"old bytes",
            Boundary::After => b"new bytes",
        };
        assert_eq!(
            contents(&d),
            [
                ("a.txt".to_string(), b"new bytes".to_vec()),
                ("b.txt".to_string(), second.to_vec()),
                ("c.txt".to_string(), b"old bytes".to_vec()),
            ]
        );
    }

    // Windows: a target that another handle holds open without delete sharing cannot be
    // replaced; the platform's refusal is the error, and the target keeps its old bytes
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let d = root.path().join("held");
        fs::create_dir(&d).unwrap();
        let target = d.join("t.txt");
        for (name, write) in writers() {
            fs::write(&target, b"old bytes").unwrap();
            // FILE_SHARE_READ (0x1) only
            let held = fs::OpenOptions::new()
                .read(true)
                .share_mode(0x1)
                .open(&target)
                .unwrap();
            let e = write(&target, b"new bytes").unwrap_err();
            let code = e
                .to_string()
                .rsplit_once("(os error ")
                .map(|(_, n)| n.trim_end_matches(')').to_string());
            assert!(
                matches!(code.as_deref(), Some("5") | Some("32")),
                "{name}: {e}"
            );
            drop(held);
            assert_eq!(fs::read(&target).unwrap(), b"old bytes", "{name}");
            for (entry, bytes) in contents(&d) {
                assert!(
                    bytes == b"old bytes" || bytes == b"new bytes",
                    "{name}: {entry} holds {bytes:?}"
                );
            }
            write(&target, b"next").unwrap();
            assert_eq!(contents(&d), [("t.txt".to_string(), b"next".to_vec())]);
        }
    }
}

#[test]
fn wx6_cross_volume_rename_does_not_copy() {
    let root = tempfile::tempdir().unwrap();
    let d = root.path();
    let from = d.join("from.txt");
    let to = d.join("to.txt");
    fs::write(&from, b"bytes").unwrap();
    // the platform's answer to a move across volumes is the error: nothing is copied instead
    let cross = io::Error::from_raw_os_error(CROSS_DEVICE);
    let (r, seen) = with_backend(
        |s, b| {
            (rename_names(s).is_some() && b == Boundary::Before)
                .then(|| io::Error::from_raw_os_error(CROSS_DEVICE))
        },
        || pio::rename_durable(&from, &to),
    );
    let e = r.unwrap_err();
    assert_eq!(
        e.to_string(),
        format!(
            "could not rename {} to {}: {cross}",
            from.display(),
            to.display()
        )
    );
    assert_eq!(e.kind(), io::ErrorKind::CrossesDevices);
    assert_eq!(seen.len(), 1, "{seen:?}");
    let (_, _, flags) = rename_names(&seen[0].0).unwrap();
    assert_eq!(flags & COPY_ALLOWED, 0);
    assert_eq!(fs::read(&from).unwrap(), b"bytes");
    assert!(!to.exists());
    // a replacement that meets it fails, and its target keeps its old bytes
    fs::write(&to, b"old bytes").unwrap();
    let (r, _) = with_backend(
        |s, b| {
            (rename_names(s).is_some() && b == Boundary::Before)
                .then(|| io::Error::from_raw_os_error(CROSS_DEVICE))
        },
        || fsutil::write_durable(&to, b"new bytes"),
    );
    assert_eq!(r.unwrap_err().kind(), io::ErrorKind::CrossesDevices);
    assert_eq!(fs::read(&to).unwrap(), b"old bytes");

    // a real move to another volume, where this machine has one: the OS refuses it, and nothing
    // is copied
    match other_volume(d) {
        Some(other) => {
            let there = tempfile::tempdir_in(&other).unwrap();
            let target = there.path().join("moved.txt");
            let e = pio::rename_durable(&from, &target).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::CrossesDevices, "{e}");
            assert!(
                e.to_string()
                    .ends_with(&format!("(os error {CROSS_DEVICE})")),
                "{e}"
            );
            assert_eq!(fs::read(&from).unwrap(), b"bytes");
            assert!(!target.exists());
        }
        None => eprintln!(
            "wx6_cross_volume_rename_does_not_copy: no second volume here; the real move is \
             not run (set RDY_TEST_OTHER_VOLUME to a writable directory on another volume)"
        ),
    }
}

/// A writable directory on another volume than `here`: `RDY_TEST_OTHER_VOLUME` when it is set,
/// else `/dev/shm` when it is on another device.
fn other_volume(here: &Path) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("RDY_TEST_OTHER_VOLUME") {
        return Some(PathBuf::from(dir));
    }
    let shm = Path::new("/dev/shm");
    let device = |p: &Path| {
        let f = fs::File::open(p).ok()?;
        let id = rdy_store::fileid::FileId::of_open(&f, p).ok()?.to_string();
        Some(id.split(':').next()?.to_string())
    };
    let (a, b) = (device(here)?, device(shm)?);
    (a != b && tempfile::tempdir_in(shm).is_ok()).then(|| shm.to_path_buf())
}
