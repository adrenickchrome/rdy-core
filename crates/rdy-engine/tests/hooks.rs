//! m4a1: the shared hook checks (FW-HOOK D-NNQ0A, D-GHR0Y; FW-L3 D-54CKQ refined by D-DC5D1;
//! D-KG0V6). The code check warns when staged paths touch code anchored to a decision and the
//! message has no `Implements:` value; the edit check warns when an agent edited working-copy
//! members. Inputs are one closed form with fixed bounds; checks change nothing; an engine with
//! no local host refuses install and remove.
mod common;

use rdy_engine::hooks::{
    CodeCheck, EditCheck, FileResult, Harness, HookCheck, HookFile, HookHost, HookInstall,
    HookKind, HookRequest, HookTarget, HookWarning, MAX_HOOK_CHECKOUT, MAX_HOOK_HEAD,
    MAX_HOOK_INPUT, MAX_HOOK_MESSAGE, MAX_HOOK_PATH, MAX_HOOK_PATHS, RunContext, check_code,
    check_edit,
};
use rdy_engine::wc::files::checkout;
use rdy_engine::wc::ignore::Ignore;
use rdy_engine::wc::layout::read_tree;
use rdy_engine::wc::project::project_state;
use rdy_engine::wc::state::{Place, WcState};
use rdy_engine::*;
use rdy_model::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn writer() -> Session {
    Session::human("human:qa".into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human("human:qa".into(), Role::Approver).unwrap()
}

fn ops(v: Value) -> Vec<OpInput> {
    parse_ops(serde_json::from_value(v).unwrap()).unwrap()
}

fn put(r: &mut Rdy, s: &Session, view: &str, v: Value) -> BTreeMap<String, String> {
    r.write(
        s,
        WriteRequest {
            view: view.into(),
            ops: ops(v),
            justification: None,
            message: format!("write to {view}"),
        },
    )
    .unwrap_or_else(|e| panic!("write to {view}: {e}"))
    .created
}

fn land(r: &mut Rdy, view: &str) {
    common::answer_open_items(r, &writer(), view);
    let p = r
        .propose(&writer(), view, view, Some(common::TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e}"));
    common::confirm_exceptions(r, &approver(), &p);
    r.land(&approver(), &p.id)
        .unwrap_or_else(|e| panic!("land {view}: {e}"));
}

fn decide(r: &mut Rdy, view: &str, parent: &str, statement: &str) -> String {
    r.decide(
        &writer(),
        view,
        DecisionInput {
            parent: parent.into(),
            statement: statement.into(),
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("decide {statement}: {e}"))
    .created["$decision"]
        .clone()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=qa",
            "-c",
            "user.email=qa",
            "-c",
            "commit.gpgsign=false",
            "-c",
            &format!(
                "core.hooksPath={}",
                rdy_engine::code_index::empty_hooks_dir().display()
            ),
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write_file(dir: &Path, path: &str, bytes: &[u8]) {
    let file = dir.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, bytes).unwrap();
}

/// Writes `path` and commits it with `message`; returns the commit.
fn commit_file(dir: &Path, path: &str, message: &str) -> String {
    write_file(dir, path, format!("{path} {message}\n").as_bytes());
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--no-verify", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

fn link_repos(project: &Path, repos: &[(&str, &Path)]) {
    let names: Vec<&str> = repos.iter().map(|r| r.0).collect();
    let mut yaml = std::fs::read_to_string(project.join(".rdy.yaml")).unwrap();
    yaml.push_str(&format!("code_repos: [{}]\n", names.join(", ")));
    std::fs::write(project.join(".rdy.yaml"), yaml).unwrap();
    std::fs::create_dir_all(project.join(".rdy")).unwrap();
    let mut local = String::from("code_paths:\n");
    for (n, p) in repos {
        local.push_str(&format!("  {n}: \"{}\"\n", p.display()));
    }
    std::fs::write(project.join(".rdy/local.yaml"), local).unwrap();
}

/// A landed canon and a linked checkout `app`. Canon: doc `spec` with section `s` (live decision
/// `d`, claim `c` that implements `d`), section `notes` (claim `u`, which nothing governs, with a
/// `reflects` glob on `src/reflected.rs`), a rejected decision `rej` and a superseded decision
/// `old` (superseded by `new`). View `open` holds the still-proposed decision `prop`, never
/// landed. Each commit of the checkout cites one of them on one file.
struct World {
    _project: tempfile::TempDir,
    _checkout: tempfile::TempDir,
    r: Rdy,
    repo: PathBuf,
    ids: BTreeMap<&'static str, String>,
}

impl World {
    fn new() -> World {
        let project = tempfile::tempdir().unwrap();
        let checkout = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(project.path(), "rdy-standard").unwrap();
        let mut ids: BTreeMap<&'static str, String> = BTreeMap::new();
        let made = put(
            &mut r,
            &writer(),
            "seed",
            json!([
                {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
                {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Refunds"}},
                {"op": "create", "type": "section", "id": "$notes", "parent": "$doc", "fields": {"title": "Notes"}},
                {"op": "create", "type": "claim", "id": "$u", "parent": "$notes", "body": "A plain note.", "fields": {"reflects": ["src/reflected.rs"]}}
            ]),
        );
        for (k, v) in [
            ("doc", "$doc"),
            ("s", "$s"),
            ("notes", "$notes"),
            ("u", "$u"),
        ] {
            ids.insert(k, made[v].clone());
        }
        let d = decide(&mut r, "seed", &ids["s"], "Refunds take five days");
        let old = decide(&mut r, "seed", &ids["s"], "Refunds go to the card");
        let c = put(
            &mut r,
            &writer(),
            "seed",
            json!([{"op": "create", "type": "claim", "id": "$c", "parent": ids["s"], "body": "The card is refunded.", "links": {"implements": [d]}}]),
        )["$c"]
            .clone();
        let rej = put(
            &mut r,
            &approver(),
            "seed",
            json!([{"op": "create", "type": "decision", "id": "$x", "parent": ids["notes"],
                "fields": {"statement": "Refunds in cash", "status": "rejected",
                           "decided_at": "2021-01-10T09:00:00Z", "recorded_at": "2021-01-10T09:00:00Z",
                           "options": ["yes", "no"]}}]),
        )["$x"]
            .clone();
        land(&mut r, "seed");
        let new = r
            .supersede(
                &writer(),
                "sup",
                &old,
                None,
                DecisionInput {
                    parent: ids["s"].clone(),
                    statement: "Refunds go to the original method".into(),
                    ..Default::default()
                },
            )
            .unwrap_or_else(|e| panic!("supersede: {e}"))
            .created["$decision"]
            .clone();
        land(&mut r, "sup");
        let prop = decide(&mut r, "open", &ids["s"], "Refunds need a receipt");
        for (k, v) in [
            ("d", d),
            ("c", c),
            ("rej", rej),
            ("old", old),
            ("new", new),
            ("prop", prop),
        ] {
            ids.insert(k, v);
        }

        git(checkout.path(), &["init", "-q", "--initial-branch=main"]);
        link_repos(project.path(), &[("app", checkout.path())]);
        let repo = std::fs::canonicalize(checkout.path()).unwrap();
        let cite = |id: &str| format!("feat: work\n\nImplements: {id}");
        commit_file(&repo, "src/direct.rs", &cite(&ids["d"]));
        commit_file(&repo, "src/governed.rs", &cite(&ids["c"]));
        commit_file(&repo, "src/section.rs", &cite(&ids["s"]));
        commit_file(&repo, "src/plain.rs", &cite(&ids["u"]));
        commit_file(&repo, "src/rejected.rs", &cite(&ids["rej"]));
        commit_file(&repo, "src/superseded.rs", &cite(&ids["old"]));
        commit_file(&repo, "src/proposed.rs", &cite(&ids["prop"]));
        commit_file(&repo, "src/unknown.rs", &cite("D-ZZZZZ"));
        commit_file(&repo, "src/reflected.rs", "feat: reflected, cites nothing");
        commit_file(&repo, "src/none.rs", "chore: no trailer");
        World {
            _project: project,
            _checkout: checkout,
            r,
            repo,
            ids,
        }
    }

    fn head(&self) -> Option<String> {
        Some(git(&self.repo, &["rev-parse", "HEAD"]))
    }

    fn req(&self, paths: &[&str], message: &str) -> CodeCheck {
        CodeCheck {
            repo: "app".into(),
            checkout: self.repo.display().to_string(),
            head: self.head(),
            paths: paths.iter().map(|p| p.to_string()).collect(),
            message: message.into(),
        }
    }

    fn check(&self, req: &CodeCheck) -> HookCheck {
        check_code(&self.r.read(), req, &RunContext::new())
            .unwrap_or_else(|e| panic!("check {req:?}: {e}"))
    }

    /// The paths of the one `W_HOOK_NO_CITATION` warning, or none when the check is clean.
    fn warned(&self, paths: &[&str], message: &str) -> Vec<String> {
        let got = self.check(&self.req(paths, message));
        assert!(got.checked, "{got:?}");
        assert_eq!(got.kind, HookKind::Code);
        match got.warnings.as_slice() {
            [] => vec![],
            [w] => {
                assert_eq!(w.code, W_HOOK_NO_CITATION);
                assert_eq!(w.message, HOOK_NO_CITATION_MESSAGE);
                assert_eq!(w.fix, HOOK_NO_CITATION_FIX);
                w.paths.clone()
            }
            more => panic!("one warning at most: {more:?}"),
        }
    }
}

fn reason_of(got: &HookCheck) -> Option<&'static str> {
    assert!(!got.checked, "an unavailable check is never clean: {got:?}");
    let w = got
        .warnings
        .iter()
        .find(|w| w.code == W_HOOK_UNAVAILABLE)
        .unwrap_or_else(|| panic!("no unavailable warning: {got:?}"));
    assert_eq!(w.fix, HOOK_UNAVAILABLE_FIX);
    assert_eq!(
        w.message,
        format!(
            "The rdy hook could not check this action: {}.",
            w.reason.unwrap()
        )
    );
    w.reason
}

#[test]
fn m4a1_code_warns_only_for_anchored_paths_without_citations() {
    let w = World::new();
    // a direct decision, a block it governs and a section holding both each anchor
    for p in ["src/direct.rs", "src/governed.rs", "src/section.rs"] {
        assert_eq!(w.warned(&[p], ""), vec![p.to_string()], "{p}");
    }
    // a claim nothing governs, a text naming no block, an uncited file, a path absent at HEAD
    for p in [
        "src/plain.rs",
        "src/unknown.rs",
        "src/none.rs",
        "src/new.rs",
    ] {
        assert!(w.warned(&[p], "").is_empty(), "{p}");
    }
    // several paths: the anchored ones, in the order given
    let all = [
        "src/none.rs",
        "src/section.rs",
        "src/plain.rs",
        "src/direct.rs",
    ];
    assert_eq!(
        w.warned(&all, "fix: tidy\n\nbody text"),
        vec!["src/section.rs".to_string(), "src/direct.rs".to_string()]
    );
    // any nonempty Implements value is a citation: this checks omission only
    let d = &w.ids["d"];
    assert!(
        w.warned(&all, &format!("fix: tidy\n\nImplements: {d}"))
            .is_empty()
    );
    assert!(
        w.warned(&all, "fix: tidy\n\nImplements: s-NOTHING")
            .is_empty()
    );
    // an empty message, and an empty trailer value, cite nothing
    assert_eq!(w.warned(&["src/direct.rs"], "").len(), 1);
    assert_eq!(
        w.warned(&["src/direct.rs"], "fix: tidy\n\nImplements: \n")
            .len(),
        1
    );
    // the listed paths stop at 20; the rest are counted
    let many: Vec<String> = (0..25).map(|_| "src/direct.rs".to_string()).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    let got = w.check(&w.req(&refs, ""));
    assert_eq!(got.warnings[0].paths.len(), 20);
    assert_eq!(got.warnings[0].omitted, 5);
    let v = serde_json::to_value(&got).unwrap();
    assert_eq!(v["kind"], "code");
    assert_eq!(v["checked"], true);
    assert_eq!(v["warnings"][0]["code"], "W_HOOK_NO_CITATION");
    assert_eq!(v["warnings"][0]["omitted"], 5);
}

#[test]
fn m4a1_code_preserves_trailer_rules_and_status_independence() {
    let w = World::new();
    let d = w.ids["d"].clone();
    let p = ["src/direct.rs"];
    // repeated headers, a key in any case, folded and comma-joined values: git's trailer rules
    for message in [
        format!("feat: x\n\nImplements: \nImplements: {d}"),
        format!("feat: x\n\nimplements: {d}"),
        format!("feat: x\n\nIMPLEMENTS: s-AAAAA,\n  {d}"),
        format!("feat: x\n\nSigned-off-by: qa\nImplements: , {d}"),
        format!("feat: x\n\nImplements:\n {d}"),
    ] {
        assert!(w.warned(&p, &message).is_empty(), "{message:?}");
    }
    // no trailer block: only commas, the subject line, a paragraph after the trailers, a body line
    for message in [
        "feat: x\n\nImplements: , ,".to_string(),
        format!("Implements: {d}"),
        format!("feat: x\n\nImplements: {d}\n\nMore words after it."),
        format!("feat: x\n\nThis implements {d} in prose."),
        format!("feat: x\n\nImplements-Not: {d}"),
    ] {
        assert_eq!(w.warned(&p, &message).len(), 1, "{message:?}");
    }
    // every decision status anchors, a still-proposed one included
    for path in [
        "src/rejected.rs",
        "src/superseded.rs",
        "src/proposed.rs",
        "src/direct.rs",
    ] {
        assert_eq!(
            w.warned(&[path], "chore: x"),
            vec![path.to_string()],
            "{path}"
        );
    }
    // a reflects match alone is no implementation anchor
    assert!(w.warned(&["src/reflected.rs"], "chore: x").is_empty());
    // 128 citation values are read; one more is the input limit
    let values = |n: usize| {
        (0..n)
            .map(|i| format!("s-{i:05}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    assert!(
        w.warned(&p, &format!("feat: x\n\nImplements: {}", values(128)))
            .is_empty()
    );
    let e = check_code(
        &w.r.read(),
        &w.req(&p, &format!("feat: x\n\nImplements: {}", values(129))),
        &RunContext::new(),
    )
    .unwrap_err();
    assert_eq!(e.code, E_LIMIT);
    assert_eq!(e.message, "The hook input exceeds 128 citation values.");
    assert_eq!(e.fix, HOOK_LIMIT_FIX);
}

#[test]
fn m4a1_code_validates_the_callers_checkout_and_head() {
    let w = World::new();
    let head = w.head().unwrap();
    // a registered worktree of the same repository, on a branch where the anchor is absent
    let wt_parent = tempfile::tempdir().unwrap();
    let wt = wt_parent.path().join("wt");
    let first = git(&w.repo, &["rev-list", "--max-parents=0", "HEAD"]);
    git(
        &w.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            wt.to_str().unwrap(),
            &first,
        ],
    );
    let wt = std::fs::canonicalize(&wt).unwrap();
    let side_head = git(&wt, &["rev-parse", "HEAD"]);
    let mut req = w.req(&["src/section.rs"], "");
    req.checkout = wt.display().to_string();
    req.head = Some(side_head.clone());
    let got = w.check(&req);
    assert!(got.checked && got.warnings.is_empty(), "{got:?}");
    // its own anchor warns there, while the configured checkout has no such file
    commit_file(
        &wt,
        "src/side.rs",
        &format!("feat: side\n\nImplements: {}", w.ids["d"]),
    );
    req.head = Some(git(&wt, &["rev-parse", "HEAD"]));
    req.paths = vec!["src/side.rs".into()];
    assert_eq!(w.check(&req).warnings.len(), 1);
    assert!(w.warned(&["src/side.rs"], "").is_empty());
    // the HEAD moved since collection: history unavailable
    req.head = Some(side_head);
    assert_eq!(reason_of(&w.check(&req)), Some("history unavailable"));
    // a commit that is not HEAD, an unborn claim on a born HEAD
    let mut req = w.req(&["src/direct.rs"], "");
    req.head = Some(first.clone());
    assert_eq!(reason_of(&w.check(&req)), Some("history unavailable"));
    req.head = None;
    assert_eq!(reason_of(&w.check(&req)), Some("history unavailable"));
    // another repository, a folder inside the checkout, a missing folder: checkout unavailable
    let other = tempfile::tempdir().unwrap();
    git(other.path(), &["init", "-q", "--initial-branch=main"]);
    let other_head = commit_file(other.path(), "x.rs", "x");
    let mut req = w.req(&["src/direct.rs"], "");
    req.checkout = std::fs::canonicalize(other.path())
        .unwrap()
        .display()
        .to_string();
    req.head = Some(other_head);
    assert_eq!(reason_of(&w.check(&req)), Some("checkout unavailable"));
    let mut req = w.req(&["src/direct.rs"], "");
    req.checkout = w.repo.join("src").display().to_string();
    assert_eq!(reason_of(&w.check(&req)), Some("checkout unavailable"));
    let mut req = w.req(&["src/direct.rs"], "");
    req.checkout = w.repo.join("gone").display().to_string();
    assert_eq!(reason_of(&w.check(&req)), Some("checkout unavailable"));
    // a repo name that is not linked is a bad input, with the nearest name
    let mut req = w.req(&["src/direct.rs"], "");
    req.repo = "apq".into();
    let e = check_code(&w.r.read(), &req, &RunContext::new()).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert!(e.message.starts_with("Invalid rdy_hook form: "), "{e}");
    assert!(e.message.contains("did you mean \"app\""), "{e}");
    assert_eq!(e.fix, HOOK_FORM_FIX);
    // the configured checkout at its HEAD is still checked as before
    let mut req = w.req(&["src/direct.rs"], "");
    req.head = Some(head);
    assert_eq!(w.check(&req).warnings.len(), 1);
    // a fresh repository with an unborn HEAD is clean once that is verified
    let fresh = tempfile::tempdir().unwrap();
    git(fresh.path(), &["init", "-q", "--initial-branch=main"]);
    std::fs::write(
        w.r.root().join(".rdy/local.yaml"),
        format!("code_paths:\n  app: \"{}\"\n", fresh.path().display()),
    )
    .unwrap();
    let req = CodeCheck {
        repo: "app".into(),
        checkout: std::fs::canonicalize(fresh.path())
            .unwrap()
            .display()
            .to_string(),
        head: None,
        paths: vec!["src/direct.rs".into()],
        message: String::new(),
    };
    let got = w.check(&req);
    assert!(got.checked && got.warnings.is_empty(), "{got:?}");
}

/// A main working copy checked out from canon, and a registered worktree `wt` checked out too.
struct Wc {
    _dir: tempfile::TempDir,
    _wt_dir: tempfile::TempDir,
    r: Rdy,
    root: PathBuf,
    wt: PathBuf,
    doc: String,
}

fn wc_world() -> Wc {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    put(
        &mut r,
        &writer(),
        "seed",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "guides"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "plan", "title": "Plan", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Steps"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "One step."},
            {"op": "create", "type": "doc", "id": "$t", "fields": {"name": "top", "title": "Top", "doc_type": "overview"}}
        ]),
    );
    land(&mut r, "seed");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let st = r.read();
    let m = st.materialized(&Scope::Canon).unwrap();
    let proj = project_state(&m, st.pack(), "canon", "canon:test", None).unwrap();
    let place = Place::main(&root);
    let mut state = WcState::default();
    let tree = read_tree(&place.root, &Ignore::empty(), st.pack());
    checkout(&place, &mut state, &tree, &proj, false).unwrap();
    // a registered worktree: its pointer names the project, its state names its directory
    let wt_dir = tempfile::tempdir().unwrap();
    let wt = std::fs::canonicalize(wt_dir.path()).unwrap();
    std::fs::write(
        wt.join(".rdy-worktree"),
        format!("project: \"{}\"\nname: wt\n", root.display()),
    )
    .unwrap();
    let state_dir = root.join(".rdy/worktrees/wt");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(state_dir.join("dir.txt"), wt.display().to_string()).unwrap();
    let wt_place = Place::from_pointer(&wt).unwrap();
    let mut wt_state = WcState::default();
    let wt_tree = read_tree(&wt_place.root, &Ignore::empty(), st.pack());
    checkout(&wt_place, &mut wt_state, &wt_tree, &proj, false).unwrap();
    drop(st);
    let doc = "guides/plan.yaml".to_string();
    assert!(root.join(&doc).is_file() && wt.join(&doc).is_file());
    Wc {
        _dir: dir,
        _wt_dir: wt_dir,
        r,
        root,
        wt,
        doc,
    }
}

impl Wc {
    fn check(&self, paths: &[PathBuf]) -> HookCheck {
        let req = EditCheck {
            paths: paths.iter().map(|p| p.display().to_string()).collect(),
        };
        check_edit(&self.r.read(), &req, &RunContext::new())
    }

    /// The paths the one direct-edit warning names; none for a clean check.
    fn warned(&self, paths: &[PathBuf]) -> Vec<String> {
        let got = self.check(paths);
        assert!(got.checked, "{got:?}");
        assert_eq!(got.kind, HookKind::Edit);
        match got.warnings.as_slice() {
            [] => vec![],
            [w] => {
                assert_eq!(w.code, W_HOOK_DIRECT_EDIT);
                assert_eq!(w.message, HOOK_DIRECT_EDIT_MESSAGE);
                assert_eq!(w.fix, HOOK_DIRECT_EDIT_FIX);
                w.paths.clone()
            }
            more => panic!("{more:?}"),
        }
    }
}

const NEW_DOC: &str = "- id: d-NEW01\n  type: doc\n  title: New\n  doc_type: overview\n";

#[test]
fn m4a1_edit_uses_working_copy_membership() {
    let w = wc_world();
    let shown = |p: &Path| vec![p.display().to_string()];
    for root in [&w.root, &w.wt] {
        // a tracked doc, a tracked folder file and the top-level doc
        for rel in [w.doc.as_str(), "guides/_folder.yaml", "top.yaml"] {
            let p = root.join(rel);
            assert_eq!(w.warned(std::slice::from_ref(&p)), shown(&p), "{rel}");
        }
        // a tracked member whose YAML an edit broke still warns
        let p = root.join(&w.doc);
        std::fs::write(&p, "{ not yaml [").unwrap();
        assert_eq!(w.warned(std::slice::from_ref(&p)), shown(&p));
        // a new doc file in a folder, a new folder with its folder file, a managed asset
        write_file(root, "guides/new.yaml", NEW_DOC.as_bytes());
        write_file(
            root,
            "more/_folder.yaml",
            b"id: fd-NEW02\ntype: folder\norder: a0\n",
        );
        write_file(root, "more/inner.yaml", NEW_DOC.as_bytes());
        write_file(root, "_blobs/figure.png", b"png");
        for rel in [
            "guides/new.yaml",
            "more/_folder.yaml",
            "more/inner.yaml",
            "_blobs/figure.png",
        ] {
            let p = root.join(rel);
            assert_eq!(w.warned(std::slice::from_ref(&p)), shown(&p), "{rel}");
        }
        // code files, YAML that is no doc, YAML under a directory that is no folder, a conflict
        // side, a hidden path, a nested blob and a missing untracked file get none
        write_file(root, "src/main.rs", b"fn main() {}\n");
        write_file(root, "guides/config.yaml", b"key: value\n");
        write_file(root, "plain/doc.yaml", NEW_DOC.as_bytes());
        write_file(root, "guides/plan.theirs.yaml", NEW_DOC.as_bytes());
        write_file(root, ".github/ci.yaml", NEW_DOC.as_bytes());
        write_file(root, "_blobs/deep/figure.png", b"png");
        for rel in [
            "src/main.rs",
            "guides/config.yaml",
            "plain/doc.yaml",
            "guides/plan.theirs.yaml",
            ".github/ci.yaml",
            "_blobs/deep/figure.png",
            "guides/absent.yaml",
            "README.md",
        ] {
            assert!(w.warned(&[root.join(rel)]).is_empty(), "{rel}");
        }
        // an ignored path gets none, even a doc file
        std::fs::write(root.join(".rdyignore"), "drafts/\n*.draft.yaml\n").unwrap();
        write_file(root, "guides/x.draft.yaml", NEW_DOC.as_bytes());
        write_file(root, "drafts/_folder.yaml", b"id: fd-NEW03\ntype: folder\n");
        write_file(root, "drafts/d.yaml", NEW_DOC.as_bytes());
        for rel in [
            "guides/x.draft.yaml",
            "drafts/d.yaml",
            "drafts/_folder.yaml",
        ] {
            assert!(w.warned(&[root.join(rel)]).is_empty(), "{rel}");
        }
        // a deleted tracked file still names a member
        std::fs::remove_file(root.join("top.yaml")).unwrap();
        let p = root.join("top.yaml");
        assert_eq!(w.warned(std::slice::from_ref(&p)), shown(&p));
        // so does a tracked doc whose whole folder is gone; an untracked file there does not
        std::fs::remove_dir_all(root.join("guides")).unwrap();
        for rel in [w.doc.as_str(), "guides/_folder.yaml"] {
            let p = root.join(rel);
            assert_eq!(w.warned(std::slice::from_ref(&p)), shown(&p), "{rel}");
        }
        assert!(w.warned(&[root.join("guides/new.yaml")]).is_empty());
    }
    // another project's doc and a path outside every project get none
    let other = wc_world();
    assert!(w.warned(&[other.root.join(&other.doc)]).is_empty());
    assert!(w.warned(&[std::env::temp_dir().join("x.yaml")]).is_empty());
    // a mix keeps only the members, in order
    let mix = [
        w.root.join("src/main.rs"),
        w.wt.join("more/inner.yaml"),
        w.root.join("more/inner.yaml"),
    ];
    assert_eq!(
        w.warned(&mix),
        vec![mix[1].display().to_string(), mix[2].display().to_string()]
    );
}

/// Makes `link` a link to `target` through the store's portable fixture helper. A Windows
/// account without the symbolic-link privilege cannot make one: `false` then, and the caller
/// skips the case (W2).
fn link(target: &Path, link: &Path) -> bool {
    match rdy_store::pio::symlink(target, link) {
        Ok(()) => true,
        Err(e) if cfg!(windows) => {
            eprintln!("skip: no symbolic link on this account ({e})");
            false
        }
        Err(e) => panic!("symlink {}: {e}", link.display()),
    }
}

#[test]
fn m4a1_edit_refuses_links_below_a_root() {
    let w = wc_world();
    let outside = tempfile::tempdir().unwrap();
    write_file(outside.path(), "doc.yaml", NEW_DOC.as_bytes());
    // a linked directory inside the working copy, and a link from outside into it
    if !link(outside.path(), &w.root.join("guides/linked")) {
        return;
    }
    let got = w.check(&[w.root.join("guides/linked/doc.yaml")]);
    assert_eq!(reason_of(&got), Some("unsafe path"));
    let alias = tempfile::tempdir().unwrap();
    assert!(link(&w.root.join("guides"), &alias.path().join("g")));
    let got = w.check(&[alias.path().join("g/plan.yaml")]);
    assert_eq!(reason_of(&got), Some("unsafe path"));
    // a member next to an unsafe path still warns, and the result is incomplete
    let got = w.check(&[w.root.join("guides/linked/doc.yaml"), w.root.join(&w.doc)]);
    assert!(!got.checked);
    assert_eq!(got.warnings[0].code, W_HOOK_DIRECT_EDIT);
    assert_eq!(got.warnings[1].code, W_HOOK_UNAVAILABLE);
}

/// Parses `v` and returns its refusal.
fn refused(v: Value) -> RdyError {
    HookRequest::from_value(&v).expect_err(&v.to_string())
}

fn form(v: Value, reason: &str) {
    let e = refused(v);
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert_eq!(e.message, format!("Invalid rdy_hook form: {reason}."));
    assert_eq!(e.fix, "Use one documented check, install or remove form.");
}

fn over(v: Value, limit: usize, unit: &str) {
    let e = refused(v);
    assert_eq!(e.code, E_LIMIT, "{e}");
    assert_eq!(e.message, format!("The hook input exceeds {limit} {unit}."));
    assert_eq!(e.fix, "Pass a smaller hook input.");
}

fn abs(rest: &str) -> String {
    std::env::temp_dir().join(rest).display().to_string()
}

#[test]
fn m4a1_inputs_are_closed_and_bounded() {
    let h = "a".repeat(40);
    let code = |extra: Value| {
        let mut v = json!({"op": "check", "kind": "code", "repo": "app", "checkout": abs("co"),
            "head": h, "paths": ["src/a.rs"], "message": ""});
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    };
    // the four documented forms parse
    assert_eq!(
        HookRequest::from_value(&code(json!({}))).unwrap(),
        HookRequest::CheckCode(CodeCheck {
            repo: "app".into(),
            checkout: abs("co"),
            head: Some(h.clone()),
            paths: vec!["src/a.rs".into()],
            message: String::new(),
        })
    );
    assert!(matches!(
        HookRequest::from_value(&code(json!({"head": null}))).unwrap(),
        HookRequest::CheckCode(CodeCheck { head: None, .. })
    ));
    assert_eq!(
        HookRequest::from_value(&json!({"op": "check", "kind": "edit", "paths": [abs("a.yaml")]}))
            .unwrap(),
        HookRequest::CheckEdit(EditCheck {
            paths: vec![abs("a.yaml")]
        })
    );
    assert_eq!(
        HookRequest::from_value(&json!({"op": "install", "kind": "code", "repo": "app"})).unwrap(),
        HookRequest::Install(HookTarget::Code { repo: "app".into() })
    );
    assert_eq!(
        HookRequest::from_value(
            &json!({"op": "remove", "kind": "edit", "harness": "claude-code", "worktree": "wt"})
        )
        .unwrap(),
        HookRequest::Remove(HookTarget::Edit {
            harness: Harness::ClaudeCode,
            worktree: Some("wt".into())
        })
    );
    assert_eq!(
        HookRequest::from_value(
            &json!({"op": "install", "kind": "edit", "harness": "claude-code", "worktree": null})
        )
        .unwrap(),
        HookRequest::Install(HookTarget::Edit {
            harness: Harness::ClaudeCode,
            worktree: None
        })
    );
    // unknown fields name the nearest one
    form(
        code(json!({"pahts": []})),
        "unknown field \"pahts\" (did you mean \"paths\"?)",
    );
    form(
        json!({"op": "check", "kind": "edit", "paths": [], "zzzzzzzz": 1}),
        "unknown field \"zzzzzzzz\" (the fields are op, kind, repo, checkout, head, paths, message, harness, worktree)",
    );
    // the form's keys: op and kind first, then no field of another form, then every required one
    form(json!({"kind": "edit", "paths": []}), "pass op");
    form(json!({"op": "check", "paths": []}), "pass kind");
    form(
        json!({"op": "lint", "kind": "edit"}),
        "op is check, install or remove",
    );
    form(
        json!({"op": "check", "kind": "docs"}),
        "kind is code or edit",
    );
    form(
        json!({"op": "check", "kind": "edit", "paths": [], "repo": "app"}),
        "remove repo: the check edit form does not take it",
    );
    form(
        code(json!({"harness": "claude-code"})),
        "remove harness: the check code form does not take it",
    );
    form(
        json!({"op": "install", "kind": "code", "repo": "app", "paths": []}),
        "remove paths: the install code form does not take it",
    );
    form(
        json!({"op": "remove", "kind": "edit", "harness": "claude-code", "repo": "app"}),
        "remove repo: the remove edit form does not take it",
    );
    let mut no_message = code(json!({}));
    no_message.as_object_mut().unwrap().remove("message");
    form(no_message, "pass message");
    let mut no_head = code(json!({}));
    no_head.as_object_mut().unwrap().remove("head");
    form(no_head, "pass head");
    form(json!({"op": "install", "kind": "edit"}), "pass harness");
    form(
        json!({"op": "install", "kind": "edit", "harness": "cursor"}),
        "harness is claude-code",
    );
    form(
        json!({"op": "install", "kind": "edit", "harness": "claude-code", "worktree": "../x"}),
        "worktree is the name of a registered worktree",
    );
    form(json!([1]), "the input is not a JSON object");
    form(
        code(json!({"paths": "src/a.rs"})),
        "paths is a list of strings",
    );
    form(code(json!({"paths": [1]})), "paths is a list of strings");
    form(code(json!({"message": 3})), "message is a string");
    form(code(json!({"repo": ""})), "repo names a code_repos entry");
    // head: a full lowercase commit hash or null
    for bad in [
        json!("HEAD"),
        json!("A".repeat(40)),
        json!("a".repeat(39)),
        json!(7),
    ] {
        form(
            code(json!({"head": bad})),
            "head is a full lowercase commit hash, or null for an unborn HEAD",
        );
    }
    assert!(HookRequest::from_value(&code(json!({"head": "b".repeat(64)}))).is_ok());
    // path escapes and invalid native paths
    for bad in [
        "../a.rs",
        "src/../a.rs",
        "/src/a.rs",
        "src//a.rs",
        "./a.rs",
        "a\0b",
    ] {
        form(
            code(json!({"paths": [bad]})),
            &format!("{bad:?} is not a repo-relative code path"),
        );
    }
    let dotted = abs("x/../y.yaml");
    for bad in ["relative/a.yaml", dotted.as_str(), "a\0b"] {
        form(
            json!({"op": "check", "kind": "edit", "paths": [bad]}),
            &format!("{bad:?} is not an absolute native path"),
        );
    }
    for bad in ["relative", "", "a\0b"] {
        form(
            code(json!({"checkout": bad})),
            "checkout is the absolute native root of a Git worktree",
        );
    }
    // every bound at its exact edge
    let n_paths = |n: usize| json!(vec!["src/a.rs"; n]);
    assert!(HookRequest::from_value(&code(json!({"paths": n_paths(MAX_HOOK_PATHS)}))).is_ok());
    over(
        code(json!({"paths": n_paths(MAX_HOOK_PATHS + 1)})),
        256,
        "paths",
    );
    let edit_paths = |n: usize| json!(vec![abs("a.yaml"); n]);
    assert!(
        HookRequest::from_value(
            &json!({"op": "check", "kind": "edit", "paths": edit_paths(MAX_HOOK_PATHS)})
        )
        .is_ok()
    );
    over(
        json!({"op": "check", "kind": "edit", "paths": edit_paths(MAX_HOOK_PATHS + 1)}),
        256,
        "paths",
    );
    assert!(HookRequest::from_value(&code(json!({"paths": ["a".repeat(MAX_HOOK_PATH)]}))).is_ok());
    over(
        code(json!({"paths": ["a".repeat(MAX_HOOK_PATH + 1)]})),
        4096,
        "path bytes",
    );
    assert!(
        HookRequest::from_value(&code(json!({"message": "m".repeat(MAX_HOOK_MESSAGE)}))).is_ok()
    );
    over(
        code(json!({"message": "m".repeat(MAX_HOOK_MESSAGE + 1)})),
        65536,
        "message bytes",
    );
    let long_checkout = |n: usize| {
        let base = abs("");
        format!("{base}{}", "c".repeat(n - base.len()))
    };
    assert!(
        HookRequest::from_value(&code(json!({"checkout": long_checkout(MAX_HOOK_CHECKOUT)})))
            .is_ok()
    );
    over(
        code(json!({"checkout": long_checkout(MAX_HOOK_CHECKOUT + 1)})),
        4096,
        "checkout bytes",
    );
    over(
        code(json!({"head": "a".repeat(MAX_HOOK_HEAD + 1)})),
        64,
        "head bytes",
    );
    // the whole input: 1 MiB of JSON at most, refused before parsing
    let body = |n: usize| {
        let mut b = br#"{"op":"check","kind":"edit","paths":[]}"#.to_vec();
        b.resize(n, b' ');
        b
    };
    assert!(HookRequest::from_slice(&body(MAX_HOOK_INPUT)).is_ok());
    let e = HookRequest::from_slice(&body(MAX_HOOK_INPUT + 1)).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_LIMIT, "The hook input exceeds 1048576 bytes.")
    );
    let e = HookRequest::from_slice(b"{\"op\": ").unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert!(
        e.message
            .starts_with("Invalid rdy_hook form: the input is not JSON")
    );
    let big =
        json!({"op": "check", "kind": "edit", "paths": [], "message": "m".repeat(MAX_HOOK_INPUT)});
    over(big, 1048576, "bytes");
}

/// Every file under `dir` with its bytes, by relative path.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let m = std::fs::symlink_metadata(&p).unwrap();
            if m.is_dir() {
                walk(&p, base, out);
            } else if m.is_file() {
                out.insert(
                    p.strip_prefix(base).unwrap().to_path_buf(),
                    std::fs::read(&p).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

#[test]
fn m4a1_checks_preserve_log_and_working_copy() {
    let w = World::new();
    let wc = wc_world();
    let pending = |r: &Rdy| {
        let q = r
            .read()
            .pending(&rdy_engine::pending::PendingQuery {
                person: "human:qa".into(),
                session_ref: None,
                max_tokens: rdy_engine::pending::CARD_MAX_TOKENS,
            })
            .unwrap();
        serde_json::to_string(&q).unwrap()
    };
    // stage something in the checkout, so the index has bytes to keep
    write_file(&w.repo, "src/direct.rs", b"changed\n");
    git(&w.repo, &["add", "src/direct.rs"]);
    let before = (
        snapshot(w.r.root()),
        snapshot(&w.repo),
        snapshot(&wc.root),
        snapshot(&wc.wt),
        pending(&w.r),
        pending(&wc.r),
        w.r.read().canon_head(),
    );
    // a warning, a clean result and unavailable results, through both checks
    assert_eq!(w.warned(&["src/direct.rs"], "").len(), 1);
    assert!(w.warned(&["src/none.rs"], "").is_empty());
    let mut moved = w.req(&["src/direct.rs"], "");
    moved.head = Some("f".repeat(40));
    assert_eq!(reason_of(&w.check(&moved)), Some("history unavailable"));
    let doc = wc.root.join(&wc.doc);
    assert_eq!(wc.warned(std::slice::from_ref(&doc)).len(), 1);
    let after = (
        snapshot(w.r.root()),
        snapshot(&w.repo),
        snapshot(&wc.root),
        snapshot(&wc.wt),
        pending(&w.r),
        pending(&wc.r),
        w.r.read().canon_head(),
    );
    assert!(
        before == after,
        "a check changed the store, a checkout or a card"
    );

    // a passed deadline or a fired cancellation is unavailable, never clean
    let late = RunContext::new().with_deadline(Instant::now());
    let got = check_code(&w.r.read(), &w.req(&["src/direct.rs"], ""), &late).unwrap();
    assert_eq!(reason_of(&got), Some("deadline"));
    let got = check_edit(
        &wc.r.read(),
        &EditCheck {
            paths: vec![doc.display().to_string()],
        },
        &late,
    );
    assert_eq!(reason_of(&got), Some("deadline"));
    assert!(got.warnings.iter().all(|w| w.code != W_HOOK_DIRECT_EDIT));
    // a cancellation once the first git child runs stops the check and starts no other
    let ctx = RunContext::new();
    let spawns = Arc::new(Mutex::new(0usize));
    let (count, canceller) = (Arc::clone(&spawns), ctx.clone());
    let ctx = ctx.on_spawn(move || {
        *count.lock().unwrap() += 1;
        canceller.cancel();
    });
    let got = check_code(&w.r.read(), &w.req(&["src/direct.rs"], ""), &ctx).unwrap();
    assert_eq!(reason_of(&got), Some("deadline"));
    assert_eq!(
        *spawns.lock().unwrap(),
        1,
        "no git command after the cancellation"
    );
    let diag = got.warnings[0]
        .diagnostic
        .as_ref()
        .expect("the git refusal");
    assert_eq!(diag.code, E_IO);
    // the same check without the cancellation still warns: nothing half-built was kept
    assert_eq!(w.warned(&["src/direct.rs"], "").len(), 1);
    // a project folder that is gone: unavailable, not clean
    let gone = World::new();
    let st = gone.r.read();
    let root = st.root().to_path_buf();
    std::fs::rename(&root, root.with_extension("moved")).unwrap();
    let got = check_edit(
        &st,
        &EditCheck {
            paths: vec![root.join("x.yaml").display().to_string()],
        },
        &RunContext::new(),
    );
    assert_eq!(reason_of(&got), Some("project unavailable"));
    let got = check_code(&st, &gone.req(&["src/direct.rs"], ""), &RunContext::new()).unwrap();
    assert_eq!(reason_of(&got), Some("project unavailable"));
    std::fs::rename(root.with_extension("moved"), &root).unwrap();
}

/// A host that records what it was asked and writes nothing.
#[derive(Default)]
struct Recorder {
    asked: Mutex<Vec<String>>,
}

impl HookHost for Recorder {
    fn install(&self, target: &HookTarget, _ctx: &RunContext) -> Result<HookInstall, RdyError> {
        self.asked
            .lock()
            .unwrap()
            .push(format!("install {target:?}"));
        Ok(HookInstall {
            kind: target.kind(),
            files: vec![HookFile {
                path: "commit-msg".into(),
                result: FileResult::Written,
            }],
            warnings: vec![HookWarning::conflict(
                ".claude/settings.local.json",
                rdy_engine::hooks::Conflict::InvalidSettings,
            )],
        })
    }
    fn remove(&self, target: &HookTarget, _ctx: &RunContext) -> Result<HookInstall, RdyError> {
        self.asked
            .lock()
            .unwrap()
            .push(format!("remove {target:?}"));
        Ok(HookInstall {
            kind: target.kind(),
            files: vec![],
            warnings: vec![],
        })
    }
}

#[test]
fn m4a1_a_host_without_local_hooks_still_compiles() {
    let w = wc_world();
    let root = w.r.root().to_path_buf();
    drop(w.r);
    let engine = engine::Local::open(&root).unwrap();
    assert!(engine.person_host().is_none() && engine.hook_host().is_none());
    let ctx = RunContext::new();
    for req in [
        HookRequest::Install(HookTarget::Code { repo: "app".into() }),
        HookRequest::Remove(HookTarget::Edit {
            harness: Harness::ClaudeCode,
            worktree: None,
        }),
    ] {
        let e = rdy_engine::hooks::run(&engine, &req, &ctx).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_BAD_INPUT,
                "This host cannot install local hooks.",
                "Use rdy setup on the machine that holds the checkout."
            )
        );
    }
    // a check needs no host, and its reply is the documented shape
    let doc = std::fs::canonicalize(&root).unwrap().join(&w.doc);
    let reply = rdy_engine::hooks::run(
        &engine,
        &HookRequest::CheckEdit(EditCheck {
            paths: vec![doc.display().to_string()],
        }),
        &ctx,
    )
    .unwrap();
    assert_eq!(reply["kind"], "edit");
    assert_eq!(reply["checked"], true);
    assert_eq!(reply["warnings"][0]["code"], "W_HOOK_DIRECT_EDIT");
    assert_eq!(
        reply["warnings"][0]["paths"],
        json!([doc.display().to_string()])
    );
    // an injected host receives install and remove; the person layer stays as it was
    let host = Arc::new(Recorder::default());
    let engine = engine.with_hook_host(host.clone());
    assert!(engine.person_host().is_none());
    let reply = rdy_engine::hooks::run(
        &engine,
        &HookRequest::Install(HookTarget::Code { repo: "app".into() }),
        &ctx,
    )
    .unwrap();
    assert_eq!(
        reply,
        json!({"kind": "code", "files": [{"path": "commit-msg", "result": "written"}],
            "warnings": [{"code": "W_HOOK_CONFLICT",
                "message": "The rdy hook left .claude/settings.local.json unchanged: invalid settings.",
                "fix": "Review the existing file, then install or remove the rdy hook explicitly.",
                "reason": "invalid settings", "paths": [".claude/settings.local.json"]}]})
    );
    rdy_engine::hooks::run(
        &engine,
        &HookRequest::Remove(HookTarget::Edit {
            harness: Harness::ClaudeCode,
            worktree: Some("wt".into()),
        }),
        &ctx,
    )
    .unwrap();
    assert_eq!(
        *host.asked.lock().unwrap(),
        vec![
            "install Code { repo: \"app\" }".to_string(),
            "remove Edit { harness: ClaudeCode, worktree: Some(\"wt\") }".to_string()
        ]
    );
    engine.close().unwrap();
}
