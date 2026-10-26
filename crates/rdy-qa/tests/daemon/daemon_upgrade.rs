//! ok11: store maintenance around the daemon (D-WWAK7, D-1HDVA). `rdy upgrade` on a directory
//! plans first, stops the project's daemon only when there is work, backs up, appends one batch,
//! reindexes and checks; an ordinary connection completes a pending upgrade (full fsck, then
//! `FORMAT`) before it starts a daemon. The steps here are an injected registry that adds the
//! registered `assessment` feature, so every rdy of this build opens the upgraded store.
use rdy_daemon::maintenance::run_with_registry_for_tests;
use rdy_daemon::{
    ErrorReply, Proto, RunDir, StartOptions, connect_or_start, live_daemon, serve_command,
    spawn_serve,
};
use rdy_engine::{
    CompletionPhase, E_UPGRADE_FAILED, E_UPGRADE_MOVED, Rdy, Session, SessionKind, WriteRequest,
    with_completion_hook_for_tests,
};
use rdy_model::{Hash, Provenance, Role, Tag};
use rdy_qa::daemon::{Served, exited, short_tempdir};
use rdy_qa::fixtures::rdy_bin;
use rdy_store::format::{
    FORMAT_FILE, StoreFeature, StoreFeatures, baseline_features, registered_features,
};
use rdy_store::upgrade::{self, UpgradeStep};
use rdy_store::{OpenPolicy, Store};
use serde_json::json;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Barrier, Mutex};

/// Every path under `dir` with its length and contents (None for a directory).
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

fn assessment() -> StoreFeature {
    registered_features()
        .into_iter()
        .find(|f| f.name == "assessment")
        .expect("assessment is registered")
}

/// One step: from any manifest to one with `assessment`.
fn registry() -> Vec<UpgradeStep> {
    vec![UpgradeStep {
        id: "add_assessment".into(),
        from: StoreFeatures::of(vec![]),
        to: StoreFeatures::of(vec![assessment()]),
    }]
}

fn upgraded() -> StoreFeatures {
    baseline_features().merged(&[assessment()])
}

fn store_dir(root: &Path) -> PathBuf {
    root.join(".rdy").join("store")
}

fn format_text(root: &Path) -> String {
    std::fs::read_to_string(store_dir(root).join(FORMAT_FILE)).unwrap()
}

/// The `meta/upgrade` frames in the store's raw segments: one per upgrade batch.
fn upgrade_batches(root: &Path) -> usize {
    let mut n = 0;
    for e in std::fs::read_dir(store_dir(root).join("segments")).unwrap() {
        let bytes = std::fs::read(e.unwrap().path()).unwrap();
        n += bytes
            .windows(b"meta/upgrade".len())
            .filter(|w| *w == b"meta/upgrade")
            .count();
    }
    n
}

/// The upgrade backups beside the store.
fn backups(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(root.join(".rdy"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(upgrade::BACKUP_PREFIX))
        .collect();
    out.sort();
    out
}

/// A phase of the upgrade, with whether the daemon had exited by the first copy.
type Seen = (String, Option<bool>);

/// A closed project at `<base>/p`.
fn closed_project() -> (tempfile::TempDir, PathBuf) {
    let base = short_tempdir();
    let root = base.path().join("p");
    std::fs::create_dir(&root).unwrap();
    Rdy::init(&root, "rdy-standard").unwrap().close().unwrap();
    (base, root)
}

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

/// Lands one doc in the closed project at `root`; returns the landing.
fn land_a_doc(root: &Path) -> Hash {
    let mut r = Rdy::open(root).unwrap();
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(json!([
                {"op": "create", "type": "doc", "fields": {"title": "Doc", "doc_type": "page"}}
            ]))
            .unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    let p = r
        .propose(&session(Role::Writer), "work", "Doc", Some("a summary"))
        .unwrap();
    let landing = r
        .land(&session(Role::Approver), &p.id)
        .unwrap()
        .parse()
        .unwrap();
    r.close().unwrap();
    landing
}

/// Leaves the project's upgrade sealed and its completion pending: the upgrade stops after its
/// store check, before the engine ever runs (a kill there).
fn make_pending(root: &Path) {
    let dir = store_dir(root);
    let stop = upgrade::with_hook_for_tests(
        |p| match p {
            upgrade::Phase::Checked => Err(std::io::Error::other("stopped after the check")),
            _ => Ok(()),
        },
        || Store::open_with_registry_for_tests(&dir, OpenPolicy::AutoUpgrade, &registry()),
    );
    assert!(stop.is_err());
    let plan = upgrade::inspect(&dir).unwrap();
    assert!(plan.pending_check && plan.steps.is_empty(), "{plan:?}");
    assert_eq!(upgrade_batches(root), 1);
}

#[test]
fn ok11_dry_run_has_no_daemon_or_disk_effect() {
    // a closed project: the dry run lists the step and changes no byte, makes no lock, backup,
    // index or run directory, and starts no daemon
    let (base, root) = closed_project();
    let before = tree(base.path());
    let r = run_with_registry_for_tests(&root, true, &registry()).unwrap();
    assert_eq!(
        (
            r.dry_run,
            r.changed,
            r.steps.clone(),
            r.backup.clone(),
            r.batch,
            r.reindexed,
            r.checked
        ),
        (
            true,
            false,
            vec!["add_assessment".to_string()],
            None,
            None,
            false,
            false
        )
    );
    assert_eq!(
        (r.from.clone(), r.to.clone()),
        (baseline_features(), upgraded())
    );
    assert_eq!(tree(base.path()), before, "no byte changed");
    assert!(!root.join(".rdy").join("run").exists(), "no run directory");
    assert!(live_daemon(&root).is_none(), "no daemon started");
    // this rdy's own registry: the store is current
    let r = rdy_daemon::maintenance::run(&root, true).unwrap();
    assert!(r.steps.is_empty() && !r.changed, "{r:?}");
    assert_eq!(tree(base.path()), before, "no byte changed");

    // a running daemon is left running, and its store as it is
    let s = Served::new();
    assert!(s.rdy(&["status"]).status.success());
    let root = s.dir.path().to_path_buf();
    let pid = live_daemon(&root).expect("the daemon runs");
    let segments = tree(&store_dir(&root).join("segments"));
    let format = format_text(&root);
    let r = run_with_registry_for_tests(&root, true, &registry()).unwrap();
    assert_eq!(r.steps, ["add_assessment"]);
    assert_eq!(
        live_daemon(&root),
        Some(pid),
        "the same daemon still serves"
    );
    assert!(!exited(pid));
    assert_eq!(tree(&store_dir(&root).join("segments")), segments);
    assert_eq!(format_text(&root), format);
    assert!(backups(&root).is_empty());
}

#[test]
fn ok11_apply_stops_before_backup() {
    let s = Served::new();
    assert!(s.rdy(&["status"]).status.success());
    let root = s.dir.path().to_path_buf();
    let pid = live_daemon(&root).expect("the daemon runs");
    let old = format_text(&root);
    // the copy barrier: each phase of the upgrade is recorded, the first copy with whether the
    // daemon's process had exited by then
    let seen: Rc<RefCell<Vec<Seen>>> = Rc::default();
    let rec = Rc::clone(&seen);
    let r = upgrade::with_hook_for_tests(
        move |p| {
            let mut v = rec.borrow_mut();
            let first_copy =
                matches!(p, upgrade::Phase::Copy(_)) && !v.iter().any(|(n, _)| n == "copy");
            v.push((p.name().to_string(), first_copy.then(|| exited(pid))));
            Ok(())
        },
        || run_with_registry_for_tests(&root, false, &registry()),
    )
    .unwrap();
    let seen = seen.borrow().clone();
    assert_eq!(
        seen.first(),
        Some(&("copy".to_string(), Some(true))),
        "the daemon exited before the first byte was copied: {seen:?}"
    );
    let order: Vec<&str> = seen
        .iter()
        .map(|(n, _)| n.as_str())
        .filter(|n| !matches!(*n, "copy" | "sync"))
        .collect();
    assert_eq!(
        order,
        [
            "backup_complete",
            "opened",
            "sealed",
            "reindexed",
            "checked"
        ]
    );
    // the completed report: backup, one batch, reindex, check and the receipt
    let names = backups(&root);
    assert_eq!(names.len(), 1);
    let backup = r.backup.clone().expect("a backup");
    assert_eq!(backup.file_name().unwrap().to_string_lossy(), names[0]);
    assert!(backup.exists());
    assert!(r.changed && r.reindexed && r.checked && !r.dry_run, "{r:?}");
    assert!(r.batch.is_some());
    assert_eq!(r.steps, ["add_assessment"]);
    assert_eq!(
        (r.from.clone(), r.to.clone()),
        (baseline_features(), upgraded())
    );
    let receipt = r.receipt.clone().expect("the receipt");
    assert_eq!(receipt.steps, ["add_assessment"]);
    assert_eq!(receipt.backup, names[0]);
    assert_eq!(
        (receipt.from, receipt.to),
        (baseline_features().hash(), upgraded().hash())
    );
    assert_ne!(format_text(&root), old);
    assert_eq!(format_text(&root), upgraded().to_text());
    assert_eq!(upgrade_batches(&root), 1);
    // the old daemon is not restarted; the next ordinary connection starts this rdy's
    assert!(live_daemon(&root).is_none());
    assert!(s.rdy(&["status"]).status.success());
    let next = live_daemon(&root).expect("a new daemon");
    assert_ne!(next, pid);
}

#[test]
fn ok11_start_upgrades_before_serving() {
    for case in ["passes", "fsck_fails"] {
        let s = Served::new();
        let root = s.dir.path().to_path_buf();
        if case == "fsck_fails" {
            // a tag the full fsck refuses, which the store check alone lets through
            let landing = land_a_doc(&root);
            let mut r = Rdy::open(&root).unwrap();
            r.store_mut()
                .put_tag(&Tag {
                    name: "release-1".into(),
                    landing,
                    message: None,
                    tagged_by: Provenance {
                        actor: "x".into(),
                        run: None,
                        role: Role::Approver,
                        at_ms: 0,
                    },
                })
                .unwrap();
            r.seal_open().unwrap();
            assert!(r.fsck().is_err(), "fsck fails");
            r.close().unwrap();
        }
        make_pending(&root);
        let old = format_text(&root);
        let events: Arc<Mutex<Vec<String>>> = Arc::default();
        let spawned = Arc::clone(&events);
        let env = s.env_pairs();
        let project = root.clone();
        let opts = StartOptions::new(
            env!("CARGO_PKG_VERSION").to_string(),
            Proto::Ctl,
            root.clone(),
            Box::new(move |run| {
                spawned.lock().unwrap().push("spawn".into());
                let mut cmd = serve_command(&rdy_bin(), &project, run)?;
                cmd.envs(env.iter().map(|(k, v)| (k, v)));
                spawn_serve(&mut cmd)
            }),
        );
        let run = RunDir::of_project(&root);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let phases = Arc::clone(&events);
        let at = root.clone();
        let mirror = old.clone();
        let out = with_completion_hook_for_tests(
            move |p: CompletionPhase| {
                let f = if format_text(&at) == mirror {
                    "old"
                } else {
                    "new"
                };
                phases.lock().unwrap().push(format!("{p:?} {f}"));
                Ok(())
            },
            || {
                rt.block_on(async {
                    let mut c = connect_or_start(&run, opts).await?;
                    c.ctl("session", json!({})).await?;
                    Ok::<_, ErrorReply>(c)
                })
            },
        );
        let events = events.lock().unwrap().clone();
        match case {
            "passes" => {
                let c = out.unwrap_or_else(|e| panic!("{e:?}"));
                c.close();
                assert_eq!(
                    events,
                    ["BeforeFsck old", "Checked old", "Published new", "spawn"],
                    "the full fsck, then FORMAT, then the daemon and its sessions"
                );
                assert_eq!(format_text(&root), upgraded().to_text());
                assert_eq!(upgrade_batches(&root), 1, "no second batch");
                assert_eq!(backups(&root).len(), 1, "no second backup");
                assert!(live_daemon(&root).is_some());
            }
            _ => {
                let e = out.expect_err("the completion fails");
                let names = backups(&root);
                assert_eq!(names.len(), 1, "the backup is kept");
                assert!(root.join(".rdy").join(&names[0]).is_dir());
                assert_eq!(
                    (e.code.as_str(), e.message.clone(), e.fix.as_str()),
                    (
                        E_UPGRADE_FAILED,
                        format!(
                            "the upgrade batch is durable, but the store check failed; backup: {}",
                            names[0]
                        ),
                        "stop the daemon and restore the backup before retrying"
                    )
                );
                assert_eq!(events, ["BeforeFsck old"], "no daemon, no session");
                assert_eq!(format_text(&root), old, "FORMAT stays old");
                assert_eq!(upgrade_batches(&root), 1);
                assert!(live_daemon(&root).is_none(), "no daemon serves");
            }
        }
    }
}

#[test]
fn ok11_two_upgraders_seal_once() {
    let (_base, root) = closed_project();
    let start = Arc::new(Barrier::new(2));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let root = root.clone();
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                run_with_registry_for_tests(&root, false, &registry())
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    let changed = results
        .iter()
        .filter(|r| r.as_ref().is_ok_and(|r| r.changed))
        .count();
    assert_eq!(changed, 1, "{results:?}");
    for r in &results {
        match r {
            Ok(r) if r.changed => assert_eq!(r.steps, ["add_assessment"]),
            // the second one found the store upgraded under the lock, or planned the old
            // store before the first one sealed
            Ok(r) => assert!(r.steps.is_empty() && r.receipt.is_some(), "{r:?}"),
            Err(e) => assert_eq!(e.code, E_UPGRADE_MOVED, "{e:?}"),
        }
    }
    assert_eq!(upgrade_batches(&root), 1, "one batch");
    assert_eq!(backups(&root).len(), 1, "one backup");
    assert_eq!(format_text(&root), upgraded().to_text());
    // a third attempt has nothing to do
    let r = run_with_registry_for_tests(&root, false, &registry()).unwrap();
    assert!(
        !r.changed && r.steps.is_empty() && r.receipt.is_some(),
        "{r:?}"
    );
    assert_eq!(upgrade_batches(&root), 1);
}
