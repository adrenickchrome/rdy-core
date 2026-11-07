//! df3 (D-YMWEM): a project keeps its own daemon however its directory reaches it: a stale record,
//! a copy, a restore and a deleted project. The socket path's own rules are Unix only
//! (`daemon_short_socket_unix`).
use rdy_daemon::{Attempt, Hello, PROTO_VERSION, Proto, RunDir, connect};
use rdy_engine::Rdy;
use rdy_qa::daemon::{
    dead_endpoint, endpoint_live, human_command, short_tempdir, wait_endpoint_gone,
};
use std::path::{Path, PathBuf};
use std::process::Output;

/// The demo's case: a sandbox or CI workspace path, nested, one component more.
pub(crate) fn long_project(base: &Path) -> PathBuf {
    let project = base
        .join("a-long-directory-name-like-a-sandbox-or-ci-workspace")
        .join("nested")
        .join("project")
        .join("checkout-01");
    std::fs::create_dir_all(&project).unwrap();
    drop(Rdy::init(&project, "rdy-standard").unwrap());
    #[cfg(unix)]
    {
        let canonical = std::fs::canonicalize(&project).unwrap();
        let long = canonical.join(".rdy/run/rdy.sock");
        assert!(
            long.as_os_str().len() > rdy_daemon::MAX_SOCKET_PATH,
            "the case needs a socket path over the limit: {}",
            long.display()
        );
    }
    project
}

/// Runs `rdy serve --stop` for `project` when dropped, so a failed assertion leaves no daemon.
pub(crate) struct StopOnDrop<'a>(pub(crate) &'a Path);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        let _ = human_command(self.0, &[], &["serve", "--stop"]).output();
    }
}

pub(crate) fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The pid the project's daemon reports to `ping`.
pub(crate) fn ping_pid(project: &Path) -> u64 {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let hello = Hello {
            rdy: PROTO_VERSION,
            version: env!("CARGO_PKG_VERSION").into(),
            build: rdy_daemon::BUILD.to_string(),
            proto: Proto::Ctl,
            cwd: project.into(),
            project: RunDir::of_project(project).project_id().unwrap(),
        };
        let Attempt::Ready(mut c) = connect(&RunDir::of_project(project), &hello).await else {
            panic!(
                "no daemon answers on {}",
                RunDir::of_project(project).socket().display()
            )
        };
        let pid = c.ctl("ping", serde_json::json!({})).await.unwrap()["pid"]
            .as_u64()
            .unwrap();
        c.close();
        pid
    })
}

/// How many daemons started for `project`: the "opening store" lines of its daemon.log.
fn starts(project: &Path) -> usize {
    std::fs::read_to_string(project.join(".rdy/run/daemon.log"))
        .unwrap_or_default()
        .matches("opening store")
        .count()
}

/// Starts the daemon through `first`, then checks that `status` through `second` reaches the same
/// daemon (same pid) with no second spawn, and stops it.
pub(crate) fn one_daemon_through(project: &Path, first: &Path, second: &Path) {
    let before = starts(project);
    ok(&human_command(first, &[], &["--json", "status"])
        .output()
        .unwrap());
    let pid = ping_pid(first);
    ok(&human_command(second, &[], &["--json", "status"])
        .output()
        .unwrap());
    assert_eq!(
        ping_pid(second),
        pid,
        "{} reached another daemon than {}",
        second.display(),
        first.display()
    );
    assert_eq!(starts(project), before + 1, "a second daemon was spawned");
    let record = project.join(".rdy/run/daemon.sock");
    assert!(record.exists(), "the daemon records its socket");
    ok(&human_command(second, &[], &["serve", "--stop"])
        .output()
        .unwrap());
    assert!(!record.exists(), "a clean exit removes the record");
}

/// Fix round 1 (ruling 2): a record that names a socket nobody answers on is stale: the client
/// computes its own socket, starts a daemon, and that daemon rewrites the record.
#[test]
fn df3_a_stale_socket_record_is_recomputed_and_rewritten() {
    let base = short_tempdir();
    let project = base.path().join("p");
    std::fs::create_dir(&project).unwrap();
    drop(Rdy::init(&project, "rdy-standard").unwrap());
    let _stop = StopOnDrop(&project);
    let run = RunDir::of_project(&project);
    run.ensure().unwrap();
    let dead = dead_endpoint(base.path());
    let id = run.project_id().unwrap();
    rdy_store::fsutil::write_private_durable(
        &run.socket_record(),
        format!("{}\n{id}\n", dead.display()).as_bytes(),
    )
    .unwrap();
    ok(&human_command(&project, &[], &["--json", "status"])
        .output()
        .unwrap());
    assert_eq!(
        std::fs::read_to_string(run.socket_record()).unwrap(),
        format!("{}\n{id}\n", run.socket().display()),
        "the new daemon rewrites the record"
    );
    assert_eq!(run.recorded_socket(), Some(run.socket()));
    ping_pid(&project);
    ok(&human_command(&project, &[], &["serve", "--stop"])
        .output()
        .unwrap());
    assert!(!run.socket_record().exists());
}

/// A person's copy of a project: `cp -R` on Unix, where it may complain about the socket file. On
/// Windows, what the running daemon holds stays behind, and the run files stay private to the
/// account, as a copy made by that account can.
#[cfg(unix)]
fn copy_project(from: &Path, to: &Path) -> bool {
    std::process::Command::new("cp")
        .arg("-R")
        .arg(from)
        .arg(to)
        .output()
        .unwrap()
        .status
        .success()
}
#[cfg(windows)]
pub(crate) fn copy_project(from: &Path, to: &Path) -> bool {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if e.file_type().unwrap().is_dir() {
            copy_project(&src, &dst);
        } else if let Ok(bytes) = std::fs::read(&src) {
            if from.ends_with(".rdy/run") {
                rdy_store::fsutil::write_private_durable(&dst, &bytes).unwrap();
            } else {
                std::fs::write(&dst, bytes).unwrap();
            }
        }
    }
    true
}

/// Whether `rdy ls --scope work` in `project` lists the folder `path` (`--no-daemon` with
/// `direct`).
fn work_lists(project: &Path, path: &str, direct: bool) -> bool {
    let mut args = vec!["--json", "ls", "--scope", "work"];
    if direct {
        args.insert(0, "--no-daemon");
    }
    let out = human_command(project, &[], &args).output().unwrap();
    ok(&out);
    let entries: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    let path = path.trim_start_matches('/');
    entries.iter().any(|e| e["path"] == path)
}

/// Fix round 2 (ruling task-3-round1), the reviewer's probe: the original's daemon is live, and
/// a copy made with `cp -R` carries its `.rdy/run/daemon.sock`, which names the original's
/// socket. The copy's first write lands in the copy, and the original is untouched. The record
/// names the original's `.rdy` (`dev:ino`), so the copy never follows it, also while its own
/// daemon has the store and has not bound yet. Ten fresh copies.
#[test]
fn df3_a_copied_project_never_talks_to_the_originals_daemon() {
    let base = short_tempdir();
    let original = base.path().join("orig");
    std::fs::create_dir(&original).unwrap();
    drop(Rdy::init(&original, "rdy-standard").unwrap());
    let _stop = StopOnDrop(&original);
    ok(&human_command(&original, &[], &["new", "folder", "/orig"])
        .output()
        .unwrap());
    let pid = ping_pid(&original);
    assert!(RunDir::of_project(&original).socket_record().exists());
    assert!(work_lists(&original, "/orig", false));
    for n in 1..=10 {
        let copy = base.path().join(format!("c{n}"));
        // cp may complain about the socket file; the record must reach the copy
        let _ = copy_project(&original, &copy);
        assert!(
            copy.join(".rdy/run/daemon.sock").exists(),
            "the copy carries the original's record"
        );
        let _stop_copy = StopOnDrop(&copy);
        let folder = format!("/only-c{n}");
        ok(&human_command(&copy, &[], &["new", "folder", &folder])
            .output()
            .unwrap());
        assert_ne!(
            ping_pid(&copy),
            pid,
            "copy {n} reached the original's daemon"
        );
        ok(&human_command(&copy, &[], &["serve", "--stop"])
            .output()
            .unwrap());
        assert!(
            work_lists(&copy, &folder, true),
            "copy {n}'s write is not in the copy"
        );
        assert!(
            !work_lists(&original, &folder, false),
            "copy {n}'s write landed in the original"
        );
        assert_eq!(ping_pid(&original), pid, "the original's daemon changed");
    }
    ok(&human_command(&original, &[], &["serve", "--stop"])
        .output()
        .unwrap());
}

/// Ends process `pid` when dropped, so a failed assertion leaves no orphan daemon.
pub(crate) struct KillOnDrop(pub(crate) u32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if rdy_daemon::process_alive(self.0) {
            rdy_qa::platform::kill_hard(self.0);
        }
    }
}

/// A person moves a project away, whole. Windows refuses to rename a directory with an open file
/// below it (code 5, however the file is shared), so a move there is what `Move-Item` then does: a
/// copy and a delete.
fn move_project_away(project: &Path, away: &Path) {
    if cfg!(windows) {
        assert!(copy_project(project, away), "the copy failed");
        std::fs::remove_dir_all(project).unwrap();
    } else {
        std::fs::rename(project, away).unwrap();
    }
}

/// Final ruling (c) + (a), the reviewer's probe: a long-path project (on Unix its socket is the
/// short one in `/tmp/rdy-<uid>`) is moved away, copied back and the old copy removed, while its
/// daemon lives. That daemon's `.rdy` is gone, so the restored project's `status` makes it refuse
/// (stopping) and exit; the client retries once and starts the restored project's own daemon
/// within its start poll. The old process exits.
#[test]
fn df3_a_restored_project_replaces_its_old_daemon() {
    let base = short_tempdir();
    let project = long_project(base.path());
    let _stop = StopOnDrop(&project);
    ok(&human_command(&project, &[], &["--json", "status"])
        .output()
        .unwrap());
    let old = ping_pid(&project) as u32;
    let _kill = KillOnDrop(old);
    let away = base.path().join("away");
    move_project_away(&project, &away);
    assert!(copy_project(&away, &project), "the copy back failed");
    std::fs::remove_dir_all(&away).unwrap();
    ok(&human_command(&project, &[], &["--json", "status"])
        .output()
        .unwrap());
    let new = ping_pid(&project) as u32;
    assert_ne!(new, old, "the restored project reached its old daemon");
    // the old daemon drains (no connections) and exits; bounded, polled
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while rdy_daemon::process_alive(old) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        !rdy_daemon::process_alive(old),
        "the old daemon {old} lives on"
    );
    ok(&human_command(&project, &[], &["serve", "--stop"])
        .output()
        .unwrap());
}

/// A person deletes a project while its real daemon lives. The delete succeeds, whatever the daemon
/// holds, and no client comes: the daemon's own idle tick finds the project gone, and it ends.
#[test]
fn wx25c_deleting_a_project_stops_its_daemon() {
    let base = short_tempdir();
    let project = long_project(base.path());
    let _stop = StopOnDrop(&project);
    ok(&human_command(&project, &[], &["--json", "status"])
        .output()
        .unwrap());
    let daemon = ping_pid(&project) as u32;
    let _kill = KillOnDrop(daemon);
    let endpoint = RunDir::of_project(&project);
    std::fs::remove_dir_all(&project).unwrap();
    assert!(!project.exists());
    // a bound on the wait, for a failure message: never a measure
    let until = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while rdy_daemon::process_alive(daemon) {
        assert!(
            std::time::Instant::now() < until,
            "the daemon {daemon} lives on"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    wait_endpoint_gone(&endpoint);
}

/// D-6JZ5V: on every platform the project check stops a daemon whose store or its lock file is
/// gone, though `.rdy` stands. The check is the existing one (`check_project` is what the idle tick
/// calls), with no timer of its own.
#[test]
fn wx25c_a_store_gone_under_a_standing_rdy_dir_stops_the_daemon() {
    let dir = short_tempdir();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let run = rdy_daemon::start(rdy_daemon::ServeOptions {
        project: dir.path().to_path_buf(),
        version: env!("CARGO_PKG_VERSION").into(),
        detach: false,
        idle_timeout: None,
        human_actor: None,
        site_lan_override: None,
    })
    .unwrap();
    assert!(!run.handle().check_project(), "its store stands");
    std::fs::remove_dir_all(dir.path().join(".rdy").join("store")).unwrap();
    assert!(dir.path().join(".rdy").is_dir());
    assert!(run.handle().check_project());
    assert_eq!(run.wait().unwrap(), rdy_daemon::ExitReason::ProjectGone);
}

/// Final ruling (c): a daemon whose project is deleted exits on its next idle tick (driven
/// through the test hook, no sleep) and removes the short socket it bound.
#[test]
fn df3_a_daemon_exits_when_its_project_is_deleted() {
    let base = short_tempdir();
    let project = long_project(base.path());
    let run = rdy_daemon::start(rdy_daemon::ServeOptions {
        project: project.clone(),
        version: "t".into(),
        detach: false,
        idle_timeout: None,
        human_actor: None,
        site_lan_override: None,
    })
    .unwrap();
    let sock = run.handle().socket();
    #[cfg(unix)]
    assert!(sock.starts_with("/tmp"), "{}", sock.display());
    let endpoint = RunDir::of_project(&project);
    assert_eq!(endpoint.socket(), sock);
    assert!(endpoint_live(&endpoint));
    std::fs::remove_dir_all(&project).unwrap();
    run.handle().tick();
    assert_eq!(run.wait().unwrap(), rdy_daemon::ExitReason::ProjectGone);
    wait_endpoint_gone(&endpoint);
}
