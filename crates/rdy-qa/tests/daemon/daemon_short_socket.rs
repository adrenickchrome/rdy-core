#![cfg(unix)]
// Unix only: it uses Unix sockets, owners and symlinks. FW-XP1 Phase 4 ports it.
//! df3 (F12, D-YMWEM): a project whose `.rdy/run/rdy.sock` is longer than `sun_path` allows
//! gets a short socket, `/tmp/rdy-<uid>/<hash>.sock`, in a directory only the user can read.
//! Every other run file stays in `.rdy/run/`.
use rdy_daemon::socket::own_uid;
use rdy_daemon::{Attempt, Hello, MAX_SOCKET_PATH, PROTO_VERSION, Proto, RunDir, connect};
use rdy_engine::Rdy;
use rdy_qa::daemon::{Served, human_command, short_tempdir};
use std::path::{Path, PathBuf};
use std::process::Output;

/// The demo's case: a sandbox or CI workspace path, nested, one component more.
fn long_project(base: &Path) -> PathBuf {
    let project = base
        .join("a-long-directory-name-like-a-sandbox-or-ci-workspace")
        .join("nested")
        .join("project")
        .join("checkout-01");
    std::fs::create_dir_all(&project).unwrap();
    drop(Rdy::init(&project, "rdy-standard").unwrap());
    let canonical = std::fs::canonicalize(&project).unwrap();
    let long = canonical.join(".rdy/run/rdy.sock");
    assert!(
        long.as_os_str().len() > MAX_SOCKET_PATH,
        "the case needs a socket path over the limit: {}",
        long.display()
    );
    project
}

/// Runs `rdy serve --stop` for `project` when dropped, so a failed assertion leaves no daemon.
struct StopOnDrop<'a>(&'a Path);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        let _ = human_command(self.0, &[], &["serve", "--stop"]).output();
    }
}

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The pid the project's daemon reports to `ping`.
fn ping_pid(project: &Path) -> u64 {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let hello = Hello {
            rdy: PROTO_VERSION,
            version: env!("CARGO_PKG_VERSION").into(),
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

#[test]
fn df3_long_socket_path() {
    let base = short_tempdir();
    let project = long_project(base.path());
    let _stop = StopOnDrop(&project);
    let out = human_command(&project, &[], &["--json", "status"])
        .output()
        .unwrap();
    ok(&out);
    let run = RunDir::of_project(&project);
    let sock = run.socket();
    assert!(
        sock.to_string_lossy().starts_with("/tmp/rdy-")
            && sock.as_os_str().len() <= MAX_SOCKET_PATH,
        "{}",
        sock.display()
    );
    assert_eq!(run.short_socket().as_ref(), Some(&sock));
    assert!(sock.exists(), "the daemon listens on {}", sock.display());
    assert!(project.join(".rdy/run/daemon.log").exists());
    assert_eq!(
        std::fs::read_to_string(project.join(".rdy/run/.gitignore")).unwrap(),
        "*\n"
    );
    assert!(!project.join(".rdy/run/rdy.sock").exists());
    ok(&human_command(&project, &[], &["serve", "--stop"])
        .output()
        .unwrap());
    assert!(!sock.exists(), "the stopped daemon left {}", sock.display());
}

/// Guard: a project whose `.rdy/run/rdy.sock` fits keeps it there, spelled with the canonical
/// root.
#[test]
fn df3_a_short_project_keeps_its_socket_in_rdy_run() {
    let s = Served::new();
    let canonical = std::fs::canonicalize(s.dir.path()).unwrap();
    assert_eq!(s.run_dir().socket(), canonical.join(".rdy/run/rdy.sock"));
    assert_eq!(s.run_dir().short_socket(), None);
}

/// Review Focus 2: `/tmp/x`, `/private/tmp/x` and a relative `--project` reach one socket and
/// one daemon.
#[test]
fn df3_every_spelling_of_a_long_project_path_reaches_one_socket() {
    let base = short_tempdir();
    let project = long_project(base.path());
    let _stop = StopOnDrop(&project);
    let sock = RunDir::of_project(&project).socket();
    assert_eq!(
        sock.parent().unwrap(),
        Path::new(&format!("/tmp/rdy-{}", own_uid()))
    );
    let canonical = std::fs::canonicalize(&project).unwrap();
    assert_eq!(RunDir::of_project(&canonical).socket(), sock);
    let tmp_is_a_link = std::fs::symlink_metadata("/tmp").is_ok_and(|m| m.file_type().is_symlink());
    let private = Path::new("/private").join(project.strip_prefix("/").unwrap());
    if tmp_is_a_link {
        assert_eq!(RunDir::of_project(&private).socket(), sock);
    }

    // the absolute spelling starts the daemon
    ok(&human_command(&project, &[], &["--json", "status"])
        .output()
        .unwrap());
    let pid = ping_pid(&project);

    // a relative --project from the project's parent's parent reaches it
    let grand = project.parent().unwrap().parent().unwrap();
    let relative = project.strip_prefix(grand).unwrap();
    let out = std::process::Command::new(rdy_qa::fixtures::rdy_bin())
        .current_dir(grand)
        .arg("--project")
        .arg(relative)
        .args(["--json", "status"])
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .output()
        .unwrap();
    ok(&out);
    assert_eq!(
        ping_pid(&project),
        pid,
        "a relative --project started another daemon"
    );

    // the /private spelling reaches it too
    if tmp_is_a_link {
        ok(&human_command(&private, &[], &["--json", "status"])
            .output()
            .unwrap());
        assert_eq!(
            ping_pid(&private),
            pid,
            "the /private spelling started another daemon"
        );
    }
    ok(&human_command(&project, &[], &["serve", "--stop"])
        .output()
        .unwrap());
    assert!(!sock.exists());
}

/// The `/System/Volumes/Data` spelling of `canonical` (a macOS firmlink that `fs::canonicalize`
/// leaves as it is), when it names the same directory under another string.
fn firmlink_spelling(canonical: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let data = Path::new("/System/Volumes/Data").join(canonical.strip_prefix("/").ok()?);
    let (a, b) = (
        std::fs::metadata(canonical).ok()?,
        std::fs::metadata(&data).ok()?,
    );
    let same = (a.dev(), a.ino()) == (b.dev(), b.ino());
    let unmerged = std::fs::canonicalize(&data).ok()? != canonical;
    (same && unmerged).then_some(data)
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
fn one_daemon_through(project: &Path, first: &Path, second: &Path) {
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

/// Fix round 1 (ruling 2): every spelling of one project reaches one daemon, also a spelling
/// that `fs::canonicalize` does not merge (the macOS `/System/Volumes/Data` firmlink), because
/// the daemon records where it bound and clients connect there.
#[test]
fn df3_two_spellings_of_one_project_reach_one_daemon() {
    // a symlink: canonicalize merges it
    let base = short_tempdir();
    let project = long_project(base.path());
    let _stop = StopOnDrop(&project);
    let link = base.path().join("link");
    std::os::unix::fs::symlink(&project, &link).unwrap();
    one_daemon_through(&project, &link, &project);

    // the firmlink spelling of a long project: both spellings get a short socket, keyed by
    // different strings
    let canonical = std::fs::canonicalize(&project).unwrap();
    if let Some(data) = firmlink_spelling(&canonical) {
        one_daemon_through(&project, &data, &canonical);
    }

    // at the length limit: the canonical spelling fits .rdy/run/rdy.sock, the firmlink one does
    // not
    let base = short_tempdir();
    let canonical_base = std::fs::canonicalize(base.path()).unwrap();
    let pad = MAX_SOCKET_PATH - canonical_base.as_os_str().len() - 1 - "/.rdy/run/rdy.sock".len();
    let edge = canonical_base.join("p".repeat(pad));
    std::fs::create_dir(&edge).unwrap();
    drop(Rdy::init(&edge, "rdy-standard").unwrap());
    assert_eq!(
        edge.join(".rdy/run/rdy.sock").as_os_str().len(),
        MAX_SOCKET_PATH
    );
    let _stop_edge = StopOnDrop(&edge);
    if let Some(data) = firmlink_spelling(&edge) {
        one_daemon_through(&edge, &data, &edge);
    }
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
    let dead = base.path().join("dead.sock");
    drop(std::os::unix::net::UnixListener::bind(&dead).unwrap());
    let id = run.project_id().unwrap();
    std::fs::write(run.socket_record(), format!("{}\n{id}\n", dead.display())).unwrap();
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
        let _ = std::process::Command::new("cp")
            .arg("-R")
            .arg(&original)
            .arg(&copy)
            .output()
            .unwrap();
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
struct KillOnDrop(u32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if rdy_daemon::process_alive(self.0) {
            let _ = std::process::Command::new("kill")
                .arg(self.0.to_string())
                .output();
        }
    }
}

/// Final ruling (c) + (a), the reviewer's probe: a long-path project (its socket is the short
/// one in `/tmp/rdy-<uid>`) is moved away, copied back with `cp -R` and the old copy removed,
/// while its daemon lives. That daemon's `.rdy` is gone, so the restored project's `status`
/// makes it refuse (stopping) and exit; the client retries once and starts the restored
/// project's own daemon within its start poll. The old process exits.
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
    std::fs::rename(&project, &away).unwrap();
    let copied = std::process::Command::new("cp")
        .arg("-R")
        .arg(&away)
        .arg(&project)
        .output()
        .unwrap();
    assert!(copied.status.success(), "{copied:?}");
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
    assert!(sock.starts_with("/tmp"), "{}", sock.display());
    assert!(sock.exists());
    std::fs::remove_dir_all(&project).unwrap();
    run.handle().tick();
    assert_eq!(run.wait().unwrap(), rdy_daemon::ExitReason::ProjectGone);
    assert!(
        !sock.exists(),
        "the gone project's daemon left {}",
        sock.display()
    );
}
