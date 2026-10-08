//! The daemon area, transport part (race1): real client processes and threads against a daemon
//! in a child process, auto-start races, a stale socket, hostile hellos and lines.
use rdy_daemon::*;
use rdy_qa::daemon::*;
use serde_json::json;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// The re-exec child. Not a test of anything: it returns at once in a normal run.
#[test]
fn race1_child() {
    child_daemon_entry();
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn race1_eight_clients_against_a_cold_project_start_one_daemon_and_all_reach_it() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let (spawn, count) = spawn_child_daemon(
        &rdy_qa::test_name!("race1_child"),
        &run,
        "1",
        &[("RDY_DAEMON_PROGRESS_MS", "300")],
    );
    let pids: Vec<u64> = rt().block_on(async {
        let mut tasks = vec![];
        for _ in 0..8 {
            let (run, cwd, spawn) = (run.clone(), dir.path().to_path_buf(), spawn.clone());
            tasks.push(tokio::spawn(async move {
                let mut c = connect_or_start(&run, client(&run, "1", &cwd, spawn))
                    .await
                    .unwrap();
                let pid = c.ctl("ping", json!({})).await.unwrap()["pid"]
                    .as_u64()
                    .unwrap();
                c.ctl("count", json!({})).await.unwrap();
                pid
            }));
        }
        let mut out = vec![];
        for t in tasks {
            out.push(t.await.unwrap());
        }
        out
    });
    assert_eq!(count.load(Ordering::SeqCst), 1, "one spawn");
    assert!(
        pids.iter().all(|p| *p == pids[0]) && pids[0] != u64::from(std::process::id()),
        "{pids:?}"
    );
    rt().block_on(async {
        let mut c = connect_or_start(&run, client(&run, "1", dir.path(), spawn.clone()))
            .await
            .unwrap();
        assert_eq!(c.ctl("ping", json!({})).await.unwrap()["sessions"], 1);
        c.ctl("stop", json!({})).await.unwrap();
    });
    wait_gone(&run);
}

#[test]
fn race1_eight_clients_against_a_stale_socket_start_one_daemon() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    run.ensure().unwrap();
    drop(std::os::unix::net::UnixListener::bind(run.socket()).unwrap());
    let (spawn, count) = spawn_child_daemon(&rdy_qa::test_name!("race1_child"), &run, "1", &[]);
    rt().block_on(async {
        let mut tasks = vec![];
        for _ in 0..8 {
            let (run, cwd, spawn) = (run.clone(), dir.path().to_path_buf(), spawn.clone());
            tasks.push(tokio::spawn(async move {
                let mut c = connect_or_start(&run, client(&run, "1", &cwd, spawn))
                    .await
                    .unwrap();
                c.ctl("echo", json!({})).await.unwrap();
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        let mut c = connect_or_start(&run, client(&run, "1", dir.path(), spawn.clone()))
            .await
            .unwrap();
        c.ctl("stop", json!({})).await.unwrap();
    });
    assert_eq!(count.load(Ordering::SeqCst), 1);
    wait_gone(&run);
}

#[test]
fn race1_a_killed_daemon_is_replaced_on_the_next_client() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let (spawn, count) = spawn_child_daemon(&rdy_qa::test_name!("race1_child"), &run, "1", &[]);
    let pid1 = rt().block_on(async {
        let mut c = connect_or_start(&run, client(&run, "1", dir.path(), spawn.clone()))
            .await
            .unwrap();
        c.ctl("ping", json!({})).await.unwrap()["pid"]
            .as_u64()
            .unwrap()
    });
    kill9(pid1);
    std::thread::sleep(Duration::from_millis(200));
    assert!(run.socket().exists(), "kill -9 leaves the socket file");
    let pid2 = rt().block_on(async {
        let mut c = connect_or_start(&run, client(&run, "1", dir.path(), spawn.clone()))
            .await
            .unwrap();
        let pid = c.ctl("ping", json!({})).await.unwrap()["pid"]
            .as_u64()
            .unwrap();
        c.ctl("stop", json!({})).await.unwrap();
        pid
    });
    assert_ne!(pid1, pid2);
    assert_eq!(count.load(Ordering::SeqCst), 2);
    wait_gone(&run);
}

/// SIGKILL through the shell: rdy-qa has no libc dependency.
fn kill9(pid: u64) {
    assert!(
        std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn race1_a_detached_daemon_idles_out_and_the_next_client_starts_a_new_one() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let (spawn, count) = spawn_child_daemon(
        &rdy_qa::test_name!("race1_child"),
        &run,
        "1",
        &[("RDY_DAEMON_IDLE_MS", "500")],
    );
    rt().block_on(async {
        let mut c = connect_or_start(&run, client(&run, "1", dir.path(), spawn.clone()))
            .await
            .unwrap();
        c.ctl("echo", json!({})).await.unwrap();
    });
    wait_gone(&run);
    rt().block_on(async {
        let mut c = connect_or_start(&run, client(&run, "1", dir.path(), spawn.clone()))
            .await
            .unwrap();
        c.ctl("stop", json!({})).await.unwrap();
    });
    assert_eq!(count.load(Ordering::SeqCst), 2);
    wait_gone(&run);
}

#[test]
fn race1_version_mismatch_idle_daemon_is_replaced_busy_one_refuses() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let (spawn_old, _) = spawn_child_daemon(&rdy_qa::test_name!("race1_child"), &run, "0.1.0", &[]);
    let (spawn_new, count_new) =
        spawn_child_daemon(&rdy_qa::test_name!("race1_child"), &run, "0.2.0", &[]);
    rt().block_on(async {
        let old = connect_or_start(&run, client(&run, "0.1.0", dir.path(), spawn_old.clone()))
            .await
            .unwrap();
        let e = connect_or_start(&run, client(&run, "0.2.0", dir.path(), spawn_new.clone()))
            .await
            .unwrap_err();
        assert_eq!(e.code, E_DAEMON_VERSION);
        assert_eq!(count_new.load(Ordering::SeqCst), 0);
        old.close();
        // the old daemon steps aside once it has dropped `old`'s session; until then a newer
        // client is refused as before (and spawns nothing): poll, with a generous deadline
        let t0 = std::time::Instant::now();
        let mut new = loop {
            match connect_or_start(&run, client(&run, "0.2.0", dir.path(), spawn_new.clone())).await
            {
                Ok(c) => break c,
                Err(e) if e.code == E_DAEMON_VERSION && t0.elapsed() < Duration::from_secs(30) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => panic!("the old daemon was not replaced: {e:?}"),
            }
        };
        assert_eq!(new.version(), "0.2.0");
        assert_eq!(count_new.load(Ordering::SeqCst), 1);
        new.ctl("stop", json!({})).await.unwrap();
    });
    wait_gone(&run);
}

#[test]
fn race1_hostile_hellos_and_lines_never_take_the_daemon_down() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let (spawn, _) = spawn_child_daemon(&rdy_qa::test_name!("race1_child"), &run, "1", &[]);
    rt().block_on(async {
        let mut c = connect_or_start(&run, client(&run, "1", dir.path(), spawn.clone())).await.unwrap();
        let junk: Vec<Vec<u8>> = vec![
            b"\n".to_vec(),
            b"\xff\xfe\x00garbage\n".to_vec(),
            vec![b'{'; 5000],
            format!("{{\"rdy\":1,\"version\":\"1\",\"proto\":\"ctl\",\"cwd\":\"{}\"}}\n{{\"id\":1,\"op\":\"echo\",\"args\":{}}}\n", dir.path().display(), "[".repeat(100_000)).into_bytes(),
            format!("{{\"rdy\":1,\"version\":\"1\",\"proto\":\"ctl\",\"cwd\":\"{}\"}}\n{}", dir.path().display(), "x".repeat(17 * 1024 * 1024)).into_bytes(),
            format!("{{\"rdy\":1,\"version\":\"1\",\"proto\":\"mcp\",\"cwd\":\"{}\"}}\n{{\"jsonrpc\":\"2.0\"}}\n", dir.path().display()).into_bytes(),
        ];
        for bytes in junk {
            let mut s = UnixStream::connect(run.socket()).await.unwrap();
            let _ = s.write_all(&bytes).await;
            let mut r = BufReader::new(s);
            let mut out = String::new();
            let _ = tokio::time::timeout(Duration::from_secs(7), r.read_line(&mut out)).await;
            drop(r);
            let v = c.ctl("ping", json!({})).await.unwrap();
            assert_eq!(v["version"], "1", "alive after {:.40?}", String::from_utf8_lossy(&bytes));
        }
        // the junk connections end on the daemon's side in their own time: poll until only
        // this one is left, with a generous deadline (a leak never gets there)
        let t0 = std::time::Instant::now();
        loop {
            let v = c.ctl("ping", json!({})).await.unwrap();
            if v["sessions"] == 1 && v["connections"] == 1 {
                break;
            }
            assert!(t0.elapsed() < Duration::from_secs(30), "a session or connection leaked: {v}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        c.ctl("stop", json!({})).await.unwrap();
    });
    wait_gone(&run);
}

#[test]
fn race1_two_hundred_pipelined_requests_from_four_clients_all_answer_in_order() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let (spawn, _) = spawn_child_daemon(&rdy_qa::test_name!("race1_child"), &run, "1", &[]);
    rt().block_on(async {
        let mut tasks = vec![];
        for k in 0..4u64 {
            let (run, cwd, spawn) = (run.clone(), dir.path().to_path_buf(), spawn.clone());
            tasks.push(tokio::spawn(async move {
                let c = connect_or_start(&run, client(&run, "1", &cwd, spawn))
                    .await
                    .unwrap();
                let mut s = BufReader::new(c.into_stream());
                let mut lines = String::new();
                for i in 0..200u64 {
                    lines.push_str(
                        &json!({"id": k * 1000 + i, "op": "echo", "args": {"i": i}}).to_string(),
                    );
                    lines.push('\n');
                }
                s.get_mut().write_all(lines.as_bytes()).await.unwrap();
                for i in 0..200u64 {
                    let mut out = String::new();
                    s.read_line(&mut out).await.unwrap();
                    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
                    assert_eq!(v["id"], k * 1000 + i, "{out}");
                }
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        let mut c = connect_or_start(&run, client(&run, "1", dir.path(), spawn.clone()))
            .await
            .unwrap();
        c.ctl("stop", json!({})).await.unwrap();
    });
    wait_gone(&run);
}

#[test]
fn race1_children_spawned_by_other_threads_during_the_start_lock_handoff_do_not_keep_it() {
    use std::sync::{Arc, atomic::AtomicBool};
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let (spawn, count) = spawn_child_daemon(
        &rdy_qa::test_name!("race1_child"),
        &run,
        "1",
        &[("RDY_DAEMON_PROGRESS_MS", "200")],
    );
    let stop = Arc::new(AtomicBool::new(false));
    // two threads spawn short-lived children nonstop while eight clients race on start.lock
    let churn: Vec<_> = (0..2)
        .map(|_| {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut kids = vec![];
                while !stop.load(Ordering::SeqCst) {
                    kids.push(
                        std::process::Command::new("sleep")
                            .arg("0.3")
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn()
                            .unwrap(),
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                for mut k in kids {
                    let _ = k.wait();
                }
            })
        })
        .collect();
    rt().block_on(async {
        let mut tasks = vec![];
        for _ in 0..8 {
            let (run, cwd, spawn) = (run.clone(), dir.path().to_path_buf(), spawn.clone());
            tasks.push(tokio::spawn(async move {
                let mut c = connect_or_start(&run, client(&run, "1", &cwd, spawn))
                    .await
                    .unwrap();
                c.ctl("ping", json!({})).await.unwrap();
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
    });
    // still churning: the lock must already be free, not freed when the last child execs
    assert!(
        rdy_daemon::lock::FileLock::try_take(&run.start_lock())
            .unwrap()
            .is_some(),
        "start.lock is free after the race"
    );
    stop.store(true, Ordering::SeqCst);
    for t in churn {
        t.join().unwrap();
    }
    assert_eq!(count.load(Ordering::SeqCst), 1, "one spawn");
    rt().block_on(async {
        let mut c = connect_or_start(&run, client(&run, "1", dir.path(), spawn.clone()))
            .await
            .unwrap();
        c.ctl("stop", json!({})).await.unwrap();
    });
    wait_gone(&run);
}

/// Waits until the daemon's socket is gone (it unlinks it on exit).
fn wait_gone(run: &RunDir) {
    let t0 = std::time::Instant::now();
    while run.socket().exists() {
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "daemon did not exit"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
