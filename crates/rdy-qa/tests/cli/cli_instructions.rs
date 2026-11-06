//! ain5 (D-8X04F, D-YQPB6, D-A8N0E, D-KQ4CA): the CLI's parity with `rdy_instructions`.
//! `rdy instructions [--load <id>...] [--json]` gives the reply MCP gives, through the daemon
//! and with `--no-daemon`; its text prints the whole bodies, their reasons and the menu with the
//! estimated tokens. `rdy status` and `rdy brief` carry the reminder until a complete reply goes
//! out for the session's actor and run. `rdy new instruction` writes one instruction through the
//! ordinary write, with the write's own checks. Every person here lives in an isolated home.
use rdy_qa::daemon::short_tempdir;
use rdy_qa::instructions::{REMINDER, Set, World, author};
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::path::Path;
use std::process::Stdio;

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
        rules: &[(
            &["act:review"],
            "Read the whole diff.\n\nThe last paragraph counts.",
        )],
    },
];

/// The built-in rulesets, in menu order.
const BUILTIN: [&str; 12] = [
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

/// The rows of `key` that a project or a person wrote.
fn authored(v: &Value, key: &str) -> Vec<Value> {
    v[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} in {v}"))
        .iter()
        .filter(|r| r["layer"] != "rdy")
        .cloned()
        .collect()
}

/// The rulesets the built-in rows of `v` belong to, without repeats, in reply order.
fn builtin_sets(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for r in v["instructions"].as_array().unwrap() {
        if r["layer"] == "rdy" {
            let s = r["ruleset"].as_str().unwrap().to_string();
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out
}

fn p(id: &str) -> String {
    format!("project:{id}")
}

fn q(id: &str) -> String {
    format!("person:{id}")
}

/// `rdy --json <args>` in `dir` as the world's human: the reply, which must succeed.
fn ok_json(w: &World, dir: &Path, args: &[&str]) -> Value {
    let (ok, v) = w.json(dir, args);
    assert!(ok, "rdy {args:?}: {v}");
    v
}

/// `rdy <args>` in `dir` as the world's human: stdout, which must succeed.
fn ok_text(w: &World, dir: &Path, args: &[&str]) -> String {
    let out = w.rdy(dir, args);
    assert!(
        out.status.success(),
        "rdy {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn ain5_cli_json_and_mcp_return_the_same_rules_and_menu() {
    let w = World::new();
    let (pd, md) = (w.project(), w.project());
    let pa = author(pd.path(), "Small changes keep review cheap", PROJECT_SETS);
    let ma = author(md.path(), "One spelling in every reply", META_SETS);
    w.person(md.path());
    // the reminder rides on status and brief until the human's own instructions call
    let st = ok_json(&w, pd.path(), &["status"]);
    assert_eq!(st["reminder"], REMINDER, "{st}");
    let br = ok_json(&w, pd.path(), &["brief", &pa.docs[0]]);
    assert_eq!(br["reminder"], REMINDER, "{br}");
    assert!(
        ok_text(&w, pd.path(), &["status"])
            .lines()
            .any(|l| l == REMINDER)
    );
    // MCP's reply, then the CLI's: the same JSON, unchanged
    let mut c = w.mcp(pd.path());
    assert!(
        c.call(
            "rdy_session",
            json!({"actor": "agent:a", "run": "a1", "role": "reader"})
        )
        .is_ok()
    );
    let mcp = c.call("rdy_instructions", json!({})).ok();
    // the built-in bootstrap comes first and whole, then the project's rules, then the person's
    assert_eq!(builtin_sets(&mcp), ["rdy:bootstrap"], "{mcp}");
    assert_eq!(
        mcp["menu"]
            .as_array()
            .unwrap()
            .iter()
            .take(BUILTIN.len())
            .map(|m| m["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        BUILTIN
    );
    assert_eq!(
        authored(&mcp, "instructions")
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        [
            pa.rules[0][0].as_str(),
            pa.rules[0][1].as_str(),
            ma.rules[0][0].as_str()
        ]
        .map(String::from)
    );
    let cli = ok_json(&w, pd.path(), &["instructions"]);
    assert_eq!(cli, mcp);
    // a complete reply clears the human's reminder on the daemon, and only the human's
    let st = ok_json(&w, pd.path(), &["status"]);
    assert!(st.get("reminder").is_none(), "{st}");
    let br = ok_json(&w, pd.path(), &["brief", &pa.docs[0]]);
    assert!(br.get("reminder").is_none(), "{br}");
    // --load takes one or more ids and may be repeated; bare ids load when one layer holds them
    let loaded = c
        .call(
            "rdy_instructions",
            json!({"load": [p(&pa.docs[1]), q(&ma.docs[1])]}),
        )
        .ok();
    assert_eq!(authored(&loaded, "instructions").len(), 5);
    for args in [
        vec!["instructions", "--load", &p(&pa.docs[1]), &q(&ma.docs[1])],
        vec![
            "instructions",
            "--load",
            &p(&pa.docs[1]),
            "--load",
            &q(&ma.docs[1]),
        ],
        vec!["instructions", "--load", &pa.docs[1], "--load", &ma.docs[1]],
    ]
    .iter()
    {
        assert_eq!(ok_json(&w, pd.path(), args), loaded, "{args:?}");
    }
    // an id no menu holds is the shared refusal, unchanged
    let missing = c
        .call("rdy_instructions", json!({"load": ["project:d-ZZZZZ"]}))
        .code()
        .map(String::from);
    let (ok, e) = w.json(pd.path(), &["instructions", "--load", "project:d-ZZZZZ"]);
    assert!(!ok);
    assert_eq!(e["code"].as_str().map(String::from), missing);
    assert_eq!(e["message"], "ruleset project:d-ZZZZZ was not found");
    assert_eq!(
        e["fix"],
        "call rdy_instructions and load an id from its menu"
    );
    // the text: whole bodies and their reasons, then the menu with the estimated tokens
    let t = ok_text(&w, pd.path(), &["instructions", "--load", &q(&ma.docs[1])]);
    for body in [
        "Write small changes.\n\nKeep one idea per change.",
        "Cite a decision in every claim.",
        "Answer in British English.",
        "Read the whole diff.\n\nThe last paragraph counts.",
    ] {
        assert!(t.contains(body), "{body:?} in\n{t}");
    }
    assert!(
        t.contains(&format!("{}: {}", pa.decision, pa.statement)),
        "{t}"
    );
    assert!(
        t.contains(&format!("{}: {}", ma.decision, ma.statement)),
        "{t}"
    );
    let menu = t.find("\nmenu").unwrap_or_else(|| panic!("a menu in\n{t}"));
    assert!(t[..menu].contains("The last paragraph counts."), "{t}");
    let rest = &t[menu..];
    for (id, tokens) in [
        (p(&pa.docs[1]), "Run the tests first.".len().div_ceil(4)),
        (
            q(&ma.docs[1]),
            "Read the whole diff.\n\nThe last paragraph counts."
                .len()
                .div_ceil(4),
        ),
    ] {
        assert!(
            rest.lines()
                .any(|l| l.contains(&id) && l.contains(&format!("~{tokens} tokens"))),
            "{id} with ~{tokens} tokens in\n{rest}"
        );
    }
    // with --no-daemon, the same reply: meta's running daemon answers for the person layer
    drop(c);
    w.stop(pd.path());
    assert!(w.daemon_pid(md.path()).is_some(), "meta's daemon serves it");
    let direct = ok_json(&w, pd.path(), &["--no-daemon", "instructions"]);
    assert_eq!(direct, mcp);
    // a fresh direct process starts with the reminder owed
    let st = ok_json(&w, pd.path(), &["--no-daemon", "status"]);
    assert_eq!(st["reminder"], REMINDER, "{st}");
    // and with meta's daemon stopped, the direct run opens meta's unused store itself
    w.stop(md.path());
    let direct = ok_json(&w, pd.path(), &["--no-daemon", "instructions"]);
    assert_eq!(direct, mcp);
    assert!(w.daemon_pid(md.path()).is_none(), "no daemon was started");
}

#[test]
fn ain5_config_origin_and_warnings_match_cli_and_mcp() {
    let w = World::new();
    let (pd, md) = (w.project(), w.project());
    author(pd.path(), "Small changes keep review cheap", PROJECT_SETS);
    author(md.path(), "One spelling in every reply", META_SETS);
    w.config(&format!(
        "instructions:\n  person: {}\n  persn: typo\n",
        md.path().display()
    ));
    let mut c = w.mcp(pd.path());
    assert!(
        c.call("rdy_session", json!({"actor": "agent:a", "run": "a1"}))
            .is_ok()
    );
    let mcp = c.call("rdy_instructions", json!({})).ok();
    let warnings = mcp["warnings"].as_array().cloned().unwrap_or_default();
    assert!(
        warnings.iter().any(|w| w["code"] == "W_CONFIG_UNKNOWN"
            && w["message"].as_str().unwrap_or("").contains("persn")),
        "{mcp}"
    );
    // the person layer still comes from the configured path
    assert!(
        mcp["instructions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["layer"] == "person"),
        "{mcp}"
    );
    let cli = ok_json(&w, pd.path(), &["instructions"]);
    assert_eq!(cli, mcp);
    // the text prints each warning once, in the shared form
    let out = w.rdy(pd.path(), &["instructions"]);
    assert!(out.status.success());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    for warn in &warnings {
        let line = format!(
            "warning {}: {}; {}",
            warn["code"].as_str().unwrap(),
            warn["message"].as_str().unwrap(),
            warn["fix"].as_str().unwrap()
        );
        assert_eq!(all.matches(&line).count(), 1, "{line:?} once in\n{all}");
    }
    // the origin: rdy config lists the person path from the user config
    let origin = ok_text(&w, pd.path(), &["config", "--show-origin"]);
    let target = md.path().display().to_string();
    assert!(
        origin
            .lines()
            .any(|l| l.contains("instructions.person") && l.contains(&target)),
        "{origin}"
    );
}

/// The refusal of a write: code, message and fix, each new instruction's id as `ai-?` (the
/// two writes assign their own).
fn refusal_of(v: &Value) -> (String, String, String) {
    let mask = |t: &str| {
        t.split(' ')
            .map(|w| if w.starts_with("ai-") { "ai-?" } else { w })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let s = |k: &str| mask(v[k].as_str().unwrap_or_default());
    (s("code"), s("message"), s("fix"))
}

#[test]
fn ain5_new_instruction_uses_the_same_write_and_reason_checks() {
    let w = World::new();
    let pd = w.project();
    let pa = author(pd.path(), "Small changes keep review cheap", PROJECT_SETS);
    let doc = pa.docs[1].clone();
    // one instruction: body, applies_to, locked and decided_by, through the ordinary write
    let v = ok_json(
        &w,
        pd.path(),
        &[
            "new",
            "instruction",
            "--parent",
            &doc,
            "--text",
            "Prefer table tests.\n\nName each case.",
            "--applies-to",
            "type:claim",
            "--applies-to",
            "area:nowhere",
            "--reason",
            &pa.decision,
            "--locked",
        ],
    );
    let id = v["created"]["$instruction"]
        .as_str()
        .unwrap_or_else(|| panic!("an id in {v}"))
        .to_string();
    assert_eq!(v["view"], "work", "{v}");
    // the unknown area is the write's own warning
    assert!(
        v["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|x| x["code"] == "W_INSTRUCTION_SELECTOR"),
        "{v}"
    );
    // the block as the store holds it: no key, the fields and the reason link
    let mut c = w.mcp(pd.path());
    assert!(
        c.call(
            "rdy_session",
            json!({"actor": "agent:a", "run": "a1", "role": "reader"})
        )
        .is_ok()
    );
    let b = c
        .call(
            "rdy_read",
            json!({"id": id, "scope": "view:work", "format": "yaml"}),
        )
        .ok();
    let text = b.to_string();
    assert!(text.contains("Prefer table tests."), "{b}");
    assert!(
        text.contains("type:claim") && text.contains("area:nowhere"),
        "{b}"
    );
    assert!(text.contains(&pa.decision), "{b}");
    assert!(text.contains("locked: true"), "{b}");
    assert!(!text.contains("key:"), "{b}");
    drop(c);
    // the write's refusals, as rdy_write gives them for the same create
    let mut a = w.mcp(pd.path());
    assert!(
        a.call(
            "rdy_session",
            json!({"actor": "agent:b", "run": "b1", "role": "writer"})
        )
        .is_ok()
    );
    let token = a.call("rdy_brief", json!({"ids": [doc]})).ok()["base"]["token"]
        .as_str()
        .unwrap()
        .to_string();
    let mcp_refusal = |a: &mut rdy_qa::mcp::McpClient, sels: &[&str], reason: &str| {
        let op = json!({"op": "create", "type": "instruction", "id": "$instruction",
            "parent": doc, "body": "A rule.", "fields": {"applies_to": sels},
            "links": {"decided_by": [{"target": reason}]}});
        let r = a.call(
            "rdy_write",
            json!({"ops": [op], "brief": token, "implements": pa.decision}),
        );
        match r {
            rdy_qa::mcp::Reply::ToolError(e) => refusal_of(&e),
            other => panic!("expected a refusal, got {other:?}"),
        }
    };
    let cli_refusal = |sels: &[&str], reason: &str| {
        let mut args = vec!["new", "instruction", "--parent", &doc, "--text", "A rule."];
        for s in sels {
            args.push("--applies-to");
            args.push(s);
        }
        args.push("--reason");
        args.push(reason);
        let (ok, e) = w.json(pd.path(), &args);
        assert!(!ok, "{args:?} was refused: {e}");
        refusal_of(&e)
    };
    // an unknown act
    let m = mcp_refusal(&mut a, &["act:fly"], &pa.decision);
    assert_eq!(m.0, "E_VALIDATION", "{m:?}");
    assert_eq!(cli_refusal(&["act:fly"], &pa.decision), m);
    // a reason that is no decision
    let m = mcp_refusal(&mut a, &["all"], &doc);
    assert_eq!(cli_refusal(&["all"], &doc), m);
    assert!(
        m.1.contains("decided_by") || m.1.contains("decision"),
        "{m:?}"
    );
    // the reason is required and no key is invented
    for args in [
        vec![
            "new",
            "instruction",
            "--parent",
            &doc,
            "--text",
            "x",
            "--applies-to",
            "all",
        ],
        vec![
            "new",
            "instruction",
            "--parent",
            &doc,
            "--text",
            "x",
            "--applies-to",
            "all",
            "--reason",
            &pa.decision,
            "--key",
            "k",
        ],
        vec![
            "new",
            "instruction",
            "--parent",
            &doc,
            "--text",
            "x",
            "--reason",
            &pa.decision,
        ],
    ] {
        let out = w.rdy(pd.path(), &args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: clap refuses it");
    }
}

/// `rdy mcp` started in `cwd` with no project, as the world's human, for its reply to a tool.
fn mcp_outside(w: &World, cwd: &Path) -> McpClient {
    let mut cmd = rdy_qa::fixtures::rdy_command();
    cmd.arg("mcp")
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(w.env())
        .current_dir(cwd)
        .stderr(Stdio::piped());
    for name in w.removed_env() {
        cmd.env_remove(name);
    }
    McpClient::launch(cmd)
}

/// `rdy --json <args>` in `cwd` with no `--project`: the reply, which must succeed.
fn json_at(w: &World, cwd: &Path, args: &[&str]) -> Value {
    let mut cmd = rdy_qa::fixtures::rdy_command();
    cmd.arg("--json")
        .args(args)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(w.env())
        .current_dir(cwd);
    for name in w.removed_env() {
        cmd.env_remove(name);
    }
    let out = rdy_qa::fixtures::output_within(&mut cmd, std::time::Duration::from_secs(120))
        .expect("rdy ran");
    assert!(
        out.status.success(),
        "rdy {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("JSON")
}

#[test]
fn agi4_cli_full_matches_mcp_inside_and_outside_projects() {
    let w = World::new();
    let (pd, md) = (w.project(), w.project());
    let pa = author(pd.path(), "Small changes keep review cheap", PROJECT_SETS);
    let ma = author(md.path(), "One spelling in every reply", META_SETS);
    w.person(md.path());
    let mut c = w.mcp(pd.path());
    assert!(
        c.call(
            "rdy_session",
            json!({"actor": "agent:a", "run": "a1", "role": "reader"})
        )
        .is_ok()
    );
    let base = c.call("rdy_instructions", json!({})).ok();
    assert_eq!(builtin_sets(&base), ["rdy:bootstrap"], "{base}");
    // full: the same JSON from both, every built-in ruleset whole, nothing else added
    let full = c.call("rdy_instructions", json!({"full": true})).ok();
    assert_eq!(ok_json(&w, pd.path(), &["instructions", "--full"]), full);
    assert_eq!(builtin_sets(&full), BUILTIN, "{full}");
    assert_eq!(
        authored(&full, "instructions"),
        authored(&base, "instructions")
    );
    assert_eq!(full["menu"], base["menu"]);
    assert_eq!(full["hint"], base["hint"]);
    assert_eq!(full["warnings"], base["warnings"]);
    // an explicit load can add a stored optional ruleset; full still adds no stored one
    let (l1, l2) = (p(&pa.docs[1]), q(&ma.docs[1]));
    let loaded = c
        .call("rdy_instructions", json!({"full": true, "load": [l1, l2]}))
        .ok();
    assert_eq!(authored(&loaded, "instructions").len(), 5, "{loaded}");
    assert_eq!(builtin_sets(&loaded), BUILTIN);
    let rows = |v: &Value| -> Vec<String> {
        v["instructions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect()
    };
    let mut seen = rows(&loaded);
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), loaded["instructions"].as_array().unwrap().len());
    for args in [
        vec!["instructions", "--full", "--load", &l1, &l2],
        vec!["instructions", "--load", &l1, "--full", "--load", &l2],
        vec!["instructions", "--load", &l1, &l2, "--full"],
    ] {
        assert_eq!(ok_json(&w, pd.path(), &args), loaded, "{args:?}");
    }
    // a built-in handle with full is deduplicated, and one without full adds just that ruleset
    let both = c
        .call(
            "rdy_instructions",
            json!({"full": true, "load": ["rdy:write", "rdy:bootstrap"]}),
        )
        .ok();
    assert_eq!(both, full);
    assert_eq!(
        ok_json(
            &w,
            pd.path(),
            &[
                "instructions",
                "--full",
                "--load",
                "rdy:write",
                "rdy:bootstrap"
            ]
        ),
        full
    );
    let one = c
        .call("rdy_instructions", json!({"load": ["rdy:write"]}))
        .ok();
    assert_eq!(builtin_sets(&one), ["rdy:bootstrap", "rdy:write"]);
    assert_eq!(
        ok_json(&w, pd.path(), &["instructions", "--load", "rdy:write"]),
        one
    );
    // the text names the built-in rows and the whole menu
    let t = ok_text(&w, pd.path(), &["instructions", "--full"]);
    for id in BUILTIN {
        assert!(t.contains(id), "{id} in\n{t}");
    }
    // outside every project: the person's project is the project; the same replies
    let outside = short_tempdir();
    let mut o = mcp_outside(&w, outside.path());
    assert!(
        o.call(
            "rdy_session",
            json!({"actor": "agent:a", "run": "o1", "role": "reader"})
        )
        .is_ok()
    );
    let o_base = o.call("rdy_instructions", json!({})).ok();
    assert_eq!(json_at(&w, outside.path(), &["instructions"]), o_base);
    let o_full = o.call("rdy_instructions", json!({"full": true})).ok();
    assert_eq!(
        json_at(&w, outside.path(), &["instructions", "--full"]),
        o_full
    );
    assert_eq!(builtin_sets(&o_full), BUILTIN);
    assert_eq!(
        authored(&o_full, "instructions"),
        authored(&o_base, "instructions")
    );
    // with no rules project either: the projectless server and the CLI agree too
    let w2 = World::new();
    let nowhere = short_tempdir();
    let mut n = mcp_outside(&w2, nowhere.path());
    assert!(
        n.call(
            "rdy_session",
            json!({"actor": "agent:a", "run": "n1", "role": "reader"})
        )
        .is_ok()
    );
    let n_full = n.call("rdy_instructions", json!({"full": true})).ok();
    assert_eq!(builtin_sets(&n_full), BUILTIN);
    assert_eq!(
        json_at(&w2, nowhere.path(), &["instructions", "--full"]),
        n_full
    );
    let n_one = n
        .call("rdy_instructions", json!({"load": ["rdy:review"]}))
        .ok();
    assert_eq!(
        json_at(
            &w2,
            nowhere.path(),
            &["instructions", "--load", "rdy:review"]
        ),
        n_one
    );
}
