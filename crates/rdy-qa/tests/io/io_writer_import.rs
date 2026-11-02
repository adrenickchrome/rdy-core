//! ui2 (D-Y9DNZ, D-KQ4CA, D-XR09R; TASK-982 O1): an import writes into a view, never into canon,
//! under the same role rule as an ordinary write. A writer, a reviewer and a person's approver
//! import; a reader is refused, as are canon, a reserved view and a view another session owns,
//! before any file of the tree is read. Each change keeps its author's provenance and records the
//! session that imported it. The import reply (`import_api::run_import`) keeps every key the
//! daemon's import reply has: the write report, the warning docs, the coverage baseline and the
//! onboarding path (or `coverage_error`), and the write receipt under `briefing`.
use rdy_engine::engine::Local;
use rdy_engine::import_api::run_import;
use rdy_engine::*;
use rdy_model::*;
use rdy_qa::daemon::Served;
use serde_json::Value;
use std::path::Path;

fn sess(actor: &str, run: Option<&str>, role: Role, kind: SessionKind) -> Session {
    Session {
        actor: actor.into(),
        run: run.map(str::to_string),
        role,
        kind,
    }
}

/// An agent session of role writer.
fn agent(actor: &str, run: &str) -> Session {
    sess(actor, Some(run), Role::Writer, SessionKind::Agent)
}

/// One block row of an export tree's doc file.
fn block_yaml(
    id: &str,
    ty: &str,
    parent: Option<&str>,
    order: &str,
    fields: &[(&str, &str)],
) -> String {
    let mut s = format!("- id: {id}\n  type: {ty}\n");
    if let Some(p) = parent {
        s.push_str(&format!("  parent: {p}\n"));
    }
    s.push_str(&format!("  order: {order}\n  fields:\n"));
    for (k, v) in fields {
        s.push_str(&format!("    {k}: {v}\n"));
    }
    s
}

/// An export tree at `out`: doc `name` (id `doc`) with section `section`, written by agent:a
/// (run r1) at 100, and one decision per `(id, status)` under the section, written by agent:b
/// (run r2) at 200.
fn tree(out: &Path, name: &str, doc: &str, section: &str, decisions: &[(&str, &str)]) {
    std::fs::create_dir_all(out.join("docs")).unwrap();
    std::fs::write(
        out.join("rdy-export.yaml"),
        "format: 3\npack: rdy-standard\nscope: work\n",
    )
    .unwrap();
    let mut docs = block_yaml(
        doc,
        "doc",
        None,
        "a1",
        &[
            ("doc_type", "overview"),
            ("key", name),
            ("title", "Imported"),
        ],
    );
    docs.push_str(&block_yaml(
        section,
        "section",
        Some(doc),
        "a1",
        &[("title", "Rulings")],
    ));
    for (n, (id, status)) in decisions.iter().enumerate() {
        let statement = format!("Decision {id} holds");
        docs.push_str(&block_yaml(
            id,
            "decision",
            Some(section),
            &format!("a{}", n + 1),
            &[
                ("needs_content", "false"),
                ("statement", &statement),
                ("status", status),
            ],
        ));
    }
    std::fs::write(out.join("docs").join(format!("{name}.yaml")), docs).unwrap();
    let row = |id: &str, actor: &str, run: &str, at: i64| {
        format!("- id: {id}\n  actor: {actor}\n  run: {run}\n  role: writer\n  at_ms: {at}\n")
    };
    let mut authors = row(doc, "agent:a", "r1", 100) + &row(section, "agent:a", "r1", 100);
    for (id, _) in decisions {
        authors.push_str(&row(id, "agent:b", "r2", 200));
    }
    std::fs::write(out.join("authors.yaml"), authors).unwrap();
}

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

fn canon_blocks(r: &Rdy) -> std::collections::BTreeMap<BlockId, Hash> {
    r.read()
        .materialized(&Scope::Canon)
        .unwrap()
        .snapshot()
        .blocks
}

#[test]
fn ui2_writer_import_preserves_authors_and_names_the_session() {
    let t = tempfile::tempdir().unwrap();
    let out = t.path().join("x");
    tree(
        &out,
        "imported",
        "d-AA001",
        "s-AA001",
        &[("D-AC001", "accepted"), ("D-RJ001", "rejected")],
    );
    for importer in [
        agent("agent:w", "w1"),
        sess("human:rev", None, Role::Reviewer, SessionKind::Human),
        sess("human:founder", None, Role::Approver, SessionKind::Human),
    ] {
        let (_d, mut r) = project();
        let canon = canon_blocks(&r);
        let view = importer.default_view();
        let res = r
            .import(&importer, &view, &out)
            .unwrap_or_else(|e| panic!("{}: {e:?}", importer.actor));
        // one change per author, oldest first: agent:a's doc and section, then agent:b's decisions
        assert_eq!(res.changes.len(), 2, "{res:?}");
        let st = r.read();
        let ch = |h: &str| st.reader().change(&h.parse::<Hash>().unwrap()).unwrap();
        let (one, two) = (ch(&res.changes[0]), ch(&res.changes[1]));
        for (c, actor, run, at) in [(&one, "agent:a", "r1", 100), (&two, "agent:b", "r2", 200)] {
            assert_eq!(
                (
                    c.provenance.actor.as_str(),
                    c.provenance.run.as_deref(),
                    c.provenance.role,
                    c.provenance.at_ms
                ),
                (actor, Some(run), Role::Writer, at),
                "{}",
                importer.actor
            );
            assert_eq!(
                c.links.imported_by,
                Some(ActorRun {
                    actor: importer.actor.clone(),
                    run: importer.run.clone(),
                }),
                "the importer is the session, never an author"
            );
            assert_eq!(c.view, view);
        }
        // the decisions keep the status the tree gives them, in the view only
        let m = st.materialized(&Scope::View(view.clone())).unwrap();
        let status = |id: &str| {
            m.get(&BlockId::parse(id).unwrap())
                .and_then(|b| b.field_str("status").map(str::to_string))
        };
        assert_eq!(status("D-AC001").as_deref(), Some("accepted"));
        assert_eq!(status("D-RJ001").as_deref(), Some("rejected"));
        drop(st);
        assert_eq!(canon_blocks(&r), canon, "canon is unchanged");
    }
}

#[test]
fn ui2_reader_and_forbidden_views_fail_before_file_reads() {
    let (_d, mut r) = project();
    // agent:o owns view `theirs`
    r.write(
        &agent("agent:o", "o1"),
        WriteRequest {
            view: "theirs".into(),
            ops: parse_ops(
                serde_json::from_value(serde_json::json!([
                    {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "owned", "title": "Owned", "doc_type": "overview"}}
                ]))
                .unwrap(),
            )
            .unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    // the sentinel is never created: a refusal after a file read would say it is missing
    let t = tempfile::tempdir().unwrap();
    let sentinel = t.path().join("never-read");
    let seq = r.read().seq();
    let w = agent("agent:w", "w1");
    let cases: Vec<(Session, &str, &str, &str, &str)> = vec![
        (
            sess("human:qa", None, Role::Reader, SessionKind::Human),
            "imp",
            E_PERMISSION,
            "role Reader cannot import",
            "start the session as a writer",
        ),
        (
            sess("agent:r", Some("r1"), Role::Reader, SessionKind::Agent),
            "r1",
            E_PERMISSION,
            "role Reader cannot import",
            "start the session as a writer",
        ),
        (
            w.clone(),
            "canon",
            E_RESERVED_VIEW,
            "canon cannot be written directly",
            "write to a view and land it with rdy_propose",
        ),
        (
            sess("human:founder", None, Role::Approver, SessionKind::Human),
            "canon",
            E_RESERVED_VIEW,
            "canon cannot be written directly",
            "write to a view and land it with rdy_propose",
        ),
        (
            w.clone(),
            "theirs",
            E_VIEW_OWNER,
            "view \"theirs\" is owned by agent:o (run o1)",
            "write to your own view (w1), fork it, or include its changes into your own view",
        ),
        (
            w.clone(),
            "",
            E_BAD_INPUT,
            "a view name is empty",
            "name a view, e.g. work",
        ),
    ];
    for (s, view, code, message, fix) in cases {
        let e = r.import(&s, view, &sentinel).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (code, message, fix),
            "{} into {view:?}",
            s.actor
        );
    }
    // a reserved view refuses with its own error
    let e = r.import(&w, "reaffirm-x", &sentinel).unwrap_err();
    assert_eq!(e.code, E_RESERVED_VIEW, "{e:?}");
    assert!(!sentinel.exists());
    assert_eq!(r.read().seq(), seq, "nothing was written");
    assert!(r.read().view_head("imp").is_none());
    assert!(r.read().view_head("r1").is_none());
}

/// The keys of `v`'s object, sorted.
fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

#[test]
fn ui2_import_report_keeps_every_existing_key() {
    let t = tempfile::tempdir().unwrap();
    let out = t.path().join("x");
    tree(
        &out,
        "imported",
        "d-AA001",
        "s-AA001",
        &[("D-AC001", "accepted"), ("D-PR001", "proposed")],
    );
    // the daemon's import op, as the human
    let served = Served::new();
    let (ok, daemon) = served.json(&["--json", "import", out.to_str().unwrap(), "--view", "imp"]);
    assert!(ok, "{daemon}");
    // run_import on a twin project, as the same human
    let (_d, r) = project();
    let e = Local::new(r);
    let human = Session::human("human:qa".into(), Role::Approver).unwrap();
    let mine = run_import(&e, &human, "imp", &out).unwrap();
    assert_eq!(keys(&mine), keys(&daemon), "{mine:#}\n{daemon:#}");
    for key in [
        "changes",
        "change",
        "created",
        "warnings",
        "warning_docs",
        "coverage",
        "onboarding",
    ] {
        assert_eq!(
            mine.get(key),
            daemon.get(key),
            "{key}: {mine:#}\n{daemon:#}"
        );
    }
    assert!(mine.get("coverage_error").is_none(), "{mine:#}");
    assert!(
        mine["warnings"].as_array().is_some_and(|w| !w.is_empty()),
        "{mine:#}"
    );
    assert!(mine.get("consequences").is_some(), "{mine:#}");
    // M3e's write receipt (D-B2KRN) stays under `briefing`; an import has no cited briefing
    assert_eq!(keys(&mine["briefing"]), ["covers", "token"], "{mine:#}");
    assert_eq!(mine["briefing"]["covers"], daemon["briefing"]["covers"]);
    assert!(mine.get("cited_briefing").is_none(), "{mine:#}");

    // an agent writer's import replies the same way, read as the importing session
    let (_d2, r2) = project();
    let e2 = Local::new(r2);
    let w = agent("agent:w", "w1");
    let reply = run_import(&e2, &w, "w1", &out).unwrap();
    assert_eq!(keys(&reply), keys(&mine), "{reply:#}");
    // the import ran once: the view holds exactly the reply's changes
    let changes: Vec<String> = reply["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect();
    assert_eq!(changes.len(), 2, "{reply:#}");
    let held: Vec<String> = e2
        .read()
        .included("w1")
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(held, changes, "one import, never retried");
    // a refused import is the write's own error: nothing is reported or written
    let seq = e2.read().seq();
    let reader = sess("human:qa", None, Role::Reader, SessionKind::Human);
    let err = run_import(&e2, &reader, "other", &out).unwrap_err();
    assert_eq!(err.code, E_PERMISSION);
    assert_eq!(e2.read().seq(), seq);
}
