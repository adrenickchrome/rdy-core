//! mh5 (D-CCC6E, D-4PHYQ): W_DAEMON_BUILD over MCP: once per actor and run, on the first tool
//! result after a valid claim, through M3g's claims registry.
use rdy_daemon::{
    Attempt, Client, Daemon, DaemonConfig, Hello, PROTO_VERSION, Proto, RunDir, ServeHandler,
    Shared, connect,
};
use rdy_engine::Rdy;
use rdy_engine::engine::{Engine, Local};
use rdy_engine::signing_crypto::UnavailableAgent;
use rdy_mcp::claims::{MAX_SETTINGS, Registry, SETTINGS_IDLE};
use rdy_mcp::notify::NotifyHub;
use rdy_mcp::server::{DaemonBuildWarning, serve_on};
use rdy_model::Role;
use rdy_qa::daemon::{Served, short_tempdir};
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

const OTHER: &str = "0123456789abcdef";
const THIRD: &str = "fedcba9876543210";
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn warning(client: &str) -> DaemonBuildWarning {
    DaemonBuildWarning {
        version: "1.2.3".into(),
        daemon_build: OTHER.into(),
        client_build: client.into(),
    }
}

fn expected(client: &str) -> Value {
    json!({
        "code": "W_DAEMON_BUILD",
        "message": format!("the running daemon is another build of rdy 1.2.3 ({OTHER}); this client is build {client}"),
        "fix": "close the other clients; the daemon then restarts on this build. Or run rdy serve --stop",
    })
}

struct Conn {
    r: BufReader<ReadHalf<DuplexStream>>,
    w: WriteHalf<DuplexStream>,
    next: u64,
}

impl Conn {
    async fn start(
        engine: &Arc<dyn Engine>,
        sid: u64,
        hub: &Arc<NotifyHub>,
        warn: Option<DaemonBuildWarning>,
    ) -> Conn {
        let (client, server) = tokio::io::duplex(1 << 20);
        let (sr, sw) = tokio::io::split(server);
        tokio::spawn(serve_on(
            Arc::clone(engine),
            sid,
            Arc::clone(hub),
            Arc::new(UnavailableAgent),
            warn,
            sr,
            sw,
        ));
        let (cr, cw) = tokio::io::split(client);
        let mut c = Conn {
            r: BufReader::new(cr),
            w: cw,
            next: 0,
        };
        c.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "mh5", "version": "0"}}),
        )
        .await;
        c.w.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .unwrap();
        c
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let line = json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params});
        self.w
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
        loop {
            let mut got = String::new();
            assert!(self.r.read_line(&mut got).await.unwrap() > 0, "closed");
            let v: Value = serde_json::from_str(&got).unwrap();
            if v["id"] == json!(self.next) {
                return v;
            }
        }
    }

    /// `(isError, the reply's body)` of a tool call.
    async fn call(&mut self, tool: &str, args: Value, meta: Value) -> (bool, Value) {
        let v = self
            .request(
                "tools/call",
                json!({"name": tool, "arguments": args, "_meta": meta}),
            )
            .await;
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        (
            v["result"]["isError"] == json!(true),
            serde_json::from_str(text).unwrap(),
        )
    }
}

fn claim(actor: &str, run: &str) -> Value {
    json!({"rdy/actor": actor, "rdy/run": run})
}

fn folder(name: &str) -> Value {
    json!({"ops": [{"op": "create", "type": "folder", "id": "$f", "fields": {"name": name, "title": name}}]})
}

fn warned(v: &Value) -> usize {
    v["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["code"] == "W_DAEMON_BUILD")
        .count()
}

fn key(actor: &str, run: &str) -> (String, Option<String>) {
    (actor.to_string(), Some(run.to_string()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mh5_mcp_warns_once_for_the_bound_actor_and_run() {
    let dir = short_tempdir();
    let engine: Arc<dyn Engine> =
        Arc::new(Local::new(Rdy::init(dir.path(), "rdy-standard").unwrap()));
    let hub = NotifyHub::new();
    let w = || Some(warning(THIRD));

    // initialize, tools/list, an unclaimed call and a resource read consume nothing
    let mut a = Conn::start(&engine, 1, &hub, w()).await;
    a.request("tools/list", json!({})).await;
    let (err, v) = a.call("rdy_tags", json!({}), json!({})).await;
    assert!(err);
    assert_eq!(v["code"], "E_NO_SESSION");
    assert_eq!(warned(&v), 0, "{v}");
    a.request(
        "resources/read",
        json!({"uri": "rdy://project", "_meta": claim("agent:a", "r1")}),
    )
    .await;
    assert!(
        hub.claims()
            .get(&key("agent:a", "r1"))
            .is_none_or(|s| !s.daemon_build_warned),
        "a resource read does not consume the bit"
    );

    // the first tool result after a `_meta` claim carries it, a refused result too; its keys stay
    let (err, v) = a
        .call("rdy_tags", json!({"nope": 1}), claim("agent:a", "r1"))
        .await;
    assert!(err, "{v}");
    assert_eq!(v["code"], "E_BAD_INPUT");
    assert!(v["fix"].is_string());
    assert_eq!(v["warnings"], json!([expected(THIRD)]), "{v}");
    // a warning-only entry: no role, no view, no dedupe
    let saved = hub.claims().get(&key("agent:a", "r1")).unwrap();
    assert_eq!(
        (
            saved.view.clone(),
            saved.role,
            saved.dedupe,
            saved.daemon_build_warned
        ),
        (None, None, false, true)
    );
    // once: and the role and view resolve as before, with a call-local reader override
    let (err, v) = a
        .call(
            "rdy_write",
            folder("ro"),
            json!({"rdy/actor": "agent:a", "rdy/run": "r1", "rdy/role": "reader"}),
        )
        .await;
    assert!(err, "a reader cannot write: {v}");
    assert_eq!(warned(&v), 0, "{v}");
    let (err, v) = a.call("rdy_write", folder("rw"), json!({})).await;
    assert!(!err, "the call's reader did not stay: {v}");
    assert_eq!(warned(&v), 0, "{v}");
    let (_, v) = a.call("rdy_overview", json!({}), json!({})).await;
    assert_eq!(v["session"], json!({"actor": "agent:a", "view": "r1"}));
    assert_eq!(v["daemon"], Value::Null, "a Local engine has no daemon");

    // another connection of the same actor and run, another client build, explicit sessions
    let mut b = Conn::start(&engine, 2, &hub, Some(warning("aaaaaaaaaaaaaaaa"))).await;
    let session = json!({"actor": "agent:a", "run": "r1", "role": "reader"});
    let (err, v) = b.call("rdy_session", session.clone(), json!({})).await;
    assert!(!err, "{v}");
    assert_eq!(warned(&v), 0, "{v}");
    let saved = hub.claims().get(&key("agent:a", "r1")).unwrap();
    assert_eq!(saved.role, Some(Role::Reader));
    assert_eq!(saved.view.as_deref(), Some("r1"));
    assert!(saved.daemon_build_warned, "rdy_session keeps the bit");
    let (_, v) = b.call("rdy_session", session, json!({})).await;
    assert_eq!(warned(&v), 0, "{v}");

    // an explicit rdy_session is a valid claim: its own result carries it
    let mut c = Conn::start(&engine, 3, &hub, w()).await;
    let (err, v) = c
        .call(
            "rdy_session",
            json!({"actor": "agent:s", "run": "r9"}),
            json!({}),
        )
        .await;
    assert!(!err, "{v}");
    assert!(v["view"].is_string(), "the old keys stay: {v}");
    assert_eq!(warned(&v), 1, "{v}");

    // a distinct run and a distinct actor each hear it once
    for (i, (actor, run)) in [("agent:a", "r2"), ("agent:b", "r1")]
        .into_iter()
        .enumerate()
    {
        let mut d = Conn::start(&engine, 10 + i as u64, &hub, w()).await;
        let (_, v) = d.call("rdy_tags", json!({}), claim(actor, run)).await;
        assert_eq!(warned(&v), 1, "{actor} {run}: {v}");
        let (_, v) = d.call("rdy_tags", json!({}), json!({})).await;
        assert_eq!(warned(&v), 0, "{actor} {run}: {v}");
    }

    // concurrent connections of one actor and run: exactly one reply carries it
    let mut joins = vec![];
    for i in 0..6u64 {
        let (engine, hub) = (Arc::clone(&engine), Arc::clone(&hub));
        joins.push(tokio::spawn(async move {
            let mut e = Conn::start(&engine, 20 + i, &hub, Some(warning(THIRD))).await;
            let (_, v) = e
                .call("rdy_tags", json!({}), claim("agent:c", "same"))
                .await;
            warned(&v)
        }));
    }
    let mut total = 0;
    for j in joins {
        total += j.await.unwrap();
    }
    assert_eq!(total, 1);

    // an elicited answer that is refused is a tool result of its own path
    let mut f = Conn::start(&engine, 30, &hub, w()).await;
    let (err, v) = f
        .call(
            "rdy_answer",
            json!({"elicit": true, "ref": "P-x@1", "card_hash": "h"}),
            claim("agent:e", "r1"),
        )
        .await;
    assert!(err, "{v}");
    assert_eq!(warned(&v), 1, "{v}");

    // a stated max_tokens holds with the warning inside it
    let mut g = Conn::start(&engine, 31, &hub, w()).await;
    let (err, v) = g
        .call(
            "rdy_overview",
            json!({"max_tokens": 600}),
            claim("agent:g", "r1"),
        )
        .await;
    assert!(!err, "{v}");
    assert_eq!(warned(&v), 1);
    assert!(v.to_string().len() / 4 <= 600, "{}", v.to_string().len());

    // standalone: no warning value, no record
    let mut s = Conn::start(&engine, 32, &hub, None).await;
    let (_, v) = s.call("rdy_tags", json!({}), claim("agent:n", "r1")).await;
    assert_eq!(warned(&v), 0, "{v}");
    assert!(hub.claims().get(&key("agent:n", "r1")).is_none());

    // a daemon restart: a new registry warns again
    let restarted = NotifyHub::new();
    let mut r = Conn::start(&engine, 1, &restarted, w()).await;
    let (_, v) = r.call("rdy_tags", json!({}), claim("agent:a", "r1")).await;
    assert_eq!(warned(&v), 1, "{v}");

    // a full registry: it warns on each call, records nothing, and never refuses for it
    let full = NotifyHub::new();
    for n in 0..MAX_SETTINGS {
        full.claims()
            .save(
                &(format!("agent:f{n}"), None),
                "v".into(),
                Role::Writer,
                None,
            )
            .unwrap();
    }
    let mut h = Conn::start(&engine, 40, &full, w()).await;
    for meta in [claim("agent:new", "r1"), json!({})] {
        let (err, v) = h.call("rdy_tags", json!({}), meta).await;
        assert!(!err, "{v}");
        assert_eq!(warned(&v), 1, "{v}");
    }
    assert!(full.claims().get(&key("agent:new", "r1")).is_none());
    assert_eq!(full.claims().len(), MAX_SETTINGS);

    // the entry expires under the injected clock, one hour after its last call
    let base = Instant::now();
    let at = Arc::new(Mutex::new(base));
    let clock = Arc::clone(&at);
    let reg = Registry::with_clock(Arc::new(move || *clock.lock().unwrap()));
    let k = key("agent:t", "r1");
    assert!(reg.take_build_warning(&k));
    assert!(!reg.take_build_warning(&k));
    *at.lock().unwrap() = base + SETTINGS_IDLE / 2;
    reg.touch(&k);
    *at.lock().unwrap() = base + SETTINGS_IDLE;
    assert!(!reg.take_build_warning(&k), "the touch kept it");
    *at.lock().unwrap() = base + SETTINGS_IDLE + SETTINGS_IDLE;
    assert!(
        reg.take_build_warning(&k),
        "gone an hour after its last call"
    );

    // through a real daemon: ServeHandler passes the warning to the session
    std::thread::spawn(daemon_path).join().unwrap();
}

/// A daemon of another build in this process, one held connection, and `rdy mcp` as a client.
fn daemon_path() {
    let s = Served::new();
    let dir = s.dir.path().to_path_buf();
    let run = RunDir::of_project(&dir);
    let shared = Shared::new(Rdy::open(&dir).unwrap());
    shared.set_daemon_identity(VERSION, OTHER);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let handler = ServeHandler::new(Arc::clone(&shared), Some("human:qa".into()));
    let mut config = DaemonConfig::new(run.clone(), VERSION.into());
    config.build = OTHER.into();
    let mut d = Daemon::new(config, Arc::new(handler));
    rt.block_on(d.bind()).unwrap();
    d.readiness().ready();
    let task = rt.spawn(d.run());
    let holder = rt.block_on(hold(&run, &dir));

    let mut m = McpClient::start_proxy(&s);
    let v = m
        .call("rdy_session", json!({"actor": "agent:p", "run": "p1"}))
        .ok();
    let got = v["warnings"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(got["code"], "W_DAEMON_BUILD");
    assert_eq!(
        got["message"],
        format!(
            "the running daemon is another build of rdy {VERSION} ({OTHER}); this client is build {}",
            rdy_daemon::BUILD
        )
    );
    let v = m.call("rdy_overview", json!({})).ok();
    assert_eq!(warned(&v), 0, "{v}");
    assert_eq!(v["daemon"], format!("rdy {VERSION} build {OTHER}"));
    m.close_stdin();
    let _ = m.wait_exit(rdy_qa::mcp::DEADLINE);
    holder.close();
    rt.block_on(task).unwrap().unwrap();
    drop(rt);
    shared.shutdown().unwrap();
    drop(s);
}

async fn hold(run: &RunDir, dir: &Path) -> Client {
    let hello = Hello {
        rdy: PROTO_VERSION,
        version: VERSION.into(),
        build: OTHER.into(),
        proto: Proto::Ctl,
        cwd: dir.to_path_buf(),
        project: run.project_id().unwrap(),
    };
    match connect(run, &hello).await {
        Attempt::Ready(c) => c,
        other => panic!("{other:?}"),
    }
}
