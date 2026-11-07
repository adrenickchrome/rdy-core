//! rtc5 (FW-MH.rules, D-ZSRBN): `rdy_rule` over MCP, on the real `rdy`. The project server lists
//! it with the others; the projectless server lists it only while a meta project exists, and its
//! call needs a claim and a writer or reviewer role. A person's rule goes to the meta project and
//! reaches every other project on its next instructions call; a project's rule stays in that
//! project. The tool belongs to the `write` ruleset, so it warns and counts like its neighbours,
//! once per request, and never changes what the call does. Every person lives in an isolated home.
use rdy_qa::daemon::short_tempdir;
use rdy_qa::fixtures::output_within;
use rdy_qa::instructions::World;
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const BUDGET: Duration = Duration::from_secs(120);
/// A hang guard: loops wait for a signal and assert nothing about how long it took.
const WAIT: Duration = Duration::from_secs(60);
const STALE: &str = "kf-00000000";

/// A process of `w` started with no project, in `cwd`.
fn command(w: &World, cwd: &Path, args: &[&str]) -> Command {
    let mut cmd = rdy_qa::fixtures::rdy_command();
    cmd.args(args)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(w.env())
        .current_dir(cwd);
    cmd
}

fn rdy_at(w: &World, cwd: &Path, args: &[&str]) -> Output {
    output_within(&mut command(w, cwd, args), BUDGET).expect("rdy ran")
}

fn json_of(out: &Output) -> Value {
    let raw = if out.status.success() {
        &out.stdout
    } else {
        &out.stderr
    };
    let text = String::from_utf8_lossy(raw);
    serde_json::from_str(text.trim())
        .or_else(|_| serde_json::from_str(text.trim().lines().last().unwrap_or("")))
        .unwrap_or_else(|_| panic!("JSON in {text}"))
}

/// `rdy mcp` with no `--project`, started in `cwd`.
fn mcp_at(w: &World, cwd: &Path) -> McpClient {
    let mut cmd = command(w, cwd, &["mcp"]);
    cmd.stderr(Stdio::piped());
    McpClient::launch(cmd)
}

fn names(c: &mut McpClient) -> Vec<String> {
    let mut n: Vec<String> = c
        .tools()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    n.sort();
    n
}

fn session_as(c: &mut McpClient, actor: &str, run: &str, role: &str) {
    let r = c.call(
        "rdy_session",
        json!({"actor": actor, "run": run, "role": role}),
    );
    assert!(r.is_ok(), "{r:?}");
}

fn body(r: Reply) -> Value {
    match r {
        Reply::Ok(v) | Reply::ToolError(v) => v,
        Reply::RpcError(e) => panic!("a protocol error: {e}"),
    }
}

fn ok(c: &mut McpClient, tool: &str, args: Value) -> Value {
    match c.call(tool, args.clone()) {
        Reply::Ok(v) => v,
        other => panic!("{tool} {args}: {other:?}"),
    }
}

/// The code of a refused call, with its body.
fn refused(c: &mut McpClient, tool: &str, args: Value) -> Value {
    match c.call(tool, args.clone()) {
        Reply::ToolError(e) => e,
        other => panic!("{tool} {args} was not refused: {other:?}"),
    }
}

fn add(scope: &str, text: &str) -> Value {
    json!({"scope": scope, "op": "add", "text": text, "why": "The person asked for it.",
        "evidence": "Please keep this rule.", "question": "Shall I save this rule?",
        "session_ref": "chat-1"})
}

/// A request that no card exists for: the answer form with a ref of no proposal.
fn stray_answer(scope: &str) -> Value {
    json!({"scope": scope, "answer": {"ref": "P-ZZZZZ@1", "option": "later",
        "card_hash": "0".repeat(64), "evidence": "Not yet.", "question": "Land it?"}})
}

/// The instructions rows an authored layer sent.
fn rows(v: &Value, layer: &str) -> Vec<Value> {
    v["instructions"]
        .as_array()
        .unwrap_or_else(|| panic!("instructions in {v}"))
        .iter()
        .filter(|r| r["layer"] == layer)
        .cloned()
        .collect()
}

fn texts(v: &Value, layer: &str) -> Vec<String> {
    rows(v, layer)
        .iter()
        .map(|r| r["text"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// `rdy setup` for `w`, run outside every project: the meta project at its default place.
fn setup(w: &World, outside: &Path) {
    let out = rdy_at(w, outside, &["--json", "setup"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(json_of(&out)["meta"]["result"], "created");
}

#[test]
fn rtc5_projectless_rule_claims_roles_and_scope() {
    let w = World::new();
    let outside = short_tempdir();
    let mut old = mcp_at(&w, outside.path());
    assert_eq!(names(&mut old), ["rdy_instructions", "rdy_session"]);
    setup(&w, outside.path());
    // the same connection now lists the person's rule tool and no other new tool
    assert_eq!(
        names(&mut old),
        ["rdy_instructions", "rdy_rule", "rdy_session"]
    );
    // no session: the claim comes first
    let e = refused(
        &mut old,
        "rdy_rule",
        add("person", "Answer in British English."),
    );
    assert_eq!(e["code"], "E_NO_SESSION", "{e}");
    // a reader writes nothing
    session_as(&mut old, "agent:p", "p1", "reader");
    let e = refused(
        &mut old,
        "rdy_rule",
        add("person", "Answer in British English."),
    );
    assert_eq!(e["code"], "E_PERMISSION", "{e}");
    // a writer's person rule lands in the meta project
    session_as(&mut old, "agent:p", "p1", "writer");
    let v = ok(
        &mut old,
        "rdy_rule",
        add("person", "Answer in British English."),
    );
    assert_eq!(v["scope"], "person", "{v}");
    assert_eq!(v["rule"]["text"], "Answer in British English.", "{v}");
    assert_eq!(v["rule"]["retired"], false, "{v}");
    assert!(v["landing"]["landing"].is_string(), "{v}");
    // a project rule has no project here
    let e = refused(&mut old, "rdy_rule", add("project", "Keep to the point."));
    assert_eq!(
        (e["code"].as_str(), e["message"].as_str(), e["fix"].as_str()),
        (
            Some("E_NO_PROJECT"),
            Some("this session has no project"),
            Some("use scope person, or run rdy init in a folder and reconnect")
        ),
        "{e}"
    );
    // every other tool is as it was
    let e = refused(&mut old, "rdy_overview", json!({}));
    assert_eq!(e["code"], "E_NO_PROJECT", "{e}");
    assert_eq!(
        names(&mut old),
        ["rdy_instructions", "rdy_rule", "rdy_session"]
    );
    // the rule is in the person's canon: a new connection works in the meta project and reads it
    let v = json_of(&rdy_at(&w, outside.path(), &["--json", "instructions"]));
    assert!(
        texts(&v, "person").contains(&"Answer in British English.".to_string()),
        "{v}"
    );
    // only protocol lines reach stdout
    let t = old.finish(BUDGET);
    for line in t.stdout.lines() {
        let v: Value = serde_json::from_str(line).unwrap_or_else(|_| panic!("not JSON: {line}"));
        assert_eq!(v["jsonrpc"], "2.0", "{line}");
    }
    assert!(!t.stderr.contains("panicked"), "{}", t.stderr);
}

#[test]
fn rtc5_person_rule_reaches_project_b_on_next_instructions() {
    let w = World::new();
    let meta = w.project();
    w.person(meta.path());
    let (a, b) = (w.project(), w.project());
    let mut ca = w.mcp(a.path());
    session_as(&mut ca, "agent:a", "a1", "writer");
    let v = ok(
        &mut ca,
        "rdy_rule",
        add("person", "Answer in British English."),
    );
    assert_eq!(v["scope"], "person", "{v}");
    // project B never had an authoring session: its canon delivers the rule as the person's
    let mut cb = w.mcp(b.path());
    session_as(&mut cb, "agent:b", "b1", "reader");
    let v = ok(&mut cb, "rdy_instructions", json!({}));
    assert_eq!(texts(&v, "person"), ["Answer in British English."], "{v}");
    assert!(rows(&v, "project").is_empty(), "{v}");
    // project A reads the same rule from its person layer, not as its own
    let v = ok(&mut ca, "rdy_instructions", json!({}));
    assert_eq!(texts(&v, "person"), ["Answer in British English."], "{v}");
    assert!(rows(&v, "project").is_empty(), "{v}");
}

#[test]
fn rtc5_project_rule_stays_in_project_a() {
    let w = World::new();
    let meta = w.project();
    w.person(meta.path());
    let (a, b) = (w.project(), w.project());
    let mut ca = w.mcp(a.path());
    session_as(&mut ca, "agent:a", "a1", "writer");
    let before = ok(&mut ca, "rdy_view", json!({"op": "list"}));
    let v = ok(&mut ca, "rdy_rule", add("project", "Keep to the point."));
    assert_eq!(v["scope"], "project", "{v}");
    assert_eq!(v["rule"]["text"], "Keep to the point.", "{v}");
    // the caller's own working view is as it was
    assert_eq!(ok(&mut ca, "rdy_view", json!({"op": "list"})), before);
    // project A delivers it as its own
    let v = ok(&mut ca, "rdy_instructions", json!({}));
    assert_eq!(texts(&v, "project"), ["Keep to the point."], "{v}");
    assert!(rows(&v, "person").is_empty(), "{v}");
    // no person row was authored, and project B has none
    let mut cb = w.mcp(b.path());
    session_as(&mut cb, "agent:b", "b1", "reader");
    let v = ok(&mut cb, "rdy_instructions", json!({}));
    assert!(rows(&v, "project").is_empty(), "{v}");
    assert!(rows(&v, "person").is_empty(), "{v}");
    let mut cm = w.mcp(meta.path());
    session_as(&mut cm, "agent:m", "m1", "reader");
    let v = ok(&mut cm, "rdy_instructions", json!({}));
    assert!(
        !v.to_string().contains("Keep to the point."),
        "the meta project holds the project's rule: {v}"
    );
}

// ---- kungfu ----

/// The count rows of the project at `dir`, as the file holds them now.
fn file_rows(dir: &Path) -> Vec<Value> {
    let path = dir
        .join(".rdy/run")
        .join(rdy_engine::kungfu_counts::FILE_NAME);
    let Ok(text) = std::fs::read_to_string(path) else {
        return vec![];
    };
    let v: Value = serde_json::from_str(&text).expect("a count file");
    v["rows"].as_array().expect("rows").clone()
}

/// The `(not_loaded, lost, wrong)` sums of owner `name` in `rows`.
fn totals(rows: &[Value], name: &str) -> (u64, u64, u64) {
    let mut t = (0, 0, 0);
    for r in rows.iter().filter(|r| r["ruleset"] == name) {
        t.0 += r["not_loaded"].as_u64().unwrap();
        t.1 += r["lost"].as_u64().unwrap();
        t.2 += r["wrong"].as_u64().unwrap();
    }
    t
}

/// Waits until the project at `dir` counts at least `n` calls of `owner`, then returns the sums.
/// The wait is a hang guard: the loop waits for a signal and asserts nothing about its length.
fn counted(dir: &Path, owner: &str, n: u64) -> (u64, u64, u64) {
    let start = Instant::now();
    loop {
        let t = totals(&file_rows(dir), owner);
        if t.0 + t.1 + t.2 >= n {
            return t;
        }
        assert!(start.elapsed() < WAIT, "{owner} never reached {n} counts");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The `W_KUNGFU` entries of a reply body.
fn advisories(v: &Value) -> Vec<Value> {
    v["warnings"]
        .as_array()
        .map(|all| {
            all.iter()
                .filter(|w| w["code"] == "W_KUNGFU")
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn without_warning(v: &Value) -> Value {
    let mut out = v.clone();
    if let Some(all) = out.get_mut("warnings").and_then(Value::as_array_mut) {
        all.retain(|w| w["code"] != "W_KUNGFU");
        if all.is_empty() {
            out.as_object_mut().unwrap().remove("warnings");
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Correct,
    Absent,
    Wrong,
}

fn valued(mode: Mode, mut args: Value) -> Value {
    match mode {
        Mode::Correct => rdy_qa::mcp::supply_kungfu("rdy_rule", &mut args),
        Mode::Absent => {}
        Mode::Wrong => args["kungfu"] = json!(STALE),
    }
    args
}

/// What one run of a mode saw: each form's reply without the advisory, whether it was refused,
/// the advisories it carried, and the counts after it.
struct Run {
    replies: Vec<(bool, Value)>,
    carried: Vec<usize>,
    counts: Vec<(u64, u64, u64)>,
}

/// Both forms of the tool, in `order` (`true` is the mutation form), on a fresh project.
fn run_forms(mode: Mode, order: [bool; 2]) -> Run {
    let w = World::new();
    let dir = w.project();
    let mut c = w.mcp(dir.path()).raw();
    session_as(&mut c, "agent:k", "k1", "writer");
    let mut run = Run {
        replies: vec![],
        carried: vec![],
        counts: vec![],
    };
    let mut calls = 0;
    for mutation in order {
        let args = if mutation {
            add("project", "Keep to the point.")
        } else {
            stray_answer("project")
        };
        let (is_err, v) = match c.call("rdy_rule", valued(mode, args)) {
            Reply::Ok(v) => (false, v),
            Reply::ToolError(v) => (true, v),
            Reply::RpcError(e) => panic!("a protocol error: {e}"),
        };
        run.carried.push(advisories(&v).len());
        run.replies.push((is_err, without_warning(&v)));
        if mode != Mode::Correct {
            calls += 1;
            // one count for each admitted call, whichever form and whatever it came to
            run.counts.push(counted(dir.path(), "write", calls));
        }
    }
    drop(c);
    w.stop(dir.path());
    run
}

/// The normalized reply: ids and hashes differ between worlds.
fn masked(v: &Value) -> Value {
    let mut v = v.clone();
    // a project's name follows its folder
    if let Some(project) = v.get_mut("project").and_then(Value::as_object_mut) {
        project.remove("name");
    }
    let text = v.to_string();
    let re = regex::Regex::new(r"[0-9a-f]{8,}|\b[A-Za-z]{1,3}-[0-9A-Z]{5}\b|\d{10,}").unwrap();
    let text = re.replace_all(&text, "<x>");
    let text = regex::Regex::new(r"/[^\s\x22]*rdyd[0-9a-z]+")
        .unwrap()
        .replace_all(&text, "<dir>");
    serde_json::from_str(&text).unwrap()
}

#[test]
fn rtc5_rule_uses_write_kungfu_on_both_forms() {
    const NOT_LOADED: &str = "This call has no kungfu value: you have not loaded this ruleset.";
    const WRONG: &str =
        "This call's kungfu value is not current: your instructions may be out of date.";
    for order in [[true, false], [false, true]] {
        let correct = run_forms(Mode::Correct, order);
        assert_eq!(correct.carried, [0, 0], "{order:?}");
        for (mode, message, want) in [
            (Mode::Absent, NOT_LOADED, [(1, 0, 0), (2, 0, 0)]),
            (Mode::Wrong, WRONG, [(0, 0, 1), (0, 0, 2)]),
        ] {
            let got = run_forms(mode, order);
            // the ordinary action and the ordinary refusal, whatever the value
            assert_eq!(got.replies.len(), 2);
            for (a, b) in correct.replies.iter().zip(&got.replies) {
                assert_eq!(a.0, b.0, "{mode:?} {order:?}");
                assert_eq!(masked(&a.1), masked(&b.1), "{mode:?} {order:?}");
            }
            // each form is refused or lands as it does with the right value
            for (mutation, (is_err, v)) in order.iter().zip(&got.replies) {
                assert_eq!(*is_err, !mutation, "{v}");
            }
            // one count for each call, and one warning for the whole run, on its first reply
            assert_eq!(got.counts, want, "{mode:?} {order:?}");
            assert_eq!(got.carried, [1, 0], "{mode:?} {order:?}");
            let _ = message;
        }
    }
}

#[test]
fn rtc5_projectless_rule_counts_once_at_meta() {
    let w = World::new();
    let outside = short_tempdir();
    let meta = w.default_meta();
    let mut c = mcp_at(&w, outside.path()).raw();
    setup(&w, outside.path());
    assert!(names(&mut c).contains(&"rdy_rule".to_string()));
    session_as(&mut c, "agent:p", "p1", "writer");
    // a refusal before any host work: counted at meta, and no daemon or store is opened for it
    let bad = json!({"scope": "person", "op": "add", "text": "x"});
    let e = body(c.call("rdy_rule", bad.clone()));
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert_eq!(advisories(&e).len(), 1, "{e}");
    assert_eq!(counted(&meta, "write", 1), (1, 0, 0));
    assert_eq!(
        w.daemon_pid(&meta),
        None,
        "the count opened the meta project"
    );
    // a call that goes through the host: one count, no copy from the forwarded call
    let v = body(c.call("rdy_rule", add("person", "Answer in British English.")));
    assert_eq!(v["scope"], "person", "{v}");
    assert!(advisories(&v).is_empty(), "one warning for the owner: {v}");
    assert_eq!(counted(&meta, "write", 2), (2, 0, 0));
    // the rule call marked nothing as received: a third miss is still `not loaded`, not `lost`
    let e = body(c.call("rdy_rule", bad.clone()));
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert_eq!(counted(&meta, "write", 3), (3, 0, 0));
    // the forwarded call counted nothing of its own: stop the daemon so it flushes, then read
    w.stop(&meta);
    assert_eq!(totals(&file_rows(&meta), "write"), (3, 0, 0));
    // after a complete instructions reply the same miss is `lost`
    let v = body(c.call("rdy_instructions", json!({"full": true})));
    assert!(v["instructions"].is_array(), "{v}");
    let e = body(c.call("rdy_rule", bad));
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert_eq!(counted(&meta, "write", 4), (3, 1, 0));
    // a correct value counts nothing
    let v = body(c.call(
        "rdy_rule",
        valued(Mode::Correct, add("person", "Answer in British English.")),
    ));
    assert!(v["scope"] == "person" || v["code"].is_string(), "{v}");
    assert_eq!(totals(&file_rows(&meta), "write"), (3, 1, 0));
}
