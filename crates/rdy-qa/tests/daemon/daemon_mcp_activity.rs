//! MCP over HTTP work through the daemon's life (D-6M0AF, D-CCC6E, D-122E5, D-PBXS5). An
//! open listen or a running HTTP call is an active client: it holds off the idle exit and a
//! replacement by another build, and a stop seals admission before it cancels streams and drains
//! the admitted work. Cached claim state, a finished call and a live page are not active clients.
//! Fake activity permits stand in for HTTP requests: no HTTP endpoint is bound here.
use rdy_daemon::daemon::{HttpActivity, HttpPermit, HttpWork};
use rdy_daemon::*;
use rdy_mcp::server::DaemonBuildWarning;
use rdy_qa::daemon::{EchoHandler, Hosted, connect_ready, short_tempdir};
use rdy_store::start_lock::StartGuard;
use std::path::Path;
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const VERSION: &str = "1";
/// A build that is not this binary's: a fixed synthetic id, never the live one.
const OTHER: &str = "0123456789abcdef";
/// A hang guard for what a test waits on: a failure message, never a measure.
const T: Duration = Duration::from_secs(120);
/// An idle timeout no test reaches on the clock: only `DaemonHandle::expire_idle` ends it.
const LONG_IDLE: Duration = Duration::from_secs(3600);

/// An in-process daemon over a fresh directory with a `.rdy`, bound and ready, not yet running.
struct Fixture {
    dir: tempfile::TempDir,
    run: RunDir,
    daemon: Daemon,
}

async fn fixture(idle: Option<Duration>, store: bool) -> Fixture {
    let dir = short_tempdir();
    std::fs::create_dir(dir.path().join(".rdy")).unwrap();
    if store {
        // what the project check watches beside `.rdy`
        let store = dir.path().join(".rdy").join("store");
        std::fs::create_dir(&store).unwrap();
        std::fs::write(store.join("lock"), b"").unwrap();
        // and the start lock a real start makes before the bind, where the check takes it
        drop(
            StartGuard::try_take(&dir.path().join(".rdy"))
                .unwrap()
                .unwrap(),
        );
    }
    let run = RunDir::of_project(dir.path());
    let mut config = DaemonConfig::new(run.clone(), VERSION.into());
    config.idle_timeout = idle;
    let mut daemon = Daemon::new(config, Arc::new(EchoHandler::default()));
    daemon.bind().await.unwrap();
    daemon.readiness().ready();
    Fixture { dir, run, daemon }
}

/// A ctl hello of this test's version and `build`, from the project at `cwd`.
fn hello(run: &RunDir, cwd: &Path, build: &str) -> Hello {
    Hello {
        rdy: PROTO_VERSION,
        version: VERSION.into(),
        build: build.into(),
        proto: Proto::Ctl,
        cwd: cwd.to_path_buf(),
        project: run.project_id().unwrap(),
    }
}

async fn ready(run: &RunDir, hello: &Hello) -> Client {
    match connect(run, hello).await {
        Attempt::Ready(c) => c,
        other => panic!("expected ready, got {other:?}"),
    }
}

/// Waits until the daemon counts `n` open socket connections.
async fn connections_reach(d: &DaemonHandle, n: usize) {
    let mut w = d.keepers_watch();
    tokio::time::timeout(T, async {
        while d.connections() != n {
            w.changed().await.unwrap();
        }
    })
    .await
    .expect("the connection count changed");
}

async fn exit_of(serving: JoinHandle<std::io::Result<ExitReason>>) -> ExitReason {
    tokio::time::timeout(T, serving)
        .await
        .expect("the daemon exited")
        .unwrap()
        .unwrap()
}

fn activity(calls: usize, streams: usize) -> HttpActivity {
    HttpActivity { calls, streams }
}

/// The pid `daemon.pid` names, if the file stands.
fn pid_named(run: &RunDir) -> Option<u32> {
    run.pid_record().map(|r| r.pid())
}

/// A running call and an open listen each hold off the idle exit; neither is a socket connection.
/// Once the last one ends, the normal idle timer runs again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mht5_active_http_work_prevents_idle_exit() {
    let f = fixture(Some(LONG_IDLE), false).await;
    let handle = f.daemon.handle();
    let http = handle.http_admission();
    let serving = tokio::spawn(f.daemon.run());

    let call = http
        .admit(HttpWork::Call)
        .expect("a running daemon admits a call");
    assert_eq!(http.activity(), activity(1, 0));
    assert_eq!(handle.keepers(), 1);
    assert_eq!(handle.connections(), 0, "HTTP work is no socket connection");
    assert!(
        !handle.expire_idle(),
        "a running call holds off the idle exit"
    );

    let listen = http.admit(HttpWork::Listen).expect("and a listen");
    drop(call);
    assert_eq!(http.activity(), activity(0, 1));
    assert_eq!(handle.keepers(), 1);
    assert!(
        !handle.expire_idle(),
        "an open listen holds off the idle exit"
    );

    drop(listen);
    assert_eq!(http.activity(), activity(0, 0));
    assert_eq!(handle.keepers(), 0);
    assert!(
        handle.expire_idle(),
        "the last one ended: the daemon can idle out"
    );
    assert_eq!(exit_of(serving).await, ExitReason::Idle);
    assert!(
        http.admit(HttpWork::Call).is_none(),
        "an idle exit seals admission"
    );

    // the real timer: armed again once the last HTTP request ends (no clock is asserted)
    let f = fixture(Some(Duration::from_millis(1)), false).await;
    let handle = f.daemon.handle();
    let http = handle.http_admission();
    let call = http.admit(HttpWork::Call).unwrap();
    let serving = tokio::spawn(f.daemon.run());
    assert!(!handle.expire_idle());
    drop(call);
    assert_eq!(exit_of(serving).await, ExitReason::Idle);
}

/// Saved claim state and a call that has ended keep nothing: the daemon idles out with them.
#[test]
fn mht5_cached_claims_do_not_prevent_idle_exit() {
    let mut host = Hosted::new();
    let run = host.run.take().expect("the host runs");
    let shared = Arc::clone(run.shared());
    let http = shared
        .http
        .get()
        .expect("the daemon's start sets MCP over HTTP admission")
        .clone();
    // a finished call (an initialize, a tool call) and the claim state it left behind
    drop(http.admit(HttpWork::Call).expect("admitted"));
    let key = ("agent:cached".to_string(), Some("r1".to_string()));
    shared.hub.claims().mark_instructions_read(&key);
    assert!(
        shared.hub.claims().instructions_read(&key),
        "guard: the claim's state is saved"
    );
    assert_eq!(http.activity(), activity(0, 0));
    assert_eq!(run.handle().keepers(), 0);
    assert!(run.handle().expire_idle(), "cached claims keep nothing");
    assert_eq!(run.wait().unwrap(), ExitReason::Idle);
}

/// A stop seals admission first: when an open listen hears its cancel, no new call or listen can
/// enter, and the listen still counts until it ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mht5_stop_seals_admission_before_stream_cancel() {
    let f = fixture(None, false).await;
    let handle = f.daemon.handle();
    let http = handle.http_admission();
    let serving = tokio::spawn(f.daemon.run());

    let listen = http.admit(HttpWork::Listen).unwrap();
    let (heard, cancel_heard) = oneshot::channel();
    let (end, ended) = oneshot::channel::<()>();
    let probe = http.clone();
    let stream = tokio::spawn(async move {
        listen.cancelled().await;
        let seen = (
            probe.admit(HttpWork::Call).is_none(),
            probe.admit(HttpWork::Listen).is_none(),
            probe.activity(),
        );
        let _ = heard.send(seen);
        let _ = ended.await;
        drop(listen);
    });

    handle.stop();
    let (call_refused, listen_refused, at_cancel) = tokio::time::timeout(T, cancel_heard)
        .await
        .expect("the stop cancels the listen")
        .unwrap();
    assert!(
        call_refused,
        "a call cannot enter once the stream is cancelled"
    );
    assert!(listen_refused, "nor a listen");
    assert_eq!(
        at_cancel,
        activity(0, 1),
        "the cancelled stream counts until it ends"
    );

    end.send(()).unwrap();
    stream.await.unwrap();
    assert_eq!(exit_of(serving).await, ExitReason::Stopped);
    assert_eq!(http.activity(), activity(0, 0));
    assert!(
        http.admit(HttpWork::Call).is_none(),
        "a stopped daemon admits nothing"
    );
}

/// The permit goes with the worker, not the response: a client that goes away mid-call leaves its
/// write counted, so a client of another build latches the replacement and the daemon serves on
/// until the write ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mht5_cancelled_response_keeps_running_writer_counted() {
    let f = fixture(None, false).await;
    let handle = f.daemon.handle();
    let http = handle.http_admission();
    let serving = tokio::spawn(f.daemon.run());

    let permit = http.admit(HttpWork::Call).unwrap();
    let (entered, in_flight) = oneshot::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let worker = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _ = entered.send(());
        released.recv().unwrap();
    });
    // the response only waits on the worker; aborting it detaches the worker
    let response = tokio::spawn(worker);
    in_flight.await.unwrap();
    response.abort();
    assert!(
        response.await.unwrap_err().is_cancelled(),
        "the response is gone"
    );
    assert_eq!(
        http.activity(),
        activity(1, 0),
        "the admitted write still counts"
    );

    let other = ready(&f.run, &hello(&f.run, f.dir.path(), OTHER)).await;
    assert_eq!(
        http.newer_build().as_deref(),
        Some(OTHER),
        "the replacement is latched"
    );
    other.close();
    connections_reach(&handle, 0).await;
    assert!(
        http.admit(HttpWork::Call).is_some(),
        "the daemon is not leaving while the write runs"
    );
    assert!(!*handle.stopped().borrow());

    release.send(()).unwrap();
    assert_eq!(exit_of(serving).await, ExitReason::BuildReplaced);
    assert!(!f.run.socket_record().exists());
    assert_eq!(pid_named(&f.run), None);
}

/// With HTTP work active, a client of another build is served and latches the replacement; the
/// current build keeps serving everyone, and the warning for its next HTTP reply is published
/// once per actor and run.
#[test]
fn mht5_build_change_waits_for_active_http_work() {
    let mut host = Hosted::new();
    let rt = Hosted::rt();
    let run = host.run.take().expect("the host runs");
    let shared = Arc::clone(run.shared());
    let handle = run.handle();
    let http = shared.http.get().expect("admission").clone();
    let run_dir = RunDir::of_project(host.dir.path());
    rt.block_on(async {
        let listen = http.admit(HttpWork::Listen).unwrap();
        assert_eq!(http.newer_build(), None);
        assert_eq!(
            shared.http_build_warning(),
            None,
            "no client of another build yet"
        );

        let mut newer = host.hello(Proto::Ctl);
        newer.build = OTHER.into();
        let other = connect_ready(&run_dir, &newer).await;
        assert_eq!(http.newer_build().as_deref(), Some(OTHER));
        assert_eq!(
            shared.http_build_warning(),
            Some(DaemonBuildWarning {
                version: "t".into(),
                daemon_build: BUILD.into(),
                client_build: OTHER.into(),
            })
        );
        // the registry's once-per-claim mark hands it out once per actor and run (D-PBXS5)
        let key = ("agent:old".to_string(), Some("r1".to_string()));
        assert!(shared.hub.claims().take_build_warning(&key));
        assert!(!shared.hub.claims().take_build_warning(&key));

        other.close();
        connections_reach(&handle, 0).await;
        // the open listen keeps the current build serving
        let same = connect_ready(&run_dir, &host.hello(Proto::Ctl)).await;
        same.close();
        connections_reach(&handle, 0).await;
        assert!(http.admit(HttpWork::Call).is_some());
        assert!(!*handle.stopped().borrow());
        drop(listen);
    });
    assert_eq!(run.wait().unwrap(), ExitReason::BuildReplaced);
}

/// A latched replacement waits for the last active client, of either kind: the last HTTP request
/// to end releases it, and so does the last socket connection to close.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mht5_last_http_client_releases_latched_replacement() {
    // the listen ends last
    let f = fixture(None, false).await;
    let handle = f.daemon.handle();
    let http = handle.http_admission();
    let serving = tokio::spawn(f.daemon.run());
    let socket = ready(&f.run, &hello(&f.run, f.dir.path(), BUILD)).await;
    let listen = http.admit(HttpWork::Listen).unwrap();
    let other = ready(&f.run, &hello(&f.run, f.dir.path(), OTHER)).await;
    other.close();
    socket.close();
    connections_reach(&handle, 0).await;
    assert!(!*handle.stopped().borrow(), "the listen still runs");
    assert!(f.run.socket_record().exists());
    drop(listen);
    assert_eq!(exit_of(serving).await, ExitReason::BuildReplaced);
    assert!(!f.run.socket_record().exists(), "the run files go first");
    assert_eq!(pid_named(&f.run), None);
    assert!(
        http.admit(HttpWork::Call).is_none(),
        "a replaced daemon admits nothing"
    );

    // the socket connection closes last
    let f = fixture(None, false).await;
    let handle = f.daemon.handle();
    let http = handle.http_admission();
    let serving = tokio::spawn(f.daemon.run());
    let socket = ready(&f.run, &hello(&f.run, f.dir.path(), BUILD)).await;
    let call = http.admit(HttpWork::Call).unwrap();
    let other = ready(&f.run, &hello(&f.run, f.dir.path(), OTHER)).await;
    other.close();
    connections_reach(&handle, 1).await;
    drop(call);
    assert!(
        !*handle.stopped().borrow(),
        "a socket connection still runs"
    );
    socket.close();
    assert_eq!(exit_of(serving).await, ExitReason::BuildReplaced);
    assert!(http.admit(HttpWork::Call).is_none());
}

/// A live page in view holds off the idle exit, not a replacement: with no active client, a
/// client of another build replaces the daemon at once, and a finished call changes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mht5_page_hold_does_not_block_build_replacement() {
    let f = fixture(Some(LONG_IDLE), false).await;
    let handle = f.daemon.handle();
    let http = handle.http_admission();
    let serving = tokio::spawn(f.daemon.run());
    let hold = handle.hold();
    drop(http.admit(HttpWork::Call).expect("a finished call"));
    assert_eq!(handle.keepers(), 1, "guard: the page holds the daemon up");
    assert!(!handle.expire_idle(), "a live page holds off the idle exit");

    match connect(&f.run, &hello(&f.run, f.dir.path(), OTHER)).await {
        Attempt::Exiting => {}
        other => panic!("a daemon with no active client is replaced at once, got {other:?}"),
    }
    assert!(
        http.admit(HttpWork::Call).is_none(),
        "a daemon leaving to be replaced admits nothing"
    );
    assert_eq!(exit_of(serving).await, ExitReason::BuildReplaced);
    drop(hold);
}

/// Admission and the replacement decision meet under the keepers' lock: a request either enters
/// before the daemon decides, and holds the replacement off, or is refused. Never both.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht5_admission_cannot_race_leaving_state() {
    const ROUNDS: usize = 12;
    const ADMITTERS: usize = 4;

    /// `ADMITTERS` threads that each try one call once `barrier` opens.
    fn admitters(
        http: &rdy_daemon::daemon::HttpAdmission,
        barrier: &Arc<Barrier>,
    ) -> Vec<std::thread::JoinHandle<Option<HttpPermit>>> {
        (0..ADMITTERS)
            .map(|_| {
                let (http, barrier) = (http.clone(), Arc::clone(barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    http.admit(HttpWork::Call)
                })
            })
            .collect()
    }

    fn admitted(threads: Vec<std::thread::JoinHandle<Option<HttpPermit>>>) -> Vec<HttpPermit> {
        threads
            .into_iter()
            .filter_map(|t| t.join().unwrap())
            .collect()
    }

    // a hello of another build, alone, against concurrent admission
    for round in 0..ROUNDS {
        let f = fixture(None, false).await;
        let handle = f.daemon.handle();
        let http = handle.http_admission();
        let serving = tokio::spawn(f.daemon.run());
        let barrier = Arc::new(Barrier::new(ADMITTERS + 1));
        let threads = admitters(&http, &barrier);
        let gate = Arc::clone(&barrier);
        tokio::task::spawn_blocking(move || gate.wait())
            .await
            .unwrap();
        let outcome = connect(&f.run, &hello(&f.run, f.dir.path(), OTHER)).await;
        let permits = admitted(threads);
        match outcome {
            Attempt::Exiting => assert!(
                permits.is_empty(),
                "round {round}: a daemon that left to be replaced admitted {} calls",
                permits.len()
            ),
            Attempt::Ready(c) => {
                assert!(
                    !permits.is_empty(),
                    "round {round}: another build alone is served only beside admitted work"
                );
                assert!(!*handle.stopped().borrow(), "round {round}");
                c.close();
            }
            other => panic!("round {round}: {other:?}"),
        }
        drop(permits);
        assert_eq!(
            exit_of(serving).await,
            ExitReason::BuildReplaced,
            "round {round}"
        );
        assert!(http.admit(HttpWork::Call).is_none(), "round {round}");
    }

    // the last socket connection of a latched daemon closes against concurrent admission
    for round in 0..ROUNDS {
        let f = fixture(None, false).await;
        let handle = f.daemon.handle();
        let http = handle.http_admission();
        let serving = tokio::spawn(f.daemon.run());
        let socket = ready(&f.run, &hello(&f.run, f.dir.path(), BUILD)).await;
        let other = ready(&f.run, &hello(&f.run, f.dir.path(), OTHER)).await;
        other.close();
        connections_reach(&handle, 1).await;
        let barrier = Arc::new(Barrier::new(ADMITTERS + 1));
        let threads = admitters(&http, &barrier);
        let gate = Arc::clone(&barrier);
        tokio::task::spawn_blocking(move || gate.wait())
            .await
            .unwrap();
        socket.close();
        connections_reach(&handle, 0).await;
        let permits = admitted(threads);
        // the count reached 0 under the same lock that decided: admission tells which way
        if permits.is_empty() {
            assert!(
                http.admit(HttpWork::Call).is_none(),
                "round {round}: no call entered, so the daemon left to be replaced"
            );
        } else {
            assert!(
                http.admit(HttpWork::Call).is_some(),
                "round {round}: {} calls entered, so the daemon serves on",
                permits.len()
            );
            assert!(!*handle.stopped().borrow(), "round {round}");
        }
        drop(permits);
        assert_eq!(
            exit_of(serving).await,
            ExitReason::BuildReplaced,
            "round {round}"
        );
        assert!(http.admit(HttpWork::Call).is_none(), "round {round}");
    }
}

/// An ordinary stop (no start lock) drains an admitted HTTP write. The record and the
/// socket go at once, `daemon.pid` stays until the write's permit ends, and the stop's bounded
/// wait covers the write.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mht5_ordinary_stop_drains_an_admitted_http_write() {
    let f = fixture(None, false).await;
    let handle = f.daemon.handle();
    let http = handle.http_admission();
    let order = Arc::new(Mutex::new(Vec::new()));
    let serving = {
        let order = Arc::clone(&order);
        let daemon = f.daemon;
        tokio::spawn(async move {
            let out = daemon.run().await;
            order.lock().unwrap().push("run returned");
            out
        })
    };
    let permit = http.admit(HttpWork::Call).unwrap();
    let (entered, in_flight) = oneshot::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let worker = {
        let order = Arc::clone(&order);
        tokio::task::spawn_blocking(move || {
            let _ = entered.send(());
            released.recv().unwrap();
            order.lock().unwrap().push("write ended");
            drop(permit);
        })
    };
    in_flight.await.unwrap();

    handle.stop();
    assert!(!f.run.socket_record().exists(), "no new client reaches it");
    assert_eq!(
        pid_named(&f.run),
        Some(std::process::id()),
        "daemon.pid stays while the admitted write runs"
    );
    assert!(
        http.admit(HttpWork::Call).is_none(),
        "the stop sealed admission"
    );

    release.send(()).unwrap();
    worker.await.unwrap();
    assert_eq!(pid_named(&f.run), None, "daemon.pid goes with the write");
    assert_eq!(exit_of(serving).await, ExitReason::Stopped);
    assert_eq!(
        *order.lock().unwrap(),
        ["write ended", "run returned"],
        "the stop's drain waited for the write"
    );
}

/// The project-check stop (D-122E5): the host's drain waits for an admitted HTTP write too.
/// `daemon.pid` and the start lock stay until the write's permit ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mht5_project_check_drain_waits_for_http_work() {
    let f = fixture(None, true).await;
    let handle = f.daemon.handle();
    let http = handle.http_admission();
    let serving = tokio::spawn(f.daemon.run());
    let permit = http.admit(HttpWork::Call).unwrap();

    let store = f.dir.path().join(".rdy").join("store");
    std::fs::rename(&store, f.dir.path().join("store-aside")).unwrap();
    assert!(
        handle.check_project(),
        "the store moved: the project is gone"
    );
    assert!(
        http.admit(HttpWork::Call).is_none(),
        "a draining daemon admits nothing"
    );
    assert!(!f.run.socket_record().exists());

    // the host's word comes while the write still runs
    handle.drained();
    assert_eq!(
        pid_named(&f.run),
        Some(std::process::id()),
        "daemon.pid stays while the admitted write runs"
    );
    assert!(
        StartGuard::try_take(f.run.rdy_dir()).unwrap().is_none(),
        "the start lock stays while the admitted write runs"
    );

    drop(permit);
    assert_eq!(pid_named(&f.run), None, "daemon.pid goes with the write");
    assert!(
        StartGuard::try_take(f.run.rdy_dir()).unwrap().is_some(),
        "and the start lock"
    );
    assert_eq!(exit_of(serving).await, ExitReason::ProjectGone);
}
