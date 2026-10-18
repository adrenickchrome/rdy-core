//! ec11: `rdy_export` prints the docs for a person (D-EVRNM, D-KQ4CA): one PDF file, one HTML
//! file or a Markdown tree. Any session may call it (O6); it runs in the daemon (O35); it never
//! writes a format-3 tree (O12).
use rdy_engine::Rdy;
use rdy_engine::engine::Local;
use rdy_mcp::params::parse_params;
use rdy_mcp::tools::{ExportFormat, ExportParams, McpState, rdy_export, rdy_session};
use rdy_qa::daemon::{Served, short_tempdir};
use rdy_qa::golden::{FIXTURE_DIR, approver, land};
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

/// A served project holding the golden corpus, landed and tagged `v1.0.0`.
fn golden_served() -> Served {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    r.import(&approver(), "import", Path::new(FIXTURE_DIR))
        .unwrap();
    land(&mut r, "import").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    r.close().unwrap();
    s
}

fn agent(s: &Served) -> McpClient {
    let mut c = McpClient::start_proxy(s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer"}),
    )
    .ok();
    c
}

/// The code, message and fix of a tool error.
fn refused(r: Reply) -> (String, String, String) {
    let Reply::ToolError(e) = r else {
        panic!("expected a tool error, got {r:?}")
    };
    let s = |k: &str| e[k].as_str().unwrap_or_default().to_string();
    (s("code"), s("message"), s("fix"))
}

/// Every path under `dir`, relative to it.
fn files_under(dir: &Path) -> Vec<String> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                stack.push(p.clone());
            }
            out.push(p.strip_prefix(dir).unwrap().to_string_lossy().into_owned());
        }
    }
    out
}

#[test]
fn ec11_rdy_export_writes_each_format() {
    let s = golden_served();
    let mut c = agent(&s);
    let dir = s.dir.path();

    // one PDF file, relative to the project directory
    let v = c
        .call("rdy_export", json!({"out": "p.pdf", "format": "pdf"}))
        .ok();
    let pdf = std::fs::read(dir.join("p.pdf")).unwrap();
    assert!(pdf.starts_with(b"%PDF-"), "a PDF file");
    assert_eq!(v["format"], "pdf", "{v}");
    assert_eq!(v["bytes"], pdf.len() as u64, "{v}");
    assert!(v["pages"].as_u64().unwrap() > 0, "{v}");
    assert!(v["docs"].as_u64().unwrap() > 0, "{v}");
    assert!(v["warnings"].is_array(), "{v}");

    // one HTML file
    let v = c
        .call("rdy_export", json!({"out": "p.html", "format": "html"}))
        .ok();
    let html = std::fs::read(dir.join("p.html")).unwrap();
    assert!(html.starts_with(b"<!doctype html>"), "an HTML file");
    assert_eq!(v["format"], "html", "{v}");
    assert_eq!(v["bytes"], html.len() as u64, "{v}");
    assert_eq!(v["pages"], Value::Null, "{v}");

    // a Markdown tree, plain and for MkDocs
    let v = c
        .call("rdy_export", json!({"out": "md", "format": "md"}))
        .ok();
    assert_eq!(v["format"], "md", "{v}");
    assert!(dir.join("md/index.md").is_file(), "{v}");
    assert!(!dir.join("md/mkdocs.yml").exists());
    let v = c
        .call(
            "rdy_export",
            json!({"out": "mk", "format": "md", "layout": "mkdocs"}),
        )
        .ok();
    assert!(dir.join("mk/mkdocs.yml").is_file(), "{v}");
    assert!(dir.join("mk/docs/index.md").is_file(), "{v}");

    // an absolute path is used as given, and a scope prints that version
    let scratch = short_tempdir();
    let abs = scratch.path().join("v1.html");
    let v = c
        .call(
            "rdy_export",
            json!({"out": abs.to_str().unwrap(), "format": "html", "scope": "tag:v1.0.0"}),
        )
        .ok();
    assert!(abs.is_file(), "{v}");

    // it never overwrites
    let (code, message, _) =
        refused(c.call("rdy_export", json!({"out": "p.html", "format": "html"})));
    assert_eq!(code, "E_ALREADY_EXISTS", "{message}");
    assert_eq!(std::fs::read(dir.join("p.html")).unwrap(), html);

    // no export writes a format-3 tree (O12)
    for root in [dir, scratch.path()] {
        let f3: Vec<String> = files_under(root)
            .into_iter()
            .filter(|f| f.ends_with("rdy-export.yaml"))
            .collect();
        assert!(f3.is_empty(), "{f3:?}");
    }
}

#[test]
fn ec11_rdy_export_refuses_inflight_and_a_layout_without_md() {
    let s = golden_served();
    let mut c = agent(&s);
    let dir = s.dir.path();

    let (code, message, fix) = refused(c.call(
        "rdy_export",
        json!({"out": "i.html", "format": "html", "scope": "inflight"}),
    ));
    assert_eq!(code, "E_BAD_INPUT");
    assert_eq!(
        message,
        "inflight is an analysis scope: export (render) canon, a view or a version"
    );
    assert_eq!(
        fix,
        "export or render canon, a view (view:<name>) or a version (tag:<version>)"
    );
    assert!(!dir.join("i.html").exists());

    for (out, format) in [("l.pdf", "pdf"), ("l.html", "html")] {
        let (code, message, fix) = refused(c.call(
            "rdy_export",
            json!({"out": out, "format": format, "layout": "mkdocs"}),
        ));
        assert_eq!(code, "E_BAD_INPUT", "{format}");
        assert_eq!(message, "--layout mkdocs applies to the md format only");
        assert_eq!(fix, "drop --layout, or export with --format md");
        assert!(!dir.join(out).exists(), "{format}");
    }
}

#[test]
fn ec11_rdy_export_params_are_strict() {
    let bad = |args: Value| {
        parse_params::<ExportParams>("rdy_export", args.as_object().unwrap().clone())
            .expect_err("a refusal")
    };
    let e = bad(json!({"out": "x", "format": "md", "formats": "md"}));
    assert_eq!(e.code, "E_BAD_INPUT");
    assert!(e.message.contains("format"), "{}", e.message);
    for args in [
        json!({"out": "x"}),
        json!({"format": "md"}),
        json!({"out": "x", "format": "docx"}),
        json!({"out": "x", "format": "MD"}),
        json!({"out": "x", "format": "md", "layout": "madr"}),
        json!({"out": "x", "format": "md", "md": true}),
    ] {
        assert_eq!(bad(args.clone()).code, "E_BAD_INPUT", "{args}");
    }
    let ok = parse_params::<ExportParams>(
        "rdy_export",
        json!({"out": "x", "format": "md", "layout": "mkdocs", "scope": "canon"})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert!(ok.is_ok(), "{ok:?}");

    // over the wire: a session is required, and an unknown key writes nothing
    let s = Served::new();
    let mut c = McpClient::start_proxy(&s);
    let (code, ..) = refused(c.call("rdy_export", json!({"out": "x.html", "format": "html"})));
    assert_eq!(code, "E_NO_SESSION");
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer"}),
    )
    .ok();
    let (code, ..) = refused(c.call(
        "rdy_export",
        json!({"out": "x.html", "format": "html", "scopes": "canon"}),
    ));
    assert_eq!(code, "E_BAD_INPUT");
    assert!(!s.dir.path().join("x.html").exists());
}

#[test]
fn ec11_rdy_export_without_a_daemon_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let r = Rdy::init(d.path(), "rdy-standard").unwrap();
    let mut st = McpState::new(Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:a", "run": "a1", "role": "writer"})).unwrap(),
    )
    .unwrap();
    let e = rdy_export(
        &mut st,
        ExportParams {
            out: "p.pdf".into(),
            format: ExportFormat::Pdf,
            layout: None,
            scope: None,
            profile: None,
        },
    )
    .expect_err("no daemon, no export");
    assert_eq!(e.code, "E_BAD_INPUT");
    assert_eq!(
        e.message,
        "rdy_export runs in the daemon, and this session runs without one"
    );
    assert_eq!(
        e.fix,
        "drop --no-daemon, or run rdy export --out <path> --format <pdf|html|md>"
    );
    assert!(!d.path().join("p.pdf").exists());
}
