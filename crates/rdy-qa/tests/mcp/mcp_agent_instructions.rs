//! ain4 (D-8X04F, D-YQPB6, D-A8N0E, D-4PHYQ): `rdy_instructions` over MCP. One call gives the
//! required instructions of the project and the person layer in full, and the menu; `load`
//! adds the chosen rulesets. Overview and every briefing route carry the reminder until a
//! complete reply goes out for the session's actor and run; the read bit lives with the claim's
//! saved settings, in memory only. Every person here lives in an isolated home.
use rdy_daemon::instructions::PersonReader;
use rdy_engine::engine::{Engine, Local};
use rdy_engine::instructions::create_hint;
use rdy_mcp::claims::{self, Registry, SETTINGS_IDLE};
use rdy_mcp::notify::NotifyHub;
use rdy_mcp::tools::{self, McpState};
use rdy_qa::daemon::short_tempdir;
use rdy_qa::fixtures::{land, write};
use rdy_qa::git_repo::GitRepo;
use rdy_qa::instructions::{Authored, REMINDER, Set, World, author};
use rdy_qa::mcp::{DEADLINE, McpClient, Reply};
use serde_json::{Value, json};
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const NOT_FOUND_FIX: &str = "call rdy_instructions and load an id from its menu";
const BAD_LOAD: &str = "load must be a list of ruleset ids";
const BAD_LOAD_FIX: &str = "call rdy_instructions and pass ids from its menu";

const PROJECT_SETS: &[Set] = &[
    Set {
        name: "baseline",
        title: "Baseline",
        required: true,
        lead: Some("Read these before any task."),
        rules: &[
            (
                &["act:write"],
                "Write small changes.\n\nKeep one idea per change.",
            ),
            (&["area:nowhere"], "Cite a decision in every claim."),
        ],
    },
    Set {
        name: "testing",
        title: "Testing",
        required: false,
        lead: Some("Load this for test work."),
        rules: &[(&["type:claim"], "Run the tests first.")],
    },
];

const META_SETS: &[Set] = &[
    Set {
        name: "personal",
        title: "Personal",
        required: true,
        lead: Some("My rules in every project."),
        rules: &[(&["all"], "Answer in British English.")],
    },
    Set {
        name: "reviews",
        title: "Reviews",
        required: false,
        lead: None,
        rules: &[(&["act:review"], "Read the whole diff.")],
    },
];

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
fn call_meta(c: &mut McpClient, tool: &str, args: Value, meta: Value) -> Reply {
    let id = c.next_id();
    let line = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": tool, "arguments": args, "_meta": meta}});
    c.send_raw(&line.to_string());
    decode(&c.recv(id, DEADLINE))
}

fn claim(actor: &str, run: &str) -> Value {
    json!({"rdy/actor": actor, "rdy/run": run})
}

fn refusal(r: Reply) -> (String, String, String) {
    let e = match r {
        Reply::ToolError(v) => v,
        other => panic!("expected an rdy tool error, got {other:?}"),
    };
    let s = |k: &str| e[k].as_str().unwrap_or_default().to_string();
    (s("code"), s("message"), s("fix"))
}

fn ids(v: &Value, key: &str, field: &str) -> Vec<String> {
    v[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} in {v}"))
        .iter()
        .map(|r| r[field].as_str().unwrap().to_string())
        .collect()
}

/// The reply with its built-in rows and menu entries (layer `rdy`) taken out.
fn without_rdy(v: &Value) -> Value {
    let mut v = v.clone();
    for key in ["instructions", "menu"] {
        if let Some(a) = v[key].as_array_mut() {
            a.retain(|r| r["layer"] != "rdy");
        }
    }
    v
}

/// The built-in rows or menu entries of a reply, as delivered.
fn rdy_part(v: &Value, key: &str) -> Vec<Value> {
    v[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} in {v}"))
        .iter()
        .filter(|r| r["layer"] == "rdy")
        .cloned()
        .collect()
}

fn rdy_ids(v: &Value, key: &str) -> Vec<String> {
    rdy_part(v, key)
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect()
}

const BOOTSTRAP_ROWS: [&str; 3] = [
    "rdy:bootstrap:before",
    "rdy:bootstrap:approval",
    "rdy:bootstrap:after",
];

fn p(id: &str) -> String {
    format!("project:{id}")
}

fn q(id: &str) -> String {
    format!("person:{id}")
}

#[test]
fn ain4_one_tool_delivers_required_and_selected_rules_over_mcp() {
    let w = World::new();
    let (pd, md) = (w.project(), w.project());
    let pa = author(pd.path(), "Small changes keep review cheap", PROJECT_SETS);
    let ma = author(md.path(), "One spelling in every reply", META_SETS);
    w.person(md.path());
    let mut c = w.mcp(pd.path());
    // the one new tool, listed once, with one closed input
    let tools = c.tools();
    let listed: Vec<&Value> = tools
        .iter()
        .filter(|t| t["name"] == "rdy_instructions")
        .collect();
    assert_eq!(listed.len(), 1, "{tools:?}");
    assert_eq!(listed[0]["inputSchema"]["additionalProperties"], false);
    let keys: Vec<&String> = listed[0]["inputSchema"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(keys, ["full", "load"]);
    assert_eq!(
        listed[0]["inputSchema"]["properties"]["full"]["type"],
        "boolean"
    );
    // the first claim, by _meta, gets the required rules of both layers and the whole menu
    let v = call_meta(
        &mut c,
        "rdy_instructions",
        json!({}),
        claim("agent:a", "a1"),
    )
    .ok();
    // the required built-in bootstrap and the whole built-in menu come first; the stored
    // layers follow unchanged
    let built = v.clone();
    assert_eq!(rdy_ids(&built, "instructions"), BOOTSTRAP_ROWS);
    assert_eq!(rdy_part(&built, "menu").len(), 12, "{built}");
    let v = without_rdy(&v);
    assert_eq!(
        ids(&v, "instructions", "id"),
        [
            pa.rules[0][0].clone(),
            pa.rules[0][1].clone(),
            ma.rules[0][0].clone()
        ]
    );
    assert_eq!(
        ids(&v, "instructions", "layer"),
        ["project", "project", "person"]
    );
    assert_eq!(
        ids(&v, "instructions", "ruleset"),
        [p(&pa.docs[0]), p(&pa.docs[0]), q(&ma.docs[0])]
    );
    let first = &v["instructions"][0];
    assert_eq!(
        first["text"],
        "Write small changes.\n\nKeep one idea per change."
    );
    assert_eq!(first["locked"], false);
    assert_eq!(
        first["reason"],
        json!([{"id": pa.decision, "statement": pa.statement}])
    );
    assert_eq!(
        v["instructions"][2]["reason"],
        json!([{"id": ma.decision, "statement": ma.statement}])
    );
    assert_eq!(
        ids(&v, "menu", "id"),
        [
            p(&pa.docs[0]),
            p(&pa.docs[1]),
            q(&ma.docs[0]),
            q(&ma.docs[1])
        ]
    );
    for m in v["menu"].as_array().unwrap() {
        let mut k: Vec<&String> = m.as_object().unwrap().keys().collect();
        k.sort();
        assert_eq!(
            k,
            ["hints", "id", "layer", "name", "required", "tokens", "when"]
        );
    }
    let body = "Run the tests first.";
    assert_eq!(
        v["menu"][1],
        json!({"id": p(&pa.docs[1]), "name": "Testing", "when": "Load this for test work.",
            "layer": "project", "required": false, "tokens": body.len().div_ceil(4),
            "hints": ["type:claim"]})
    );
    assert_eq!(v["menu"][3]["when"], "");
    assert!(v.get("warnings").is_none(), "{v}");
    assert!(built.get("hint").is_none(), "{built}");
    // load: a qualified handle and a bare id that one layer holds; the required rules stay
    let v = c
        .call(
            "rdy_instructions",
            json!({"load": [p(&pa.docs[1]), ma.docs[1].clone()]}),
        )
        .ok();
    let v = without_rdy(&v);
    assert_eq!(
        ids(&v, "instructions", "id"),
        [
            pa.rules[0][0].clone(),
            pa.rules[0][1].clone(),
            pa.rules[1][0].clone(),
            ma.rules[0][0].clone(),
            ma.rules[1][0].clone()
        ]
    );
    assert_eq!(v["instructions"][4]["text"], "Read the whole diff.");
    // an id from the other layer is not found; a malformed one is a bad load
    let wrong = q(&pa.docs[1]);
    assert_eq!(
        refusal(c.call("rdy_instructions", json!({"load": [wrong.clone()]}))),
        (
            "E_NOT_FOUND".to_string(),
            format!("ruleset {wrong} was not found"),
            NOT_FOUND_FIX.to_string()
        )
    );
    assert_eq!(
        refusal(c.call("rdy_instructions", json!({"load": ["team:x"]}))),
        (
            "E_BAD_INPUT".to_string(),
            BAD_LOAD.to_string(),
            BAD_LOAD_FIX.to_string()
        )
    );
    c.alive();
}

/// A local engine for `dir`, whose person layer is read in `w`'s isolated home.
fn local(w: &World, dir: &Path) -> Arc<Local> {
    let (platform, env) = w.path_env();
    let host = PersonReader::direct(env!("CARGO_PKG_VERSION")).with_env(platform, env);
    Arc::new(
        Local::open(dir)
            .expect("open the project")
            .with_person_host(Arc::new(host)),
    )
}

fn state(engine: &Arc<Local>, hub: &Arc<NotifyHub>) -> McpState {
    let mut st = McpState::new(Arc::clone(engine) as Arc<dyn Engine>);
    st.hub = Arc::clone(hub);
    st
}

/// `rdy_session` through the protocol's door.
fn open(st: &mut McpState, actor: &str, run: &str, role: &str) {
    let p = serde_json::from_value(json!({"actor": actor, "run": run, "role": role})).unwrap();
    claims::call(st, None, "rdy_session", |st| tools::rdy_session(st, p)).expect("rdy_session");
}

fn overview(st: &mut McpState, args: Value) -> Value {
    let p = serde_json::from_value(args).unwrap();
    claims::call(st, None, "rdy_overview", |st| tools::rdy_overview(st, p)).expect("rdy_overview")
}

fn brief(st: &mut McpState, args: Value) -> Value {
    let p = serde_json::from_value(args).unwrap();
    claims::call(st, None, "rdy_brief", |st| tools::rdy_brief(st, p)).expect("rdy_brief")
}

fn instructions(st: &mut McpState, args: Value) -> Result<Value, rdy_engine::RdyError> {
    let p = serde_json::from_value(args).unwrap();
    claims::call(st, None, "rdy_instructions", |st| {
        tools::rdy_instructions(st, p)
    })
}

/// Lands a doc whose one claim is cited by 60 others, so its briefing is cut; returns the
/// cited claim's id.
fn cited(dir: &Path) -> String {
    let mut r = rdy_engine::Rdy::open(dir).unwrap();
    let wr = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "hub", "title": "Hub", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$h", "parent": "$s", "body": "The needle holds."}
    ]))
    .unwrap();
    let (s, h) = (wr.created["$s"].clone(), wr.created["$h"].clone());
    let ops: Vec<Value> = (0..60)
        .map(|i| json!({"op": "create", "type": "claim", "parent": s, "body": format!("Claim {i} cites {{{{{h}}}}}.")}))
        .collect();
    write(&mut r, "work", Value::Array(ops)).unwrap();
    land(&mut r, "work").unwrap();
    r.close().unwrap();
    h
}

#[test]
fn ain4_reminder_is_per_actor_and_run_and_clears_after_success() {
    let w = World::new();
    let pd = w.project();
    let pa: Authored = author(pd.path(), "Small changes keep review cheap", PROJECT_SETS);
    let h = cited(pd.path());
    let repo = GitRepo::new();
    repo.write("src/a.rs", "fn a() {}\n");
    repo.commit("add a");
    repo.link(pd.path());
    {
        let engine = local(&w, pd.path());
        let base = Instant::now();
        let now = Arc::new(Mutex::new(base));
        let clock = {
            let now = Arc::clone(&now);
            Arc::new(move || *now.lock().unwrap())
        };
        let at = |secs: u64| *now.lock().unwrap() = base + Duration::from_secs(secs);
        let hub = NotifyHub::with_claims(Registry::with_clock(clock));
        let (mut a1, mut a2, mut b1) = (
            state(&engine, &hub),
            state(&engine, &hub),
            state(&engine, &hub),
        );
        open(&mut a1, "agent:a", "r1", "writer");
        open(&mut a2, "agent:a", "r2", "writer");
        open(&mut b1, "agent:b", "r1", "reader");
        for st in [&mut a1, &mut a2, &mut b1] {
            assert_eq!(overview(st, json!({}))["reminder"], REMINDER);
        }
        // a refused call leaves it owed
        let e = instructions(&mut a1, json!({"load": ["project:d-ZZZZZ"]})).unwrap_err();
        assert_eq!(e.code, "E_NOT_FOUND");
        assert_eq!(overview(&mut a1, json!({}))["reminder"], REMINDER);
        // a complete reply clears it for that actor and run only; an empty person layer counts
        let v = instructions(&mut a1, json!({})).unwrap();
        assert_eq!(
            v["hint"].as_str(),
            Some(create_hint(&w.default_meta()).as_str())
        );
        assert!(overview(&mut a1, json!({})).get("reminder").is_none());
        assert!(
            brief(&mut a1, json!({"ids": [pa.docs[0]]}))
                .get("reminder")
                .is_none()
        );
        assert_eq!(overview(&mut a2, json!({}))["reminder"], REMINDER);
        assert_eq!(overview(&mut b1, json!({}))["reminder"], REMINDER);
        // every briefing route carries it while it is owed: blocks, a path, a code file, a page
        assert_eq!(
            brief(&mut b1, json!({"ids": [pa.docs[0]]}))["reminder"],
            REMINDER
        );
        assert_eq!(
            brief(&mut b1, json!({"path": "baseline"}))["reminder"],
            REMINDER
        );
        let code = brief(&mut b1, json!({"code_path": "src/a.rs"}));
        assert_eq!(code["code"]["path"], "src/a.rs", "{code}");
        assert_eq!(code["reminder"], REMINDER);
        let cut = brief(
            &mut b1,
            json!({"ids": [h], "scope": "canon", "max_tokens": 400}),
        );
        assert_eq!(cut["reminder"], REMINDER);
        let next = cut["next"].clone();
        assert!(next.is_string(), "{cut}");
        let page = brief(
            &mut b1,
            json!({"page": next, "scope": "canon", "max_tokens": 400}),
        );
        assert!(page["dependents"].is_array(), "{page}");
        assert_eq!(page["reminder"], REMINDER);
        // a repeated rdy_session keeps the bit; later calls still get the required rules
        open(&mut a1, "agent:a", "r1", "writer");
        assert!(overview(&mut a1, json!({})).get("reminder").is_none());
        let again = instructions(&mut a1, json!({})).unwrap();
        assert_eq!(again["instructions"].as_array().unwrap().len(), 5);
        assert_eq!(rdy_ids(&again, "instructions"), BOOTSTRAP_ROWS);
        assert_eq!(
            ids(&without_rdy(&again), "instructions", "id"),
            [pa.rules[0][0].clone(), pa.rules[0][1].clone()]
        );
        assert_eq!(
            ids(&without_rdy(&again), "instructions", "layer"),
            ["project", "project"]
        );
        // the bit lives with the saved settings: an idle hour drops both
        at(SETTINGS_IDLE.as_secs() - 1);
        assert!(overview(&mut a1, json!({})).get("reminder").is_none());
        at(2 * SETTINGS_IDLE.as_secs() - 1);
        assert_eq!(overview(&mut a1, json!({}))["reminder"], REMINDER);
        // a restart (a new registry) starts with it owed
        let fresh = NotifyHub::new();
        let mut a3 = state(&engine, &fresh);
        open(&mut a3, "agent:b", "r1", "writer");
        assert_eq!(overview(&mut a3, json!({}))["reminder"], REMINDER);
    }
    // over the wire: a claim by rdy_session, a reconnect while the settings live, a restart
    let mut c = w.mcp(pd.path());
    let s = c.call(
        "rdy_session",
        json!({"actor": "agent:x", "run": "x1", "role": "writer"}),
    );
    assert!(s.is_ok(), "{s:?}");
    assert_eq!(c.call("rdy_overview", json!({})).ok()["reminder"], REMINDER);
    assert!(c.call("rdy_instructions", json!({})).is_ok());
    assert!(
        c.call("rdy_overview", json!({}))
            .ok()
            .get("reminder")
            .is_none()
    );
    drop(c);
    let mut c = w.mcp(pd.path());
    let ov = call_meta(&mut c, "rdy_overview", json!({}), claim("agent:x", "x1")).ok();
    assert!(ov.get("reminder").is_none(), "the daemon kept it: {ov}");
    let mut y = w.mcp(pd.path());
    let ov = call_meta(&mut y, "rdy_overview", json!({}), claim("agent:y", "y1")).ok();
    assert_eq!(ov["reminder"], REMINDER);
    drop((c, y));
    w.stop(pd.path());
    let mut c = w.mcp(pd.path());
    let ov = call_meta(&mut c, "rdy_overview", json!({}), claim("agent:x", "x1")).ok();
    assert_eq!(ov["reminder"], REMINDER, "a restart resets it");
}

#[test]
fn ain4_brief_only_names_the_instruction_tool() {
    let w = World::new();
    let pd = w.project();
    let pa = author(pd.path(), "Small changes keep review cheap", PROJECT_SETS);
    let engine = local(&w, pd.path());
    let hub = NotifyHub::new();
    let mut st = state(&engine, &hub);
    open(&mut st, "agent:a", "r1", "writer");
    let before = brief(&mut st, json!({"ids": [pa.docs[0]]}));
    assert_eq!(before["reminder"], REMINDER);
    for k in ["instructions", "menu", "acts"] {
        assert!(before.get(k).is_none(), "{k} in {before}");
    }
    // brief takes no acts input
    let e = rdy_mcp::params::parse_params::<tools::BriefParams>(
        "rdy_brief",
        json!({"ids": [pa.docs[0]], "acts": ["write"]})
            .as_object()
            .unwrap()
            .clone(),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert!(e.message.contains("unknown parameter \"acts\""), "{e:?}");
    // the reminder is outside the briefing: once read, the same briefing has no reminder and
    // the same token
    instructions(&mut st, json!({})).unwrap();
    let after = brief(&mut st, json!({"ids": [pa.docs[0]]}));
    let mut stripped = before.clone();
    stripped.as_object_mut().unwrap().remove("reminder");
    assert_eq!(after, stripped);
    // the overview reserves its space: a small budget still carries it
    let mut other = state(&engine, &hub);
    open(&mut other, "agent:b", "r1", "writer");
    let small = overview(&mut other, json!({"max_tokens": 400}));
    assert_eq!(small["reminder"], REMINDER);
    assert!(
        small.to_string().len().div_ceil(4) <= 400,
        "{} tokens",
        small.to_string().len().div_ceil(4)
    );
}

#[test]
fn ain4_tool_inputs_are_closed_and_reader_calls_write_no_content() {
    let w = World::new();
    let (pd, md) = (w.project(), w.project());
    author(pd.path(), "Small changes keep review cheap", PROJECT_SETS);
    author(md.path(), "One spelling in every reply", META_SETS);
    w.person(md.path());
    let mut c = w.mcp(pd.path());
    let s = c.call(
        "rdy_session",
        json!({"actor": "agent:r", "run": "r1", "role": "reader", "view": "canon"}),
    );
    assert!(s.is_ok(), "{s:?}");
    let (code, message, fix) = refusal(c.call("rdy_instructions", json!({"lod": []})));
    assert_eq!(code, "E_BAD_INPUT");
    assert_eq!(message, "rdy_instructions: unknown parameter \"lod\"");
    assert!(fix.starts_with("did you mean \"load\"?"), "{fix}");
    for bad in [
        json!({"full": null}),
        json!({"full": "true"}),
        json!({"full": 1}),
        json!({"full": [true]}),
    ] {
        let (code, message, _) = refusal(c.call("rdy_instructions", bad));
        assert_eq!(code, "E_BAD_INPUT");
        assert!(
            message.starts_with("rdy_instructions: parameter \"full"),
            "{message}"
        );
    }
    for bad in [json!({"load": "x"}), json!({"load": [1]})] {
        let (code, message, _) = refusal(c.call("rdy_instructions", bad));
        assert_eq!(code, "E_BAD_INPUT");
        assert!(
            message.starts_with("rdy_instructions: parameter \"load"),
            "{message}"
        );
    }
    for bad in ["project:", "d-AAAAA:x", ""] {
        assert_eq!(
            refusal(c.call("rdy_instructions", json!({"load": [bad]}))).1,
            BAD_LOAD
        );
    }
    let many: Vec<String> = (0..1025).map(|i| format!("d-{i:05}")).collect();
    let (code, message, _) = refusal(c.call("rdy_instructions", json!({"load": many})));
    assert_eq!(
        (code.as_str(), message.as_str()),
        ("E_LIMIT", "instructions exceed 1024 load ids (found 1025)")
    );
    // every refusal leaves the reminder owed
    assert_eq!(c.call("rdy_overview", json!({})).ok()["reminder"], REMINDER);
    // a reader's complete call writes nothing to either project
    let mut m = w.mcp(md.path());
    assert!(
        m.call(
            "rdy_session",
            json!({"actor": "agent:m", "run": "m1", "role": "reader", "view": "canon"})
        )
        .is_ok()
    );
    let seen = |c: &mut McpClient| {
        let o = c.call("rdy_overview", json!({"scope": "canon"})).ok();
        (
            o["views"].clone(),
            o["canon"].clone(),
            o["version_position"].clone(),
        )
    };
    let (p0, m0) = (seen(&mut c), seen(&mut m));
    let v = c.call("rdy_instructions", json!({})).ok();
    assert_eq!(v["instructions"].as_array().unwrap().len(), 6, "{v}");
    assert_eq!(without_rdy(&v)["instructions"].as_array().unwrap().len(), 3);
    assert_eq!((seen(&mut c), seen(&mut m)), (p0, m0));
    c.alive();
}

/// A projectless `rdy mcp` in an empty folder of `w`, its reader session claimed.
fn projectless(w: &World, cwd: &Path) -> McpClient {
    let mut cmd = rdy_qa::fixtures::rdy_command();
    cmd.args(["mcp", "--no-daemon"])
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(w.env())
        .current_dir(cwd)
        .stderr(Stdio::piped());
    let mut c = McpClient::launch(cmd);
    let r = c.call(
        "rdy_session",
        json!({"actor": "agent:p", "run": "p1", "role": "reader"}),
    );
    assert!(r.is_ok(), "{r:?}");
    c
}

#[test]
fn agi3_both_servers_share_full_and_closed_inputs() {
    let w = World::new();
    let pd = w.project();
    let pa = author(pd.path(), "Small changes keep review cheap", PROJECT_SETS);
    let outside = short_tempdir();
    let mut served = w.mcp(pd.path());
    let r = served.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a1", "role": "reader"}),
    );
    assert!(r.is_ok(), "{r:?}");
    let mut bare = projectless(&w, outside.path());
    // both schemas name the same two closed inputs
    for c in [&mut served, &mut bare] {
        let tools = c.tools();
        let t = tools
            .iter()
            .find(|t| t["name"] == "rdy_instructions")
            .unwrap();
        assert_eq!(t["inputSchema"]["additionalProperties"], false);
        let mut keys: Vec<&String> = t["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        keys.sort();
        assert_eq!(keys, ["full", "load"]);
    }
    // false and absent read the same: the bootstrap whole, the menu, no optional body
    let (a, b) = (
        served.call("rdy_instructions", json!({})).ok(),
        bare.call("rdy_instructions", json!({})).ok(),
    );
    let a_false = served.call("rdy_instructions", json!({"full": false})).ok();
    assert_eq!(a, a_false);
    assert_eq!(rdy_part(&a, "instructions"), rdy_part(&b, "instructions"));
    assert_eq!(rdy_part(&a, "menu"), rdy_part(&b, "menu"));
    assert_eq!(rdy_ids(&a, "instructions"), BOOTSTRAP_ROWS);
    // true gives every built-in body once, the same on both servers; stored required rules stay
    let (a, b) = (
        served.call("rdy_instructions", json!({"full": true})).ok(),
        bare.call("rdy_instructions", json!({"full": true})).ok(),
    );
    let built = rdy_ids(&a, "instructions");
    assert_eq!(built.len(), 14, "{built:?}");
    assert_eq!(rdy_part(&a, "instructions"), rdy_part(&b, "instructions"));
    assert_eq!(rdy_part(&a, "menu"), rdy_part(&b, "menu"));
    let mut once = built.clone();
    once.sort();
    once.dedup();
    assert_eq!(once.len(), 14);
    assert_eq!(
        ids(&without_rdy(&a), "instructions", "id"),
        [pa.rules[0][0].clone(), pa.rules[0][1].clone()]
    );
    // full plus load: the stored optional ruleset arrives as well, built-ins are not repeated
    let both = served
        .call(
            "rdy_instructions",
            json!({"full": true, "load": [p(&pa.docs[1]), "rdy:write"]}),
        )
        .ok();
    assert_eq!(rdy_ids(&both, "instructions"), built);
    assert_eq!(
        ids(&without_rdy(&both), "instructions", "id"),
        [
            pa.rules[0][0].clone(),
            pa.rules[0][1].clone(),
            pa.rules[1][0].clone()
        ]
    );
    // every handle is still checked under full
    let (code, message, _) = refusal(served.call(
        "rdy_instructions",
        json!({"full": true, "load": ["rdy:nowhere"]}),
    ));
    assert_eq!(
        (code.as_str(), message.as_str()),
        ("E_NOT_FOUND", "ruleset rdy:nowhere was not found")
    );
    // closed inputs: same refusals on both servers
    for c in [&mut served, &mut bare] {
        for bad in [
            json!({"full": null}),
            json!({"full": "true"}),
            json!({"full": 0}),
            json!({"fulll": true}),
            json!({"full": true, "extra": 1}),
        ] {
            let (code, message, _) = refusal(c.call("rdy_instructions", bad));
            assert_eq!(code, "E_BAD_INPUT");
            assert!(message.starts_with("rdy_instructions:"), "{message}");
        }
    }
    let (_, message, fix) = refusal(bare.call("rdy_instructions", json!({"ful": true})));
    assert_eq!(message, "rdy_instructions: unknown parameter \"ful\"");
    assert!(fix.starts_with("did you mean \"full\"?"), "{fix}");
    served.alive();
    bare.alive();
}

#[test]
fn agi3_refused_instructions_keep_the_reminder_due() {
    let w = World::new();
    let pd = w.project();
    author(pd.path(), "Small changes keep review cheap", PROJECT_SETS);
    let engine = local(&w, pd.path());
    let hub = NotifyHub::new();
    let (mut a1, mut a2) = (state(&engine, &hub), state(&engine, &hub));
    open(&mut a1, "agent:a", "r1", "reader");
    open(&mut a2, "agent:a", "r2", "reader");
    assert_eq!(overview(&mut a1, json!({}))["reminder"], REMINDER);
    // a bad handle, a handle of the wrong kind, and a failed limit check: all owed
    let many: Vec<String> = (0..1025).map(|i| format!("d-{i:05}")).collect();
    for args in [
        json!({"full": true, "load": ["rdy:nowhere"]}),
        json!({"full": true, "load": ["project:d-ZZZZZ"]}),
        json!({"full": true, "load": many}),
        json!({"load": ["team:x"]}),
    ] {
        assert!(instructions(&mut a1, args).is_err());
        assert_eq!(overview(&mut a1, json!({}))["reminder"], REMINDER);
    }
    // a complete full reply clears it for that actor and run only
    let v = instructions(&mut a1, json!({"full": true})).unwrap();
    assert_eq!(rdy_ids(&v, "instructions").len(), 14);
    assert!(overview(&mut a1, json!({})).get("reminder").is_none());
    assert_eq!(overview(&mut a2, json!({}))["reminder"], REMINDER);
    // a later refusal does not bring it back
    assert!(instructions(&mut a1, json!({"load": ["rdy:nowhere"]})).is_err());
    assert!(overview(&mut a1, json!({})).get("reminder").is_none());
}
