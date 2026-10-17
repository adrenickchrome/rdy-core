//! cview1: the view commands on the CLI, through the daemon, as the human and as an agent.
use rdy_daemon::{OpContext, dispatch};
use rdy_engine::*;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use serde_json::json;

fn with_rdy<T>(s: &Served, f: impl FnOnce(&mut Rdy) -> T) -> T {
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let out = f(&mut r);
    r.close().unwrap();
    out
}
fn text(s: &Served, args: &[&str]) -> (bool, String) {
    let o = s.rdy(args);
    (
        o.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}
fn out(o: &std::process::Output) -> (bool, String, String) {
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).into(),
        String::from_utf8_lossy(&o.stderr).into(),
    )
}

#[test]
fn cview1_new_fork_include_exclude_show_ls_compare_close_own_undo() {
    let s = Served::new();
    let (section, claim) = with_rdy(&s, |r| {
        let (_doc, section, claim) = seed_doc(r, "seed");
        land(r, "seed").unwrap();
        (section, claim)
    });
    let (ok, out) = text(&s, &["view", "new", "feat", "--base", "canon"]);
    assert!(ok && out.starts_with("view feat (0 changes"), "{out}");
    let (ok, v) = s.json(&["--json", "view", "new", "fix", "--base", "tag:v0.0.0"]);
    assert!(!ok && v["code"] == "E_NOT_FOUND", "{v}");
    let (ca, cb) = with_rdy(&s, |r| {
        let v = version_of(r, &Scope::Canon, &claim);
        let ca = write_as(
            r,
            &as_agent("agent:a", "r1"),
            "a",
            json!([{"op": "update", "id": claim, "base": v, "body": "a's"}]),
        )
        .unwrap()
        .change;
        let cb = write_as(
            r,
            &as_agent("agent:b", "r1"),
            "b",
            json!([{"op": "update", "id": claim, "base": v, "body": "b's"}]),
        )
        .unwrap()
        .change;
        write_as(
            r,
            &as_agent("agent:a", "r1"),
            "a",
            json!([{"op": "create", "type": "claim", "parent": section, "body": "y"}]),
        )
        .unwrap();
        (ca, cb)
    });
    let (ok, out) = text(&s, &["view", "fork", "a", "a-copy"]);
    assert!(ok && out.contains("2 changes"), "{out}");
    let (ok, out) = text(&s, &["view", "include", "a", &cb]);
    assert!(
        ok && out.starts_with("included 1 change(s) into a") && out.contains("conflict"),
        "{out}"
    );
    let (ok, out) = text(&s, &["show", &claim, "--scope", "a"]);
    assert!(
        ok && out.contains("conflict ") && out.contains("sides:"),
        "{out}"
    );
    let (ok, v) = s.json(&["--json", "show", &claim, "--scope", "a"]);
    assert!(
        ok && v["conflict"]["sides"].as_array().unwrap().len() == 2,
        "{v}"
    );
    let (ok, out) = text(&s, &["show", &claim]);
    assert!(ok && !out.contains("conflict"), "canon has none: {out}");
    let (ok, out) = text(&s, &["view", "show", "a"]);
    assert!(
        ok && out.contains("changes:") && out.contains("conflicts:") && out.contains("ops:"),
        "{out}"
    );
    let (ok, out) = text(&s, &["view", "ls"]);
    assert!(
        ok && out.contains("VIEW") && out.contains("a-copy") && out.contains("feat"),
        "{out}"
    );
    let (ok, v) = s.json(&["--json", "view", "ls", "--stale", "0"]);
    assert!(
        ok && v["views"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["idle"] == true),
        "{v}"
    );
    let (ok, out) = text(&s, &["view", "compare", "a"]);
    assert!(
        ok && out.starts_with("a vs canon") && out.contains("+ ") && out.contains("! "),
        "{out}"
    );
    let (ok, out) = text(&s, &["view", "exclude", "a", &ca]);
    assert!(
        ok && out.starts_with("excluded 1 change(s) from a"),
        "{out}"
    );
    let (ok, out) = text(&s, &["undo", "--view", "a"]);
    assert!(ok && out.starts_with("undid exclude"), "{out}");
    let (ok, out) = text(&s, &["view", "own", "a", "agent:z/r9"]);
    assert!(ok && out.trim_end().ends_with("owner agent:z/r9"), "{out}");
    let (ok, v) = s.json_as("agent:a", Some("r1"), &["--json", "undo", "--view", "a"]);
    assert!(
        !ok && v["code"] == "E_VIEW_OWNER",
        "an agent on the CLI is an agent: {v}"
    );
    let (ok, v) = s.json_as(
        "agent:a",
        Some("r1"),
        &["--json", "view", "own", "a", "--none"],
    );
    assert!(!ok && v["code"] == "E_PERMISSION", "{v}");
    let (ok, out) = text(&s, &["view", "own", "a", "--none"]);
    assert!(ok && out.trim_end().ends_with("owner none"), "{out}");
    let (ok, out) = text(
        &s,
        &["view", "close", "a-copy", "feat", "--reason", "scratch"],
    );
    assert!(
        ok && out.lines().count() == 2 && out.contains("closed a-copy (scratch)"),
        "{out}"
    );
    let (ok, out) = text(&s, &["view", "ls"]);
    assert!(ok && !out.contains("a-copy"), "{out}");
    let (ok, out) = text(&s, &["view", "ls", "--all"]);
    assert!(
        ok && out.contains("a-copy") && out.contains("closed"),
        "{out}"
    );
    let (ok, v) = s.json_as(
        "agent:a",
        Some("r1"),
        &["--json", "view", "close", "--stale", "0"],
    );
    assert!(!ok && v["code"] == "E_PERMISSION", "{v}");
    let (ok, out) = text(&s, &["view", "close", "--stale", "0"]);
    assert!(ok && out.starts_with("closed "), "{out}");
}

#[test]
fn cview1_compare_of_identical_scopes_says_so_and_undo_without_ops_says_why() {
    let s = Served::new();
    with_rdy(&s, |r| {
        seed_doc(r, "seed");
        land(r, "seed").unwrap();
    });
    let (ok, out) = text(&s, &["view", "compare", "canon", "canon"]);
    assert!(ok && out.trim_end().ends_with("no differences"), "{out}");
    let (ok, v) = s.json(&["--json", "undo", "--view", "seed"]);
    assert!(!ok && v["code"] == "E_NOTHING_TO_UNDO", "{v}");
}

#[test]
fn eff2_rdy_show_prints_when_a_decision_takes_effect() {
    let s = Served::new();
    let id = with_rdy(&s, |r| {
        let w = write(
            r,
            "work",
            json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Log", "doc_type": "decisions-log"}},
                {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}}
            ]),
        )
        .unwrap();
        let d = DecisionInput {
            parent: w.created["$s"].clone(),
            statement: "S".into(),
            effective_from: Some("2026-09-01T00:00:00Z".into()),
            ..Default::default()
        };
        r.decide(&writer(), "work", d).unwrap().created["$decision"].clone()
    });
    let (ok, text, err) = out(&s.rdy(&["show", "--scope", "work", &id]));
    assert!(ok, "{err}");
    assert!(
        text.lines()
            .any(|l| l == "effective from: 2026-09-01T00:00:00Z"),
        "{text}"
    );
}

/// Final review fix round 1 (Minor 8): `rdy view close --stale` keeps the human's own `work`
/// view open and says so; the next edit there still works.
#[test]
fn cview1_close_stale_keeps_the_humans_work_view_and_says_so() {
    let s = Served::new();
    with_rdy(&s, |r| {
        seed_doc(r, "work");
        seed_doc(r, "other");
    });
    std::thread::sleep(std::time::Duration::from_millis(5));
    let (ok, v) = s.json(&["--json", "view", "close", "--stale", "0"]);
    assert!(ok && v["kept"] == "work", "{v}");
    assert_eq!(v["closed"][0]["view"], "other", "{v}");
    with_rdy(&s, |r| {
        r.view_close(&writer(), &["other".into()], "x").unwrap_err();
        assert!(!r.read().view_head("work").unwrap().closed);
        write(r, "work", json!([{"op": "create", "type": "doc", "fields": {"name": "w2", "title": "W2", "doc_type": "page"}}])).unwrap();
    });
    std::thread::sleep(std::time::Duration::from_millis(5));
    let (ok, out) = text(&s, &["view", "close", "--stale", "0"]);
    assert!(
        ok && out.starts_with("closed 0 views") && out.contains("kept work open"),
        "{out}"
    );
}

#[test]
fn msw13_block_and_brief_strip_a_view_prefix_as_edit_block_does() {
    let (d, mut r) = project();
    let (_doc, _s, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(
        &mut r,
        "foo",
        json!([{"op": "update", "id": c, "body": "two"}]),
    )
    .unwrap();
    let want = version_of(&r, &Scope::View("foo".into()), &c);
    let e = Local::new(r);
    let h = writer();
    let cx = OpContext {
        engine: &e,
        session: &h,
        human: "human:qa",
        cwd: d.path(),
    };
    let b = dispatch(&cx, "block", json!({"id": c, "view": "view:foo"})).unwrap();
    assert_eq!(b["version"], want.as_str(), "{b}");
    let br = dispatch(&cx, "brief", json!({"ids": [c], "view": "view:foo"})).unwrap();
    assert_eq!(br["base"]["blocks"][0]["version"], want.as_str(), "{br}");
    e.close().unwrap();
}

#[test]
fn msw13_a_write_or_proposal_to_the_empty_view_name_is_refused() {
    let (d, r) = project();
    let e = Local::new(r);
    let h = writer();
    let cx = OpContext {
        engine: &e,
        session: &h,
        human: "human:qa",
        cwd: d.path(),
    };
    for view in ["view:", ""] {
        let err = dispatch(&cx, "new_doc", json!({"path": "a", "view": view})).unwrap_err();
        assert_eq!(err.code, E_BAD_INPUT, "{view:?}: {err}");
        assert!(err.message.contains("empty"), "{err}");
    }
    let err = dispatch(&cx, "propose", json!({"title": "t", "view": ""})).unwrap_err();
    assert!(err.message.contains("empty"), "{err}");
    e.close().unwrap();
}

/// Final review M8: a proposal names a view under the same length cap as a write (R10).
#[test]
fn msw13_a_proposal_to_an_over_long_view_name_is_e_limit() {
    let (d, r) = project();
    let e = Local::new(r);
    let h = writer();
    let cx = OpContext {
        engine: &e,
        session: &h,
        human: "human:qa",
        cwd: d.path(),
    };
    let err = dispatch(
        &cx,
        "propose",
        json!({"title": "t", "view": "v".repeat(300)}),
    )
    .unwrap_err();
    assert_eq!(err.code, E_LIMIT, "{err}");
    e.close().unwrap();
}

/// Final review M8: an agent's default view must be a writable name, so a run that names a
/// reserved view is refused when the session starts, not at its first write.
#[test]
fn msw13_an_agent_session_whose_run_names_a_reserved_view_is_refused() {
    for run in ["canon", "inflight"] {
        let e = Session::agent("agent:a".into(), Some(run.into()), rdy_model::Role::Writer)
            .unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{run}: {e}");
        assert!(e.message.contains(run), "{e}");
    }
    assert!(
        Session::agent(
            "agent:a".into(),
            Some("canon2".into()),
            rdy_model::Role::Writer
        )
        .is_ok()
    );
}

/// cf1 (D-X4GMH): `rdy view resolve <view> <block> --landed` takes the landed side of a frozen
/// conflict, as the human and as the view's agent; the text names the conflict and the version.
#[test]
fn cf1_rdy_view_resolve_landed_takes_the_landed_side() {
    let s = Served::new();
    let d = with_rdy(&s, |r| frozen_conflict(r).0);
    let (ok, said) = text(&s, &["view", "resolve", "b", &d]);
    assert!(
        !ok && said.contains("--landed"),
        "landed is required: {said}"
    );
    let o = s.rdy_as(
        "agent:b",
        Some("r1"),
        &["view", "resolve", "b", &d, "--landed"],
    );
    let (ok, stdout, stderr) = out(&o);
    assert!(ok, "{stdout}{stderr}");
    assert!(
        stdout.starts_with("resolved conflict ")
            && stdout.contains(&format!(" on {d} in b: took the landed side ")),
        "{stdout}"
    );
    let (ok, v) = s.json(&["--json", "view", "show", "b"]);
    assert!(ok && v["conflicts"] == json!([]), "{v}");
    let (ok, v) = s.json(&["--json", "view", "resolve", "b", &d, "--landed"]);
    assert!(!ok && v["code"] == "E_BAD_INPUT", "no conflict left: {v}");
}
