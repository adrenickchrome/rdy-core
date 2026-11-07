//! ain5 (D-8X04F, D-YQPB6, D-A8N0E): how `rdy mcp` and `rdy instructions` start with no
//! project. With no project and no working-copy pointer, the meta project is the project and
//! every tool acts on it; with no meta project either, a projectless server answers the session
//! claim and `rdy_instructions` (an empty reply and the creation hint) and refuses the rest with
//! `E_NO_PROJECT`. An explicit project, a damaged pointer and a partial project are their own
//! refusals and never fall back to meta. The last pin is the acceptance of the whole path: an
//! agent authors both sources over MCP, a person approves, and after a restart a new reader and
//! the CLI get the same rules. Every person here lives in an isolated home.
use rdy_engine::instructions::create_hint;
use rdy_qa::daemon::short_tempdir;
use rdy_qa::fixtures::output_within;
use rdy_qa::instructions::{REMINDER, Set, World, author};
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

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

const BUDGET: Duration = Duration::from_secs(120);

/// The environment of a process of `w` started with no project, in `cwd`.
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

/// `rdy <args>` in `cwd`, its stdin closed.
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

/// `rdy mcp <extra>` in `cwd`, with no `--project`, its every line kept.
fn mcp_at(w: &World, cwd: &Path, extra: &[&str]) -> McpClient {
    let mut args = vec!["mcp"];
    args.extend_from_slice(extra);
    let mut cmd = command(w, cwd, &args);
    cmd.stderr(Stdio::piped());
    McpClient::launch(cmd)
}

fn session(c: &mut McpClient, role: &str) {
    session_as(c, "a1", role);
}

fn session_as(c: &mut McpClient, run: &str, role: &str) {
    let r = c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": run, "role": role}),
    );
    assert!(r.is_ok(), "{r:?}");
}

fn col(v: &Value, key: &str, field: &str) -> Vec<String> {
    v[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} in {v}"))
        .iter()
        .map(|r| r[field].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The built-in menu, in order (design section 5): the same ids on every reply.
const BUILTIN_MENU: [&str; 12] = [
    "rdy:bootstrap",
    "rdy:write",
    "rdy:after-write",
    "rdy:approvals",
    "rdy:as-built",
    "rdy:read",
    "rdy:publish",
    "rdy:review",
    "rdy:diagrams",
    "rdy:parallel",
    "rdy:project-ops",
    "rdy:troubleshoot",
];

/// The rows of `key` that a project or a person wrote: the built-in layer is checked apart.
fn authored(v: &Value, key: &str) -> Vec<Value> {
    v[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} in {v}"))
        .iter()
        .filter(|r| r["layer"] != "rdy")
        .cloned()
        .collect()
}

/// [`col`] over the authored rows alone.
fn authored_col(v: &Value, key: &str, field: &str) -> Vec<String> {
    authored(v, key)
        .iter()
        .map(|r| r[field].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Every reply carries the built-in bootstrap whole and the built-in menu, before every other
/// layer, and nothing else of the built-in layer.
fn assert_builtins_first(v: &Value) {
    let rows = col(v, "instructions", "id");
    assert_eq!(
        rows[..3],
        [
            "rdy:bootstrap:before",
            "rdy:bootstrap:approval",
            "rdy:bootstrap:after"
        ],
        "{v}"
    );
    assert_eq!(col(v, "instructions", "layer")[..3], ["rdy", "rdy", "rdy"]);
    assert_eq!(
        col(v, "menu", "id")[..BUILTIN_MENU.len()],
        BUILTIN_MENU,
        "{v}"
    );
    assert!(
        v["menu"].as_array().unwrap()[..BUILTIN_MENU.len()]
            .iter()
            .all(|m| m["layer"] == "rdy"),
        "{v}"
    );
    assert_eq!(
        v["instructions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["layer"] == "rdy")
            .count(),
        3,
        "{v}"
    );
}

/// Every stdout line is one JSON-RPC message: no warning or progress text reaches it.
fn protocol_only(c: McpClient) -> String {
    let t = c.finish(BUDGET);
    for line in t.stdout.lines() {
        let v: Value = serde_json::from_str(line).unwrap_or_else(|_| panic!("not JSON: {line}"));
        assert_eq!(v["jsonrpc"], "2.0", "{line}");
    }
    t.stderr
}

#[test]
fn ain5_mcp_outside_a_project_uses_meta_for_all_tools() {
    let w = World::new();
    let meta = w.default_meta();
    w.project_at(&meta);
    let ma = author(&meta, "One spelling in every reply", META_SETS);
    // a directory outside every project, and one under the home (whose .rdy holds meta)
    let outside = short_tempdir();
    let under_home = w.home.path().join("work").join("a");
    std::fs::create_dir_all(&under_home).unwrap();
    for (n, (cwd, extra)) in [
        (outside.path().to_path_buf(), vec!["--no-daemon"]),
        (under_home.clone(), vec!["--no-daemon"]),
        (outside.path().to_path_buf(), vec![]),
        (under_home.clone(), vec![]),
    ]
    .into_iter()
    .enumerate()
    {
        let mut c = mcp_at(&w, &cwd, &extra);
        // the ordinary project server: every tool, acting on meta
        let names: Vec<String> = c
            .tools()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.len() > 2, "{names:?}");
        // a run of its own: meta's daemon keeps an earlier run's read bit
        session_as(&mut c, &format!("r{n}"), "reader");
        let ov = c.call("rdy_overview", json!({})).ok();
        assert_eq!(ov["reminder"], REMINDER, "{ov}");
        let read = c.call("rdy_read", json!({"id": ma.docs[0]})).ok();
        assert!(
            read.to_string().contains("Answer in British English."),
            "{read}"
        );
        // meta is read once, as the person layer
        let v = c.call("rdy_instructions", json!({})).ok();
        assert_builtins_first(&v);
        assert_eq!(
            authored_col(&v, "instructions", "id"),
            [ma.rules[0][0].clone()]
        );
        assert_eq!(authored_col(&v, "instructions", "layer"), ["person"]);
        assert_eq!(
            authored_col(&v, "menu", "id"),
            [
                format!("person:{}", ma.docs[0]),
                format!("person:{}", ma.docs[1])
            ]
        );
        assert_eq!(
            v["hint"],
            "This session is on your own rules project. Use it for your rules. For other work, run rdy init in a folder and reconnect.",
            "{v}"
        );
        // the CLI from the same directory gives the same reply
        let mut args = vec!["--json"];
        args.extend_from_slice(&extra);
        args.push("instructions");
        let cli = rdy_at(&w, &cwd, &args);
        // with --no-daemon, the MCP server holds meta's store; the CLI then waits for it
        if extra.is_empty() {
            assert!(
                cli.status.success(),
                "{}",
                String::from_utf8_lossy(&cli.stderr)
            );
            assert_eq!(json_of(&cli), v, "{cwd:?}");
        }
        let stderr = protocol_only(c);
        assert!(!stderr.contains("panicked"), "{stderr}");
        if !extra.is_empty() {
            let cli = rdy_at(&w, &cwd, &args);
            assert!(
                cli.status.success(),
                "{}",
                String::from_utf8_lossy(&cli.stderr)
            );
            assert_eq!(json_of(&cli), v, "{cwd:?}");
        }
    }
    assert!(
        w.daemon_pid(&meta).is_some(),
        "meta's daemon served the proxy"
    );
    // other commands keep their discovery: no meta fallback for them
    let v = json_of(&rdy_at(&w, outside.path(), &["--json", "status"]));
    assert_eq!(v["code"], "E_NO_PROJECT", "{v}");
}

#[test]
fn ain5_missing_meta_startup_returns_the_creation_hint() {
    let w = World::new();
    let outside = short_tempdir();
    for extra in [vec![], vec!["--no-daemon"]] {
        let mut c = mcp_at(&w, outside.path(), &extra);
        let mut names: Vec<String> = c
            .tools()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        names.sort();
        assert_eq!(names, ["rdy_instructions", "rdy_session"]);
        session(&mut c, "writer");
        let v = c.call("rdy_instructions", json!({})).ok();
        let hint = format!(
            "Run rdy setup to create your own rules project at {}. Tell the person when setup finishes. Reconnect this MCP server after setup.",
            w.default_meta().display()
        );
        assert_eq!(
            hint,
            format!(
                "{} Reconnect this MCP server after setup.",
                create_hint(&w.default_meta())
            )
        );
        assert_builtins_first(&v);
        assert_eq!(
            v["menu"].as_array().unwrap().len(),
            BUILTIN_MENU.len(),
            "{v}"
        );
        assert_eq!(v["instructions"].as_array().unwrap().len(), 3, "{v}");
        assert_eq!(v["hint"], hint, "{v}");
        match c.call("rdy_overview", json!({})) {
            Reply::ToolError(e) => assert_eq!(e["code"], "E_NO_PROJECT", "{e}"),
            other => panic!("expected E_NO_PROJECT, got {other:?}"),
        }
        let stderr = protocol_only(c);
        assert!(!stderr.contains("panicked"), "{stderr}");
        // the CLI says the same, and its text names the hint
        let mut args = vec!["--json"];
        args.extend_from_slice(&extra);
        args.push("instructions");
        assert_eq!(json_of(&rdy_at(&w, outside.path(), &args)), v);
        let text = rdy_at(&w, outside.path(), &["instructions"]);
        assert!(text.status.success());
        assert!(
            String::from_utf8_lossy(&text.stdout).contains(&hint),
            "{}",
            String::from_utf8_lossy(&text.stdout)
        );
    }
    // reading made no project: neither meta nor a store in the directory
    assert!(!w.default_meta().exists());
    assert!(!w.home.path().join(".rdy").exists());
    assert!(!outside.path().join(".rdy").exists());
    // an override that names no project: the hint names that path, never the default
    let elsewhere = short_tempdir();
    let named = elsewhere.path().join("rules");
    w.person(&named);
    let v = json_of(&rdy_at(&w, outside.path(), &["--json", "instructions"]));
    assert_eq!(
        v["hint"],
        format!(
            "Run rdy setup to create your own rules project at {}. Tell the person when setup finishes. Reconnect this MCP server after setup.",
            named.display()
        ),
        "{v}"
    );
}

/// agi4: a projectless server stays projectless after `rdy setup` made the rules project. Its
/// instructions call sees the new source and says to reconnect; `rdy_rule` joins the tools, and
/// every other project tool still refuses.
/// A new connection from the same place selects the rules project and serves every tool.
#[test]
fn agi4_setup_requires_reconnect_for_the_old_projectless_server() {
    let w = World::new();
    let outside = short_tempdir();
    let missing = format!(
        "Run rdy setup to create your own rules project at {}. Tell the person when setup finishes. Reconnect this MCP server after setup.",
        w.default_meta().display()
    );
    let ready = "Your own rules project is ready. Reconnect this MCP server to use it. For other work, run rdy init in a folder and reconnect.";
    let meta_session = "This session is on your own rules project. Use it for your rules. For other work, run rdy init in a folder and reconnect.";
    for extra in [vec![], vec!["--no-daemon"]] {
        // before setup: the two tools and the creation hint
        let mut old = mcp_at(&w, outside.path(), &extra);
        let names = |c: &mut McpClient| {
            let mut n: Vec<String> = c
                .tools()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_string())
                .collect();
            n.sort();
            n
        };
        assert_eq!(names(&mut old), ["rdy_instructions", "rdy_session"]);
        session(&mut old, "reader");
        let v = old.call("rdy_instructions", json!({})).ok();
        assert_builtins_first(&v);
        assert_eq!(v["hint"], missing, "{v}");
        // setup runs on the person's side
        let out = rdy_at(&w, outside.path(), &["--json", "setup"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = json_of(&out);
        assert_eq!(s["meta"]["result"], "created", "{s}");
        // the same connection: the new source is seen, the hint says to reconnect, and the
        // person's rule tool joins the two (rtc5); every other project tool still needs a
        // new connection
        let v = old.call("rdy_instructions", json!({})).ok();
        assert_builtins_first(&v);
        assert_eq!(v["hint"], ready, "{v}");
        assert!(authored(&v, "instructions").is_empty(), "{v}");
        assert_eq!(
            names(&mut old),
            ["rdy_instructions", "rdy_rule", "rdy_session"]
        );
        match old.call("rdy_overview", json!({})) {
            Reply::ToolError(e) => assert_eq!(e["code"], "E_NO_PROJECT", "{e}"),
            other => panic!("expected E_NO_PROJECT, got {other:?}"),
        }
        let stderr = protocol_only(old);
        assert!(!stderr.contains("panicked"), "{stderr}");
        // a fresh connection selects the rules project and serves its normal tools
        let mut fresh = mcp_at(&w, outside.path(), &extra);
        assert!(names(&mut fresh).len() > 2, "{:?}", names(&mut fresh));
        session_as(&mut fresh, "r2", "reader");
        let v = fresh.call("rdy_instructions", json!({})).ok();
        assert_builtins_first(&v);
        assert_eq!(v["hint"], meta_session, "{v}");
        assert!(fresh.call("rdy_overview", json!({})).is_ok());
        let stderr = protocol_only(fresh);
        assert!(!stderr.contains("panicked"), "{stderr}");
        // the next round starts again with no rules project
        w.stop(&w.default_meta());
        std::fs::remove_dir_all(w.default_meta()).unwrap();
    }
}

#[test]
fn ain5_explicit_projects_and_broken_pointers_never_fall_back() {
    let w = World::new();
    let meta = w.default_meta();
    w.project_at(&meta);
    author(&meta, "One spelling in every reply", META_SETS);
    let cwd = short_tempdir();
    let none = short_tempdir();
    let none_s = none.path().to_str().unwrap();
    let missing = format!("{} is not an rdy project", none.path().display());
    // an explicit project that is none
    let v = json_of(&rdy_at(
        &w,
        cwd.path(),
        &["--json", "--project", none_s, "instructions"],
    ));
    assert_eq!(
        (v["code"].as_str(), v["message"].as_str()),
        (Some("E_NO_PROJECT"), Some(missing.as_str())),
        "{v}"
    );
    let out = rdy_at(&w, cwd.path(), &["--project", none_s, "mcp"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "no protocol line");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(&missing),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // a damaged working-copy pointer
    let broken = short_tempdir();
    std::fs::write(broken.path().join(".rdy-worktree"), "not: [a pointer").unwrap();
    let inner = broken.path().join("src");
    std::fs::create_dir_all(&inner).unwrap();
    let v = json_of(&rdy_at(&w, &inner, &["--json", "instructions"]));
    assert_eq!(v["code"], "E_BAD_INPUT", "{v}");
    assert!(
        v["message"]
            .as_str()
            .unwrap()
            .contains("does not name a worktree"),
        "{v}"
    );
    let out = rdy_at(&w, &inner, &["mcp"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    // a partial project: a project config and no store, seen from a nested directory
    let partial = short_tempdir();
    std::fs::write(partial.path().join(".rdy.yaml"), "pack: rdy-standard\n").unwrap();
    let deep = partial.path().join("x").join("y");
    std::fs::create_dir_all(&deep).unwrap();
    let v = json_of(&rdy_at(&w, &deep, &["--json", "instructions"]));
    assert_eq!(v["code"], "E_NO_PROJECT", "{v}");
    assert_eq!(
        v["message"].as_str().unwrap(),
        format!(
            "{} has .rdy.yaml but no store",
            partial.path().canonicalize().unwrap().display()
        )
    );
    let out = rdy_at(&w, &deep, &["mcp"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    // a project's .rdy with no config is partial too
    let bare = short_tempdir();
    std::fs::create_dir_all(bare.path().join(".rdy").join("store")).unwrap();
    let v = json_of(&rdy_at(&w, bare.path(), &["--json", "instructions"]));
    assert_eq!(v["code"], "E_NO_PROJECT", "{v}");
    // a broken meta project is its own refusal, never an empty person layer
    let damaged = short_tempdir();
    std::fs::write(damaged.path().join(".rdy.yaml"), "pack: rdy-standard\n").unwrap();
    w.person(damaged.path());
    let v = json_of(&rdy_at(&w, cwd.path(), &["--json", "instructions"]));
    assert_eq!(v["code"], "E_NO_PROJECT", "{v}");
    let out = rdy_at(&w, cwd.path(), &["mcp"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
}

/// One agent call that must succeed.
fn ok(c: &mut McpClient, tool: &str, args: Value) -> Value {
    match c.call(tool, args.clone()) {
        Reply::Ok(v) => v,
        other => panic!("{tool} {args}: {other:?}"),
    }
}

/// Over MCP as an agent: a reason, a required ruleset with a lead and two instructions (the
/// second of two paragraphs), and an optional ruleset, proposed with a summary. Returns the
/// proposal's ref, the docs and the instructions.
fn author_over_mcp(c: &mut McpClient, tag: &str) -> (String, Vec<String>, Vec<String>) {
    session(c, "writer");
    let w = ok(
        c,
        "rdy_write",
        json!({"ops": [
            {"op": "create", "type": "doc", "id": "$r", "fields": {"name": format!("{tag}-reasons"), "title": "Reasons", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$r", "fields": {"title": "Why"}}
        ], "editorial": {"class": "restructure", "reason": "a home for the reasons"}}),
    );
    let section = w["created"]["$s"].as_str().unwrap().to_string();
    let d = ok(
        c,
        "rdy_decide",
        json!({"parent": section, "statement": format!("{tag}: small changes keep review cheap"),
            "title": "Keep changes small"}),
    );
    let decision = d["created"]["$decision"].as_str().unwrap().to_string();
    let last = format!("{tag} sentinel: the last paragraph arrives.");
    let rules = ok(
        c,
        "rdy_write",
        json!({"ops": [
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": format!("{tag}-baseline"), "title": "Baseline", "doc_type": "page", "required": true}},
            {"op": "create", "type": "prose", "parent": "$b", "body": "Read these before any task."},
            {"op": "create", "type": "instruction", "id": "$i1", "parent": "$b", "body": format!("{tag}: write small changes."),
                "fields": {"applies_to": ["act:write"]}, "links": {"decided_by": [{"target": decision}]}},
            {"op": "create", "type": "instruction", "id": "$i2", "parent": "$b", "body": format!("{tag}: keep one idea per change.\n\n{last}"),
                "fields": {"applies_to": ["all"]}, "links": {"decided_by": [{"target": decision}]}},
            {"op": "create", "type": "doc", "id": "$o", "fields": {"name": format!("{tag}-testing"), "title": "Testing", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$o", "body": "Load this for test work."},
            {"op": "create", "type": "instruction", "id": "$i3", "parent": "$o", "body": format!("{tag}: run the tests first."),
                "fields": {"applies_to": ["type:claim"]}, "links": {"decided_by": [{"target": decision}]}}
        ], "implements": decision}),
    );
    let id = |k: &str| rules["created"][k].as_str().unwrap().to_string();
    let docs = vec![id("$b"), id("$o")];
    let instructions = vec![id("$i1"), id("$i2"), id("$i3")];
    // an unlanded rule is not delivered (D-A8N0E: canon only)
    let v = ok(c, "rdy_instructions", json!({}));
    assert!(
        !v.to_string().contains(&last),
        "a view's rule was delivered: {v}"
    );
    let p = ok(
        c,
        "rdy_propose",
        json!({"title": format!("{tag} rules"), "summary": format!("Adds the {tag} baseline and testing rules and their reason.")}),
    );
    let r = p["ref"]
        .as_str()
        .or_else(|| p["proposal"]["ref"].as_str())
        .unwrap_or_else(|| panic!("a ref in {p}"))
        .to_string();
    (r, docs, instructions)
}

/// The person approves `r` in `dir` as themselves.
fn person_approves(w: &World, dir: &Path, r: &str) {
    let out = w.rdy(dir, &["approve", r]);
    assert!(
        out.status.success(),
        "rdy approve {r}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn ain5_full_chat_authoring_and_loading_survive_restart() {
    let w = World::new();
    let meta = w.default_meta();
    w.project_at(&meta);
    let pd = w.project();
    let pd2 = w.project();
    let outside = short_tempdir();
    // the person layer, authored by an agent outside every project: meta is the project
    let mut c = mcp_at(&w, outside.path(), &[]);
    let (mr, mdocs, mrules) = author_over_mcp(&mut c, "meta");
    drop(c);
    person_approves(&w, &meta, &mr);
    // the project layer, authored in the project
    let mut c = w.mcp(pd.path());
    let (pr, pdocs, prules) = author_over_mcp(&mut c, "proj");
    drop(c);
    person_approves(&w, pd.path(), &pr);
    // a restart of both daemons: nothing of the read path lives in memory
    w.stop(pd.path());
    w.stop(&meta);
    let nested = pd.path().join("src").join("deep");
    std::fs::create_dir_all(&nested).unwrap();
    let pid = |d: &str| format!("project:{d}");
    let qid = |d: &str| format!("person:{d}");
    for extra in [vec![], vec!["--no-daemon"]] {
        let mut c = mcp_at(&w, &nested, &extra);
        session(&mut c, "reader");
        assert_eq!(ok(&mut c, "rdy_overview", json!({}))["reminder"], REMINDER);
        let v = ok(&mut c, "rdy_instructions", json!({}));
        assert_builtins_first(&v);
        assert_eq!(
            authored_col(&v, "instructions", "id"),
            [
                prules[0].clone(),
                prules[1].clone(),
                mrules[0].clone(),
                mrules[1].clone()
            ]
        );
        assert_eq!(
            authored_col(&v, "instructions", "layer"),
            ["project", "project", "person", "person"]
        );
        assert!(
            authored(&v, "instructions")[1]["text"]
                .as_str()
                .unwrap()
                .ends_with("proj sentinel: the last paragraph arrives."),
            "{v}"
        );
        assert!(
            authored(&v, "instructions")[3]["text"]
                .as_str()
                .unwrap()
                .ends_with("meta sentinel: the last paragraph arrives."),
            "{v}"
        );
        assert_eq!(
            authored(&v, "instructions")[0]["reason"][0]["statement"],
            "proj: small changes keep review cheap"
        );
        assert_eq!(
            authored(&v, "instructions")[2]["reason"][0]["statement"],
            "meta: small changes keep review cheap"
        );
        assert_eq!(
            authored_col(&v, "menu", "id"),
            [
                pid(&pdocs[0]),
                pid(&pdocs[1]),
                qid(&mdocs[0]),
                qid(&mdocs[1])
            ]
        );
        assert_eq!(
            authored_col(&v, "menu", "when")[1],
            "Load this for test work."
        );
        // load the optional sets
        let loaded = ok(
            &mut c,
            "rdy_instructions",
            json!({"load": [pid(&pdocs[1]), qid(&mdocs[1])]}),
        );
        assert_eq!(authored(&loaded, "instructions").len(), 6);
        assert_builtins_first(&loaded);
        assert!(
            ok(&mut c, "rdy_overview", json!({}))
                .get("reminder")
                .is_none()
        );
        drop(c);
        // the CLI, from the nested directory, gives the same replies
        let mut args = vec!["--json"];
        args.extend_from_slice(&extra);
        args.push("instructions");
        let cli = rdy_at(&w, &nested, &args);
        assert!(
            cli.status.success(),
            "{}",
            String::from_utf8_lossy(&cli.stderr)
        );
        assert_eq!(json_of(&cli), v);
        let (l1, l2) = (pid(&pdocs[1]), qid(&mdocs[1]));
        args.extend(["--load", l1.as_str(), "--load", l2.as_str()]);
        let cli = rdy_at(&w, &nested, &args);
        assert_eq!(json_of(&cli), loaded);
        if extra.is_empty() {
            w.stop(pd.path());
            w.stop(&meta);
        }
    }
    // another project: its own rules, and the same person layer
    let mut c = w.mcp(pd2.path());
    session(&mut c, "reader");
    let v = ok(&mut c, "rdy_instructions", json!({}));
    assert_builtins_first(&v);
    assert_eq!(
        authored_col(&v, "instructions", "id"),
        [mrules[0].clone(), mrules[1].clone()]
    );
    assert_eq!(
        authored_col(&v, "instructions", "layer"),
        ["person", "person"]
    );
    drop(c);
    // meta under another spelling of the same directory (a native alias: a symbolic link, or a
    // junction where a link needs a privilege) is read once
    let alias: PathBuf = w.home.path().join("meta-link");
    rdy_qa::platform::dir_alias(&meta, &alias).unwrap();
    w.person(&alias);
    let mut c = w.mcp(&meta);
    session(&mut c, "reader");
    let v = ok(&mut c, "rdy_instructions", json!({}));
    assert_eq!(
        authored_col(&v, "instructions", "layer"),
        ["person", "person"]
    );
    let cli = rdy_at(&w, &meta, &["--json", "instructions"]);
    assert_eq!(json_of(&cli), v);
}
