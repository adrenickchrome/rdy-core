#![cfg(unix)]
// Unix only: it makes symlinks. FW-XP1 Phase 4 ports it.
//! ctl2, the read half: each op reads one snapshot; paths resolve against the session cwd;
//! status reports the open and quarantine files; errors carry the M1 shape.
use rdy_daemon::{ErrorReply, OPS, OpContext, clean_quarantine, dispatch, quarantine};
use rdy_engine::engine::{Engine, Local, write_typed};
use rdy_engine::{E_BAD_INPUT, Rdy, RdyError, Scope, Session, WriteRequest};
use rdy_model::Role;
use serde_json::{Value, json};

fn project() -> (tempfile::TempDir, Local) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, Local::new(r))
}

fn human() -> Session {
    Session::human("human:t".into(), Role::Writer).unwrap()
}

fn doc(e: &Local, name: &str) -> Value {
    let name = name.to_string();
    write_typed(e, move |r| {
        let ops = serde_json::from_value(json!([{"op": "create", "type": "doc", "id": "$d", "fields": {"name": name, "title": "D", "doc_type": "page"}}])).unwrap();
        r.write(&human(), WriteRequest { view: "work".into(), ops, justification: None, message: String::new() })
    })
    .unwrap()
}

fn cx<'a>(e: &'a Local, s: &'a Session, cwd: &'a std::path::Path) -> OpContext<'a> {
    OpContext {
        engine: e,
        session: s,
        human: "human:t",
        cwd,
        agent: &rdy_engine::signing_crypto::UnavailableAgent,
    }
}

#[test]
fn ctl2_ops_are_named_and_an_unknown_op_names_them() {
    assert_eq!(OPS.len(), 56);
    assert_eq!(OPS[0], "session");
    let impact = OPS.iter().position(|o| *o == "impact").unwrap();
    assert_eq!(
        &OPS[impact + 1..impact + 3],
        ["trace", "suspects"],
        "rf12: trace follows impact"
    );
    let new_doc = OPS.iter().position(|o| *o == "new_doc").unwrap();
    assert_eq!(
        OPS[new_doc + 1],
        "new_profile",
        "pub5: new_profile follows new_doc"
    );
    assert_eq!(
        &OPS[new_doc + 2..new_doc + 4],
        ["new_finding", "new_source"],
        "rf5: new_finding then new_source follow new_profile"
    );
    let render = OPS.iter().position(|o| *o == "render").unwrap();
    assert_eq!(
        OPS[render + 1],
        "publication_check",
        "pub9: publication_check follows render"
    );
    let answer = OPS.iter().position(|o| *o == "answer").unwrap();
    assert_eq!(
        &OPS[answer - 3..answer],
        ["plan", "reaffirm", "repin"],
        "cp11: plan, reaffirm and repin come immediately before answer"
    );
    assert_eq!(
        &OPS[answer + 1..],
        [
            "sign",
            "key_show",
            "remote",
            "key_bind",
            "key_retire",
            "key_revoke",
            "publish"
        ],
        "sig9: sign and key_show follow answer; hsa9: remote, key_bind, key_retire and key_revoke follow key_show; hpb15: publish follows key_revoke"
    );
    let export = OPS.iter().position(|o| *o == "export").unwrap();
    assert_eq!(OPS[export + 1], "dump", "dl7: dump follows export");
    assert_eq!(OPS[export + 2], "clone", "ec12: clone follows dump");
    assert_eq!(OPS[export + 3], "redact");
    let (d, e) = project();
    let s = human();
    let err = dispatch(&cx(&e, &s, d.path()), "frobnicate", json!({})).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT);
    assert!(
        err.fix.contains("status") && err.fix.contains("approve"),
        "{err}"
    );
}

/// hpb15 (D-8QV9G, D-KQ4CA): the `publish` op takes the coordinator's strict request
/// `{tag, to, site, keep_latest?}`. An unknown key is the shared nearest-key refusal, a missing
/// field and a `keep_latest` that is not a boolean are the op's argument errors, and no profile
/// or audience can be passed. Each is refused before the user config or a hub is read.
#[test]
fn hpb15_the_publish_op_is_strict() {
    let (d, e) = project();
    let s = human();
    let c = cx(&e, &s, d.path());
    let err = dispatch(
        &c,
        "publish",
        json!({"tag": "v1.0.0", "to": "pub", "ste": "docs"}),
    )
    .unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT);
    assert_eq!(err.message, "publish: unknown field \"ste\"");
    assert!(err.fix.starts_with("did you mean \"site\"?"), "{err}");
    for key in ["profile", "audience", "token", "ratify"] {
        let mut args = json!({"tag": "v1.0.0", "to": "pub", "site": "docs"});
        args[key] = json!("x");
        let err = dispatch(&c, "publish", args).unwrap_err();
        assert_eq!(err.code, E_BAD_INPUT, "{key}");
        assert_eq!(
            err.message,
            format!("publish: unknown field \"{key}\""),
            "{key}"
        );
    }
    for args in [
        json!({"to": "pub", "site": "docs"}),
        json!({"tag": "v1.0.0", "to": "pub", "site": "docs", "keep_latest": "true"}),
    ] {
        let err = dispatch(&c, "publish", args.clone()).unwrap_err();
        assert_eq!(err.code, E_BAD_INPUT, "{args}");
        assert!(err.message.starts_with("publish: "), "{args}: {err}");
        assert_eq!(
            err.fix, "args for publish: {tag, to, site, keep_latest?}",
            "{args}"
        );
    }
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
        r.propose(&human(), "work", "p", Some("test summary"))
            .and_then(|p| {
                r.land_report(
                    &Session::human("human:t".into(), Role::Approver).unwrap(),
                    &p.id,
                )
            })
    })
    .unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let c = cx(&e, &s, cwd.path());
    let v = dispatch(&c, "export", json!({"out": "exp", "format": "md"})).unwrap();
    assert_eq!(v["docs"], 1);
    assert!(cwd.path().join("exp").is_dir(), "export went to cwd/exp");
    let v = dispatch(&c, "render", json!({"out": "site"})).unwrap();
    assert!(v["pages"].as_u64().unwrap() >= 1, "{v}");
    assert!(cwd.path().join("site").is_dir());
    // no op writes a format-3 tree (O12): the engine writes the one check lints
    e.read()
        .export(&Scope::Canon, &cwd.path().join("exp3"))
        .unwrap();
    let v = dispatch(&c, "check", json!({"path": "exp3"})).unwrap();
    assert!(
        v.as_array().unwrap().is_empty(),
        "a fresh export lints clean: {v}"
    );
    let abs = d.path().join("exp2");
    dispatch(
        &c,
        "export",
        json!({"out": abs.to_string_lossy(), "format": "md"}),
    )
    .unwrap();
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
    let p = dispatch(
        &c,
        "propose",
        json!({"title": "first", "summary": "test summary"}),
    )
    .unwrap();
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
fn ctl2_an_agent_may_write_but_not_approve_alone_tag_or_import() {
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
    let err = dispatch(&c, "approve", json!({"proposal": p["id"]})).unwrap_err();
    assert_eq!(err.code, rdy_engine::E_PERMISSION);
    assert_eq!(
        err.message,
        "an agent approves only by relaying a person's approval"
    );
    for (op, args) in [
        ("tag", json!({"version": "v0.1.0"})),
        ("import", json!({"dir": "x", "view": "r1"})),
    ] {
        let err = dispatch(&c, op, args).unwrap_err();
        assert_eq!(err.code, rdy_engine::E_PERMISSION, "{op}");
        assert_eq!(err.message, "agent sessions cannot tag or import");
        assert_eq!(
            err.fix,
            "tags and imports are the person's acts: ask the person to run the command"
        );
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
    let p = dispatch(
        &c,
        "propose",
        json!({"title": "t", "summary": "test summary"}),
    )
    .unwrap();
    dispatch(&c, "approve", json!({"proposal": p["id"]})).unwrap();
    let other = tempfile::tempdir().unwrap();
    let o = Local::new(Rdy::init(other.path(), "rdy-standard").unwrap());
    let dir = other.path().join("exp");
    {
        let oc = cx(&o, &s, other.path());
        dispatch(&oc, "new_doc", json!({"path": "b"})).unwrap();
        o.read().export(&Scope::View("work".into()), &dir).unwrap();
    }
    o.close().unwrap();
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

/// rdb7 (D-6H0ZX): `fsck --clean-quarantine` overwrites a damaged index with zeros and syncs it
/// before it deletes it; a torn segment tail is removed as before.
#[test]
fn rdb7_clean_quarantine_zeroes_damaged_indexes() {
    let d = tempfile::tempdir().unwrap();
    let store = d.path().join(".rdy/store");
    std::fs::create_dir_all(store.join("segments")).unwrap();
    std::fs::write(store.join("segments/seg-00000001.log.torn-7"), b"torn").unwrap();
    let damaged = store.join("index.redb.damaged-3");
    let old = b"old index canary drawing ".repeat(100_000);
    std::fs::write(&damaged, &old).unwrap();
    let (c, zeroed) = rdy_store::fsutil::record_zeroed(|| clean_quarantine(d.path()));
    assert_eq!(
        c.removed,
        vec![
            "index.redb.damaged-3".to_string(),
            "segments/seg-00000001.log.torn-7".to_string()
        ]
    );
    assert!(c.skipped.is_empty(), "{:?}", c.skipped);
    assert_eq!(zeroed.len(), 1, "{zeroed:?}");
    assert_eq!(zeroed[0].path, damaged);
    assert_eq!(zeroed[0].len, old.len() as u64);
    assert!(zeroed[0].all_zero, "every byte is zero before the delete");
    assert!(!damaged.exists());
}

/// rdb7 (ruling task-7 Round 2, F6): a damaged index with a second link is not zeroed, since the
/// zeroing is for rdy's own index file only: it is skipped untouched, the store logs it, and the
/// other link keeps its bytes.
#[test]
fn rdb7_clean_quarantine_skips_a_damaged_index_with_two_links() {
    let d = tempfile::tempdir().unwrap();
    let store = d.path().join(".rdy/store");
    std::fs::create_dir_all(store.join("segments")).unwrap();
    let damaged = store.join("index.redb.damaged-3");
    let old = b"old index canary drawing ".repeat(1000);
    std::fs::write(&damaged, &old).unwrap();
    let other = d.path().join("other-link");
    std::fs::hard_link(&damaged, &other).unwrap();
    let capture = rdy_store::capture_notices_for_tests(&[&damaged.display().to_string()]);
    let (c, zeroed) = rdy_store::fsutil::record_zeroed(|| clean_quarantine(d.path()));
    let lines = capture.notices();
    drop(capture);
    assert!(c.removed.is_empty(), "{:?}", c.removed);
    assert_eq!(c.skipped.len(), 1, "{:?}", c.skipped);
    assert_eq!(c.skipped[0].0, "index.redb.damaged-3");
    assert!(
        c.skipped[0].1.contains("more than one link"),
        "{:?}",
        c.skipped
    );
    assert_eq!(zeroed, vec![]);
    assert_eq!(std::fs::read(&damaged).unwrap(), old, "skipped untouched");
    assert_eq!(std::fs::read(&other).unwrap(), old);
    assert_eq!(lines.len(), 1, "{lines:?}");
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
                human: "human:t",
                cwd: &root,
                agent: &rdy_engine::signing_crypto::UnavailableAgent,
            };
            dispatch(
                &c,
                "wc_commit",
                json!({
                    "view": "work",
                    "ops": [{"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}}],
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
                human: "human:t",
                cwd: &root,
                agent: &rdy_engine::signing_crypto::UnavailableAgent,
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
    let reader = Session::human("human:r".into(), Role::Reader).unwrap();
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
                "ops": [{"op": "create", "type": "doc", "id": "$d", "fields": {"name": name, "title": "D", "doc_type": "page"}}],
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
                     "fields": {"name": "a", "title": "D", "doc_type": "page"}}]
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
        "did you mean \"limit\"? the fields are limit, since, verify"
    );
}

/// hc8: the dump op's shape hint names `after`, the field `DumpArgs` now takes.
#[test]
fn hc8_a_malformed_dump_arg_gets_a_shape_hint_that_names_after() {
    let (d, e) = project();
    let s = human();
    let err = dispatch(&cx(&e, &s, d.path()), "dump", json!({"out": 5})).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    assert_eq!(err.fix, "args for dump: {out, after?}", "{err}");
}

#[test]
fn hd3_an_alt_member_with_an_unknown_field_is_refused() {
    let (d, e) = project();
    let s = human();
    let err = dispatch(
        &cx(&e, &s, d.path()),
        "alt",
        json!({"op": "create", "title": "t", "members": [
            {"label": "a", "view": "v", "viw": 1},
            {"label": "b", "view": "w"}
        ]}),
    )
    .unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    assert_eq!(err.message, "alt: unknown field \"members[0].viw\"");
    assert_eq!(err.fix, "did you mean \"view\"? the fields are label, view");
}

/// A tension detector that always fails: every tension read of the state errs.
struct Failing;
static FAILING_DETS: [&dyn rdy_engine::tensions::Detector; 1] = [&Failing];
impl rdy_engine::tensions::Detector for Failing {
    fn kind(&self) -> rdy_engine::tensions::TensionKind {
        rdy_engine::tensions::TensionKind::EditCollision
    }
    fn detect(
        &self,
        _cx: &rdy_engine::tensions::DetectCx<'_>,
        _seeds: Option<&std::collections::BTreeSet<rdy_model::BlockId>>,
        _out: &mut Vec<rdy_engine::tensions::Found>,
    ) -> Result<(), RdyError> {
        Err(RdyError::new(
            rdy_engine::E_INTERNAL,
            "boom",
            "a test detector",
        ))
    }
}

/// cc15: a coverage read error never fails `status`: the coverage is left out and
/// `coverage_error` says why.
#[test]
fn cc15_a_coverage_read_error_never_fails_status() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    r.set_detectors_for_tests(&FAILING_DETS);
    let e = Local::new(r);
    let s = human();
    let v = dispatch(&cx(&e, &s, dir.path()), "status", json!({})).unwrap();
    assert!(v.get("coverage").is_none(), "{v:#}");
    assert!(
        v["coverage_error"]
            .as_str()
            .is_some_and(|m| m.starts_with("E_INTERNAL: ") && m.contains("boom")),
        "{v:#}"
    );
    assert_eq!(v["landings"], 0, "the rest of the status stands");
}

/// cc15 (perf fix): `status` reads the memoised coverage summary and never builds the full
/// report (every issue cloned, given a path and sorted); its numbers are the report's.
/// Fix round 2 (FW-PF2; `daemon_read_during_big_write`): a write to a view leaves canon as it
/// was, so the next status carries canon's coverage from the state before and builds no state
/// layer; its numbers still agree with the report.
#[test]
fn cc15_status_after_a_view_write_carries_canons_coverage() {
    let (d, e) = project();
    let s = human();
    doc(&e, "a");
    let c = cx(&e, &s, d.path());
    dispatch(&c, "status", json!({})).unwrap();
    let before = rdy_engine::coverage::state_layer_builds();
    doc(&e, "b");
    let v = dispatch(&c, "status", json!({})).unwrap();
    assert_eq!(
        rdy_engine::coverage::state_layer_builds(),
        before,
        "carried, not built in full: {v:#}"
    );
    let report = dispatch(&c, "coverage", json!({})).unwrap();
    for k in ["score", "dimensions", "issues_total", "kinds"] {
        assert_eq!(v["coverage"][k], report[k], "{k}: {v:#} vs {report:#}");
    }
}

#[test]
fn cc15_status_builds_no_coverage_report_and_agrees_with_it() {
    let (d, e) = project();
    let s = human();
    doc(&e, "a");
    let c = cx(&e, &s, d.path());
    let before = rdy_engine::coverage::report_builds();
    let v = dispatch(&c, "status", json!({})).unwrap();
    assert_eq!(rdy_engine::coverage::report_builds(), before, "{v:#}");
    assert!(v.get("coverage_error").is_none(), "{v:#}");
    let report = dispatch(&c, "coverage", json!({})).unwrap();
    for k in [
        "score",
        "dimensions",
        "issues_total",
        "kinds",
        "similar_kinds",
    ] {
        assert_eq!(v["coverage"][k], report[k], "{k}: {v:#} vs {report:#}");
    }
}

#[test]
fn di12_pending_and_answer_refuse_a_field_they_do_not_take() {
    // FW-HD1: every mutation request refuses unknown fields, and so do the queue's ops
    let (d, e) = project();
    let s = human();
    let c = cx(&e, &s, d.path());
    let q = dispatch(&c, "pending", json!({})).unwrap();
    assert_eq!(q["items"], json!([]), "{q}");
    assert_eq!(q["snoozed"], 0, "{q}");
    for (op, args) in [
        ("pending", json!({"max_tokens": 100, "limit": 3})),
        (
            "answer",
            json!({"ref": "P-x@1", "option": "approve", "force": true}),
        ),
        (
            "answer",
            json!({"all_recommended": [{"ref": "P-x@1", "hash": "00"}]}),
        ),
    ] {
        let err = dispatch(&c, op, args).unwrap_err();
        assert_eq!(err.code, E_BAD_INPUT, "{op}: {err}");
    }
    // a malformed item ref is bad input too, not a missing item
    let err = dispatch(&c, "answer", json!({"ref": "nope", "option": "approve"})).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    // a request must name its item, or the items to answer with their recommendation
    let err = dispatch(&c, "answer", json!({"option": "approve"})).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    let err = dispatch(
        &c,
        "answer",
        json!({"ref": "P-x@1", "option": "approve", "all_recommended": [{"ref": "P-x@1"}]}),
    )
    .unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
}

/// A project whose view `work` holds a prose block with secret text; returns its id.
fn secret_project() -> (tempfile::TempDir, Local, String) {
    let (d, e) = project();
    let w = write_typed(&e, move |r| {
        let ops = serde_json::from_value(json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "design", "title": "Design"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "Doc", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "The launch code is ZEBRA-7731."}
        ]))
        .unwrap();
        r.write(&human(), WriteRequest { view: "work".into(), ops, justification: None, message: String::new() })
    })
    .unwrap();
    let id = w["created"]["$p"].as_str().unwrap().to_string();
    (d, e, id)
}

fn keys(v: &Value) -> std::collections::BTreeSet<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

#[test]
fn rdx5_daemon_redact_reports_and_records() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let (d, e, id) = secret_project();
    let s = human();
    let c = cx(&e, &s, d.path());
    // a dry run: the result, the shared text and nothing written
    let end = e.read().seq();
    let v = dispatch(
        &c,
        "redact",
        json!({"targets": [id], "reason_class": "secret", "reason": " leaked ", "dry_run": true}),
    )
    .unwrap();
    assert_eq!(
        keys(&v),
        [
            "change",
            "dry_run",
            "reason",
            "reason_class",
            "redaction",
            "report",
            "text",
            "warnings"
        ]
        .into_iter()
        .map(String::from)
        .collect()
    );
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["reason"], "leaked");
    assert_eq!(v["reason_class"], "secret");
    assert!(v["change"].is_null() && v["redaction"].is_null());
    assert_eq!(v["report"]["targets"], json!([{"all_versions": id}]));
    // M3a Task 8 carries the redaction support, so the dry run's report says carried
    assert_eq!(v["report"]["assessment"], "carried");
    assert_eq!(v["warnings"].as_array().unwrap().len(), 5);
    let result: rdy_engine::redaction::RedactResult = serde_json::from_value(json!({
        "dry_run": v["dry_run"], "reason_class": v["reason_class"], "reason": v["reason"],
        "change": v["change"], "redaction": v["redaction"], "report": v["report"],
        "warnings": v["warnings"],
    }))
    .unwrap();
    let text = rdy_engine::redaction::report_text(&result, 4000)
        .unwrap()
        .text;
    assert_eq!(v["text"], text);
    assert!(text.starts_with("Redaction dry run\n"));
    assert!(text.ends_with("\nNo request was recorded."));
    assert_eq!(e.read().seq(), end, "a dry run writes nothing");
    // the defaults: not a dry run, the session's view, 4000 tokens
    let v = dispatch(
        &c,
        "redact",
        json!({"targets": [id], "reason_class": "personal", "reason": "a name"}),
    )
    .unwrap();
    assert_eq!(v["dry_run"], false);
    let change = v["change"].as_str().unwrap().to_string();
    let redaction = v["redaction"].as_str().unwrap().to_string();
    assert_eq!(change.len(), 64);
    assert!(v.get("truncated").is_none());
    assert!(v["consequences"].is_array(), "the mutation envelope: {v}");
    let text = v["text"].as_str().unwrap();
    assert!(text.starts_with("Redaction request\n"), "{text}");
    assert!(text.ends_with(&format!(
        "Recorded redaction {redaction} in change {change}.\n\nPropose the view to ask for approval."
    )));
    assert!(!text.contains("W_REDACTION_SECRET"));
    let st = e.read();
    let ch: rdy_model::Hash = change.parse().unwrap();
    assert!(st.included("work").contains(&ch), "the carrier is in work");
    let req = st
        .reader()
        .redaction_request(&redaction.parse().unwrap())
        .unwrap();
    assert_eq!(req.reason, "a name");
    assert_eq!(req.carrier, ch);
    drop(st);
    // an explicit view, by either spelling, and a budget
    let v = dispatch(
        &c,
        "redact",
        json!({"targets": [id], "reason_class": "legal", "reason": "a court order", "view": "view:work", "max_tokens": 20000}),
    )
    .unwrap();
    let ch2: rdy_model::Hash = v["change"].as_str().unwrap().parse().unwrap();
    assert!(e.read().included("work").contains(&ch2));
    // a small budget truncates the rows, never the warnings; a smaller one is refused first
    let full = dispatch(
        &c,
        "redact",
        json!({"targets": [id], "reason_class": "legal", "reason": "x", "dry_run": true, "max_tokens": 20000}),
    )
    .unwrap();
    // final review fix round 1, finding 6: one token short of the full text, the shortened
    // text must fit (its one row-list line replaces six outside rows and the inside rows, far
    // more than four bytes), so the reply is a truncated success, never a refusal
    let full_text = full["text"].as_str().unwrap();
    assert!(full.get("truncated").is_none(), "{full}");
    let need = full_text.len() / 4;
    assert!(
        need > 256,
        "the full text is over the smallest budget: {need}"
    );
    let rows = full["report"]["inside"].as_array().unwrap().len()
        + full["report"]["outside"].as_array().unwrap().len();
    assert!(rows >= 7, "the report has rows to omit: {full}");
    let v = dispatch(
        &c,
        "redact",
        json!({"targets": [id], "reason_class": "legal", "reason": "x", "dry_run": true, "max_tokens": need - 1}),
    )
    .unwrap();
    assert_eq!(v["truncated"], true, "{v}");
    let short = v["text"].as_str().unwrap();
    assert!(short.len() / 4 < need);
    assert!(
        short.contains("\nThe row list was omitted. Increase max_tokens to read it.\n"),
        "{short}"
    );
    assert!(
        !short.contains("location unknown."),
        "the rows are omitted: {short}"
    );
    let warnings = full["warnings"].as_array().unwrap();
    assert!(!warnings.is_empty());
    for w in warnings {
        let line = format!(
            "{}: {}",
            w["code"].as_str().unwrap(),
            w["message"].as_str().unwrap()
        );
        assert!(short.contains(&line), "every warning stays: {line}");
    }
    assert_eq!(
        v["report"], full["report"],
        "the structured report is complete"
    );
    assert!(short.ends_with("\nNo request was recorded."), "{short}");
    // a budget the lines that never truncate exceed (a 4096-byte reason at 256 tokens) is
    // refused, and writes nothing
    let end = e.read().seq();
    let err = dispatch(
        &c,
        "redact",
        json!({"targets": [id], "reason_class": "legal", "reason": "r".repeat(4096), "dry_run": true, "max_tokens": 256}),
    )
    .unwrap_err();
    assert_eq!(err.code, rdy_engine::E_REDACTION_BUDGET, "{err}");
    assert_eq!(e.read().seq(), end);
    let end = e.read().seq();
    for bad in [255, 20_001] {
        let err = dispatch(
            &c,
            "redact",
            json!({"targets": [id], "reason_class": "legal", "reason": "x", "max_tokens": bad}),
        )
        .unwrap_err();
        assert_eq!(err.code, E_BAD_INPUT);
        assert_eq!(err.message, "max_tokens must be from 256 to 20000.");
    }
    assert_eq!(e.read().seq(), end, "a refused budget writes nothing");
}

#[test]
fn rdx5_daemon_redact_preserves_roles_and_errors() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let (d, e, id) = secret_project();
    let args = |dry: bool| json!({"targets": [id], "reason_class": "secret", "reason": "leak", "dry_run": dry});
    let end = e.read().seq();
    // a reader may run the dry run, never record
    let reader = Session::human("human:t".into(), Role::Reader).unwrap();
    let c = cx(&e, &reader, d.path());
    dispatch(&c, "redact", args(true)).unwrap();
    let err = dispatch(&c, "redact", args(false)).unwrap_err();
    assert_eq!(err.code, "E_PERMISSION", "{err}");
    // an agent records under the existing ownership rules: never in the human's work view
    let a = Session::agent("agent:a".into(), Some("r1".into()), Role::Writer).unwrap();
    let c = cx(&e, &a, d.path());
    dispatch(&c, "redact", args(true)).unwrap();
    let mut w = args(false);
    w["view"] = json!("work");
    let err = dispatch(&c, "redact", w).unwrap_err();
    assert_eq!(err.code, "E_VIEW_OWNER", "{err}");
    assert_eq!(e.read().seq(), end, "refusals write nothing");
    // an agent's first request creates its own view and claims it, as its first commit would
    assert!(e.read().view_head("r1").is_none(), "r1 has no head yet");
    let v = dispatch(&c, "redact", args(false)).unwrap();
    let ch: rdy_model::Hash = v["change"].as_str().unwrap().parse().unwrap();
    assert!(e.read().included("r1").contains(&ch));
    let owner = e.read().view_head("r1").unwrap().owner.clone().unwrap();
    assert_eq!(
        (owner.actor.as_str(), owner.run.as_deref()),
        ("agent:a", Some("r1"))
    );
    // another agent may not record in it: the existing ownership error, unchanged
    let other = Session::agent("agent:b".into(), Some("r2".into()), Role::Writer).unwrap();
    let mut w = args(false);
    w["view"] = json!("r1");
    let err = dispatch(&cx(&e, &other, d.path()), "redact", w).unwrap_err();
    assert_eq!(err.code, "E_VIEW_OWNER", "{err}");
    assert_eq!(err.message, "view \"r1\" is owned by agent:a (run r1)");
    // the strict args: a near key names the field, in the args' order
    let s = human();
    let c = cx(&e, &s, d.path());
    let err = dispatch(
        &c,
        "redact",
        json!({"targets": [id], "reason_class": "secret", "reason": "x", "dryrun": true}),
    )
    .unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    assert_eq!(err.message, "redact: unknown field \"dryrun\"");
    assert_eq!(
        err.fix,
        "did you mean \"dry_run\"? the fields are targets, reason_class, reason, dry_run, view, max_tokens"
    );
    let err = dispatch(&c, "redact", json!({"targets": [id]})).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    assert!(err.message.starts_with("redact:"), "{err}");
    // the engine's own refusals pass through unchanged
    let err = dispatch(
        &c,
        "redact",
        json!({"targets": ["p-ZZZZZ"], "reason_class": "secret", "reason": "x"}),
    )
    .unwrap_err();
    assert_eq!(err.code, "E_REDACTION_TARGET");
    assert_eq!(err.message, "Cannot redact p-ZZZZZ: unknown target.");
    let err = dispatch(
        &c,
        "redact",
        json!({"targets": [id], "reason_class": "other", "reason": "x"}),
    )
    .unwrap_err();
    assert_eq!(err.code, "E_REDACTION_REASON");
    // the quarantine listing is the engine's, and the cleanup is unchanged
    let store = d.path().join(".rdy/store");
    std::fs::write(store.join("segments/seg-000000.log.torn-8"), b"x").unwrap();
    std::fs::create_dir(store.join("index.redb.damaged-8")).unwrap();
    std::os::unix::fs::symlink("/nonexistent/rdx5", store.join("index.redb.damaged-9")).unwrap();
    assert_eq!(
        quarantine(d.path()),
        rdy_engine::quarantine::names(d.path())
    );
    let cleaned = clean_quarantine(d.path());
    assert_eq!(
        cleaned.removed,
        vec!["segments/seg-000000.log.torn-8".to_string()]
    );
    assert_eq!(
        cleaned
            .skipped
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>(),
        ["index.redb.damaged-8", "index.redb.damaged-9"]
    );
}

/// The profile request every refusal below changes one thing of.
fn profile_args(parent: &str, select: &str) -> Value {
    json!({"key": "site", "parent": parent, "title": "Site", "audience": "public", "select": [select]})
}

#[test]
fn pub5_profile_create_is_strict_and_uses_normal_write_rules() {
    let (d, e) = project();
    let s = human();
    let guide = doc(&e, "guide")["created"]["$d"]
        .as_str()
        .unwrap()
        .to_string();
    let other = doc(&e, "other")["created"]["$d"]
        .as_str()
        .unwrap()
        .to_string();
    let c = cx(&e, &s, d.path());
    let end = e.read().seq();

    // an unknown request key fails before the write, and the nearest key is named
    let mut args = profile_args(&guide, &other);
    let list = args.as_object_mut().unwrap().remove("select").unwrap();
    args["selects"] = list;
    let err = dispatch(&c, "new_profile", args).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    assert_eq!(err.message, "new_profile: unknown field \"selects\"");
    assert_eq!(
        err.fix,
        "did you mean \"select\"? the fields are key, parent, title, audience, select, description, trail, view"
    );
    // a required key left out is the op's shape error
    let mut args = profile_args(&guide, &other);
    args.as_object_mut().unwrap().remove("title");
    let err = dispatch(&c, "new_profile", args).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    assert!(err.message.contains("missing field `title`"), "{err}");
    // a parent that is no block, and the schema's refusal of a wrong audience, come from the
    // write path
    let err = dispatch(&c, "new_profile", profile_args("d-ZZZZZ", &other)).unwrap_err();
    assert_ne!(err.code, "E_VIEW_OWNER", "{err}");
    let mut args = profile_args(&guide, &other);
    args["audience"] = json!("everyone");
    let err = dispatch(&c, "new_profile", args).unwrap_err();
    assert_eq!(err.code, "E_VALIDATION", "{err}");
    // view ownership: an agent never writes the human's view
    let a = Session::agent("agent:a".into(), Some("r1".into()), Role::Writer).unwrap();
    let mut args = profile_args(&guide, &other);
    args["view"] = json!("work");
    let err = dispatch(&cx(&e, &a, d.path()), "new_profile", args).unwrap_err();
    assert_eq!(err.code, "E_VIEW_OWNER", "{err}");
    assert_eq!(e.read().seq(), end, "every refusal wrote nothing");

    // the same request, valid: one create of a profile, the ordinary write result, and a change
    // message of exactly `new publication profile site`
    let v = dispatch(&c, "new_profile", profile_args(&guide, &other)).unwrap();
    assert_eq!(v["view"], "work");
    let id = v["created"]["$profile"].as_str().unwrap().to_string();
    assert!(id.starts_with("pf-"), "{id}");
    assert!(e.read().seq() > end);
    let st = e.read();
    let b = st
        .block(&Scope::View("work".into()), &id.parse().unwrap())
        .unwrap();
    assert_eq!(b.block_type, "publication_profile");
    assert_eq!(
        b.parent.as_ref().map(|p| p.to_string()),
        Some(guide.clone())
    );
    assert_eq!(b.fields["key"].as_str(), Some("site"));
    assert!(!b.fields.contains_key("trail"), "{:?}", b.fields);
    assert!(b.body.is_empty() && b.links.is_empty());
    let change: rdy_model::Hash = v["change"].as_str().unwrap().parse().unwrap();
    let ch: rdy_model::Change = st
        .reader()
        .get_typed(rdy_store::kinds::CHANGE, &change)
        .unwrap();
    assert_eq!(ch.message, "new publication profile site");
    assert_eq!(ch.ops.len(), 1);
}

/// pub9: the `publication_check` op reads one snapshot for every session role, refuses an unknown
/// key by name, takes only canon, tags and landings, and writes nothing, even when the policy
/// result is blocked.
#[test]
fn pub9_publication_check_op_is_strict_read_only_and_open_to_every_role() {
    let (d, e) = project();
    let s = human();
    let w = write_typed(&e, |r| {
        let ops = serde_json::from_value(json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$out", "fields": {"name": "out", "title": "Out", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]))
        .unwrap();
        r.write(&human(), WriteRequest { view: "work".into(), ops, justification: None, message: String::new() })
    })
    .unwrap();
    let (a, out, admin) = (
        w["created"]["$a"].as_str().unwrap().to_string(),
        w["created"]["$out"].as_str().unwrap().to_string(),
        w["created"]["$admin"].as_str().unwrap().to_string(),
    );
    write_typed(&e, move |r| {
        let ops = serde_json::from_value(json!([
            {"op": "create", "type": "prose", "parent": a, "body": format!("See {{{{{out}}}}}.")},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "open", "title": "Open", "audience": "public", "select": [a]}}
        ]))
        .unwrap();
        r.write(
            &human(),
            WriteRequest {
                view: "work".into(),
                ops,
                justification: None,
                message: String::new(),
            },
        )?;
        let approver = Session::human("human:t".into(), Role::Approver).unwrap();
        let p = r.propose(&human(), "work", "pub", Some("summary"))?;
        r.land(&approver, &p.id)
    })
    .unwrap();
    let c = cx(&e, &s, d.path());
    let end = e.read().seq();

    // every role inspects; a blocked policy is a result
    for role in [Role::Reader, Role::Writer, Role::Reviewer, Role::Approver] {
        let who = Session::human("human:t".into(), role).unwrap();
        let v = dispatch(
            &cx(&e, &who, d.path()),
            "publication_check",
            json!({"profile": "open"}),
        )
        .unwrap();
        assert_eq!(v["references"]["status"], "blocked", "{role:?}: {v}");
        assert_eq!(v["warnings"], json!([]));
        assert_eq!(v["scope"]["kind"], "canon");
    }
    // the closed keys: a misspelt one is named, a missing profile is the op's shape error
    let err = dispatch(
        &c,
        "publication_check",
        json!({"profile": "open", "againts": "canon"}),
    )
    .unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    assert_eq!(err.message, "publication_check: unknown field \"againts\"");
    assert!(err.fix.starts_with("did you mean \"against\"?"), "{err}");
    let err = dispatch(&c, "publication_check", json!({})).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    assert!(err.message.contains("missing field `profile`"), "{err}");
    // an existing view is a candidate (D-CWC91), and still no baseline
    let v = dispatch(
        &c,
        "publication_check",
        json!({"profile": "open", "scope": "view:work"}),
    )
    .unwrap();
    assert_eq!(
        v["scope"],
        json!({"kind": "view", "landing": null, "view": "work"}),
        "{v}"
    );
    assert_eq!(v["references"]["status"], "blocked", "{v}");
    let err = dispatch(
        &c,
        "publication_check",
        json!({"profile": "open", "against": "view:work"}),
    )
    .unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    assert_eq!(
        err.message,
        "Publication check against must be canon, tag:<name>, or landing:<hash>."
    );
    assert_eq!(e.read().seq(), end, "no check wrote anything");
}

/// rf5 (D-Z8P09): new_finding and new_source are strict, create one block through the ordinary
/// write in the bound view, and resolve each evidence source (an id, key or path) to a full
/// reference. With no parent and no decision home, the write's own refusal stands.
#[test]
fn rf5_new_finding_and_new_source_use_the_write_path() {
    let (d, e) = project();
    let s = human();
    let reg = doc(&e, "register")["created"]["$d"]
        .as_str()
        .unwrap()
        .to_string();
    let c = cx(&e, &s, d.path());
    let end = e.read().seq();
    let err = dispatch(
        &c,
        "new_finding",
        json!({"statment": "S", "evidence": [{"quote": "Q"}]}),
    )
    .unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    assert_eq!(err.message, "new_finding: unknown field \"statment\"");
    assert_eq!(
        err.fix,
        "did you mean \"statement\"? the fields are statement, evidence, parent, view"
    );
    let err = dispatch(
        &c,
        "new_source",
        json!({"title": "T", "kind": "release", "locater": "v2"}),
    )
    .unwrap_err();
    assert_eq!(err.message, "new_source: unknown field \"locater\"");
    assert_eq!(
        err.fix,
        "did you mean \"locator\"? the fields are title, kind, locator, version, parent, view"
    );
    // no parent and no home: the decision home's refusal
    let err = dispatch(
        &c,
        "new_source",
        json!({"title": "T", "kind": "release", "locator": "v2"}),
    )
    .unwrap_err();
    assert_eq!(
        err.message,
        "no parent given, and the project names no decision home"
    );
    // an evidence source that names nothing is refused before the write
    let err = dispatch(
        &c,
        "new_finding",
        json!({"statement": "S", "evidence": [{"source": "S-ZZZZZ"}], "parent": reg}),
    )
    .unwrap_err();
    assert_ne!(err.code, "E_INTERNAL", "{err}");
    assert_eq!(e.read().seq(), end, "every refusal wrote nothing");

    let v = dispatch(
        &c,
        "new_source",
        json!({"title": "Release 2", "kind": "release", "locator": "v2", "version": "2.0", "parent": reg}),
    )
    .unwrap();
    assert_eq!(v["view"], "work");
    let src = v["created"]["$source"].as_str().unwrap().to_string();
    assert!(src.starts_with("S-"), "{v}");
    let v = dispatch(
        &c,
        "new_finding",
        json!({"statement": "The release changed retention.", "parent": reg,
            "evidence": [{"quote": "A", "question": "Q?"}, {"source": src}]}),
    )
    .unwrap();
    let f = v["created"]["$finding"].as_str().unwrap().to_string();
    let st = e.read();
    let scope = Scope::View("work".into());
    let b = st.block(&scope, &f.parse().unwrap()).unwrap();
    assert_eq!(b.block_type, "finding");
    assert_eq!(b.parent.as_ref().map(|p| p.to_string()), Some(reg.clone()));
    assert_eq!(
        rdy_engine::field_json(&b.fields["evidence"]),
        json!([{"quote": "A", "question": "Q?"}, {"source": format!("{{{{{src}}}}}")}])
    );
    let sb = st.block(&scope, &src.parse().unwrap()).unwrap();
    assert_eq!(sb.field_str("version"), Some("2.0"));
    // no file is written beside the project
    assert!(!d.path().join("register.yaml").exists());
}

/// rf5 fix round 1 (D-Z8P09): under a stable parent, new_finding and new_source brief the
/// effective parent themselves (the decision home for a missing parent, else the parent given),
/// as reaffirm and repin do, so the write's briefing check passes without a token from the
/// caller. The check itself stays the engine's: a plain write of the same op is still refused.
#[test]
fn rf5_new_finding_and_new_source_brief_a_stable_parent() {
    let (d, e) = project();
    let s = human();
    let made = write_typed(&e, move |r| {
        let ops = serde_json::from_value(json!([
            {"op": "create", "type": "doc", "id": "$reg", "fields": {"name": "register", "title": "Register", "doc_type": "decisions-log", "lifecycle": "stable"}},
            {"op": "create", "type": "doc", "id": "$log", "fields": {"name": "log", "title": "Log", "doc_type": "decisions-log", "lifecycle": "stable"}}
        ]))
        .unwrap();
        r.write(&human(), WriteRequest { view: "work".into(), ops, justification: None, message: String::new() })
    })
    .unwrap();
    let reg = made["created"]["$reg"].as_str().unwrap().to_string();
    let log = made["created"]["$log"].as_str().unwrap().to_string();
    // the engine's check holds: an unbriefed create under the stable parent is refused
    let unbriefed = write_typed(&e, {
        let reg = reg.clone();
        move |r| {
            let ops = rdy_engine::parse_ops(vec![json!({"op": "create", "type": "source", "id": "$s", "parent": reg, "fields": {"title": "T", "kind": "release", "locator": "v1"}})])?;
            r.write(&human(), WriteRequest { view: "work".into(), ops, justification: None, message: String::new() })
        }
    })
    .unwrap_err();
    assert_eq!(unbriefed.code, "E_BRIEF_REQUIRED", "{unbriefed}");
    let c = cx(&e, &s, d.path());

    // an explicit stable parent
    let v = dispatch(
        &c,
        "new_source",
        json!({"title": "Release 2", "kind": "release", "locator": "v2", "parent": log}),
    )
    .unwrap();
    let src = v["created"]["$source"].as_str().unwrap().to_string();
    let v = dispatch(
        &c,
        "new_finding",
        json!({"statement": "The release changed retention.", "parent": log,
            "evidence": [{"quote": "Retention is now seven days."}, {"source": src}]}),
    )
    .unwrap();
    let f = v["created"]["$finding"].as_str().unwrap().to_string();
    for id in [&src, &f] {
        let b = e
            .read()
            .block(&Scope::View("work".into()), &id.parse().unwrap())
            .unwrap();
        assert_eq!(b.parent.as_ref().map(|p| p.to_string()), Some(log.clone()));
    }

    // the stable decision home, for a missing parent
    let cfg = d.path().join(".rdy.yaml");
    let text = std::fs::read_to_string(&cfg).unwrap() + "decisions:\n  home: register\n";
    std::fs::write(&cfg, text).unwrap();
    write_typed(&e, |r| r.reload_pack().map(|_| ())).unwrap();
    let v = dispatch(
        &c,
        "new_source",
        json!({"title": "Release 3", "kind": "release", "locator": "v3"}),
    )
    .unwrap();
    let src = v["created"]["$source"].as_str().unwrap().to_string();
    let v = dispatch(
        &c,
        "new_finding",
        json!({"statement": "The release changed retention again.",
            "evidence": [{"quote": "Retention is now one day."}]}),
    )
    .unwrap();
    let f = v["created"]["$finding"].as_str().unwrap().to_string();
    for id in [&src, &f] {
        let b = e
            .read()
            .block(&Scope::View("work".into()), &id.parse().unwrap())
            .unwrap();
        assert_eq!(b.parent.as_ref().map(|p| p.to_string()), Some(reg.clone()));
    }
}

/// rf12 (D-1876T): the `trace` op takes `{id, scope?, depth?, detail?, max_tokens?}` and nothing
/// else, reads like `rdy_trace` (the same refusals, the same reply), and writes nothing.
#[test]
fn rf12_the_trace_op_is_strict_and_reads_the_engine() {
    let (d, e) = project();
    let s = human();
    let w = doc(&e, "a");
    let id = w["created"]["$d"].as_str().unwrap().to_string();
    let c = cx(&e, &s, d.path());
    let log_end = || {
        e.write(Box::new(|r| {
            Ok(json!(format!("{:?}", r.store().log_end())))
        }))
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
    };
    let before = log_end();
    // canon is the default scope, and the doc is only in the work view
    let err = dispatch(&c, "trace", json!({"id": id})).unwrap_err();
    assert_eq!(err.code, "E_UNKNOWN_BLOCK", "{err}");
    assert_eq!(err.fix, "check the id with rdy_search", "{err}");
    let v = dispatch(&c, "trace", json!({"id": id, "scope": "work"})).unwrap();
    assert_eq!(v["id"], id);
    assert_eq!(v["block_type"], "doc");
    for k in [
        "governing",
        "sources",
        "decisions",
        "premises",
        "edges",
        "questions",
        "frontier",
    ] {
        assert_eq!(v[k], json!([]), "{k}: {v}");
    }
    assert_eq!(v["ungrounded"], false, "{v}");
    assert_eq!(
        v["introduced"],
        Value::Null,
        "an unlanded block has no introduction"
    );
    for k in ["truncated", "omitted"] {
        assert!(v.get(k).is_none(), "{k}: {v}");
    }
    // every parameter parses
    dispatch(
        &c,
        "trace",
        json!({"id": id, "scope": "work", "depth": 8, "detail": true, "max_tokens": 100_000}),
    )
    .unwrap();
    // strict: an unknown key names the nearest known one, a wrong shape names the args
    let err = dispatch(
        &c,
        "trace",
        json!({"id": id, "scope": "work", "detial": true}),
    )
    .unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT);
    assert_eq!(err.message, "trace: unknown field \"detial\"");
    assert!(err.fix.starts_with("did you mean \"detail\"?"), "{err}");
    for args in [
        json!({}),
        json!({"id": id, "depth": "3"}),
        json!({"id": id, "detail": "yes"}),
        json!({"id": id, "max_tokens": -1}),
    ] {
        let err = dispatch(&c, "trace", args.clone()).unwrap_err();
        assert_eq!(err.code, E_BAD_INPUT, "{args}");
        assert!(err.message.starts_with("trace: "), "{args}: {err}");
        assert_eq!(
            err.fix, "args for trace: {id, scope?, depth?, detail?, max_tokens?}",
            "{args}"
        );
    }
    // the closed depth range, signed
    for depth in [-1i64, 0, 9] {
        let err = dispatch(
            &c,
            "trace",
            json!({"id": id, "scope": "work", "depth": depth}),
        )
        .unwrap_err();
        assert_eq!(err.code, E_BAD_INPUT);
        assert_eq!(err.message, format!("depth {depth} is outside 1 to 8"));
        assert_eq!(err.fix, "pass a depth from 1 to 8");
    }
    // the token limit, with the read boundaries' text
    let err = dispatch(
        &c,
        "trace",
        json!({"id": id, "scope": "work", "max_tokens": 100_001}),
    )
    .unwrap_err();
    assert_eq!(err.code, "E_LIMIT");
    assert_eq!(err.message, "max_tokens 100001 is over the limit of 100000");
    assert_eq!(
        err.fix,
        "ask for at most 100000 tokens and page through the rest"
    );
    // a pinned version and a different scope clash; an unknown scope is not found
    let err = dispatch(
        &c,
        "trace",
        json!({"id": format!("{id}@v1"), "scope": "work"}),
    )
    .unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT);
    assert_eq!(
        err.fix,
        "use the pinned version, or read the bare id with a scope"
    );
    let err = dispatch(&c, "trace", json!({"id": id, "scope": "nope"})).unwrap_err();
    assert_eq!(err.code, "E_NOT_FOUND", "{err}");
    assert_eq!(log_end(), before, "a trace writes nothing");
}
