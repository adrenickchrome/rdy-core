//! ain4 (D-8X04F, D-YQPB6): the projectless MCP server, over an in-memory stream. Outside any
//! project, a connection can claim (by `rdy_session` or `_meta`) and read its instructions: the
//! person layer alone, or an empty reply with the creation hint. Every other tool is
//! `E_NO_PROJECT`. Nothing but protocol lines comes out, and no store is made.
use rdy_engine::instructions::{self, Caller, Contribution, Layer, LoadRequest, PersonHost};
use rdy_engine::instructions_config::{PathEnv, PersonSource, Platform, resolve_person};
use rdy_engine::{Rdy, RdyError};
use rdy_mcp::bootstrap::{ProjectlessServer, serve_projectless};
use rdy_mcp::notify::NotifyHub;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const DEADLINE: Duration = Duration::from_secs(30);

/// A person host over an isolated home: the meta project is opened in this process.
struct Home(PathBuf);

impl PersonHost for Home {
    fn locate(&self) -> Result<PersonSource, RdyError> {
        resolve_person(
            Platform::HOST,
            &PathEnv {
                home: Some(self.0.clone().into()),
                xdg_config_home: Some(self.0.join("cfg").into()),
                ..PathEnv::default()
            },
        )
    }

    fn fetch(
        &self,
        meta: &Path,
        load: &LoadRequest,
        _caller: &Caller,
    ) -> Result<Contribution, RdyError> {
        let r = Rdy::open(meta)?;
        let part = instructions::collect(&r.read(), Layer::Person, load);
        r.close()?;
        part
    }
}

/// A connection to a projectless server started in `dir`.
struct Conn {
    w: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    r: BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    next: u64,
}

impl Conn {
    async fn start(dir: &Path, home: &Path) -> Conn {
        let (client, end) = tokio::io::duplex(1 << 16);
        let (sr, sw) = tokio::io::split(end);
        let server = ProjectlessServer::new(dir, Arc::new(Home(home.into())), NotifyHub::new());
        tokio::spawn(serve_projectless(server, sr, sw));
        let (r, w) = tokio::io::split(client);
        let mut c = Conn {
            w,
            r: BufReader::new(r),
            next: 1,
        };
        let init = c
            .request(
                "initialize",
                json!({"protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": {"name": "ain4", "version": "0"}}),
            )
            .await;
        assert_eq!(init["result"]["serverInfo"]["name"], "rdy", "{init}");
        c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        c
    }

    async fn send(&mut self, v: Value) {
        self.w.write_all(format!("{v}\n").as_bytes()).await.unwrap();
    }

    /// The reply to one request; every line out is a JSON-RPC message.
    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        loop {
            let mut line = String::new();
            tokio::time::timeout(DEADLINE, self.r.read_line(&mut line))
                .await
                .expect("a reply before the deadline")
                .unwrap();
            let v: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("a non-protocol line {line:?}: {e}"));
            assert_eq!(v["jsonrpc"], "2.0", "{v}");
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    /// A tool call's body and whether it is an error.
    async fn call(&mut self, tool: &str, args: Value, meta: Option<Value>) -> (bool, Value) {
        let mut params = json!({"name": tool, "arguments": args});
        if let Some(m) = meta {
            params["_meta"] = m;
        }
        let v = self.request("tools/call", params).await;
        let text = v["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("a tool reply: {v}"));
        (
            v["result"]["isError"] == json!(true),
            serde_json::from_str(text).unwrap(),
        )
    }
}

fn claim(actor: &str, run: &str) -> Value {
    json!({"rdy/actor": actor, "rdy/run": run})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ain4_projectless_mcp_can_claim_and_read_empty_instructions() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut c = Conn::start(dir.path(), home.path()).await;
    // the two tools, with the project server's schemas
    let listed = c.request("tools/list", json!({})).await;
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["rdy_session", "rdy_instructions"]);
    let schema = &listed["result"]["tools"][1]["inputSchema"];
    assert_eq!(schema["additionalProperties"], false, "{schema}");
    // no claim yet: refused; a project tool: E_NO_PROJECT
    let (err, e) = c.call("rdy_instructions", json!({}), None).await;
    assert!(err);
    assert_eq!(e["code"], "E_NO_SESSION", "{e}");
    let (err, e) = c.call("rdy_overview", json!({}), None).await;
    assert!(err);
    assert_eq!(
        (e["code"].as_str(), e["message"].as_str(), e["fix"].as_str()),
        (
            Some("E_NO_PROJECT"),
            Some(format!("{} is not an rdy project", dir.path().display()).as_str()),
            Some("run rdy init in the project directory")
        )
    );
    // the first claim, by _meta, reads an empty reply and the hint
    let (err, v) = c
        .call("rdy_instructions", json!({}), Some(claim("agent:p", "p1")))
        .await;
    assert!(!err, "{v}");
    let meta = home.path().join(".rdy").join("meta");
    assert_eq!(
        v,
        json!({"instructions": [], "menu": [], "hint": instructions::create_hint(&meta)})
    );
    // the connection is bound: another actor is refused, the same one is accepted
    let (err, e) = c
        .call(
            "rdy_session",
            json!({"actor": "agent:q", "run": "p1", "role": "reader"}),
            None,
        )
        .await;
    assert!(err);
    assert_eq!(
        e["message"],
        "this stdio connection is bound to another actor or run"
    );
    let (err, v) = c
        .call(
            "rdy_session",
            json!({"actor": "agent:p", "run": "p1", "role": "reader"}),
            None,
        )
        .await;
    assert!(!err, "{v}");
    // closed inputs, and the load checks of the project server
    let (err, e) = c.call("rdy_instructions", json!({"lod": []}), None).await;
    assert!(err);
    assert_eq!(e["message"], "rdy_instructions: unknown parameter \"lod\"");
    let (err, e) = c
        .call("rdy_instructions", json!({"load": ["d-AAAAA"]}), None)
        .await;
    assert!(err);
    assert_eq!(e["code"], "E_NOT_FOUND", "{e}");
    // areas need a project's inbox
    let (err, e) = c
        .call(
            "rdy_session",
            json!({"actor": "agent:p", "run": "p1", "role": "reader", "areas": []}),
            None,
        )
        .await;
    assert!(err);
    assert_eq!(e["code"], "E_NO_PROJECT", "{e}");
    // reading made no store anywhere
    assert!(!dir.path().join(".rdy").exists());
    assert!(!meta.exists());
    // once the meta project exists, a new connection reads its canon as the person layer
    std::fs::create_dir_all(&meta).unwrap();
    drop(Rdy::init(&meta, "rdy-standard").unwrap());
    let mut c = Conn::start(dir.path(), home.path()).await;
    let (err, v) = c
        .call(
            "rdy_session",
            json!({"actor": "agent:p", "run": "p2", "role": "reader"}),
            None,
        )
        .await;
    assert!(!err, "{v}");
    let (err, v) = c.call("rdy_instructions", json!({}), None).await;
    assert!(!err, "{v}");
    assert_eq!(v, json!({"instructions": [], "menu": []}));
}
