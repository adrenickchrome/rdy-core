//! m4bc6 (D-19BZD, D-KQ4CA, D-Y9DNZ, D-5GFFX, D-QG54Z, D-122E5): a Markdown migration holds its
//! contract through the real transports. The CLI, the stdio MCP proxy and MCP over HTTP give the
//! same import identity and result, and no child reads or changes the operator's home. A reply
//! lost after the seal is an unknown outcome, never a failure: the view shows the import once,
//! and nothing sends it again.
//!
//! Every child is a real `rdy` binary or its real daemon, in a fixture home. The HTTP URL is the
//! one `rdy status` reports (W5), never one derived from the port candidates. A daemon that
//! stops is waited on by its endpoint, never by a delay (S2, S3). The file is portable: the
//! kill goes through `Served::kill9`, which is `rdy_qa::platform::kill_hard`, on every platform.
use rdy_daemon::{Attempt, E_DAEMON_GONE, Hello, PROTO_VERSION, Proto, RunDir, connect};
use rdy_engine::{Rdy, Scope};
use rdy_qa::daemon::{Served, wait_endpoint_gone};
use rdy_qa::invariants::check;
use rdy_qa::mcp::{McpClient, Reply};
use rdy_qa::mcp_http::{Endpoint, LEGACY, call, claim, legacy, pairs};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A hang guard for what a test waits on: a failure message, never a measure.
const T: Duration = Duration::from_secs(60);
const ON: &str = "serve:\n  mcp_http: true\n";
const ACTOR: &str = "agent:mig";
const CHANGED: (&str, &str, &str) = (
    "E_BAD_INPUT",
    "the Markdown source changed after preview",
    "preview the source again and review the new report",
);
const BETA: &str = "# Beta\n\nText.\n\n| k | v |\n|---|---|\n| 1 | 2 |\n";

/// A Markdown source: two docs (one links to the other), a raw HTML line, a picture, and a
/// `.rdyignore` that leaves one file out.
fn source(root: &Path) {
    for (rel, bytes) in [
        (
            "a.md",
            "# Alpha\n\nSee [beta](sub/b.md#beta).\n\n<b>html</b>\n".as_bytes(),
        ),
        ("sub/b.md", BETA.as_bytes()),
        ("skip.draft.md", "# Draft\n".as_bytes()),
        ("pics/one.png", &[1u8, 2, 3][..]),
        (".rdyignore", "*.draft.md\n".as_bytes()),
    ] {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
}

/// Every directory and file under `root` with its logical path and bytes, in one digest.
fn tree_digest(root: &Path) -> String {
    fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, Option<Vec<u8>>)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if entry.file_type().unwrap().is_dir() {
                out.push((path.clone(), None));
                walk(&entry.path(), &path, out);
            } else {
                out.push((path, Some(std::fs::read(entry.path()).unwrap())));
            }
        }
    }
    let mut rows = vec![];
    walk(root, "", &mut rows);
    rows.sort();
    let mut buf = vec![];
    for (path, bytes) in rows {
        buf.extend((path.len() as u64).to_le_bytes());
        buf.extend(path.as_bytes());
        match bytes {
            Some(b) => {
                buf.push(1);
                buf.extend((b.len() as u64).to_le_bytes());
                buf.extend(b);
            }
            None => buf.push(0),
        }
    }
    rdy_model::Hash::of(&buf).to_string()
}

/// A served project (real `rdy`, real daemon child) with MCP over HTTP on.
fn on() -> Served {
    let s = Served::new();
    std::fs::write(s.dir.path().join(".rdy/local.yaml"), ON).unwrap();
    s
}

/// The HTTP endpoint as `rdy status` reports it (W5): its URL, with the bearer from
/// `rdy mcp --http-config`, which must name the same URL.
fn endpoint(s: &Served) -> Endpoint {
    let (ok, status) = s.json(&["--json", "status"]);
    assert!(ok, "{status}");
    assert_eq!(status["mcp_http"]["enabled"], json!(true), "{status}");
    let url = status["mcp_http"]["url"]
        .as_str()
        .expect("a url")
        .to_string();
    let (ok, cfg) = s.json(&["--json", "mcp", "--http-config"]);
    assert!(ok, "{cfg}");
    assert_eq!(
        cfg["url"],
        json!(url),
        "status and --http-config name one endpoint"
    );
    let site = url
        .strip_suffix("~mcp")
        .unwrap_or_else(|| panic!("not an MCP url: {url}"));
    let token = cfg["headers"]["Authorization"]
        .as_str()
        .and_then(|a| a.strip_prefix("Bearer "))
        .expect("a bearer");
    Endpoint::of(site, token)
}

/// One import through each transport, as the same actor with a run of its own (the run names
/// the view). Each gives the reply value, or the refusal's `{code, message, fix}`.
struct Transports<'a> {
    s: &'a Served,
    ep: Endpoint,
    next: u64,
}

impl<'a> Transports<'a> {
    fn new(s: &'a Served) -> Transports<'a> {
        Transports {
            s,
            ep: endpoint(s),
            next: 1,
        }
    }

    /// `rdy import <dir> --view <run> --md <flags>` as `ACTOR` in run `run`.
    fn cli(&self, run: &str, dir: &str, flags: &[&str]) -> Result<Value, Value> {
        let mut args = vec!["--json", "import", dir, "--view", run, "--md"];
        args.extend_from_slice(flags);
        let (ok, v) = self.s.json_as(ACTOR, Some(run), &args);
        if ok { Ok(v) } else { Err(v) }
    }

    /// `rdy_import` through a stdio proxy of its own, whose session is `ACTOR` in run `run`.
    fn stdio(&self, run: &str, args: Value) -> Result<Value, Value> {
        let mut c = McpClient::start_proxy(self.s);
        c.call(
            "rdy_session",
            json!({"actor": ACTOR, "run": run, "role": "writer"}),
        )
        .ok();
        match c.call("rdy_import", import_args(run, &args)) {
            Reply::Ok(v) => Ok(v),
            Reply::ToolError(v) => Err(v),
            Reply::RpcError(v) => panic!("a protocol error: {v}"),
        }
    }

    /// `rdy_import` over HTTP, with the claim `ACTOR` in run `run`.
    fn http(&mut self, run: &str, args: Value) -> Result<Value, Value> {
        self.next += 1;
        // a chat client states the kungfu value of the ruleset it loaded; the test supplies the
        // fixture's, as the stdio client does, so the reply carries no kungfu warning
        let mut a = import_args(run, &args);
        rdy_qa::mcp::supply_kungfu("rdy_import", &mut a);
        let r = self.ep.post(
            &pairs(&legacy(LEGACY[1])),
            &call(self.next, "rdy_import", a, claim(ACTOR, run)),
        );
        let (is_error, v) = r.tool();
        if is_error { Err(v) } else { Ok(v) }
    }
}

/// The arguments of a Markdown `rdy_import` into `view`, with `dir` and the optional
/// `dry_run` and `expected_source` of `args`.
fn import_args(view: &str, args: &Value) -> Value {
    let mut a = json!({"dir": args["dir"], "view": view, "md": true});
    for k in ["dry_run", "expected_source"] {
        if !args[k].is_null() {
            a[k] = args[k].clone();
        }
    }
    a
}

fn keys(v: &Value) -> BTreeSet<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

fn triple(v: &Value) -> (String, String, String) {
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    (s("code"), s("message"), s("fix"))
}

fn paths_of(reply: &Value) -> Vec<&str> {
    reply["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect()
}

#[test]
fn m4bc6_http_and_stdio_keep_the_same_import_identity_and_result() {
    let s = on();
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("md");
    source(&src);
    let dir = src.to_str().unwrap();
    let source_before = tree_digest(&src);
    let mut tr = Transports::new(&s);

    // the preview: one reply on the command line, the stdio proxy and HTTP; no view anywhere
    let cli = tr.cli("cli1", dir, &["--dry-run"]).unwrap();
    let chat = tr
        .stdio("stdio1", json!({"dir": dir, "dry_run": true}))
        .unwrap();
    let http = tr
        .http("http1", json!({"dir": dir, "dry_run": true}))
        .unwrap();
    assert_eq!(cli, chat, "the CLI and stdio previews");
    assert_eq!(cli, http, "the CLI and HTTP previews");
    assert_eq!(
        keys(&cli),
        ["blocks", "dry_run", "files", "source_digest", "unmapped"]
            .map(String::from)
            .into_iter()
            .collect()
    );
    let digest = cli["source_digest"].as_str().unwrap().to_string();
    assert_eq!(digest.len(), 64);
    assert_eq!(
        paths_of(&cli),
        ["a.md", "sub/b.md"],
        "the ignored draft is left out"
    );
    assert_eq!(
        cli["unmapped"],
        json!([
            {"path": "a.md", "line": 5, "kind": "html"},
            {"path": "pics/one.png", "line": 0, "kind": "non_markdown_file"},
        ])
    );
    let (ok, st) = s.json(&["--json", "status"]);
    assert!(ok);
    assert!(
        st["views"].as_array().unwrap().is_empty(),
        "a preview claims no view on any transport: {st}"
    );

    // the import with the preview's digest, one view for each transport
    let by_cli = tr
        .cli("cli2", dir, &["--expected-source", &digest])
        .unwrap();
    let by_stdio = tr
        .stdio("stdio2", json!({"dir": dir, "expected_source": digest}))
        .unwrap();
    let by_http = tr
        .http("http2", json!({"dir": dir, "expected_source": digest}))
        .unwrap();
    for (what, v) in [("stdio", &by_stdio), ("http", &by_http)] {
        assert_eq!(keys(v), keys(&by_cli), "{what}: the same reply keys");
        for k in ["source_digest", "files", "unmapped"] {
            assert_eq!(v[k], by_cli[k], "{what}: {k}");
        }
        assert_eq!(
            v["created"].as_object().map(|o| o.len()),
            by_cli["created"].as_object().map(|o| o.len()),
            "{what}: the blocks created"
        );
        assert_eq!(v["changes"], by_cli["changes"], "{what}: the changes");
        assert_eq!(
            v["coverage"]["score"], by_cli["coverage"]["score"],
            "{what}"
        );
        assert_eq!(
            v["coverage"]["dimensions"], by_cli["coverage"]["dimensions"],
            "{what}"
        );
        assert_eq!(
            v["onboarding"], by_cli["onboarding"],
            "{what}: the next phase"
        );
        assert!(v.get("coverage_error").is_none(), "{what}: {v}");
    }
    assert_eq!(by_cli["source_digest"], json!(digest));
    assert_eq!(
        by_cli["created"].as_object().map(|o| o.len()),
        cli["blocks"].as_u64().map(|n| n as usize),
        "the preview counted the blocks the import created"
    );

    // a source that changed after its preview refuses the same way on every transport
    std::fs::write(src.join("sub/b.md"), "# Beta\n\nChanged.\n").unwrap();
    let changed = [
        tr.cli("cli3", dir, &["--expected-source", &digest])
            .unwrap_err(),
        tr.stdio("stdio3", json!({"dir": dir, "expected_source": digest}))
            .unwrap_err(),
        tr.http("http3", json!({"dir": dir, "expected_source": digest}))
            .unwrap_err(),
    ];
    for v in &changed {
        assert_eq!(
            triple(v),
            (CHANGED.0.into(), CHANGED.1.into(), CHANGED.2.into()),
            "{v}"
        );
    }
    std::fs::write(src.join("sub/b.md"), BETA).unwrap();
    assert_eq!(
        tree_digest(&src),
        source_before,
        "no transport changed the source"
    );
    drop(tr);

    // identity: each view is the actor's own, and every imported block names the same
    // importing session as author and importer, whichever transport carried the import
    s.stop();
    wait_endpoint_gone(&s.run_dir());
    let r = Rdy::open(s.dir.path()).unwrap();
    let st = r.read();
    for v in ["cli1", "stdio1", "http1", "cli3", "stdio3", "http3"] {
        assert!(
            st.view_head(v).is_none(),
            "{v}: a preview or a refusal claims no view"
        );
    }
    let mut shapes: Vec<Vec<(String, String, String)>> = vec![];
    for run in ["cli2", "stdio2", "http2"] {
        let head = st.view_head(run).unwrap_or_else(|| panic!("view {run}"));
        let owner = head.owner.clone().expect("an owner");
        assert_eq!(
            (owner.actor.as_str(), owner.run.as_deref()),
            (ACTOR, Some(run)),
            "{run}: the owner"
        );
        let scope = Scope::View(run.into());
        let mut docs = vec![];
        for key in ["a", "sub.b"] {
            let id = st
                .resolve_id(&scope, key)
                .unwrap_or_else(|e| panic!("{run}: doc {key}: {e}"));
            let block = st.block(&scope, &id).unwrap();
            let h = st.history(&scope, &id, 5).unwrap();
            let created = h.entries.last().expect("a creating change");
            assert_eq!(
                (created.author.actor.as_str(), created.author.run.as_deref()),
                (ACTOR, Some(run)),
                "{run}/{key}: the author is the importing session"
            );
            assert_eq!(
                created
                    .imported_by
                    .as_ref()
                    .map(|i| (i.actor.as_str(), i.run.as_deref())),
                Some((ACTOR, Some(run))),
                "{run}/{key}: the importer"
            );
            docs.push((
                key.to_string(),
                block.field_str("title").unwrap_or_default().to_string(),
                block.field_str("doc_type").unwrap_or_default().to_string(),
            ));
        }
        shapes.push(docs);
    }
    assert_eq!(shapes[0], shapes[1], "stdio built the CLI's docs");
    assert_eq!(shapes[0], shapes[2], "HTTP built the CLI's docs");
    drop(st);
    check(&r, &Scope::Canon).unwrap();
    for v in ["cli2", "stdio2", "http2"] {
        check(&r, &Scope::View(v.into())).unwrap();
    }
    r.fsck().unwrap();
}

/// The fixture home of a served project's commands, and its config root.
fn homes(s: &Served) -> (PathBuf, PathBuf) {
    let env = s.env_pairs();
    let get = |name: &str| {
        env.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| PathBuf::from(v))
    };
    let home = get("HOME")
        .or_else(|| get("USERPROFILE"))
        .expect("the fixture sets a home");
    let cfg = get("XDG_CONFIG_HOME")
        .or_else(|| get("APPDATA"))
        .expect("the fixture sets a config home");
    (home, cfg)
}

#[test]
fn m4bc6_import_never_reads_or_changes_the_operator_home() {
    let s = on();
    let (home, cfg) = homes(&s);
    // every variable that places a home or config is set, below the fixture where it is a
    // path, and never to the test process's own (the operator's) value
    for (name, value) in s.env_pairs() {
        if matches!(
            name.as_str(),
            "HOME" | "USERPROFILE" | "APPDATA" | "XDG_CONFIG_HOME"
        ) {
            assert!(
                Path::new(&value).starts_with(&cfg) || Path::new(&value).starts_with(&home),
                "{name}={value} is not below the fixture"
            );
        }
        if matches!(
            name.as_str(),
            "HOME" | "USERPROFILE" | "APPDATA" | "XDG_CONFIG_HOME" | "HOMEPATH"
        ) {
            assert_ne!(
                std::env::var(&name).ok().as_deref(),
                Some(value.as_str()),
                "{name} is the test process's own value"
            );
        }
    }
    // bait in the fixture home: an ignore file that would drop every Markdown file, a
    // Markdown file a source link could reach, and a user config that must stay as it is
    std::fs::write(home.join(".rdyignore"), "*.md\n").unwrap();
    std::fs::create_dir_all(home.join("notes")).unwrap();
    std::fs::write(home.join("notes/leak.md"), "# Leak\n").unwrap();
    let user_config = s.user_config();
    let config_bytes = std::fs::read(&user_config).unwrap();
    let home_before = tree_digest(&home);
    let cfg_before = tree_digest(&cfg);

    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("md");
    source(&src);
    // links out of the source stay text; they never make the importer read the target
    std::fs::write(
        src.join("out.md"),
        format!(
            "# Out\n\n[up](../../notes/leak.md) and [home]({}).\n",
            home.join("notes/leak.md")
                .to_string_lossy()
                .replace('\\', "/")
        ),
    )
    .unwrap();
    let dir = src.to_str().unwrap();
    let source_before = tree_digest(&src);

    let mut tr = Transports::new(&s);
    let pv = tr.cli("h1", dir, &["--dry-run"]).unwrap();
    assert_eq!(
        paths_of(&pv),
        ["a.md", "out.md", "sub/b.md"],
        "the home's .rdyignore is not read: only the source's own applies"
    );
    assert!(
        pv["unmapped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["path"] == json!("out.md") && r["kind"] == json!("link")),
        "an outside link is a reported row, not a read: {pv}"
    );
    let digest = pv["source_digest"].as_str().unwrap().to_string();
    let by_cli = tr.cli("h2", dir, &["--expected-source", &digest]).unwrap();
    let by_stdio = tr
        .stdio("h3", json!({"dir": dir, "expected_source": digest}))
        .unwrap();
    let by_http = tr
        .http("h4", json!({"dir": dir, "expected_source": digest}))
        .unwrap();
    for v in [&by_cli, &by_stdio, &by_http] {
        assert_eq!(v["source_digest"], json!(digest));
        assert_eq!(v["files"], pv["files"]);
    }
    drop(tr);

    // nothing of the fixture home or config was changed, no meta project was made, and the
    // source is as it was
    assert_eq!(tree_digest(&src), source_before, "the source");
    assert_eq!(tree_digest(&home), home_before, "the fixture home");
    assert_eq!(tree_digest(&cfg), cfg_before, "the fixture config root");
    assert_eq!(std::fs::read(&user_config).unwrap(), config_bytes);
    assert!(
        !home.join(".rdy/meta").exists(),
        "no import made or started a meta project under {}",
        home.display()
    );
    s.stop();
    wait_endpoint_gone(&s.run_dir());
    let r = Rdy::open(s.dir.path()).unwrap();
    for v in ["h2", "h3", "h4"] {
        assert!(r.read().view_head(v).is_some(), "{v}");
    }
    assert!(
        r.read().view_head("h1").is_none(),
        "the preview claims no view"
    );
}

/// The number of changes `view` holds in `rdy status`, `None` while it has no view.
fn view_changes(s: &Served, view: &str) -> Option<u64> {
    let (ok, st) = s.json(&["--json", "status"]);
    assert!(ok, "{st}");
    st["views"]
        .as_array()?
        .iter()
        .find(|v| v["name"] == json!(view))
        .and_then(|v| v["changes"].as_u64())
}

fn hello_for(s: &Served) -> Hello {
    Hello {
        rdy: PROTO_VERSION,
        version: env!("CARGO_PKG_VERSION").into(),
        build: rdy_daemon::BUILD.to_string(),
        proto: Proto::Ctl,
        cwd: s.dir.path().into(),
        project: RunDir::of_project(s.dir.path())
            .project_id()
            .expect("the project's .rdy"),
    }
}

#[test]
fn m4bc6_a_lost_import_reply_is_inspected_and_never_retried() {
    let s = Served::new();
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("md");
    source(&src);
    let dir = src.to_str().unwrap().to_string();
    let source_before = tree_digest(&src);
    let (ok, st) = s.json(&["--json", "status"]);
    assert!(ok, "the first status starts the daemon: {st}");
    let first = s.daemon_pid().expect("the daemon runs");

    // a client sends the import and never reads its reply: the connection ends once the
    // import is in the view. That is a reply lost after the seal.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let stream = rt.block_on(async {
        let hello = hello_for(&s);
        let mut c = match connect(&RunDir::of_project(s.dir.path()), &hello).await {
            Attempt::Ready(c) => c,
            other => panic!("no daemon of this build: {other:?}"),
        };
        c.ctl("session", json!({"actor": ACTOR, "run": "lost1"}))
            .await
            .unwrap();
        let mut stream = c.into_stream();
        let line = format!(
            "{}\n",
            json!({"id": 2, "op": "import",
                "args": {"dir": dir, "view": "lost1", "md": true}})
        );
        use tokio::io::AsyncWriteExt;
        stream.write_all(line.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
        stream
    });
    // wait on the signal that matters: the import is in the view
    let end = Instant::now() + T;
    while view_changes(&s, "lost1").is_none_or(|n| n == 0) {
        assert!(Instant::now() < end, "the import never reached the view");
        std::thread::sleep(Duration::from_millis(10));
    }
    // the daemon dies and the connection is dropped, the reply unread
    s.kill9();
    drop(stream);
    drop(rt);
    assert_eq!(
        s.daemon_pid(),
        None,
        "nothing reconnected, so nothing sent the import again"
    );

    // inspect the view: the import is there once, whole, and the source is as it was
    let (ok, st) = s.json(&["--json", "status"]);
    assert!(ok, "{st}");
    assert_ne!(
        s.daemon_pid(),
        Some(first),
        "a successor answered the inspection"
    );
    assert_eq!(
        view_changes(&s, "lost1"),
        Some(1),
        "one import, one change: {st}"
    );
    assert_eq!(
        st["open_report"]["index_rebuilt"],
        json!(false),
        "a kill is catch-up, not reindex: {st}"
    );
    assert_eq!(tree_digest(&src), source_before);
    // an explicit second try is refused by what is already there, and adds nothing
    let (ok, v) = s.json_as(
        ACTOR,
        Some("lost1"),
        &["--json", "import", &dir, "--view", "lost1", "--md"],
    );
    assert!(!ok, "{v}");
    assert_eq!(v["code"], json!("E_PATH_CLASH"), "{v}");
    assert_eq!(view_changes(&s, "lost1"), Some(1), "still one import");

    // a client whose connection is gone is told the outcome is unknown, its next call goes
    // nowhere, and it starts no successor of its own
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let hello = hello_for(&s);
        let Attempt::Ready(mut c) = connect(&RunDir::of_project(s.dir.path()), &hello).await else {
            panic!("the successor answers");
        };
        c.ctl("session", json!({"actor": ACTOR, "run": "lost2"}))
            .await
            .unwrap();
        s.kill9();
        let args = json!({"dir": dir, "view": "lost2", "md": true});
        let e = c.ctl("import", args.clone()).await.unwrap_err();
        assert_eq!(e.code, E_DAEMON_GONE);
        assert!(e.fix.contains("may or may not have committed"), "{e}");
        assert!(e.fix.contains("not retried"), "{e}");
        let e = c.ctl("import", args).await.unwrap_err();
        assert_eq!(
            e.code, E_DAEMON_GONE,
            "every later call on a gone connection"
        );
    });
    assert_eq!(
        s.daemon_pid(),
        None,
        "the client did not start a successor itself"
    );
    let (ok, st) = s.json(&["--json", "status"]);
    assert!(ok, "{st}");
    assert_eq!(
        view_changes(&s, "lost2"),
        None,
        "the import that was never sent is not there"
    );
    assert_eq!(view_changes(&s, "lost1"), Some(1));
    s.stop();
    wait_endpoint_gone(&s.run_dir());
    let r = Rdy::open(s.dir.path()).unwrap();
    r.fsck().unwrap();
    check(&r, &Scope::View("lost1".into())).unwrap();
}
