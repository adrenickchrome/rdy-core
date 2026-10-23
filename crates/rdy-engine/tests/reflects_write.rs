//! rf8 (D-4WC6R, D-XC9B9, D-W7KAK): a write justified by `reflects` names sources (S-…) and, with
//! `code: true`, records the code state of the blocks it writes that declare reflects globs. The
//! raw selectors resolve in the write's own overlay, after every op: a source created in the same
//! write counts, one deleted later in it does not. Content blocks the write creates or rewrites
//! get each source as a `reflects` link; decisions, supporting and structural blocks do not. Every
//! refusal leaves the log and the view head as they were.
use rdy_engine::reflects::{W_REFLECTS_NO_CODE, code_suspects, snapshot};
use rdy_engine::*;
use rdy_model::engine_fields::{REFLECTS_AT, ReflectsAt, decode_reflects_at};
use rdy_model::*;
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;

const VIEW: &str = "work";

fn writer() -> Session {
    Session {
        actor: "agent:designer".into(),
        run: Some("run-1".into()),
        role: Role::Writer,
        kind: SessionKind::Human,
    }
}

fn work() -> Scope {
    Scope::View(VIEW.into())
}

fn req(ops: Value) -> WriteRequest {
    WriteRequest {
        view: VIEW.into(),
        ops: parse_ops(serde_json::from_value(ops).unwrap()).unwrap(),
        justification: None,
        message: "rf8".into(),
    }
}

fn write(r: &mut Rdy, ops: Value) -> Result<WriteResult, RdyError> {
    r.write(&writer(), req(ops))
}

fn reflecting(
    r: &mut Rdy,
    ops: Value,
    sources: &[&str],
    code: bool,
) -> Result<WriteResult, RdyError> {
    r.write_reflecting(
        &writer(),
        req(ops),
        ReflectsInput {
            sources: sources.iter().map(|s| s.to_string()).collect(),
            code,
        },
        ChangeLinks::default(),
        None,
    )
}

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn block(r: &Rdy, id: &str) -> Block {
    r.read().block(&work(), &bid(id)).unwrap()
}

fn reflects_links(r: &Rdy, id: &str) -> Vec<String> {
    block(r, id)
        .links
        .get("reflects")
        .map(|l| l.iter().map(|l| l.target.to_string()).collect())
        .unwrap_or_default()
}

fn stored_change(r: &Rdy, w: &WriteResult) -> Change {
    r.read()
        .reader()
        .change(&w.change.parse::<Hash>().unwrap())
        .unwrap()
}

/// The log position and the view's head: a refusal leaves both as they were.
fn mark(r: &Rdy) -> (String, Option<Hash>) {
    let st = r.read();
    (
        format!("{:?}", st.seq()),
        st.view_head(VIEW).map(|h| h.head),
    )
}

fn refused(e: &RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e}"
    );
}

/// A draft doc `notes` with section `s` holding claims `c` and `c2`, and a source `src` (key
/// SRC-ONE) beside them. Returns the created ids by placeholder.
fn setup(r: &mut Rdy) -> WriteResult {
    write(
        r,
        json!([
            {"op": "create", "type": "doc", "id": "$notes", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$notes", "fields": {"title": "Retention"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Retention is thirty days."},
            {"op": "create", "type": "claim", "id": "$c2", "parent": "$s", "body": "Backups follow retention."},
            {"op": "create", "type": "source", "id": "$src", "parent": "$s",
                "fields": {"title": "Release 2", "kind": "release", "locator": "v2", "key": "SRC-ONE"}},
            {"op": "create", "type": "source", "id": "$src2", "parent": "$s",
                "fields": {"title": "Release 3", "kind": "release", "locator": "v3"}}
        ]),
    )
    .unwrap()
}

fn project() -> (tempfile::TempDir, Rdy, WriteResult) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = setup(&mut r);
    (dir, r, w)
}

#[test]
fn rf8_raw_sources_resolve_in_the_write_overlay() {
    let (_d, mut r, w) = project();
    let (c, c2, src, notes) = (
        w.created["$c"].clone(),
        w.created["$c2"].clone(),
        w.created["$src"].clone(),
        w.created["$notes"].clone(),
    );
    let s = w.created["$s"].clone();
    // an id, its key in another case and a source created after the content: the first two
    // name one source, kept once at its first place
    let got = reflecting(
        &mut r,
        json!([
            {"op": "update", "id": c, "body": "Retention is seven days."},
            {"op": "create", "type": "claim", "id": "$new", "parent": s, "body": "Seven days hold."},
            {"op": "create", "type": "source", "id": "$late", "parent": s,
                "fields": {"title": "Release 4", "kind": "release", "locator": "v4"}}
        ]),
        &[&src, "src-one", "$late", &src],
        false,
    )
    .unwrap();
    let late = got.created["$late"].clone();
    let new = got.created["$new"].clone();
    let want = Justification::Reflects {
        sources: vec![bid(&src), bid(&late)],
        code: false,
    };
    assert_eq!(stored_change(&r, &got).justification, Some(want.clone()));
    assert_eq!(got.justification, Some(JustificationInfo::from(&want)));
    assert_eq!(got.code_recorded, None);
    let v = serde_json::to_value(&got).unwrap();
    assert_eq!(
        v["justification"],
        json!({"reflects": {"sources": [src, late], "code": false}})
    );
    assert!(v.get("code_recorded").is_none(), "{v}");
    for b in [&c, &new] {
        assert_eq!(
            reflects_links(&r, b),
            vec![src.clone(), late.clone()],
            "{b}"
        );
        let pins = block(&r, b).pins;
        assert!(
            pins.contains_key(&bid(&src)) && pins.contains_key(&bid(&late)),
            "{pins:?}"
        );
    }
    // a source created before the content counts too
    let got = reflecting(
        &mut r,
        json!([
            {"op": "create", "type": "source", "id": "$early", "parent": s,
                "fields": {"title": "Release 5", "kind": "release", "locator": "v5"}},
            {"op": "update", "id": c2, "body": "Backups follow retention, now seven days."}
        ]),
        &["$early"],
        false,
    )
    .unwrap();
    assert_eq!(reflects_links(&r, &c2), vec![got.created["$early"].clone()]);
    // a path resolves, to a doc: refused as a wrong type
    let before = mark(&r);
    let e = reflecting(
        &mut r,
        json!([{"op": "update", "id": c2, "body": "Again."}]),
        &["notes"],
        false,
    )
    .unwrap_err();
    refused(
        &e,
        E_BAD_INPUT,
        &format!("reflects names {notes}, a doc; reflects names a source"),
        "name a source (S-…), or create one with a source op",
    );
    assert_eq!(mark(&r), before);
    // a source the same write deletes later is not held: nothing is appended
    let e = reflecting(
        &mut r,
        json!([
            {"op": "update", "id": c2, "body": "Backups follow release 3."},
            {"op": "delete", "id": w.created["$src2"]}
        ]),
        &[&w.created["$src2"]],
        false,
    )
    .unwrap_err();
    let gone = &w.created["$src2"];
    refused(
        &e,
        E_BAD_INPUT,
        &format!("reflects names {gone}, which this view does not hold"),
        &format!("include the change that records {gone}, or create the source in this write"),
    );
    assert_eq!(mark(&r), before);
    // a direct engine request carrying Reflects passes the same checks
    let mut direct = req(json!([{"op": "update", "id": c2, "body": "Direct."}]));
    direct.justification = Some(Justification::Reflects {
        sources: vec![bid(&notes)],
        code: false,
    });
    let e = r.write(&writer(), direct).unwrap_err();
    assert_eq!(
        e.message,
        format!("reflects names {notes}, a doc; reflects names a source"),
        "{e}"
    );
    assert_eq!(mark(&r), before);
    let mut direct = req(json!([{"op": "update", "id": c2, "body": "Direct."}]));
    direct.justification = Some(Justification::Reflects {
        sources: vec![bid(&src), bid(&src)],
        code: false,
    });
    let got = r.write(&writer(), direct).unwrap();
    assert_eq!(
        stored_change(&r, &got).justification,
        Some(Justification::Reflects {
            sources: vec![bid(&src)],
            code: false
        })
    );
}

#[test]
fn rf8_reflects_links_follow_the_implements_recipient_rule() {
    let (_d, mut r, w) = project();
    let (c, c2, s, src, src2) = (
        w.created["$c"].clone(),
        w.created["$c2"].clone(),
        w.created["$s"].clone(),
        w.created["$src"].clone(),
        w.created["$src2"].clone(),
    );
    let d = r
        .decide(
            &writer(),
            VIEW,
            DecisionInput {
                parent: s.clone(),
                statement: "Keep thirty days".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let got = reflecting(
        &mut r,
        json!([
            {"op": "update", "id": c, "body": "Retention is seven days."},
            {"op": "update", "id": d, "fields": {"statement": "Keep seven days"}},
            {"op": "update", "id": s, "fields": {"title": "Retention rules"}},
            {"op": "update", "id": src2, "fields": {"locator": "v3.1"}},
            {"op": "move", "id": c2, "parent": s, "before": c}
        ]),
        &[&src],
        false,
    )
    .unwrap();
    assert_eq!(reflects_links(&r, &c), vec![src.clone()]);
    assert!(block(&r, &c).pins.contains_key(&bid(&src)));
    for (b, what) in [
        (&d, "a decision"),
        (&s, "a structural section"),
        (&src2, "a supporting source"),
        (&c2, "a move alone"),
    ] {
        assert!(reflects_links(&r, b).is_empty(), "{what} gets no link");
    }
    assert!(got.warnings.iter().all(|w| w.code != W_REFLECTS_NO_CODE));
    // an explicit list on an update replaces the kind exactly: no source is appended
    reflecting(
        &mut r,
        json!([{"op": "update", "id": c, "body": "Seven days.", "links": {"reflects": [src2]}}]),
        &[&src],
        false,
    )
    .unwrap();
    assert_eq!(reflects_links(&r, &c), vec![src2.clone()]);
    // and an explicit empty list removes it
    reflecting(
        &mut r,
        json!([{"op": "update", "id": c, "body": "Seven days, held.", "links": {"reflects": []}}]),
        &[&src],
        false,
    )
    .unwrap();
    assert!(reflects_links(&r, &c).is_empty());
    // an explicit target is checked as a held source
    let before = mark(&r);
    let e = reflecting(
        &mut r,
        json!([{"op": "update", "id": c, "body": "x", "links": {"reflects": [c2]}}]),
        &[&src],
        false,
    )
    .unwrap_err();
    refused(
        &e,
        E_BAD_INPUT,
        &format!("reflects names {c2}, a claim; reflects names a source"),
        "name a source (S-…), or create one with a source op",
    );
    assert_eq!(mark(&r), before);
    // a create already linking the source keeps one link
    let got = reflecting(
        &mut r,
        json!([{"op": "create", "type": "claim", "id": "$k", "parent": s, "body": "K.",
            "links": {"reflects": [src]}}]),
        &[&src, &src2],
        false,
    )
    .unwrap();
    assert_eq!(
        reflects_links(&r, &got.created["$k"]),
        vec![src.clone(), src2.clone()]
    );
}

#[test]
fn rf8_all_reflects_refusals_are_atomic() {
    let (_d, mut r, w) = project();
    let (c, c2, src) = (
        w.created["$c"].clone(),
        w.created["$c2"].clone(),
        w.created["$src"].clone(),
    );
    let before = mark(&r);
    let body = json!([{"op": "update", "id": c, "body": "Changed."}]);
    // one justification only: at the boundary's parser and on the engine entry point
    let st = r.read();
    let two = |e: RdyError| {
        refused(
            &e,
            E_BAD_INPUT,
            "give one of implements, editorial or reflects",
            "implements for decision-backed changes, editorial for typo, restructure or clarify, reflects for changes that follow a source",
        )
    };
    let raw = || {
        Some(ReflectsArg {
            sources: Some(vec![src.clone()]),
            code: None,
        })
    };
    two(parse_justification(&st, &work(), Some(vec![src.clone()]), None, raw()).unwrap_err());
    two(
        parse_justification(&st, &work(), None, Some(("typo".into(), "x".into())), raw())
            .unwrap_err(),
    );
    two(parse_justification(
        &st,
        &work(),
        Some(vec![src.clone()]),
        Some(("typo".into(), "x".into())),
        None,
    )
    .unwrap_err());
    drop(st);
    let mut both = req(body.clone());
    both.justification = Some(Justification::Editorial {
        class: EditorialClass::Typo,
        reason: "x".into(),
    });
    two(r
        .write_reflecting(
            &writer(),
            both,
            ReflectsInput {
                sources: vec![src.clone()],
                code: false,
            },
            ChangeLinks::default(),
            None,
        )
        .unwrap_err());
    // a false code, and an unknown key in the raw object
    let e = ReflectsArg {
        sources: None,
        code: Some(false),
    }
    .input()
    .unwrap_err();
    refused(
        &e,
        E_BAD_INPUT,
        "reflects code must be true when given",
        "pass code: true, or omit code",
    );
    let unknown = serde_json::from_value::<ReflectsArg>(json!({"sorces": [src]})).unwrap_err();
    assert!(unknown.to_string().contains("sources"), "{unknown}");
    // the parsed form keeps the raw input for the writer
    let st = r.read();
    let p = parse_justification(
        &st,
        &work(),
        None,
        None,
        Some(ReflectsArg {
            sources: None,
            code: Some(true),
        }),
    )
    .unwrap();
    assert_eq!(p.stored, None);
    assert_eq!(
        p.reflects,
        Some(ReflectsInput {
            sources: vec![],
            code: true
        })
    );
    drop(st);
    // empty, wrong type, missing source, no glob
    let cases: Vec<(Vec<String>, bool, &str, String, String)> = vec![
        (
            vec![],
            false,
            E_BAD_INPUT,
            "reflects names no source and no code".into(),
            "name a source (S-…), or pass code: true for a block with reflects globs".into(),
        ),
        (
            vec![c2.clone()],
            false,
            E_BAD_INPUT,
            format!("reflects names {c2}, a claim; reflects names a source"),
            "name a source (S-…), or create one with a source op".into(),
        ),
        (
            vec!["S-ZZZZZ".into()],
            false,
            E_BAD_INPUT,
            "reflects names S-ZZZZZ, which this view does not hold".into(),
            "include the change that records S-ZZZZZ, or create the source in this write".into(),
        ),
        (
            vec!["NO-SUCH-KEY".into()],
            false,
            E_BAD_INPUT,
            "reflects names NO-SUCH-KEY, which this view does not hold".into(),
            "include the change that records NO-SUCH-KEY, or create the source in this write"
                .into(),
        ),
        (
            vec![src.clone()],
            true,
            E_BAD_INPUT,
            "reflects code: no block this write changes declares reflects globs".into(),
            "set reflects on the block, or name a source".into(),
        ),
        (
            vec![],
            true,
            E_BAD_INPUT,
            "reflects code: no block this write changes declares reflects globs".into(),
            "set reflects on the block, or name a source".into(),
        ),
        (
            vec![src.clone(); 5001],
            false,
            E_LIMIT,
            "reflects takes at most 5000 sources".into(),
            "split the write into smaller changes".into(),
        ),
        (
            vec!["k".repeat(2001)],
            false,
            E_LIMIT,
            "a reflects source selector takes at most 2000 characters".into(),
            "use the source's id".into(),
        ),
    ];
    for (sources, code, want_code, message, fix) in cases {
        let e = r
            .write_reflecting(
                &writer(),
                req(body.clone()),
                ReflectsInput { sources, code },
                ChangeLinks::default(),
                None,
            )
            .unwrap_err();
        refused(&e, want_code, &message, &fix);
        assert_eq!(mark(&r), before, "{message}");
    }
    // the bounds hold at their edges: 5000 sources and a 2000-character selector are read
    let e = r
        .write_reflecting(
            &writer(),
            req(body.clone()),
            ReflectsInput {
                sources: vec!["k".repeat(2000)],
                code: false,
            },
            ChangeLinks::default(),
            None,
        )
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    let ok = r
        .write_reflecting(
            &writer(),
            req(body),
            ReflectsInput {
                sources: vec![src.clone(); 5000],
                code: false,
            },
            ChangeLinks::default(),
            None,
        )
        .unwrap();
    assert_eq!(
        ok.justification,
        Some(JustificationInfo::Reflects {
            sources: vec![src],
            code: false
        })
    );
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
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
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

fn commit(dir: &Path, files: &[(&str, &str)]) -> String {
    for (path, text) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--no-verify", "-m", "c"]);
    git(dir, &["rev-parse", "HEAD"])
}

fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q"]);
    commit(d.path(), files);
    d
}

/// Links checkout `app` (or none) as repo `app` of the project at `dir`.
fn link(dir: &Path, app: Option<&Path>) {
    let yaml = dir.join(".rdy.yaml");
    let text = std::fs::read_to_string(&yaml).unwrap();
    if !text.contains("code_repos:") {
        std::fs::write(&yaml, format!("{text}\ncode_repos: [app]\n")).unwrap();
    }
    std::fs::create_dir_all(dir.join(".rdy")).unwrap();
    let local = match app {
        Some(p) => format!("code_paths:\n  app: {}\n", p.display()),
        None => "code_paths: {}\n".into(),
    };
    std::fs::write(dir.join(".rdy/local.yaml"), local).unwrap();
}

/// A project linked to a checkout holding src/a.rs, with the setup tree plus claims `r` and `q`
/// that reflect src/*.rs and claim `t` with no globs.
fn code_project() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    Rdy,
    WriteResult,
    WriteResult,
) {
    let app = repo(&[("src/a.rs", "a\n")]);
    let (dir, mut r, w) = project();
    link(dir.path(), Some(app.path()));
    let s = w.created["$s"].clone();
    let g = write(
        &mut r,
        json!([
            {"op": "create", "type": "claim", "id": "$r", "parent": s, "body": "R.", "fields": {"reflects": ["src/*.rs"]}},
            {"op": "create", "type": "claim", "id": "$q", "parent": s, "body": "Q.", "fields": {"reflects": ["src/*.rs"]}},
            {"op": "create", "type": "claim", "id": "$t", "parent": s, "body": "T."}
        ]),
    )
    .unwrap();
    (dir, app, r, w, g)
}

fn at(r: &Rdy, id: &str) -> Option<ReflectsAt> {
    block(r, id)
        .fields
        .get(REFLECTS_AT)
        .map(|v| decode_reflects_at(v).unwrap())
}

fn suspect_ids(r: &Rdy) -> Vec<String> {
    code_suspects(&r.read(), &work(), None)
        .unwrap()
        .into_iter()
        .map(|s| s.block.to_string())
        .collect()
}

#[test]
fn rf8_code_true_stamps_written_globs_and_reports_ids() {
    let (dir, app, mut r, _w, g) = code_project();
    let (rb, qb, tb) = (
        g.created["$r"].clone(),
        g.created["$q"].clone(),
        g.created["$t"].clone(),
    );
    let q_before = at(&r, &qb).unwrap();
    commit(app.path(), &[("src/a.rs", "a2\n")]);
    let mut both = vec![rb.clone(), qb.clone()];
    both.sort();
    assert_eq!(suspect_ids(&r), both);
    let got = reflecting(
        &mut r,
        json!([
            {"op": "update", "id": rb, "body": "R follows a2."},
            {"op": "update", "id": tb, "body": "T changed."}
        ]),
        &[],
        true,
    )
    .unwrap();
    assert_eq!(got.code_recorded, Some(vec![rb.clone()]));
    let v = serde_json::to_value(&got).unwrap();
    assert_eq!(v["code_recorded"], json!([rb]));
    assert_eq!(
        v["justification"],
        json!({"reflects": {"sources": [], "code": true}})
    );
    assert_eq!(suspect_ids(&r), vec![qb.clone()], "r's code suspect clears");
    // the stamp is the code index's own snapshot of the block's globs
    let now = snapshot(dir.path(), &block(&r, &rb)).unwrap().unwrap();
    let stamped = at(&r, &rb).unwrap();
    assert_eq!(stamped.files, now.files);
    assert_eq!(
        stamped.commits,
        now.commits
            .iter()
            .map(|c| (c.repo.clone(), c.head.clone()))
            .collect()
    );
    assert_eq!(
        at(&r, &qb).unwrap(),
        q_before,
        "an untouched block keeps its own"
    );
    assert_eq!(at(&r, &tb), None, "a block with no globs gets none");
    assert_eq!(
        stored_change(&r, &got).justification,
        Some(Justification::Reflects {
            sources: vec![],
            code: true
        })
    );
}

#[test]
fn rf8_sources_only_and_ordinary_rewrite_keep_code_suspects() {
    let (_dir, app, mut r, w, g) = code_project();
    let (rb, qb) = (g.created["$r"].clone(), g.created["$q"].clone());
    let src = w.created["$src"].clone();
    let (r_before, q_before) = (at(&r, &rb).unwrap(), at(&r, &qb).unwrap());
    commit(app.path(), &[("src/a.rs", "a2\n")]);
    let got = reflecting(
        &mut r,
        json!([{"op": "update", "id": rb, "body": "R follows the release."}]),
        &[&src],
        false,
    )
    .unwrap();
    assert_eq!(got.code_recorded, None);
    write(
        &mut r,
        json!([{"op": "update", "id": qb, "body": "Q again."}]),
    )
    .unwrap();
    assert_eq!(at(&r, &rb).unwrap(), r_before);
    assert_eq!(at(&r, &qb).unwrap(), q_before);
    let mut both = vec![rb.clone(), qb.clone()];
    both.sort();
    assert_eq!(suspect_ids(&r), both, "both keep their code suspect");
    // a new reflects block and a changed glob list still record the current state
    let s = w.created["$s"].clone();
    let n = reflecting(
        &mut r,
        json!([
            {"op": "create", "type": "claim", "id": "$n", "parent": s, "body": "N.", "fields": {"reflects": ["src/*.rs"]}},
            {"op": "update", "id": qb, "fields": {"reflects": ["src/a.rs"]}}
        ]),
        &[&src],
        false,
    )
    .unwrap();
    assert_ne!(at(&r, &qb).unwrap(), q_before, "a re-glob records anew");
    assert!(at(&r, &n.created["$n"]).is_some());
    assert_eq!(suspect_ids(&r), vec![rb]);
}

#[test]
fn rf8_no_checkout_warns_without_clearing_code() {
    let (dir, app, mut r, _w, g) = code_project();
    let rb = g.created["$r"].clone();
    let r_before = at(&r, &rb).unwrap();
    commit(app.path(), &[("src/a.rs", "a2\n")]);
    link(dir.path(), None);
    let got = reflecting(
        &mut r,
        json!([{"op": "update", "id": rb, "body": "R, unlinked."}]),
        &[],
        true,
    )
    .unwrap();
    assert_eq!(got.code_recorded, Some(vec![]));
    assert_eq!(
        serde_json::to_value(&got).unwrap()["code_recorded"],
        json!([])
    );
    let warn: Vec<_> = got
        .warnings
        .iter()
        .filter(|w| w.code == W_REFLECTS_NO_CODE)
        .collect();
    assert_eq!(warn.len(), 1, "{:?}", got.warnings);
    assert_eq!(
        warn[0].message,
        format!("no linked checkout; rdy records no code state for {rb}")
    );
    assert_eq!(warn[0].fix, "set code_paths, then repin it with code: true");
    assert_eq!(at(&r, &rb).unwrap(), r_before, "no snapshot is stored");
    // an explicit code repin keeps its own refusal
    let before = mark(&r);
    let e = write(&mut r, json!([{"op": "repin", "id": rb, "code": true}])).unwrap_err();
    refused(
        &e,
        E_CONFIG,
        "no linked checkout for a code repin",
        "set code_paths to a linked checkout, then repin with code: true",
    );
    assert_eq!(mark(&r), before);
    // linked again, the drift is still a code suspect
    link(dir.path(), Some(app.path()));
    assert_eq!(suspect_ids(&r), {
        let mut v = vec![rb, g.created["$q"].clone()];
        v.sort();
        v
    });
}
