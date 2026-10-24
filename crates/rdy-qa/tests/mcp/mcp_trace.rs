//! rf12 (D-1876T, D-KQ4CA): `rdy_trace` is a registered read of the chat path. It refuses what
//! the other reads refuse, keeps `rdy_context` at 20 history rows and a briefing one level deep,
//! and ends the chat path of the design-rebase features: a source, a finding, a decision that
//! cites it, review citations, reflects, consequences and a briefing, then the trace.
use rdy_daemon::{OPS, OpContext, dispatch};
use rdy_engine::engine::{Engine, Local};
use rdy_engine::signing_crypto::UnavailableAgent;
use rdy_engine::{Rdy, Session};
use rdy_mcp::tools::{McpState, rdy_session, rdy_trace};
use rdy_model::Role;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::git_repo::GitRepo;
use rdy_qa::mcp::{McpClient, Reply};
use rdy_qa::tensions::{put, upd};
use serde_json::{Value, from_value, json};
use std::sync::Arc;

/// The tool's description, word for word (TASK-507 (4)).
const TRACE_DESCRIPTION: &str = "Why a block says what it says: governing decisions, sources, lineage, one-step premises, questions, introduction and acceptance landings, and sealed plans. depth is 1 to 8, default 3. detail adds recorded evidence. max_tokens defaults to 1000. Continue at an id in frontier.";

fn tool_error(r: Reply) -> Value {
    match r {
        Reply::ToolError(v) => v,
        other => panic!("expected a tool error, got {other:?}"),
    }
}

fn session(c: &mut McpClient, role: &str) {
    let r = c.call(
        "rdy_session",
        json!({"actor": "agent:qa", "run": "qa", "role": role}),
    );
    assert!(r.code().is_none(), "{:?}", r.ok());
}

/// (code, message, fix) of an error reply.
fn triple(e: &Value) -> (String, String, String) {
    (
        e["code"].as_str().unwrap().to_string(),
        e["message"].as_str().unwrap().to_string(),
        e["fix"].as_str().unwrap().to_string(),
    )
}

/// Canon: doc `refunds` with section `Rules`, a decision D and a claim K decided by it; tag
/// v1.0.0 names that landing.
fn seed_small(r: &mut Rdy) -> (String, String) {
    let w = write(r, "w", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "refunds", "title": "Refunds", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
        {"op": "create", "type": "decision", "id": "$dd", "parent": "$s",
            "fields": {"statement": "Refunds need an approver", "status": "proposed"}},
        {"op": "create", "type": "claim", "id": "$k", "parent": "$s", "body": "A refund of 200 needs an approver.",
            "links": {"decided_by": [{"target": "$dd"}]}}
    ]))
    .unwrap();
    land(r, "w").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    (w.created["$k"].clone(), w.created["$dd"].clone())
}

/// `seed_small` in the project a served daemon will open.
fn small(s: &Served) -> (String, String) {
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let ids = seed_small(&mut r);
    r.close().unwrap();
    ids
}

/// The MCP arguments, the CLI arguments and the refusal both give: its code, its message (empty
/// when only the code is pinned) and its fix.
type Case = (Value, Vec<String>, (&'static str, String, &'static str));

fn cli_refusal(s: &Served, args: &[&str]) -> Value {
    let mut full = vec!["--json", "trace"];
    full.extend_from_slice(args);
    let (ok, v) = s.json(&full);
    assert!(!ok, "rdy {args:?} should refuse: {v}");
    v
}

#[test]
fn rf12_trace_refusals_cross_both_boundaries() {
    let s = Served::new();
    let (k, d) = small(&s);
    let mut c = McpClient::start_proxy(&s);
    // no session: refused before anything is read
    let e = tool_error(c.call("rdy_trace", json!({"id": k})));
    assert_eq!(e["code"], "E_NO_SESSION", "{e}");
    let valid = |c: &mut McpClient, s: &Served| {
        let v = c.call("rdy_trace", json!({"id": k, "scope": "canon"})).ok();
        assert_eq!(v["id"], k.as_str(), "{v}");
        let (ok, j) = s.json(&["--json", "trace", &k, "--scope", "canon"]);
        assert!(ok && j == v, "{j}\n{v}");
    };
    session(&mut c, "reader");
    valid(&mut c, &s);

    let missing = "c-0A1B2";
    let ref_clash = format!("{k}@v1.0.0");
    // (MCP arguments, CLI arguments, what the refusal is)
    let cases: Vec<Case> = vec![
        (
            json!({"id": missing, "scope": "canon"}),
            vec![missing.into(), "--scope".into(), "canon".into()],
            (
                "E_UNKNOWN_BLOCK",
                format!("unknown block {missing}"),
                "check the id with rdy_search",
            ),
        ),
        (
            json!({"id": "nowhere/doc", "scope": "canon"}),
            vec!["nowhere/doc".into(), "--scope".into(), "canon".into()],
            (
                "E_UNKNOWN_BLOCK",
                "no folder or doc at \"nowhere/doc\"".into(),
                "check the id with rdy_search",
            ),
        ),
        (
            json!({"id": k, "scope": "nope"}),
            vec![k.clone(), "--scope".into(), "nope".into()],
            ("E_NOT_FOUND", String::new(), ""),
        ),
        (
            json!({"id": ref_clash, "scope": "canon"}),
            vec![ref_clash.clone(), "--scope".into(), "canon".into()],
            (
                "E_BAD_INPUT",
                format!("{k} pins version v1.0.0; drop scope or make it tag:v1.0.0"),
                "use the pinned version, or read the bare id with a scope",
            ),
        ),
        (
            json!({"id": "[c-1 broken", "scope": "canon"}),
            vec!["[c-1 broken".into(), "--scope".into(), "canon".into()],
            ("E_BAD_INPUT", String::new(), ""),
        ),
    ];
    for (mcp, cli, (code, message, fix)) in cases {
        let m = tool_error(c.call("rdy_trace", mcp.clone()));
        let cli: Vec<&str> = cli.iter().map(String::as_str).collect();
        let j = cli_refusal(&s, &cli);
        assert_eq!(triple(&m), triple(&j), "{mcp}: MCP and CLI agree");
        assert_eq!(m["code"], code, "{m}");
        if !message.is_empty() {
            assert_eq!(m["message"], message.as_str(), "{m}");
            assert_eq!(m["fix"], fix, "{m}");
        }
        valid(&mut c, &s);
    }
    // depth: the same closed range for -1, 0 and 9
    for depth in [-1i64, 0, 9] {
        let m = tool_error(c.call(
            "rdy_trace",
            json!({"id": k, "scope": "canon", "depth": depth}),
        ));
        let d_arg = depth.to_string();
        let j = cli_refusal(&s, &[&k, "--scope", "canon", "--depth", &d_arg]);
        assert_eq!(
            triple(&m),
            (
                "E_BAD_INPUT".to_string(),
                format!("depth {depth} is outside 1 to 8"),
                "pass a depth from 1 to 8".to_string()
            ),
            "{m}"
        );
        assert_eq!(triple(&m), triple(&j), "depth {depth}");
        valid(&mut c, &s);
    }

    // max_tokens and an unknown key: MCP and the daemon's JSON op
    let (dir2, r2) = project();
    let e2 = Local::new(r2);
    let human = Session::human("human:qa".into(), Role::Reader).unwrap();
    let cx = OpContext {
        engine: &e2,
        session: &human,
        human: "human:qa",
        cwd: dir2.path(),
        agent: &UnavailableAgent,
    };
    let m = tool_error(c.call("rdy_trace", json!({"id": k, "max_tokens": 100_001})));
    assert_eq!(
        triple(&m),
        (
            "E_LIMIT".to_string(),
            "max_tokens 100001 is over the limit of 100000".to_string(),
            "ask for at most 100000 tokens and page through the rest".to_string()
        ),
        "{m}"
    );
    let t = dispatch(&cx, "trace", json!({"id": k, "max_tokens": 100_001})).unwrap_err();
    assert_eq!(
        (t.code, t.message.as_str(), t.fix.as_str()),
        (
            "E_LIMIT",
            &*m["message"].as_str().unwrap().to_string(),
            &*m["fix"].as_str().unwrap().to_string()
        )
    );
    valid(&mut c, &s);
    let m = tool_error(c.call("rdy_trace", json!({"id": k, "detial": true})));
    assert_eq!(m["code"], "E_BAD_INPUT", "{m}");
    assert_eq!(
        m["message"], "rdy_trace: unknown parameter \"detial\"",
        "{m}"
    );
    assert!(
        m["fix"]
            .as_str()
            .unwrap()
            .starts_with("did you mean \"detail\"? the parameters here are "),
        "{m}"
    );
    let t = dispatch(&cx, "trace", json!({"id": k, "detial": true})).unwrap_err();
    assert_eq!(t.code, "E_BAD_INPUT");
    assert_eq!(t.message, "trace: unknown field \"detial\"");
    assert!(t.fix.starts_with("did you mean \"detail\"?"), "{t}");
    valid(&mut c, &s);
    let _ = d;
    c.alive();
}

/// A log end that reads must not move.
fn log_end(e: &Local) -> String {
    e.write(Box::new(|r| {
        Ok(json!(format!("{:?}", r.store().log_end())))
    }))
    .unwrap()
    .as_str()
    .unwrap()
    .to_string()
}

#[test]
fn rf12_trace_is_registered_as_an_epoch_guarded_read() {
    // the daemon op sits right after impact, and the tool is listed with its decided shape
    let impact = OPS.iter().position(|o| *o == "impact").unwrap();
    assert_eq!(OPS[impact + 1], "trace");
    let s = Served::new();
    let (k, _d) = small(&s);
    let mut c = McpClient::start_proxy(&s);
    let tools = c.tools();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    let at = |n: &str| {
        names
            .iter()
            .position(|x| *x == n)
            .unwrap_or_else(|| panic!("no tool {n}"))
    };
    let trace = &tools[at("rdy_trace")];
    assert_eq!(trace["description"], TRACE_DESCRIPTION);
    let props = trace["inputSchema"]["properties"].as_object().unwrap();
    let mut keys: Vec<&str> = props.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["depth", "detail", "id", "max_tokens", "scope"]);
    assert_eq!(trace["inputSchema"]["required"], json!(["id"]));
    for banned in [
        "page",
        "cursor",
        "next",
        "out",
        "mutate",
        "at",
        "time",
        "timestamp",
    ] {
        assert!(!props.contains_key(banned), "{banned}");
    }
    // the neighbours' descriptions gained exactly the decided words
    let desc = |n: &str| tools[at(n)]["description"].as_str().unwrap().to_string();
    assert!(desc("rdy_context").ends_with(
        " Governing decisions include live decisions that affect an entity the block describes."
    ));
    assert!(desc("rdy_brief").ends_with(" Governs includes proposed decisions and decisions that affect a described entity; through names that entity. Sources follow suspects. Findings follow related_decisions."));
    assert!(desc("rdy_impact").contains(
        "(depends_on, conditional_on, implements, decided_by, describes, affects, reflects)"
    ));
    assert!(
        desc("rdy_suspects")
            .contains("(depends_on, conditional_on, implements, decided_by, reflects)")
    );
    // a read, not a write: nothing is appended in this process's engine either
    let (dir2, r2) = project();
    let e2 = Local::new(r2);
    let human = Session::human("human:qa".into(), Role::Reader).unwrap();
    let cx = OpContext {
        engine: &e2,
        session: &human,
        human: "human:qa",
        cwd: dir2.path(),
        agent: &UnavailableAgent,
    };
    let before = log_end(&e2);
    let _ = dispatch(&cx, "trace", json!({"id": "c-0A1B2"}));
    let _ = dispatch(&cx, "trace", json!({}));
    assert_eq!(log_end(&e2), before);
    // and through the server: a reader can call it, and its two replies agree
    session(&mut c, "reader");
    let v = c.call("rdy_trace", json!({"id": k, "scope": "canon"})).ok();
    assert_eq!(v["id"], k.as_str());
    let again = c.call("rdy_trace", json!({"id": k, "scope": "canon"})).ok();
    assert_eq!(v, again);
    c.alive();

    // the invariant itself: on one served project, after the session is set up, successful MCP
    // and daemon trace reads leave the log position and every ref where they were
    let (dir3, mut r3) = project();
    let (k3, _d3) = seed_small(&mut r3);
    let served = Arc::new(Local::new(r3));
    let mut st = McpState::new(served.clone());
    rdy_session(
        &mut st,
        from_value(json!({"actor": "agent:qa", "run": "qa", "role": "reader"})).unwrap(),
    )
    .unwrap();
    let human = Session::human("human:qa".into(), Role::Reader).unwrap();
    let cx3 = OpContext {
        engine: &*served,
        session: &human,
        human: "human:qa",
        cwd: dir3.path(),
        agent: &UnavailableAgent,
    };
    let state = |e: &Local| -> (String, Vec<(String, Vec<u8>)>) {
        e.inspect(|r| {
            Ok((
                format!("{:?}", r.store().log_end()),
                r.store().refs_with_prefix("").unwrap(),
            ))
        })
        .unwrap()
    };
    let before = state(&served);
    assert!(!before.1.is_empty(), "the project has refs to compare");
    for args in [
        json!({"id": k3, "scope": "canon"}),
        json!({"id": k3, "scope": "canon", "depth": 8, "detail": true}),
        json!({"id": k3, "scope": "tag:v1.0.0", "max_tokens": 1}),
    ] {
        let mcp = rdy_trace(&mut st, from_value(args.clone()).unwrap()).unwrap();
        let daemon = dispatch(&cx3, "trace", args.clone()).unwrap();
        assert_eq!(mcp["id"], k3.as_str(), "{mcp}");
        assert_eq!(daemon, mcp, "{args}");
        assert_eq!(
            state(&served),
            before,
            "a trace read changed the store: {args}"
        );
    }
    drop(dir3);
}

#[test]
fn rf12_context_stays_twenty_and_brief_stays_one_level() {
    let s = Served::new();
    let (k, ids) = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let w = write(&mut r, "w", json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "decision", "id": "$d1", "parent": "$s",
                "fields": {"statement": "Plans are written", "status": "proposed"}},
            {"op": "create", "type": "decision", "id": "$d2", "parent": "$s",
                "fields": {"statement": "Plans are reviewed", "status": "proposed"},
                "links": {"refines": [{"target": "$d1"}]}},
            {"op": "create", "type": "decision", "id": "$d3", "parent": "$s",
                "fields": {"statement": "Plans are reviewed weekly", "status": "proposed"},
                "links": {"refines": [{"target": "$d2"}]}},
            {"op": "create", "type": "claim", "id": "$k", "parent": "$s", "body": "Version 0.",
                "links": {"decided_by": [{"target": "$d3"}]}}
        ]))
        .unwrap();
        land(&mut r, "w").unwrap();
        let k = w.created["$k"].clone();
        // more than 20 later versions of K, in a view
        for n in 1..=24 {
            write(
                &mut r,
                "w2",
                json!([{"op": "update", "id": k, "body": format!("Version {n}.")}]),
            )
            .unwrap();
        }
        r.close().unwrap();
        (
            k,
            [
                w.created["$d1"].clone(),
                w.created["$d2"].clone(),
                w.created["$d3"].clone(),
            ],
        )
    };
    let mut c = McpClient::start_proxy(&s);
    session(&mut c, "reader");
    // context: 20 rows, and it says there are more
    let ctx = c
        .call("rdy_context", json!({"id": k, "scope": "view:w2"}))
        .ok();
    assert_eq!(ctx["history"].as_array().unwrap().len(), 20, "{ctx}");
    assert_eq!(ctx["history_more"], true);
    // trace: the first landing is found past those 20 rows, and the lineage is three deep
    let t = c
        .call(
            "rdy_trace",
            json!({"id": k, "scope": "view:w2", "depth": 3}),
        )
        .ok();
    assert!(t["introduced"]["id"].is_string(), "{t}");
    let depths: Vec<(String, u64)> = t["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["id"].as_str().unwrap().to_string(),
                d["depth"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        depths,
        vec![
            (ids[2].clone(), 0),
            (ids[1].clone(), 1),
            (ids[0].clone(), 2)
        ],
        "{t}"
    );
    // the briefing of K names the decision that governs it; the briefing of that decision
    // names the decision it refines, one level, and not the one that decision refines
    let b = c
        .call("rdy_brief", json!({"ids": [k], "scope": "canon"}))
        .ok();
    let governs = serde_json::to_string(&b["governs"]).unwrap();
    assert!(governs.contains(&ids[2]), "{b}");
    let b = c
        .call("rdy_brief", json!({"ids": [ids[2]], "scope": "canon"}))
        .ok();
    let lineage = serde_json::to_string(&b["lineage"]).unwrap();
    assert!(lineage.contains(&ids[1]), "one level is there: {b}");
    assert!(
        !lineage.contains(&ids[0]),
        "a briefing stays one level deep: {lineage}"
    );
    c.alive();
}

#[test]
fn rf12_all_dr1b_user_paths_are_chat_reachable() {
    let s = Served::new();
    {
        // a landed tension for the review path
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let spec = spec_doc(&mut r);
        put(&mut r, "a", upd(&spec.c, "C contradicts B."));
        r.close().unwrap();
    }
    // a linked code checkout, so a code reflects has a tree to record
    let g = GitRepo::new();
    g.write("src/retention.rs", "pub const DAYS: u32 = 7;\n");
    g.commit("feat: retention");
    g.link(s.dir.path());
    let mut c = McpClient::start_proxy(&s);
    session(&mut c, "writer");
    let created = |v: &Value, k: &str| v["created"][k].as_str().unwrap().to_string();

    // a source, and a finding that quotes it
    let w = c
        .call("rdy_write", json!({"ops": [
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Retention"}},
            {"op": "create", "type": "source", "id": "$src", "parent": "$s",
                "fields": {"title": "Release 2", "kind": "release", "locator": "v2"}},
            {"op": "create", "type": "claim", "id": "$k", "parent": "$s", "body": "Retention is seven days."},
            {"op": "create", "type": "claim", "id": "$c2", "parent": "$s", "body": "Backups follow retention.",
                "fields": {"reflects": ["src/*.rs"]}}
        ]}))
        .ok();
    let (sec, src, k, c2) = (
        created(&w, "$s"),
        created(&w, "$src"),
        created(&w, "$k"),
        created(&w, "$c2"),
    );
    let w = c
        .call("rdy_write", json!({"ops": [
            {"op": "create", "type": "finding", "id": "$f", "parent": sec,
                "fields": {"statement": "The release changed retention.",
                           "evidence": [{"quote": "Retention is now seven days."}, {"source": format!("{{{{{src}}}}}")}]}}
        ]}))
        .ok();
    let finding = created(&w, "$f");
    // the refusals this feature adds, each through the chat path
    let e = tool_error(c.call(
        "rdy_write",
        json!({"ops": [{"op": "create", "type": "finding", "parent": sec,
            "fields": {"statement": "No evidence.", "evidence": []}}]}),
    ));
    assert_eq!(e["code"], "E_VALIDATION", "{e}");
    assert!(e["fix"].as_str().is_some_and(|f| !f.is_empty()), "{e}");
    let e = tool_error(c.call(
        "rdy_write",
        json!({"ops": [{"op": "update", "id": k, "body": "Retention is seven days."}], "reflects": {"sources": [c2]}}),
    ));
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert_eq!(
        e["message"],
        format!("reflects names {c2}, a claim; reflects names a source"),
        "{e}"
    );
    let e = tool_error(c.call("rdy_review_submit", json!({"request": "rq-000000000000", "case_hash": "0", "verdict": "tension", "findings": []})));
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert_eq!(
        e["message"], "rdy_review_submit: unknown parameter \"findings\"",
        "{e}"
    );
    assert!(e["fix"].as_str().unwrap().contains("citations"), "{e}");

    // a decision that cites the finding, and K written as following the source
    let d = c
        .call(
            "rdy_decide",
            json!({"parent": sec, "statement": "Retention is seven days",
            "depends_on": [finding]}),
        )
        .ok();
    let decision = created(&d, "$decision");
    let w = c
        .call(
            "rdy_write",
            json!({"ops": [{"op": "update", "id": k, "body": "Retention is seven days.",
            "links": {"decided_by": [{"target": decision}]}}],
            "reflects": {"sources": [src]}}),
        )
        .ok();
    assert!(w["justification"].is_object(), "{w}");
    // and C2, which declares reflects globs, written as following the code
    let w = c
        .call(
            "rdy_write",
            json!({"ops": [{"op": "update", "id": c2, "body": "Backups follow retention in code."}],
            "reflects": {"code": true}}),
        )
        .ok();
    assert_eq!(
        w["justification"],
        json!({"reflects": {"sources": [], "code": true}}),
        "{w}"
    );
    assert_eq!(w["code_recorded"], json!([c2]), "{w}");
    // changing the finding gives consequences
    let w = c
        .call(
            "rdy_write",
            json!({"ops": [{"op": "update", "id": finding,
            "fields": {"statement": "The release changed retention to seven days."}}],
            "editorial": {"class": "clarify", "reason": "Say the number."}}),
        )
        .ok();
    assert!(w["consequences"].is_array(), "{w}");
    // the briefing shows what the block follows and what the decision stands on
    let b = c.call("rdy_brief", json!({"ids": [k]})).ok();
    assert!(b["sources"].is_array() || b["sources"].is_object(), "{b}");
    let b = c.call("rdy_brief", json!({"ids": [decision]})).ok();
    assert!(b["findings"].is_array() || b["findings"].is_object(), "{b}");
    let b = c
        .call("rdy_brief", json!({"code_path": "src/retention.rs"}))
        .ok();
    assert_eq!(b["code"]["path"], "src/retention.rs", "{b}");
    assert_eq!(b["code"]["reflects"][0]["block"], c2.as_str(), "{b}");

    // review citations on a recorded tension
    let mut r = McpClient::start_proxy(&s);
    session_as(&mut r, "agent:r", "reviewer");
    let n = r.call("rdy_review_next", json!({"kind": "tension"})).ok();
    let block = n["case"]["blocks"][0]["id"].clone();
    let sub = r
        .call(
            "rdy_review_submit",
            json!({
                "request": n["request"], "case_hash": n["case_hash"], "verdict": "tension",
                "citations": [{"block": block, "note": "these two cannot both hold"}]
            }),
        )
        .ok();
    assert_eq!(sub["severity"], "advisory", "{sub}");

    // and the trace reads it all back
    let t = c.call("rdy_trace", json!({"id": k})).ok();
    assert_eq!(t["governing"][0]["decision"], decision.as_str(), "{t}");
    assert_eq!(t["governing"][0]["via"], "decided_by", "{t}");
    assert_eq!(t["sources"][0]["source"], src.as_str(), "{t}");
    assert_eq!(t["sources"][0]["kind"], "release", "{t}");
    assert!(
        t["premises"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == finding.as_str() && p["block_type"] == "finding"),
        "{t}"
    );
    assert_eq!(t["ungrounded"], false);
    let t2 = c.call("rdy_trace", json!({"id": c2})).ok();
    assert_eq!(t2["ungrounded"], false, "reflects globs ground it: {t2}");
    let e = tool_error(c.call("rdy_trace", json!({"id": k, "depth": 0})));
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    c.alive();
}

fn session_as(c: &mut McpClient, actor: &str, role: &str) {
    let r = c.call(
        "rdy_session",
        json!({"actor": actor, "run": actor.replace(':', "-"), "role": role}),
    );
    assert!(r.code().is_none(), "{:?}", r.ok());
}
