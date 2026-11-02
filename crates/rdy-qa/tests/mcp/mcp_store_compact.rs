//! mh7: `rdy_compact` and the byte report (D-4YRQ4, D-KQ4CA). One shared helper
//! (`rdy_engine::run_compact`) guards the role and builds the report for the chat tool, the
//! daemon op, the CLI and the adapter; a dry run reports the same fields and punches nothing.
use rdy_daemon::{OpContext, dispatch};
use rdy_engine::engine::{Engine, Local};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{E_PERMISSION, Rdy, Session};
use rdy_mcp::params::parse_params;
use rdy_mcp::tools::{CompactParams, E_NO_SESSION, McpState, rdy_compact, rdy_session};
use rdy_model::Role;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{approver, land, project, write};
use rdy_qa::mcp::{McpClient, Reply};
use rdy_qa::tensions::{ag, created};
use rdy_store::compact::on_allocation;
use rdy_store::pio::{self, Punch};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

const REPORT_KEYS: [&str; 6] = [
    "log_bytes",
    "removed_bytes",
    "punched_bytes",
    "on_disk_bytes",
    "by_kind",
    "notes",
];
const ROW_KEYS: [&str; 4] = ["kind", "name", "bytes", "removed_bytes"];

/// Removes the hooks when a test ends, also when it panics.
struct Hooks;
impl Drop for Hooks {
    fn drop(&mut self) {
        pio::on_punch(None);
        on_allocation(None);
    }
}

/// A landed block, then the person's approved removal of it: the log holds a removed span.
fn remove_a_block(r: &mut Rdy) {
    let w = write(
        r,
        "gone",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "gone", "title": "Gone", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "The launch code is ZEBRA-7731."}
        ]),
    )
    .unwrap();
    land(r, "gone").unwrap();
    let rq = ag("rq");
    r.redact_in(
        &rq,
        "rq",
        RedactInput {
            targets: vec![created(&w, "$p").to_string()],
            reason_class: "secret".into(),
            reason: "a leaked code".into(),
            dry_run: false,
        },
    )
    .unwrap()
    .redaction
    .expect("the request is recorded");
    let p = r
        .propose(&rq, "rq", "Remove the code", Some("Removes a leaked code."))
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
    r.seal_open().unwrap();
}

fn removed_project() -> (tempfile::TempDir, Rdy) {
    let (dir, mut r) = project();
    remove_a_block(&mut r);
    (dir, r)
}

fn session(role: Role) -> Session {
    Session {
        actor: "agent:a".into(),
        run: None,
        role,
        kind: rdy_engine::SessionKind::Human,
    }
}

/// Counts the allocation censuses of segment 0 on this thread (one per report: a real run makes
/// two, a dry run one), and answers every segment with `bytes`.
fn count_censuses(bytes: u64) -> Arc<AtomicU32> {
    let seen = Arc::new(AtomicU32::new(0));
    let count = Arc::clone(&seen);
    on_allocation(Some(Box::new(move |seg| {
        if seg == 0 {
            count.fetch_add(1, Ordering::SeqCst);
        }
        Some(Ok(bytes))
    })));
    seen
}

/// The keys of an object, sorted: a JSON value keeps no key order, so the order the report
/// serializes its fields in is pinned by the release contract's field lists instead.
fn keys(v: &Value) -> Vec<&str> {
    v.as_object().unwrap().keys().map(String::as_str).collect()
}

fn sorted<const N: usize>(mut names: [&'static str; N]) -> [&'static str; N] {
    names.sort_unstable();
    names
}

/// A report has the six keys, and rows of four keys with ascending numeric kinds.
fn assert_shape(v: &Value) {
    assert_eq!(keys(v), sorted(REPORT_KEYS), "{v}");
    let rows = v["by_kind"].as_array().unwrap();
    assert!(!rows.is_empty(), "{v}");
    let mut last = None;
    for row in rows {
        assert_eq!(keys(row), sorted(ROW_KEYS), "{row}");
        let kind = row["kind"].as_u64().unwrap();
        assert!(last.is_none_or(|l| l < kind), "ascending kinds: {v}");
        last = Some(kind);
    }
    assert_eq!(
        rows.iter()
            .map(|r| r["bytes"].as_u64().unwrap())
            .sum::<u64>(),
        v["log_bytes"].as_u64().unwrap()
    );
    assert_eq!(
        rows.iter()
            .map(|r| r["removed_bytes"].as_u64().unwrap())
            .sum::<u64>(),
        v["removed_bytes"].as_u64().unwrap()
    );
}

/// The bytes of every segment file, in order: a compact leaves them as they are (the holes it
/// punches read as the zeros the removal wrote).
fn segments(root: &Path) -> Vec<Vec<u8>> {
    let mut files: Vec<_> = std::fs::read_dir(root.join(".rdy/store/segments"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    files.iter().map(|p| std::fs::read(p).unwrap()).collect()
}

fn tool_error(r: Reply) -> Value {
    match r {
        Reply::ToolError(v) => v,
        other => panic!("expected an rdy tool error, got {other:?}"),
    }
}

#[test]
fn mh7_chat_cli_and_adapter_share_one_report() {
    let _hooks = Hooks;
    let (dir, r) = removed_project();
    let engine = Arc::new(Local::new(r));

    // forced allocation: the chat tool, the daemon op, the shared helper and the engine's own
    // report (the adapter's call) agree on every key, value and row
    count_censuses(8192);
    let mut st = McpState::new(engine.clone());
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:a", "run": "a", "role": "writer"})).unwrap(),
    )
    .unwrap();
    let chat = rdy_compact(
        &mut st,
        CompactParams {
            dry_run: Some(true),
        },
    )
    .unwrap();
    let human = Session::human("human:qa".into(), Role::Writer).unwrap();
    let op = dispatch(
        &OpContext {
            engine: &*engine,
            session: &human,
            human: "human:qa",
            cwd: dir.path(),
            agent: &rdy_engine::signing_crypto::UnavailableAgent,
        },
        "compact",
        json!({"dry_run": true}),
    )
    .unwrap();
    let shared = rdy_engine::run_compact(&*engine, &session(Role::Writer), true).unwrap();
    let direct = engine
        .read_serialized(Box::new(|r| {
            Ok(serde_json::to_value(r.compact(true)?).unwrap())
        }))
        .unwrap();
    assert_shape(&chat);
    assert!(chat["removed_bytes"].as_u64().unwrap() > 0, "{chat}");
    assert_eq!(chat["punched_bytes"], 0);
    assert_eq!(chat["on_disk_bytes"].as_u64().unwrap() % 8192, 0);
    for other in [&op, &shared, &direct] {
        assert_eq!(&chat, other);
    }
    on_allocation(None);
    drop(st);
    Arc::try_unwrap(engine).ok().unwrap().close().unwrap();

    // the real transports on one project: a child `rdy mcp`, the CLI direct and through the
    // daemon, and a proxy; none adds a frame
    let s = Served::new();
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    remove_a_block(&mut r);
    r.close().unwrap();
    // each transport reports the same bytes. The first pass lets every client make its own first
    // writes (a child's first open records a session batch); the second pass is compared
    let pass = |compare: Option<&Value>| {
        let mut reports = vec![];
        let mut c = McpClient::start(s.dir.path());
        c.call(
            "rdy_session",
            json!({"actor": "agent:qa", "run": "qa", "role": "writer"}),
        )
        .ok();
        reports.push((
            "child rdy mcp",
            c.call("rdy_compact", json!({"dry_run": true})).ok(),
        ));
        drop(c);
        let (ok, v) = s.json(&["--json", "--no-daemon", "compact", "--dry-run"]);
        assert!(ok, "{v}");
        reports.push(("rdy --no-daemon compact --dry-run", v));
        let (ok, v) = s.json(&["--json", "compact", "--dry-run"]);
        assert!(ok, "{v}");
        reports.push(("rdy compact --dry-run through the daemon", v));
        let mut c = McpClient::start_proxy(&s);
        c.call(
            "rdy_session",
            json!({"actor": "agent:qa", "run": "qa", "role": "writer"}),
        )
        .ok();
        reports.push((
            "rdy mcp through the daemon",
            c.call("rdy_compact", json!({"dry_run": true})).ok(),
        ));
        drop(c);
        s.stop();
        for (what, v) in &reports {
            assert_shape(v);
            if let Some(base) = compare {
                assert_eq!(v, base, "{what}");
            }
        }
    };
    pass(None);
    let before_log = segments(s.dir.path());
    let base = {
        let r = Rdy::open(s.dir.path()).unwrap();
        let v = serde_json::to_value(r.compact(true).unwrap()).unwrap();
        r.close().unwrap();
        v
    };
    assert_shape(&base);
    assert!(base["removed_bytes"].as_u64().unwrap() > 0, "{base}");
    pass(Some(&base));
    assert_eq!(segments(s.dir.path()), before_log, "no frame changed");
}

#[test]
fn mh7_compact_role_and_input_guards_precede_io() {
    let _hooks = Hooks;
    let (_dir, r) = removed_project();
    let engine = Arc::new(Local::new(r));
    let censuses = count_censuses(4096);
    let punches = Arc::new(AtomicU32::new(0));
    let punched = Arc::clone(&punches);
    pio::on_punch(Some(Box::new(move |_, _| {
        punched.fetch_add(1, Ordering::SeqCst);
        Some(Ok(Punch::Supported))
    })));

    // no session: the existing text, and no census
    let mut st = McpState::new(engine.clone());
    let e = rdy_compact(&mut st, CompactParams { dry_run: None }).unwrap_err();
    assert_eq!(e.code, E_NO_SESSION);
    assert_eq!(e.message, "no session");
    assert_eq!(
        e.fix,
        "state rdy/actor and rdy/run in _meta, or call rdy_session"
    );
    // a reader: refused with a dry run too
    let mut reader = McpState::new(engine.clone());
    reader.session = Some(session(Role::Reader));
    for dry_run in [None, Some(false), Some(true)] {
        let e = rdy_compact(&mut reader, CompactParams { dry_run }).unwrap_err();
        assert_eq!(e.code, E_PERMISSION);
        assert_eq!(e.message, "role Reader cannot compact");
        assert_eq!(e.fix, "start the session as a writer");
    }
    // a close key and a wrong type never reach the tool
    for args in [
        json!({"dry_runs": true}),
        json!({"dry_run": "true"}),
        json!({"dry_run": 0}),
    ] {
        parse_params::<CompactParams>("rdy_compact", args.as_object().unwrap().clone())
            .unwrap_err();
    }
    assert_eq!(censuses.load(Ordering::SeqCst), 0, "no census ran");
    assert_eq!(punches.load(Ordering::SeqCst), 0, "no punch ran");

    // omitted, null, false and true all pass the guards; true only reports
    let mut writer = McpState::new(engine.clone());
    writer.session = Some(session(Role::Writer));
    for (args, runs) in [
        (json!({}), 2),
        (json!({"dry_run": null}), 2),
        (json!({"dry_run": false}), 2),
        (json!({"dry_run": true}), 1),
    ] {
        censuses.store(0, Ordering::SeqCst);
        let p = parse_params::<CompactParams>("rdy_compact", args.as_object().unwrap().clone())
            .unwrap();
        let v = rdy_compact(&mut writer, p).unwrap();
        assert_shape(&v);
        assert_eq!(censuses.load(Ordering::SeqCst), runs, "{args}");
    }
    drop((st, reader, writer));
    Arc::try_unwrap(engine).ok().unwrap().close().unwrap();
}

#[test]
fn mh7_a_compact_through_the_wire_refuses_before_a_claim_and_a_reader() {
    let (dir, r) = removed_project();
    r.close().unwrap();
    let mut c = McpClient::start(dir.path());
    // the wire needs a claim before the arguments are read; a reader's claim is refused after
    let e = tool_error(c.call("rdy_compact", json!({})));
    assert_eq!(e["code"], E_NO_SESSION, "{e}");
    c.call(
        "rdy_session",
        json!({"actor": "agent:r", "run": "r", "role": "reader"}),
    )
    .ok();
    let e = tool_error(c.call("rdy_compact", json!({"dry_run": true})));
    assert_eq!(e["code"], E_PERMISSION, "{e}");
    assert_eq!(e["message"], "role Reader cannot compact");
    let e = tool_error(c.call("rdy_compact", json!({"dry_runs": true})));
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    c.alive();
}

const DESCRIPTION: &str = "Free disk space used by removed payloads without changing the log. Set dry_run to true to report bytes without punching holes. Requires a writer session.";

#[test]
fn mh7_the_tool_is_listed_with_its_description_and_one_property() {
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    let tools = c.tools();
    let tool = tools
        .iter()
        .find(|t| t["name"] == "rdy_compact")
        .expect("rdy_compact is listed");
    assert_eq!(tool["description"], DESCRIPTION);
    let props = tool["inputSchema"]["properties"].as_object().unwrap();
    assert_eq!(props.keys().collect::<Vec<_>>(), ["dry_run"]);
    assert_eq!(
        props["dry_run"]["description"],
        "true: report bytes without punching holes. Default false."
    );
}
