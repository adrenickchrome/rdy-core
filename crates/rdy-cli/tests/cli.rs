#![cfg(unix)]
// Unix only: it sets file modes. FW-XP1 Phase 4 ports it.
use serde_json::Value;
use std::process::Command;

/// A user config naming `human:tester` as the human actor, and the environment that points at it.
fn human_config() -> (tempfile::TempDir, (&'static str, std::path::PathBuf)) {
    let cfg = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(cfg.path().join("rdy")).unwrap();
    std::fs::write(cfg.path().join("rdy/config.yaml"), "actor: human:tester\n").unwrap();
    let env = ("XDG_CONFIG_HOME", cfg.path().to_path_buf());
    (cfg, env)
}

/// `rdy --no-daemon <args>` in `dir` as the human `human:tester` (these tests drive the direct
/// path; the daemon path is covered by rdy-qa's cli_daemon).
fn rdy_cmd(dir: &std::path::Path, args: &[&str]) -> (tempfile::TempDir, Command) {
    let (cfg, (k, v)) = human_config();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rdy"));
    cmd.arg("--no-daemon")
        .args(args)
        .current_dir(dir)
        .env("RDY_ACTOR", "human:tester")
        .env(k, v);
    (cfg, cmd)
}

fn rdy(dir: &std::path::Path, args: &[&str]) -> (bool, String, String) {
    let (_cfg, mut cmd) = rdy_cmd(dir, args);
    let out = cmd.output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into(),
        String::from_utf8_lossy(&out.stderr).into(),
    )
}

fn json(dir: &std::path::Path, args: &[&str]) -> Value {
    let mut a = vec!["--json"];
    a.extend_from_slice(args);
    let (ok, out, err) = rdy(dir, &a);
    assert!(ok, "rdy {args:?} failed: {err}");
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("bad json from {args:?}: {e}\n{out}"))
}

#[test]
fn init_status_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    let (ok, _, err) = rdy(dir.path(), &["status"]);
    assert!(!ok);
    assert!(
        err.contains("E_NO_PROJECT") && err.contains("rdy init"),
        "{err}"
    );
    json(dir.path(), &["init"]);
    let st = json(dir.path(), &["status"]);
    assert_eq!(st["pack"], "rdy-standard");
    assert_eq!(st["landings"], 0);
    let sub = dir.path().join("deep/er");
    std::fs::create_dir_all(&sub).unwrap();
    assert_eq!(
        json(&sub, &["status"])["pack"],
        "rdy-standard",
        "project found by walking up"
    );
}

#[test]
fn import_propose_approve_render_fsck() {
    // build an export with the library, then drive the rest through the CLI
    let src = tempfile::tempdir().unwrap();
    let export = tempfile::tempdir().unwrap();
    {
        use rdy_engine::*;
        use rdy_model::Role;
        let mut r = Rdy::init(src.path(), "rdy-standard").unwrap();
        let w = Session {
            actor: "agent:a".into(),
            run: None,
            role: Role::Writer,
            kind: rdy_engine::SessionKind::Human,
        };
        r.write(&w, WriteRequest { view: "work".into(), ops: serde_json::from_value(serde_json::json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Hello", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
            {"op": "create", "type": "term", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."}
        ])).unwrap(), justification: None, message: String::new() }).unwrap();
        r.read()
            .export(&Scope::View("work".into()), export.path())
            .unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    json(dir.path(), &["init"]);
    let ex = export.path().to_str().unwrap();
    // import needs an approver; the CLI import runs as approver because it can carry accepted decisions
    json(dir.path(), &["import", ex, "--view", "migrate"]);
    let p = json(
        dir.path(),
        &[
            "propose",
            "--view",
            "migrate",
            "--title",
            "migrate",
            "--summary",
            "test summary",
        ],
    );
    let pid = p["id"].as_str().unwrap().to_string();
    let landed = json(dir.path(), &["approve", &pid]);
    assert_eq!(landed["landing"].as_str().unwrap().len(), 64);
    let log = json(dir.path(), &["log"]);
    assert_eq!(log[0]["approved_by"], "human:tester");
    let site = dir.path().join("site");
    let r = json(dir.path(), &["render", "--out", site.to_str().unwrap()]);
    // the home page, the doc and the glossary its term earns
    assert_eq!(r["pages"], 3);
    assert!(site.join("index.html").exists());
    let f = json(dir.path(), &["fsck"]);
    assert_eq!(f["landings"], 1);
    assert!(f["frames"].as_u64().unwrap() > 0);
}

fn rdy_env(dir: &std::path::Path, editor: &str, args: &[&str]) -> (bool, String, String) {
    let (_cfg, mut cmd) = rdy_cmd(dir, args);
    let out = cmd.env("EDITOR", editor).output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into(),
        String::from_utf8_lossy(&out.stderr).into(),
    )
}

fn project_with_term() -> (tempfile::TempDir, String) {
    use rdy_engine::*;
    use rdy_model::Role;
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
        kind: rdy_engine::SessionKind::Human,
    };
    let res = r.write(&w, WriteRequest { view: "work".into(), ops: serde_json::from_value(serde_json::json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Hello", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let id = res.created["$t"].clone();
    drop(r);
    (dir, id)
}

#[test]
fn edit_runs_editor_with_args_and_failure_changes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, id) = project_with_term();
    let script = dir.path().join("ed.sh");
    std::fs::write(&script, "#!/bin/sh\n[ \"$1\" = \"--flag\" ] && shift\nsed 's/^body: .*/body: Edited by script/' \"$1\" > \"$1.new\" && mv \"$1.new\" \"$1\"\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let editor = format!("{} --flag", script.display());
    let (ok, _, err) = rdy_env(dir.path(), &editor, &["edit", &id, "--view", "work"]);
    assert!(ok, "{err}");
    assert_eq!(
        json(dir.path(), &["show", &id, "--scope", "work"])["body"],
        "Edited by script"
    );

    let (dir2, id2) = project_with_term();
    let (ok, _, _) = rdy_env(dir2.path(), "false", &["edit", &id2, "--view", "work"]);
    assert!(!ok);
    assert_eq!(
        json(dir2.path(), &["show", &id2, "--scope", "work"])["body"],
        "Balances."
    );
}

#[test]
fn fsck_detects_corruption() {
    let (dir, _) = project_with_term();
    // Seal segment 0 by starting segment 1: Store::open only repairs (truncates) the tail of the
    // CURRENT segment, so corruption must be in a sealed one for fsck to see it.
    // M2a1 (fmt2): the store is .rdy/store again
    let segs = dir.path().join(".rdy/store/segments");
    let seg = segs.join("seg-000000.log");
    std::fs::File::create(segs.join("seg-000001.log")).unwrap();
    let (ok, _, err) = rdy(dir.path(), &["fsck"]);
    assert!(ok, "{err}");
    let mut bytes = std::fs::read(&seg).unwrap();
    // inside the first frame's payload (header is 37 bytes)
    let at = 40.min(bytes.len() - 1);
    bytes[at] ^= 0xff;
    std::fs::write(&seg, bytes).unwrap();
    let (ok, _, err) = rdy(dir.path(), &["fsck"]);
    assert!(!ok, "fsck passed on a corrupted segment; stderr: {err}");
}

#[test]
fn init_honours_the_project_flag() {
    let cwd = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    json(
        cwd.path(),
        &["--project", target.path().to_str().unwrap(), "init"],
    );
    assert!(target.path().join(".rdy.yaml").exists());
    assert!(!cwd.path().join(".rdy").exists());
}

fn seed_and_land(dir: &std::path::Path, title: &str) -> String {
    use rdy_engine::*;
    use rdy_model::Role;
    let mut r = Rdy::open(dir).unwrap();
    let w = Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
        kind: rdy_engine::SessionKind::Human,
    };
    let a = Session {
        actor: "human:tester".into(),
        run: None,
        role: Role::Approver,
        kind: rdy_engine::SessionKind::Human,
    };
    let doc = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .children(None)
        .first()
        .map(|d| d.id.to_string());
    let ops = match doc {
        None => {
            serde_json::json!([{"op": "create", "type": "doc", "fields": {"title": title, "doc_type": "page"}}])
        }
        Some(d) => serde_json::json!([{"op": "update", "id": d, "fields": {"title": title}}]),
    };
    let res = r
        .write(
            &w,
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(ops).unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
    let p = r.propose(&w, "work", title, Some("test summary")).unwrap();
    r.land(&a, &p.id).unwrap();
    res.created.values().next().cloned().unwrap_or_default()
}

#[test]
fn versions_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    json(dir.path(), &["init"]);
    let doc = seed_and_land(dir.path(), "First title");
    let v = json(dir.path(), &["tag", "v0.1.0", "-m", "first cut"]);
    assert_eq!(v["name"], "v0.1.0");
    assert_eq!(v["message"], "first cut");
    let (ok, _, err) = rdy(dir.path(), &["tag", "0.1.0"]);
    assert!(!ok && err.contains("E_TAG_EXISTS"), "{err}");

    let tags = json(dir.path(), &["tags"]);
    assert_eq!(tags["current"], "v0.1.0");
    assert_eq!(tags["versions"].as_array().unwrap().len(), 1);
    assert_eq!(
        json(dir.path(), &["status"])["version_position"]["label"],
        "v0.1.0"
    );

    seed_and_land(dir.path(), "Second title");
    let st = json(dir.path(), &["status"]);
    assert_eq!(st["version_position"]["label"], "v0.1.0 + 1 landing");
    let (_, text, _) = rdy(dir.path(), &["status"]);
    assert!(text.contains("canon: v0.1.0 + 1 landing"), "{text}");

    let log = json(dir.path(), &["log"]);
    assert_eq!(
        log[0]["tags"],
        serde_json::json!([]),
        "newest landing first"
    );
    assert_eq!(log[1]["tags"], serde_json::json!(["v0.1.0"]));
    let (_, text, _) = rdy(dir.path(), &["log"]);
    assert!(text.contains("[v0.1.0]"), "{text}");

    let old = json(dir.path(), &["show", &doc, "--scope", "tag:v0.1.0"]);
    assert_eq!(old["fields"]["title"], "First title");
    assert_eq!(
        json(dir.path(), &["show", &doc])["fields"]["title"],
        "Second title"
    );
    let (ok, _, err) = rdy(dir.path(), &["show", &doc, "--scope", "tag:v9.9.9"]);
    assert!(!ok && err.contains("E_NOT_FOUND"), "{err}");
    assert_eq!(json(dir.path(), &["fsck"])["versions"], 1);
}

#[test]
fn status_warns_about_legacy_colon_views() {
    let dir = tempfile::tempdir().unwrap();
    json(dir.path(), &["init"]);
    {
        use rdy_model::*;
        let mut r = rdy_engine::Rdy::open(dir.path()).unwrap();
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("title".to_string(), FieldValue::Str("Legacy".into()));
        fields.insert("doc_type".to_string(), FieldValue::Str("page".into()));
        let block = Block {
            id: BlockId::parse("d-AAAAB").unwrap(),
            block_type: "doc".into(),
            parent: None,
            order: "V".into(),
            fields,
            body: String::new(),
            links: Default::default(),
            pins: Default::default(),
            read_identity: None,
        };
        let prov = Provenance {
            actor: "agent:a".into(),
            run: None,
            role: Role::Writer,
            at_ms: 0,
        };
        r.store_mut()
            .commit_to_view(
                "old:view",
                Change {
                    view: "old:view".into(),
                    ops: vec![Op::Create { block }],
                    deps: vec![],
                    provenance: prov,
                    justification: None,
                    message: String::new(),
                    links: ChangeLinks::default(),
                    read_identity: None,
                },
            )
            .unwrap();
    }
    let st = json(dir.path(), &["status"]);
    let warnings = st["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].as_str().unwrap().contains("old:view"));
    let p = json(
        dir.path(),
        &["propose", "--view", "view:old:view", "--title", "legacy"],
    );
    assert!(p["id"].is_string());
}

#[test]
fn tags_with_no_versions_says_so() {
    let dir = tempfile::tempdir().unwrap();
    json(dir.path(), &["init"]);
    let (ok, text, _) = rdy(dir.path(), &["tags"]);
    assert!(ok);
    assert_eq!(text.trim(), "no versions yet");
}
#[test]
fn check_exit_codes_and_pack_outside_a_project() {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(
        t.path().join("ok.yaml"),
        "- id: d-AAAAA\n  type: doc\n  order: V\n  fields:\n    title: T\n    doc_type: page\n",
    )
    .unwrap();
    let (ok, _, err) = rdy(t.path(), &["check", "ok.yaml"]);
    assert!(!ok && err.contains("E_NO_PROJECT"), "{err}");
    let (ok, out, _) = rdy(t.path(), &["check", "ok.yaml", "--pack", "rdy-standard"]);
    assert!(ok && out.contains("ok"), "{out}");
    std::fs::write(
        t.path().join("index.yaml"),
        "- id: d-BBBBB\n  type: doc\n  order: V\n  fields:\n    title: T\n    doc_type: page\n",
    )
    .unwrap();
    let (ok, out, _) = rdy(t.path(), &["check", ".", "--pack", "rdy-standard"]);
    assert!(!ok, "an error exits 1");
    assert!(out.contains("index.yaml:1:1: E_BAD_NAME"), "{out}");
    let (_, out, _) = rdy(
        t.path(),
        &["--json", "check", ".", "--pack", "rdy-standard"],
    );
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v[0]["code"], "E_BAD_NAME");
}

#[test]
fn new_ls_and_paths_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    json(dir.path(), &["init"]);
    let f = json(dir.path(), &["new", "folder", "wallets/cards"]);
    assert_eq!(
        f["created"].as_object().unwrap().len(),
        2,
        "wallets and cards"
    );
    json(
        dir.path(),
        &[
            "new",
            "doc",
            "wallets/transactions",
            "--title",
            "Transactions",
        ],
    );
    let (ok, _, err) = rdy(dir.path(), &["new", "doc", "wallets/transactions"]);
    assert!(!ok && err.contains("E_PATH_CLASH"), "{err}");
    let ls = json(dir.path(), &["ls", "--scope", "work"]);
    assert_eq!(ls[0]["path"], "wallets");
    let (_, text, _) = rdy(dir.path(), &["ls", "--scope", "work"]);
    assert!(
        text.contains("wallets/  folder") && text.contains("  transactions  doc"),
        "{text}"
    );
    let p = json(
        dir.path(),
        &[
            "propose",
            "--view",
            "work",
            "--title",
            "tree",
            "--summary",
            "test summary",
        ],
    );
    json(dir.path(), &["approve", p["id"].as_str().unwrap()]);
    let show = json(dir.path(), &["show", "wallets/transactions"]);
    assert_eq!(show["fields"]["title"], "Transactions");
    json(dir.path(), &["tag", "v0.1.0"]);
    let old = json(
        dir.path(),
        &["show", "--scope", "tag:v0.1.0", "wallets/transactions"],
    );
    assert_eq!(old["id"], show["id"]);
    assert!(json(dir.path(), &["impact", "wallets/transactions"])["items"].is_array());
}

/// Review Focus 6: `rdy --json log` names each landing's assurance; the text is unchanged.
#[test]
fn repr1_log_json_shows_each_landings_assurance() {
    let dir = tempfile::tempdir().unwrap();
    json(dir.path(), &["init"]);
    json(dir.path(), &["new", "folder", "probe"]);
    json(dir.path(), &["new", "doc", "probe/one", "--title", "One"]);
    let p = json(
        dir.path(),
        &[
            "propose",
            "--view",
            "work",
            "--title",
            "p",
            "--summary",
            "test summary",
        ],
    );
    json(dir.path(), &["approve", p["id"].as_str().unwrap()]);
    let log = json(dir.path(), &["log"]);
    assert_eq!(log[0]["assurance"], "local");
    assert_eq!(log[0]["approved_by"], "human:tester");
    let (_, text, _) = rdy(dir.path(), &["log"]);
    assert!(!text.contains("local"), "{text}");
}

/// Guard (final review fix round 1): `rdy status` counts each view's changes from its head
/// (`ViewHead.len`), which must equal the view's included set.
#[test]
fn chain1_status_counts_each_views_changes() {
    use rdy_engine::*;
    use rdy_model::Role;
    let dir = tempfile::tempdir().unwrap();
    json(dir.path(), &["init"]);
    {
        let mut r = Rdy::open(dir.path()).unwrap();
        let w = Session {
            actor: "agent:a".into(),
            run: None,
            role: Role::Writer,
            kind: rdy_engine::SessionKind::Human,
        };
        for (view, title) in [("work", "One"), ("work", "Two"), ("side", "Three")] {
            r.write(
                &w,
                WriteRequest {
                    view: view.into(),
                    ops: serde_json::from_value(serde_json::json!([{"op": "create", "type": "doc", "fields": {"title": title, "doc_type": "page"}}])).unwrap(),
                    justification: None,
                    message: String::new(),
                },
            )
            .unwrap();
        }
    }
    let counts = |st: &Value| -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> = st["views"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                (
                    v["name"].as_str().unwrap().to_string(),
                    v["changes"].as_u64().unwrap(),
                )
            })
            .collect();
        v.sort();
        v
    };
    let st = json(dir.path(), &["status"]);
    assert_eq!(
        counts(&st),
        vec![("side".to_string(), 1), ("work".to_string(), 2)]
    );
    let p = json(
        dir.path(),
        &[
            "propose",
            "--view",
            "work",
            "--title",
            "two",
            "--summary",
            "test summary",
        ],
    );
    json(dir.path(), &["approve", p["id"].as_str().unwrap()]);
    // F11 (Demo findings): a view whose changes all landed holds none and leaves the list.
    let st = json(dir.path(), &["status"]);
    assert_eq!(counts(&st), vec![("side".to_string(), 1)]);
}
