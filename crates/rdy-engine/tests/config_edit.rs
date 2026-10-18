#![cfg(unix)]
// Unix only: it reads file modes, owners and inodes. FW-XP1 Phase 4 ports it.
//! A locked config file: one blocking sibling lock per file, a reread under the lock, and a
//! durable replacement that keeps a settings file's mode and makes a secret file 0600. Every
//! entry below the caller's config home is checked without following links, and the
//! replacement goes through the directory checked at open.

use rdy_engine::config_edit::LockedConfig;
use rdy_engine::{E_IO, E_LIMIT, RdyError};
use rdy_store::fsutil::{PrivateWriteStage, with_private_write_probe_for_tests};
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const LIMIT: usize = 1 << 20;
const PRIVACY: &str = "the config path is not private to this user";
const PRIVACY_FIX: &str =
    "use owner-only permissions and remove symbolic links from the config path";
const SAVE: &str = "the config file could not be saved durably";
const SAVE_FIX: &str = "check the config path and available disk space, then try again";

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn lock_of(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".lock");
    PathBuf::from(s)
}

/// Opens `rel` below `home` with the usual limit.
fn open(home: &Path, rel: &str, private: bool) -> Result<LockedConfig, RdyError> {
    LockedConfig::open(home, Path::new(rel), private, LIMIT)
}

#[track_caller]
fn assert_privacy<T>(r: Result<T, RdyError>, what: &str) {
    let e = r
        .err()
        .unwrap_or_else(|| panic!("{what}: expected a refusal"));
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (E_IO, PRIVACY, PRIVACY_FIX),
        "{what}"
    );
}

/// The child half of `hsa2_config_lock_serializes_processes`: it runs only in the process that
/// test starts.
#[test]
fn hsa2_config_lock_child() {
    let Some(home) = std::env::var_os("RDY_HSA2_LOCK_CHILD") else {
        return;
    };
    println!("HSA2 opening");
    let mut c = open(Path::new(&home), "credentials.yaml", true).unwrap();
    println!("HSA2 read {}", String::from_utf8_lossy(c.bytes()));
    c.replace(b"child").unwrap();
    drop(c);
    println!("HSA2 done");
}

fn next_mark(lines: &mut impl Iterator<Item = io::Result<String>>) -> String {
    for l in lines.by_ref() {
        let l = l.unwrap();
        // libtest may print "test <name> ... " on the same line first
        if let Some((_, rest)) = l.split_once("HSA2 ") {
            return rest.to_string();
        }
    }
    panic!("the child ended before its next mark");
}

#[test]
fn hsa2_config_lock_serializes_processes() {
    let d = tempfile::tempdir().unwrap();
    let mut held = open(d.path(), "credentials.yaml", true).unwrap();
    held.replace(b"first").unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["hsa2_config_lock_child", "--exact", "--nocapture"])
        .args(["--test-threads=1"])
        .env("RDY_HSA2_LOCK_CHILD", d.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    assert_eq!(next_mark(&mut lines), "opening");
    // the child is opening now; it cannot read until this lock is released, so it reads the
    // bytes written after its mark
    held.replace(b"winner").unwrap();
    drop(held);
    assert_eq!(next_mark(&mut lines), "read winner");
    assert_eq!(next_mark(&mut lines), "done");
    assert!(child.wait().unwrap().success());
    let again = open(d.path(), "credentials.yaml", true).unwrap();
    assert_eq!(again.bytes(), b"child");
}

#[test]
fn hsa2_settings_mode_is_preserved_and_secrets_require_0600() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path();
    let rdy = home.join("rdy");
    fs::create_dir(&rdy).unwrap();
    chmod(&rdy, 0o755);
    let config = rdy.join("config.yaml");
    fs::write(&config, b"# mine\nremotes: {}\n").unwrap();
    chmod(&config, 0o644);
    // a 0644 settings file is read and replaced in place, mode kept
    let mut c = open(home, "rdy/config.yaml", false).unwrap();
    assert_eq!(c.bytes(), b"# mine\nremotes: {}\n");
    c.replace(b"# mine\nremotes: {a: 1}\n").unwrap();
    assert_eq!(c.bytes(), b"# mine\nremotes: {a: 1}\n");
    drop(c);
    assert_eq!(mode(&config), 0o644);
    assert_eq!(mode(&lock_of(&config)), 0o600, "a lock is 0600");
    assert_eq!(
        mode(&rdy),
        0o755,
        "an owner-held 0755 parent is kept as it is"
    );
    let config_bytes = fs::read(&config).unwrap();
    // a new secret file beside it is 0600 and leaves the settings file byte-identical
    let creds = rdy.join("credentials.yaml");
    let mut s = open(home, "rdy/credentials.yaml", true).unwrap();
    assert_eq!(s.bytes(), b"", "a missing target reads as empty bytes");
    assert!(!creds.exists(), "open creates no target");
    s.replace(b"installs: {}\n").unwrap();
    drop(s);
    assert_eq!(mode(&creds), 0o600);
    assert_eq!(mode(&lock_of(&creds)), 0o600);
    assert_eq!(fs::read(&config).unwrap(), config_bytes);
    assert_eq!(mode(&config), 0o644);
    // a 0644 secret file is refused and left as it is
    chmod(&creds, 0o644);
    assert_privacy(
        open(home, "rdy/credentials.yaml", true),
        "a 0644 secret file",
    );
    assert_eq!(mode(&creds), 0o644, "no silent chmod");
    assert_eq!(fs::read(&creds).unwrap(), b"installs: {}\n");
    chmod(&creds, 0o640);
    assert_privacy(
        open(home, "rdy/credentials.yaml", true),
        "a 0640 secret file",
    );
    // a new settings file is 0600
    let mut l = open(home, "rdy/local.yaml", false).unwrap();
    l.replace(b"a: 1\n").unwrap();
    drop(l);
    assert_eq!(mode(&rdy.join("local.yaml")), 0o600);
    // missing parents are created private, and durably
    let mut n = open(home, "x/y/credentials.yaml", true).unwrap();
    n.replace(b"z").unwrap();
    drop(n);
    assert_eq!(mode(&home.join("x")), 0o700);
    assert_eq!(mode(&home.join("x/y")), 0o700);
    assert_eq!(mode(&home.join("x/y/credentials.yaml")), 0o600);
}

#[test]
fn hsa2_lock_inode_survives_replacement() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("config.yaml");
    let lock = lock_of(&path);
    let mut c = open(d.path(), "config.yaml", false).unwrap();
    let ino = fs::symlink_metadata(&lock).unwrap().ino();
    c.replace(b"one").unwrap();
    let target_ino = fs::symlink_metadata(&path).unwrap().ino();
    c.replace(b"two").unwrap();
    assert_ne!(
        fs::symlink_metadata(&path).unwrap().ino(),
        target_ino,
        "the target is replaced by a rename"
    );
    assert_eq!(fs::symlink_metadata(&lock).unwrap().ino(), ino);
    drop(c);
    assert_eq!(
        fs::symlink_metadata(&lock).unwrap().ino(),
        ino,
        "a lock file is never unlinked"
    );
    let c = open(d.path(), "config.yaml", false).unwrap();
    assert_eq!(c.bytes(), b"two", "the next open rereads the target");
    assert_eq!(fs::symlink_metadata(&lock).unwrap().ino(), ino);
    drop(c);
    assert_eq!(names(d.path()), ["config.yaml", "config.yaml.lock"]);
}

#[test]
fn hsa2_failure_keeps_old_or_complete_new_bytes() {
    // (the stage whose probe fails, what that models, whether the new bytes are in place)
    let faults = [
        (PrivateWriteStage::Created, "write", false),
        (PrivateWriteStage::Written, "sync", false),
        (PrivateWriteStage::Synced, "rename", false),
        (PrivateWriteStage::Renamed, "dir-sync", true),
        (PrivateWriteStage::DirSynced, "after the dir sync", true),
    ];
    for (fault, what, renamed) in faults {
        for private in [true, false] {
            let d = tempfile::tempdir().unwrap();
            let path = d.path().join("f.yaml");
            let mut c = open(d.path(), "f.yaml", private).unwrap();
            c.replace(b"old bytes").unwrap();
            let e = with_private_write_probe_for_tests(
                move |stage, _: &Path| {
                    if stage == fault {
                        Err(io::Error::other("injected /secret/path"))
                    } else {
                        Ok(())
                    }
                },
                || c.replace(b"new bytes"),
            )
            .unwrap_err();
            assert_eq!(
                (e.code, e.message.as_str(), e.fix.as_str()),
                (E_IO, SAVE, SAVE_FIX),
                "{what}: no OS text or path enters the error"
            );
            drop(c);
            let want: &[u8] = if renamed { b"new bytes" } else { b"old bytes" };
            assert_eq!(fs::read(&path).unwrap(), want, "{what} private={private}");
            assert_eq!(
                names(d.path()),
                ["f.yaml", "f.yaml.lock"],
                "{what}: the temp file is removed"
            );
            let again = open(d.path(), "f.yaml", private).unwrap();
            assert_eq!(again.bytes(), want, "{what}: complete bytes on reopen");
        }
    }
}

#[test]
fn hsa2_symlinks_and_foreign_paths_are_refused() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path();
    fs::create_dir(home.join("rdy")).unwrap();
    let elsewhere = home.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let victim = elsewhere.join("victim.yaml");
    fs::write(&victim, b"keep").unwrap();
    chmod(&victim, 0o600);
    for private in [true, false] {
        // a link at the target
        let target = format!("rdy/target-{private}.yaml");
        symlink(&victim, home.join(&target)).unwrap();
        assert_privacy(open(home, &target, private), "a target link");
        // a link at the lock
        let locked = format!("rdy/locked-{private}.yaml");
        let victim_lock = elsewhere.join(format!("planted-{private}.lock"));
        symlink(&victim_lock, lock_of(&home.join(&locked))).unwrap();
        assert_privacy(open(home, &locked, private), "a lock link");
        assert!(
            !victim_lock.exists(),
            "the link at the lock is never followed"
        );
        // a link at the directory that holds the file
        let linked = format!("linked-{private}");
        symlink(&elsewhere, home.join(&linked)).unwrap();
        assert_privacy(
            open(home, &format!("{linked}/victim.yaml"), private),
            "a parent link",
        );
        // a directory at the target is not a regular file
        let dir_target = format!("rdy/dir-{private}.yaml");
        fs::create_dir(home.join(&dir_target)).unwrap();
        assert_privacy(open(home, &dir_target, private), "a directory target");
        // an intermediate directory owned by another user (the system's temp directory,
        // owned by root), with the directory above it as the home
        if rdy_store::pio::own_uid() != 0 {
            let root_tmp = fs::canonicalize("/tmp").unwrap();
            if fs::symlink_metadata(&root_tmp).unwrap().uid() != rdy_store::pio::own_uid() {
                let sub = format!("rdy-hsa2-{}-{private}", std::process::id());
                let rel = Path::new(root_tmp.file_name().unwrap())
                    .join(&sub)
                    .join("credentials.yaml");
                assert_privacy(
                    LockedConfig::open(root_tmp.parent().unwrap(), &rel, private, LIMIT),
                    "a foreign intermediate directory",
                );
                assert!(!root_tmp.join(&sub).exists(), "nothing is made there");
            }
        }
        // a target owned by another user, where this system lets a hard link reach one
        let theirs = format!("rdy/theirs-{private}.yaml");
        let linked = ["/private/etc/hosts", "/etc/hosts", "/etc/passwd"]
            .iter()
            .filter_map(|p| fs::canonicalize(p).ok())
            .filter(|p| fs::metadata(p).is_ok_and(|m| m.uid() != rdy_store::pio::own_uid()))
            .any(|p| fs::hard_link(&p, home.join(&theirs)).is_ok());
        if linked {
            assert_privacy(open(home, &theirs, private), "a foreign target");
            fs::remove_file(home.join(&theirs)).unwrap();
        }
    }
    assert_eq!(fs::read(&victim).unwrap(), b"keep");
    assert_eq!(mode(&victim), 0o600);
}

#[test]
fn hsa2_config_reads_and_writes_are_bounded() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("config.yaml");
    fs::write(&path, [b'a'; 17]).unwrap();
    chmod(&path, 0o600);
    let e = LockedConfig::open(d.path(), Path::new("config.yaml"), true, 16)
        .err()
        .unwrap();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_LIMIT,
            "config input exceeds 16 bytes",
            "reduce the config file to the stated limit"
        )
    );
    fs::write(&path, [b'a'; 16]).unwrap();
    let mut c = LockedConfig::open(d.path(), Path::new("config.yaml"), true, 16).unwrap();
    assert_eq!(c.bytes(), [b'a'; 16]);
    let e = with_private_write_probe_for_tests(
        |_, _: &Path| -> io::Result<()> { panic!("no temp file is made for an oversize write") },
        || c.replace(&[b'b'; 17]),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_LIMIT, "config input exceeds 16 bytes")
    );
    assert_eq!(c.bytes(), [b'a'; 16]);
    c.replace(&[b'c'; 16]).unwrap();
    drop(c);
    assert_eq!(fs::read(&path).unwrap(), [b'c'; 16]);
    assert_eq!(names(d.path()), ["config.yaml", "config.yaml.lock"]);
}

// Fix round 1.

/// Finding 1: in a directory that group or others may write, another user can swap the synced
/// temp file for their own before the rename. Such a directory is refused, at open and again at
/// replace, before any temp file is made. An owner-held 0755 directory still works.
#[test]
fn hsa2_a_directory_others_may_write_is_refused_before_a_temp_is_made() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path();
    let rdy = home.join("rdy");
    fs::create_dir(&rdy).unwrap();
    for wide in [0o777, 0o775, 0o757, 0o1777] {
        chmod(&rdy, wide);
        assert_privacy(
            open(home, "rdy/credentials.yaml", true),
            &format!("a {wide:o} directory"),
        );
        assert_privacy(
            open(home, "rdy/config.yaml", false),
            &format!("a {wide:o} directory, settings"),
        );
        assert_eq!(mode(&rdy), wide, "no silent chmod");
        assert!(names(&rdy).is_empty(), "{wide:o}: no lock is made there");
    }
    chmod(&rdy, 0o755);
    let mut c = open(home, "rdy/credentials.yaml", true).unwrap();
    c.replace(b"mine").unwrap();
    // the directory is opened to others after open: the replace refuses before the temp file is
    // made, so the swap this probe would make never happens
    chmod(&rdy, 0o777);
    let e = with_private_write_probe_for_tests(
        |stage, p: &Path| {
            if stage == PrivateWriteStage::Synced {
                fs::remove_file(p)?;
                fs::write(p, b"theirs")?;
            }
            Ok(())
        },
        || c.replace(b"new"),
    );
    assert_privacy(e, "a directory opened to others after open");
    assert_eq!(fs::read(rdy.join("credentials.yaml")).unwrap(), b"mine");
    assert_eq!(c.bytes(), b"mine");
    assert_eq!(
        names(&rdy),
        ["credentials.yaml", "credentials.yaml.lock"],
        "no temp file is made"
    );
    drop(c);
    chmod(&rdy, 0o755);
    // a home that holds the file itself is judged as the directory that holds it
    let wide_home = home.join("wide");
    fs::create_dir(&wide_home).unwrap();
    chmod(&wide_home, 0o777);
    assert_privacy(
        open(&wide_home, "credentials.yaml", true),
        "a wide home that holds the file",
    );
    assert!(names(&wide_home).is_empty());
}

/// Finding 2: the replacement goes through the directory checked at open, and a path changed
/// after open is refused. A config renamed aside and replaced by a link to another config's
/// directory never overwrites that other config, which keeps its own lock.
#[test]
fn hsa2_a_replaced_directory_is_refused_and_never_redirected() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path();
    for name in ["a", "b"] {
        fs::create_dir(home.join(name)).unwrap();
    }
    let mut a = open(home, "a/credentials.yaml", true).unwrap();
    a.replace(b"a bytes").unwrap();
    let mut b = open(home, "b/credentials.yaml", true).unwrap();
    b.replace(b"b bytes").unwrap();
    // a is renamed aside and replaced by a link to b
    fs::rename(home.join("a"), home.join("a-aside")).unwrap();
    symlink(home.join("b"), home.join("a")).unwrap();
    assert_privacy(a.replace(b"redirected"), "a directory replaced by a link");
    assert_eq!(
        fs::read(home.join("b/credentials.yaml")).unwrap(),
        b"b bytes"
    );
    assert_eq!(
        fs::read(home.join("a-aside/credentials.yaml")).unwrap(),
        b"a bytes"
    );
    // b's lock and replacement still work
    b.replace(b"b again").unwrap();
    assert_eq!(
        fs::read(home.join("b/credentials.yaml")).unwrap(),
        b"b again"
    );
    // a real directory put in its place is refused too
    fs::remove_file(home.join("a")).unwrap();
    fs::create_dir(home.join("a")).unwrap();
    assert_privacy(a.replace(b"new dir"), "a directory replaced by another");
    assert!(names(&home.join("a")).is_empty());
    drop(a);
    // a swap during the write cannot redirect it: the bytes land in the directory opened at
    // open, the other config is untouched, and the changed path is an error
    fs::remove_dir(home.join("a")).unwrap();
    fs::rename(home.join("a-aside"), home.join("a")).unwrap();
    let mut a = open(home, "a/credentials.yaml", true).unwrap();
    let swap_home = home.to_path_buf();
    let e = with_private_write_probe_for_tests(
        move |stage, _: &Path| {
            if stage == PrivateWriteStage::Written {
                fs::rename(swap_home.join("a"), swap_home.join("a-aside"))?;
                symlink(swap_home.join("b"), swap_home.join("a"))?;
            }
            Ok(())
        },
        || a.replace(b"during the swap"),
    );
    assert_privacy(e, "a directory swapped during the write");
    assert_eq!(
        fs::read(home.join("b/credentials.yaml")).unwrap(),
        b"b again"
    );
    assert_eq!(
        fs::read(home.join("a-aside/credentials.yaml")).unwrap(),
        b"during the swap",
        "the bytes went to the directory opened at open"
    );
    assert_eq!(
        names(&home.join("b")),
        ["credentials.yaml", "credentials.yaml.lock"]
    );
    drop(b);
    // a target link planted after open is refused, not replaced
    let mut c = open(home, "b/credentials.yaml", true).unwrap();
    let outside = home.join("outside.yaml");
    fs::write(&outside, b"outside").unwrap();
    fs::remove_file(home.join("b/credentials.yaml")).unwrap();
    symlink(&outside, home.join("b/credentials.yaml")).unwrap();
    assert_privacy(
        c.replace(b"through the link"),
        "a target link planted after open",
    );
    assert!(
        fs::symlink_metadata(home.join("b/credentials.yaml"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "the link is kept"
    );
    assert_eq!(fs::read(&outside).unwrap(), b"outside");
}

/// Finding 3 (ruling): every entry below the caller's home is checked without following links,
/// whether the deeper directories exist or must be made; nothing is written where a link
/// points. The home itself may be reached through a link.
#[test]
fn hsa2_every_component_below_the_home_is_checked() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    fs::create_dir(&home).unwrap();
    chmod(&home, 0o755);
    let elsewhere = d.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    symlink(&elsewhere, home.join("link")).unwrap();
    // H/link -> E, target H/link/sub/credentials.yaml: sub missing, then present
    assert_privacy(
        open(&home, "link/sub/credentials.yaml", true),
        "a link above a missing directory",
    );
    assert!(
        names(&elsewhere).is_empty(),
        "nothing is made under the link"
    );
    fs::create_dir(elsewhere.join("sub")).unwrap();
    for private in [true, false] {
        assert_privacy(
            open(&home, "link/sub/credentials.yaml", private),
            "a link above an existing directory",
        );
    }
    assert!(
        names(&elsewhere.join("sub")).is_empty(),
        "nothing is written there"
    );
    // a link as the last directory, and as an intermediate one
    assert_privacy(
        open(&home, "link/credentials.yaml", true),
        "a link as the last directory",
    );
    fs::create_dir(home.join("a")).unwrap();
    symlink(&elsewhere, home.join("a/mid")).unwrap();
    assert_privacy(
        open(&home, "a/mid/sub/credentials.yaml", true),
        "a link as an intermediate directory",
    );
    assert!(names(&elsewhere.join("sub")).is_empty());
    // a file where a directory is needed
    fs::write(home.join("plain"), b"x").unwrap();
    assert_privacy(
        open(&home, "plain/credentials.yaml", true),
        "a file as a directory",
    );
    // rel must be relative, plain and not empty
    for bad in [
        "",
        "/abs/credentials.yaml",
        "../credentials.yaml",
        "rdy/../credentials.yaml",
        "./credentials.yaml",
        "rdy/./credentials.yaml",
        "rdy//credentials.yaml",
        "rdy/",
    ] {
        assert_privacy(open(&home, bad, true), &format!("rel {bad:?}"));
    }
    assert_eq!(
        names(&home),
        ["a", "link", "plain"],
        "nothing is made for a bad rel"
    );
    // a missing or non-directory home
    assert_privacy(
        open(&d.path().join("missing"), "c.yaml", true),
        "a missing home",
    );
    assert_privacy(
        open(&home.join("plain"), "c.yaml", true),
        "a file as the home",
    );
    // a home reached through a link works, and its files are where the link points
    let via = d.path().join("via");
    symlink(&home, &via).unwrap();
    let mut c = open(&via, "rdy/credentials.yaml", true).unwrap();
    c.replace(b"through the home link").unwrap();
    drop(c);
    assert_eq!(
        fs::read(home.join("rdy/credentials.yaml")).unwrap(),
        b"through the home link"
    );
    let c = open(&via, "rdy/credentials.yaml", true).unwrap();
    assert_eq!(c.bytes(), b"through the home link");
}

/// Finding 4: an existing lock that group or others may use is refused, never chmodded, so no
/// other user can hold it. An owner-only lock with fewer bits (a racing creator's umask) is used.
#[test]
fn hsa2_a_lock_open_to_group_or_others_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path();
    let lock = home.join("credentials.yaml.lock");
    for wide in [0o666, 0o640, 0o604, 0o660] {
        fs::write(&lock, b"").unwrap();
        chmod(&lock, wide);
        for private in [true, false] {
            assert_privacy(
                open(home, "credentials.yaml", private),
                &format!("a {wide:o} lock, private={private}"),
            );
        }
        assert_eq!(mode(&lock), wide, "no silent chmod");
        fs::remove_file(&lock).unwrap();
    }
    fs::write(&lock, b"").unwrap();
    chmod(&lock, 0o400);
    let mut c = open(home, "credentials.yaml", true).unwrap();
    c.replace(b"x").unwrap();
    drop(c);
    assert_eq!(mode(&lock), 0o400, "an owner-only lock is used as it is");
    assert_eq!(fs::read(home.join("credentials.yaml")).unwrap(), b"x");
}

/// The home of the child half of the umask test; without it the child does nothing.
const UMASK_CHILD: &str = "RDY_HSA2_UMASK_CHILD";

/// The child's expectation: `made` (the directories are made 0700) or `refused`.
const UMASK_EXPECT: &str = "RDY_HSA2_UMASK_EXPECT";

/// Round 1 finding 5, and round 3: a directory this open makes is never chmodded (no call proves
/// that the directory opened after a mkdir is the one it made), so it is made only when the umask
/// keeps 0o700 whole, and is then 0o700. A umask that takes an owner bit (277, 477, 177, 377,
/// 700) is refused before anything is made, so it cannot leave an unusable config directory or a
/// probe file. Each umask runs in its own process, set by `sh`, twice against the same home: the
/// second process finds the first one's directories usable, or finds nothing made.
#[test]
fn hsa2_new_directories_are_0700_or_not_made_at_all() {
    let exe = std::env::current_exe().unwrap();
    let cases = [
        ("000", "made"),
        ("022", "made"),
        ("077", "made"),
        ("277", "refused"),
        ("477", "refused"),
        ("177", "refused"),
        ("377", "refused"),
        ("700", "refused"),
    ];
    for (umask, expect) in cases {
        let d = tempfile::tempdir().unwrap();
        chmod(d.path(), 0o700);
        for run in 1..=2 {
            let out = Command::new("/bin/sh")
                .args(["-c", r#"umask "$1" && shift && exec "$@""#, "sh", umask])
                .arg(&exe)
                .args(["hsa2_new_directories_child", "--exact", "--test-threads=1"])
                .arg("--nocapture")
                .env(UMASK_CHILD, d.path())
                .env(UMASK_EXPECT, expect)
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success()
                    && stdout.contains("test result: ok. 1 passed")
                    && stdout.contains("HSA2 umask child done"),
                "umask {umask}, run {run}: {}\n{stdout}\n{}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        if expect == "refused" {
            assert!(names(d.path()).is_empty(), "umask {umask}: nothing is made");
        }
    }
}

/// The child half of the test above: it runs only in the process that test starts.
#[test]
fn hsa2_new_directories_child() {
    let Some(home) = std::env::var_os(UMASK_CHILD) else {
        return;
    };
    let home = Path::new(&home);
    if std::env::var(UMASK_EXPECT).as_deref() == Ok("refused") {
        for rel in ["rdy/deeper/credentials.yaml", "rdy/config.yaml"] {
            let e = open(home, rel, rel.ends_with("credentials.yaml"))
                .err()
                .expect("a umask that takes an owner bit is refused");
            assert_eq!(
                (e.code, e.message.as_str(), e.fix.as_str()),
                (E_IO, SAVE, SAVE_FIX),
                "{rel}"
            );
        }
        assert!(names(home).is_empty(), "no directory and no probe file");
        println!("HSA2 umask child done");
        return;
    }
    // twice: the second open must find the first one's directories usable
    for _ in 0..2 {
        let mut c = open(home, "rdy/deeper/credentials.yaml", true).unwrap();
        c.replace(b"secret").unwrap();
        drop(c);
        let mut s = open(home, "rdy/config.yaml", false).unwrap();
        s.replace(b"a: 1\n").unwrap();
    }
    assert_eq!(mode(&home.join("rdy")), 0o700);
    assert_eq!(mode(&home.join("rdy/deeper")), 0o700);
    assert_eq!(mode(&home.join("rdy/deeper/credentials.yaml")), 0o600);
    assert_eq!(mode(&home.join("rdy/deeper/credentials.yaml.lock")), 0o600);
    assert_eq!(mode(&home.join("rdy/config.yaml")), 0o600);
    assert_eq!(names(home), ["rdy"], "no probe file is left");
    let c = open(home, "rdy/deeper/credentials.yaml", true).unwrap();
    assert_eq!(c.bytes(), b"secret");
    println!("HSA2 umask child done");
}

/// Round 2, finding 2, and round 3: mkdirat returns no handle, so the directory at the new name
/// when it is opened cannot be proved to be the one the mkdir made. It is never chmodded: it is
/// used only if it is what that mkdir makes under a umask that keeps 0o700 whole (checked before
/// the mkdir), a 0o700 directory this user owns. A directory put at the name between the mkdir
/// and the open with any other mode, owner-only ones included, is refused, keeps its mode and
/// gets nothing written into it, and so does a link there.
#[test]
fn hsa2_a_directory_put_in_place_of_a_new_one_is_refused_and_never_chmodded() {
    // (what is put at the new name, its mode, a file inside it, the refusal); a mode without the
    // owner's read bit cannot be opened for reading, which is a failed save
    let cases = [
        ("a same-owner 0755 directory", 0o755, false, PRIVACY),
        (
            "a same-owner 0750 directory with a file",
            0o750,
            true,
            PRIVACY,
        ),
        ("a same-owner 0711 directory", 0o711, false, PRIVACY),
        ("a link to a directory", 0o700, false, PRIVACY),
        ("a same-owner 0500 directory", 0o500, false, PRIVACY),
        ("a same-owner 0600 directory", 0o600, false, PRIVACY),
        (
            "a same-owner 0500 directory with a file",
            0o500,
            true,
            PRIVACY,
        ),
        ("a same-owner 0300 directory", 0o300, false, SAVE),
        ("a same-owner 0100 directory", 0o100, false, SAVE),
    ];
    for (what, pre_mode, with_file, refusal) in cases {
        for private in [true, false] {
            let d = tempfile::tempdir().unwrap();
            let home = d.path().to_path_buf();
            chmod(&home, 0o700);
            let pre = home.join("pre");
            fs::create_dir(&pre).unwrap();
            if with_file {
                fs::write(pre.join("keep"), b"keep").unwrap();
            }
            chmod(&pre, pre_mode);
            let pre_ino = fs::symlink_metadata(&pre).unwrap().ino();
            let link = what.starts_with("a link");
            let made = home.join("rdy");
            let (h, m, p) = (home.clone(), made.clone(), pre.clone());
            let calls = std::rc::Rc::new(std::cell::Cell::new(0));
            let count = calls.clone();
            let r = with_private_write_probe_for_tests(
                move |stage, path: &Path| {
                    if stage == PrivateWriteStage::DirMade {
                        count.set(count.get() + 1);
                        assert_eq!(path, m, "the probe gets the new directory's path");
                        fs::rename(&m, h.join("aside"))?;
                        if link {
                            symlink(&p, &m)?;
                        } else {
                            fs::rename(&p, &m)?;
                        }
                    }
                    Ok(())
                },
                || open(&home, "rdy/credentials.yaml", private),
            );
            let e = r
                .err()
                .unwrap_or_else(|| panic!("{what}, private {private}: expected a refusal"));
            assert_eq!(
                (e.code, e.message.as_str()),
                (E_IO, refusal),
                "{what}, private {private}"
            );
            assert_eq!(
                calls.get(),
                1,
                "{what}: the probe ran between the mkdir and the open"
            );
            let (stands, want_names): (PathBuf, Vec<&str>) = if link {
                (pre.clone(), if with_file { vec!["keep"] } else { vec![] })
            } else {
                (made.clone(), if with_file { vec!["keep"] } else { vec![] })
            };
            assert_eq!(mode(&stands), pre_mode, "{what}: never chmodded");
            assert_eq!(
                fs::symlink_metadata(&stands).unwrap().ino(),
                pre_ino,
                "{what}"
            );
            // the test lists it, and the temp dir removes it, only with the owner's bits back
            chmod(&stands, 0o700);
            assert_eq!(
                names(&stands),
                want_names,
                "{what}: no lock or file is made in it"
            );
            assert!(
                names(&home.join("aside")).is_empty(),
                "{what}: nothing is written into the directory moved aside"
            );
        }
    }
}

/// Round 3, the limit of finding 2. No call proves which directory a mkdir made, so an empty
/// 0o700 directory of this user, put at the new name between the mkdir and the open, is the one
/// case that cannot be told from the new directory. It is used exactly as the same directory is
/// when it stands there before the open (the ruling accepts an existing owner-held directory):
/// its mode and inode stay, it is never chmodded, and the lock is made in it.
#[test]
fn hsa2_an_empty_0700_directory_put_in_place_is_used_as_an_existing_one_unchanged() {
    for private in [true, false] {
        // put in place between the mkdir and the open
        let d = tempfile::tempdir().unwrap();
        let home = d.path().to_path_buf();
        chmod(&home, 0o700);
        let pre = home.join("pre");
        fs::create_dir(&pre).unwrap();
        chmod(&pre, 0o700);
        let pre_ino = fs::symlink_metadata(&pre).unwrap().ino();
        let made = home.join("rdy");
        let (h, m, p) = (home.clone(), made.clone(), pre.clone());
        let c = with_private_write_probe_for_tests(
            move |stage, _path: &Path| {
                if stage == PrivateWriteStage::DirMade {
                    fs::rename(&m, h.join("aside"))?;
                    fs::rename(&p, &m)?;
                }
                Ok(())
            },
            || open(&home, "rdy/credentials.yaml", private),
        )
        .unwrap();
        drop(c);
        assert_eq!(mode(&made), 0o700, "never chmodded");
        assert_eq!(fs::symlink_metadata(&made).unwrap().ino(), pre_ino);
        assert_eq!(names(&made), ["credentials.yaml.lock"]);
        assert!(names(&home.join("aside")).is_empty());
        // the same directory standing there before the open: the same outcome
        let d2 = tempfile::tempdir().unwrap();
        chmod(d2.path(), 0o700);
        fs::create_dir(d2.path().join("rdy")).unwrap();
        chmod(&d2.path().join("rdy"), 0o700);
        drop(open(d2.path(), "rdy/credentials.yaml", private).unwrap());
        assert_eq!(mode(&d2.path().join("rdy")), 0o700);
        assert_eq!(names(&d2.path().join("rdy")), ["credentials.yaml.lock"]);
    }
}
