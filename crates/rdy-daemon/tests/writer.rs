//! wrt1: one writer thread; the published state is stored before the reply; a panic answers and the
//! writer keeps serving; shutdown closes the store once.
use rdy_daemon::{DaemonEngine, E_DAEMON_GONE, Shared, WriterHandle};
use rdy_engine::engine::write_typed;
use rdy_engine::{E_INTERNAL, Rdy, Scope, Session, WriteRequest};
use rdy_model::Role;
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
        {"op": "create", "type": "doc", "fields": {"name": name, "title": "D", "doc_type": "glossary"}}
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
    // the store's idle rule (2 s) plus two ticks
    std::thread::sleep(Duration::from_millis(2_600));
    let after = shared
        .engine()
        .write(Box::new(|r| {
            Ok(serde_json::json!(r.store().checkpoint_count()))
        }))
        .unwrap();
    assert!(
        after.as_u64().unwrap() > before,
        "no checkpoint from the tick"
    );
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
