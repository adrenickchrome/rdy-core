//! wrt1: one writer thread; the published state is stored before the reply; a panic answers and the
//! writer keeps serving; shutdown closes the store once.
use rdy_daemon::{CHECKPOINT_TICK, DaemonEngine, E_DAEMON_GONE, Shared, WriterHandle};
use rdy_engine::engine::write_typed;
use rdy_engine::tensions::{DetectCx, Detector, Found, TensionKind};
use rdy_engine::{E_INTERNAL, Rdy, Scope, Session, WriteRequest};
use rdy_model::{BlockId, Role};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

fn assert_send_sync<T: Send + Sync>() {}

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

fn doc(
    r: &mut Rdy,
    view: &str,
    name: &str,
) -> Result<rdy_engine::WriteResult, rdy_engine::RdyError> {
    let ops = serde_json::from_value(serde_json::json!([
        {"op": "create", "type": "doc", "fields": {"name": name, "title": "D", "doc_type": "page"}}
    ]))
    .unwrap();
    r.write(
        &Session::human("human:t".into(), Role::Writer),
        WriteRequest {
            view: view.into(),
            ops,
            justification: None,
            message: String::new(),
        },
    )
}

#[test]
fn wrt1_shared_and_the_engine_are_send_sync() {
    assert_send_sync::<Shared>();
    assert_send_sync::<DaemonEngine>();
    assert_send_sync::<WriterHandle>();
}

#[test]
fn wrt1_the_state_is_stored_before_the_reply_is_sent() {
    let (_d, r) = project();
    let shared = Shared::new(r);
    let e = shared.engine();
    // 64 writers; each asserts that, the instant its reply arrives, a fresh read on *another*
    // handle of the engine already includes its own write
    let barrier = Arc::new(Barrier::new(64));
    let hs: Vec<_> = (0..64)
        .map(|i| {
            let (e, barrier) = (Arc::clone(&e), Arc::clone(&barrier));
            std::thread::spawn(move || {
                barrier.wait();
                let v = write_typed(&*e, move |r| doc(r, "work", &format!("d{i}"))).unwrap();
                let change = v["change"].as_str().unwrap().to_string();
                let st = e.read();
                assert!(
                    st.included("work").iter().any(|h| h.to_string() == change),
                    "writer {i}: its own write is not in the published state"
                );
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    assert_eq!(e.read().included("work").len(), 64);
    shared.shutdown().unwrap();
}

#[test]
fn wrt1_writes_apply_in_arrival_order_on_one_thread() {
    let (_d, r) = project();
    let shared = Shared::new(r);
    let e = shared.engine();
    let names: Vec<String> = (0..20).map(|i| format!("n{i:02}")).collect();
    for n in &names {
        let n = n.clone();
        write_typed(&*e, move |r| doc(r, "work", &n)).unwrap();
    }
    let st = e.read();
    let tree = st.tree(&Scope::View("work".into()), None, None).unwrap();
    let got: Vec<&str> = tree.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(got, names.iter().map(String::as_str).collect::<Vec<_>>());
    shared.shutdown().unwrap();
}

#[test]
fn wrt1_a_panic_in_a_write_answers_e_internal_and_the_writer_keeps_serving() {
    let (_d, r) = project();
    let shared = Shared::new(r);
    let e = shared.engine();
    let err = e.write(Box::new(|_r| panic!("boom"))).unwrap_err();
    assert_eq!(err.code, E_INTERNAL);
    assert!(err.message.contains("boom"), "{err}");
    assert!(shared.writer.is_alive());
    write_typed(&*e, |r| doc(r, "work", "after")).unwrap();
    assert_eq!(e.read().included("work").len(), 1);
    shared.shutdown().unwrap();
}

/// Panics while `BOOM` is set: a tension detector that fails the move computation of a write.
struct Boom;
static BOOM: AtomicBool = AtomicBool::new(false);
static BOOM_DETECTORS: [&dyn Detector; 1] = [&Boom];
impl Detector for Boom {
    fn kind(&self) -> TensionKind {
        TensionKind::EditCollision
    }
    fn detect(
        &self,
        _cx: &DetectCx<'_>,
        _seeds: Option<&BTreeSet<BlockId>>,
        _out: &mut Vec<Found>,
    ) -> Result<(), rdy_engine::RdyError> {
        assert!(!BOOM.load(Ordering::SeqCst), "moves boom");
        Ok(())
    }
}

#[test]
fn wrt1_a_panic_while_logging_tension_moves_leaves_the_writer_serving() {
    // the daemon's two notices are expected: the test takes and checks them (final review F9)
    let log = rdy_daemon::daemon::capture_log_for_tests(&["logging tension moves"]);
    let (_d, mut r) = project();
    r.set_detectors_for_tests(&BOOM_DETECTORS);
    let shared = Shared::new(r);
    let e = shared.engine();
    BOOM.store(true, Ordering::SeqCst);
    // the write's move computation panics: it is caught, the batch is sealed without moves,
    // and the write succeeds
    write_typed(&*e, |r| doc(r, "work", "one")).unwrap();
    write_typed(&*e, |r| doc(r, "work", "two")).unwrap();
    assert!(shared.writer.is_alive());
    BOOM.store(false, Ordering::SeqCst);
    write_typed(&*e, |r| doc(r, "work", "three")).unwrap();
    assert_eq!(e.read().included("work").len(), 3);
    assert!(e.read().tensions().unwrap().is_empty());
    shared.shutdown().unwrap();
    // logged once while it stood, and once when it cleared
    assert_eq!(
        log.lines(),
        vec![
            "logging tension moves failed (E_INTERNAL: internal error: moves boom (fix: report \
             this as a bug in rdy)); the next write logs them"
                .to_string(),
            "logging tension moves works again".to_string(),
        ]
    );
}

#[test]
fn wrt1_reads_do_not_wait_for_a_slow_write() {
    let (_d, r) = project();
    let shared = Shared::new(r);
    let e = shared.engine();
    let started = Arc::new(Barrier::new(2));
    let s2 = Arc::clone(&started);
    let e2 = Arc::clone(&e);
    let slow = std::thread::spawn(move || {
        e2.write(Box::new(move |r| {
            s2.wait();
            std::thread::sleep(Duration::from_millis(400));
            doc(r, "work", "slow").map(|_| serde_json::Value::Null)
        }))
        .unwrap();
    });
    started.wait();
    let t = Instant::now();
    for _ in 0..50 {
        let st = e.read();
        st.tree(&Scope::Canon, None, None).unwrap();
    }
    assert!(
        t.elapsed() < Duration::from_millis(200),
        "reads waited behind the write: {:?}",
        t.elapsed()
    );
    slow.join().unwrap();
    assert_eq!(e.read().included("work").len(), 1);
    shared.shutdown().unwrap();
}

#[test]
fn wrt1_shutdown_closes_the_store_and_later_writes_are_daemon_gone() {
    let (d, r) = project();
    let shared = Shared::new(r);
    let e = shared.engine();
    write_typed(&*e, |r| doc(r, "work", "a")).unwrap();
    shared.shutdown().unwrap();
    shared.shutdown().unwrap();
    assert!(!shared.writer.is_alive());
    let err = e
        .write(Box::new(|_| Ok(serde_json::Value::Null)))
        .unwrap_err();
    assert_eq!(err.code, E_DAEMON_GONE);
    // the store is closed: it reopens cleanly with no catch-up
    drop(e);
    drop(shared);
    let r = Rdy::open(d.path()).unwrap();
    assert!(!r.open_report().unclean_shutdown);
    assert_eq!(r.open_report().caught_up_frames, 0);
}

#[test]
fn wrt1_the_idle_tick_checkpoints_without_a_write() {
    let (d, mut r) = project();
    r.store_mut().set_checkpoint_frames(1_000_000);
    doc(&mut r, "work", "a").unwrap();
    let before = r.store().checkpoint_count();
    let shared = Shared::new(r);
    // polled with a deadline (the store's idle rule is 2 s): each poll is a message, which
    // restarts the writer's tick wait, so polls are two ticks apart to leave the tick room to
    // fire between them; a starved tick only makes the poll take longer
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let after = shared
            .engine()
            .write(Box::new(|r| {
                Ok(serde_json::json!(r.store().checkpoint_count()))
            }))
            .unwrap();
        if after.as_u64().unwrap() > before {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no checkpoint from the tick within 30 s"
        );
        std::thread::sleep(CHECKPOINT_TICK * 2);
    }
    shared.shutdown().unwrap();
    drop(d);
}

#[test]
fn wrt1_session_end_hooks_run_once_per_end() {
    let (_d, r) = project();
    let shared = Shared::new(r);
    let seen = Arc::new(std::sync::Mutex::new(vec![]));
    let s2 = Arc::clone(&seen);
    shared.on_session_end(move |id| s2.lock().unwrap().push(id));
    shared.end_session(rdy_daemon::SessionId(7));
    shared.end_session(rdy_daemon::SessionId(9));
    assert_eq!(
        seen.lock().unwrap().clone(),
        vec![rdy_daemon::SessionId(7), rdy_daemon::SessionId(9)]
    );
    shared.shutdown().unwrap();
}

/// Blocks while `HOLD` is set (until `HOLD` clears or a deadline passes) and counts its calls
/// in `HELD`: a tension build the test can catch in the middle.
struct Hold;
static HOLD: AtomicBool = AtomicBool::new(false);
static HELD: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static HOLD_DETECTORS: [&dyn Detector; 1] = [&Hold];
impl Detector for Hold {
    fn kind(&self) -> TensionKind {
        TensionKind::EditCollision
    }
    fn detect(
        &self,
        _cx: &DetectCx<'_>,
        _seeds: Option<&BTreeSet<BlockId>>,
        _out: &mut Vec<Found>,
    ) -> Result<(), rdy_engine::RdyError> {
        if HOLD.load(Ordering::SeqCst) {
            HELD.fetch_add(1, Ordering::SeqCst);
            let deadline = Instant::now() + Duration::from_secs(10);
            while HOLD.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        Ok(())
    }
}

/// Perf fix (daemon_read_during_big_write): the writer builds the tension set of the state it
/// publishes before it publishes it, so the first read after a big write never builds it.
#[test]
fn wrt1_the_writer_builds_the_tension_set_before_it_publishes_the_state() {
    let (_d, mut r) = project();
    r.set_detectors_for_tests(&HOLD_DETECTORS);
    let shared = Shared::new(r);
    let e = shared.engine();
    write_typed(&*e, |r| doc(r, "work", "one")).unwrap();
    // the reload message runs after the first write's settle: nothing is building now
    shared.writer.reload().unwrap();
    assert!(e.read().tensions_built());
    HOLD.store(true, Ordering::SeqCst);
    let e2 = Arc::clone(&e);
    let w = std::thread::spawn(move || write_typed(&*e2, |r| doc(r, "work", "two")).unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    while HELD.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        HELD.load(Ordering::SeqCst) > 0,
        "the second write built no tension set"
    );
    // the build is held: a state that holds the second write is published only once built
    let st = e.read();
    let published = st.included("work").len() == 2;
    let built = st.tensions_built();
    HOLD.store(false, Ordering::SeqCst);
    w.join().unwrap();
    assert!(
        !published || built,
        "the writer published a state whose tension set is not built"
    );
    let st = e.read();
    assert_eq!(st.included("work").len(), 2);
    assert!(st.tensions_built());
    shared.shutdown().unwrap();
}
