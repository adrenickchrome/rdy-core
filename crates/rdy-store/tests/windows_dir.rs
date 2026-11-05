//! Calls in an open directory (`rdy_store::pio`): opens, entry kinds, listings, creates, removals,
//! renames and child processes anchored to a directory handle (FW-XP1 Phase 4). The pins run on
//! every platform: a directory moved or replaced after its handle was opened, or an entry swapped
//! for a link, never redirects a call, and every refusal fails closed. What a create makes is private from the
//! moment it exists. Names and native paths are bounded before anything is made of them.

use rdy_store::fileid::FileId;
use rdy_store::pio::{self, EntryKind};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::Path;
use std::sync::mpsc;

/// The names that `pio::for_each_entry` lists in the open directory `dir`, sorted.
fn listed(dir: &fs::File) -> Vec<OsString> {
    let mut names = vec![];
    pio::for_each_entry::<io::Error>(dir, |n| {
        names.push(n.to_os_string());
        Ok(())
    })
    .unwrap();
    names.sort();
    names
}

/// The identity of what `path` names (a link is not followed).
fn id(path: &Path) -> FileId {
    let f = pio::open_no_follow(fs::OpenOptions::new().read(true), path).unwrap();
    FileId::of_open(&f, path).unwrap()
}

/// The identity of an open file or directory.
fn id_of(f: &fs::File) -> FileId {
    FileId::of_open(f, Path::new("the handle")).unwrap()
}

/// A link at `link` to the directory `target`: a symbolic link on Unix, a junction (a mount-point
/// reparse point) on Windows, which needs no privilege.
fn link_dir(target: &Path, link: &Path) {
    #[cfg(unix)]
    pio::symlink(target, link).unwrap();
    #[cfg(windows)]
    run_mklink(&["/J".as_ref(), link.as_os_str(), target.as_os_str()]);
}

/// A symbolic link at `link` to the file `target`.
fn link_file(target: &Path, link: &Path) {
    #[cfg(unix)]
    pio::symlink(target, link).unwrap();
    #[cfg(windows)]
    run_mklink(&[link.as_os_str(), target.as_os_str()]);
}

/// A directory symbolic link at `link` to `target` (on Unix the same link as [`link_dir`]).
fn symlink_dir(target: &Path, link: &Path) {
    #[cfg(unix)]
    pio::symlink(target, link).unwrap();
    #[cfg(windows)]
    run_mklink(&["/D".as_ref(), link.as_os_str(), target.as_os_str()]);
}

#[cfg(windows)]
fn run_mklink(args: &[&OsStr]) {
    let out = std::process::Command::new("cmd")
        .args(["/C", "mklink"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "mklink {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Removes the link at `link` itself, never what it points to.
fn unlink(link: &Path) {
    #[cfg(unix)]
    fs::remove_file(link).unwrap();
    #[cfg(windows)]
    {
        // a directory link or a junction is removed as a directory, a file link as a file
        if fs::remove_dir(link).is_err() {
            fs::remove_file(link).unwrap();
        }
    }
}

#[test]
fn wx5a_list_and_open_share_the_directory_identity() {
    let root = tempfile::tempdir().unwrap();
    let p = root.path().join("p");
    fs::create_dir(&p).unwrap();
    fs::write(p.join("a"), b"a").unwrap();
    fs::write(p.join(".hidden"), b"h").unwrap();
    fs::create_dir(p.join("s")).unwrap();
    fs::write(p.join("s").join("inner"), b"inner").unwrap();
    let dir = pio::open_dir(&p).unwrap();
    let own = id_of(&dir);
    assert_eq!(own, id(&p));

    // the directory moves, and another one with other entries takes its path
    let moved = root.path().join("p-moved");
    fs::rename(&p, &moved).unwrap();
    fs::create_dir(&p).unwrap();
    fs::write(p.join("theirs"), b"theirs").unwrap();
    fs::create_dir(p.join("s")).unwrap();
    assert_eq!(id_of(&dir), own, "the handle names the same directory");
    assert_eq!(id(&moved), own);

    // the listing is of the handle, never of the path
    let names = listed(&dir);
    assert_eq!(names, [".hidden", "a", "s"]);
    // each listed entry opens, in the same directory, to the object listed there
    for name in &names {
        let at = moved.join(name);
        let want = id(&at);
        let kind = pio::entry_kind_at(&dir, name).unwrap();
        if name == "s" {
            assert_eq!(kind, EntryKind::Dir, "{name:?}");
            let sub = pio::open_dir_at(&dir, name).unwrap();
            assert_eq!(id_of(&sub), want, "{name:?}");
            assert_eq!(listed(&sub), ["inner"]);
            let inner = pio::open_read_at(&sub, OsStr::new("inner")).unwrap();
            assert_eq!(id_of(&inner), id(&at.join("inner")));
            assert!(!pio::is_empty_dir(&sub).unwrap());
            fs::remove_file(at.join("inner")).unwrap();
            assert!(pio::is_empty_dir(&sub).unwrap());
            assert!(listed(&sub).is_empty());
        } else {
            assert_eq!(kind, EntryKind::File, "{name:?}");
            let f = pio::open_read_at(&dir, name).unwrap();
            assert_eq!(id_of(&f), want, "{name:?}");
            assert!(f.metadata().unwrap().is_file());
        }
    }
    // a directory also opens for reading, and the caller judges its kind from the handle
    let as_read = pio::open_read_at(&dir, OsStr::new("s")).unwrap();
    assert!(as_read.metadata().unwrap().is_dir());
    assert_eq!(id_of(&as_read), id(&moved.join("s")));

    // each listing has its own position: a listing inside a listing of the same handle lists all
    let mut outer = vec![];
    pio::for_each_entry::<io::Error>(&dir, |n| {
        outer.push(n.to_os_string());
        assert_eq!(listed(&dir), names, "inside the listing of {n:?}");
        assert!(!pio::is_empty_dir(&dir)?);
        Ok(())
    })
    .unwrap();
    outer.sort();
    assert_eq!(outer, names);
    // and a listing again from the start finds the same entries
    assert_eq!(listed(&dir), names);

    // the first error of the callback ends the listing and is returned
    let mut seen = 0;
    let e = pio::for_each_entry(&dir, |_| {
        seen += 1;
        Err(io::Error::other("stop"))
    })
    .unwrap_err();
    assert_eq!((seen, e.to_string()), (1, "stop".to_string()));

    // many entries: every one is listed once, across the listing's buffers
    let many = root.path().join("many");
    fs::create_dir(&many).unwrap();
    let mut want: Vec<OsString> = (0..1500)
        .map(|i| OsString::from(format!("entry-{i:04}-{}", "x".repeat(i % 200))))
        .collect();
    for n in &want {
        fs::write(many.join(n), b"").unwrap();
    }
    want.sort();
    let many_dir = pio::open_dir(&many).unwrap();
    assert_eq!(listed(&many_dir), want);

    // a missing entry, and the path given to open_dir names something that is not a directory
    assert_eq!(
        pio::entry_kind_at(&dir, OsStr::new("gone"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        pio::open_dir_at(&dir, OsStr::new("gone"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    let e = pio::open_dir(&moved.join("a")).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::NotADirectory, "{e}");
    // open_dir follows links in its path: the caller trusts that path. On Windows the OS itself
    // may refuse to traverse a link that an ordinary account made (error 448, an untrusted mount
    // point, for junctions and directory symbolic links alike); that refusal is the OS's, and
    // nothing else is opened.
    let via = root.path().join("via");
    symlink_dir(&moved, &via);
    match pio::open_dir(&via) {
        Ok(d) => assert_eq!(id_of(&d), own),
        #[cfg(windows)]
        Err(e) if e.raw_os_error() == Some(448) => {}
        Err(e) => panic!("{e}"),
    }
}

/// The stages of [`wx5a_parent_swap_cannot_redirect_open`]. Two threads meet at a barrier of two
/// channels: the swapper makes a stage and says so, the opener checks it and says so. A thread
/// that fails drops its sender, so the other one stops instead of waiting for ever.
#[derive(Clone, Copy, Debug)]
enum Stage {
    /// The directory's path names another directory, holding other entries.
    ParentMoved,
    /// The directory's path is a link to another directory.
    ParentLinked,
    /// The entry `s` is a link (a junction on Windows) to another directory.
    EntryJunction,
    /// The entry `s` is a directory symbolic link.
    EntrySymlink,
    /// The entry `f` of `s` is a file symbolic link.
    FileLinked,
    /// Everything is back where it was.
    Restored,
}

#[test]
fn wx5a_parent_swap_cannot_redirect_open() {
    let root = tempfile::tempdir().unwrap();
    let p = root.path().join("p");
    fs::create_dir_all(p.join("s")).unwrap();
    fs::write(p.join("s").join("f"), b"mine").unwrap();
    let evil = tempfile::tempdir().unwrap();
    fs::create_dir_all(evil.path().join("s")).unwrap();
    fs::write(evil.path().join("s").join("f"), b"theirs").unwrap();
    let evil_file = evil.path().join("s").join("f");
    let evil_id = id(&evil_file);

    let dir = pio::open_dir(&p).unwrap();
    let s_id = id(&p.join("s"));
    let f_id = id(&p.join("s").join("f"));
    let old = root.path().join("p-old");

    let stages = [
        Stage::ParentMoved,
        Stage::ParentLinked,
        Stage::EntryJunction,
        Stage::EntrySymlink,
        Stage::FileLinked,
        Stage::Restored,
    ];
    let rounds = 8;
    let (made_tx, made) = mpsc::channel::<()>();
    let (checked, checked_rx) = mpsc::channel::<()>();
    let swapper = {
        let (p, old, evil) = (p.clone(), old.clone(), evil.path().to_path_buf());
        std::thread::spawn(move || {
            for _ in 0..rounds {
                for stage in stages {
                    // the real directory is at `old` from the first stage to the last
                    let real = |rel: &str| old.join(rel);
                    match stage {
                        Stage::ParentMoved => {
                            fs::rename(&p, &old).unwrap();
                            fs::create_dir_all(p.join("s")).unwrap();
                            fs::write(p.join("s").join("f"), b"theirs").unwrap();
                        }
                        Stage::ParentLinked => {
                            fs::remove_dir_all(&p).unwrap();
                            link_dir(&evil, &p);
                        }
                        Stage::EntryJunction => {
                            fs::rename(real("s"), real("s-aside")).unwrap();
                            link_dir(&evil.join("s"), &real("s"));
                        }
                        Stage::EntrySymlink => {
                            unlink(&real("s"));
                            symlink_dir(&evil.join("s"), &real("s"));
                        }
                        Stage::FileLinked => {
                            unlink(&real("s"));
                            fs::rename(real("s-aside"), real("s")).unwrap();
                            fs::rename(real("s").join("f"), real("s").join("f-aside")).unwrap();
                            link_file(&evil.join("s").join("f"), &real("s").join("f"));
                        }
                        Stage::Restored => {
                            unlink(&real("s").join("f"));
                            fs::rename(real("s").join("f-aside"), real("s").join("f")).unwrap();
                            unlink(&p);
                            fs::rename(&old, &p).unwrap();
                        }
                    }
                    made_tx.send(()).unwrap(); // the stage is made
                    if checked_rx.recv().is_err() {
                        return; // the opener failed
                    }
                }
            }
        })
    };

    for round in 0..rounds {
        for stage in stages {
            if made.recv().is_err() {
                // the swapper failed: its panic is the cause
                let cause = swapper.join().unwrap_err();
                std::panic::resume_unwind(cause);
            }
            let what = format!("round {round}, {stage:?}");
            match stage {
                Stage::ParentMoved | Stage::ParentLinked | Stage::Restored => {
                    // every call through the handle reaches the directory it opened
                    assert_eq!(listed(&dir), ["s"], "{what}");
                    assert_eq!(
                        pio::entry_kind_at(&dir, OsStr::new("s")).unwrap(),
                        EntryKind::Dir,
                        "{what}"
                    );
                    let s = pio::open_dir_at(&dir, OsStr::new("s")).unwrap();
                    assert_eq!(id_of(&s), s_id, "{what}");
                    assert_eq!(listed(&s), ["f"], "{what}");
                    let f = pio::open_read_at(&s, OsStr::new("f")).unwrap();
                    assert_eq!(id_of(&f), f_id, "{what}");
                    let mut text = String::new();
                    io::Read::read_to_string(&mut &f, &mut text).unwrap();
                    assert_eq!(text, "mine", "{what}");
                }
                Stage::EntryJunction | Stage::EntrySymlink => {
                    // the link at the entry is refused, never followed, by every call
                    assert_eq!(
                        pio::entry_kind_at(&dir, OsStr::new("s")).unwrap(),
                        EntryKind::Link,
                        "{what}"
                    );
                    let e = pio::open_dir_at(&dir, OsStr::new("s")).unwrap_err();
                    assert!(pio::is_link_refusal(&e), "{what}: {e}");
                    let e = pio::open_read_at(&dir, OsStr::new("s")).unwrap_err();
                    assert!(pio::is_link_refusal(&e), "{what}: {e}");
                    #[cfg(windows)]
                    assert!(
                        e.to_string()
                            .ends_with("s is a reparse point; rdy will not follow it"),
                        "{what}: {e}"
                    );
                    assert_eq!(listed(&dir), ["s", "s-aside"], "{what}");
                }
                Stage::FileLinked => {
                    let s = pio::open_dir_at(&dir, OsStr::new("s")).unwrap();
                    assert_eq!(id_of(&s), s_id, "{what}");
                    assert_eq!(
                        pio::entry_kind_at(&s, OsStr::new("f")).unwrap(),
                        EntryKind::Link,
                        "{what}"
                    );
                    let e = pio::open_read_at(&s, OsStr::new("f")).unwrap_err();
                    assert!(pio::is_link_refusal(&e), "{what}: {e}");
                    // a file where a directory is opened is a link refusal too
                    let e = pio::open_dir_at(&s, OsStr::new("f-aside")).unwrap_err();
                    assert!(pio::is_link_refusal(&e), "{what}: {e}");
                    assert_eq!(e.kind(), io::ErrorKind::NotADirectory, "{what}: {e}");
                }
            }
            checked.send(()).unwrap();
        }
    }
    swapper.join().unwrap();
    // nothing was changed through a link
    assert_eq!(fs::read(&evil_file).unwrap(), b"theirs");
    assert_eq!(id(&evil_file), evil_id);
    assert_eq!(fs::read(p.join("s").join("f")).unwrap(), b"mine");
    assert_eq!(id_of(&dir), id(&p));
}

#[test]
fn wx5a_native_path_bounds_precede_allocation() {
    let root = tempfile::tempdir().unwrap();
    let dir = pio::open_dir(root.path()).unwrap();
    fs::write(root.path().join("a"), b"a").unwrap();

    // one entry name only, on every platform
    for bad in ["", ".", "..", "a/b"] {
        for e in [
            pio::open_read_at(&dir, OsStr::new(bad)).unwrap_err(),
            pio::open_dir_at(&dir, OsStr::new(bad)).unwrap_err(),
            pio::entry_kind_at(&dir, OsStr::new(bad)).unwrap_err(),
        ] {
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{bad:?}: {e}");
            assert!(!pio::is_link_refusal(&e), "{bad:?}");
        }
    }
    // a NUL in a name is refused, and reaches no entry
    let nul = OsStr::new("a\0b");
    for e in [
        pio::open_read_at(&dir, nul).unwrap_err(),
        pio::open_dir_at(&dir, nul).unwrap_err(),
        pio::entry_kind_at(&dir, nul).unwrap_err(),
    ] {
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{e}");
        #[cfg(windows)]
        assert!(
            e.to_string().starts_with("the native path for ")
                && e.to_string().ends_with(" contains a NUL"),
            "{e}"
        );
    }
    let e = pio::open_dir(&root.path().join("a\0b")).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{e}");
    #[cfg(windows)]
    assert_eq!(
        e.to_string(),
        format!(
            "the native path for {} contains a NUL",
            root.path().join("a\0b").display()
        )
    );

    // a name or a path longer than any native path is refused before it is made
    let long = "n".repeat(40_000);
    for e in [
        pio::open_read_at(&dir, OsStr::new(&long)).unwrap_err(),
        pio::open_dir_at(&dir, OsStr::new(&long)).unwrap_err(),
        pio::entry_kind_at(&dir, OsStr::new(&long)).unwrap_err(),
        pio::open_dir(&root.path().join(&long)).unwrap_err(),
    ] {
        let shown: String = e.to_string().chars().take(80).collect();
        assert!(!pio::is_link_refusal(&e), "{shown}");
        assert_ne!(e.kind(), io::ErrorKind::NotFound);
        #[cfg(windows)]
        {
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
            assert!(
                e.to_string().ends_with(" exceeds 32767 UTF-16 code units"),
                "{shown}"
            );
        }
    }

    // the longest name the file system takes is listed and opened whole
    let max = "m".repeat(255);
    fs::write(root.path().join(&max), b"max").unwrap();
    assert_eq!(listed(&dir), [OsString::from("a"), OsString::from(&max)]);
    let f = pio::open_read_at(&dir, OsStr::new(&max)).unwrap();
    assert_eq!(id_of(&f), id(&root.path().join(&max)));

    #[cfg(windows)]
    {
        // a stream name or a Windows separator is not one entry name
        for bad in ["a:stream", "a\\b", "a:"] {
            let e = pio::open_read_at(&dir, OsStr::new(bad)).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{bad:?}: {e}");
        }
        assert!(!root.path().join("a:stream").exists());
        // a directory far deeper than 260 units opens and lists through handles alone
        let mut at = pio::open_dir(root.path()).unwrap();
        let part = "d".repeat(200);
        let mut path = root.path().to_path_buf();
        for _ in 0..4 {
            path.push(&part);
            fs::create_dir(&path).unwrap();
            at = pio::open_dir_at(&at, OsStr::new(&part)).unwrap();
        }
        fs::write(path.join("leaf"), b"leaf").unwrap();
        assert_eq!(listed(&at), ["leaf"]);
        assert_eq!(id_of(&pio::open_dir(&path).unwrap()), id_of(&at));
    }
}

/// Lets every account write the directory `path`, remove or replace its entries, and hands that
/// access to what is made in it: mode 0o777 on Unix; on Windows inheritable full access for
/// Everyone and for Users.
fn widen(path: &Path) {
    #[cfg(unix)]
    fs::set_permissions(path, pio::permissions(0o777)).unwrap();
    #[cfg(windows)]
    for sid in ["*S-1-1-0", "*S-1-5-32-545"] {
        let out = std::process::Command::new("icacls")
            .arg(path)
            .args(["/grant", &format!("{sid}:(OI)(CI)(F)")])
            .output()
            .unwrap();
        assert!(out.status.success(), "icacls: {out:?}");
    }
}

/// `f` is private to this account: on Unix mode 0o600 or 0o700; on Windows its owner is this
/// account and its protected DACL holds the account's one ACE.
fn assert_private(f: &fs::File, what: &str) {
    let p = pio::privacy(f).unwrap();
    assert!(p.mine && p.private && p.owner_only, "{what}: {p:?}");
    assert!(!p.shared_writable && !p.open_to_all, "{what}: {p:?}");
    #[cfg(windows)]
    {
        let kind = if f.metadata().unwrap().is_dir() {
            pio::ObjectKind::Dir
        } else {
            pio::ObjectKind::File
        };
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
}

/// The stages of [`wx5b_parent_swap_cannot_redirect_a_write`], met at the same two-channel
/// barrier as [`Stage`].
#[derive(Clone, Copy, Debug)]
enum WriteStage {
    /// The directory's path names another directory, holding the decoys `w` and `d`.
    ParentMoved,
    /// The directory's path is a link to another directory, holding the decoys `w` and `d`.
    ParentLinked,
    /// In the directory itself, `w` is a file link and `d` a directory link to the decoys.
    EntriesLinked,
    /// Everything is back where it was.
    Restored,
}

#[test]
fn wx5b_parent_swap_cannot_redirect_a_write() {
    let root = tempfile::tempdir().unwrap();
    let p = root.path().join("p");
    fs::create_dir(&p).unwrap();
    let evil = tempfile::tempdir().unwrap();
    fs::write(evil.path().join("w"), b"theirs").unwrap();
    fs::create_dir(evil.path().join("d")).unwrap();
    fs::write(evil.path().join("d").join("inside"), b"theirs").unwrap();
    let (evil_w, evil_d) = (id(&evil.path().join("w")), id(&evil.path().join("d")));
    let dir = pio::open_dir(&p).unwrap();
    let own = id_of(&dir);
    let old = root.path().join("p-old");

    let stages = [
        WriteStage::ParentMoved,
        WriteStage::ParentLinked,
        WriteStage::EntriesLinked,
        WriteStage::Restored,
    ];
    let rounds = 8;
    let (made_tx, made) = mpsc::channel::<()>();
    let (checked, checked_rx) = mpsc::channel::<()>();
    let swapper = {
        let (p, old, evil) = (p.clone(), old.clone(), evil.path().to_path_buf());
        std::thread::spawn(move || {
            for _ in 0..rounds {
                for stage in stages {
                    match stage {
                        WriteStage::ParentMoved => {
                            fs::rename(&p, &old).unwrap();
                            fs::create_dir(&p).unwrap();
                            fs::write(p.join("w"), b"decoy").unwrap();
                            fs::create_dir(p.join("d")).unwrap();
                        }
                        WriteStage::ParentLinked => {
                            fs::remove_dir_all(&p).unwrap();
                            link_dir(&evil, &p);
                        }
                        WriteStage::EntriesLinked => {
                            link_file(&evil.join("w"), &old.join("w"));
                            link_dir(&evil.join("d"), &old.join("d"));
                        }
                        WriteStage::Restored => {
                            unlink(&old.join("w"));
                            unlink(&old.join("d"));
                            unlink(&p);
                            fs::rename(&old, &p).unwrap();
                        }
                    }
                    made_tx.send(()).unwrap(); // the stage is made
                    if checked_rx.recv().is_err() {
                        return; // the writer failed
                    }
                }
            }
        })
    };

    for round in 0..rounds {
        for stage in stages {
            if made.recv().is_err() {
                // the swapper failed: its panic is the cause
                let cause = swapper.join().unwrap_err();
                std::panic::resume_unwind(cause);
            }
            let what = format!("round {round}, {stage:?}");
            // the directory the handle opened, wherever it is now
            let real = match stage {
                WriteStage::Restored => p.clone(),
                _ => old.clone(),
            };
            match stage {
                WriteStage::ParentMoved | WriteStage::ParentLinked | WriteStage::Restored => {
                    // each create and removal reaches the directory the handle opened, never
                    // what its path names now
                    let mut f = pio::create_private_at(&dir, OsStr::new("w")).unwrap();
                    io::Write::write_all(&mut f, b"mine").unwrap();
                    drop(f);
                    assert_eq!(fs::read(real.join("w")).unwrap(), b"mine", "{what}");
                    pio::create_private_dir_at(&dir, OsStr::new("d")).unwrap();
                    let d = pio::open_dir_at(&dir, OsStr::new("d")).unwrap();
                    assert_eq!(id_of(&d), id(&real.join("d")), "{what}");
                    assert!(pio::is_empty_dir(&d).unwrap(), "{what}");
                    drop(d);
                    assert_eq!(listed(&dir), ["d", "w"], "{what}");
                    pio::remove_file_at(&dir, OsStr::new("w")).unwrap();
                    pio::remove_dir_at(&dir, OsStr::new("d")).unwrap();
                    assert!(listed(&dir).is_empty(), "{what}");
                    assert!(
                        pio::umask_keeps_0700_at(&dir, OsStr::new("w")).unwrap(),
                        "{what}"
                    );
                    assert!(listed(&dir).is_empty(), "{what}");
                }
                WriteStage::EntriesLinked => {
                    // a link at the entry is never written through: each create refuses it
                    for e in [
                        pio::create_private_at(&dir, OsStr::new("w")).unwrap_err(),
                        pio::create_private_dir_at(&dir, OsStr::new("w")).unwrap_err(),
                        pio::create_private_at(&dir, OsStr::new("d")).unwrap_err(),
                        pio::create_private_dir_at(&dir, OsStr::new("d")).unwrap_err(),
                        pio::umask_keeps_0700_at(&dir, OsStr::new("w")).unwrap_err(),
                        pio::umask_keeps_0700_at(&dir, OsStr::new("d")).unwrap_err(),
                    ] {
                        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{what}: {e}");
                    }
                    assert_eq!(listed(&dir), ["d", "w"], "{what}");
                }
            }
            // the decoys and the other directory are as they were
            assert_eq!(
                fs::read(evil.path().join("w")).unwrap(),
                b"theirs",
                "{what}"
            );
            assert_eq!(id(&evil.path().join("w")), evil_w, "{what}");
            assert_eq!(id(&evil.path().join("d")), evil_d, "{what}");
            assert_eq!(
                listed(&pio::open_dir(&evil.path().join("d")).unwrap()),
                ["inside"]
            );
            if let WriteStage::ParentMoved = stage {
                assert_eq!(fs::read(p.join("w")).unwrap(), b"decoy", "{what}");
                assert!(p.join("d").is_dir(), "{what}");
            }
            checked.send(()).unwrap();
        }
    }
    swapper.join().unwrap();
    assert!(listed(&dir).is_empty());
    assert_eq!(id(&p), own);
    assert_eq!(listed(&pio::open_dir(evil.path()).unwrap()), ["d", "w"]);
}

#[test]
fn wx5b_reparse_swap_cannot_redirect_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let p = root.path().join("p");
    fs::create_dir(&p).unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let victim = elsewhere.path().join("victim");
    let vdir = elsewhere.path().join("vdir");
    fs::write(&victim, b"keep").unwrap();
    fs::create_dir(&vdir).unwrap();
    fs::write(vdir.join("inside"), b"keep").unwrap();
    let (victim_id, vdir_id) = (id(&victim), id(&vdir));
    let dir = pio::open_dir(&p).unwrap();
    let intact = |what: &str| {
        assert_eq!(fs::read(&victim).unwrap(), b"keep", "{what}");
        assert_eq!(id(&victim), victim_id, "{what}");
        assert_eq!(id(&vdir), vdir_id, "{what}");
        assert_eq!(fs::read(vdir.join("inside")).unwrap(), b"keep", "{what}");
    };

    // a temp file is made; then its entry is swapped for a link: the cleanup removes the link
    // itself, never what it points to, and our file stays where it was moved
    for target in [&victim, &vdir] {
        let what = format!("tmp swapped for a link to {}", target.display());
        let f = pio::create_private_at(&dir, OsStr::new("tmp")).unwrap();
        fs::rename(p.join("tmp"), p.join("tmp-aside")).unwrap();
        if target.is_dir() {
            link_dir(target, &p.join("tmp"));
        } else {
            link_file(target, &p.join("tmp"));
        }
        assert_eq!(
            pio::entry_kind_at(&dir, OsStr::new("tmp")).unwrap(),
            EntryKind::Link,
            "{what}"
        );
        pio::remove_file_at(&dir, OsStr::new("tmp")).unwrap();
        intact(&what);
        assert_eq!(listed(&dir), ["tmp-aside"], "{what}");
        assert_eq!(id(&p.join("tmp-aside")), id_of(&f), "{what}");
        drop(f);
        pio::remove_file_at(&dir, OsStr::new("tmp-aside")).unwrap();
        assert!(listed(&dir).is_empty(), "{what}");
    }

    // a directory link where a directory is removed is refused, and nothing is removed
    for (name, make) in [
        ("dlink", link_dir as fn(&Path, &Path)),
        ("dsym", symlink_dir as fn(&Path, &Path)),
    ] {
        make(&vdir, &p.join(name));
        let e = pio::remove_dir_at(&dir, OsStr::new(name)).unwrap_err();
        assert!(pio::is_link_refusal(&e), "{name}: {e}");
        intact(name);
        assert_eq!(
            pio::entry_kind_at(&dir, OsStr::new(name)).unwrap(),
            EntryKind::Link,
            "{name}"
        );
        // the link itself is removed as an entry
        pio::remove_file_at(&dir, OsStr::new(name)).unwrap();
        intact(name);
    }
    // a file link where a directory is removed is refused too
    link_file(&victim, &p.join("flink"));
    let e = pio::remove_dir_at(&dir, OsStr::new("flink")).unwrap_err();
    assert!(pio::is_link_refusal(&e), "{e}");
    pio::remove_file_at(&dir, OsStr::new("flink")).unwrap();
    intact("flink");

    // the directory's path is swapped for a link to the victim's directory: a cleanup through
    // the handle removes only the entries of the directory it opened
    fs::write(p.join("inside"), b"mine").unwrap();
    fs::create_dir(p.join("sub")).unwrap();
    let moved = root.path().join("p-moved");
    fs::rename(&p, &moved).unwrap();
    link_dir(elsewhere.path(), &p);
    fs::write(vdir.join("sub-decoy"), b"keep").unwrap();
    pio::remove_file_at(&dir, OsStr::new("inside")).unwrap();
    pio::remove_dir_at(&dir, OsStr::new("sub")).unwrap();
    assert!(listed(&dir).is_empty());
    assert!(listed(&pio::open_dir(&moved).unwrap()).is_empty());
    intact("parent linked");
    assert_eq!(fs::read(vdir.join("sub-decoy")).unwrap(), b"keep");

    // removals keep their kinds: a directory is not removed as a file, a file not as a
    // directory, a directory that is not empty stays, and a missing entry is NotFound
    fs::create_dir(moved.join("full")).unwrap();
    fs::write(moved.join("full").join("x"), b"x").unwrap();
    fs::write(moved.join("file"), b"f").unwrap();
    assert!(pio::remove_file_at(&dir, OsStr::new("full")).is_err());
    let e = pio::remove_dir_at(&dir, OsStr::new("full")).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::DirectoryNotEmpty, "{e}");
    let e = pio::remove_dir_at(&dir, OsStr::new("file")).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::NotADirectory, "{e}");
    assert_eq!(listed(&dir), ["file", "full"]);
    for e in [
        pio::remove_file_at(&dir, OsStr::new("gone")).unwrap_err(),
        pio::remove_dir_at(&dir, OsStr::new("gone")).unwrap_err(),
    ] {
        assert_eq!(e.kind(), io::ErrorKind::NotFound, "{e}");
    }
    // a removal while the file is still open, then a create at the same name
    let open = pio::open_read_at(&dir, OsStr::new("file")).unwrap();
    pio::remove_file_at(&dir, OsStr::new("file")).unwrap();
    assert_eq!(listed(&dir), ["full"]);
    drop(pio::create_private_at(&dir, OsStr::new("file")).unwrap());
    drop(open);
    // a file whose owner may only read it is removed, as unlink does on Unix
    let mut ro = fs::metadata(moved.join("file")).unwrap().permissions();
    ro.set_readonly(true);
    fs::set_permissions(moved.join("file"), ro).unwrap();
    pio::remove_file_at(&dir, OsStr::new("file")).unwrap();
    assert_eq!(listed(&dir), ["full"]);

    // names are checked as for the opens, before anything is made of them
    let long = "n".repeat(40_000);
    for bad in ["", ".", "..", "a/b", "a\0b", long.as_str()] {
        for e in [
            pio::create_private_at(&dir, OsStr::new(bad)).unwrap_err(),
            pio::create_private_dir_at(&dir, OsStr::new(bad)).unwrap_err(),
            pio::umask_keeps_0700_at(&dir, OsStr::new(bad)).unwrap_err(),
            pio::remove_file_at(&dir, OsStr::new(bad)).unwrap_err(),
            pio::remove_dir_at(&dir, OsStr::new(bad)).unwrap_err(),
        ] {
            let shown: String = bad.chars().take(8).collect();
            assert_ne!(e.kind(), io::ErrorKind::NotFound, "{shown:?}: {e}");
            assert!(!pio::is_link_refusal(&e), "{shown:?}");
            #[cfg(windows)]
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{shown:?}");
        }
    }
    assert_eq!(listed(&dir), ["full"]);
    intact("names");
}

#[test]
fn wx5b_private_create_has_no_public_window() {
    // a directory that hands every account access to what is made in it
    let root = tempfile::tempdir().unwrap();
    let open = root.path().join("open");
    fs::create_dir(&open).unwrap();
    widen(&open);
    let dir = pio::open_dir(&open).unwrap();
    let p = pio::privacy(&dir).unwrap();
    assert!(p.shared_writable && p.open_to_all, "{p:?}");

    // a new file is private before any byte is written and before make_private_file: it takes
    // none of the directory's access
    let mut f = pio::create_private_at(&dir, OsStr::new("f")).unwrap();
    assert_private(&f, "a new file");
    assert_eq!(f.metadata().unwrap().len(), 0);
    io::Write::write_all(&mut f, b"secret").unwrap();
    f.sync_all().unwrap();
    pio::make_private_file(&f).unwrap();
    assert_private(&f, "the file after make_private_file");
    let again = pio::open_read_at(&dir, OsStr::new("f")).unwrap();
    assert_private(&again, "the file opened again");
    assert_eq!(id_of(&again), id_of(&f));

    // a new directory is private from the moment it exists, and so is what is made in it
    pio::create_private_dir_at(&dir, OsStr::new("d")).unwrap();
    let d = pio::open_dir_at(&dir, OsStr::new("d")).unwrap();
    assert_private(&d, "a new directory");
    assert!(pio::is_empty_dir(&d).unwrap());
    let inner = pio::create_private_at(&d, OsStr::new("inner")).unwrap();
    assert_private(&inner, "a file in the new directory");
    pio::create_private_dir_at(&d, OsStr::new("sub")).unwrap();
    assert_private(
        &pio::open_dir_at(&d, OsStr::new("sub")).unwrap(),
        "a directory in the new directory",
    );

    // the probe makes the private file it asks about and removes it again
    assert!(pio::umask_keeps_0700_at(&dir, OsStr::new(".probe")).unwrap());
    assert_eq!(listed(&dir), ["d", "f"]);

    // an existing entry is never opened, widened or changed by a create
    for name in ["f", "d"] {
        for e in [
            pio::create_private_at(&dir, OsStr::new(name)).unwrap_err(),
            pio::create_private_dir_at(&dir, OsStr::new(name)).unwrap_err(),
            pio::umask_keeps_0700_at(&dir, OsStr::new(name)).unwrap_err(),
        ] {
            assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{name}: {e}");
        }
    }
    fs::write(open.join("theirs"), b"theirs").unwrap();
    let e = pio::create_private_at(&dir, OsStr::new("theirs")).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{e}");
    assert_eq!(fs::read(open.join("theirs")).unwrap(), b"theirs");
    let p = pio::privacy(&pio::open_read_at(&dir, OsStr::new("theirs")).unwrap()).unwrap();
    assert!(!p.private, "an existing file keeps its access: {p:?}");
    assert_eq!(fs::read(open.join("f")).unwrap(), b"secret");
    assert_private(
        &pio::open_read_at(&dir, OsStr::new("f")).unwrap(),
        "f at the end",
    );
}

/// The contents of the file `name` in the open directory `dir`.
fn read_at(dir: &fs::File, name: &str) -> String {
    let f = pio::open_read_at(dir, OsStr::new(name)).unwrap();
    let mut text = String::new();
    io::Read::read_to_string(&mut &f, &mut text).unwrap();
    text
}

#[test]
fn wx5c_rename_pins_both_parents() {
    let root = tempfile::tempdir().unwrap();
    let p = root.path().join("p");
    fs::create_dir(&p).unwrap();
    fs::write(p.join("a"), b"mine").unwrap();
    fs::write(p.join("t"), b"old target").unwrap();
    fs::create_dir(p.join("s")).unwrap();
    fs::write(p.join("s").join("inner"), b"inner").unwrap();
    let evil = tempfile::tempdir().unwrap();
    for name in ["a", "b", "t"] {
        fs::write(evil.path().join(name), b"theirs").unwrap();
    }
    fs::create_dir(evil.path().join("sub")).unwrap();
    fs::write(evil.path().join("sub").join("x"), b"theirs").unwrap();
    let evil_ids: Vec<FileId> = ["a", "b", "t", "sub"]
        .iter()
        .map(|n| id(&evil.path().join(n)))
        .collect();
    let intact = |what: &str| {
        for (n, want) in ["a", "b", "t", "sub"].iter().zip(&evil_ids) {
            assert_eq!(id(&evil.path().join(n)), *want, "{what}: {n}");
        }
        for n in ["a", "b", "t"] {
            assert_eq!(fs::read(evil.path().join(n)).unwrap(), b"theirs", "{what}");
        }
        assert_eq!(
            listed(&pio::open_dir(&evil.path().join("sub")).unwrap()),
            ["x"]
        );
    };
    let dir = pio::open_dir(&p).unwrap();
    let own = id_of(&dir);
    let a_id = id(&p.join("a"));
    let s_id = id(&p.join("s"));

    // the directory's path names another directory: both names of a rename are entries of the
    // directory the handle opened, never of the one at its path now
    let real = root.path().join("p-moved");
    fs::rename(&p, &real).unwrap();
    fs::create_dir(&p).unwrap();
    fs::write(p.join("a"), b"decoy").unwrap();
    fs::write(p.join("t"), b"decoy").unwrap();
    pio::rename_at(&dir, OsStr::new("a"), OsStr::new("b")).unwrap();
    assert_eq!(listed(&dir), ["b", "s", "t"]);
    assert_eq!(read_at(&dir, "b"), "mine");
    assert_eq!(
        id(&real.join("b")),
        a_id,
        "a rename moves the entry, never a copy"
    );
    assert_eq!(listed(&pio::open_dir(&p).unwrap()), ["a", "t"]);
    assert_eq!(fs::read(p.join("a")).unwrap(), b"decoy");

    // the path is a link to another directory: a rename that replaces an entry replaces the
    // entry of the directory the handle opened
    fs::remove_dir_all(&p).unwrap();
    link_dir(evil.path(), &p);
    pio::rename_at(&dir, OsStr::new("b"), OsStr::new("t")).unwrap();
    assert_eq!(listed(&dir), ["s", "t"]);
    assert_eq!(read_at(&dir, "t"), "mine");
    assert_eq!(id(&real.join("t")), a_id);
    intact("the path linked");
    // a directory is renamed with what it holds
    pio::rename_at(&dir, OsStr::new("s"), OsStr::new("s2")).unwrap();
    let s2 = pio::open_dir_at(&dir, OsStr::new("s2")).unwrap();
    assert_eq!(id_of(&s2), s_id);
    assert_eq!(listed(&s2), ["inner"]);
    drop(s2);
    // a case-only rename keeps the entry
    pio::rename_at(&dir, OsStr::new("s2"), OsStr::new("S2")).unwrap();
    assert_eq!(listed(&dir), ["S2", "t"]);
    assert_eq!(id(&real.join("S2")), s_id);
    pio::rename_at(&dir, OsStr::new("S2"), OsStr::new("s")).unwrap();

    // a link at the old name is renamed as itself, never followed
    link_file(&evil.path().join("a"), &real.join("lf"));
    link_dir(&evil.path().join("sub"), &real.join("ld"));
    pio::rename_at(&dir, OsStr::new("lf"), OsStr::new("lf2")).unwrap();
    pio::rename_at(&dir, OsStr::new("ld"), OsStr::new("ld2")).unwrap();
    for name in ["lf2", "ld2"] {
        assert_eq!(
            pio::entry_kind_at(&dir, OsStr::new(name)).unwrap(),
            EntryKind::Link,
            "{name}"
        );
    }
    intact("links renamed");
    // a file link at the new name is replaced itself, never written through
    pio::rename_at(&dir, OsStr::new("t"), OsStr::new("lf2")).unwrap();
    assert_eq!(
        pio::entry_kind_at(&dir, OsStr::new("lf2")).unwrap(),
        EntryKind::File
    );
    assert_eq!(read_at(&dir, "lf2"), "mine");
    intact("a file link replaced");
    // a directory link at the new name: Unix replaces the link itself; Windows refuses to
    // replace a directory entry; neither reaches the directory it names
    fs::write(real.join("c"), b"c").unwrap();
    let replaced = pio::rename_at(&dir, OsStr::new("c"), OsStr::new("ld2"));
    #[cfg(unix)]
    {
        replaced.unwrap();
        assert_eq!(
            pio::entry_kind_at(&dir, OsStr::new("ld2")).unwrap(),
            EntryKind::File
        );
    }
    #[cfg(windows)]
    {
        let e = replaced.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
        assert_eq!(
            pio::entry_kind_at(&dir, OsStr::new("ld2")).unwrap(),
            EntryKind::Link
        );
        unlink(&real.join("ld2"));
        pio::rename_at(&dir, OsStr::new("c"), OsStr::new("ld2")).unwrap();
    }
    intact("a directory link at the new name");

    // a missing entry is NotFound, and nothing is made at either name
    let e = pio::rename_at(&dir, OsStr::new("gone"), OsStr::new("new")).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::NotFound, "{e}");
    // both names are checked as for the opens, before anything is made of them
    let long = "n".repeat(40_000);
    for bad in ["", ".", "..", "a/b", "a\0b", long.as_str()] {
        for e in [
            pio::rename_at(&dir, OsStr::new(bad), OsStr::new("new")).unwrap_err(),
            pio::rename_at(&dir, OsStr::new("lf2"), OsStr::new(bad)).unwrap_err(),
        ] {
            let shown: String = bad.chars().take(8).collect();
            assert_ne!(e.kind(), io::ErrorKind::NotFound, "{shown:?}: {e}");
            #[cfg(windows)]
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{shown:?}: {e}");
        }
    }
    #[cfg(windows)]
    {
        let e = pio::rename_at(&dir, OsStr::new("lf2"), OsStr::new("a\0b")).unwrap_err();
        assert!(e.to_string().ends_with("contains a NUL"), "{e}");
        let e = pio::rename_at(&dir, OsStr::new(&long), OsStr::new("new")).unwrap_err();
        assert!(
            e.to_string().ends_with("exceeds 32767 UTF-16 code units"),
            "{e}"
        );
    }
    assert_eq!(listed(&dir), ["ld2", "lf2", "s"]);
    assert_eq!(id_of(&dir), own);
    intact("the end");
}

#[test]
fn wx5c_start_in_dir_uses_the_verified_directory() {
    let root = tempfile::tempdir().unwrap();
    let p = root.path().join("p");
    fs::create_dir(&p).unwrap();
    fs::write(p.join("marker"), b"mine").unwrap();
    let real = root.path().join("p-moved");
    // a program in the directory, started by a relative name
    #[cfg(unix)]
    let (program, script) = ("./show", "#!/bin/sh\ncat marker\n");
    #[cfg(windows)]
    let (program, script) = (".\\show.cmd", "@type marker\r\n");
    let program_file = Path::new(program).file_name().unwrap().to_owned();
    fs::write(p.join(&program_file), script).unwrap();
    #[cfg(unix)]
    fs::set_permissions(p.join(&program_file), pio::permissions(0o755)).unwrap();
    // the decoy that takes the directory's path, with its own marker and program
    let decoy = |at: &Path| {
        fs::create_dir(at).unwrap();
        fs::write(at.join("marker"), b"theirs").unwrap();
        // the decoy's program shows its own marker: a start in the decoy shows "theirs"
        fs::write(at.join(&program_file), script).unwrap();
        #[cfg(unix)]
        fs::set_permissions(at.join(&program_file), pio::permissions(0o755)).unwrap();
    };
    let shown = |cmd: &mut std::process::Command| {
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };

    // the directory moves before the call, and another one takes its path
    let dir = pio::open_dir(&p).unwrap();
    fs::rename(&p, &real).unwrap();
    decoy(&p);
    let mut cmd = std::process::Command::new(program);
    pio::start_in_dir(&mut cmd, &dir).unwrap();
    assert_eq!(shown(&mut cmd), "mine");

    // the path changes after the call and before the start: the child still starts in the
    // directory the handle opened, and the caller may close its handle first
    fs::remove_dir_all(&p).unwrap();
    fs::rename(&real, &p).unwrap();
    let mut cmd = std::process::Command::new(program);
    pio::start_in_dir(&mut cmd, &dir).unwrap();
    drop(dir);
    fs::rename(&p, &real).unwrap();
    decoy(&p);
    assert_eq!(shown(&mut cmd), "mine");
    // the path is a link to the decoy when the child starts
    let decoy_dir = root.path().join("decoy");
    fs::rename(&p, &decoy_dir).unwrap();
    let dir = pio::open_dir(&real).unwrap();
    let mut cmd = std::process::Command::new(program);
    pio::start_in_dir(&mut cmd, &dir).unwrap();
    fs::rename(&real, &p).unwrap();
    link_dir(&decoy_dir, &real);
    assert_eq!(shown(&mut cmd), "mine");
    // the same command starts again in the same directory
    assert_eq!(shown(&mut cmd), "mine");
    assert_eq!(fs::read(decoy_dir.join("marker")).unwrap(), b"theirs");
}

/// The stages of [`wx5c_adversarial_swaps_preserve_every_boundary`], met at the same two-channel
/// barrier as [`Stage`].
#[derive(Clone, Copy, Debug)]
enum SwapStage {
    /// The directory's path names another directory, holding the decoys `s`, `f`, `w` and `d`.
    ParentMoved,
    /// The directory's path is a link to another directory, holding the same decoys.
    ParentLinked,
    /// In the directory itself, `s` is a directory link and `f` a file link to the decoys.
    EntriesLinked,
    /// Everything is back where it was.
    Restored,
}

#[test]
fn wx5c_adversarial_swaps_preserve_every_boundary() {
    let root = tempfile::tempdir().unwrap();
    let p = root.path().join("p");
    fs::create_dir_all(p.join("s")).unwrap();
    fs::write(p.join("s").join("inner"), b"mine").unwrap();
    fs::write(p.join("f"), b"mine").unwrap();
    let evil = tempfile::tempdir().unwrap();
    fs::create_dir(evil.path().join("s")).unwrap();
    fs::write(evil.path().join("s").join("inner"), b"theirs").unwrap();
    for name in ["f", "w", "w2"] {
        fs::write(evil.path().join(name), b"theirs").unwrap();
    }
    fs::create_dir(evil.path().join("d")).unwrap();
    let evil_names = ["d", "f", "s", "w", "w2"];
    let evil_ids: Vec<FileId> = evil_names
        .iter()
        .map(|n| id(&evil.path().join(n)))
        .collect();
    let dir = pio::open_dir(&p).unwrap();
    let own = id_of(&dir);
    let (s_id, f_id) = (id(&p.join("s")), id(&p.join("f")));
    let old = root.path().join("p-old");

    let stages = [
        SwapStage::ParentMoved,
        SwapStage::ParentLinked,
        SwapStage::EntriesLinked,
        SwapStage::Restored,
    ];
    let rounds = 6;
    let (made_tx, made) = mpsc::channel::<()>();
    let (checked, checked_rx) = mpsc::channel::<()>();
    let swapper = {
        let (p, old, evil) = (p.clone(), old.clone(), evil.path().to_path_buf());
        std::thread::spawn(move || {
            for _ in 0..rounds {
                for stage in stages {
                    match stage {
                        SwapStage::ParentMoved => {
                            fs::rename(&p, &old).unwrap();
                            fs::create_dir_all(p.join("s")).unwrap();
                            fs::create_dir(p.join("d")).unwrap();
                            for name in ["f", "w", "w2"] {
                                fs::write(p.join(name), b"decoy").unwrap();
                            }
                        }
                        SwapStage::ParentLinked => {
                            fs::remove_dir_all(&p).unwrap();
                            link_dir(&evil, &p);
                        }
                        SwapStage::EntriesLinked => {
                            fs::rename(old.join("s"), old.join("s-aside")).unwrap();
                            link_dir(&evil.join("s"), &old.join("s"));
                            fs::rename(old.join("f"), old.join("f-aside")).unwrap();
                            link_file(&evil.join("f"), &old.join("f"));
                        }
                        SwapStage::Restored => {
                            // what the opener left at `s` and `f`: a link, or the file a rename
                            // put over the link
                            unlink(&old.join("s"));
                            fs::rename(old.join("s-aside"), old.join("s")).unwrap();
                            unlink(&old.join("f"));
                            fs::rename(old.join("f-aside"), old.join("f")).unwrap();
                            unlink(&p);
                            fs::rename(&old, &p).unwrap();
                        }
                    }
                    made_tx.send(()).unwrap(); // the stage is made
                    if checked_rx.recv().is_err() {
                        return; // the opener failed
                    }
                }
            }
        })
    };

    for round in 0..rounds {
        for stage in stages {
            if made.recv().is_err() {
                // the swapper failed: its panic is the cause
                let cause = swapper.join().unwrap_err();
                std::panic::resume_unwind(cause);
            }
            let what = format!("round {round}, {stage:?}");
            let real = match stage {
                SwapStage::Restored => p.clone(),
                _ => old.clone(),
            };
            match stage {
                SwapStage::ParentMoved | SwapStage::ParentLinked | SwapStage::Restored => {
                    // every call through the handle reaches the directory it opened: opens,
                    // kinds and listings, creates, the probe, renames and removals
                    assert_eq!(listed(&dir), ["f", "s"], "{what}");
                    let s = pio::open_dir_at(&dir, OsStr::new("s")).unwrap();
                    assert_eq!(id_of(&s), s_id, "{what}");
                    assert_eq!(read_at(&s, "inner"), "mine", "{what}");
                    drop(s);
                    let f = pio::open_read_at(&dir, OsStr::new("f")).unwrap();
                    assert_eq!(id_of(&f), f_id, "{what}");
                    drop(f);
                    let mut w = pio::create_private_at(&dir, OsStr::new("w")).unwrap();
                    io::Write::write_all(&mut w, b"mine").unwrap();
                    let w_id = id_of(&w);
                    drop(w);
                    pio::rename_at(&dir, OsStr::new("w"), OsStr::new("w2")).unwrap();
                    assert_eq!(id(&real.join("w2")), w_id, "{what}");
                    assert_eq!(read_at(&dir, "w2"), "mine", "{what}");
                    pio::create_private_dir_at(&dir, OsStr::new("d")).unwrap();
                    pio::rename_at(&dir, OsStr::new("d"), OsStr::new("d2")).unwrap();
                    assert_eq!(
                        pio::entry_kind_at(&dir, OsStr::new("d2")).unwrap(),
                        EntryKind::Dir,
                        "{what}"
                    );
                    assert_eq!(listed(&dir), ["d2", "f", "s", "w2"], "{what}");
                    pio::remove_dir_at(&dir, OsStr::new("d2")).unwrap();
                    pio::remove_file_at(&dir, OsStr::new("w2")).unwrap();
                    assert!(
                        pio::umask_keeps_0700_at(&dir, OsStr::new("w")).unwrap(),
                        "{what}"
                    );
                    assert_eq!(listed(&dir), ["f", "s"], "{what}");
                }
                SwapStage::EntriesLinked => {
                    // a link at an entry is never followed: opens refuse it, creates find it
                    // there, and a rename moves or replaces the link itself
                    for name in ["s", "f"] {
                        assert_eq!(
                            pio::entry_kind_at(&dir, OsStr::new(name)).unwrap(),
                            EntryKind::Link,
                            "{what}: {name}"
                        );
                        let e = pio::open_read_at(&dir, OsStr::new(name)).unwrap_err();
                        assert!(pio::is_link_refusal(&e), "{what}: {name}: {e}");
                        let e = pio::open_dir_at(&dir, OsStr::new(name)).unwrap_err();
                        assert!(pio::is_link_refusal(&e), "{what}: {name}: {e}");
                        for e in [
                            pio::create_private_at(&dir, OsStr::new(name)).unwrap_err(),
                            pio::create_private_dir_at(&dir, OsStr::new(name)).unwrap_err(),
                            pio::umask_keeps_0700_at(&dir, OsStr::new(name)).unwrap_err(),
                        ] {
                            assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{what}: {e}");
                        }
                        let e = pio::remove_dir_at(&dir, OsStr::new(name)).unwrap_err();
                        assert!(pio::is_link_refusal(&e), "{what}: {name}: {e}");
                        // the link moves away and back as itself
                        pio::rename_at(&dir, OsStr::new(name), OsStr::new("moved")).unwrap();
                        assert_eq!(
                            pio::entry_kind_at(&dir, OsStr::new("moved")).unwrap(),
                            EntryKind::Link,
                            "{what}: {name}"
                        );
                        pio::rename_at(&dir, OsStr::new("moved"), OsStr::new(name)).unwrap();
                    }
                    // a file renamed over the file link replaces the link, never its target
                    let mut w = pio::create_private_at(&dir, OsStr::new("w")).unwrap();
                    io::Write::write_all(&mut w, b"mine").unwrap();
                    drop(w);
                    pio::rename_at(&dir, OsStr::new("w"), OsStr::new("f")).unwrap();
                    assert_eq!(
                        pio::entry_kind_at(&dir, OsStr::new("f")).unwrap(),
                        EntryKind::File,
                        "{what}"
                    );
                    assert_eq!(read_at(&dir, "f"), "mine", "{what}");
                    assert_eq!(listed(&dir), ["f", "f-aside", "s", "s-aside"], "{what}");
                }
            }
            // the decoys and the other directory are as they were
            for (n, want) in evil_names.iter().zip(&evil_ids) {
                assert_eq!(id(&evil.path().join(n)), *want, "{what}: {n}");
            }
            for n in ["f", "w", "w2"] {
                assert_eq!(fs::read(evil.path().join(n)).unwrap(), b"theirs", "{what}");
            }
            assert_eq!(
                fs::read(evil.path().join("s").join("inner")).unwrap(),
                b"theirs",
                "{what}"
            );
            if let SwapStage::ParentMoved = stage {
                for n in ["f", "w", "w2"] {
                    assert_eq!(fs::read(p.join(n)).unwrap(), b"decoy", "{what}");
                }
                assert!(listed(&pio::open_dir(&p.join("d")).unwrap()).is_empty());
            }
            checked.send(()).unwrap();
        }
    }
    swapper.join().unwrap();
    assert_eq!(listed(&dir), ["f", "s"]);
    assert_eq!(id(&p), own);
    assert_eq!(id(&p.join("s")), s_id);
    assert_eq!(listed(&pio::open_dir(evil.path()).unwrap()), evil_names);
}
