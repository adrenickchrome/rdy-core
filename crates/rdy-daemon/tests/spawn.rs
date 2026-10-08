//! The spawn command and detach (spawn1).
mod common;
use common::*;
use rdy_daemon::*;
use std::ffi::OsStr;
use std::sync::{Mutex, MutexGuard};

/// Every test in this file holds this while it runs: one of them sets environment variables and
/// others read the environment (`serve_command`), and the harness runs tests on parallel threads.
static ENV: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV.lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
fn spawn1_the_serve_command_has_the_right_shape_and_no_rdy_env() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let _env = env_lock();
    // SAFETY: no other test of this binary runs while `_env` is held (each takes it first), and
    // nothing else in this process reads the environment meanwhile
    unsafe {
        std::env::set_var("RDY_TEST_HOOKS", "1");
        std::env::set_var("RDY_QA_DEEP", "1");
        std::env::set_var("RDY_BIN", "/x");
        std::env::set_var("NOT_RDY", "keep");
    }
    let cmd = serve_command(std::path::Path::new("/usr/local/bin/rdy"), dir.path(), &run).unwrap();
    assert_eq!(cmd.get_program(), OsStr::new("/usr/local/bin/rdy"));
    let args: Vec<&OsStr> = cmd.get_args().collect();
    assert_eq!(
        args,
        vec![
            OsStr::new("serve"),
            OsStr::new("--detach"),
            // fix forward (cli2): the spawned child serves in place rather than spawning again
            OsStr::new("--spawned"),
            OsStr::new("--project"),
            dir.path().as_os_str()
        ]
    );
    assert_eq!(cmd.get_current_dir(), Some(dir.path()));
    let removed: Vec<&OsStr> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k)
        .collect();
    for k in ["RDY_TEST_HOOKS", "RDY_QA_DEEP", "RDY_BIN"] {
        assert!(
            removed.contains(&OsStr::new(k)),
            "{k} not removed: {removed:?}"
        );
    }
    assert!(!removed.contains(&OsStr::new("NOT_RDY")));
    assert!(
        run.log_file().exists(),
        "the log file is created for the child's stdio"
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(run.log_file())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    unsafe {
        std::env::remove_var("RDY_TEST_HOOKS");
        std::env::remove_var("RDY_QA_DEEP");
        std::env::remove_var("RDY_BIN");
        std::env::remove_var("NOT_RDY");
    }
}

#[test]
fn spawn1_a_spawned_child_writes_to_the_log_in_its_own_process_group() {
    let _env = env_lock();
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    // a script stands in for the rdy binary: it ignores `serve --detach --project …` and reports itself
    let fake = dir.path().join("fake-rdy");
    std::fs::write(
        &fake,
        "#!/bin/sh\necho child-$$ >&2\nps -o pgid= -p $$ >&2\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cmd = serve_command(&fake, dir.path(), &run).unwrap();
    spawn_serve(&mut cmd).unwrap();
    // poll until the child has written both lines (its name, then its pgid); a fixed sleep flaked
    // under load (two tracks building on one machine); 60 s: it took 24.7 s while another worktree
    // built
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let text = loop {
        let text = std::fs::read_to_string(run.log_file()).unwrap_or_default();
        let done = text.contains("child-")
            && text
                .lines()
                .last()
                .is_some_and(|l| l.trim().parse::<i32>().is_ok());
        if done || std::time::Instant::now() > deadline {
            break text;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert!(text.contains("child-"), "{text}");
    let pgid: i32 = text.lines().last().unwrap().trim().parse().unwrap();
    assert_ne!(
        pgid,
        rdy_daemon::spawn::own_pgid(),
        "the child is not in our process group"
    );
}

#[test]
fn spawn1_detach_makes_a_new_session_in_a_child() {
    let _env = env_lock();
    if std::env::var("RDY_SPAWN_CHILD").is_ok() {
        detach().unwrap();
        println!("sid={}", rdy_daemon::spawn::own_sid());
        return;
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "spawn1_detach_makes_a_new_session_in_a_child",
            "--exact",
            "--nocapture",
        ])
        .env("RDY_SPAWN_CHILD", "1")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let sid: i32 = text
        .lines()
        .find_map(|l| l.strip_prefix("sid="))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_ne!(
        sid,
        rdy_daemon::spawn::own_sid(),
        "the child left our session"
    );
}

/// What `rdy serve --detach` does once spawned: `detach()`, then it must lead a session of its
/// own. A child made a process-group leader by `process_group(0)` cannot `setsid` (EPERM), so
/// its `detach` silently stayed in the spawner's session.
#[test]
fn spawn1_a_spawned_serve_child_detaches_into_a_session_of_its_own() {
    let _env = env_lock();
    const PROBE: &str = "SPAWN1_DETACH_PROBE";
    if std::env::var(PROBE).is_ok() {
        detach().unwrap();
        eprintln!(
            "probe pid={} sid={}",
            std::process::id(),
            rdy_daemon::spawn::own_sid()
        );
        return;
    }
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    // the fake rdy re-runs this test binary as the probe; `serve --detach --project …` is ignored
    let exe = std::env::current_exe().unwrap();
    let fake = dir.path().join("fake-rdy");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\n{PROBE}=1 exec '{}' spawn1_a_spawned_serve_child_detaches_into_a_session_of_its_own --exact --nocapture\n",
            exe.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cmd = serve_command(&fake, dir.path(), &run).unwrap();
    spawn_serve(&mut cmd).unwrap();
    let t0 = std::time::Instant::now();
    let line = loop {
        let text = std::fs::read_to_string(run.log_file()).unwrap_or_default();
        if let Some(l) = text.lines().find(|l| l.starts_with("probe pid=")) {
            break l.to_string();
        }
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(60),
            "no probe line: {text}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let field = |k: &str| -> i32 {
        line.split_whitespace()
            .find_map(|w| w.strip_prefix(k))
            .unwrap()
            .parse()
            .unwrap()
    };
    let (pid, sid) = (field("pid="), field("sid="));
    assert_ne!(
        sid,
        rdy_daemon::spawn::own_sid(),
        "the child left our session: {line}"
    );
    assert_eq!(sid, pid, "the child leads its own session: {line}");
}

#[test]
fn spawn1_a_symlinked_daemon_log_is_refused_and_its_target_is_untouched() {
    let _env = env_lock();
    use std::os::unix::fs::PermissionsExt;
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    run.ensure().unwrap();
    let victim = dir.path().join("victim");
    std::fs::write(&victim, "keep\n").unwrap();
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::os::unix::fs::symlink(&victim, run.log_file()).unwrap();
    let e = serve_command(std::path::Path::new("/bin/sh"), dir.path(), &run).unwrap_err();
    assert!(e.to_string().contains("symbolic link"), "{e}");
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep\n");
    assert_eq!(
        std::fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
        0o644
    );
}

#[test]
fn spawn1_the_serve_command_refuses_a_socket_path_over_the_limit() {
    let _env = env_lock();
    let dir = short_tempdir();
    let deep = dir.path().join("a".repeat(120));
    std::fs::create_dir_all(&deep).unwrap();
    let run = RunDir::of_project(&deep);
    let e = serve_command(std::path::Path::new("/bin/sh"), &deep, &run).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        e.to_string().contains("the limit is 103") && e.to_string().contains("shorter path"),
        "{e}"
    );
    assert!(
        !run.path().exists(),
        "no run dir or log for a daemon that could not bind"
    );
}

/// A relative project reaches the child as an absolute path: the child runs in the project, so
/// a relative `--project` would resolve against the project itself (`<p>/<p>`) and the daemon
/// would exit with `E_NO_PROJECT` (cli2 fix round 1).
#[test]
fn spawn1_a_relative_project_reaches_the_child_absolute() {
    let _env = env_lock();
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let rel = std::path::Path::new("docs");
    let cmd = serve_command(std::path::Path::new("/usr/local/bin/rdy"), rel, &run).unwrap();
    let want = std::env::current_dir().unwrap().join("docs");
    let args: Vec<&OsStr> = cmd.get_args().collect();
    assert_eq!(args.last(), Some(&want.as_os_str()), "{args:?}");
    assert_eq!(cmd.get_current_dir(), Some(want.as_path()));
}

/// A spawned child that exits is reaped even after its `Spawned` is dropped: without a reaper a
/// long-lived spawner (`rdy edit`, an `rdy mcp` proxy) keeps the daemon as a zombie, which
/// `process_alive` still reports, so `rdy serve --stop` waits it out and fails (cli2 fix round 1).
#[test]
fn spawn1_a_spawned_child_is_reaped_and_reports_its_code_after_the_spawned_is_dropped() {
    let _env = env_lock();
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let fake = dir.path().join("fake-rdy");
    std::fs::write(&fake, "#!/bin/sh\necho pid-$$ >&2\nexit 3\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    // a deadline, not an expected time: the child exits in well under a second, but a fork and
    // exec stalled past 5 s while other tracks built and tested on the same machine (M2c t15/t16
    // integration), and macOS can hold the first exec of a freshly written script for 20 s or
    // more while it assesses it; the child-launch waits in this file allow 60 s for that reason
    let wait = std::time::Duration::from_secs(60);
    // the exit code is still reported
    let mut spawned = spawn_serve(&mut serve_command(&fake, dir.path(), &run).unwrap()).unwrap();
    let t0 = std::time::Instant::now();
    let code = loop {
        if let Some(c) = (spawned.exited)() {
            break c;
        }
        assert!(t0.elapsed() < wait, "no exit code");
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert_eq!(code, 3);
    // and a child whose Spawned is dropped at once does not stay behind as a zombie
    drop(spawn_serve(&mut serve_command(&fake, dir.path(), &run).unwrap()).unwrap());
    let t0 = std::time::Instant::now();
    let pid: u32 = loop {
        let text = std::fs::read_to_string(run.log_file()).unwrap_or_default();
        let pids: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("pid-"))
            .collect();
        if pids.len() == 2 {
            break pids[1].trim().parse().unwrap();
        }
        assert!(t0.elapsed() < wait, "no second pid line: {text}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let t0 = std::time::Instant::now();
    while process_alive(pid) {
        assert!(
            t0.elapsed() < wait,
            "the exited child {pid} was never reaped (a zombie still counts as alive)"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
