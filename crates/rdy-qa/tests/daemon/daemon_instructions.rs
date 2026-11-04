//! ain4 (D-YQPB6, D-A8N0E): a project's daemon reads the person layer through the meta
//! project's own daemon (ctl `instructions`, its private form), as the caller's actor and run.
//! It never opens meta's store, so meta's writer stays the one writer; each read is of one
//! state, and a later landing is seen by the next call. The meta project read as this project
//! is read once, as the person layer. A missing meta project is an empty person layer with the
//! creation hint; a broken one is a visible refusal. Every person lives in an isolated home.
use rdy_qa::instructions::{REMINDER, Set, World, author};
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Barrier;

const PROJECT_SETS: &[Set] = &[Set {
    name: "baseline",
    title: "Baseline",
    required: true,
    lead: Some("Read these before any task."),
    rules: &[(&["act:write"], "Write small changes.")],
}];

const META_SETS: &[Set] = &[Set {
    name: "personal",
    title: "Personal",
    required: true,
    lead: Some("My rules in every project."),
    rules: &[(&["all"], "Answer in British English.")],
}];

fn session(c: &mut McpClient, actor: &str, run: &str, role: &str) {
    let s = c.call(
        "rdy_session",
        json!({"actor": actor, "run": run, "role": role}),
    );
    assert!(s.is_ok(), "{s:?}");
}

/// Each instruction row's id and layer.
fn rows(v: &Value) -> Vec<(String, String)> {
    v["instructions"]
        .as_array()
        .unwrap_or_else(|| panic!("instructions in {v}"))
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_string(),
                r["layer"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn row(id: &str, layer: &str) -> (String, String) {
    (id.to_string(), layer.to_string())
}

/// Writes a required ruleset with one instruction (reason `decision`) into the agent's view
/// over `c`, proposes it with a summary, and has the person of `w` approve it in `dir`.
/// Returns the new instruction's id.
fn land_rule(w: &World, dir: &Path, c: &mut McpClient, name: &str, decision: &str) -> String {
    let v = c
        .call(
            "rdy_write",
            json!({"ops": [
                {"op": "create", "type": "doc", "id": "$d",
                 "fields": {"name": name, "title": name, "doc_type": "page", "required": true}},
                {"op": "create", "type": "instruction", "id": "$i", "parent": "$d",
                 "fields": {"applies_to": ["all"]}, "body": format!("Rule {name}."),
                 "links": {"decided_by": [{"target": decision}]}}
            ]}),
        )
        .ok();
    let rule = v["created"]["$i"].as_str().unwrap().to_string();
    let p = c
        .call(
            "rdy_propose",
            json!({"title": name, "summary": format!("Adds the rule {name}.")}),
        )
        .ok();
    let pref = p["ref"].as_str().unwrap().to_string();
    let out = w.rdy(dir, &["approve", &pref]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    rule
}

#[test]
fn ain4_meta_reads_share_its_daemon_and_never_reopen_its_writer() {
    let w = World::new();
    let md = w.project();
    let ma = author(md.path(), "One spelling in every reply", META_SETS);
    let (p1, p2) = (w.project(), w.project());
    let a1 = author(p1.path(), "Small changes keep review cheap", PROJECT_SETS);
    let a2 = author(p2.path(), "Small changes keep review cheap", PROJECT_SETS);
    w.person(md.path());
    // meta's daemon runs, with a writer session on it, before any project reads it
    let mut mw = w.mcp(md.path());
    session(&mut mw, "agent:meta", "m1", "writer");
    let meta_pid = w.daemon_pid(md.path()).expect("meta's daemon");
    // two project daemons read the person layer at once
    let barrier = Barrier::new(2);
    let replies: Vec<Value> = std::thread::scope(|s| {
        let hs: Vec<_> = [p1.path(), p2.path()]
            .into_iter()
            .enumerate()
            .map(|(i, dir)| {
                let (w, barrier) = (&w, &barrier);
                s.spawn(move || {
                    let mut c = w.mcp(dir);
                    session(&mut c, "agent:p", &format!("p{i}"), "reader");
                    barrier.wait();
                    c.call("rdy_instructions", json!({})).ok()
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(
        rows(&replies[0]),
        [
            row(&a1.rules[0][0], "project"),
            row(&ma.rules[0][0], "person")
        ]
    );
    assert_eq!(
        rows(&replies[1]),
        [
            row(&a2.rules[0][0], "project"),
            row(&ma.rules[0][0], "person")
        ]
    );
    assert_eq!(replies[0]["instructions"][1], replies[1]["instructions"][1]);
    // the same meta daemon served both, and it still holds its store: nothing reopened it
    assert_eq!(w.daemon_pid(md.path()), Some(meta_pid));
    let held = rdy_engine::Rdy::open(md.path()).map(drop).unwrap_err();
    assert_eq!(held.code, "E_LOCKED", "{held:?}");
    // a later landing through meta's writer is seen by the next read
    let late = land_rule(&w, md.path(), &mut mw, "late", &ma.decision);
    let mut c = w.mcp(p1.path());
    session(&mut c, "agent:p", "p9", "reader");
    let v = c.call("rdy_instructions", json!({})).ok();
    assert_eq!(
        rows(&v),
        [
            row(&a1.rules[0][0], "project"),
            row(&ma.rules[0][0], "person"),
            row(&late, "person")
        ]
    );
    assert_eq!(w.daemon_pid(md.path()), Some(meta_pid));
    mw.alive();
}

#[test]
fn ain4_same_project_is_read_once_and_later_landings_are_seen() {
    let w = World::new();
    let pd = w.project();
    let a = author(pd.path(), "One spelling in every reply", META_SETS);
    // the person layer is this project, under another spelling of its path
    let link = w.home.path().join("meta-link");
    rdy_qa::platform::symlink(pd.path(), &link).unwrap();
    w.person(&link);
    let mut c = w.mcp(pd.path());
    session(&mut c, "agent:s", "s1", "writer");
    let v = c.call("rdy_instructions", json!({})).ok();
    assert_eq!(rows(&v), [row(&a.rules[0][0], "person")]);
    assert_eq!(
        v["menu"][0]["id"],
        format!("person:{}", a.docs[0]),
        "read once, as the person layer: {v}"
    );
    assert_eq!(v["menu"].as_array().unwrap().len(), 1);
    let late = land_rule(&w, pd.path(), &mut c, "later", &a.decision);
    let v = c.call("rdy_instructions", json!({})).ok();
    assert_eq!(
        rows(&v),
        [row(&a.rules[0][0], "person"), row(&late, "person")]
    );
}

#[test]
fn ain4_missing_meta_is_empty_but_bad_meta_is_a_visible_failure() {
    let w = World::new();
    let pd = w.project();
    let a = author(pd.path(), "Small changes keep review cheap", PROJECT_SETS);
    // the default place, in this home, holds nothing: the project's rules and the hint
    let mut c = w.mcp(pd.path());
    session(&mut c, "agent:m", "m1", "writer");
    let v = c.call("rdy_instructions", json!({})).ok();
    assert_eq!(rows(&v), [row(&a.rules[0][0], "project")]);
    let meta = w.default_meta();
    assert_eq!(
        v["hint"].as_str().unwrap(),
        format!(
            "Create your meta project with rdy init {}, then write its rules with rdy's tools.",
            meta.display()
        )
    );
    assert!(
        c.call("rdy_overview", json!({}))
            .ok()
            .get("reminder")
            .is_none()
    );
    // an explicit absolute override that names no project: the hint names it, never the default
    let gone = w.home.path().join("no-such-meta");
    w.person(&gone);
    let v = c.call("rdy_instructions", json!({})).ok();
    assert!(
        v["hint"]
            .as_str()
            .unwrap()
            .contains(&gone.display().to_string()),
        "{v}"
    );
    // a misspelt key warns, and the default applies
    w.config("instructions:\n  persn: /x\n");
    let v = c.call("rdy_instructions", json!({})).ok();
    let warnings = v["warnings"].as_array().unwrap_or_else(|| panic!("{v}"));
    assert_eq!(warnings[0]["code"], "W_CONFIG_UNKNOWN", "{v}");
    assert!(
        v["hint"]
            .as_str()
            .unwrap()
            .contains(&meta.display().to_string())
    );
    // a partial project at the meta path is a refusal, not an empty layer
    let partial = w.home.path().join("partial");
    std::fs::create_dir_all(partial.join(".rdy")).unwrap();
    w.person(&partial);
    let mut c = w.mcp(pd.path());
    session(&mut c, "agent:m", "m4", "writer");
    match c.call("rdy_instructions", json!({})) {
        Reply::ToolError(e) => {
            assert_eq!(e["code"], "E_NO_PROJECT", "{e}");
            assert_eq!(
                e["message"],
                format!("{} has no .rdy.yaml, the project config", partial.display()),
                "{e}"
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    // the refusal leaves the reminder owed, and the project's other tools work
    let ov = c.call("rdy_overview", json!({})).ok();
    assert_eq!(ov["reminder"], REMINDER);
    // a meta project whose pack does not load is the open's refusal too
    let broken = w.project();
    std::fs::write(broken.path().join(".rdy.yaml"), "pack: no-such-pack\n").unwrap();
    w.person(broken.path());
    match c.call("rdy_instructions", json!({})) {
        Reply::ToolError(e) => assert!(e["code"].is_string(), "{e}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(c.call("rdy_overview", json!({})).ok()["reminder"], REMINDER);
}
