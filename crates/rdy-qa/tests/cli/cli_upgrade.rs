//! ok11: `rdy upgrade [dir] [--dry-run] [--json]` (D-WWAK7, D-1HDVA): a current store is
//! current, a dry run changes nothing and starts no daemon, and a preview store (a scalar store
//! format) gets the exact dump-and-load refusal, with no backup or upgrade.
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::rdy_bin;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<(u64, Vec<u8>)>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let m = std::fs::symlink_metadata(&p).unwrap();
        if m.is_dir() {
            out.insert(p.clone(), None);
            out.extend(tree(&p));
        } else {
            out.insert(p.clone(), Some((m.len(), std::fs::read(&p).unwrap())));
        }
    }
    out
}

fn has_backup(root: &Path) -> bool {
    std::fs::read_dir(root.join(".rdy")).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".store-upgrade-")
    })
}

const DUMP_LOAD: &str =
    "dump with the rdy that wrote this store, then load into a directory with no store";

#[test]
fn ok11_preview_store_asks_for_dump_load() {
    let s = Served::new();
    let root = s.dir.path().to_path_buf();
    std::fs::write(
        root.join(".rdy/store/FORMAT"),
        "rdy store format\nstore_format 18\n",
    )
    .unwrap();
    let before = tree(&root);
    let dir = root.to_string_lossy().into_owned();
    for args in [
        vec!["--json", "upgrade"],
        vec!["--json", "upgrade", "--dry-run"],
        vec!["--json", "upgrade", dir.as_str()],
    ] {
        let (ok, e) = s.json(&args);
        assert!(!ok, "{args:?}: {e}");
        assert_eq!(e["code"], "E_STORE_FORMAT", "{args:?}: {e}");
        assert_eq!(
            e["message"],
            "store format 18 predates feature-based stores"
        );
        assert_eq!(e["fix"], DUMP_LOAD);
    }
    let out = s.rdy(&["upgrade"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!(
            "error E_STORE_FORMAT: store format 18 predates feature-based stores\nfix: {DUMP_LOAD}\n"
        )
    );
    assert_eq!(
        tree(&root),
        before,
        "no backup, lock, index or run directory"
    );
    assert!(!has_backup(&root));
    // an ordinary connection gets the same refusal before any daemon starts
    let (ok, e) = s.json(&["--json", "status"]);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_STORE_FORMAT", "{e}");
    assert_eq!(
        e["message"],
        "store format 18 predates feature-based stores"
    );
    assert_eq!(e["fix"], DUMP_LOAD);
    assert!(rdy_daemon::live_daemon(&root).is_none());
    let store = root.join(".rdy/store");
    let mut kept = before.clone();
    kept.retain(|p, _| p != &store && p.starts_with(&store));
    assert_eq!(tree(&store), kept, "the store is as it was");
    assert!(!has_backup(&root));
}

#[test]
fn ok11_cli_upgrade_reports_a_current_store_and_keeps_its_daemon() {
    let s = Served::new();
    let root = s.dir.path().to_path_buf();
    let before = tree(&root);
    // no daemon: current, with no daemon started, no file written and no warning
    for args in [vec!["upgrade"], vec!["upgrade", "--dry-run"]] {
        let out = s.rdy(&args);
        assert!(out.status.success(), "{out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "store is current\n");
        assert!(out.stderr.is_empty(), "{out:?}");
    }
    let (ok, v) = s.json(&["--json", "upgrade"]);
    assert!(ok, "{v}");
    assert_eq!(v["dry_run"], Value::Bool(false));
    assert_eq!(v["changed"], Value::Bool(false));
    assert_eq!(v["steps"], serde_json::json!([]));
    assert_eq!(v["receipt"], Value::Null);
    assert_eq!(v["from"], v["to"]);
    assert_eq!(v["from"].as_str().unwrap().len(), 64);
    assert_eq!(tree(&root), before, "nothing was written");
    assert!(!root.join(".rdy/run").exists(), "no daemon was started");
    // the directory argument, from elsewhere
    let elsewhere = tempfile::tempdir().unwrap();
    let out = Command::new(rdy_bin())
        .args(["--json", "upgrade", "--dry-run"])
        .arg(&root)
        .current_dir(elsewhere.path())
        .env_remove("RDY_NO_DAEMON")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let w: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(w["from"], v["from"]);
    assert_eq!(w["dry_run"], Value::Bool(true));
    // the current directory when neither is given
    let out = Command::new(rdy_bin())
        .args(["upgrade"])
        .current_dir(&root)
        .env_remove("RDY_NO_DAEMON")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "store is current\n");
    // a running daemon is left running: a current store needs no stop
    assert!(s.rdy(&["status"]).status.success());
    let pid = rdy_daemon::live_daemon(&root).expect("the daemon runs");
    let out = s.rdy(&["upgrade"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "store is current\n");
    assert_eq!(rdy_daemon::live_daemon(&root), Some(pid));
    // the help
    let out = Command::new(rdy_bin())
        .args(["upgrade", "--help"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .starts_with("Inspect or upgrade this project's store."),
        "{out:?}"
    );
}
