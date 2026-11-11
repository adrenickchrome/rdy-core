//! ui6 (D-Y9DNZ, D-KQ4CA, D-XR09R): a writer session imports a format-3 tree through chat
//! (`rdy_import`) and through the command line (`rdy import`) with one reply: the engine's
//! `run_import` value, with no key renamed or dropped. A reader, canon and a misspelt key are
//! refused on every surface that can reach them, with no write. An agent session cannot tag
//! alone, and says so in its own words.
use rdy_daemon::{OpContext, dispatch};
use rdy_engine::engine::Local;
use rdy_engine::{Rdy, Scope, Session};
use rdy_model::{ActorRun, Hash, Role};
use rdy_qa::daemon::Served;
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

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

/// A format-3 export tree at `out`: doc `imported` (id d-AA001) with section s-AA001 by agent:a
/// (run r1) at 100, and two decisions by agent:b (run r2) at 200, one accepted and one proposed.
fn tree(out: &Path) {
    std::fs::create_dir_all(out.join("docs")).unwrap();
    std::fs::write(
        out.join("rdy-export.yaml"),
        "format: 3\npack: rdy-standard\nscope: work\n",
    )
    .unwrap();
    let mut docs = block_yaml(
        "d-AA001",
        "doc",
        None,
        "a1",
        &[
            ("doc_type", "overview"),
            ("key", "imported"),
            ("title", "Imported"),
        ],
    );
    docs.push_str(&block_yaml(
        "s-AA001",
        "section",
        Some("d-AA001"),
        "a1",
        &[("title", "Rulings")],
    ));
    for (n, (id, status)) in [("D-AC001", "accepted"), ("D-PR001", "proposed")]
        .iter()
        .enumerate()
    {
        let statement = format!("Decision {id} holds");
        docs.push_str(&block_yaml(
            id,
            "decision",
            Some("s-AA001"),
            &format!("a{}", n + 1),
            &[
                ("needs_content", "false"),
                ("statement", &statement),
                ("status", status),
            ],
        ));
    }
    std::fs::write(out.join("docs/imported.yaml"), docs).unwrap();
    let row = |id: &str, actor: &str, run: &str, at: i64| {
        format!("- id: {id}\n  actor: {actor}\n  run: {run}\n  role: writer\n  at_ms: {at}\n")
    };
    let authors = row("d-AA001", "agent:a", "r1", 100)
        + &row("s-AA001", "agent:a", "r1", 100)
        + &row("D-AC001", "agent:b", "r2", 200)
        + &row("D-PR001", "agent:b", "r2", 200);
    std::fs::write(out.join("authors.yaml"), authors).unwrap();
}

fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

/// The warning texts of an import reply, with the doc label the CLI prints before each.
fn warning_texts(v: &Value) -> Vec<String> {
    v["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|w| {
            let doc = w["block"]
                .as_str()
                .and_then(|id| v["warning_docs"][id].as_str())
                .unwrap_or_default();
            format!(
                "{}: {doc}{}",
                w["code"].as_str().unwrap_or_default(),
                w["message"].as_str().unwrap_or_default()
            )
        })
        .collect()
}

/// The code, message and fix of a tool error.
fn refused(r: Reply) -> (String, String, String) {
    let Reply::ToolError(e) = r else {
        panic!("expected a tool error, got {r:?}")
    };
    let s = |k: &str| e[k].as_str().unwrap_or_default().to_string();
    (s("code"), s("message"), s("fix"))
}

fn session(c: &mut McpClient, actor: &str, run: &str, role: &str) {
    c.call(
        "rdy_session",
        json!({"actor": actor, "run": run, "role": role}),
    )
    .ok();
}

/// What a view holds, without its provenance: each block as the view materializes it.
fn blocks_of(r: &Rdy, view: &str) -> BTreeMap<String, String> {
    let m = r
        .read()
        .materialized(&Scope::View(view.to_string()))
        .unwrap();
    m.iter()
        .map(|b| {
            (
                b.id.to_string(),
                format!(
                    "{} {:?} {} {:?} {}",
                    b.block_type, b.parent, b.order, b.fields, b.body
                ),
            )
        })
        .collect()
}

/// For each change of `view`: its author, run and time, and who imported it.
fn changes_of(r: &Rdy, view: &str) -> Vec<(String, Option<String>, i64, Option<ActorRun>)> {
    let st = r.read();
    st.included(view)
        .iter()
        .map(|h| {
            let h: Hash = h.to_string().parse().unwrap();
            let c = st.reader().change(&h).unwrap();
            (
                c.provenance.actor.clone(),
                c.provenance.run.clone(),
                c.provenance.at_ms,
                c.links.imported_by.clone(),
            )
        })
        .collect()
}

#[test]
fn ui6_import_is_chat_first_and_cli_equivalent() {
    let s = Served::new();
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("tree");
    tree(&src);
    let dir = src.to_str().unwrap();

    // chat: a writer session imports into its own view
    let mut c = McpClient::start_proxy(&s);
    session(&mut c, "agent:chat", "m1", "writer");
    let chat = c.call("rdy_import", json!({"dir": dir, "view": "m1"})).ok();
    // the command line: an agent session imports the same tree into its own view
    let (ok, cli) = s.json_as(
        "agent:cli",
        Some("c1"),
        &["--json", "import", dir, "--view", "c1"],
    );
    assert!(ok, "{cli}");

    // one reply: the same keys, the same warning texts, the write receipt under briefing
    assert_eq!(keys(&chat), keys(&cli), "{chat:#}\n{cli:#}");
    for key in [
        "changes",
        "change",
        "created",
        "warnings",
        "warning_docs",
        "coverage",
        "onboarding",
    ] {
        assert!(chat.get(key).is_some(), "{key} is missing: {chat:#}");
    }
    assert_eq!(chat["created"].as_object().unwrap().len(), 4, "{chat:#}");
    assert_eq!(chat["changes"].as_array().unwrap().len(), 2, "{chat:#}");
    assert_eq!(keys(&chat["briefing"]), ["covers", "token"], "{chat:#}");
    assert!(chat.get("cited_briefing").is_none(), "{chat:#}");
    let warnings = warning_texts(&chat);
    assert!(!warnings.is_empty(), "{chat:#}");
    assert_eq!(warnings, warning_texts(&cli));
    assert_eq!(chat["warning_docs"], cli["warning_docs"]);

    // the command line's text keeps its first line and its warnings
    let out = s.rdy_as("agent:cli", Some("c2"), &["import", dir, "--view", "c2"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        text.lines().next(),
        Some("imported 4 blocks into view c2 in 2 changes, one per author"),
        "{text}"
    );
    for w in &warnings {
        assert!(text.contains(w), "the text lacks {w:?}: {text}");
    }
    drop(c);

    // the stores agree: the same blocks, the same source authors, each importer recorded
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    assert_eq!(blocks_of(&r, "m1"), blocks_of(&r, "c1"));
    assert_eq!(blocks_of(&r, "m1").len(), 4);
    let imported_by = |actor: &str, run: &str| {
        Some(ActorRun {
            actor: actor.into(),
            run: Some(run.into()),
        })
    };
    let author = |ch: &[(String, Option<String>, i64, Option<ActorRun>)]| {
        ch.iter()
            .map(|(a, run, at, _)| (a.clone(), run.clone(), *at))
            .collect::<Vec<_>>()
    };
    let (m, k) = (changes_of(&r, "m1"), changes_of(&r, "c1"));
    assert_eq!(
        author(&m),
        [
            ("agent:a".into(), Some("r1".into()), 100),
            ("agent:b".into(), Some("r2".into()), 200)
        ]
    );
    assert_eq!(author(&m), author(&k), "the same source authors");
    assert!(
        m.iter().all(|c| c.3 == imported_by("agent:chat", "m1")),
        "{m:?}"
    );
    assert!(
        k.iter().all(|c| c.3 == imported_by("agent:cli", "c1")),
        "{k:?}"
    );
    drop(r);
}

#[test]
fn ui6_reader_import_and_canon_are_refused_on_every_surface() {
    let s = Served::new();
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("tree");
    tree(&src);
    let dir = src.to_str().unwrap();

    // chat: a reader is refused, a writer is refused canon, and a misspelt key gets the
    // nearest key
    let mut c = McpClient::start_proxy(&s);
    session(&mut c, "agent:rdr", "rd1", "reader");
    assert_eq!(
        refused(c.call("rdy_import", json!({"dir": dir, "view": "rd1"}))),
        (
            "E_PERMISSION".into(),
            "role Reader cannot import".into(),
            "start the session as a writer".into()
        )
    );
    drop(c);
    // a connection is bound to one actor and run, so the writer has its own
    let mut c = McpClient::start_proxy(&s);
    session(&mut c, "agent:w", "w1", "writer");
    assert_eq!(
        refused(c.call("rdy_import", json!({"dir": dir, "view": "canon"}))),
        (
            "E_RESERVED_VIEW".into(),
            "canon cannot be written directly".into(),
            "write to a view and land it with rdy_propose".into()
        )
    );
    assert_eq!(
        refused(c.call("rdy_import", json!({"dri": dir, "view": "w1"}))),
        (
            "E_BAD_INPUT".into(),
            "rdy_import: unknown parameter \"dri\"".into(),
            "did you mean \"dir\"? the parameters here are dir, dry_run, expected_source, md, view"
                .into()
        )
    );
    assert_eq!(
        refused(c.call("rdy_import", json!({"dir": dir, "veiw": "w1"}))),
        (
            "E_BAD_INPUT".into(),
            "rdy_import: unknown parameter \"veiw\"".into(),
            "did you mean \"view\"? the parameters here are dir, dry_run, expected_source, md, view".into()
        )
    );
    drop(c);

    // the daemon's op: the same refusals and the same nearest key
    let d = tempfile::tempdir().unwrap();
    let engine = Local::new(Rdy::init(d.path(), "rdy-standard").unwrap());
    let ctl = |session: &Session, args: Value| {
        let cx = OpContext {
            engine: &engine,
            session,
            human: "human:qa",
            cwd: d.path(),
            agent: &rdy_engine::signing_crypto::UnavailableAgent,
        };
        dispatch(&cx, "import", args)
    };
    let reader = Session::agent("agent:rdr".into(), Some("rd1".into()), Role::Reader).unwrap();
    let writer = Session::agent("agent:w".into(), Some("w1".into()), Role::Writer).unwrap();
    let e = ctl(&reader, json!({"dir": dir, "view": "rd1"})).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_PERMISSION",
            "role Reader cannot import",
            "start the session as a writer"
        )
    );
    let e = ctl(&writer, json!({"dir": dir, "view": "canon"})).unwrap_err();
    assert_eq!(e.code, "E_RESERVED_VIEW", "{e:?}");
    let e = ctl(&writer, json!({"dri": dir, "view": "w1"})).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e:?}");
    assert!(
        e.message.contains("dri") && e.fix.contains("dir"),
        "nearest key: {e:?}"
    );
    drop(engine);

    // the command line: canon is refused for an agent and for the person
    let (ok, v) = s.json_as(
        "agent:w",
        Some("w1"),
        &["--json", "import", dir, "--view", "canon"],
    );
    assert!(!ok);
    assert_eq!(v["code"], "E_RESERVED_VIEW", "{v}");
    let (ok, v) = s.json(&["--json", "import", dir, "--view", "canon"]);
    assert!(!ok);
    assert_eq!(v["code"], "E_RESERVED_VIEW", "{v}");
    // a misspelt flag gets the nearest flag from the parser
    let out = s.rdy_as("agent:w", Some("w1"), &["import", dir, "--veiw", "w1"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--view"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // nothing was written anywhere
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    // the daemon's own bookkeeping may move refs; no change, version or view is recorded
    {
        let st = r.read();
        let mut frames = vec![];
        st.reader()
            .for_each_batch(st.seq(), |b| {
                frames.extend(b.frames.iter().map(|f| f.kind()));
                Ok(())
            })
            .unwrap();
        assert!(
            frames.iter().all(|k| *k == rdy_store::kinds::REF_UPDATE),
            "only ref updates: {frames:?}"
        );
    }
    for view in ["rd1", "w1"] {
        assert!(r.read().view_head(view).is_none(), "{view}");
        assert!(r.read().included(view).is_empty(), "{view}");
    }
    drop(r);
}

#[test]
fn ui6_agent_tag_has_the_new_exact_refusal() {
    let s = Served::new();
    // a project with one landing, so a tag has something to name
    s.stop();
    {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        rdy_qa::fixtures::seed_doc(&mut r, "seed");
        rdy_qa::fixtures::land(&mut r, "seed").unwrap();
        r.close().unwrap();
    }
    // no relay: the agent's refusal, in the two exact strings
    let (ok, v) = s.json_as("agent:relay", Some("r1"), &["--json", "tag", "v0.1.0"]);
    assert!(!ok);
    assert_eq!(v["code"], "E_PERMISSION", "{v}");
    assert_eq!(v["message"], "agent sessions cannot tag", "{v}");
    assert_eq!(
        v["fix"], "tags are the person's acts: ask the person to run the command",
        "{v}"
    );
    // a relayed tag and a person's tag still pass
    let out = s.rdy_as(
        "agent:relay",
        Some("r1"),
        &[
            "tag",
            "v0.1.0",
            "--relayed-by",
            "agent:relay",
            "--evidence",
            "yes, tag it",
            "--question",
            "Tag v0.1.0?",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (ok, v) = s.json(&["--json", "tag", "v0.2.0"]);
    assert!(ok, "{v}");
}
