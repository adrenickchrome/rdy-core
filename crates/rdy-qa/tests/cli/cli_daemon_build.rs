//! mh5 (D-CCC6E): the CLI's W_DAEMON_BUILD and the daemon line of its status. The daemon of
//! another build runs in this process; one held connection keeps it busy.
use rdy_daemon::ctl::CtlRequest;
use rdy_daemon::{
    Attempt, Client, Daemon, DaemonConfig, ErrorReply, ExitReason, Handler, Hello, McpStream,
    PROTO_VERSION, Proto, RunDir, ServeHandler, SessionInfo, Shared, connect,
};
use rdy_engine::Rdy;
use rdy_engine::signing_crypto::SigningAgent;
use rdy_qa::daemon::{Hosted, Served};
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use tokio::task::JoinHandle;

const OTHER: &str = "0123456789abcdef";
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// ServeHandler, with one op's name replaced, so a request reaches the real unknown-op path.
struct Renaming {
    inner: ServeHandler,
    rename: Option<(&'static str, &'static str)>,
}

impl Handler for Renaming {
    fn ctl(
        &self,
        session: &SessionInfo,
        mut request: CtlRequest,
        agent: Arc<dyn SigningAgent>,
    ) -> Result<Value, ErrorReply> {
        if let Some((from, to)) = self.rename
            && request.op == from
        {
            request.op = to.to_string();
        }
        self.inner.ctl(session, request, agent)
    }
    fn mcp(
        &self,
        session: SessionInfo,
        stream: McpStream,
        agent: Arc<dyn SigningAgent>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        self.inner.mcp(session, stream, agent)
    }
    fn session_ended(&self, session: &SessionInfo) {
        self.inner.session_ended(session);
    }
}

struct Busy {
    shared: Arc<Shared>,
    rt: tokio::runtime::Runtime,
    task: JoinHandle<std::io::Result<ExitReason>>,
    holder: Client,
}

fn busy(dir: &Path, rename: Option<(&'static str, &'static str)>) -> Busy {
    let run = RunDir::of_project(dir);
    let shared = Shared::new(Rdy::open(dir).unwrap());
    shared.set_daemon_identity(VERSION, OTHER);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let inner = ServeHandler::new(Arc::clone(&shared), Some("human:qa".into()));
    let mut config = DaemonConfig::new(run.clone(), VERSION.into());
    config.build = OTHER.into();
    let mut d = Daemon::new(config, Arc::new(Renaming { inner, rename }));
    rt.block_on(d.bind()).unwrap();
    d.readiness().ready();
    let task = rt.spawn(d.run());
    let hello = Hello {
        rdy: PROTO_VERSION,
        version: VERSION.into(),
        build: OTHER.into(),
        proto: Proto::Ctl,
        cwd: dir.to_path_buf(),
        project: run.project_id().unwrap(),
    };
    let holder = match rt.block_on(connect(&run, &hello)) {
        Attempt::Ready(c) => c,
        other => panic!("{other:?}"),
    };
    Busy {
        shared,
        rt,
        task,
        holder,
    }
}

impl Busy {
    /// The last close stops the daemon: the CLI was another build.
    fn end(self) {
        let Busy {
            shared,
            rt,
            task,
            holder,
        } = self;
        holder.close();
        assert_eq!(
            rt.block_on(task).unwrap().unwrap(),
            ExitReason::BuildReplaced
        );
        drop(rt);
        shared.shutdown().unwrap();
    }
}

fn warning_lines() -> String {
    format!(
        "W_DAEMON_BUILD: the running daemon is another build of rdy {VERSION} ({OTHER}); this client is build {}\nfix: close the other clients; the daemon then restarts on this build. Or run rdy serve --stop\n",
        rdy_daemon::BUILD
    )
}

#[test]
fn mh5_cli_warns_on_success_and_unknown_op() {
    // success: the warning on stderr, once; stdout keeps its object
    let s = Served::new();
    let b = busy(s.dir.path(), None);
    let o = s.rdy(&["status", "--json"]);
    assert!(o.status.success(), "{o:?}");
    assert_eq!(String::from_utf8_lossy(&o.stderr), warning_lines());
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(v["version_position"].is_object(), "{v}");
    assert_eq!(v["daemon"], format!("rdy {VERSION} build {OTHER}"));
    b.end();

    // an op this daemon does not know: E_BAD_INPUT, from the existing path, with the warning
    let b = busy(s.dir.path(), Some(("status", "no_such_op")));
    let o = s.rdy(&["status", "--json"]);
    assert!(!o.status.success(), "{o:?}");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.starts_with(&warning_lines()), "{err}");
    let out = String::from_utf8_lossy(&o.stdout);
    let shown = format!("{out}{err}");
    assert!(shown.contains("E_BAD_INPUT"), "{shown}");
    assert!(
        shown.contains("unknown op \\\"no_such_op\\\"")
            || shown.contains("unknown op \"no_such_op\""),
        "{shown}"
    );
    b.end();

    // the same build warns of nothing
    let o = s.rdy(&["status", "--json"]);
    assert!(o.status.success(), "{o:?}");
    assert!(
        !String::from_utf8_lossy(&o.stderr).contains("W_DAEMON_BUILD"),
        "{o:?}"
    );
    s.stop();
}

#[test]
fn mh5_status_and_overview_report_the_serving_build() {
    let s = Served::new();
    let id = format!("rdy {VERSION} build {}", rdy_daemon::BUILD);
    // the exact line follows the pack line
    let o = s.rdy(&["status"]);
    let text = String::from_utf8_lossy(&o.stdout);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "pack rdy-standard", "{text}");
    assert_eq!(lines[1], format!("daemon: {id}"), "{text}");
    assert!(lines[2].starts_with("repo "), "{text}");
    let o = s.rdy(&["status", "--json"]);
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["daemon"], id);

    // a bounded overview names it, inside its budget
    let mut m = McpClient::start_proxy(&s);
    m.call("rdy_session", json!({"actor": "agent:o", "run": "o1"}))
        .ok();
    let v = m.call("rdy_overview", json!({"max_tokens": 500})).ok();
    assert_eq!(v["daemon"], id);
    assert!(v.to_string().len() / 4 <= 500, "{v}");
    drop(m.finish(rdy_qa::mcp::DEADLINE));
    s.stop();

    // no daemon: none and null
    let o = s.rdy(&["--no-daemon", "status"]);
    let text = String::from_utf8_lossy(&o.stdout);
    assert_eq!(text.lines().nth(1), Some("daemon: none"), "{text}");
    let o = s.rdy(&["--no-daemon", "status", "--json"]);
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["daemon"], Value::Null, "{v}");
    let mut m = McpClient::start(s.dir.path());
    m.call("rdy_session", json!({"actor": "agent:o", "run": "o1"}))
        .ok();
    let v = m.call("rdy_overview", json!({})).ok();
    assert_eq!(v["daemon"], Value::Null, "{v}");
    drop(m.finish(rdy_qa::mcp::DEADLINE));

    // ping and sessions over the real host
    let host = Hosted::new();
    let rt = Hosted::rt();
    rt.block_on(async {
        let mut c = host.connect(Proto::Ctl).await;
        assert_eq!(c.build(), rdy_daemon::BUILD);
        let ping = c.ctl("ping", json!({})).await.unwrap();
        assert_eq!(ping["build"], rdy_daemon::BUILD);
        let sessions = c.ctl("sessions", json!({})).await.unwrap();
        assert_eq!(sessions[0]["build"], rdy_daemon::BUILD);
        c.close();
    });
}
