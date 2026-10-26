//! ok11: `rdy_upgrade` (D-WWAK7, D-KQ4CA, D-1HDVA) acts on the connected project only, from
//! one captured state: no steps (a normally opened store is already upgraded) and the last
//! `meta/upgrade` receipt. It takes no directory, never stops the process that answers, and
//! returns the JSON `rdy upgrade --json` prints.
use rdy_daemon::Shared;
use rdy_daemon::maintenance::coordinator_runs_for_tests;
use rdy_engine::engine::{Engine, Local};
use rdy_engine::{
    E_BAD_INPUT, E_LIMIT, E_PERMISSION, E_STORE_FORMAT, E_STORE_READ_ONLY, E_UPGRADE_BACKUP,
    E_UPGRADE_FAILED, E_UPGRADE_MOVED, Rdy, RdyError, Session, SessionKind, WriteRequest,
};
use rdy_mcp::params::parse_params;
use rdy_mcp::tools::{ERROR_ROWS, McpState, UpgradeParams, rdy_upgrade};
use rdy_model::Role;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{project, rdy_bin};
use rdy_qa::mcp::{McpClient, Reply};
use rdy_store::error::UpgradeFailure;
use rdy_store::format::{
    REF_UPGRADE, StoreFeature, StoreFeatures, baseline_features, registered_features,
};
use rdy_store::holder::StoreMode;
use rdy_store::upgrade::UpgradeStep;
use rdy_store::{OpenPolicy, StoreError};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const DESCRIPTION: &str = "Inspect this project's store upgrade and show the last upgrade receipt. Set dry_run to true to inspect without writes.";

fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<(u64, Vec<u8>)>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let m = std::fs::symlink_metadata(&p).unwrap();
        if m.is_dir() {
            out.insert(p.clone(), None);
            out.extend(tree(&p));
        } else {
            out.insert(p.clone(), Some((m.len(), std::fs::read(&p).unwrap())));
        }
    }
    out
}

fn assessment() -> StoreFeature {
    registered_features()
        .into_iter()
        .find(|f| f.name == "assessment")
        .expect("assessment is registered")
}

fn registry() -> Vec<UpgradeStep> {
    vec![UpgradeStep {
        id: "add_assessment".into(),
        from: StoreFeatures::of(vec![]),
        to: StoreFeatures::of(vec![assessment()]),
    }]
}

fn upgraded() -> StoreFeatures {
    baseline_features().merged(&[assessment()])
}

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

fn state(engine: Arc<dyn Engine>, role: Role) -> McpState {
    let mut st = McpState::new(engine);
    st.session = Some(session(role));
    st
}

fn upgrade(st: &mut McpState, dry_run: Option<bool>) -> Result<Value, RdyError> {
    rdy_upgrade(st, UpgradeParams { dry_run })
}

/// The report of a connected store of `features` with `receipt`: nothing planned, nothing run.
fn connected(dry_run: bool, features: &StoreFeatures, receipt: &Value) -> Value {
    let h = features.hash().to_string();
    json!({"dry_run": dry_run, "changed": false, "from": h, "to": h, "steps": [],
        "backup": null, "batch": null, "reindexed": false, "checked": false,
        "receipt": receipt})
}

fn tool_error(r: Reply) -> Value {
    match r {
        Reply::ToolError(v) => v,
        other => panic!("expected an rdy tool error, got {other:?}"),
    }
}

#[test]
fn ok11_mcp_is_connected_only_and_returns_last_receipt() {
    // project a: upgraded by its open, which ran the steps and the completion
    let (_a_dir, a) = project();
    drop(a);
    let a = _a_dir.path().to_path_buf();
    let r = Rdy::open_with_registry_for_tests(
        &a,
        StoreMode::Project,
        OpenPolicy::AutoUpgrade,
        &registry(),
    )
    .unwrap();
    let raw = r.store().get_ref(REF_UPGRADE).unwrap().expect("a receipt");
    let receipt: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(receipt["steps"], json!(["add_assessment"]));
    // project b: a second project that needs the same step
    let (b_dir, b) = project();
    drop(b);
    let b = b_dir.path().to_string_lossy().into_owned();
    let b_before = tree(b_dir.path());
    let calls = coordinator_runs_for_tests();
    let check = |st: &mut McpState| {
        // a directory is an unknown parameter: refused before anything runs
        for args in [
            json!({"dir": b}),
            json!({"dry_run": true, "dir": b}),
            json!({"dir": b, "dry_run": false}),
        ] {
            let e = parse_params::<UpgradeParams>("rdy_upgrade", args.as_object().unwrap().clone())
                .unwrap_err();
            assert_eq!(e.code, E_BAD_INPUT, "{e:?}");
            assert_eq!(e.message, "rdy_upgrade: unknown parameter \"dir\"");
        }
        for dry in [true, false] {
            let v = upgrade(st, Some(dry)).unwrap();
            assert_eq!(v, connected(dry, &upgraded(), &receipt), "dry_run {dry}");
        }
        assert_eq!(
            upgrade(st, None).unwrap(),
            connected(false, &upgraded(), &receipt)
        );
    };
    // the engine's default, over a local handle
    let local = Arc::new(Local::new(r));
    check(&mut state(
        Arc::clone(&local) as Arc<dyn Engine>,
        Role::Writer,
    ));
    Arc::try_unwrap(local).ok().unwrap().close().unwrap();
    // the daemon's engine, over a fresh open of the upgraded store
    let sh = Shared::new(Rdy::open(&a).unwrap());
    check(&mut state(sh.engine(), Role::Writer));
    sh.shutdown().unwrap();
    // project b was never touched, and no coordinator ran
    assert_eq!(tree(b_dir.path()), b_before);
    let plan =
        rdy_engine::maintenance::plan_with_registry_for_tests(b_dir.path(), &registry()).unwrap();
    assert_eq!(plan.steps, ["add_assessment"]);
    assert_eq!(
        coordinator_runs_for_tests(),
        calls,
        "the coordinator never ran"
    );
}

#[test]
fn ok11_current_mcp_upgrade_does_not_stop_itself() {
    let calls = coordinator_runs_for_tests();
    // a current store over the daemon's engine: the no-op report, and the daemon keeps serving
    let (dir, r) = project();
    let sh = Shared::new(r);
    let engine = sh.engine();
    let mut st = state(Arc::clone(&engine), Role::Writer);
    let none = Value::Null;
    assert_eq!(
        upgrade(&mut st, Some(false)).unwrap(),
        connected(false, &baseline_features(), &none)
    );
    engine
        .write(Box::new(|r| {
            r.write(
                &session(Role::Writer),
                WriteRequest {
                    view: "work".into(),
                    ops: serde_json::from_value(json!([
                        {"op": "create", "type": "doc", "fields": {"title": "After", "doc_type": "page"}}
                    ]))
                    .unwrap(),
                    justification: None,
                    message: String::new(),
                },
            )?;
            Ok(Value::Null)
        }))
        .expect("the daemon still writes after rdy_upgrade");
    assert_eq!(
        upgrade(&mut st, Some(true)).unwrap(),
        connected(true, &baseline_features(), &none)
    );
    drop(st);
    drop(engine);
    sh.shutdown().unwrap();
    drop(sh);

    // a read-only store: an apply is E_STORE_READ_ONLY, a dry run answers
    let ro = tempfile::tempdir().unwrap();
    {
        let _known = rdy_engine::read_only_feature_for_tests();
        let mut r = Rdy::init(ro.path(), "rdy-standard").unwrap();
        r.store_mut().put_blob(b"uses the feature").unwrap();
        r.seal_open().unwrap();
        r.close().unwrap();
    }
    let r = Rdy::open(ro.path()).unwrap();
    assert!(r.is_read_only());
    let features = r.store().features().clone();
    let sh = Shared::new(r);
    let mut st = state(sh.engine(), Role::Writer);
    let e = upgrade(&mut st, Some(false)).unwrap_err();
    assert_eq!(e.code, E_STORE_READ_ONLY, "{e:?}");
    assert_eq!(e.message, "store feature test_feature permits reads only");
    assert_eq!(
        upgrade(&mut st, Some(true)).unwrap(),
        connected(true, &features, &none)
    );
    drop(st);
    sh.shutdown().unwrap();
    assert_eq!(
        coordinator_runs_for_tests(),
        calls,
        "the coordinator never ran"
    );
    drop(dir);

    // through a real daemon: the proxy's answer comes from the daemon, which keeps its pid
    let s = Served::new();
    let mut c = McpClient::start_proxy(&s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:qa", "run": "qa", "role": "writer"}),
    )
    .ok();
    let pid = rdy_daemon::live_daemon(s.dir.path()).expect("the daemon runs");
    let v = c.call("rdy_upgrade", json!({})).ok();
    assert_eq!(v["changed"], json!(false), "{v}");
    assert_eq!(v["steps"], json!([]));
    let v = c.call("rdy_upgrade", json!({"dry_run": true})).ok();
    assert_eq!(v["dry_run"], json!(true), "{v}");
    assert_eq!(rdy_daemon::live_daemon(s.dir.path()), Some(pid));
    c.alive();
}

#[test]
fn ok11_upgrade_mcp_cli_json_and_errors_match() {
    let (dir, r) = project();
    r.close().unwrap();
    let mut c = McpClient::start(dir.path());
    // the schema: one optional boolean, closed
    let tools = c.tools();
    let t = tools
        .iter()
        .find(|t| t["name"] == "rdy_upgrade")
        .expect("rdy_upgrade is listed");
    assert_eq!(t["description"], DESCRIPTION);
    let schema = &t["inputSchema"];
    assert_eq!(schema["additionalProperties"], json!(false), "{schema}");
    let props: Vec<&String> = schema["properties"].as_object().unwrap().keys().collect();
    assert_eq!(props, ["dry_run"], "{schema}");
    let ty = &schema["properties"]["dry_run"]["type"];
    assert!(
        ty == "boolean" || ty.as_array().is_some_and(|a| a.contains(&json!("boolean"))),
        "{schema}"
    );
    let required = schema["required"].as_array().cloned().unwrap_or_default();
    assert!(required.is_empty(), "{schema}");
    // a session is required
    let e = tool_error(c.call("rdy_upgrade", json!({})));
    assert_eq!(e["code"], "E_NO_SESSION", "{e}");
    // a reader may dry-run, not apply
    c.call(
        "rdy_session",
        json!({"actor": "agent:qa", "run": "qa", "role": "reader"}),
    )
    .ok();
    let e = tool_error(c.call("rdy_upgrade", json!({})));
    assert_eq!(
        (&e["code"], &e["message"], &e["fix"]),
        (
            &json!(E_PERMISSION),
            &json!("role Reader cannot upgrade"),
            &json!("start the session as a writer")
        ),
        "{e}"
    );
    let reader_dry = c.call("rdy_upgrade", json!({"dry_run": true})).ok();
    // a writer applies, with dry_run false by default
    c.call(
        "rdy_session",
        json!({"actor": "agent:qa", "run": "qa", "role": "writer"}),
    )
    .ok();
    let mcp_apply = c.call("rdy_upgrade", json!({})).ok();
    let mcp_dry = c.call("rdy_upgrade", json!({"dry_run": true})).ok();
    assert_eq!(reader_dry, mcp_dry);
    assert_eq!(
        mcp_apply,
        connected(false, &baseline_features(), &Value::Null)
    );
    let e = tool_error(c.call("rdy_upgrade", json!({"dir": "elsewhere"})));
    assert_eq!(e["code"], E_BAD_INPUT, "{e}");
    assert_eq!(e["message"], "rdy_upgrade: unknown parameter \"dir\"");
    let e = tool_error(c.call("rdy_upgrade", json!({"dry_run": "yes"})));
    assert_eq!(e["code"], E_BAD_INPUT, "{e}");
    c.alive();
    drop(c);

    // the CLI prints the same JSON for the same project, and its text
    let cli = |args: &[&str]| {
        let out = std::process::Command::new(rdy_bin())
            .arg("--project")
            .arg(dir.path())
            .args(args)
            .env_remove("RDY_ACTOR")
            .env_remove("RDY_NO_DAEMON")
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap()
    };
    let v: Value = serde_json::from_str(cli(&["--json", "upgrade"]).trim()).unwrap();
    assert_eq!(v, mcp_apply);
    let v: Value = serde_json::from_str(cli(&["--json", "upgrade", "--dry-run"]).trim()).unwrap();
    assert_eq!(v, mcp_dry);
    assert_eq!(cli(&["upgrade"]), "store is current\n");

    // every role that writes applies; the reader dry-runs only
    let local: Arc<dyn Engine> = Arc::new(Local::open(dir.path()).unwrap());
    for role in [Role::Writer, Role::Reviewer, Role::Approver] {
        let mut st = state(Arc::clone(&local), role);
        assert_eq!(upgrade(&mut st, None).unwrap(), mcp_apply, "{role:?}");
    }
    let mut st = state(Arc::clone(&local), Role::Reader);
    let e = upgrade(&mut st, Some(false)).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_PERMISSION,
            "role Reader cannot upgrade",
            "start the session as a writer"
        )
    );
    assert_eq!(upgrade(&mut st, Some(true)).unwrap(), mcp_dry);

    // the five rows: the four new codes and the added E_STORE_FORMAT condition
    for row in [
        (
            E_STORE_FORMAT,
            "this rdy does not open the store as it is: an older or newer store format, an unknown store feature, an invalid feature manifest, or a store that needs an upgrade",
            "follow the refusal's fix: open the project with the rdy it names, dump and load, or run rdy upgrade",
        ),
        (
            E_STORE_READ_ONLY,
            "an unsupported feature allows reads only, or its store has no lock file",
            "open the project with the rdy build named by the refusal",
        ),
        (
            E_UPGRADE_MOVED,
            "the store changed after the upgrade was planned",
            "run rdy_upgrade with dry_run true, or rdy upgrade --dry-run, then retry",
        ),
        (
            E_UPGRADE_BACKUP,
            "the backup is incomplete, differs from the store, or contains a link or special file",
            "follow the reported backup fix, then retry",
        ),
        (
            E_UPGRADE_FAILED,
            "the upgrade batch is durable, but the store check or fsck failed",
            "stop the daemon and restore the backup before retrying",
        ),
    ] {
        assert!(ERROR_ROWS.contains(&row), "{row:?}");
    }
    // the refusals as the engine maps them, which both surfaces carry unchanged
    let upgrade_error = |failure| {
        RdyError::from(StoreError::Upgrade {
            failure,
            backup: None,
        })
    };
    for (failure, code, message, fix) in [
        (
            UpgradeFailure::Moved,
            E_UPGRADE_MOVED,
            "the store changed after the upgrade was planned",
            "run rdy_upgrade with dry_run true, or rdy upgrade --dry-run, then retry",
        ),
        (
            UpgradeFailure::BackupMismatch,
            E_UPGRADE_BACKUP,
            "the upgrade backup is incomplete or does not match the store",
            "move the incomplete backup aside, then retry",
        ),
        (
            UpgradeFailure::BackupFileType,
            E_UPGRADE_BACKUP,
            "the store contains a link or a special file",
            "remove the link or special file, then retry the upgrade",
        ),
        (
            UpgradeFailure::FsckFailed,
            E_UPGRADE_FAILED,
            "the upgrade batch is durable, but the store check failed",
            "stop the daemon and restore the backup before retrying",
        ),
        (
            UpgradeFailure::Limit,
            E_LIMIT,
            "the upgrade backup exceeds its file or path limit",
            "reduce the store's extra files, then retry",
        ),
    ] {
        let e = upgrade_error(failure);
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (code, message, fix)
        );
    }
    let e = RdyError::from(StoreError::UpgradeRequired);
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_STORE_FORMAT,
            "the store needs an upgrade before it can open",
            "run the host's store maintenance, or rdy upgrade"
        )
    );
    let e = RdyError::from(StoreError::ReadOnly {
        feature: "f".into(),
        required_rdy: "9.9.9".into(),
    });
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_STORE_READ_ONLY,
            "store feature f permits reads only",
            "open the project with rdy 9.9.9 or later to write"
        )
    );
}
