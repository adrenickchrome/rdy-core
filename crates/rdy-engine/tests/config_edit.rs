#![cfg(unix)]
// Unix only: it reads file modes, owners and inodes. FW-XP1 Phase 4 ports it.
//! A locked config file: one blocking sibling lock per file, a reread under the lock, and a
//! durable replacement that keeps a settings file's mode and makes a secret file 0600.

use rdy_engine::config_edit::LockedConfig;
use rdy_engine::{E_IO, E_LIMIT, RdyError};
use rdy_store::fsutil::{PrivateWriteStage, with_private_write_probe_for_tests};
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const LIMIT: usize = 1 << 20;
const PRIVACY: &str = "the config path is not private to this user";
const PRIVACY_FIX: &str =
    "use owner-only permissions and remove symbolic links from the config path";
const SAVE: &str = "the config file could not be saved durably";
const SAVE_FIX: &str = "check the config path and available disk space, then try again";

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

fn lock_of(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".lock");
    PathBuf::from(s)
}

#[track_caller]
fn assert_privacy(r: Result<LockedConfig, RdyError>, what: &str) {
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
    let Some(path) = std::env::var_os("RDY_HSA2_LOCK_CHILD") else {
        return;
    };
    println!("HSA2 opening");
    let mut c = LockedConfig::open(Path::new(&path), true, LIMIT).unwrap();
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
    let path = d.path().join("credentials.yaml");
    let mut held = LockedConfig::open(&path, true, LIMIT).unwrap();
    held.replace(b"first").unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["hsa2_config_lock_child", "--exact", "--nocapture"])
        .args(["--test-threads=1"])
        .env("RDY_HSA2_LOCK_CHILD", &path)
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
    let again = LockedConfig::open(&path, true, LIMIT).unwrap();
    assert_eq!(again.bytes(), b"child");
}

#[test]
fn hsa2_settings_mode_is_preserved_and_secrets_require_0600() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("rdy");
    fs::create_dir(&home).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).unwrap();
    let config = home.join("config.yaml");
    fs::write(&config, b"# mine\nremotes: {}\n").unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o644)).unwrap();
    // a 0644 settings file is read and replaced in place, mode kept
    let mut c = LockedConfig::open(&config, false, LIMIT).unwrap();
    assert_eq!(c.bytes(), b"# mine\nremotes: {}\n");
    c.replace(b"# mine\nremotes: {a: 1}\n").unwrap();
    assert_eq!(c.bytes(), b"# mine\nremotes: {a: 1}\n");
    drop(c);
    assert_eq!(mode(&config), 0o644);
    assert_eq!(mode(&lock_of(&config)), 0o600, "a lock is 0600");
    assert_eq!(
        mode(&home),
        0o755,
        "an owner-held 0755 parent is kept as it is"
    );
    let config_bytes = fs::read(&config).unwrap();
    // a new secret file beside it is 0600 and leaves the settings file byte-identical
    let creds = home.join("credentials.yaml");
    let mut s = LockedConfig::open(&creds, true, LIMIT).unwrap();
    assert_eq!(s.bytes(), b"", "a missing target reads as empty bytes");
    assert!(!creds.exists(), "open creates no target");
    s.replace(b"installs: {}\n").unwrap();
    drop(s);
    assert_eq!(mode(&creds), 0o600);
    assert_eq!(mode(&lock_of(&creds)), 0o600);
    assert_eq!(fs::read(&config).unwrap(), config_bytes);
    assert_eq!(mode(&config), 0o644);
    // a 0644 secret file is refused and left as it is
    fs::set_permissions(&creds, fs::Permissions::from_mode(0o644)).unwrap();
    assert_privacy(
        LockedConfig::open(&creds, true, LIMIT),
        "a 0644 secret file",
    );
    assert_eq!(mode(&creds), 0o644, "no silent chmod");
    assert_eq!(fs::read(&creds).unwrap(), b"installs: {}\n");
    fs::set_permissions(&creds, fs::Permissions::from_mode(0o640)).unwrap();
    assert_privacy(
        LockedConfig::open(&creds, true, LIMIT),
        "a 0640 secret file",
    );
    // a new settings file is 0600
    let local = home.join("local.yaml");
    let mut l = LockedConfig::open(&local, false, LIMIT).unwrap();
    l.replace(b"a: 1\n").unwrap();
    drop(l);
    assert_eq!(mode(&local), 0o600);
    // missing parents are created private, and durably
    let deep = d.path().join("x/y/credentials.yaml");
    let mut n = LockedConfig::open(&deep, true, LIMIT).unwrap();
    n.replace(b"z").unwrap();
    drop(n);
    assert_eq!(mode(&d.path().join("x")), 0o700);
    assert_eq!(mode(&d.path().join("x/y")), 0o700);
    assert_eq!(mode(&deep), 0o600);
}

#[test]
fn hsa2_lock_inode_survives_replacement() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("config.yaml");
    let lock = lock_of(&path);
    let mut c = LockedConfig::open(&path, false, LIMIT).unwrap();
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
    let c = LockedConfig::open(&path, false, LIMIT).unwrap();
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
            let mut c = LockedConfig::open(&path, private, LIMIT).unwrap();
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
            let again = LockedConfig::open(&path, private, LIMIT).unwrap();
            assert_eq!(again.bytes(), want, "{what}: complete bytes on reopen");
        }
    }
}

#[test]
fn hsa2_symlinks_and_foreign_paths_are_refused() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("rdy");
    fs::create_dir(&home).unwrap();
    let elsewhere = d.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let victim = elsewhere.join("victim.yaml");
    fs::write(&victim, b"keep").unwrap();
    fs::set_permissions(&victim, fs::Permissions::from_mode(0o600)).unwrap();
    for private in [true, false] {
        // a link at the target
        let target = home.join(format!("target-{private}.yaml"));
        std::os::unix::fs::symlink(&victim, &target).unwrap();
        assert_privacy(LockedConfig::open(&target, private, LIMIT), "a target link");
        // a link at the lock
        let locked = home.join(format!("locked-{private}.yaml"));
        let victim_lock = elsewhere.join(format!("planted-{private}.lock"));
        std::os::unix::fs::symlink(&victim_lock, lock_of(&locked)).unwrap();
        assert_privacy(LockedConfig::open(&locked, private, LIMIT), "a lock link");
        assert!(
            !victim_lock.exists(),
            "the link at the lock is never followed"
        );
        // a link at an existing parent
        let linked_home = d.path().join(format!("linked-{private}"));
        std::os::unix::fs::symlink(&elsewhere, &linked_home).unwrap();
        assert_privacy(
            LockedConfig::open(&linked_home.join("victim.yaml"), private, LIMIT),
            "a parent link",
        );
        // a directory at the target is not a regular file
        let dir_target = home.join(format!("dir-{private}.yaml"));
        fs::create_dir(&dir_target).unwrap();
        assert_privacy(
            LockedConfig::open(&dir_target, private, LIMIT),
            "a directory target",
        );
        // a parent owned by another user (the system's temp directory, owned by root)
        if rdy_store::pio::own_uid() != 0 {
            let root_tmp = fs::canonicalize("/tmp").unwrap();
            if fs::symlink_metadata(&root_tmp).unwrap().uid() != rdy_store::pio::own_uid() {
                let foreign =
                    root_tmp.join(format!("rdy-hsa2-{}-{private}.yaml", std::process::id()));
                assert_privacy(
                    LockedConfig::open(&foreign, private, LIMIT),
                    "a foreign parent",
                );
                assert!(!lock_of(&foreign).exists(), "no lock is made there");
            }
        }
        // a target owned by another user, where this system lets a hard link reach one
        let theirs = home.join(format!("theirs-{private}.yaml"));
        let linked = ["/private/etc/hosts", "/etc/hosts", "/etc/passwd"]
            .iter()
            .filter_map(|p| fs::canonicalize(p).ok())
            .filter(|p| fs::metadata(p).is_ok_and(|m| m.uid() != rdy_store::pio::own_uid()))
            .any(|p| fs::hard_link(&p, &theirs).is_ok());
        if linked {
            assert_privacy(
                LockedConfig::open(&theirs, private, LIMIT),
                "a foreign target",
            );
            fs::remove_file(&theirs).unwrap();
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
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let e = LockedConfig::open(&path, true, 16).err().unwrap();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_LIMIT,
            "config input exceeds 16 bytes",
            "reduce the config file to the stated limit"
        )
    );
    fs::write(&path, [b'a'; 16]).unwrap();
    let mut c = LockedConfig::open(&path, true, 16).unwrap();
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
