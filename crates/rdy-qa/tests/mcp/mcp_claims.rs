//! ui5 (D-4PHYQ, D-ME2C0, D-Q8NFG): a stdio connection's claim, from `_meta` or `rdy_session`,
//! and the session settings saved per actor and run.
use rdy_engine::engine::{Engine, Local};
use rdy_mcp::claims::{self, Asked, Claim, MAX_SETTINGS, Registry, SETTINGS_IDLE};
use rdy_mcp::notify::NotifyHub;
use rdy_mcp::tools::{self, McpState};
use rdy_model::Role;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::project;
use rdy_qa::mcp::{DEADLINE, McpClient, Reply};
use rdy_qa::packs::{QA_PACK_YAML, use_pack};
use serde_json::{Value, json};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

const NO_SESSION_FIX: &str = "state rdy/actor and rdy/run in _meta, or call rdy_session";
const MISMATCH: &str = "this stdio connection is bound to another actor or run";
const MISMATCH_FIX: &str = "use the connection's actor and run, or open another stdio connection";
const KEYS: &str = "the parameters here are rdy/actor, rdy/dedupe, rdy/role, rdy/run";

/// The reply of a raw JSON-RPC response to `tools/call`.
fn decode(v: &Value) -> Reply {
    if let Some(e) = v.get("error") {
        return Reply::RpcError(e.clone());
    }
    let text = v["result"]["content"][0]["text"].as_str().unwrap_or("null");
    let body: Value = serde_json::from_str(text).unwrap_or(Value::String(text.into()));
    if v["result"]["isError"] == json!(true) {
        Reply::ToolError(body)
    } else {
        Reply::Ok(body)
    }
}

/// `tools/call` of `tool` with `args` and the request metadata `meta`.
fn call_meta(c: &mut McpClient, tool: &str, mut args: Value, meta: Value) -> Reply {
    rdy_qa::mcp::supply_kungfu(tool, &mut args);
    let id = c.next_id();
    let line = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": tool, "arguments": args, "_meta": meta}});
    c.send_raw(&line.to_string());
    decode(&c.recv(id, DEADLINE))
}

/// The raw reply to a request `method` with `params`.
fn request(c: &mut McpClient, method: &str, params: Value) -> Value {
    let id = c.next_id();
    c.send_raw(
        &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string(),
    );
    c.recv(id, DEADLINE)
}

fn claim(actor: &str, run: Option<&str>) -> Value {
    json!({"rdy/actor": actor, "rdy/run": run})
}

fn tool_error(r: Reply) -> Value {
    match r {
        Reply::ToolError(v) => v,
        other => panic!("expected an rdy tool error, got {other:?}"),
    }
}

fn refusal(r: Reply) -> (String, String, String) {
    let e = tool_error(r);
    let s = |k: &str| e[k].as_str().unwrap_or_default().to_string();
    (s("code"), s("message"), s("fix"))
}

fn triple(code: &str, message: &str, fix: &str) -> (String, String, String) {
    (code.into(), message.into(), fix.into())
}

/// A folder named `name`, as one write's ops.
fn folder(name: &str) -> Value {
    json!({"ops": [{"op": "create", "type": "folder", "id": "$f", "fields": {"name": name, "title": name}}]})
}

fn stdio() -> (tempfile::TempDir, McpClient) {
    let (dir, r) = project();
    drop(r);
    let c = McpClient::start(dir.path());
    (dir, c)
}

#[test]
fn ui5_meta_and_session_bind_the_same_stdio_identity() {
    let (dir, mut a) = stdio();
    // the first claim, by _meta, binds the connection; the write is the call's own session's
    let w = call_meta(
        &mut a,
        "rdy_write",
        folder("m"),
        claim("agent:m", Some("m1")),
    )
    .ok();
    assert!(w["change"].is_string(), "{w}");
    let ov = a.call("rdy_overview", json!({})).ok();
    assert_eq!(
        ov["session"],
        json!({"actor": "agent:m", "view": "m1"}),
        "{ov}"
    );
    let shown = a.call("rdy_view", json!({"op": "show", "view": "m1"})).ok();
    assert_eq!(shown["changes"][0]["actor"], "agent:m", "{shown}");
    assert_eq!(shown["changes"][0]["run"], "m1", "{shown}");
    // rdy_session's arguments are a claim: the same one is accepted, another is refused
    let s = a.call(
        "rdy_session",
        json!({"actor": "agent:m", "run": "m1", "role": "writer"}),
    );
    assert!(s.is_ok(), "{s:?}");
    for r in [
        a.call(
            "rdy_session",
            json!({"actor": "agent:other", "run": "m1", "role": "writer"}),
        ),
        a.call("rdy_session", json!({"actor": "agent:m", "role": "writer"})),
        call_meta(
            &mut a,
            "rdy_overview",
            json!({}),
            claim("agent:other", Some("o")),
        ),
        call_meta(&mut a, "rdy_overview", json!({}), claim("agent:m", None)),
    ] {
        assert_eq!(refusal(r), triple("E_BAD_INPUT", MISMATCH, MISMATCH_FIX));
    }
    let ov = a.call("rdy_overview", json!({})).ok();
    assert_eq!(ov["session"]["actor"], "agent:m", "the claim holds: {ov}");
    // the matching claim in _meta is accepted
    assert!(
        call_meta(
            &mut a,
            "rdy_overview",
            json!({}),
            claim("agent:m", Some("m1"))
        )
        .is_ok()
    );
    drop(a);

    // both carriers on one call must agree; a refused rdy_session binds nothing
    let mut b = McpClient::start(dir.path());
    let both = json!({"actor": "agent:b", "run": "b1", "role": "reader"});
    let r = call_meta(
        &mut b,
        "rdy_session",
        both.clone(),
        claim("agent:c", Some("c1")),
    );
    assert_eq!(refusal(r), triple("E_BAD_INPUT", MISMATCH, MISMATCH_FIX));
    let r = b.call("rdy_overview", json!({}));
    assert_eq!(refusal(r).2, NO_SESSION_FIX);
    let r = call_meta(&mut b, "rdy_session", both, claim("agent:b", Some("b1"))).ok();
    assert_eq!(
        (r["role"].as_str(), r["view"].as_str()),
        (Some("reader"), Some("b1"))
    );
    drop(b);

    // a null run is the legacy no-run claim; rdy_session's role defaults to writer
    let mut n = McpClient::start(dir.path());
    let ov = call_meta(&mut n, "rdy_overview", json!({}), claim("agent:n", None)).ok();
    assert_eq!(ov["session"]["view"], "work-agent-n", "{ov}");
    let s = n.call("rdy_session", json!({"actor": "agent:n"})).ok();
    assert_eq!(s["role"], "writer", "{s}");
    let r = call_meta(
        &mut n,
        "rdy_overview",
        json!({}),
        claim("agent:n", Some("x")),
    );
    assert_eq!(refusal(r).1, MISMATCH);
    drop(n);

    // a CI actor claims as an agent does
    let mut ci = McpClient::start(dir.path());
    let ov = call_meta(
        &mut ci,
        "rdy_overview",
        json!({}),
        claim("ci:0123456789abcdef0123456789abcdef/nightly", Some("r")),
    )
    .ok();
    assert_eq!(
        ov["session"]["actor"], "ci:0123456789abcdef0123456789abcdef/nightly",
        "{ov}"
    );
    drop(ci);

    // a new connection is a new claim: another actor may bind it
    let mut again = McpClient::start(dir.path());
    let ov = call_meta(
        &mut again,
        "rdy_overview",
        json!({}),
        claim("agent:other", Some("o")),
    )
    .ok();
    assert_eq!(ov["session"]["actor"], "agent:other", "{ov}");

    // through one daemon, a reconnect finds the actor and run's saved settings
    let s = Served::new();
    let mut d1 = McpClient::start_proxy(&s);
    let r = d1
        .call(
            "rdy_session",
            json!({"actor": "agent:s", "run": "s1", "role": "reader"}),
        )
        .ok();
    assert_eq!(r["role"], "reader", "{r}");
    drop(d1);
    let mut d2 = McpClient::start_proxy(&s);
    let r = call_meta(
        &mut d2,
        "rdy_write",
        folder("s"),
        claim("agent:s", Some("s1")),
    );
    let (code, message, _) = refusal(r);
    assert_eq!(
        (code.as_str(), message.as_str()),
        ("E_PERMISSION", "role Reader cannot write")
    );
}

#[test]
fn ui5_every_tool_and_resource_read_requires_a_claim() {
    let (_dir, mut c) = stdio();
    // discovery needs no claim
    assert!(
        c.initialize_result()["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("State rdy/actor and rdy/run in _meta, or call rdy_session.")
    );
    let tools = c.tools();
    assert!(tools.len() > 20, "{} tools", tools.len());
    assert!(!c.list_resources().is_empty());
    assert!(!c.list_resource_templates().is_empty());
    assert_eq!(request(&mut c, "ping", json!({}))["result"], json!({}));
    // every tool refuses an unclaimed call before it reads its arguments; rdy_session's
    // arguments are a claim, so its missing actor is the refusal
    for t in &tools {
        let name = t["name"].as_str().unwrap();
        let r = c.call(name, json!({"x_not_a_parameter": [1]}));
        if name == "rdy_session" {
            assert_eq!(refusal(r).0, "E_BAD_INPUT", "{name}");
            continue;
        }
        assert_eq!(
            refusal(r),
            triple("E_NO_SESSION", "no session", NO_SESSION_FIX),
            "{name}"
        );
    }
    for (tool, args) in [
        ("rdy_schema", json!({})),
        ("rdy_write", json!({"ops": []})),
        ("rdy_read", json!({"id": 5})),
    ] {
        let r = call_meta(
            &mut c,
            tool,
            args,
            json!({"progressToken": 1, "rdy/role": "reader"}),
        );
        assert_eq!(
            refusal(r).0,
            "E_NO_SESSION",
            "{tool}: options alone are no claim"
        );
    }
    for uri in ["rdy://pack", "rdy://project", "rdy://inbox", "rdy://doc/x"] {
        let r = c.read_resource(uri);
        assert_eq!(r["error"]["code"], -32600, "{uri}: {r}");
        assert_eq!(r["error"]["data"]["code"], "E_NO_SESSION", "{uri}: {r}");
        assert_eq!(r["error"]["data"]["fix"], NO_SESSION_FIX, "{uri}: {r}");
    }
    // a resource read may carry the claim
    let r = request(
        &mut c,
        "resources/read",
        json!({"uri": "rdy://pack", "_meta": claim("agent:r", Some("r1"))}),
    );
    assert!(r["result"]["contents"].is_array(), "{r}");
    // unrelated metadata keeps the bound identity
    let ov = call_meta(
        &mut c,
        "rdy_overview",
        json!({}),
        json!({"progressToken": 7, "example.com/trace": "t"}),
    )
    .ok();
    assert_eq!(ov["session"]["actor"], "agent:r", "{ov}");
    let r = request(
        &mut c,
        "resources/read",
        json!({"uri": "rdy://project", "_meta": {"progressToken": "p"}}),
    );
    assert!(r["result"]["contents"].is_array(), "{r}");
    assert!(c.call("rdy_schema", json!({})).is_ok());
}

#[test]
fn ui5_claim_validation_reuses_identity_and_role_rules() {
    let (dir, r) = project();
    drop(r);
    // the refusals rdy_session gives for the same actor and run (one stdio server at a time
    // holds the project)
    let mut fresh = McpClient::start(dir.path());
    let session_refusal = |c: &mut McpClient, actor: &str, run: &str| {
        refusal(c.call(
            "rdy_session",
            json!({"actor": actor, "run": run, "role": "writer"}),
        ))
    };
    let human = session_refusal(&mut fresh, "human:pat", "r");
    let bad_actor = session_refusal(&mut fresh, "Agent A", "r");
    let bad_run = session_refusal(&mut fresh, "agent:v", "bad run!");
    let approver = refusal(fresh.call(
        "rdy_session",
        json!({"actor": "agent:v", "run": "r", "role": "approver"}),
    ));
    let boss = refusal(fresh.call(
        "rdy_session",
        json!({"actor": "agent:v", "run": "r", "role": "boss"}),
    ));
    assert_eq!(human.0, "E_PERMISSION");
    assert_eq!(
        human.2,
        "claim your own agent: actor, and relay the person's words with rdy_answer"
    );
    assert_eq!(
        approver,
        triple(
            "E_PERMISSION",
            "approver sessions are not available over MCP",
            "relay the person's answer with rdy_answer"
        )
    );
    assert_eq!(
        boss,
        triple(
            "E_BAD_INPUT",
            "unknown role \"boss\"",
            "use reader, writer or reviewer"
        )
    );
    drop(fresh);
    let mut c = McpClient::start(dir.path());
    let invalid: Vec<(Value, (String, String, String))> = vec![
        (claim("human:pat", Some("r")), human),
        (claim("Agent A", Some("r")), bad_actor),
        (claim("agent:v", Some("bad run!")), bad_run),
        (
            json!({"rdy/actor": "agent:v", "rdy/run": "v1", "rdy/role": "approver"}),
            approver.clone(),
        ),
        (json!({"rdy/role": "approver"}), approver),
        (json!({"rdy/role": "boss"}), boss),
        (
            json!({"rdy/actor": "agent:v"}),
            triple(
                "E_BAD_INPUT",
                "a _meta claim needs both rdy/actor and rdy/run",
                "pass both keys, or omit both and use the connection's claim",
            ),
        ),
        (
            json!({"rdy/actor": "agent:v", "rdy/run": 5, "rdy/dedupe": "yes"}),
            triple(
                "E_BAD_INPUT",
                "_meta: parameter \"rdy/run\" must be a string, not a number",
                "pass \"rdy/run\" as a string",
            ),
        ),
        (
            json!({"rdy/dedupe": "yes"}),
            triple(
                "E_BAD_INPUT",
                "_meta: parameter \"rdy/dedupe\" must be a boolean, not a string",
                "pass \"rdy/dedupe\" as a boolean",
            ),
        ),
        (
            json!({"rdy/role": ["reader"]}),
            triple(
                "E_BAD_INPUT",
                "_meta: parameter \"rdy/role\" must be a string, not a list",
                "pass \"rdy/role\" as a string",
            ),
        ),
        (
            json!({"rdy/rol": "reader", "rdy/zzzzzz": 1}),
            triple(
                "E_BAD_INPUT",
                "_meta: unknown parameter \"rdy/rol\"",
                &format!("did you mean \"rdy/role\"? {KEYS}"),
            ),
        ),
        (
            json!({"rdy/zzzzzz": 1}),
            triple(
                "E_BAD_INPUT",
                "_meta: unknown parameter \"rdy/zzzzzz\"",
                &format!("remove \"rdy/zzzzzz\"; {KEYS}"),
            ),
        ),
    ];
    // on an unbound connection, an invalid claim binds nothing
    for (meta, want) in &invalid {
        let r = call_meta(&mut c, "rdy_write", folder("nothing"), meta.clone());
        assert_eq!(&refusal(r), want, "{meta}");
    }
    assert_eq!(refusal(c.call("rdy_overview", json!({}))).0, "E_NO_SESSION");
    // on a bound connection, it keeps the claim and its settings
    let s = c.call(
        "rdy_session",
        json!({"actor": "agent:v", "run": "v1", "role": "writer", "view": "kept"}),
    );
    assert!(s.is_ok(), "{s:?}");
    for (meta, want) in &invalid {
        let r = call_meta(&mut c, "rdy_write", folder("nothing"), meta.clone());
        assert_eq!(&refusal(r), want, "{meta}");
        let r = request(
            &mut c,
            "resources/read",
            json!({"uri": "rdy://project", "_meta": meta}),
        );
        assert_eq!(r["error"]["data"]["message"], json!(want.1), "{meta}: {r}");
    }
    let ov = c.call("rdy_overview", json!({})).ok();
    assert_eq!(
        ov["session"],
        json!({"actor": "agent:v", "view": "kept"}),
        "{ov}"
    );
    let r = c.call("rdy_read", json!({"id": "nothing"}));
    assert_eq!(
        refusal(r).0,
        "E_UNKNOWN_BLOCK",
        "the refused claims wrote nothing"
    );
    let r = call_meta(
        &mut c,
        "rdy_overview",
        json!({}),
        claim("agent:w", Some("v1")),
    );
    assert_eq!(refusal(r).1, MISMATCH);
}

/// A state of `engine` on `hub`: one connection of one server.
fn state(engine: &Arc<Local>, hub: &Arc<NotifyHub>) -> McpState {
    let mut st = McpState::new(Arc::clone(engine) as Arc<dyn Engine>);
    st.hub = Arc::clone(hub);
    st
}

fn session_params(actor: &str, run: &str, role: &str, view: Option<&str>) -> tools::SessionParams {
    let mut v = json!({"actor": actor, "run": run, "role": role});
    if let Some(view) = view {
        v["view"] = json!(view);
    }
    serde_json::from_value(v).unwrap()
}

/// `rdy_session` through the protocol's door.
fn open(st: &mut McpState, p: tools::SessionParams) -> Result<Value, rdy_engine::RdyError> {
    claims::call(st, None, "rdy_session", |st| tools::rdy_session(st, p))
}

/// The role and view a claimed call of `st` runs with.
fn seen(st: &mut McpState, asked: Option<&Asked>) -> Result<(Role, String), rdy_engine::RdyError> {
    claims::call(st, asked, "rdy_overview", |st| {
        Ok((st.session.as_ref().unwrap().role, st.view.clone()))
    })
}

/// `rdy_inbox` with `args` through the protocol's door, as the call `asked` states.
fn inbox(st: &mut McpState, asked: Option<&Asked>, args: Value) -> Result<Value, String> {
    claims::call(st, asked, "rdy_inbox", |st| {
        tools::rdy_inbox(st, serde_json::from_value(args).unwrap())
    })
    .map_err(|e| format!("{}: {}", e.code, e.message))
}

/// The events of an inbox page.
fn events(page: &Value) -> Vec<String> {
    page["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("an inbox page: {page}"))
        .iter()
        .map(|e| e["event"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn role_only(role: Role) -> Asked {
    Asked::Options {
        role: Some(role),
        dedupe: None,
    }
}

#[test]
fn ui5_role_override_is_local_to_one_concurrent_call() {
    // two connections of one server share its settings; each call keeps its own role
    let (dir, r) = project();
    let engine = Arc::new(Local::new(r));
    let hub = NotifyHub::new();
    let (mut one, mut two) = (state(&engine, &hub), state(&engine, &hub));
    open(&mut one, session_params("agent:k", "k1", "writer", None)).unwrap();
    open(&mut two, session_params("agent:k", "k1", "writer", None)).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let reader = {
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            let asked = role_only(Role::Reader);
            let r = claims::call(&mut one, Some(&asked), "rdy_write", |st| {
                barrier.wait();
                let role = st.session.as_ref().unwrap().role;
                barrier.wait();
                let w = tools::rdy_write(st, serde_json::from_value(folder("r")).unwrap());
                Ok((role, w.map_err(|e| e.code)))
            });
            (r, one)
        })
    };
    let writer = {
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            let r = claims::call(&mut two, None, "rdy_write", |st| {
                barrier.wait();
                let role = st.session.as_ref().unwrap().role;
                barrier.wait();
                let w = tools::rdy_write(st, serde_json::from_value(folder("w")).unwrap());
                Ok((role, w.map(|_| ()).map_err(|e| e.code)))
            });
            (r, two)
        })
    };
    let ((r1, mut one), (r2, mut two)) = (reader.join().unwrap(), writer.join().unwrap());
    let (role, w) = r1.unwrap();
    assert_eq!((role, w.unwrap_err()), (Role::Reader, "E_PERMISSION"));
    assert_eq!(r2.unwrap(), (Role::Writer, Ok(())));
    // the override held for its call only, and saved nothing
    assert_eq!(seen(&mut one, None).unwrap().0, Role::Writer);
    assert_eq!(seen(&mut two, None).unwrap().0, Role::Writer);
    assert_eq!(
        hub.claims()
            .get(&("agent:k".into(), Some("k1".into())))
            .unwrap()
            .role,
        Some(Role::Writer)
    );
    assert!(one.session.as_ref().is_some_and(|s| s.role == Role::Writer));
    // rdy_session saves its role and view; a one-call role does not
    open(
        &mut one,
        session_params("agent:k", "k1", "reader", Some("canon")),
    )
    .unwrap();
    assert_eq!(
        seen(&mut two, None).unwrap(),
        (Role::Reader, "canon".to_string())
    );
    assert_eq!(
        seen(&mut two, Some(&role_only(Role::Writer))).unwrap().0,
        Role::Writer
    );
    assert_eq!(seen(&mut two, None).unwrap().0, Role::Reader);
    drop((one, two, engine, dir));

    // the inbox (review fix round 1): a one-call role filters that call's page and its ack; the
    // subscription keeps its cursor and areas and the saved role, also when that call creates it
    let (dir, mut r) = project();
    let spec = rdy_qa::edit_loop::spec_doc(&mut r);
    let engine = Arc::new(Local::new(r));
    let hub = NotifyHub::new();
    let (mut k, mut n, mut w) = (
        state(&engine, &hub),
        state(&engine, &hub),
        state(&engine, &hub),
    );
    open(&mut k, session_params("agent:k", "k1", "writer", None)).unwrap();
    open(&mut w, session_params("agent:w", "w1", "writer", None)).unwrap();
    let reviewer = role_only(Role::Reviewer);
    // n's first call is a reviewer-override inbox read: it subscribes n with the saved role
    let n_claim = Asked::Claim(Claim {
        actor: "agent:n".into(),
        run: Some("n1".into()),
        role: Some(Role::Reviewer),
        dedupe: None,
    });
    let started = inbox(&mut n, Some(&n_claim), json!({})).unwrap();
    assert_eq!(started["started"], json!(true), "{started}");
    let saved_role = |actor: &str, run: &str| {
        let st = engine.read();
        st.reader().inbox(actor, Some(run)).unwrap().unwrap().role
    };
    assert_eq!(saved_role("agent:n", "n1"), "writer");
    // another actor's change asks for reviews
    let ops =
        json!({"ops": [{"op": "update", "id": spec.c.to_string(), "body": "C contradicts B."}]});
    claims::call(&mut w, None, "rdy_write", |st| {
        tools::rdy_write(st, serde_json::from_value(ops).unwrap())
    })
    .unwrap();
    for (name, st) in [("k", &mut k), ("n", &mut n)] {
        let mine = inbox(st, None, json!({})).unwrap();
        assert!(events(&mine).is_empty(), "{name}: a writer call: {mine}");
        let asked = inbox(st, Some(&reviewer), json!({})).unwrap();
        assert_eq!(events(&asked), ["review_requested"], "{name}: {asked}");
        let acked = inbox(st, Some(&reviewer), json!({"ack": true})).unwrap();
        assert_eq!(events(&acked), ["review_requested"], "{name}: {acked}");
        assert_eq!(acked["acked"], json!(true), "{name}: {acked}");
        // the ack moved the cursor past the entries it returned
        let after = inbox(st, Some(&reviewer), json!({})).unwrap();
        assert!(events(&after).is_empty(), "{name}: {after}");
    }
    assert_eq!(saved_role("agent:k", "k1"), "writer");
    assert_eq!(saved_role("agent:n", "n1"), "writer");
    assert_eq!(seen(&mut n, None).unwrap().0, Role::Writer);
    drop((k, n, w, engine, dir));

    // over the wire: a reader override refuses one write, the next call writes
    let (_d, mut c) = stdio();
    let k = claim("agent:k", Some("k1"));
    let mut reader = k.clone();
    reader["rdy/role"] = json!("reader");
    assert_eq!(
        refusal(call_meta(&mut c, "rdy_write", folder("a"), reader.clone())).0,
        "E_PERMISSION"
    );
    assert!(call_meta(&mut c, "rdy_write", folder("b"), k).is_ok());
    assert!(c.call("rdy_write", folder("c")).is_ok());
    let s = c.call(
        "rdy_session",
        json!({"actor": "agent:k", "run": "k1", "role": "reader"}),
    );
    assert!(s.is_ok(), "{s:?}");
    assert_eq!(refusal(c.call("rdy_write", folder("d"))).0, "E_PERMISSION");
    let w = call_meta(
        &mut c,
        "rdy_write",
        folder("e"),
        json!({"rdy/role": "writer"}),
    );
    assert!(w.is_ok(), "{w:?}");
    assert_eq!(refusal(c.call("rdy_write", folder("f"))).0, "E_PERMISSION");
}

#[test]
fn ui5_settings_expire_without_changing_the_connection_claim() {
    let (dir, r) = project();
    drop(r);
    use_pack(dir.path(), "rdy-qa", QA_PACK_YAML, "");
    let engine = Arc::new(Local::open(dir.path()).unwrap());
    let base = Instant::now();
    let now = Arc::new(Mutex::new(base));
    let clock = {
        let now = Arc::clone(&now);
        Arc::new(move || *now.lock().unwrap())
    };
    let at = |secs: u64| *now.lock().unwrap() = base + Duration::from_secs(secs);
    let hub = NotifyHub::with_claims(Registry::with_clock(clock));
    let mut st = state(&engine, &hub);
    let mut p = session_params("agent:e", "e1", "reader", Some("ev"));
    p.areas = Some(vec!["payments".into()]);
    open(&mut st, p).unwrap();
    let key = ("agent:e".to_string(), Some("e1".to_string()));
    let reader = (Role::Reader, "ev".to_string());
    // a call before the hour uses the settings and refreshes them
    at(SETTINGS_IDLE.as_secs() - 1);
    assert_eq!(seen(&mut st, None).unwrap(), reader);
    at(2 * SETTINGS_IDLE.as_secs() - 2);
    assert_eq!(
        seen(&mut st, None).unwrap(),
        reader,
        "the call refreshed them"
    );
    // at one hour without a call they are gone; the connection's claim is not
    at(3 * SETTINGS_IDLE.as_secs() - 3);
    assert!(hub.claims().get(&key).is_some(), "a second before the hour");
    at(3 * SETTINGS_IDLE.as_secs() - 2);
    assert!(hub.claims().get(&key).is_none());
    assert_eq!(
        seen(&mut st, None).unwrap(),
        (Role::Writer, "e1".to_string())
    );
    assert_eq!(st.bound, Some(key.clone()));
    let other = Asked::Claim(Claim {
        actor: "agent:x".into(),
        run: Some("e1".into()),
        role: None,
        dedupe: None,
    });
    assert_eq!(seen(&mut st, Some(&other)).unwrap_err().message, MISMATCH);
    // areas live with the inbox subscription, not the settings
    let sub = engine
        .read()
        .reader()
        .inbox("agent:e", Some("e1"))
        .unwrap()
        .unwrap();
    assert_eq!(sub.areas, ["payments"]);
    // a restart drops every setting
    open(
        &mut st,
        session_params("agent:e", "e1", "reader", Some("ev")),
    )
    .unwrap();
    let restarted = NotifyHub::new();
    let mut st2 = state(&engine, &restarted);
    let asked = Asked::Claim(Claim {
        actor: "agent:e".into(),
        run: Some("e1".into()),
        role: None,
        dedupe: None,
    });
    assert_eq!(
        seen(&mut st2, Some(&asked)).unwrap(),
        (Role::Writer, "e1".to_string())
    );
    // 1024 settings fit; a 1025th actor and run is refused and binds nothing
    let full = NotifyHub::with_claims(Registry::with_clock({
        let now = Arc::clone(&now);
        Arc::new(move || *now.lock().unwrap())
    }));
    for n in 0..MAX_SETTINGS - 1 {
        full.claims()
            .save(
                &(format!("agent:f{n}"), None),
                "v".into(),
                Role::Writer,
                None,
            )
            .unwrap();
    }
    let mut last = state(&engine, &full);
    open(&mut last, session_params("agent:last", "l", "writer", None)).unwrap();
    assert_eq!(full.claims().len(), MAX_SETTINGS);
    let mut over = state(&engine, &full);
    let e = open(&mut over, session_params("agent:over", "o", "writer", None)).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_LIMIT",
            "saved session settings exceed 1024 sessions",
            "reuse an actor and run, or try again later: idle settings expire after one hour"
        )
    );
    assert_eq!(over.bound, None);
    assert!(
        engine
            .read()
            .reader()
            .inbox("agent:over", Some("o"))
            .unwrap()
            .is_none(),
        "the refused session wrote no subscription"
    );
    let dedupe = Asked::Claim(Claim {
        actor: "agent:over".into(),
        run: Some("o".into()),
        role: None,
        dedupe: Some(true),
    });
    assert_eq!(seen(&mut over, Some(&dedupe)).unwrap_err().code, "E_LIMIT");
    assert_eq!(over.bound, None);
    // an existing actor and run may save again; idle entries make room
    open(&mut last, session_params("agent:last", "l", "reader", None)).unwrap();
    at(5 * SETTINGS_IDLE.as_secs());
    open(&mut over, session_params("agent:over", "o", "writer", None)).unwrap();
    assert_eq!(full.claims().len(), 1);
}

#[test]
fn ui5_dedupe_is_an_accepted_setting_not_a_read_transform() {
    let key = ("agent:d".to_string(), Some("d1".to_string()));
    for (v, want) in [(json!(true), Some(true)), (json!(false), Some(false))] {
        let meta = json!({"rdy/actor": "agent:d", "rdy/run": "d1", "rdy/dedupe": v});
        let asked = claims::parse(meta.as_object()).unwrap().unwrap();
        assert_eq!(asked.dedupe(), want);
    }
    let asked = claims::parse(claim("agent:d", Some("d1")).as_object())
        .unwrap()
        .unwrap();
    assert_eq!(asked.dedupe(), None);
    // in memory: true and false are saved, an omitted setting keeps the saved one
    let (dir, r) = project();
    let engine = Arc::new(Local::new(r));
    let hub = NotifyHub::new();
    let mut st = state(&engine, &hub);
    let with = |dedupe: Option<bool>| {
        Asked::Claim(Claim {
            actor: "agent:d".into(),
            run: Some("d1".into()),
            role: None,
            dedupe,
        })
    };
    seen(&mut st, Some(&with(Some(true)))).unwrap();
    assert!(hub.claims().get(&key).unwrap().dedupe);
    seen(&mut st, Some(&with(None))).unwrap();
    assert!(hub.claims().get(&key).unwrap().dedupe, "omitted keeps it");
    open(&mut st, session_params("agent:d", "d1", "writer", None)).unwrap();
    assert!(
        hub.claims().get(&key).unwrap().dedupe,
        "rdy_session keeps it"
    );
    seen(&mut st, Some(&with(Some(false)))).unwrap();
    assert!(!hub.claims().get(&key).unwrap().dedupe);
    let fresh = NotifyHub::new();
    let mut st2 = state(&engine, &fresh);
    open(&mut st2, session_params("agent:d", "d1", "writer", None)).unwrap();
    assert!(
        !fresh.claims().get(&key).unwrap().dedupe,
        "the default is false"
    );
    drop((st, st2, engine, dir));

    // over the wire, a read with dedupe returns the full content every time
    let (_d, mut c) = stdio();
    let w = call_meta(
        &mut c,
        "rdy_write",
        folder("dd"),
        claim("agent:d", Some("d1")),
    )
    .ok();
    let id = w["created"]["$f"].clone();
    assert!(id.is_string(), "{w}");
    let plain = c.call("rdy_read", json!({"id": id})).ok();
    let mut on = claim("agent:d", Some("d1"));
    on["rdy/dedupe"] = json!(true);
    for _ in 0..2 {
        let r = call_meta(&mut c, "rdy_read", json!({"id": id}), on.clone()).ok();
        assert_eq!(r, plain, "dedupe does not change a read");
    }
    let mut off = on.clone();
    off["rdy/dedupe"] = json!(false);
    assert_eq!(
        call_meta(&mut c, "rdy_read", json!({"id": id}), off).ok(),
        plain
    );
}
