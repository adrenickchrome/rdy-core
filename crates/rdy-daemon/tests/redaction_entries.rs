//! rdx12: one response-check layer (plan Redaction). A read state taken before a content removal
//! refuses every new response with E_REDACTED_STATE: every ctl op (daemon `dispatch`, which is
//! also the CLI's direct route), every MCP tool (`call_tool`) and every MCP resource and
//! template (`read_resource`) checks the state it acquires before it routes, with no name
//! allowlist. The check sits at these entries only, never in the state's getters.
use rdy_daemon::{OPS, OpContext, dispatch};
use rdy_engine::engine::{Engine, Local, WriteFn};
use rdy_engine::redaction::RedactInput;
use rdy_engine::semantic::lease::LeaseRegistry;
use rdy_engine::{
    BriefRegistry, E_REDACTED_STATE, OpenReport, Rdy, RdyError, ReadState, Session, WriteRequest,
};
use rdy_mcp::notify::NotifyHub;
use rdy_mcp::server::serve_on;
use rdy_model::{Hash, Provenance, Role};
use rdy_store::LandRequest;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// An engine whose reads return a retained state once one is set, else the current one.
struct Retained {
    inner: Local,
    stale: Mutex<Option<Arc<ReadState>>>,
}

impl Engine for Retained {
    fn read(&self) -> Arc<ReadState> {
        match &*self.stale.lock().unwrap() {
            Some(st) => Arc::clone(st),
            None => self.inner.read(),
        }
    }
    fn write(&self, f: WriteFn) -> Result<Value, RdyError> {
        self.inner.write(f)
    }
    fn open_report(&self) -> OpenReport {
        self.inner.open_report()
    }
    fn root(&self) -> PathBuf {
        self.inner.root()
    }
    fn briefs(&self) -> Arc<BriefRegistry> {
        self.inner.briefs()
    }
    fn leases(&self) -> Arc<LeaseRegistry> {
        self.inner.leases()
    }
}

fn human() -> Session {
    Session::human("human:t".into(), Role::Writer)
}

/// One MCP session over an in-process duplex: JSON-RPC lines out, replies by id.
struct Client {
    r: BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    w: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    next: u64,
}

impl Client {
    async fn start(engine: Arc<dyn Engine>) -> Client {
        let (client, server) = tokio::io::duplex(1 << 20);
        let (sr, sw) = tokio::io::split(server);
        tokio::spawn(serve_on(
            engine,
            1,
            NotifyHub::new(),
            Arc::new(rdy_engine::signing_crypto::UnavailableAgent),
            sr,
            sw,
        ));
        let (cr, cw) = tokio::io::split(client);
        let mut c = Client {
            r: BufReader::new(cr),
            w: cw,
            next: 0,
        };
        c.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "rdx12", "version": "0"}}),
        )
        .await;
        c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        c
    }

    async fn send(&mut self, v: Value) {
        let mut line = v.to_string();
        line.push('\n');
        self.w.write_all(line.as_bytes()).await.unwrap();
    }

    /// The reply to one request: its `result`, or its `error` under `"error"`.
    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        loop {
            let mut line = String::new();
            let n = tokio::time::timeout(Duration::from_secs(30), self.r.read_line(&mut line))
                .await
                .expect("a reply within 30 s")
                .unwrap();
            assert!(n > 0, "the session closed");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == id {
                return match v.get("error") {
                    Some(e) => json!({"error": e}),
                    None => v["result"].clone(),
                };
            }
        }
    }

    /// A tool's reply: `(is_error, the JSON in its text)`.
    async fn call(&mut self, name: &str, args: Value) -> (bool, Value) {
        let r = self
            .request("tools/call", json!({"name": name, "arguments": args}))
            .await;
        let text = r["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: {r}"));
        (r["isError"] == true, serde_json::from_str(text).unwrap())
    }
}

/// A project whose secret prose block was landed into canon from `work` by one proposal that
/// also carries its redaction request, applied by the store's own validated local-human landing
/// path (no engine landing code); the engine then republished the current state.
struct Applied {
    dir: tempfile::TempDir,
    rdy: Rdy,
    /// A state read before the request was recorded: its epoch is behind the store's.
    retained: Arc<ReadState>,
    doc: String,
    secret: String,
    /// The request's hash.
    request: String,
    /// The proposal version's ref, `P-<stable>@1`.
    proposal: String,
    /// The ids the first write created, by its `$` names.
    created: BTreeMap<String, String>,
}

fn applied() -> Applied {
    applied_with(json!([]), &[])
}

/// [`applied`], with `extra` create ops in the first write (after the doc `$d` and the secret
/// prose `$p`) and the blocks named by `targets` removed by the same request as `$p`.
fn applied_with(extra: Value, targets: &[&str]) -> Applied {
    let dir = tempfile::tempdir().unwrap();
    let mut rdy = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let mut ops = json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Doc", "doc_type": "page"}},
        {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "The launch code is ZEBRA-7731."}
    ]);
    ops.as_array_mut()
        .unwrap()
        .extend(extra.as_array().unwrap().iter().cloned());
    let ops = serde_json::from_value(ops).unwrap();
    let w = rdy
        .write(
            &human(),
            WriteRequest {
                view: "work".into(),
                ops,
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
    let (doc, secret) = (w.created["$d"].clone(), w.created["$p"].clone());

    // a reader retains its state; the request is prepared (Task 5) and proposed
    let retained = rdy.read();
    let req = rdy
        .redact(
            &human(),
            RedactInput {
                targets: std::iter::once(secret.clone())
                    .chain(targets.iter().map(|t| w.created[*t].clone()))
                    .collect(),
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    let request = req.redaction.expect("a request is recorded").to_string();
    let p = rdy
        .propose(&human(), "work", "Remove the code", None)
        .unwrap();
    let version: Hash = p.id.parse().unwrap();
    rdy.seal_open().unwrap();
    // the store's own validated local-human landing applies it, advancing the same family's
    // epoch; then the engine republishes the current state
    rdy.store_mut().set_held(false).unwrap();
    rdy.store_mut()
        .land_atomic(LandRequest {
            proposal: version,
            accept: None,
            approver: Provenance {
                actor: "human:t".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1_000_000,
            },
            alt: None,
            relay: None,
            evidence: None,
        })
        .unwrap();
    rdy.store_mut().set_held(true).unwrap();
    rdy.invalidate();
    Applied {
        dir,
        rdy,
        retained,
        doc,
        secret,
        request,
        proposal: p.r#ref,
        created: w.created,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rdx12_all_registered_ops_tools_and_resources_cross_the_epoch_guard() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let Applied {
        dir,
        rdy,
        retained,
        doc,
        proposal,
        ..
    } = applied();
    let refusal = retained.ensure_current_epoch().unwrap_err();
    assert_eq!(
        (refusal.code, refusal.message.as_str(), refusal.fix.as_str()),
        (
            E_REDACTED_STATE,
            "This read state predates content removal.",
            "Read the project again, or run the dump again."
        )
    );
    rdy.read().ensure_current_epoch().unwrap();

    let engine = Arc::new(Retained {
        inner: Local::new(rdy),
        stale: Mutex::new(None),
    });
    let refused = |e: &RdyError| e.code == E_REDACTED_STATE;
    let s = human();
    let cx = OpContext {
        engine: &*engine,
        session: &s,
        human: "human:t",
        cwd: dir.path(),
        agent: &rdy_engine::signing_crypto::UnavailableAgent,
    };

    // while the engine serves the current state: the host lists, and responses go through
    let mut c = Client::start(Arc::clone(&engine) as Arc<dyn Engine>).await;
    let tools: Vec<String> = c.request("tools/list", json!({})).await["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(tools.contains(&"rdy_redact".to_string()), "{tools:?}");
    let resources: Vec<String> = c.request("resources/list", json!({})).await["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["uri"].as_str().unwrap().to_string())
        .collect();
    let templates: Vec<(String, String)> =
        c.request("resources/templates/list", json!({})).await["resourceTemplates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                (
                    t["name"].as_str().unwrap().to_string(),
                    t["uriTemplate"].as_str().unwrap().to_string(),
                )
            })
            .collect();
    assert!(!resources.is_empty() && !templates.is_empty());
    let (err, v) = c
        .call(
            "rdy_session",
            json!({"actor": "agent:a", "run": "r1", "role": "writer"}),
        )
        .await;
    assert!(!err, "{v}");
    let (err, v) = c.call("rdy_overview", json!({})).await;
    assert!(!err, "the current state answers: {v}");
    let r = c
        .request("resources/read", json!({"uri": "rdy://project"}))
        .await;
    assert!(r.get("error").is_none(), "{r}");
    dispatch(&cx, "status", json!({})).expect("the current state answers");

    // the engine now serves the retained state: every entry refuses before its handler (the
    // arguments are empty, so a handler would refuse them otherwise)
    *engine.stale.lock().unwrap() = Some(Arc::clone(&retained));
    for op in OPS.iter().copied().chain(["frobnicate"]) {
        let e = dispatch(&cx, op, json!({})).expect_err(op);
        assert!(refused(&e), "{op}: {e}");
        assert_eq!(
            e.message, "This read state predates content removal.",
            "{op}"
        );
    }
    for name in &tools {
        let (err, v) = c.call(name, json!({})).await;
        assert!(err, "{name}: {v}");
        assert_eq!(v["code"], E_REDACTED_STATE, "{name}: {v}");
        assert_eq!(
            v["fix"], "Read the project again, or run the dump again.",
            "{name}"
        );
    }
    // a template is read with a valid fixture id
    let fixture = |name: &str| match name {
        "doc" => doc.clone(),
        "view" => "work".to_string(),
        "proposal" => proposal.clone(),
        other => panic!("template {other} has no fixture id: add one"),
    };
    let expanded: Vec<String> = templates
        .iter()
        .map(|(name, t)| {
            let open = t.find('{').unwrap();
            format!("{}{}", &t[..open], fixture(name))
        })
        .collect();
    for uri in resources.iter().chain(&expanded) {
        let r = c.request("resources/read", json!({"uri": uri})).await;
        assert_eq!(r["error"]["data"]["code"], E_REDACTED_STATE, "{uri}: {r}");
    }
    // the guard reads the state the engine serves: back to the current state, all answer
    *engine.stale.lock().unwrap() = None;
    let (err, v) = c.call("rdy_overview", json!({})).await;
    assert!(!err, "{v}");
    for uri in expanded {
        let r = c.request("resources/read", json!({"uri": uri})).await;
        assert!(r.get("error").is_none(), "{uri}: {r}");
    }
    dispatch(&cx, "status", json!({})).unwrap();
}

/// rdx12: a source read through the tools names the redaction of a stub, after its
/// `Content removed.`, in markdown and compact form and in the context's `redaction: {id}`;
/// the removed text never appears.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rdx12_source_reads_of_a_stub_name_its_redaction() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let a = applied();
    let engine: Arc<dyn Engine> = Arc::new(Local::new(a.rdy));
    let mut c = Client::start(engine).await;
    let (err, v) = c
        .call(
            "rdy_session",
            json!({"actor": "agent:a", "run": "r1", "role": "writer"}),
        )
        .await;
    assert!(!err, "{v}");
    let line = format!("Redaction: {}.", a.request);
    let (err, md) = c
        .call(
            "rdy_read",
            json!({"id": a.secret, "scope": "canon", "format": "markdown"}),
        )
        .await;
    assert!(!err, "{md}");
    let text = md["text"].as_str().unwrap();
    assert!(
        text.ends_with(&format!("Content removed.\n\n{line}")),
        "markdown: {text}"
    );
    let (err, compact) = c
        .call("rdy_read", json!({"id": a.doc, "scope": "canon"}))
        .await;
    assert!(!err, "{compact}");
    let text = compact["text"].as_str().unwrap();
    assert!(
        text.contains(&format!("{} prose: Content removed. {line}", a.secret)),
        "compact: {text}"
    );
    let (err, ctx) = c
        .call("rdy_context", json!({"id": a.secret, "scope": "canon"}))
        .await;
    assert!(!err, "{ctx}");
    assert_eq!(ctx["redaction"], json!({"id": a.request}), "{ctx}");
    for v in [&md, &compact, &ctx] {
        assert!(!v.to_string().contains("ZEBRA-7731"), "{v}");
    }
    drop(a.dir);
}

/// rdx12 (fix round 1): every block type's markdown source read of a stub shows the neutral stub,
/// its id, `Content removed.` and then its redaction line, whatever the type's own formatter
/// does with fields and body; a pattern read finds the removal notice. The removed text and
/// fields never appear.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rdx12_source_reads_of_every_stub_type_show_the_removal_notice() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let kinds = [
        ("$s", "section"),
        ("$tb", "table"),
        ("$dc", "decision"),
        ("$cd", "code"),
        ("$dg", "diagram"),
        ("$fl", "flow"),
        ("$tm", "term"),
        ("$q", "question"),
        ("$cm", "component"),
    ];
    let a = applied_with(
        json!([
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "ZEBRA-7731 section"}, "body": "ZEBRA-7731 body."},
            {"op": "create", "type": "section", "id": "$keep", "parent": "$d", "fields": {"title": "Kept"}},
            {"op": "create", "type": "table", "id": "$tb", "parent": "$keep", "fields": {"columns": ["Code"], "rows": [["ZEBRA-7731"]]}},
            {"op": "create", "type": "decision", "id": "$dc", "parent": "$keep", "fields": {"statement": "Use ZEBRA-7731", "status": "proposed"}},
            {"op": "create", "type": "code", "id": "$cd", "parent": "$keep", "fields": {"lang": "yaml"}, "body": "code: ZEBRA-7731\n"},
            {"op": "create", "type": "diagram", "id": "$dg", "parent": "$keep", "fields": {"lang": "mermaid", "alt": "ZEBRA-7731"}, "body": "graph TD\n  A[ZEBRA-7731] --> B\n"},
            {"op": "create", "type": "flow", "id": "$fl", "parent": "$keep", "fields": {"name": "ZEBRA-7731", "steps": ["Enter ZEBRA-7731"]}},
            {"op": "create", "type": "term", "id": "$tm", "parent": "$keep", "fields": {"name": "ZEBRA-7731"}, "body": "ZEBRA-7731."},
            {"op": "create", "type": "question", "id": "$q", "parent": "$keep", "fields": {"prompt": "Is ZEBRA-7731 live?"}},
            {"op": "create", "type": "component", "id": "$cm", "parent": "$keep", "fields": {"name": "ZEBRA-7731"}}
        ]),
        &kinds.map(|(k, _)| k),
    );
    let engine: Arc<dyn Engine> = Arc::new(Local::new(a.rdy));
    let mut c = Client::start(engine).await;
    let (err, v) = c
        .call(
            "rdy_session",
            json!({"actor": "agent:a", "run": "r1", "role": "writer"}),
        )
        .await;
    assert!(!err, "{v}");
    let line = format!("Redaction: {}.", a.request);
    let mut wrong = vec![];
    for (key, kind) in std::iter::once(("$p", "prose")).chain(kinds) {
        let id = &a.created[key];
        let (err, md) = c
            .call(
                "rdy_read",
                json!({"id": id, "scope": "canon", "format": "markdown"}),
            )
            .await;
        assert!(!err, "{kind}: {md}");
        let text = md["text"].as_str().unwrap();
        if !text.starts_with(&format!("`{id}` Content removed.\n\n{line}")) {
            wrong.push(format!("{kind}: {text}"));
            continue;
        }
        let (err, hit) = c
            .call(
                "rdy_read",
                json!({"id": id, "scope": "canon", "format": "markdown", "pattern": "Content removed."}),
            )
            .await;
        assert!(!err, "{kind}: {hit}");
        if hit["matched"] != 1 || !hit["text"].as_str().unwrap().contains(&line) {
            wrong.push(format!("{kind} pattern: {hit}"));
        }
        for v in [&md, &hit] {
            assert!(!v.to_string().contains("ZEBRA-7731"), "{kind}: {v}");
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n----\n"));
    drop(a.dir);
}
