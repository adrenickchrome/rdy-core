#![cfg(unix)]
// Unix only: it uses Unix sockets. FW-XP1 Phase 4 ports it.
//! The build handshake (D-CCC6E): the hello and `ready` carry the build, a daemon alone with
//! another build exits to be replaced, and a busy one serves and exits at its last close.
mod common;
use common::*;
use rdy_daemon::*;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// A build that is not this binary's: a fixed synthetic id, never the live one.
const OTHER: &str = "0123456789abcdef";
/// A hang guard, never asserted.
const T: Duration = Duration::from_secs(30);
const BAD_BUILD: &str = "the hello build must be 16 lowercase hexadecimal digits";
const BAD_BUILD_FIX: &str = "send the build id from this rdy binary";

fn parsed(line: &str) -> Value {
    serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line:?}"))
}

fn project_of(cwd: &Path) -> ProjectId {
    RunDir::of_project(cwd).project_id().unwrap()
}

/// A hello of this version and `build`, from the project at `cwd`.
fn typed(version: &str, build: &str, cwd: &Path) -> Hello {
    Hello {
        rdy: PROTO_VERSION,
        version: version.into(),
        build: build.into(),
        proto: Proto::Ctl,
        cwd: cwd.to_path_buf(),
        project: project_of(cwd),
    }
}

async fn ready(run: &RunDir, hello: &Hello) -> Client {
    match connect(run, hello).await {
        Attempt::Ready(c) => c,
        other => panic!("expected ready, got {other:?}"),
    }
}

/// Waits until the daemon counts `n` open connections.
async fn connections_reach(d: &DaemonHandle, n: usize) {
    let mut w = d.keepers_watch();
    tokio::time::timeout(T, async {
        loop {
            if d.connections() == n {
                return;
            }
            w.changed().await.unwrap();
        }
    })
    .await
    .expect("the connection count changed");
}

fn spawn_echo(
    version: &'static str,
    build: &'static str,
    count: Arc<AtomicUsize>,
) -> impl FnMut(&RunDir) -> std::io::Result<Spawned> + Send + 'static {
    move |run: &RunDir| {
        count.fetch_add(1, Ordering::SeqCst);
        let run = run.clone();
        tokio::spawn(async move {
            let mut config = DaemonConfig::new(run, version.into());
            config.build = build.into();
            let mut d = Daemon::new(config, Arc::new(Echo::default()));
            d.bind().await.unwrap();
            d.readiness().ready();
            let _ = d.run().await;
        });
        Ok(Spawned {
            exited: Box::new(|| None),
        })
    }
}

fn start_options(
    version: &str,
    build: &str,
    cwd: &Path,
    spawn: impl FnMut(&RunDir) -> std::io::Result<Spawned> + Send + 'static,
) -> StartOptions {
    let mut o = StartOptions::new(
        version.into(),
        Proto::Ctl,
        cwd.to_path_buf(),
        Box::new(spawn),
    );
    o.build = build.into();
    o.start_timeout = T;
    o
}

fn run_files_gone(run: &RunDir) {
    assert!(!run.socket().exists(), "the socket is gone");
    assert!(!run.socket_record().exists(), "the socket record is gone");
    assert!(!run.pid_file().exists(), "the pid file is gone");
}

#[tokio::test]
async fn mh5_hello_and_ready_add_build_and_ignore_unknown_fields() {
    let d = daemon("1", true, |_| {}).await;
    let project = project_of(d.dir.path());
    let cwd = d.dir.path();

    // a hello always serializes its build; `ready` has exactly its fields
    let v = serde_json::to_value(typed("1", OTHER, cwd)).unwrap();
    assert_eq!(
        v.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["build", "cwd", "project", "proto", "rdy", "version"]
    );
    assert_eq!(v["build"], OTHER);
    let (keep, first) = connect_with(&d.run, &hello("1", "ctl", cwd)).await;
    let ready_reply = parsed(&first);
    assert_eq!(
        ready_reply.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["build", "session", "status", "version"]
    );
    assert_eq!(ready_reply["status"], "ready");
    assert_eq!(ready_reply["build"], BUILD);
    drop(keep);

    // an unknown key is ignored on the hello, and on `ready` at the client
    let mut line = json!({"rdy": 1, "version": "1", "build": BUILD, "proto": "ctl", "cwd": cwd,
        "project": project, "later": {"any": 1}});
    let (keep, first) = connect_with(&d.run, &(line.to_string() + "\n")).await;
    assert_eq!(parsed(&first)["status"], "ready");
    drop(keep);
    let reply: HelloReply = serde_json::from_value(
        json!({"status": "ready", "version": "1", "build": OTHER, "session": 1, "later": 2}),
    )
    .unwrap();
    assert!(matches!(reply, HelloReply::Ready { build, .. } if build == OTHER));

    // a missing, wrong-typed or malformed build is the build's own refusal
    let bad: Vec<Option<Value>> = vec![
        None,
        Some(json!(7)),
        Some(Value::Null),
        Some(json!("0123456789ABCDEF")),
        Some(json!("0123456789abcde")),
        Some(json!("0123456789abcdef0")),
        Some(json!("0123456789abcdeg")),
        Some(json!("")),
    ];
    for build in bad {
        let o = line.as_object_mut().unwrap();
        o.remove("later");
        match &build {
            Some(b) => o.insert("build".into(), b.clone()),
            None => o.remove("build"),
        };
        let (_r, first) = connect_with(&d.run, &(line.to_string() + "\n")).await;
        let v = parsed(&first);
        assert_eq!(v["status"], "refused", "{build:?}");
        assert_eq!(v["error"]["code"], "E_BAD_INPUT", "{build:?}");
        assert_eq!(v["error"]["message"], BAD_BUILD, "{build:?}");
        assert_eq!(v["error"]["fix"], BAD_BUILD_FIX, "{build:?}");
    }
    assert_eq!(
        d.handle.sessions().len(),
        0,
        "no refused hello has a session"
    );

    // the general shape's text names the build; its fix stays
    let (_r, first) = connect_with(&d.run, "hello\n").await;
    let v = parsed(&first);
    assert_eq!(
        v["error"]["message"],
        r#"the first line must be a hello: {"rdy":1,"version":"…","build":"…","proto":"ctl"|"mcp","cwd":"…","project":"dev:ino"}"#
    );
    assert_eq!(v["error"]["fix"], "send the hello line first");

    // the bytes behind a hello stay for the MCP stream
    let (mut a, mut b) = tokio::io::duplex(4096);
    let piped =
        serde_json::to_string(&typed("1", OTHER, cwd)).unwrap() + "\n{\"jsonrpc\":\"2.0\"}\n";
    a.write_all(piped.as_bytes()).await.unwrap();
    drop(a);
    let got = read_hello(&mut b).await.unwrap();
    assert_eq!(got.build, OTHER);
    let mut rest = String::new();
    b.read_to_string(&mut rest).await.unwrap();
    assert_eq!(rest, "{\"jsonrpc\":\"2.0\"}\n");
    d.handle.stop();

    // a malformed `ready` is the client's bad-daemon reply, with no default
    for bad in [
        r#"{"status":"ready","version":"1","session":1}"#,
        r#"{"status":"ready","version":"1","build":7,"session":1}"#,
        r#"{"status":"ready","version":"1","build":"ABCDEF0123456789","session":1}"#,
    ] {
        let dir = short_tempdir();
        std::fs::create_dir_all(dir.path().join(".rdy/run")).unwrap();
        let run = RunDir::of_project(dir.path());
        let l = tokio::net::UnixListener::bind(run.socket()).unwrap();
        let answer = bad.to_string();
        tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut s = BufReader::new(s);
            let mut line = String::new();
            s.read_line(&mut line).await.unwrap();
            s.get_mut()
                .write_all(format!("{answer}\n").as_bytes())
                .await
                .unwrap();
            s.get_mut().flush().await.unwrap();
            let mut more = String::new();
            let _ = s.read_line(&mut more).await;
        });
        let got = connect(&run, &typed("1", BUILD, dir.path())).await;
        assert!(matches!(got, Attempt::NoDaemon(_)), "{bad}: {got:?}");
    }
}

#[tokio::test]
async fn mh5_same_build_keeps_the_existing_daemon() {
    let d = daemon("1", true, |c| c.build = OTHER.into()).await;
    let cwd = d.dir.path();
    let stopped = d.handle.stopped();
    let mut c = ready(&d.run, &typed("1", OTHER, cwd)).await;
    assert_eq!(c.version(), "1");
    assert_eq!(c.build(), OTHER, "ready carries the daemon's actual build");
    let ping = c.ctl("ping", json!({})).await.unwrap();
    assert_eq!(ping["build"], OTHER);
    let sessions = c.ctl("sessions", json!({})).await.unwrap();
    assert_eq!(sessions[0]["build"], OTHER);
    c.close();
    connections_reach(&d.handle, 0).await;
    assert!(!*stopped.borrow(), "no replacement");
    assert!(d.run.socket().exists());
    // a later client reaches the same daemon
    ready(&d.run, &typed("1", OTHER, cwd)).await.close();

    // the older guards come first: a client of another version, with any build
    let (keep, _) = connect_with(&d.run, &hello_built("1", OTHER, cwd)).await;
    let (_r, first) = connect_with(&d.run, &hello_built("2", "junk", cwd)).await;
    let v = parsed(&first);
    assert_eq!(v["error"]["code"], "E_DAEMON_VERSION");
    assert_eq!(
        v["error"]["message"],
        "the running daemon is rdy 1, this client is rdy 2"
    );
    assert_eq!(
        v["error"]["fix"],
        "close the other sessions or run rdy serve --stop"
    );
    // and of another project, with any build
    let other_dir = short_tempdir();
    std::fs::create_dir_all(other_dir.path().join(".rdy/run")).unwrap();
    let line = hello_built("1", "junk", other_dir.path());
    let (_r, first) = connect_with(&d.run, &line).await;
    let v = parsed(&first);
    assert_eq!(v["error"]["code"], "E_DAEMON_START");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("serves another project"),
        "{v}"
    );
    drop(keep);
    assert!(!*stopped.borrow(), "those clients replaced nothing");
    d.handle.stop();
}

#[tokio::test]
async fn mh5_idle_other_build_is_replaced_before_retry() {
    let log = daemon::capture_log_for_tests(&["exiting (build replaced)"]);
    // one mismatched connection, by hand: the files are gone when `exiting` arrives
    let d = daemon("1", true, |_| {}).await;
    let (_r, first) = connect_with(&d.run, &hello_built("1", OTHER, d.dir.path())).await;
    let v = parsed(&first);
    assert_eq!(v["status"], "exiting");
    assert_eq!(v["version"], "1");
    run_files_gone(&d.run);
    let reason = tokio::time::timeout(T, d.task).await.unwrap().unwrap();
    assert_eq!(reason.unwrap(), ExitReason::BuildReplaced);
    assert!(
        log.lines().iter().any(|l| l == "exiting (build replaced)"),
        "{:?}",
        log.lines()
    );

    // through connect_or_start: one successor starts and answers its own build
    let d = daemon("1", true, |_| {}).await;
    let count = Arc::new(AtomicUsize::new(0));
    let opts = start_options(
        "1",
        OTHER,
        d.dir.path(),
        spawn_echo("1", OTHER, Arc::clone(&count)),
    );
    let mut c = tokio::time::timeout(T, connect_or_start(&d.run, opts))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(c.build(), OTHER, "the successor's build");
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(
        tokio::time::timeout(T, d.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        ExitReason::BuildReplaced
    );
    c.ctl("stop", json!({})).await.unwrap();
}

#[tokio::test]
async fn mh5_busy_other_build_serves_then_exits_at_last_connection() {
    // no idle timeout, so only the last close can end this daemon
    let d = daemon("1", true, |_| {}).await;
    let cwd = d.dir.path();
    let stopped = d.handle.stopped();
    let hold = d.handle.hold();
    let same = ready(&d.run, &typed("1", BUILD, cwd)).await;
    let mut other = ready(&d.run, &typed("1", OTHER, cwd)).await;
    assert_eq!(other.build(), BUILD, "ready carries the daemon's build");
    let v = other.ctl("echo", json!({"a": 1})).await.unwrap();
    assert_eq!(v["args"]["a"], 1, "the request is served normally");
    let listed = other.ctl("sessions", json!({})).await.unwrap();
    assert_eq!(listed[1]["build"], OTHER);

    // a later matching client does not clear the latch
    let later = ready(&d.run, &typed("1", BUILD, cwd)).await;
    same.close();
    connections_reach(&d.handle, 2).await;
    later.close();
    connections_reach(&d.handle, 1).await;
    assert!(!*stopped.borrow(), "one client still open");
    assert!(d.run.socket().exists(), "still serving");

    // the last close stops it, with a site hold alive
    other.close();
    let mut watch = d.handle.stopped();
    tokio::time::timeout(T, watch.wait_for(|s| *s))
        .await
        .unwrap()
        .unwrap();
    run_files_gone(&d.run);
    assert_eq!(
        tokio::time::timeout(T, d.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        ExitReason::BuildReplaced
    );
    drop(hold);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mh5_prehello_clients_and_concurrent_closes_are_counted() {
    // a client held before its hello counts as a connection
    let d = daemon("1", true, |_| {}).await;
    let cwd = d.dir.path().to_path_buf();
    let silent = UnixStream::connect(d.run.socket()).await.unwrap();
    connections_reach(&d.handle, 1).await;
    let other = ready(&d.run, &typed("1", OTHER, &cwd)).await;
    let stopped = d.handle.stopped();
    drop(silent);
    connections_reach(&d.handle, 1).await;
    assert!(!*stopped.borrow(), "the mismatched client is still open");
    other.close();
    assert_eq!(
        tokio::time::timeout(T, d.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        ExitReason::BuildReplaced
    );

    // simultaneous admissions behind a holder are all served, and simultaneous closes stop once
    let d = daemon("1", true, |_| {}).await;
    let cwd = d.dir.path().to_path_buf();
    let holder = ready(&d.run, &typed("1", BUILD, &cwd)).await;
    let gate = Arc::new(Barrier::new(8));
    let mut joins = vec![];
    for _ in 0..8 {
        let (run, cwd, gate) = (d.run.clone(), cwd.clone(), Arc::clone(&gate));
        joins.push(tokio::spawn(async move {
            let hello = typed("1", OTHER, &cwd);
            tokio::task::spawn_blocking(move || gate.wait())
                .await
                .unwrap();
            ready(&run, &hello).await
        }));
    }
    let mut clients = vec![];
    for j in joins {
        clients.push(j.await.unwrap());
    }
    holder.close();
    connections_reach(&d.handle, 8).await;
    let gate = Arc::new(Barrier::new(8));
    let mut joins = vec![];
    for c in clients {
        let gate = Arc::clone(&gate);
        joins.push(tokio::task::spawn_blocking(move || {
            gate.wait();
            c.close();
        }));
    }
    for j in joins {
        j.await.unwrap();
    }
    assert_eq!(
        tokio::time::timeout(T, d.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        ExitReason::BuildReplaced
    );
    run_files_gone(&d.run);

    // simultaneous mismatched clients with nothing else open start at most one daemon: either
    // the old daemon latched and serves them all, or one successor does
    let d = daemon("1", true, |_| {}).await;
    let cwd = d.dir.path().to_path_buf();
    let count = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Barrier::new(6));
    let mut joins = vec![];
    for _ in 0..6 {
        let (run, cwd, gate) = (d.run.clone(), cwd.clone(), Arc::clone(&gate));
        let opts = start_options("1", OTHER, &cwd, spawn_echo("1", OTHER, Arc::clone(&count)));
        joins.push(tokio::spawn(async move {
            tokio::task::spawn_blocking(move || gate.wait())
                .await
                .unwrap();
            connect_or_start(&run, opts).await
        }));
    }
    let mut clients = vec![];
    for j in joins {
        clients.push(tokio::time::timeout(T, j).await.unwrap().unwrap().unwrap());
    }
    for c in clients {
        c.close();
    }
    assert_eq!(
        tokio::time::timeout(T, d.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        ExitReason::BuildReplaced,
        "the latch was not lost"
    );
    // the old daemon is gone: one client reaches or starts this build's one daemon
    let opts = start_options("1", OTHER, &cwd, spawn_echo("1", OTHER, Arc::clone(&count)));
    let mut c = tokio::time::timeout(T, connect_or_start(&d.run, opts))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(c.build(), OTHER);
    assert_eq!(count.load(Ordering::SeqCst), 1, "one successor");
    c.ctl("stop", json!({})).await.unwrap();
}
