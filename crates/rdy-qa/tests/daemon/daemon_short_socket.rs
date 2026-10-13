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
