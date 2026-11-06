//! ain4 (D-YQPB6, D-A8N0E): a project's daemon reads the person layer through the meta
//! project's own daemon (ctl `instructions`, its private form), as the caller's actor and run.
//! It never opens meta's store, so meta's writer stays the one writer; each read is of one
//! state, and a later landing is seen by the next call. The meta project read as this project
//! is read once, as the person layer. A missing meta project is an empty person layer with the
//! creation hint; a broken one is a visible refusal. A direct run's reader opens an unused meta
//! store itself and reads a held one through its daemon. Every person lives in an isolated home.
use rdy_daemon::instructions::PersonReader;
use rdy_daemon::{Proto, RunDir, StartOptions, connect_or_start, serve_command, spawn_serve};
use rdy_engine::engine::{Engine, Local};
use rdy_engine::instructions::{self, Caller, LoadRequest};
use rdy_qa::fixtures::rdy_bin;
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

/// One instructions call of a direct run on the project at `dir`, as `agent:direct`: the
/// project's store in this process, the person layer read by a direct reader in `w`'s home.
fn direct(w: &World, dir: &Path) -> Value {
    let (platform, env) = w.path_env();
    let host = PersonReader::direct(env!("CARGO_PKG_VERSION")).with_env(platform, env);
    let local = Local::open(dir).expect("open the project");
    let caller = Caller {
        actor: "agent:direct".into(),
        run: Some("d1".into()),
    };
    let reply = instructions::deliver(
        Some((&local.read(), &local.root())),
        &LoadRequest::default(),
        &caller,
        Some(&host),
    )
    .expect("a direct read");
    local.close().expect("close the project");
    serde_json::to_value(reply).unwrap()
}

fn session(c: &mut McpClient, actor: &str, run: &str, role: &str) {
    let s = c.call(
        "rdy_session",
        json!({"actor": actor, "run": run, "role": role}),
    );
    assert!(s.is_ok(), "{s:?}");
}

/// The id of each built-in (layer `rdy`) row.
fn builtin_ids(v: &Value) -> Vec<String> {
    v["instructions"]
        .as_array()
        .unwrap_or_else(|| panic!("instructions in {v}"))
        .iter()
        .filter(|r| r["layer"] == "rdy")
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect()
}

/// Each stored instruction row's id and layer; the built-in rows are read with [`builtin_ids`].
fn rows(v: &Value) -> Vec<(String, String)> {
    v["instructions"]
        .as_array()
        .unwrap_or_else(|| panic!("instructions in {v}"))
        .iter()
        .filter(|r| r["layer"] != "rdy")
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
    // a direct run opens the unused meta store itself, and closes it
    let p3 = w.project();
    let a3 = author(p3.path(), "Small changes keep review cheap", PROJECT_SETS);
    let expected = |a: &rdy_qa::instructions::Authored| {
        [
            row(&a.rules[0][0], "project"),
            row(&ma.rules[0][0], "person"),
        ]
    };
    assert_eq!(rows(&direct(&w, p3.path())), expected(&a3));
    assert!(w.daemon_pid(md.path()).is_none(), "no daemon was started");
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
    let person = |v: &Value| -> Value {
        v["instructions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["layer"] == "person")
            .cloned()
            .unwrap()
    };
    assert_eq!(person(&replies[0]), person(&replies[1]));
    // the same meta daemon served both, and it still holds its store: nothing reopened it
    assert_eq!(w.daemon_pid(md.path()), Some(meta_pid));
    let held = rdy_engine::Rdy::open(md.path()).map(drop).unwrap_err();
    assert_eq!(held.code, "E_LOCKED", "{held:?}");
    // a direct run finds the store held, and reads it through that daemon
    assert_eq!(rows(&direct(&w, p3.path())), expected(&a3));
    assert_eq!(w.daemon_pid(md.path()), Some(meta_pid));
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
    let stored: Vec<&Value> = v["menu"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["layer"] != "rdy")
        .collect();
    assert_eq!(
        stored[0]["id"],
        format!("person:{}", a.docs[0]),
        "read once, as the person layer: {v}"
    );
    assert_eq!(stored.len(), 1, "{v}");
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
    // the person contribution is empty; the built-in layer is in the complete reply
    assert_eq!(
        builtin_ids(&v),
        [
            "rdy:bootstrap:before",
            "rdy:bootstrap:approval",
            "rdy:bootstrap:after"
        ]
    );
    let meta = w.default_meta();
    assert_eq!(
        v["hint"].as_str().unwrap(),
        instructions::create_hint(&meta)
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

/// One ctl exchange with the daemon of `dir` as `actor` and `run`: a session, then `args` to
/// the `instructions` op.
fn ctl_instructions(w: &World, dir: &Path, actor: &str, run: &str, args: Value) -> Value {
    let (project, env) = (dir.to_path_buf(), w.env());
    let opts = StartOptions::new(
        env!("CARGO_PKG_VERSION").to_string(),
        Proto::Ctl,
        dir.to_path_buf(),
        Box::new(move |r| {
            let mut cmd = serve_command(&rdy_bin(), &project, r)?;
            cmd.envs(env.iter().map(|(k, v)| (k, v)));
            spawn_serve(&mut cmd)
        }),
    );
    let run_dir = RunDir::of_project(dir);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut c = connect_or_start(&run_dir, opts)
            .await
            .unwrap_or_else(|e| panic!("{e:?}"));
        c.ctl("session", json!({"actor": actor, "run": run}))
            .await
            .unwrap_or_else(|e| panic!("{e:?}"));
        let v = c.ctl("instructions", args).await;
        c.close();
        v.unwrap_or_else(|e| panic!("{e:?}"))
    })
}

#[test]
fn agi3_full_crosses_ctl_without_duplicating_person_reads() {
    let w = World::new();
    let (pd, md) = (w.project(), w.project());
    let pa = author(pd.path(), "Small changes keep review cheap", PROJECT_SETS);
    let ma = author(md.path(), "One spelling in every reply", META_SETS);
    w.person(md.path());
    // meta's daemon has a writer session before any project reads it
    let mut mw = w.mcp(md.path());
    session(&mut mw, "agent:meta", "m1", "writer");
    let meta_pid = w.daemon_pid(md.path()).expect("meta's daemon");
    // a reader on the project, whose reminder the ctl calls may or may not clear
    let mut c = w.mcp(pd.path());
    session(&mut c, "agent:p", "p1", "reader");
    assert_eq!(c.call("rdy_overview", json!({})).ok()["reminder"], REMINDER);
    let stored = [
        row(&pa.rules[0][0], "project"),
        row(&ma.rules[0][0], "person"),
    ];
    // the private form, sent to meta's daemon, returns its own rows as the person layer and
    // nothing else, whatever else the call carries
    for args in [
        json!({"part": true}),
        json!({"part": true, "full": true}),
        json!({"part": true, "full": true, "load": ["rdy:write"]}),
    ] {
        let v = ctl_instructions(&w, md.path(), "agent:p", "p1", args);
        let part = &v["contribution"];
        assert_eq!(part["layer"], "person", "{v}");
        assert_eq!(rows(part), [row(&ma.rules[0][0], "person")], "{v}");
        assert_eq!(part["menu"].as_array().unwrap().len(), 1, "{v}");
        assert!(builtin_ids(part).is_empty(), "{v}");
        assert!(v.get("instructions").is_none() && v.get("menu").is_none());
    }
    // and it marks nothing complete
    assert_eq!(c.call("rdy_overview", json!({})).ok()["reminder"], REMINDER);
    // the public form: one built-in source, each row once, beside the stored rows
    let plain = ctl_instructions(&w, pd.path(), "agent:q", "q1", json!({}));
    assert_eq!(builtin_ids(&plain).len(), 3, "{plain}");
    assert_eq!(rows(&plain), stored);
    let full = ctl_instructions(&w, pd.path(), "agent:p", "p1", json!({"full": true}));
    let built = builtin_ids(&full);
    assert_eq!(built.len(), 14, "{full}");
    let mut once = built.clone();
    once.sort();
    once.dedup();
    assert_eq!(once.len(), 14, "{full}");
    assert_eq!(rows(&full), stored);
    let menu: Vec<&Value> = full["menu"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["layer"] == "rdy")
        .collect();
    assert_eq!(menu.len(), 12, "{full}");
    // full with load repeats no row, built-in or stored
    let both = ctl_instructions(
        &w,
        pd.path(),
        "agent:p",
        "p1",
        json!({"full": true, "load": [format!("person:{}", ma.docs[0]), "rdy:write".to_string()]}),
    );
    assert_eq!(builtin_ids(&both), built);
    assert_eq!(rows(&both), stored);
    // closed arguments
    let bad = ctl_instructions_err(&w, pd.path(), json!({"ful": true}));
    assert!(bad.contains("ful"), "{bad}");
    // the complete public reply cleared that actor and run only
    assert!(
        c.call("rdy_overview", json!({}))
            .ok()
            .get("reminder")
            .is_none()
    );
    // the writer on meta stays the one writer, and its daemon served every read
    assert_eq!(w.daemon_pid(md.path()), Some(meta_pid));
    let held = rdy_engine::Rdy::open(md.path()).map(drop).unwrap_err();
    assert_eq!(held.code, "E_LOCKED", "{held:?}");
    mw.alive();
}

fn ctl_instructions_err(w: &World, dir: &Path, args: Value) -> String {
    // the same exchange as [`ctl_instructions`], whose last call must be refused
    let (project, env) = (dir.to_path_buf(), w.env());
    let opts = StartOptions::new(
        env!("CARGO_PKG_VERSION").to_string(),
        Proto::Ctl,
        dir.to_path_buf(),
        Box::new(move |r| {
            let mut cmd = serve_command(&rdy_bin(), &project, r)?;
            cmd.envs(env.iter().map(|(k, v)| (k, v)));
            spawn_serve(&mut cmd)
        }),
    );
    let run_dir = RunDir::of_project(dir);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut c = connect_or_start(&run_dir, opts).await.unwrap();
        c.ctl("session", json!({"actor": "agent:e", "run": "e1"}))
            .await
            .unwrap();
        let e = c.ctl("instructions", args).await.unwrap_err();
        c.close();
        format!("{e:?}")
    })
}
