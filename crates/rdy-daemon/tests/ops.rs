//! ctl2, the read half: each op reads one snapshot; paths resolve against the session cwd;
//! status reports the open and quarantine files; errors carry the M1 shape.
use rdy_daemon::{ErrorReply, OPS, OpContext, clean_quarantine, dispatch, quarantine};
use rdy_engine::engine::{Local, write_typed};
use rdy_engine::{E_BAD_INPUT, Rdy, RdyError, Session, WriteRequest};
use rdy_model::Role;
use serde_json::{Value, json};

fn project() -> (tempfile::TempDir, Local) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, Local::new(r))
}

fn human() -> Session {
    Session::human("human:t".into(), Role::Writer)
}

fn doc(e: &Local, name: &str) -> Value {
    let name = name.to_string();
    write_typed(e, move |r| {
        let ops = serde_json::from_value(json!([{"op": "create", "type": "doc", "id": "$d", "fields": {"name": name, "title": "D", "doc_type": "glossary"}}])).unwrap();
        r.write(&human(), WriteRequest { view: "work".into(), ops, justification: None, message: String::new() })
    })
    .unwrap()
}

fn cx<'a>(e: &'a Local, s: &'a Session, cwd: &'a std::path::Path) -> OpContext<'a> {
    OpContext {
        engine: e,
        session: s,
        cwd,
    }
}

#[test]
fn ctl2_ops_are_named_and_an_unknown_op_names_them() {
    assert_eq!(OPS.len(), 33);
    assert_eq!(OPS[0], "session");
    let (d, e) = project();
    let s = human();
    let err = dispatch(&cx(&e, &s, d.path()), "frobnicate", json!({})).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT);
    assert!(
        err.fix.contains("status") && err.fix.contains("approve"),
        "{err}"
    );
}

#[test]
fn ctl2_status_reports_views_owners_seq_open_report_and_quarantine() {
    let (d, e) = project();
    let s = human();
    doc(&e, "a");
    let v = dispatch(&cx(&e, &s, d.path()), "status", json!({})).unwrap();
    assert_eq!(v["landings"], 0);
    let work = &v["views"][0];
    assert_eq!(work["name"], "work");
    assert_eq!(work["changes"], 1);
    assert_eq!(work["owner"]["actor"], "human:t");
    assert_eq!(work["owner"]["run"], "cli");
    assert!(v["seq"]["offset"].as_u64().unwrap() > 0, "{v}");
    assert_eq!(v["open_report"]["index_rebuilt"], false);
    assert_eq!(v["quarantine"], json!([]));
    // a quarantine file appears in the listing
    std::fs::write(
        d.path().join(".rdy/store/segments/seg-000000.log.torn-123"),
        b"x",
    )
    .unwrap();
    std::fs::write(d.path().join(".rdy/store/index.redb.damaged-1"), b"x").unwrap();
    std::fs::write(d.path().join(".rdy/store/unrelated"), b"x").unwrap();
    assert_eq!(
        quarantine(d.path()),
        vec![
            "index.redb.damaged-1".to_string(),
            "segments/seg-000000.log.torn-123".to_string()
        ]
    );
    let v = dispatch(&cx(&e, &s, d.path()), "status", json!({})).unwrap();
    assert_eq!(v["quarantine"].as_array().unwrap().len(), 2);
}

#[test]
fn ctl2_show_ls_impact_tags_log_read_canon_by_default_and_a_view_on_request() {
    let (d, e) = project();
    let s = human();
    let w = doc(&e, "a");
    let id = w["created"]["$d"].as_str().unwrap().to_string();
    let c = cx(&e, &s, d.path());
    let err = dispatch(&c, "show", json!({"id": id})).unwrap_err();
    assert!(
        err.code == rdy_engine::E_UNKNOWN_BLOCK || err.code == rdy_engine::E_NOT_FOUND,
        "canon has no such block yet: {err}"
    );
    let v = dispatch(&c, "show", json!({"id": id, "scope": "work"})).unwrap();
    assert_eq!(v["id"], id);
    let v = dispatch(&c, "ls", json!({"scope": "work"})).unwrap();
    assert_eq!(v[0]["name"], "a");
    let v = dispatch(&c, "ls", json!({})).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 0, "canon is empty");
    let v = dispatch(&c, "impact", json!({"id": id, "scope": "work"})).unwrap();
    assert!(
        v["items"].is_array(),
        "impact is {{items, by_area?}} like rdy_impact"
    );
    let v = dispatch(&c, "tags", json!({})).unwrap();
    assert_eq!(v["versions"], json!([]));
    let v = dispatch(&c, "log", json!({})).unwrap();
    assert_eq!(v, json!([]));
    let err = dispatch(&c, "ls", json!({"scope": "work", "bogus": 1})).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT);
    assert!(err.message.starts_with("ls:"), "{err}");
}

#[test]
fn ctl2_block_returns_yaml_for_the_editor() {
    let (d, e) = project();
    let s = human();
    let w = doc(&e, "a");
    let id = w["created"]["$d"].as_str().unwrap().to_string();
    let v = dispatch(
        &cx(&e, &s, d.path()),
        "block",
        json!({"id": id, "view": "work"}),
    )
    .unwrap();
    assert_eq!(v["id"], id);
    assert!(v["yaml"].as_str().unwrap().contains("title: D"), "{v}");
    assert!(v["version"].as_str().unwrap().len() >= 12);
}

#[test]
fn ctl2_render_export_and_check_resolve_relative_paths_against_the_session_cwd() {
    let (d, e) = project();
    let s = human();
    doc(&e, "a");
    write_typed(&e, |r| {
        r.propose(&human(), "work", "p")
            .and_then(|p| r.land_report(&Session::human("human:t".into(), Role::Approver), &p.id))
    })
    .unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let c = cx(&e, &s, cwd.path());
    let v = dispatch(&c, "export", json!({"out": "exp"})).unwrap();
    assert_eq!(v["docs"], 1);
    assert!(cwd.path().join("exp").is_dir(), "export went to cwd/exp");
    let v = dispatch(&c, "render", json!({"out": "site"})).unwrap();
    assert!(v["pages"].as_u64().unwrap() >= 1, "{v}");
    assert!(cwd.path().join("site").is_dir());
    let v = dispatch(&c, "check", json!({"path": "exp"})).unwrap();
    // the glossary doc has no Terms section yet (W_SKELETON_MISSING); this test is about the rest
    let rest: Vec<&Value> = v
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["code"] != "W_SKELETON_MISSING")
        .collect();
    assert!(rest.is_empty(), "a fresh export lints clean: {v}");
    let abs = d.path().join("exp2");
    dispatch(&c, "export", json!({"out": abs.to_string_lossy()})).unwrap();
    assert!(abs.is_dir(), "an absolute path is used as given");
}

#[test]
fn ctl2_an_rdy_error_becomes_an_error_reply_with_issues_and_corrected() {
    let mut e = RdyError::new(E_BAD_INPUT, "m", "f");
    e.issues.push(rdy_schema::Issue {
        severity: rdy_schema::Severity::Error,
        code: "X",
        block: None,
        message: "i".into(),
        fix: "fx".into(),
    });
    let e = e.with_corrected(json!({"a": 1}));
    let r: ErrorReply = e.into();
    assert_eq!(
        (r.code.as_str(), r.message.as_str(), r.fix.as_str()),
        ("E_BAD_INPUT", "m", "f")
    );
    assert_eq!(r.extra["issues"][0]["code"], "X");
    assert_eq!(r.extra["corrected"]["a"], 1);
    let r2: ErrorReply = RdyError::new(E_BAD_INPUT, "m", "f").into();
    assert!(!r2.extra.contains_key("issues"));
}

#[test]
fn ctl2_the_session_op_is_answered_by_the_host_not_by_dispatch() {
    let (d, e) = project();
    let s = human();
    let err = dispatch(&cx(&e, &s, d.path()), "session", json!({})).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT);
    assert_eq!(
        err.message,
        "the session op is answered by the daemon connection, not by dispatch"
    );
}

#[test]
fn ctl2_new_propose_approve_tag_land_through_the_writer_and_the_next_read_sees_them() {
    let (d, e) = project();
    let s = human();
    let c = cx(&e, &s, d.path());
    let v = dispatch(&c, "new_folder", json!({"path": "wallets"})).unwrap();
    assert!(v["change"].is_string(), "{v}");
    let v = dispatch(&c, "new_doc", json!({"path": "wallets/tx", "title": "Tx"})).unwrap();
    assert!(
        v["created"]
            .as_object()
            .unwrap()
            .values()
            .any(|x| x.as_str().unwrap().starts_with("d-"))
    );
    let p = dispatch(&c, "propose", json!({"title": "first"})).unwrap();
    let pid = p["id"].as_str().unwrap().to_string();
    let l = dispatch(&c, "approve", json!({"proposal": pid})).unwrap();
    assert!(l["landing"].is_string(), "{l}");
    let t = dispatch(&c, "tag", json!({"version": "v0.1.0"})).unwrap();
    assert_eq!(t["name"], "v0.1.0");
    let st = dispatch(&c, "status", json!({})).unwrap();
    assert_eq!(st["landings"], 1);
    assert!(
        st["version_position"]["label"]
            .as_str()
            .unwrap()
            .contains("v0.1.0"),
        "{st}"
    );
    let log = dispatch(&c, "log", json!({})).unwrap();
    assert_eq!(log[0]["title"], "first");
    assert_eq!(log[0]["tags"], json!(["v0.1.0"]));
}

#[test]
fn ctl2_an_agent_may_write_but_not_approve_tag_or_import() {
    let (d, e) = project();
    let a = Session::agent("agent:w".into(), Some("r1".into()), Role::Writer).unwrap();
    let c = cx(&e, &a, d.path());
    let v = dispatch(&c, "new_doc", json!({"path": "a"})).unwrap();
    assert!(v["change"].is_string());
    assert_eq!(
        dispatch(&c, "status", json!({})).unwrap()["views"][0]["name"],
        "r1",
        "the agent's default view is its run"
    );
    let p = dispatch(&c, "propose", json!({"title": "t"})).unwrap();
    for (op, args) in [
        ("approve", json!({"proposal": p["id"]})),
        ("tag", json!({"version": "v0.1.0"})),
        ("import", json!({"dir": "x", "view": "r1"})),
    ] {
        let err = dispatch(&c, op, args).unwrap_err();
        assert_eq!(err.code, rdy_engine::E_PERMISSION, "{op}");
        assert_eq!(err.message, "agent sessions cannot approve, tag or import");
    }
}

#[test]
fn ctl2_edit_block_round_trips_through_yaml() {
    let (d, e) = project();
    let s = human();
    let c = cx(&e, &s, d.path());
    let w = dispatch(&c, "new_doc", json!({"path": "a", "title": "Before"})).unwrap();
    let id = w["created"]
        .as_object()
        .unwrap()
        .values()
        .find(|x| x.as_str().unwrap().starts_with("d-"))
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let b = dispatch(&c, "block", json!({"id": id})).unwrap();
    let yaml = b["yaml"]
        .as_str()
        .unwrap()
        .replace("title: Before", "title: After");
    let v = dispatch(&c, "edit_block", json!({"yaml": yaml})).unwrap();
    assert!(v["change"].is_string(), "{v}");
    let shown = dispatch(&c, "show", json!({"id": id, "scope": "work"})).unwrap();
    assert_eq!(shown["fields"]["title"], "After");
    let err = dispatch(&c, "edit_block", json!({"yaml": "not: [valid"})).unwrap_err();
    assert_eq!(err.code, rdy_engine::E_BAD_INPUT);
}

#[test]
fn ctl2_import_reindex_and_fsck_run_and_fsck_reports_the_open() {
    let (d, e) = project();
    let s = human();
    let c = cx(&e, &s, d.path());
    dispatch(&c, "new_doc", json!({"path": "a"})).unwrap();
    let p = dispatch(&c, "propose", json!({"title": "t"})).unwrap();
    dispatch(&c, "approve", json!({"proposal": p["id"]})).unwrap();
    let other = tempfile::tempdir().unwrap();
    let o = Local::new(Rdy::init(other.path(), "rdy-standard").unwrap());
    {
        let oc = cx(&o, &s, other.path());
        dispatch(&oc, "new_doc", json!({"path": "b"})).unwrap();
        dispatch(&oc, "export", json!({"out": "exp", "scope": "work"})).unwrap();
    }
    o.close().unwrap();
    let dir = other.path().join("exp");
    let v = dispatch(
        &c,
        "import",
        json!({"dir": dir.to_string_lossy(), "view": "imp"}),
    )
    .unwrap();
    assert!(!v["created"].as_object().unwrap().is_empty(), "{v}");
    assert_eq!(
        dispatch(&c, "reindex", json!({})).unwrap(),
        json!({"reindexed": true})
    );
    let f = dispatch(&c, "fsck", json!({})).unwrap();
    assert_eq!(f["landings"], 1);
    assert!(f["frames"].as_u64().unwrap() > 0);
    assert_eq!(f["open_report"]["index_rebuilt"], false);
    assert_eq!(f["quarantine"], json!([]));
    assert_eq!(
        dispatch(&c, "status", json!({})).unwrap()["landings"],
        1,
        "the state was republished after reindex"
    );
}

#[test]
fn ctl2_a_refused_approve_is_the_gates_error_and_writes_nothing() {
    let (d, e) = project();
    let s = human();
    let c = cx(&e, &s, d.path());
    let err = dispatch(&c, "approve", json!({"proposal": "p-nothing"})).unwrap_err();
    assert_ne!(err.code, rdy_engine::E_INTERNAL);
    assert_eq!(dispatch(&c, "status", json!({})).unwrap()["landings"], 0);
}

#[test]
fn cbrf1_edit_block_bounds_max_tokens_like_the_brief_op() {
    let (d, e) = project();
    let s = human();
    let c = cx(&e, &s, d.path());
    let w = dispatch(&c, "new_doc", json!({"path": "a", "title": "Before"})).unwrap();
    let id = w["created"]
        .as_object()
        .unwrap()
        .values()
        .find(|x| x.as_str().unwrap().starts_with("d-"))
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let over = dispatch(&c, "brief", json!({"ids": [id], "max_tokens": 100_001})).unwrap_err();
    assert_eq!(over.code, "E_LIMIT");
    let yaml = dispatch(&c, "block", json!({"id": id})).unwrap()["yaml"]
        .as_str()
        .unwrap()
        .replace("title: Before", "title: After");
    let err = dispatch(
        &c,
        "edit_block",
        json!({"yaml": yaml, "max_tokens": 100_001}),
    )
    .unwrap_err();
    assert_eq!(err.code, "E_LIMIT", "{err:?}");
    assert_eq!(err.message, over.message);
    let shown = dispatch(&c, "show", json!({"id": id, "scope": "work"})).unwrap();
    assert_eq!(shown["fields"]["title"], "Before", "nothing was written");
    let ok = dispatch(
        &c,
        "edit_block",
        json!({"yaml": yaml, "max_tokens": 100_000}),
    )
    .unwrap();
    assert!(ok["change"].is_string(), "{ok}");
}

#[test]
fn qclean1_clean_quarantine_removes_regular_quarantine_files_and_nothing_else() {
    let d = tempfile::tempdir().unwrap();
    let store = d.path().join(".rdy/store");
    std::fs::create_dir_all(store.join("segments")).unwrap();
    std::fs::write(store.join("segments/seg-00000001.log"), b"live segment").unwrap();
    std::fs::write(store.join("segments/seg-00000001.log.torn-77"), b"torn").unwrap();
    std::fs::write(store.join("index.redb"), b"live index").unwrap();
    std::fs::write(store.join("index.redb.damaged-1"), b"old index").unwrap();
    let outside = d.path().join("keep.txt");
    std::fs::write(&outside, b"keep").unwrap();
    std::os::unix::fs::symlink(&outside, store.join("segments/link.torn-5")).unwrap();
    std::fs::create_dir(store.join("index.redb.damaged-dir")).unwrap();
    std::fs::write(store.join("index.redb.damaged-dir/inner"), b"x").unwrap();
    let c = clean_quarantine(d.path());
    assert_eq!(
        c.removed,
        vec![
            "index.redb.damaged-1".to_string(),
            "segments/seg-00000001.log.torn-77".to_string()
        ]
    );
    assert_eq!(
        c.skipped,
        vec![
            (
                "index.redb.damaged-dir".to_string(),
                "not a regular file".to_string()
            ),
            (
                "segments/link.torn-5".to_string(),
                "not a regular file".to_string()
            ),
        ]
    );
    assert!(outside.exists(), "a symlink's target is never touched");
    assert!(
        store.join("index.redb.damaged-dir/inner").exists(),
        "a directory is never recursed"
    );
    assert!(store.join("segments/seg-00000001.log").exists() && store.join("index.redb").exists());
    assert_eq!(
        quarantine(d.path()),
        vec![
            "index.redb.damaged-dir".to_string(),
            "segments/link.torn-5".to_string()
        ]
    );
    assert_eq!(
        clean_quarantine(tempfile::tempdir().unwrap().path()),
        Default::default(),
        "no store, nothing to do"
    );
}

#[test]
fn wccm1_wc_nonce_waits_for_a_commit_still_in_flight() {
    // a client sends wc_commit and dies; the next command's wc_nonce must not answer "not
    // found" while that write is queued behind another, or the journal is dropped and the stage
    // is committed a second time
    let d = tempfile::tempdir().unwrap();
    let shared = rdy_daemon::Shared::new(Rdy::init(d.path(), "rdy-standard").unwrap());
    let e = shared.engine();
    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let blocker = {
        let e = std::sync::Arc::clone(&e);
        std::thread::spawn(move || {
            e.write(Box::new(move |_r| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(Value::Null)
            }))
            .unwrap();
        })
    };
    started_rx.recv().unwrap();
    let nonce = "wccm1-in-flight-nonce";
    let commit = {
        let (e, root) = (std::sync::Arc::clone(&e), d.path().to_path_buf());
        std::thread::spawn(move || {
            let s = human();
            let c = OpContext {
                engine: &*e,
                session: &s,
                cwd: &root,
            };
            dispatch(
                &c,
                "wc_commit",
                json!({
                    "view": "work",
                    "ops": [{"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "glossary"}}],
                    "nonce": nonce,
                    "rewrite": [],
                    "message": "in flight",
                }),
            )
            .unwrap()
        })
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !rdy_daemon::wc_in_flight(nonce) {
        assert!(
            std::time::Instant::now() < deadline,
            "wc_commit never started"
        );
        std::thread::yield_now();
    }
    let lookup = {
        let (e, root) = (std::sync::Arc::clone(&e), d.path().to_path_buf());
        std::thread::spawn(move || {
            let s = human();
            let c = OpContext {
                engine: &*e,
                session: &s,
                cwd: &root,
            };
            dispatch(
                &c,
                "wc_nonce",
                json!({"view": "work", "nonce": nonce, "rewrite": []}),
            )
            .unwrap()
        })
    };
    // the lookup gets the time it needs to answer early; it must still be waiting
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while !lookup.is_finished() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    release_tx.send(()).unwrap();
    blocker.join().unwrap();
    let committed = commit.join().unwrap();
    let found = lookup.join().unwrap();
    assert!(committed["change"].is_string(), "{committed}");
    assert_eq!(found["change"], committed["change"], "{found}");
    assert!(!rdy_daemon::wc_in_flight(nonce));
    shared.shutdown().unwrap();
}

#[test]
fn wcfg1_wc_put_blob_stores_a_file_and_refuses_a_reader_a_directory_and_a_missing_file() {
    let (d, e) = project();
    std::fs::write(d.path().join("chart.png"), b"\x89PNG chart").unwrap();
    let s = human();
    let v = dispatch(
        &cx(&e, &s, d.path()),
        "wc_put_blob",
        json!({"path": "chart.png"}),
    )
    .unwrap();
    assert_eq!(v["bytes"], 10, "{v}");
    let hash = v["hash"].as_str().unwrap().to_string();
    assert_eq!(hash.len(), 64, "{v}");
    let again = dispatch(
        &cx(&e, &s, d.path()),
        "wc_put_blob",
        json!({"path": "chart.png"}),
    )
    .unwrap();
    assert_eq!(again["hash"], hash.as_str(), "content addressed");
    let reader = Session::human("human:r".into(), Role::Reader);
    let err = dispatch(
        &cx(&e, &reader, d.path()),
        "wc_put_blob",
        json!({"path": "chart.png"}),
    )
    .unwrap_err();
    assert_eq!(err.code, rdy_engine::E_PERMISSION);
    let err = dispatch(&cx(&e, &s, d.path()), "wc_put_blob", json!({"path": "."})).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    let err = dispatch(
        &cx(&e, &s, d.path()),
        "wc_put_blob",
        json!({"path": "none.png"}),
    )
    .unwrap_err();
    assert_eq!(err.code, rdy_engine::E_IO, "{err}");
    let err = dispatch(
        &cx(&e, &s, d.path()),
        "wc_put_blob",
        json!({"path": "chart.png", "x": 1}),
    )
    .unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
}

#[test]
fn msw11_a_wc_nonce_since_the_seen_head_finds_the_commit_behind_many_others() {
    use rdy_engine::engine::Engine as _;
    let (d, e) = project();
    let s = human();
    let c = cx(&e, &s, d.path());
    let commit = |name: String, nonce: String| {
        let v = dispatch(
            &c,
            "wc_commit",
            json!({
                "view": "work",
                "ops": [{"op": "create", "type": "doc", "id": "$d", "fields": {"name": name, "title": "D", "doc_type": "glossary"}}],
                "nonce": nonce,
                "rewrite": [],
                "message": "m",
            }),
        )
        .unwrap();
        assert!(v["change"].is_string(), "{v}");
    };
    commit("first".into(), "first".into());
    let since = e.read().view_head("work").unwrap().head.to_string();
    commit("mine".into(), "mine".into());
    for i in 0..20 {
        commit(format!("d{i}"), format!("other-{i}"));
    }
    let probe = dispatch(
        &c,
        "wc_nonce",
        json!({"view": "work", "nonce": "mine", "rewrite": [], "since": since}),
    )
    .unwrap();
    assert!(probe["change"].is_string(), "{probe}");
    let none = dispatch(
        &c,
        "wc_nonce",
        json!({"view": "work", "nonce": "none", "rewrite": [], "since": since}),
    )
    .unwrap();
    assert!(none["change"].is_null(), "{none}");
}

#[test]
fn hd3_wc_commit_refuses_an_op_with_an_unknown_field() {
    use rdy_engine::engine::Engine as _;
    let (d, e) = project();
    let s = human();
    let err = dispatch(
        &cx(&e, &s, d.path()),
        "wc_commit",
        json!({
            "view": "work", "nonce": "n1", "rewrite": [], "message": "m",
            "ops": [{"op": "create", "type": "doc", "id": "$d", "prent": "x",
                     "fields": {"name": "a", "title": "D", "doc_type": "glossary"}}]
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    assert_eq!(err.message, "op 0: unknown field \"prent\"");
    assert!(
        err.fix
            .starts_with("did you mean \"parent\"? the fields are "),
        "{err}"
    );
    assert!(e.read().view_head("work").is_none(), "nothing written");
}

#[test]
fn hd3_an_op_arg_typo_gets_a_did_you_mean() {
    let (d, e) = project();
    let s = human();
    let err = dispatch(&cx(&e, &s, d.path()), "status", json!({"verbos": true})).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    assert_eq!(err.message, "status: unknown field \"verbos\"");
    assert_eq!(err.fix, "remove \"verbos\"; it takes no fields");
    let err = dispatch(&cx(&e, &s, d.path()), "log", json!({"limt": 5})).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    assert_eq!(err.message, "log: unknown field \"limt\"");
    assert_eq!(
        err.fix,
        "did you mean \"limit\"? the fields are limit, since"
    );
}
