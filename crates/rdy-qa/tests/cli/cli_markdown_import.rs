//! m4bc5 (D-19BZD, D-FSWQE, D-2R9EY, D-KQ4CA, D-Y9DNZ): the Markdown import reaches the chat tool
//! `rdy_import`, the daemon `import` op and `rdy import` with the same three options (`md`,
//! `dry_run`, `expected_source`), the same refusals and the same reply. The options are closed;
//! without them an import is the format-3 import it was. A reader, canon and another owner's view
//! refuse before the source is read, and a refused import leaves no view and no claim.
use rdy_daemon::{OpContext, dispatch};
use rdy_engine::engine::Local;
use rdy_engine::{Rdy, Session};
use rdy_model::Role;
use rdy_qa::daemon::Served;
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::path::Path;

const NEEDS_MD: (&str, &str, &str) = (
    "E_BAD_INPUT",
    "dry_run and expected_source require md: true",
    "use rdy_import with md: true, or rdy import --md",
);
const BAD_DIGEST: (&str, &str, &str) = (
    "E_BAD_INPUT",
    "expected_source must be 64 lowercase hexadecimal characters",
    "use source_digest from a Markdown import preview",
);
const CHANGED: (&str, &str, &str) = (
    "E_BAD_INPUT",
    "the Markdown source changed after preview",
    "preview the source again and review the new report",
);

/// A Markdown source: two docs (one links to the other), a raw HTML line and a picture.
fn source(root: &Path) {
    for (rel, bytes) in [
        (
            "a.md",
            "# Alpha\n\nSee [beta](sub/b.md).\n\n<b>html</b>\n".as_bytes(),
        ),
        ("sub/b.md", "# Beta\n\nText.\n".as_bytes()),
        ("pics/one.png", &[1u8, 2, 3][..]),
    ] {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
}

/// A source whose one doc names a doc type the pack lacks.
fn bad_source(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(
        root.join("a.md"),
        "---\ndoc_type: nonesuch\n---\n# Alpha\n\nText.\n",
    )
    .unwrap();
}

fn session(c: &mut McpClient, actor: &str, run: &str, role: &str) {
    c.call(
        "rdy_session",
        json!({"actor": actor, "run": run, "role": role}),
    )
    .ok();
}

fn refused(r: Reply) -> (String, String, String) {
    let Reply::ToolError(e) = r else {
        panic!("expected a tool error, got {r:?}")
    };
    let s = |k: &str| e[k].as_str().unwrap_or_default().to_string();
    (s("code"), s("message"), s("fix"))
}

fn is(got: &(String, String, String), want: (&str, &str, &str)) {
    assert_eq!((&*got.0, &*got.1, &*got.2), want, "{got:?}");
}

fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

/// What `rdy --json import` printed on failure: its code, message and fix.
fn cli_refused(v: &Value) -> (String, String, String) {
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    (s("code"), s("message"), s("fix"))
}

/// The daemon's op, run in process against a project of its own.
struct Op {
    dir: tempfile::TempDir,
    engine: Local,
}

impl Op {
    fn new() -> Op {
        let dir = tempfile::tempdir().unwrap();
        let engine = Local::new(Rdy::init(dir.path(), "rdy-standard").unwrap());
        Op { dir, engine }
    }

    fn call(&self, session: &Session, args: Value) -> Result<Value, rdy_engine::RdyError> {
        let cx = OpContext {
            engine: &self.engine,
            session,
            human: "human:qa",
            cwd: self.dir.path(),
            agent: &rdy_engine::signing_crypto::UnavailableAgent,
        };
        dispatch(&cx, "import", args)
    }
}

fn agent(actor: &str, run: &str, role: Role) -> Session {
    Session::agent(actor.into(), Some(run.into()), role).unwrap()
}

fn triple(e: &rdy_engine::RdyError) -> (String, String, String) {
    (e.code.to_string(), e.message.clone(), e.fix.clone())
}

#[test]
fn m4bc5_import_options_are_closed_and_keep_old_defaults() {
    let s = Served::new();
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("md");
    source(&src);
    let dir = src.to_str().unwrap();

    // chat: the schema lists the three options, optional, and nothing else is accepted
    let mut c = McpClient::start_proxy(&s);
    let tools = c.tools();
    let tool = tools.iter().find(|t| t["name"] == "rdy_import").unwrap();
    let schema = &tool["inputSchema"];
    assert_eq!(
        keys(&schema["properties"]),
        ["dir", "dry_run", "expected_source", "md", "view"]
    );
    let mut required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    required.sort_unstable();
    assert_eq!(required, ["dir", "view"]);
    assert_eq!(schema["additionalProperties"], json!(false));
    session(&mut c, "agent:o", "o1", "writer");
    let (code, message, fix) = refused(c.call(
        "rdy_import",
        json!({"dir": dir, "view": "o1", "dryrun": true}),
    ));
    assert_eq!(code, "E_BAD_INPUT");
    assert_eq!(message, "rdy_import: unknown parameter \"dryrun\"");
    assert!(fix.starts_with("did you mean \"dry_run\"?"), "{fix}");
    let (_, message, fix) = refused(c.call(
        "rdy_import",
        json!({"dir": dir, "view": "o1", "expected_sorce": "x"}),
    ));
    assert_eq!(message, "rdy_import: unknown parameter \"expected_sorce\"");
    assert!(
        fix.starts_with("did you mean \"expected_source\"?"),
        "{fix}"
    );
    // each option needs md, and a digest has one form; nothing is read or written for either
    is(
        &refused(c.call(
            "rdy_import",
            json!({"dir": dir, "view": "o1", "dry_run": true}),
        )),
        NEEDS_MD,
    );
    is(
        &refused(c.call(
            "rdy_import",
            json!({"dir": dir, "view": "o1", "md": false, "expected_source": "0".repeat(64)}),
        )),
        NEEDS_MD,
    );
    is(
        &refused(c.call(
            "rdy_import",
            json!({"dir": dir, "view": "o1", "md": true, "expected_source": "ABC"}),
        )),
        BAD_DIGEST,
    );
    // a wrong type is refused, not coerced
    let (code, ..) = refused(c.call("rdy_import", json!({"dir": dir, "view": "o1", "md": "yes"})));
    assert_eq!(code, "E_BAD_INPUT");

    // the daemon's op takes the same keys, closed, with the same refusals
    let op = Op::new();
    let w = agent("agent:w", "w1", Role::Writer);
    let e = op
        .call(&w, json!({"dir": dir, "view": "w1", "dry_run": true}))
        .unwrap_err();
    is(&triple(&e), NEEDS_MD);
    let e = op
        .call(
            &w,
            json!({"dir": dir, "view": "w1", "md": true, "expected_source": "ABC"}),
        )
        .unwrap_err();
    is(&triple(&e), BAD_DIGEST);
    let e = op
        .call(&w, json!({"dir": dir, "view": "w1", "mdd": true}))
        .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e:?}");
    assert!(e.fix.contains("md"), "{e:?}");
    drop(op);

    // the command line: the flags, the same refusals
    let (ok, v) = s.json_as(
        "agent:o",
        Some("o2"),
        &["--json", "import", dir, "--view", "o2", "--dry-run"],
    );
    assert!(!ok);
    is(&cli_refused(&v), NEEDS_MD);
    let (ok, v) = s.json_as(
        "agent:o",
        Some("o2"),
        &[
            "--json",
            "import",
            dir,
            "--view",
            "o2",
            "--expected-source",
            &"0".repeat(64),
        ],
    );
    assert!(!ok);
    is(&cli_refused(&v), NEEDS_MD);
    let (ok, v) = s.json_as(
        "agent:o",
        Some("o2"),
        &[
            "--json",
            "import",
            dir,
            "--view",
            "o2",
            "--md",
            "--expected-source",
            "ABC",
        ],
    );
    assert!(!ok);
    is(&cli_refused(&v), BAD_DIGEST);

    // the defaults: `md: false` is the old import, and a format-3 reply has no Markdown field
    let tree = t.path().join("tree");
    std::fs::create_dir_all(tree.join("docs")).unwrap();
    std::fs::write(
        tree.join("rdy-export.yaml"),
        "format: 3\npack: rdy-standard\nscope: work\n",
    )
    .unwrap();
    std::fs::write(
        tree.join("docs/imported.yaml"),
        "- id: d-AA001\n  type: doc\n  order: a1\n  fields:\n    doc_type: overview\n    key: imported\n    title: Imported\n",
    )
    .unwrap();
    std::fs::write(
        tree.join("authors.yaml"),
        "- id: d-AA001\n  actor: agent:a\n  run: r1\n  role: writer\n  at_ms: 100\n",
    )
    .unwrap();
    let tdir = tree.to_str().unwrap();
    let plain = c
        .call("rdy_import", json!({"dir": tdir, "view": "o1"}))
        .ok();
    for k in ["source_digest", "files", "unmapped", "dry_run"] {
        assert!(plain.get(k).is_none(), "{k} in {plain:#}");
    }
    assert!(plain["created"].is_object(), "{plain:#}");
    let explicit = c
        .call(
            "rdy_import",
            json!({"dir": tdir, "view": "o3", "md": false}),
        )
        .ok();
    assert_eq!(keys(&plain), keys(&explicit));
}

#[test]
fn m4bc5_preview_and_import_match_on_cli_and_mcp() {
    let s = Served::new();
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("md");
    source(&src);
    let dir = src.to_str().unwrap();

    // a preview: the same reply on both transports, and no view anywhere
    let mut c = McpClient::start_proxy(&s);
    session(&mut c, "agent:chat", "m1", "writer");
    let chat = c
        .call(
            "rdy_import",
            json!({"dir": dir, "view": "m1", "md": true, "dry_run": true}),
        )
        .ok();
    let (ok, cli) = s.json_as(
        "agent:cli",
        Some("c1"),
        &["--json", "import", dir, "--view", "c1", "--md", "--dry-run"],
    );
    assert!(ok, "{cli}");
    assert_eq!(chat, cli, "one preview");
    assert_eq!(chat["dry_run"], json!(true));
    assert_eq!(
        keys(&chat),
        ["blocks", "dry_run", "files", "source_digest", "unmapped"]
    );
    assert!(chat.get("changes").is_none() && chat.get("created").is_none());
    let digest = chat["source_digest"].as_str().unwrap().to_string();
    assert_eq!(digest.len(), 64);
    // D-2R9EY: a reported file has line 0; the HTML keeps its one-based line
    assert_eq!(
        chat["unmapped"],
        json!([
            {"path": "a.md", "line": 5, "kind": "html"},
            {"path": "pics/one.png", "line": 0, "kind": "non_markdown_file"},
        ])
    );
    let files: Vec<&str> = chat["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(files, ["a.md", "sub/b.md"]);

    // the command line's preview text: digest, counts, every unmapped row, nothing written
    let out = s.rdy_as(
        "agent:cli",
        Some("c1"),
        &["import", dir, "--view", "c1", "--md", "--dry-run"],
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{text}");
    assert!(text.contains(&format!("source digest: {digest}")), "{text}");
    assert!(text.contains("2 files"), "{text}");
    assert!(text.contains("a.md:5 html"), "{text}");
    assert!(text.contains("pics/one.png:0 non_markdown_file"), "{text}");
    assert!(text.contains("nothing written"), "{text}");

    // the import with the preview's digest: the same digest, files and unmapped rows, the same
    // reply keys, on both transports
    let chat = c
        .call(
            "rdy_import",
            json!({"dir": dir, "view": "m1", "md": true, "expected_source": digest}),
        )
        .ok();
    let (ok, cli) = s.json_as(
        "agent:cli",
        Some("c2"),
        &[
            "--json",
            "import",
            dir,
            "--view",
            "c2",
            "--md",
            "--expected-source",
            &digest,
        ],
    );
    assert!(ok, "{cli}");
    assert_eq!(keys(&chat), keys(&cli), "{chat:#}\n{cli:#}");
    for k in ["source_digest", "files", "unmapped"] {
        assert_eq!(chat[k], cli[k], "{k}");
    }
    assert_eq!(chat["source_digest"], json!(digest));
    for k in ["changes", "created", "coverage", "onboarding"] {
        assert!(chat.get(k).is_some(), "{k}: {chat:#}");
    }
    let out = s.rdy_as(
        "agent:cli",
        Some("c3"),
        &["import", dir, "--view", "c3", "--md"],
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{text}");
    let n = cli["created"].as_object().unwrap().len();
    assert_eq!(
        text.lines().next(),
        Some(format!("imported {n} blocks into view c3").as_str()),
        "{text}"
    );
    assert!(text.contains(&format!("source digest: {digest}")), "{text}");
    assert!(text.contains("2 files"), "{text}");
    assert!(text.contains("a.md:5 html"), "{text}");
    assert!(text.contains("pics/one.png:0 non_markdown_file"), "{text}");
    // a source that changed after its preview refuses with the exact words
    std::fs::write(src.join("sub/b.md"), "# Beta\n\nChanged.\n").unwrap();
    is(
        &refused(c.call(
            "rdy_import",
            json!({"dir": dir, "view": "m9", "md": true, "expected_source": digest}),
        )),
        CHANGED,
    );
    drop(c);

    // the preview made no view; the imports made theirs
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    assert!(r.read().view_head("c1").is_none());
    assert!(r.read().view_head("m9").is_none());
    for v in ["m1", "c2", "c3"] {
        assert!(r.read().view_head(v).is_some(), "{v}");
    }
    drop(r);
}

#[test]
fn m4bc5_readers_canon_and_other_owners_refuse_before_reads() {
    let s = Served::new();
    let t = tempfile::tempdir().unwrap();
    // no source exists: a refusal that names the role, canon or the owner proves no read
    let gone = t.path().join("never-read");
    let dir = gone.to_str().unwrap();
    let reader = (
        "E_PERMISSION",
        "role Reader cannot import",
        "start the session as a writer",
    );
    let canon = (
        "E_RESERVED_VIEW",
        "canon cannot be written directly",
        "write to a view and land it with rdy_propose",
    );

    // another owner's view: a real one, owned by agent:own
    let real = t.path().join("md");
    source(&real);
    let mut own = McpClient::start_proxy(&s);
    session(&mut own, "agent:own", "ow1", "writer");
    own.call(
        "rdy_import",
        json!({"dir": real.to_str().unwrap(), "view": "mine", "md": true}),
    )
    .ok();
    drop(own);

    let mut c = McpClient::start_proxy(&s);
    session(&mut c, "agent:rdr", "rd1", "reader");
    for extra in [json!({}), json!({"dry_run": true})] {
        let mut a = json!({"dir": dir, "view": "rd1", "md": true});
        a.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        is(&refused(c.call("rdy_import", a)), reader);
    }
    drop(c);
    let mut c = McpClient::start_proxy(&s);
    session(&mut c, "agent:w", "w1", "writer");
    for extra in [json!({}), json!({"dry_run": true})] {
        for (view, want) in [("canon", Some(canon)), ("mine", None)] {
            let mut a = json!({"dir": dir, "view": view, "md": true});
            a.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let got = refused(c.call("rdy_import", a));
            match want {
                Some(w) => is(&got, w),
                None => {
                    assert_eq!(got.0, "E_VIEW_OWNER", "{got:?}");
                    assert!(got.1.contains("is owned by agent:own"), "{got:?}");
                }
            }
        }
    }
    drop(c);

    // the daemon's op
    let op = Op::new();
    let r = agent("agent:rdr", "rd1", Role::Reader);
    let w = agent("agent:w", "w1", Role::Writer);
    for extra in [json!({}), json!({"dry_run": true})] {
        let mut a = json!({"dir": dir, "view": "rd1", "md": true});
        a.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        is(&triple(&op.call(&r, a).unwrap_err()), reader);
        let mut a = json!({"dir": dir, "view": "canon", "md": true});
        a.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        is(&triple(&op.call(&w, a).unwrap_err()), canon);
    }
    drop(op);

    // the command line: an agent there is a writer, so a reader is a chat and daemon case only
    for flags in [&["--md"][..], &["--md", "--dry-run"][..]] {
        let mut args = vec!["--json", "import", dir, "--view", "canon"];
        args.extend_from_slice(flags);
        let (ok, v) = s.json_as("agent:w", Some("w1"), &args);
        assert!(!ok);
        is(&cli_refused(&v), canon);
        let mut args = vec!["--json", "import", dir, "--view", "mine"];
        args.extend_from_slice(flags);
        let (ok, v) = s.json_as("agent:w", Some("w1"), &args);
        assert!(!ok);
        assert_eq!(v["code"], "E_VIEW_OWNER", "{v}");
    }
}

#[test]
fn m4bc5_failed_import_leaves_no_claim_through_each_transport() {
    let s = Served::new();
    let t = tempfile::tempdir().unwrap();
    let bad = t.path().join("bad");
    bad_source(&bad);
    let good = t.path().join("good");
    source(&good);
    let (bad, good) = (bad.to_str().unwrap(), good.to_str().unwrap());
    let zeros = "0".repeat(64);

    // chat: an unknown doc type refuses with the schema's own words, a stale digest refuses
    let mut c = McpClient::start_proxy(&s);
    session(&mut c, "agent:m", "m1", "writer");
    let (code, message, fix) =
        refused(c.call("rdy_import", json!({"dir": bad, "view": "m1", "md": true})));
    assert_eq!(code, "E_DOC_TYPE");
    assert!(
        message.contains("unknown doc_type \"nonesuch\""),
        "{message}"
    );
    assert!(fix.starts_with("use one of: "), "{fix}");
    is(
        &refused(c.call(
            "rdy_import",
            json!({"dir": good, "view": "m1", "md": true, "expected_source": zeros}),
        )),
        CHANGED,
    );
    let (code, ..) = refused(c.call(
        "rdy_import",
        json!({"dir": bad, "view": "m1", "md": true, "dry_run": true}),
    ));
    assert_eq!(code, "E_DOC_TYPE");
    drop(c);

    // the daemon's op
    let op = Op::new();
    let w = agent("agent:w", "w1", Role::Writer);
    let e = op
        .call(&w, json!({"dir": bad, "view": "d1", "md": true}))
        .unwrap_err();
    assert_eq!(e.code, "E_DOC_TYPE", "{e:?}");
    let e = op
        .call(
            &w,
            json!({"dir": good, "view": "d1", "md": true, "expected_source": zeros}),
        )
        .unwrap_err();
    is(&triple(&e), CHANGED);
    let engine_dir = op.dir.path().to_path_buf();
    let Op {
        engine, dir: held, ..
    } = op;
    drop(engine);
    let r = Rdy::open(&engine_dir).unwrap();
    assert!(r.read().view_head("d1").is_none(), "no view, no claim");
    assert!(r.read().included("d1").is_empty());
    drop(r);
    drop(held);

    // the command line
    let (ok, v) = s.json_as(
        "agent:c",
        Some("c1"),
        &["--json", "import", bad, "--view", "c1", "--md"],
    );
    assert!(!ok);
    assert_eq!(v["code"], "E_DOC_TYPE", "{v}");
    let (ok, v) = s.json_as(
        "agent:c",
        Some("c1"),
        &[
            "--json",
            "import",
            good,
            "--view",
            "c1",
            "--md",
            "--expected-source",
            &zeros,
        ],
    );
    assert!(!ok);
    is(&cli_refused(&v), CHANGED);

    // the served project: no view was created and none is owned by a refused import
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    for view in ["m1", "c1"] {
        assert!(r.read().view_head(view).is_none(), "{view}");
        assert!(r.read().included(view).is_empty(), "{view}");
    }
    drop(r);
}
