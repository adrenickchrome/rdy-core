//! m4a4 (FW-HOOK D-NNQ0A, D-GHR0Y; FW-L3 D-54CKQ refined by D-DC5D1; D-Y18E1; D-6M0AF, D-44NVC):
//! `rdy_hook` over MCP. It runs the engine request the daemon's `hook` op and `rdy hook` run, on
//! the same host: a populated check or install gives the same reply on both, and the session's
//! project fixes the installation boundary. Any session may check; install and remove need a
//! writer and a host with local hooks. A missing, wrong or right `kungfu` value changes no
//! action. Stdio and HTTP keep their claims, and a projectless server lists no hook tool.
//!
//! Every process has a home and a user config of its own ([`World`]), and every Git run has no
//! system or global configuration of the machine.
use rdy_engine::engine::{Engine, Local};
use rdy_engine::hooks::{
    FileResult, HookFile, HookHost, HookInstall, HookKind, HookRequest, HookTarget, RunContext,
};
use rdy_engine::kungfu;
use rdy_engine::{Rdy, RdyError};
use rdy_mcp::http::HttpMcp;
use rdy_mcp::notify::NotifyHub;
use rdy_mcp::tools::{self, McpState};
use rdy_qa::daemon::short_tempdir;
use rdy_qa::fixtures::project;
use rdy_qa::git_repo::GitRepo;
use rdy_qa::instructions::{Dir, World};
use rdy_qa::mcp::{McpClient, Reply, fixture_value};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const NO_CITATION: &str =
    "This commit touches code anchored to a decision and has no Implements trailer.";
const DIRECT_EDIT: &str = "An agent edited rdy working-copy files directly.";
const HOST_ABSENT: &str = "This host cannot install local hooks.";
const HOST_ABSENT_FIX: &str = "Use rdy setup on the machine that holds the checkout.";
const NO_SESSION_FIX: &str = "state rdy/actor and rdy/run in _meta, or call rdy_session";

const NOT_LOADED: &str = "This call has no kungfu value: you have not loaded this ruleset.";
const LOST: &str = "This call has no kungfu value: you may have lost your instructions.";
const WRONG: &str =
    "This call's kungfu value is not current: your instructions may be out of date.";
const STALE: &str = "kf-00000000";

/// Lands a doc with one section holding one decision in the project at `dir`; returns the
/// decision's id.
fn seed_decision(dir: &Path) -> String {
    use rdy_qa::fixtures::{land, write, writer};
    let mut r = Rdy::open(dir).expect("open the project");
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}}
        ]),
    )
    .expect("seed the doc");
    let d = r
        .decide(
            &writer(),
            "work",
            rdy_engine::DecisionInput {
                parent: w.created["$s"].clone(),
                statement: "Anchored code cites its decision".into(),
                ..Default::default()
            },
        )
        .expect("decide");
    let decision = d.created["$decision"].clone();
    land(&mut r, "work").expect("land the seed");
    r.close().expect("close");
    decision
}

/// A project of a [`World`] with a landed decision and one linked code repo whose history
/// anchors `src/anchored.rs` to it, and leaves `src/plain.rs` unanchored. The repo names its own
/// `.git/hooks` as its hooks folder, so no machine-wide `core.hooksPath` applies.
struct Hooked {
    w: World,
    project: Dir,
    _app: GitRepo,
    scratch: tempfile::TempDir,
    /// The canonical root of the checkout.
    root: PathBuf,
    /// The name the project links the checkout under.
    name: String,
    head: String,
}

impl Hooked {
    fn new() -> Hooked {
        let w = World::new();
        let project = w.project();
        let decision = seed_decision(project.path());
        let app = GitRepo::new();
        app.write("src/plain.rs", "fn plain() {}\n");
        app.commit("feat: plain");
        app.write("src/anchored.rs", "fn anchored() {}\n");
        let head = app.commit(&format!("feat: anchored\n\nImplements: {decision}"));
        let root = std::fs::canonicalize(app.path()).unwrap();
        let hooks = root.join(".git").join("hooks");
        app.run(&["config", "core.hooksPath", &hooks.display().to_string()]);
        app.link(project.path());
        Hooked {
            w,
            project,
            name: app.configured(),
            _app: app,
            scratch: short_tempdir(),
            root,
            head,
        }
    }

    fn commit_msg(&self) -> PathBuf {
        self.root.join(".git").join("hooks").join("commit-msg")
    }

    fn settings(&self) -> PathBuf {
        std::fs::canonicalize(self.project.path())
            .unwrap()
            .join(".claude")
            .join("settings.local.json")
    }

    fn code_check(&self, paths: &[&str], message: &str) -> Value {
        json!({"op": "check", "kind": "code", "repo": self.name,
            "checkout": self.root.display().to_string(), "head": self.head,
            "paths": paths, "message": message})
    }

    /// `rdy --json hook <op> --kind <kind> ...` as the world's human, through the daemon, with
    /// `form`'s own fields as the options (a check's through an input file).
    fn cli(&self, form: &Value) -> Value {
        let op = form["op"].as_str().unwrap();
        let kind = form["kind"].as_str().unwrap();
        let mut args: Vec<String> = ["--json", "hook", op, "--kind", kind]
            .map(String::from)
            .to_vec();
        if op == "check" {
            let mut body = form.clone();
            let map = body.as_object_mut().unwrap();
            map.remove("op");
            map.remove("kind");
            let file = self.scratch.path().join(format!("in-{}.json", next()));
            std::fs::write(&file, body.to_string()).unwrap();
            args.extend(["--input".into(), file.display().to_string()]);
        } else {
            for key in ["repo", "harness", "worktree"] {
                if let Some(v) = form.get(key).and_then(Value::as_str) {
                    args.extend([format!("--{key}"), v.to_string()]);
                }
            }
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.w.rdy(self.project.path(), &refs);
        assert!(
            out.status.success(),
            "rdy {refs:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("JSON on stdout")
    }

    fn writer(&self) -> McpClient {
        let mut c = self.w.mcp(self.project.path());
        let r = c.call(
            "rdy_session",
            json!({"actor": "agent:h", "run": "h1", "role": "writer"}),
        );
        assert!(r.is_ok(), "{r:?}");
        c
    }
}

fn next() -> u64 {
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

/// The reply's one error, as a triple.
fn refusal(r: Reply) -> (String, String, String) {
    match r {
        Reply::ToolError(e) => {
            let s = |k: &str| e[k].as_str().unwrap_or_default().to_string();
            (s("code"), s("message"), s("fix"))
        }
        other => panic!("expected a tool error: {other:?}"),
    }
}

fn triple(code: &str, message: &str, fix: &str) -> (String, String, String) {
    (code.into(), message.into(), fix.into())
}

/// The warnings of a hook reply, by code with their paths.
fn warnings(v: &Value) -> Vec<(String, Vec<String>)> {
    v["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("a hook reply: {v}"))
        .iter()
        .map(|w| {
            let paths = w["paths"]
                .as_array()
                .map(|p| p.iter().map(|s| s.as_str().unwrap().to_string()).collect())
                .unwrap_or_default();
            (w["code"].as_str().unwrap().to_string(), paths)
        })
        .collect()
}

fn files(v: &Value) -> Vec<(String, String)> {
    v["files"]
        .as_array()
        .unwrap_or_else(|| panic!("an install reply: {v}"))
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_string(),
                f["result"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn m4a4_checks_and_installs_use_the_same_host() {
    let h = Hooked::new();
    let mut c = h.writer();

    // a populated code check: the same reply over MCP and over the command line
    let form = h.code_check(&["src/anchored.rs", "src/plain.rs"], "feat: change");
    let via_mcp = c.call("rdy_hook", form.clone()).ok();
    assert_eq!(via_mcp["kind"], "code", "{via_mcp}");
    assert_eq!(via_mcp["checked"], true, "{via_mcp}");
    assert_eq!(
        warnings(&via_mcp),
        [(
            "W_HOOK_NO_CITATION".to_string(),
            vec!["src/anchored.rs".to_string()]
        )],
        "{via_mcp}"
    );
    assert_eq!(via_mcp["warnings"][0]["message"], NO_CITATION);
    assert_eq!(via_mcp, h.cli(&form));
    // a cited message, and a path no anchor touches, are clean
    for form in [
        h.code_check(&["src/anchored.rs"], "feat: change\n\nImplements: D-ABCDE"),
        h.code_check(&["src/plain.rs"], "feat: change"),
    ] {
        let v = c.call("rdy_hook", form.clone()).ok();
        assert_eq!((v["checked"].as_bool(), warnings(&v)), (Some(true), vec![]));
        assert_eq!(v, h.cli(&form));
    }
    // a moved HEAD is an unavailable check, never a clean one
    let mut moved = h.code_check(&["src/anchored.rs"], "feat: change");
    moved["head"] = json!("b".repeat(40));
    let v = c.call("rdy_hook", moved.clone()).ok();
    assert_eq!(v["checked"], false, "{v}");
    assert_eq!(
        warnings(&v),
        [("W_HOOK_UNAVAILABLE".to_string(), vec![])],
        "{v}"
    );
    assert_eq!(v, h.cli(&moved));

    // a direct edit of a checked-out member warns; a code file does not
    let out = h.w.rdy(h.project.path(), &["checkout"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = std::fs::canonicalize(h.project.path())
        .unwrap()
        .join("spec.yaml");
    assert!(doc.is_file());
    let edit = json!({"op": "check", "kind": "edit", "paths": [doc.display().to_string()]});
    let v = c.call("rdy_hook", edit.clone()).ok();
    assert_eq!(v["checked"], true, "{v}");
    assert_eq!(warnings(&v)[0].0, "W_HOOK_DIRECT_EDIT", "{v}");
    assert_eq!(v["warnings"][0]["message"], DIRECT_EDIT);
    assert_eq!(v, h.cli(&edit));
    let plain = json!({"op": "check", "kind": "edit", "paths": [h.root.join("src/plain.rs").display().to_string()]});
    let v = c.call("rdy_hook", plain.clone()).ok();
    assert_eq!((v["checked"].as_bool(), warnings(&v)), (Some(true), vec![]));
    assert_eq!(v, h.cli(&plain));

    // installs: written once over MCP, kept when the command line repeats it, removed, and the
    // same the other way round
    let commit_msg = h.commit_msg();
    let code = json!({"op": "install", "kind": "code", "repo": h.name});
    let at = |p: &Path| rdy_store::pio::display_path(p);
    let v = c.call("rdy_hook", code.clone()).ok();
    assert_eq!(files(&v), [(at(&commit_msg), "written".to_string())], "{v}");
    assert_eq!((v["kind"].as_str(), warnings(&v)), (Some("code"), vec![]));
    assert!(commit_msg.is_file());
    assert_eq!(
        files(&h.cli(&code)),
        [(at(&commit_msg), "kept".to_string())]
    );
    assert_eq!(
        files(&c.call("rdy_hook", code.clone()).ok()),
        [(at(&commit_msg), "kept".to_string())]
    );
    let remove = json!({"op": "remove", "kind": "code", "repo": h.name});
    assert_eq!(
        files(&c.call("rdy_hook", remove.clone()).ok()),
        [(at(&commit_msg), "removed".to_string())]
    );
    assert!(!commit_msg.exists());
    assert_eq!(
        files(&h.cli(&code)),
        [(at(&commit_msg), "written".to_string())]
    );
    assert_eq!(
        files(&c.call("rdy_hook", remove.clone()).ok()),
        [(at(&commit_msg), "removed".to_string())]
    );

    // the session's project fixes the edit hook's boundary: its own settings, no other place
    let settings = h.settings();
    let edit_in = json!({"op": "install", "kind": "edit", "harness": "claude-code"});
    let v = c.call("rdy_hook", edit_in.clone()).ok();
    assert_eq!(files(&v), [(at(&settings), "written".to_string())], "{v}");
    assert_eq!(
        files(&h.cli(&edit_in)),
        [(at(&settings), "kept".to_string())]
    );
    let text = std::fs::read_to_string(&settings).unwrap();
    assert!(text.contains("PostToolUse"), "{text}");
    assert!(!h.w.home.path().join(".claude").exists());
    assert!(!h.w.default_meta().exists());
    let edit_out = json!({"op": "remove", "kind": "edit", "harness": "claude-code"});
    assert_eq!(
        files(&c.call("rdy_hook", edit_out).ok()),
        [(at(&settings), "removed".to_string())]
    );
    // a worktree nobody registered, and a repo nobody linked, are refused and write nothing
    for form in [
        json!({"op": "install", "kind": "edit", "harness": "claude-code", "worktree": "nowhere"}),
        json!({"op": "install", "kind": "code", "repo": "no-such-repo"}),
    ] {
        let (code, message, _) = refusal(c.call("rdy_hook", form.clone()));
        assert!(
            code == "E_BAD_INPUT" || code == "E_NOT_FOUND",
            "{form}: {code} {message}"
        );
    }
    assert!(!commit_msg.exists());
    assert!(!settings.exists());
    c.alive();
}

/// A host that records its calls and gives a fixed reply.
struct Recording {
    calls: Arc<AtomicUsize>,
    seen: Arc<Mutex<Vec<String>>>,
}

fn reply(target: &HookTarget) -> HookInstall {
    HookInstall {
        kind: target.kind(),
        files: vec![HookFile {
            path: "recorded".into(),
            result: FileResult::Written,
        }],
        warnings: vec![],
    }
}

impl HookHost for Recording {
    fn install(&self, target: &HookTarget, _ctx: &RunContext) -> Result<HookInstall, RdyError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen
            .lock()
            .unwrap()
            .push(format!("install {target:?}"));
        Ok(reply(target))
    }

    fn remove(&self, target: &HookTarget, _ctx: &RunContext) -> Result<HookInstall, RdyError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(format!("remove {target:?}"));
        Ok(reply(target))
    }
}

fn state(engine: Arc<dyn Engine>) -> McpState {
    McpState::new(engine)
}

fn open(st: &mut McpState, role: &str) {
    tools::rdy_session(
        st,
        serde_json::from_value(json!({"actor": "agent:qa", "run": role, "role": role})).unwrap(),
    )
    .expect("a session");
}

fn hook(st: &mut McpState, args: Value) -> Result<Value, RdyError> {
    tools::rdy_hook(st, serde_json::from_value(args).unwrap())
}

fn outside_edit() -> Value {
    json!({"op": "check", "kind": "edit", "paths": [
        std::env::temp_dir().join("m4a4-outside.txt").display().to_string()]})
}

#[test]
fn m4a4_reader_checks_but_cannot_install() {
    let (dir, r) = project();
    drop(r);
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(vec![]));
    let with_host: Arc<dyn Engine> = Arc::new(Local::open(dir.path()).unwrap().with_hook_host(
        Arc::new(Recording {
            calls: Arc::clone(&calls),
            seen: Arc::clone(&seen),
        }),
    ));
    let install_code = json!({"op": "install", "kind": "code", "repo": "app"});
    let remove_edit = json!({"op": "remove", "kind": "edit", "harness": "claude-code"});

    let mut st = state(Arc::clone(&with_host));
    // no session: every form is refused, and a bad form is the form's own refusal first
    for form in [outside_edit(), install_code.clone()] {
        let e = hook(&mut st, form).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            ("E_NO_SESSION", "no session", NO_SESSION_FIX)
        );
    }
    let e = hook(&mut st, json!({"op": "check"})).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert_eq!(e.message, "Invalid rdy_hook form: pass kind.");

    // a reader checks, and cannot install or remove: the host is never asked
    open(&mut st, "reader");
    let v = hook(&mut st, outside_edit()).unwrap();
    assert_eq!(v["kind"], "edit", "{v}");
    for (form, act) in [(&install_code, "install"), (&remove_edit, "remove")] {
        let e = hook(&mut st, form.clone()).unwrap_err();
        assert_eq!(e.code, "E_PERMISSION", "{act}");
        assert_eq!(e.message, format!("role Reader cannot {act} hooks"));
        assert_eq!(e.fix, "start the session as a writer");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    // a writer's install and remove reach the host, which decides the reply
    drop(st);
    let mut st = state(Arc::clone(&with_host));
    open(&mut st, "writer");
    let v = hook(&mut st, install_code.clone()).unwrap();
    assert_eq!(
        v,
        json!({"kind": "code", "files": [{"path": "recorded", "result": "written"}], "warnings": []})
    );
    let v = hook(&mut st, remove_edit.clone()).unwrap();
    assert_eq!(v["kind"], "edit", "{v}");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let seen = seen.lock().unwrap().clone();
    assert!(seen[0].starts_with("install Code"), "{seen:?}");
    assert!(seen[1].starts_with("remove Edit"), "{seen:?}");
    // a check asks no host
    hook(&mut st, outside_edit()).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // an embedding with no host: a writer's install and remove are refused alike, a check works
    drop(st);
    drop(with_host);
    let bare: Arc<dyn Engine> = Arc::new(Local::open(dir.path()).unwrap());
    let mut st = state(bare);
    open(&mut st, "writer");
    for form in [install_code, remove_edit] {
        let e = hook(&mut st, form).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            ("E_BAD_INPUT", HOST_ABSENT, HOST_ABSENT_FIX)
        );
    }
    let v = hook(&mut st, outside_edit()).unwrap();
    assert_eq!(v["kind"], "edit", "{v}");
    // the form checks the same request the engine parses
    assert!(matches!(
        HookRequest::from_value(&outside_edit()),
        Ok(HookRequest::CheckEdit(_))
    ));
    assert_eq!(HookKind::Edit.as_str(), "edit");
}

/// The `W_KUNGFU` entries of a reply's body.
fn advisories(body: &Value) -> Vec<Value> {
    body["warnings"]
        .as_array()
        .map(|all| {
            all.iter()
                .filter(|w| w["code"] == "W_KUNGFU")
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn warning(message: &str) -> Value {
    json!({"code": "W_KUNGFU", "message": message, "ruleset": "rdy:project-ops",
        "fix": "Call rdy_instructions {load: [\"rdy:project-ops\"]} and re-read it before you go on."})
}

/// A reply's body without the kungfu advisory.
fn without_advisory(body: &Value) -> Value {
    let mut out = body.clone();
    if let Some(all) = out.get_mut("warnings").and_then(Value::as_array_mut) {
        all.retain(|w| w["code"] != "W_KUNGFU");
    }
    out
}

fn body(r: Reply) -> Value {
    match r {
        Reply::Ok(v) | Reply::ToolError(v) => v,
        Reply::RpcError(e) => panic!("a protocol error: {e}"),
    }
}

#[test]
fn m4a4_kungfu_warns_without_blocking_hooks() {
    // the row: rdy:project-ops, warn
    let p = kungfu::policy("rdy_hook").expect("a row");
    assert_eq!(p.ruleset, Some("rdy:project-ops"));
    assert_eq!(p.response, kungfu::Response::Warn);
    let right = fixture_value("rdy_hook").expect("a value");

    let w = World::new();
    let pd = w.project();
    let env = w.env();
    let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut c = McpClient::start_with_env(pd.path(), &pairs).raw();
    let r = c.call(
        "rdy_session",
        json!({"actor": "agent:k", "run": "k1", "role": "writer"}),
    );
    assert!(r.is_ok(), "{r:?}");
    let load = |c: &mut McpClient| {
        let r = c.call("rdy_instructions", json!({"load": ["rdy:project-ops"]}));
        assert!(r.is_ok(), "{r:?}");
    };
    let call = |c: &mut McpClient, value: Option<Value>| {
        let mut args = outside_edit();
        if let Some(v) = value {
            args["kungfu"] = v;
        }
        body(c.call("rdy_hook", args))
    };

    // never received, received then missing, wrong, right: the action is the same each time
    let never = call(&mut c, None);
    assert_eq!(advisories(&never), [warning(NOT_LOADED)], "{never}");
    let action = without_advisory(&never);
    assert_eq!(action["kind"], "edit", "{action}");
    load(&mut c);
    let lost = call(&mut c, None);
    assert_eq!(advisories(&lost), [warning(LOST)], "{lost}");
    assert_eq!(without_advisory(&lost), action);
    for value in [
        json!(STALE),
        json!(""),
        Value::Null,
        json!(1),
        json!([right]),
        json!({"kungfu": right}),
    ] {
        load(&mut c);
        let wrong = call(&mut c, Some(value.clone()));
        assert_eq!(advisories(&wrong), [warning(WRONG)], "{value}: {wrong}");
        assert_eq!(without_advisory(&wrong), action, "{value}");
        assert!(!wrong.to_string().contains("kf-0"), "{wrong}");
    }
    load(&mut c);
    let ok = call(&mut c, Some(json!(right)));
    assert_eq!(ok.get("warnings"), action.get("warnings"), "{ok}");
    assert_eq!(ok, action);
    assert!(!ok.to_string().contains("kf-"), "{ok}");

    // a write form with the wrong value is refused by the engine's own rules, not by kungfu
    load(&mut c);
    let mut install = json!({"op": "install", "kind": "code", "repo": "nowhere"});
    install["kungfu"] = json!(STALE);
    let reply = c.call("rdy_hook", install);
    let (is_err, v) = match reply {
        Reply::ToolError(v) => (true, v),
        Reply::Ok(v) => (false, v),
        Reply::RpcError(e) => panic!("{e}"),
    };
    assert!(is_err, "{v}");
    assert_eq!(v["code"], "E_BAD_INPUT", "{v}");
    assert_eq!(advisories(&v), [warning(WRONG)], "{v}");

    // `kungfu` is stripped only at the MCP boundary: the engine's request never takes it, and
    // the schema never shows it
    let mut with_key = outside_edit();
    with_key["kungfu"] = json!(right);
    let e = HookRequest::from_value(&with_key).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert!(
        e.message
            .starts_with("Invalid rdy_hook form: unknown field \"kungfu\""),
        "{}",
        e.message
    );
    let tools = c.tools();
    let t = tools.iter().find(|t| t["name"] == "rdy_hook").unwrap();
    assert!(!t.to_string().contains("kungfu"), "{t}");
    c.alive();
}

/// The result of `call` on an in-memory HTTP server, as `(isError, body)`.
struct Http {
    mcp: HttpMcp,
    next: AtomicU64,
}

impl Http {
    async fn call(&self, tool: &str, args: Value, meta: Value) -> (bool, Value) {
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let body = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": tool, "arguments": args, "_meta": meta}});
        let p = self
            .mcp
            .post(
                &[
                    ("host", "localhost"),
                    ("accept", "application/json, text/event-stream"),
                    ("content-type", "application/json"),
                    ("mcp-protocol-version", "2025-11-25"),
                ],
                body.to_string().into_bytes(),
            )
            .await;
        assert_eq!(p.status, 200, "{p:?}");
        let reply = p.reply();
        let text = reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no tool reply in {reply}"));
        (
            reply["result"]["isError"] == json!(true),
            serde_json::from_str(text).unwrap(),
        )
    }

    async fn rpc(&self, method: &str, params: Value) -> Value {
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let p = self
            .mcp
            .post(
                &[
                    ("host", "localhost"),
                    ("accept", "application/json, text/event-stream"),
                    ("content-type", "application/json"),
                    ("mcp-protocol-version", "2025-11-25"),
                ],
                body.to_string().into_bytes(),
            )
            .await;
        assert_eq!(p.status, 200, "{p:?}");
        p.reply()
    }
}

fn claim(actor: &str, run: &str) -> Value {
    json!({"rdy/actor": actor, "rdy/run": run})
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m4a4_http_and_stdio_keep_claims_and_projectless_scope() {
    // HTTP, in process: every request states its own claim; a host the embedding gave serves
    // installs
    let (dir, r) = project();
    drop(r);
    let calls = Arc::new(AtomicUsize::new(0));
    let engine: Arc<dyn Engine> = Arc::new(Local::open(dir.path()).unwrap().with_hook_host(
        Arc::new(Recording {
            calls: Arc::clone(&calls),
            seen: Arc::new(Mutex::new(vec![])),
        }),
    ));
    let http = Http {
        mcp: HttpMcp::new(Arc::clone(&engine), NotifyHub::new()),
        next: AtomicU64::new(0),
    };
    let listed = http.rpc("tools/list", json!({})).await;
    let tool_names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(tool_names.contains(&"rdy_hook"), "{tool_names:?}");
    let none = (true, "E_NO_SESSION");
    // no claim, no session: refused whatever the form
    for form in [
        outside_edit(),
        json!({"op": "install", "kind": "code", "repo": "app"}),
    ] {
        let (is_err, v) = http.call("rdy_hook", form, json!({})).await;
        assert_eq!((is_err, v["code"].as_str().unwrap()), none, "{v}");
        assert_eq!(v["fix"], NO_SESSION_FIX);
    }
    // a reader's saved role holds on its claim, and a claim names its own run
    let (is_err, v) = http
        .call(
            "rdy_session",
            json!({"actor": "agent:r", "run": "r1", "role": "reader"}),
            json!({}),
        )
        .await;
    assert!(!is_err, "{v}");
    let reader = claim("agent:r", "r1");
    let (is_err, v) = http.call("rdy_hook", outside_edit(), reader.clone()).await;
    assert!(!is_err && v["kind"] == "edit", "{v}");
    let (is_err, v) = http
        .call(
            "rdy_hook",
            json!({"op": "install", "kind": "code", "repo": "app"}),
            reader,
        )
        .await;
    assert!(is_err, "{v}");
    assert_eq!(v["code"], "E_PERMISSION", "{v}");
    assert_eq!(v["message"], "role Reader cannot install hooks");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // another run of the same actor inherits no saved role: its claim has the default writer
    // role of a claim, so its install reaches the host
    let (is_err, v) = http
        .call(
            "rdy_hook",
            json!({"op": "install", "kind": "code", "repo": "app"}),
            claim("agent:r", "r2"),
        )
        .await;
    assert!(!is_err, "{v}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // a writer's claim reaches the host
    http.call(
        "rdy_session",
        json!({"actor": "agent:w", "run": "w1", "role": "writer"}),
        json!({}),
    )
    .await;
    let (is_err, v) = http
        .call(
            "rdy_hook",
            json!({"op": "install", "kind": "code", "repo": "app"}),
            claim("agent:w", "w1"),
        )
        .await;
    assert!(!is_err, "{v}");
    assert_eq!(
        v["files"],
        json!([{"path": "recorded", "result": "written"}])
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // stdio through the daemon, a native pipe on Windows: the same claims and the same reply
    let w = World::new();
    let pd = w.project();
    let mut served = w.mcp(pd.path());
    let (code, message, fix) = refusal(served.call("rdy_hook", outside_edit()));
    assert_eq!(
        (code.as_str(), message.as_str(), fix.as_str()),
        ("E_NO_SESSION", "no session", NO_SESSION_FIX)
    );
    let r = served.call(
        "rdy_session",
        json!({"actor": "agent:r", "run": "r1", "role": "reader"}),
    );
    assert!(r.is_ok(), "{r:?}");
    let v = served.call("rdy_hook", outside_edit()).ok();
    assert_eq!(v["kind"], "edit", "{v}");
    assert_eq!(
        refusal(served.call(
            "rdy_hook",
            json!({"op": "install", "kind": "edit", "harness": "claude-code"})
        )),
        triple(
            "E_PERMISSION",
            "role Reader cannot install hooks",
            "start the session as a writer"
        )
    );
    assert!(!pd.path().join(".claude").exists());
    assert!(names(&mut served).contains(&"rdy_hook".to_string()));

    // projectless: the same two tools as before, and no hook tool
    let outside = short_tempdir();
    let mut cmd = rdy_qa::fixtures::rdy_command();
    cmd.args(["mcp"])
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(w.env())
        .current_dir(outside.path());
    cmd.stderr(Stdio::piped());
    let mut bare = McpClient::launch(cmd);
    assert_eq!(names(&mut bare), ["rdy_instructions", "rdy_session"]);
    let r = bare.call(
        "rdy_session",
        json!({"actor": "agent:p", "run": "p1", "role": "writer"}),
    );
    assert!(r.is_ok(), "{r:?}");
    match bare.call("rdy_hook", outside_edit()) {
        Reply::ToolError(e) | Reply::Ok(e) => {
            assert_ne!(
                e["kind"], "edit",
                "a projectless server has no hook tool: {e}"
            )
        }
        Reply::RpcError(_) => {}
    }
    assert!(!outside.path().join(".rdy").exists());
}
