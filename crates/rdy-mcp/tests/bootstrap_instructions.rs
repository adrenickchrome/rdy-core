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
                // the home of each family: HOME on Unix, USERPROFILE on Windows
                home: Some(self.0.clone().into()),
                user_profile: Some(self.0.clone().into()),
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

/// The built-in layer as a reply carries it: its rows and its menu (the bootstrap whole).
fn builtin_set(full: bool) -> Value {
    let part = instructions::builtin(&LoadRequest::parse(&[]).unwrap().with_full(full)).unwrap();
    serde_json::to_value(part).unwrap()
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
    let built = builtin_set(false);
    assert_eq!(
        built["instructions"].as_array().unwrap().len(),
        3,
        "{built}"
    );
    assert_eq!(built["menu"].as_array().unwrap().len(), 12, "{built}");
    assert_eq!(
        v,
        json!({
            "instructions": built["instructions"],
            "menu": built["menu"],
            "hint": format!(
                "{} {}",
                instructions::create_hint(&meta),
                instructions::PROJECTLESS_SUFFIX
            )
        })
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
    let built = builtin_set(false);
    assert_eq!(
        v,
        json!({
            "instructions": built["instructions"],
            "menu": built["menu"],
            "hint": instructions::META_READY_HINT
        })
    );
    // rtc5: with the meta project in place, the same listing gains the person's rule tool and
    // no other
    assert_eq!(
        tool_names(&mut c).await,
        ["rdy_session", "rdy_instructions", "rdy_rule"]
    );
}

/// The names the connection lists now, in order.
async fn tool_names(c: &mut Conn) -> Vec<String> {
    let listed = c.request("tools/list", json!({})).await;
    listed["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools in {listed}"))
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rtc5_projectless_list_gains_rule_after_setup() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let meta = home.path().join(".rdy").join("meta");
    let mut c = Conn::start(dir.path(), home.path()).await;
    let two = ["rdy_session", "rdy_instructions"];
    assert_eq!(tool_names(&mut c).await, two);
    // before setup a direct call is the missing-meta refusal, and makes nothing
    let (err, e) = c
        .call(
            "rdy_rule",
            json!({"scope": "person", "answer": {"ref": "P-ABCDE@1", "option": "later",
                "card_hash": "a".repeat(64), "evidence": "Not yet."}}),
            Some(claim("agent:p", "p1")),
        )
        .await;
    assert!(err);
    assert_eq!(
        (e["code"].as_str(), e["message"].as_str(), e["fix"].as_str()),
        (
            Some("E_NO_PROJECT"),
            Some("the meta project does not exist"),
            Some("run rdy setup")
        ),
        "{e}"
    );
    assert!(!meta.exists());
    // setup happens while the connection stays open: the listing follows it, with the project
    // server's own entry for the tool
    std::fs::create_dir_all(&meta).unwrap();
    drop(Rdy::init(&meta, "rdy-standard").unwrap());
    let listed = c.request("tools/list", json!({})).await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(
        tools
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["rdy_session", "rdy_instructions", "rdy_rule"]
    );
    let entry = &tools[2];
    assert_eq!(entry["description"], rdy_mcp::tools::RULE_DOC);
    assert_eq!(entry["inputSchema"]["additionalProperties"], false);
    // the schema the project server lists, less the title and description the listing drops
    let mut want = serde_json::to_value(schemars::schema_for!(rdy_mcp::tools::RuleParams)).unwrap();
    let want_object = want.as_object_mut().unwrap();
    want_object.remove("title");
    want_object.remove("description");
    assert_eq!(entry["inputSchema"], want);
    // other project tools stay unavailable on this connection
    let (err, e) = c.call("rdy_overview", json!({}), None).await;
    assert!(err);
    assert_eq!(e["code"], "E_NO_PROJECT", "{e}");
    // removing the meta project takes the tool away again; a new one brings it back
    std::fs::remove_dir_all(&meta).unwrap();
    assert_eq!(tool_names(&mut c).await, two);
    std::fs::create_dir_all(&meta).unwrap();
    drop(Rdy::init(&meta, "rdy-standard").unwrap());
    assert_eq!(
        tool_names(&mut c).await,
        ["rdy_session", "rdy_instructions", "rdy_rule"]
    );
    // a call rechecks too: with the meta project gone it is the missing-meta refusal
    std::fs::remove_dir_all(&meta).unwrap();
    let (err, e) = c
        .call(
            "rdy_rule",
            json!({"scope": "person", "answer": {"ref": "P-ABCDE@1", "option": "later",
                "card_hash": "a".repeat(64), "evidence": "Not yet."}}),
            None,
        )
        .await;
    assert!(err);
    assert_eq!(e["message"], "the meta project does not exist", "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agi3_projectless_session_then_builtin_instructions() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut c = Conn::start(dir.path(), home.path()).await;
    // no session: the existing refusal and fix, for every form of the call
    let (err, e) = c.call("rdy_instructions", json!({}), None).await;
    assert!(err);
    assert_eq!(e["code"], "E_NO_SESSION", "{e}");
    let fix = e["fix"].clone();
    assert!(fix.as_str().is_some_and(|f| !f.is_empty()), "{e}");
    let (err, e2) = c
        .call("rdy_instructions", json!({"full": true}), None)
        .await;
    assert!(err);
    assert_eq!((&e2["code"], &e2["fix"]), (&e["code"], &fix), "{e2}");
    // a reader session reads bootstrap, menu and the setup hint
    let (err, v) = c
        .call(
            "rdy_session",
            json!({"actor": "agent:p", "run": "p1", "role": "reader"}),
            None,
        )
        .await;
    assert!(!err, "{v}");
    let (err, v) = c.call("rdy_instructions", json!({}), None).await;
    assert!(!err, "{v}");
    let ids: Vec<&str> = v["instructions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "rdy:bootstrap:before",
            "rdy:bootstrap:approval",
            "rdy:bootstrap:after"
        ]
    );
    assert_eq!(v["menu"].as_array().unwrap().len(), 12, "{v}");
    assert!(
        v["hint"]
            .as_str()
            .unwrap()
            .ends_with(instructions::PROJECTLESS_SUFFIX),
        "{v}"
    );
    // full adds every optional built-in body, in menu order, once each
    let (err, f) = c
        .call("rdy_instructions", json!({"full": true}), None)
        .await;
    assert!(!err, "{f}");
    assert_eq!(f["instructions"], builtin_set(true)["instructions"], "{f}");
    assert_eq!(f["instructions"].as_array().unwrap().len(), 14, "{f}");
    // closed: a typed null or string is refused, nothing is read
    for bad in [
        json!({"full": null}),
        json!({"full": "yes"}),
        json!({"ful": true}),
    ] {
        let (err, e) = c.call("rdy_instructions", bad, None).await;
        assert!(err, "{e}");
    }
}
