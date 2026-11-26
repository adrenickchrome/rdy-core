//! The destination daemon's history steps (D-GGTYA, O17): each step runs on the blocking pool,
//! one awaited step at a time, while the writer keeps serving person calls; a call queued on the
//! writer runs before the next step; a stop ends the fetch at a step boundary and releases what
//! the fetch owns. The steps here stand in for the engine's (a held get, then completion); the
//! engine's own steps are pinned in rdy-engine's hub_sync tests.
use rdy_daemon::Shared;
use rdy_daemon::history_fetch::drive;
use rdy_engine::engine::write_typed;
use rdy_engine::history_fetch::HistoryFetchStep;
use rdy_engine::partial_history::HistoryProgress;
use rdy_engine::{Rdy, Session, WriteRequest};
use rdy_model::{Hash, Role};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

fn doc(r: &mut Rdy, name: &str) -> Result<rdy_engine::WriteResult, rdy_engine::RdyError> {
    let ops = serde_json::from_value(serde_json::json!([
        {"op": "create", "type": "doc", "fields": {"name": name, "title": "D", "doc_type": "page"}}
    ]))
    .unwrap();
    r.write(
        &Session::human("human:t".into(), Role::Writer).unwrap(),
        WriteRequest {
            view: "work".into(),
            ops,
            justification: None,
            message: String::new(),
        },
    )
}

fn progress(held: u64, complete: bool) -> HistoryProgress {
    HistoryProgress {
        frontier: Hash::of(b"frontier"),
        complete,
        held,
        total: 3,
    }
}

/// What a fetch owns: its drop is the guard's release.
struct Owned(Arc<AtomicBool>);

impl Drop for Owned {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

/// A held get (step 1) leaves the writer free: a person read and write both finish while it is
/// held. A person write queued when the get ends runs before step 2 starts. A stop seen at a
/// step boundary ends the fetch, drops what it owns, and runs no further step; a new fetch (the
/// next start) resumes from the step after.
#[test]
fn r59018_daemon_yields_between_history_steps() {
    let (_d, r) = project();
    let shared = Shared::new(r);
    let engine = shared.engine();
    let rt = runtime();
    let (held_tx, held_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let held_tx = Mutex::new(held_tx);
    let steps = Arc::new(AtomicUsize::new(0));
    let person_done = Arc::new(AtomicBool::new(false));
    let seen_at_step_two = Arc::new(Mutex::new(None::<bool>));
    let released = Arc::new(AtomicBool::new(false));
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let job = {
        let shared = Arc::clone(&shared);
        let steps = Arc::clone(&steps);
        let person_done = Arc::clone(&person_done);
        let seen = Arc::clone(&seen_at_step_two);
        let stop_tx = stop_tx.clone();
        let owned = Owned(Arc::clone(&released));
        rt.spawn(async move {
            drive(&shared, stop_rx, owned, move |_owned: &mut Owned| {
                let n = steps.fetch_add(1, Ordering::SeqCst) + 1;
                match n {
                    1 => {
                        // the held get: on the blocking pool, off the writer
                        held_tx.lock().unwrap().send(()).unwrap();
                        release_rx.lock().unwrap().recv().unwrap();
                        Ok(HistoryFetchStep::More(progress(1, false)))
                    }
                    2 => {
                        *seen.lock().unwrap() = Some(person_done.load(Ordering::SeqCst));
                        // the stop arrives while step 2 runs: no step 3 starts
                        stop_tx.send(true).unwrap();
                        Ok(HistoryFetchStep::More(progress(2, false)))
                    }
                    _ => Ok(HistoryFetchStep::Complete(progress(3, true))),
                }
            })
            .await
        })
    };
    held_rx.recv_timeout(Duration::from_secs(60)).unwrap();
    // while the get is held: a person read and a person write
    let read = engine.read();
    drop(read);
    write_typed(&*engine, |r| doc(r, "during-get")).unwrap();
    assert!(engine.read().included("work").len() == 1);
    // a person write on the writer when the get ends: it holds the writer until the test
    // opens its gate, and step 2 must not start before it ran
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let writer_busy = {
        let engine = Arc::clone(&engine);
        let done = Arc::clone(&person_done);
        let (queued_tx, queued_rx) = mpsc::channel::<()>();
        let t = std::thread::spawn(move || {
            write_typed(&*engine, move |r| {
                queued_tx.send(()).unwrap();
                gate_rx.recv_timeout(Duration::from_secs(60)).unwrap();
                let out = doc(r, "queued");
                done.store(true, Ordering::SeqCst);
                out
            })
            .unwrap();
        });
        queued_rx.recv_timeout(Duration::from_secs(60)).unwrap();
        t
    };
    // a second write queued behind it, still in the queue when the get ends
    let second = {
        let engine = Arc::clone(&engine);
        std::thread::spawn(move || {
            write_typed(&*engine, |r| doc(r, "queued-two")).unwrap();
        })
    };
    while shared.writer.queue_len() == 0 {
        std::thread::yield_now();
    }
    release_tx.send(()).unwrap();
    // the get ended: the fetch's yield queues behind the second write (two queued), unless a
    // step 2 that did not wait has started already
    while shared.writer.queue_len() < 2 && steps.load(Ordering::SeqCst) < 2 {
        std::thread::yield_now();
    }
    gate_tx.send(()).unwrap();
    let ended = rt.block_on(job).unwrap();
    writer_busy.join().unwrap();
    second.join().unwrap();
    assert!(ended.is_none(), "a stop ends the fetch: {ended:?}");
    assert_eq!(steps.load(Ordering::SeqCst), 2, "no step after the stop");
    assert_eq!(*seen_at_step_two.lock().unwrap(), Some(true));
    assert!(
        released.load(Ordering::SeqCst),
        "the stop released the fetch"
    );
    assert_eq!(engine.read().included("work").len(), 3);
    // the next start's fetch resumes and completes
    let (_stop, stop_rx) = tokio::sync::watch::channel(false);
    let again = Arc::new(AtomicBool::new(false));
    let ended = rt.block_on(drive(&shared, stop_rx, Owned(Arc::clone(&again)), |_| {
        Ok(HistoryFetchStep::Complete(progress(3, true)))
    }));
    assert_eq!(ended.unwrap().unwrap(), progress(3, true));
    assert!(again.load(Ordering::SeqCst));
    drop(rt);
    shared.shutdown().unwrap();
}

/// O17, D-GGTYA: the CLI clone's check once its history is complete
/// (`check_after_history`, which `serve_clone_history` runs before the daemon stops) holds the
/// writer only for its capture. Its frame walk, its tag rules and its state checks run on the
/// calling thread: a person write that arrives at each of them is served while the check waits.
#[test]
fn r59019_clone_check_after_history_serves_queued_writes() {
    let (_d, r) = project();
    let shared = Shared::new(r);
    let engine = shared.engine();
    let served: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    let hook = {
        let engine = Arc::clone(&engine);
        let served = Arc::clone(&served);
        let n = AtomicUsize::new(0);
        Box::new(move |point: &'static str| {
            let name = format!("during-{point}-{}", n.fetch_add(1, Ordering::SeqCst));
            let engine = Arc::clone(&engine);
            let (tx, rx) = mpsc::channel::<()>();
            let t = std::thread::spawn(move || {
                write_typed(&*engine, move |r| doc(r, &name)).unwrap();
                tx.send(()).unwrap();
            });
            // a check on the writer would hold this write until the check ends
            if rx.recv_timeout(Duration::from_secs(60)).is_ok() {
                served.lock().unwrap().push(point);
            }
            t.join().unwrap();
        })
    };
    let report = rdy_engine::history_fetch::with_check_hook_for_tests(hook, || {
        rdy_daemon::history_fetch::check_after_history(&*engine)
    })
    .unwrap();
    assert_eq!(*served.lock().unwrap(), ["frames", "tags", "state"]);
    assert!(report.frames > 0);
    assert_eq!(engine.read().included("work").len(), 3);
    shared.shutdown().unwrap();
}
