//! serve1: the host opens, binds, warms, answers ready; sessions are classified; MCP runs over
//! the socket; stop closes the store.
use rdy_daemon::{
    E_NO_SESSION, ExitReason, Hello, PROTO_VERSION, Proto, RunDir, ServeError, ServeOptions,
    connect, start,
};
use rdy_engine::{E_BAD_INPUT, E_PERMISSION, E_VIEW_OWNER, Rdy};
use rdy_qa::daemon::{Hosted, short_tempdir};
use serde_json::json;
use std::time::Duration;

const ACTOR_FIX: &str =
    "use human:<name>, agent:<name>, agent:<handle>/<name> or ci:<repo_id>/<name>";
const RUN_FIX: &str = "use 1 to 64 letters, digits, -, _ or ., with a letter or digit at each end; not none, work, canon, inflight or reaffirm-\u{2026}";

#[test]
fn serve1_start_binds_after_the_store_is_open_and_a_second_start_is_already_running() {
    let mut h = Hosted::new();
    let run = RunDir::of_project(h.dir.path());
    assert!(run.socket().exists());
    assert!(run.pid_file().exists());
    let e = start(ServeOptions {
        project: h.dir.path().into(),
        version: "t".into(),
        detach: false,
        idle_timeout: None,
        human_actor: None,
        site_lan_override: None,
    })
    .err()
    .unwrap();
    assert!(matches!(e, ServeError::AlreadyRunning(_)), "{e}");
    assert!(
        run.socket().exists(),
        "the loser never touched the winner's socket"
    );
    assert_eq!(h.stop(), ExitReason::Stopped);
    assert!(!run.socket().exists());
    let r = Rdy::open(h.dir.path()).unwrap();
    assert!(!r.open_report().unclean_shutdown, "stop closed the store");
}

#[test]
fn serve1_a_ctl_session_must_open_with_the_session_op() {
    let h = Hosted::new();
    let rt = Hosted::rt();
    rt.block_on(async {
        let mut c = h.connect(Proto::Ctl).await;
        let e = c.ctl("status", json!({})).await.unwrap_err();
        assert_eq!(e.code, E_NO_SESSION);
        let v = c
            .ctl("session", json!({"actor": null, "run": null}))
            .await
            .unwrap();
        assert_eq!(v["kind"], "human");
        assert_eq!(v["run"], "cli");
        assert_eq!(v["default_view"], "work");
        let v = c.ctl("status", json!({})).await.unwrap();
        assert_eq!(v["landings"], 0);
    });
}

#[test]
fn serve1_the_daemon_classifies_the_human_against_the_configured_actor() {
    let h = Hosted::new_with_human("human:qa");
    let rt = Hosted::rt();
    rt.block_on(async {
        let mut c = h.client(Some("human:qa"), Some("whatever")).await;
        let v = c
            .ctl("session", json!({"actor": "human:qa", "run": "whatever"}))
            .await
            .unwrap();
        assert_eq!(
            (v["kind"].as_str(), v["run"].as_str()),
            (Some("human"), Some("cli"))
        );
        let mut a = h.client(Some("agent:w"), Some("r1")).await;
        let v = a
            .ctl("session", json!({"actor": "agent:w", "run": "r1"}))
            .await
            .unwrap();
        assert_eq!(
            (
                v["kind"].as_str(),
                v["run"].as_str(),
                v["default_view"].as_str()
            ),
            (Some("agent"), Some("r1"), Some("r1"))
        );
        // id3 (D-ME2C0): a human actor that is not the configured one is an agent claiming a
        // human, and refused
        let mut o = h.connect(Proto::Ctl).await;
        let e = o
            .ctl("session", json!({"actor": "human:other", "run": null}))
            .await
            .unwrap_err();
        assert_eq!(e.code, E_PERMISSION, "{e}");
        assert_eq!(e.message, "an agent session cannot claim a human actor");
        assert_eq!(
            e.fix,
            "unset RDY_ACTOR to act as the person, or relay their words"
        );
    });
}

#[test]
fn serve1_an_agent_session_cannot_approve_and_cannot_take_the_humans_view() {
    let h = Hosted::new();
    let rt = Hosted::rt();
    rt.block_on(async {
        let mut hu = h.client(None, None).await;
        hu.ctl("new_doc", json!({"path": "a", "view": null}))
            .await
            .unwrap();
        let mut a = h.client(Some("agent:w"), None).await;
        let e = a
            .ctl("new_doc", json!({"path": "b", "view": "work"}))
            .await
            .unwrap_err();
        assert_eq!(e.code, E_VIEW_OWNER);
        let v = a
            .ctl("new_doc", json!({"path": "b", "view": null}))
            .await
            .unwrap();
        assert!(v["change"].is_string(), "{v}");
        let st = hu.ctl("status", json!({})).await.unwrap();
        let names: Vec<&str> = st["views"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].as_str().unwrap())
            .collect();
        assert!(
            names.contains(&"work") && names.contains(&"work-agent-w"),
            "{st}"
        );
        a.ctl("propose", json!({"view": null, "title": "t"}))
            .await
            .unwrap();
        let e = a
            .ctl("approve", json!({"proposal": "whatever"}))
            .await
            .unwrap_err();
        assert_eq!(e.code, E_PERMISSION);
        assert!(e.fix.contains("--relayed-by <your actor>"), "{e}");
    });
}

#[test]
fn serve1_read_your_writes_across_two_connections() {
    let h = Hosted::new();
    let rt = Hosted::rt();
    rt.block_on(async {
        let mut a = h.client(None, None).await;
        let mut b = h.client(None, None).await;
        for i in 0..20 {
            let v = a
                .ctl("new_doc", json!({"path": format!("d{i}"), "view": null}))
                .await
                .unwrap();
            assert!(v["change"].is_string());
            let st = b.ctl("status", json!({})).await.unwrap();
            let work = st["views"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["name"] == "work")
                .unwrap();
            assert_eq!(
                work["changes"],
                i + 1,
                "b did not see a's acknowledged write {i}"
            );
        }
    });
}

#[test]
fn serve1_mcp_runs_over_the_socket_and_a_session_end_frees_its_slot() {
    let h = Hosted::new();
    let rt = Hosted::rt();
    rt.block_on(async {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let stream = h.mcp_stream().await;
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);
        let mut line = String::new();
        let line_of = |v: serde_json::Value| format!("{v}\n");
        // the same initialize line rdy_qa::mcp::McpClient sends (its protocolVersion constant)
        w.write_all(line_of(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": rdy_qa::mcp::PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}})).as_bytes()).await.unwrap();
        r.read_line(&mut line).await.unwrap();
        assert!(line.contains("\"serverInfo\""), "{line}");
        w.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n").await.unwrap();
        w.write_all(line_of(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "rdy_session", "arguments": {"actor": "agent:m", "role": "writer"}}})).as_bytes()).await.unwrap();
        line.clear();
        r.read_line(&mut line).await.unwrap();
        assert!(line.contains("work-agent-m"), "{line}");
        let sessions_before = h.run.as_ref().unwrap().handle().sessions().len();
        assert_eq!(sessions_before, 1);
        drop(w);
        drop(r);
        let t = std::time::Instant::now();
        while !h.run.as_ref().unwrap().handle().sessions().is_empty() {
            assert!(t.elapsed() < Duration::from_secs(30), "the mcp session was not removed");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
}

#[test]
fn serve1_hellos_during_warm_up_see_starting_then_ready() {
    // a project big enough that canon's warm-up is observable is not needed: `starting` is
    // answered until readiness, so a hello that arrives between bind and ready sees at least one
    // `starting`, which this test forces by connecting before the serving thread reaches ready.
    let dir = short_tempdir();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let run = start(ServeOptions {
        project: dir.path().into(),
        version: "t".into(),
        detach: false,
        idle_timeout: Some(Duration::from_secs(60)),
        human_actor: None,
        site_lan_override: None,
    })
    .unwrap();
    let rt = Hosted::rt();
    let saw_ready = rt.block_on(async {
        let hello = Hello {
            rdy: PROTO_VERSION,
            version: "t".into(),
            proto: Proto::Ctl,
            cwd: dir.path().into(),
            project: rdy_daemon::RunDir::of_project(std::path::Path::new(&dir.path()))
                .project_id()
                .expect("the project's .rdy"),
        };
        match connect(&RunDir::of_project(dir.path()), &hello).await {
            rdy_daemon::Attempt::Ready(_) => true,
            rdy_daemon::Attempt::Starting(p) => {
                let c = p
                    .wait(
                        std::time::Instant::now() + Duration::from_secs(30),
                        &mut |_| {},
                    )
                    .await
                    .unwrap();
                c.version() == "t"
            }
            other => panic!("{other:?}"),
        }
    });
    assert!(saw_ready);
    run.handle().stop();
    assert_eq!(run.wait().unwrap(), ExitReason::Stopped);
}

#[test]
fn serve1_idle_exit_closes_the_store_cleanly() {
    let dir = short_tempdir();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let run = start(ServeOptions {
        project: dir.path().into(),
        version: "t".into(),
        detach: false,
        idle_timeout: Some(Duration::from_millis(300)),
        human_actor: None,
        site_lan_override: None,
    })
    .unwrap();
    assert_eq!(run.wait().unwrap(), ExitReason::Idle);
    let r = Rdy::open(dir.path()).unwrap();
    assert!(!r.open_report().unclean_shutdown);
}

/// A successor host of `version` on `project`, as a freshly spawned daemon would start it.
fn start_successor(project: &std::path::Path, version: &str) -> rdy_daemon::ServeRun {
    start(ServeOptions {
        project: project.into(),
        version: version.into(),
        detach: false,
        idle_timeout: Some(Duration::from_secs(60)),
        human_actor: None,
        site_lan_override: None,
    })
    .unwrap_or_else(|e| panic!("the successor did not start: {e}"))
}

/// Asserts a `version` hello on `project` reaches a ready daemon.
fn assert_ready(rt: &tokio::runtime::Runtime, project: &std::path::Path, version: &str) {
    rt.block_on(async {
        let hello = Hello {
            rdy: PROTO_VERSION,
            version: version.into(),
            proto: Proto::Ctl,
            cwd: project.into(),
            project: rdy_daemon::RunDir::of_project(std::path::Path::new(&project))
                .project_id()
                .expect("the project's .rdy"),
        };
        match connect(&RunDir::of_project(project), &hello).await {
            rdy_daemon::Attempt::Ready(c) => assert_eq!(c.version(), version),
            rdy_daemon::Attempt::Starting(p) => {
                let c = p
                    .wait(
                        std::time::Instant::now() + Duration::from_secs(30),
                        &mut |_| {},
                    )
                    .await
                    .unwrap_or_else(|e| panic!("the successor did not become ready: {e}"));
                assert_eq!(c.version(), version);
            }
            other => panic!("the successor did not answer the hello: {other:?}"),
        }
    });
}

#[test]
fn serve1_a_successor_started_as_a_version_replaced_daemon_exits_comes_up_ready() {
    // the version-replace flow: the client spawns the successor as soon as it reads `exiting`,
    // while the predecessor is still draining and closing its store
    let mut h = Hosted::new();
    let old = h.run.take().unwrap();
    // the predecessor's process: it ends when its host returns
    let old = std::thread::spawn(move || old.wait());
    let rt = Hosted::rt();
    rt.block_on(async {
        let hello = Hello {
            rdy: PROTO_VERSION,
            version: "u".into(),
            proto: Proto::Ctl,
            cwd: h.dir.path().into(),
            project: rdy_daemon::RunDir::of_project(std::path::Path::new(&h.dir.path()))
                .project_id()
                .expect("the project's .rdy"),
        };
        let a = connect(&RunDir::of_project(h.dir.path()), &hello).await;
        assert!(matches!(a, rdy_daemon::Attempt::Exiting), "{a:?}");
    });
    let new = start_successor(h.dir.path(), "u");
    assert_ready(&rt, h.dir.path(), "u");
    assert_eq!(old.join().unwrap().unwrap(), ExitReason::VersionReplaced);
    new.handle().stop();
    assert_eq!(new.wait().unwrap(), ExitReason::Stopped);
}

#[test]
fn serve1_a_successor_started_as_soon_as_a_stopped_daemons_socket_is_gone_comes_up_ready() {
    // `rdy serve --stop && rdy status`: the next command starts a daemon as soon as the socket
    // is gone, while the predecessor is still draining and closing its store
    let mut h = Hosted::new();
    let old = h.run.take().unwrap();
    let old = std::thread::spawn(move || old.wait());
    let rt = Hosted::rt();
    rt.block_on(async {
        let mut c = h.client(None, None).await;
        c.ctl("stop", json!({})).await.unwrap();
    });
    let socket = RunDir::of_project(h.dir.path()).socket();
    let t = std::time::Instant::now();
    while socket.exists() {
        assert!(
            t.elapsed() < Duration::from_secs(30),
            "the socket was not removed"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let new = start_successor(h.dir.path(), "t");
    assert_ready(&rt, h.dir.path(), "t");
    assert_eq!(old.join().unwrap().unwrap(), ExitReason::Stopped);
    new.handle().stop();
    assert_eq!(new.wait().unwrap(), ExitReason::Stopped);
}

#[test]
fn msw13_the_session_op_refuses_an_empty_actor_or_run() {
    let h = Hosted::new();
    let rt = Hosted::rt();
    rt.block_on(async {
        let mut c = h.client(None, None).await;
        // id3 (D-ME2C0): the actor is checked first, then the run, each with the shared text
        let a300 = "a".repeat(300);
        let actor = |shown: &str| {
            (
                format!("the actor {shown:?} is not in rdy's actor form"),
                ACTOR_FIX.to_string(),
            )
        };
        let run = |shown: &str| {
            (
                format!("the run {shown:?} is not in rdy's run form"),
                RUN_FIX.to_string(),
            )
        };
        for (args, want) in [
            (json!({"actor": "", "run": ""}), actor("")),
            (json!({"actor": "", "run": null}), actor("")),
            (json!({"actor": "  ", "run": null}), actor("  ")),
            (json!({"actor": "agent:w", "run": ""}), run("")),
            (json!({"actor": "agent:w", "run": "../x"}), run("../x")),
            (json!({"actor": "agent:w", "run": ".x"}), run(".x")),
            (
                json!({"actor": "agent:w", "run": "r".repeat(257)}),
                run(&format!("{}\u{2026}", "r".repeat(80))),
            ),
            (
                json!({"actor": a300, "run": null}),
                actor(&format!("{}\u{2026}", "a".repeat(80))),
            ),
        ] {
            let e = c.ctl("session", args.clone()).await.unwrap_err();
            assert_eq!(e.code, E_BAD_INPUT, "{args}: {}", e.message);
            assert_eq!((e.message, e.fix), want, "{args}");
        }
    });
}

#[test]
fn hd3_the_session_op_gets_a_did_you_mean_for_a_misspelt_key() {
    let h = Hosted::new();
    let rt = Hosted::rt();
    rt.block_on(async {
        let mut c = h.connect(Proto::Ctl).await;
        // the first request after the hello: {"id":1,"op":"session","args":{"actr":"agent:x"}}
        let e = c
            .ctl("session", json!({"actr": "agent:x"}))
            .await
            .unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{e}");
        assert_eq!(e.message, "session: unknown field \"actr\"");
        assert!(e.fix.starts_with("did you mean \"actor\"?"), "{e}");
        let v = c
            .ctl("session", json!({"actor": "agent:x", "run": null}))
            .await
            .unwrap();
        assert_eq!(v["kind"], "agent", "the connection lives");
    });
}

#[test]
fn hd4_an_oversized_line_on_the_mcp_socket_is_refused_and_the_session_lives() {
    let h = Hosted::new();
    let rt = Hosted::rt();
    rt.block_on(async {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (r, mut w) = h.mcp_stream().await.into_split();
        let mut r = BufReader::new(r);
        let mut line = String::new();
        let send = |v: serde_json::Value| format!("{v}\n");
        w.write_all(send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion": rdy_qa::mcp::PROTOCOL_VERSION,"capabilities":{},
            "clientInfo":{"name":"t","version":"0"}}})).as_bytes()).await.unwrap();
        r.read_line(&mut line).await.unwrap();
        w.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n").await.unwrap();
        w.write_all(send(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"rdy_session",
            "arguments":{"actor":"agent:hd4","run":"hd4","role":"writer"}}})).as_bytes()).await.unwrap();
        line.clear();
        r.read_line(&mut line).await.unwrap();
        let big = "x".repeat(17 * 1024 * 1024);
        let req = format!(r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"rdy_write","arguments":{{"message":"{big}","ops":[]}}}}}}"#);
        tokio::time::timeout(Duration::from_secs(30), w.write_all(format!("{req}\n").as_bytes()))
            .await.expect("written").unwrap();
        line.clear();
        tokio::time::timeout(Duration::from_secs(30), r.read_line(&mut line)).await.expect("a reply").unwrap();
        assert!(line.contains("rdy_request_too_large") && line.contains("E_LIMIT"), "{}", &line[..line.len().min(400)]);
        w.write_all(send(json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"rdy_overview","arguments":{}}})).as_bytes()).await.unwrap();
        line.clear();
        r.read_line(&mut line).await.unwrap();
        assert!(line.contains("\"id\":4"), "{line}");
    });
}

/// Every file under the project's store and its length: what a refused session must not change.
fn store_bytes(project: &std::path::Path) -> Vec<(std::path::PathBuf, u64)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(std::path::PathBuf, u64)>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let m = e.metadata().unwrap();
            if m.is_dir() {
                walk(&e.path(), out);
            } else {
                out.push((e.path(), m.len()));
            }
        }
    }
    let mut out = Vec::new();
    walk(&project.join(".rdy/store"), &mut out);
    out.sort();
    out
}

/// id3 (D-ME2C0): the session op checks the actor and run before it records a session: a
/// refused handshake adds no session (a fresh connection still has none, an open one keeps its
/// own) and writes nothing to the log.
#[test]
fn id3_new_sessions_validate_before_state_changes() {
    let h = Hosted::new_with_human("human:qa");
    let rt = Hosted::rt();
    rt.block_on(async {
        // an open session that a later refusal on its connection must keep
        let mut kept = h.connect(Proto::Ctl).await;
        let v = kept
            .ctl("session", json!({"actor": "agent:keep", "run": "k1"}))
            .await
            .unwrap();
        assert_eq!(v["default_view"], "k1");
        let before = store_bytes(h.dir.path());
        let long_name = "n".repeat(65);
        let refused = [
            (json!({"actor": "Agent:W", "run": null}), E_BAD_INPUT),
            (json!({"actor": "agent:w", "run": "run:7"}), E_BAD_INPUT),
            (json!({"actor": null, "run": "work"}), E_BAD_INPUT),
            (json!({"actor": "human:qa", "run": "bad run"}), E_BAD_INPUT),
            (
                json!({"actor": format!("agent:{long_name}"), "run": null}),
                E_BAD_INPUT,
            ),
            (
                json!({"actor": "agent:w", "run": "r".repeat(65)}),
                E_BAD_INPUT,
            ),
            (json!({"actor": "human:other", "run": null}), E_PERMISSION),
            (json!({"actor": "human:other", "run": "r1"}), E_PERMISSION),
        ];
        for (args, code) in &refused {
            let e = kept.ctl("session", args.clone()).await.unwrap_err();
            assert_eq!(e.code, *code, "{args}: {e}");
            let mut fresh = h.connect(Proto::Ctl).await;
            let e = fresh.ctl("session", args.clone()).await.unwrap_err();
            assert_eq!(e.code, *code, "{args}: {e}");
            let e = fresh.ctl("status", json!({})).await.unwrap_err();
            assert_eq!(e.code, E_NO_SESSION, "{args}: no session was recorded");
        }
        // the open session is still the one it opened: its default view takes the write
        let w = kept
            .ctl("new_doc", json!({"path": "kept", "view": null}))
            .await
            .unwrap();
        assert!(w["change"].is_string(), "{w}");
        let st = kept.ctl("status", json!({})).await.unwrap();
        assert!(
            st["views"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["name"] == "k1"),
            "{st}"
        );
        assert_ne!(
            store_bytes(h.dir.path()),
            before,
            "the write after them did"
        );
        let after_write = store_bytes(h.dir.path());
        for (args, _) in &refused {
            let mut fresh = h.connect(Proto::Ctl).await;
            assert!(fresh.ctl("session", args.clone()).await.is_err());
        }
        assert_eq!(
            store_bytes(h.dir.path()),
            after_write,
            "refused sessions write nothing"
        );
        // the longest names are accepted; a two-name agent with no run keeps its slash
        let max_actor = format!("agent:{}/{}", "a".repeat(64), "b".repeat(64));
        assert_eq!(max_actor.len(), 135);
        let max_run = "r".repeat(64);
        let mut c = h.connect(Proto::Ctl).await;
        let v = c
            .ctl("session", json!({"actor": max_actor, "run": max_run}))
            .await
            .unwrap();
        assert_eq!(
            (v["kind"].as_str(), v["actor"].as_str(), v["run"].as_str()),
            (
                Some("agent"),
                Some(max_actor.as_str()),
                Some(max_run.as_str())
            )
        );
        assert_eq!(v["default_view"], max_run.as_str());
        let mut c = h.connect(Proto::Ctl).await;
        let v = c
            .ctl("session", json!({"actor": "agent:a/b", "run": null}))
            .await
            .unwrap();
        assert_eq!(
            (v["run"].is_null(), v["default_view"].as_str()),
            (true, Some("work-agent-a/b"))
        );
        let mut c = h.connect(Proto::Ctl).await;
        let v = c
            .ctl("session", json!({"actor": "agent:a-b", "run": null}))
            .await
            .unwrap();
        assert_eq!(v["default_view"], "work-agent-a-b", "a distinct default");
        // the configured human, with a valid run it ignores
        let mut c = h.connect(Proto::Ctl).await;
        let v = c
            .ctl("session", json!({"actor": "human:qa", "run": "r9"}))
            .await
            .unwrap();
        assert_eq!(
            (v["kind"].as_str(), v["run"].as_str()),
            (Some("human"), Some("cli"))
        );
    });
}

/// id3: a host whose configured (test-hook) human is outside the form opens no session.
#[test]
fn id3_a_configured_human_outside_the_form_opens_no_session() {
    let h = Hosted::new_with_human("Nick");
    let rt = Hosted::rt();
    rt.block_on(async {
        for args in [
            json!({"actor": null, "run": null}),
            json!({"actor": "agent:w", "run": "r1"}),
        ] {
            let mut c = h.connect(Proto::Ctl).await;
            let e = c.ctl("session", args.clone()).await.unwrap_err();
            assert_eq!(
                (e.code.as_str(), e.message.as_str(), e.fix.as_str()),
                (
                    E_BAD_INPUT,
                    "the actor \"Nick\" is not in rdy's actor form",
                    ACTOR_FIX
                ),
                "{args}"
            );
            assert_eq!(
                c.ctl("status", json!({})).await.unwrap_err().code,
                E_NO_SESSION
            );
        }
    });
}
