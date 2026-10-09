//! kill2 (spec 3.7, 15.2): a daemon killed at any point loses no acknowledged write; a kill leads
//! to catch-up, not reindex; clients see E_DAEMON_GONE once and reconnect to exactly one successor.
use rdy_daemon::{
    Attempt, E_DAEMON_GONE, E_DAEMON_START, Hello, PROTO_VERSION, Proto, RunDir, connect,
};
use rdy_engine::{Rdy, Scope};
use rdy_qa::daemon::{Served, human_command};
use rdy_qa::fixtures::deep;
use rdy_qa::invariants::check;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn lcg(seed: &mut u64) -> u64 {
    *seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *seed >> 33
}

fn names_in(s: &Served, scope: &str) -> Vec<String> {
    let (ok, v) = s.json(&["--json", "ls", "--scope", scope]);
    assert!(ok, "{v}");
    v.as_array()
        .unwrap()
        .iter()
        .map(|n| n["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn kill2_a_daemon_killed_mid_write_loses_no_acknowledged_write_and_never_duplicates_one() {
    let s = Served::new();
    let rounds = if deep() { 20 } else { 6 };
    let mut seed = 0x6b69u64;
    let mut acked = vec![];
    let mut gone = 0;
    let mut pids = std::collections::BTreeSet::new();
    for round in 0..rounds {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let acks = Arc::new(Mutex::new(vec![]));
        let failures = Arc::new(Mutex::new(vec![]));
        let worker = {
            let (stop, acks, failures) = (stop.clone(), acks.clone(), failures.clone());
            let s_dir = s.dir.path().to_path_buf();
            let env = s.env_pairs();
            std::thread::spawn(move || {
                let mut i = 0;
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    let name = format!("r{round}-{i}");
                    let out = human_command(&s_dir, &env, &["--json", "new", "doc", &name])
                        .output()
                        .unwrap();
                    if out.status.success() {
                        acks.lock().unwrap().push(name);
                    } else {
                        let v: Value = serde_json::from_slice(&out.stderr).unwrap_or(Value::Null);
                        failures
                            .lock()
                            .unwrap()
                            .push((name, v["code"].as_str().unwrap_or("?").to_string()));
                    }
                    i += 1;
                }
            })
        };
        // let the daemon come up and some writes land, then kill it mid-stream
        std::thread::sleep(Duration::from_millis(300 + lcg(&mut seed) % 400));
        if let Some(pid) = s.daemon_pid() {
            pids.insert(pid);
            s.kill9();
        }
        std::thread::sleep(Duration::from_millis(200 + lcg(&mut seed) % 300));
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        worker.join().unwrap();
        acked.extend(acks.lock().unwrap().iter().cloned());
        let f = failures.lock().unwrap().clone();
        assert!(
            f.len() <= 2,
            "round {round}: more than the in-flight request and one connect failed: {f:?}"
        );
        for (_, code) in &f {
            assert!(
                code == E_DAEMON_GONE || code == E_DAEMON_START,
                "round {round}: {code}"
            );
            if code == E_DAEMON_GONE {
                gone += 1;
            }
        }
    }
    // the next command starts a successor; every acknowledged doc is there, exactly once
    let names = names_in(&s, "work");
    for n in &acked {
        assert_eq!(
            names.iter().filter(|x| *x == n).count(),
            1,
            "acknowledged doc {n} missing or duplicated"
        );
    }
    let (ok, st) = s.json(&["--json", "status"]);
    assert!(ok);
    assert_eq!(
        st["open_report"]["index_rebuilt"], false,
        "a kill is catch-up, not reindex: {st}"
    );
    let (ok, f) = s.json(&["--json", "fsck"]);
    assert!(ok, "{f}");
    eprintln!(
        "kill2: {} rounds, {} acked, {} E_DAEMON_GONE, {} daemons",
        rounds,
        acked.len(),
        gone,
        pids.len()
    );
}

#[test]
fn kill2_an_in_flight_request_is_daemon_gone_with_the_may_have_committed_fix() {
    let s = Served::new();
    s.json(&["--json", "status"]);
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let hello = Hello {
            rdy: PROTO_VERSION,
            version: env!("CARGO_PKG_VERSION").into(),
            proto: Proto::Ctl,
            cwd: s.dir.path().into(),
        };
        let Attempt::Ready(mut c) = connect(&RunDir::of_project(s.dir.path()), &hello).await else {
            panic!("no daemon")
        };
        c.ctl("session", json!({"actor": null, "run": null}))
            .await
            .unwrap();
        s.kill9();
        let e = c
            .ctl("new_doc", json!({"path": "x", "view": null}))
            .await
            .unwrap_err();
        assert_eq!(e.code, E_DAEMON_GONE);
        assert!(e.fix.contains("may or may not have committed"), "{e}");
        let e2 = c.ctl("status", json!({})).await.unwrap_err();
        assert_eq!(
            e2.code, E_DAEMON_GONE,
            "every later call on a gone connection"
        );
    });
    let (ok, _) = s.json(&["--json", "status"]);
    assert!(ok, "a new client starts a successor");
}

#[test]
fn kill2_killed_during_landings_keeps_canon_consistent_and_catches_up() {
    let s = Served::new();
    let rounds = if deep() { 8 } else { 3 };
    let mut landed = 0usize;
    let mut seed = 0x6c616eu64;
    for round in 0..rounds {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let count = Arc::new(Mutex::new(0usize));
        let worker = {
            let (stop, count) = (stop.clone(), count.clone());
            let s_dir = s.dir.path().to_path_buf();
            let env = s.env_pairs();
            std::thread::spawn(move || {
                let run = |args: &[&str]| -> (bool, Value) {
                    let args: Vec<&str> = std::iter::once("--json")
                        .chain(args.iter().copied())
                        .collect();
                    let out = human_command(&s_dir, &env, &args).output().unwrap();
                    let text = if out.status.success() {
                        &out.stdout
                    } else {
                        &out.stderr
                    };
                    (
                        out.status.success(),
                        serde_json::from_slice(text).unwrap_or(Value::Null),
                    )
                };
                let mut i = 0;
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    let view = format!("v{round}-{i}");
                    if !run(&["new", "doc", &format!("l{round}-{i}"), "--view", &view]).0 {
                        i += 1;
                        continue;
                    }
                    let (ok, p) = run(&["propose", "--title", "t", "--view", &view]);
                    if !ok {
                        i += 1;
                        continue;
                    }
                    if run(&["approve", p["id"].as_str().unwrap()]).0 {
                        *count.lock().unwrap() += 1;
                    }
                    i += 1;
                }
            })
        };
        std::thread::sleep(Duration::from_millis(400 + lcg(&mut seed) % 400));
        if s.daemon_pid().is_some() {
            s.kill9();
        }
        std::thread::sleep(Duration::from_millis(100));
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        worker.join().unwrap();
        landed += *count.lock().unwrap();
    }
    let (ok, log) = s.json(&["--json", "log"]);
    assert!(ok);
    assert!(
        log.as_array().unwrap().len() >= landed,
        "acknowledged landings missing: {} < {landed}",
        log.as_array().unwrap().len()
    );
    let (ok, st) = s.json(&["--json", "status"]);
    assert!(ok);
    assert_eq!(st["open_report"]["index_rebuilt"], false);
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    check(&r, &Scope::Canon).unwrap();
    for v in r.read().view_names() {
        check(&r, &Scope::View(v)).unwrap();
    }
}

#[test]
fn kill2_clients_reconnect_and_exactly_one_successor_replaces_the_dead_daemon() {
    let s = Served::new();
    s.json(&["--json", "status"]);
    let first = s.daemon_pid().unwrap();
    // kill while writes are in flight: wait until the clients have acked some writes (polled
    // with a deadline), not a fixed sleep, which on a fast machine let every write finish first
    const PER_THREAD: usize = 60;
    let acked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let results: Vec<_> = (0..4)
        .map(|t| {
            let s_dir = s.dir.path().to_path_buf();
            let env = s.env_pairs();
            let acked = acked.clone();
            std::thread::spawn(move || {
                let mut failures = 0;
                for i in 0..PER_THREAD {
                    let out = human_command(
                        &s_dir,
                        &env,
                        &["--json", "new", "doc", &format!("t{t}-{i}")],
                    )
                    .output()
                    .unwrap();
                    if out.status.success() {
                        acked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    } else {
                        failures += 1;
                    }
                }
                failures
            })
        })
        .collect();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while acked.load(std::sync::atomic::Ordering::SeqCst) < 20
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        acked.load(std::sync::atomic::Ordering::SeqCst) < 4 * PER_THREAD,
        "every write finished before the kill; the test needs writes in flight"
    );
    s.kill9();
    let failures: Vec<usize> = results.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(failures.iter().all(|f| *f <= 2), "{failures:?}");
    let second = s.daemon_pid().expect("a successor");
    assert_ne!(first, second);
    let (ok, v) = s.json(&["--json", "status"]);
    assert!(ok);
    let changes = v["views"][0]["changes"].as_u64().unwrap();
    let failed = failures.iter().sum::<usize>() as u64;
    // a failed command may still have committed (its ack was lost), never the reverse
    assert!(
        changes + failed >= (4 * PER_THREAD) as u64 && changes <= (4 * PER_THREAD) as u64,
        "{changes} changes, {failed} failures: {v}"
    );
    assert_eq!(s.daemon_pid(), Some(second), "exactly one successor");
}

#[test]
fn kill2_a_kill_in_the_checkpoint_window_is_catch_up_not_reindex() {
    if !deep() {
        return;
    }
    let s = Served::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    s.json(&["--json", "status"]);
    let acked = rt.block_on(async {
        let hello = Hello {
            rdy: PROTO_VERSION,
            version: env!("CARGO_PKG_VERSION").into(),
            proto: Proto::Ctl,
            cwd: s.dir.path().into(),
        };
        let Attempt::Ready(mut c) = connect(&RunDir::of_project(s.dir.path()), &hello).await else {
            panic!("no daemon")
        };
        c.ctl("session", json!({"actor": null, "run": null}))
            .await
            .unwrap();
        let mut n = 0;
        let t = Instant::now();
        // past the 1,000-frame checkpoint, then kill from here while writes continue
        while n < 1_300 {
            if c.ctl("new_doc", json!({"path": format!("c{n}"), "view": null}))
                .await
                .is_err()
            {
                break;
            }
            n += 1;
            if n == 1_050 {
                s.kill9();
            }
            // a hang guard, not a speed check: since the daemon tests share one binary, 1,300
            // fsync'd writes beside the other daemon tests took over 120 s on the bench (2026-10-08)
            assert!(
                t.elapsed() < Duration::from_secs(600),
                "the write loop hung"
            );
        }
        n
    });
    assert!(acked >= 1_050, "{acked}");
    let names = names_in(&s, "work");
    assert!(
        names.len() >= acked,
        "{} < {acked}: an acknowledged write is missing",
        names.len()
    );
    let (ok, st) = s.json(&["--json", "status"]);
    assert!(ok);
    assert_eq!(st["open_report"]["index_rebuilt"], false, "{st}");
    assert!(
        st["open_report"]["caught_up_frames"].as_u64().unwrap() > 0,
        "{st}"
    );
}

#[test]
fn msw6_a_worker_command_strips_the_session_and_direct_mode_env() {
    let s = Served::new();
    let cmd = rdy_qa::daemon::human_command(s.dir.path(), &s.env_pairs(), &["--json", "status"]);
    let removed: Vec<String> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    for k in ["RDY_ACTOR", "RDY_RUN", "RDY_NO_DAEMON"] {
        assert!(
            removed.iter().any(|r| r == k),
            "{k} leaks into the worker: {removed:?}"
        );
    }
}
