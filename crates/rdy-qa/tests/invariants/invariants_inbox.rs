//! ib12: the inbox is a function of the log. A reindex, a restart, a killed daemon and a session
//! setting never change the page a cursor yields (spec §10.2, §16 criterion 5; D-TPZ0W, D-Z0EMP).
use rdy_engine::inbox::api::{InboxRequest, run_inbox};
use rdy_engine::inbox::read::cursor_text;
use rdy_engine::inbox::sub::subscribe;
use rdy_engine::{Engine, Local, Rdy, Session};
use rdy_model::BlockId;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use rdy_qa::packs::{QA_PACK_YAML, qa_project, use_pack};
use rdy_qa::tensions::*;
use rdy_store::inbox::InboxCursor;
use serde_json::{Value, json};

/// Runs `f` on the project inside `e` as one engine write; `Local::write` settles the tension
/// set after it, as the daemon's writer does.
fn on<T: Send + 'static>(e: &Local, f: impl FnOnce(&mut Rdy) -> T + Send + 'static) -> T {
    let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
    let put = std::sync::Arc::clone(&slot);
    e.write(Box::new(move |r| {
        *put.lock().unwrap() = Some(f(r));
        Ok(serde_json::Value::Null)
    }))
    .unwrap();
    let mut out = slot.lock().unwrap();
    out.take().unwrap()
}

fn page(e: &Local, s: &Session) -> Value {
    let req: InboxRequest = serde_json::from_value(json!({})).unwrap();
    run_inbox(e, s, req).unwrap()
}

/// A doc at `path` in canon with one claim; returns the claim.
fn landed_claim(r: &mut Rdy, path: &str) -> BlockId {
    r.new_doc(&writer(), "work", path, Some("overview"), Some("T"))
        .unwrap();
    let doc = r
        .read()
        .resolve_path(&rdy_engine::Scope::View("work".into()), path)
        .unwrap();
    let w = write(
        r,
        "work",
        json!([{"op": "create", "type": "claim", "id": "$c",
        "parent": doc.to_string(), "body": "x"}]),
    )
    .unwrap();
    land(r, "work").unwrap();
    created(&w, "$c")
}

/// Agent `a` watches `payments` and changes a payments claim. Agent `b` changes the same claim
/// the other way. The human lands an `identity` change. Agent `b` posts an intent on `a`'s block.
/// The human lands a `payments` change on that block, which rebases `a`'s view with a conflict.
fn build_agent(e: &Local) {
    let (pay, ident) = on(e, |r| {
        (
            landed_claim(r, "payments/fees"),
            landed_claim(r, "identity/login"),
        )
    });
    subscribe(e, &ag("a"), Some(vec!["payments".into()])).unwrap();
    let p = pay.clone();
    on(e, move |r| {
        put(r, "a", upd(&p, "Fees are 1%."));
    });
    let p = pay.clone();
    on(e, move |r| {
        put(r, "b", upd(&p, "Fees are 2%."));
    });
    on(e, move |r| {
        write(r, "work", upd(&ident, "Login needs a code.")).unwrap();
        land(r, "work").unwrap();
    });
    let p = pay.clone();
    on(e, move |r| {
        r.intent_post(&ag("b"), Some("b"), None, &[p.to_string()], "edit", None)
            .unwrap();
    });
    on(e, move |r| {
        write(r, "work", upd(&pay, "Fees are 3%.")).unwrap();
        land(r, "work").unwrap();
    });
}

fn events(v: &Value) -> Vec<&str> {
    v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect()
}

#[test]
fn ib12_reindex_and_a_restart_give_the_same_page() {
    let (d, r) = qa_project();
    let e = Local::new(r);
    build_agent(&e);
    let first = page(&e, &ag("a"));
    assert!(
        events(&first).contains(&"tension_opened") && events(&first).contains(&"view_rebased"),
        "the scenario fills the page: {first}"
    );
    on(&e, |r| r.reindex().unwrap());
    assert_eq!(page(&e, &ag("a")), first, "after a reindex");
    e.close().unwrap();
    let e = Local::open(d.path()).unwrap();
    assert_eq!(page(&e, &ag("a")), first, "after a restart");
    e.close().unwrap();
}

#[test]
fn ib12_reindex_reproduces_the_logged_moves() {
    let (_d, r) = qa_project();
    let e = Local::new(r);
    let start = e.read().seq();
    build_agent(&e);
    let log = on(&e, move |r| r.replay_events(start).unwrap());
    let open = on(&e, |r| r.read().reader().open_tensions().unwrap());
    assert!(
        !open.is_empty(),
        "the scenario leaves the a/b tension and the re-keyed a686 tension open"
    );
    let kinds: Vec<String> = log
        .iter()
        .map(|ev| serde_json::to_value(ev).unwrap()["kind"]["event"].to_string())
        .collect();
    for want in ["tension_opened", "tension_resolved"] {
        assert!(
            kinds.iter().any(|k| k.contains(want)),
            "the scenario logs {want}: {kinds:?}"
        );
    }
    on(&e, |r| r.reindex().unwrap());
    assert_eq!(on(&e, move |r| r.replay_events(start).unwrap()), log);
    assert_eq!(on(&e, |r| r.read().reader().open_tensions().unwrap()), open);
    assert_eq!(on(&e, |r| r.fsck().unwrap().unlogged_moves), 0);
    e.close().unwrap();
}

#[test]
fn ib12_a_killed_daemon_gives_the_same_page_after_restart() {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let sp = spec_doc(&mut r);
    r.close().unwrap();
    let (ok, v) = s.json_as("agent:a", Some("a"), &["--json", "inbox"]);
    assert!(ok && v["started"] == true, "{v}");
    // two agents change the spec through the running daemon, so the tension is built step by
    // step in its write path and the kill hits a daemon with live state
    let write = |actor: &str, run: &str, ops: Value| {
        let mut c = McpClient::start_proxy(&s);
        c.call(
            "rdy_session",
            json!({"actor": actor, "run": run, "role": "writer"}),
        )
        .ok();
        let w = c.call("rdy_write", json!({"ops": ops}));
        assert!(
            w.code().is_none(),
            "{actor} writes through the daemon: {w:?}"
        );
    };
    write("agent:b", "b", cite(&sp.section, &sp.a));
    write("agent:a", "a", upd(&sp.a, "A changed."));
    let (ok, before) = s.json_as("agent:a", Some("a"), &["--json", "inbox"]);
    assert!(ok, "{before}");
    assert!(
        events(&before).contains(&"tension_opened"),
        "the page holds a move: {before}"
    );
    let first = s.daemon_pid().expect("the daemon that took the writes");
    s.kill9();
    let (ok, after) = s.json_as("agent:a", Some("a"), &["--json", "inbox"]);
    assert!(ok, "{after}");
    let second = s.daemon_pid().expect("a successor");
    assert_ne!(first, second, "the second page comes from a successor");
    assert_eq!(after, before);
}

/// A served project whose pack declares the `payments` area.
fn served() -> Served {
    let s = Served::new();
    use_pack(
        s.dir.path(),
        "rdy-qa",
        QA_PACK_YAML,
        "serve:\n  idle_timeout: 60\n",
    );
    s
}

#[test]
fn ib12_a_session_setting_never_changes_the_page_of_a_cursor() {
    let s = served();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let pay = landed_claim(&mut r, "payments/fees");
    let since = cursor_text(&InboxCursor {
        at: r.read().seq(),
        skip: 0,
    });
    put(&mut r, "a", upd(&pay, "Fees are 1%."));
    put(&mut r, "b", upd(&pay, "Fees are 2%."));
    r.close().unwrap();
    let session = |view: &str| {
        json!({"actor": "agent:a", "run": "a", "role": "writer", "areas": ["payments"],
        "view": view})
    };
    let mut one = McpClient::start_proxy(&s);
    one.call("rdy_session", session("a")).ok();
    let first = one.call("rdy_inbox", json!({"since": since})).ok();
    assert!(
        events(&first).contains(&"tension_opened"),
        "the page holds a move: {first}"
    );
    let mut two = McpClient::start_proxy(&s);
    two.call("rdy_session", session("b")).ok();
    let second = two.call("rdy_inbox", json!({"since": since})).ok();
    assert_eq!(second, first);
}
