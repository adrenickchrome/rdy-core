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
    std::fs::write(run.socket_record(), format!("{}\n", dead.display())).unwrap();
    ok(&human_command(&project, &[], &["--json", "status"])
        .output()
        .unwrap());
    assert_eq!(
        std::fs::read_to_string(run.socket_record()).unwrap(),
        format!("{}\n", run.socket().display()),
        "the new daemon rewrites the record"
    );
    assert_eq!(run.recorded_socket(), Some(run.socket()));
    ping_pid(&project);
    ok(&human_command(&project, &[], &["serve", "--stop"])
        .output()
        .unwrap());
    assert!(!run.socket_record().exists());
}
