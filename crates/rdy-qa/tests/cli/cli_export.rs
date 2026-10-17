//! ec11: `rdy export` prints the docs, the CLI twin of `rdy_export` (D-EVRNM, D-KQ4CA, O1). No
//! CLI path writes a format-3 tree (O12).
use rdy_engine::Rdy;
use rdy_qa::daemon::{Served, short_tempdir};
use rdy_qa::golden::{FIXTURE_DIR, approver, land};
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::path::Path;

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

fn stdout(out: &std::process::Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Every file under `p` (or `p` itself, when it is a file) with its bytes, by its path relative
/// to `p`.
fn contents(p: &Path) -> Vec<(String, Vec<u8>)> {
    if p.is_file() {
        return vec![(String::new(), std::fs::read(p).unwrap())];
    }
    let mut out = vec![];
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let f = e.unwrap().path();
            if f.is_dir() {
                stack.push(f);
            } else {
                let rel = f.strip_prefix(p).unwrap().to_string_lossy().into_owned();
                out.push((rel, std::fs::read(&f).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// Every path under `dir`, relative to it; it reads no file, so a socket in the run directory
/// is listed like any other entry.
fn names(dir: &Path) -> Vec<String> {
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

/// The reply without `out`, which names the path each one wrote.
fn without_out(mut v: Value) -> Value {
    v.as_object_mut().unwrap().remove("out");
    v
}

#[test]
fn ec11_rdy_export_matches_rdy_export_over_mcp() {
    let s = golden_served();
    let scratch = short_tempdir();
    let mut c = McpClient::start_proxy(&s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer"}),
    )
    .ok();
    for (name, format, layout) in [
        ("p.pdf", "pdf", None),
        ("p.html", "html", None),
        ("md", "md", None),
        ("mk", "md", Some("mkdocs")),
    ] {
        let cli = scratch.path().join(format!("cli-{name}"));
        let mut args = vec![
            "--json",
            "export",
            "--out",
            cli.to_str().unwrap(),
            "--format",
            format,
        ];
        if let Some(l) = layout {
            args.extend(["--layout", l]);
        }
        let (ok, cv) = s.json(&args);
        assert!(ok, "{name}: {cv}");
        let tool = scratch.path().join(format!("mcp-{name}"));
        let mut p = json!({"out": tool.to_str().unwrap(), "format": format});
        if let Some(l) = layout {
            p["layout"] = json!(l);
        }
        let tv = c.call("rdy_export", p).ok();
        assert_eq!(without_out(cv), without_out(tv), "{name}: the same reply");
        let (a, b) = (contents(&cli), contents(&tool));
        assert!(!a.is_empty(), "{name}");
        assert!(a == b, "{name}: the CLI and the tool write the same bytes");
    }
}

#[test]
fn ec11_rdy_export_text_names_docs_format_and_bytes() {
    let s = golden_served();
    let scratch = short_tempdir();
    for (format, ext) in [("pdf", "pdf"), ("html", "html"), ("md", "md")] {
        let a = scratch.path().join(format!("a.{ext}"));
        let (ok, v) = s.json(&[
            "--json",
            "export",
            "--out",
            a.to_str().unwrap(),
            "--format",
            format,
        ]);
        assert!(ok, "{v}");
        let b = scratch.path().join(format!("b.{ext}"));
        let b_arg = b.to_str().unwrap();
        let text = stdout(&s.rdy(&["export", "--out", b_arg, "--format", format]));
        let mut want = format!(
            "exported {} docs in {} folders as {format} to {b_arg} ({} bytes)",
            v["docs"], v["folders"], v["bytes"]
        );
        if format == "pdf" {
            assert!(v["pages"].as_u64().unwrap() > 0, "{v}");
            want.push_str(&format!(", {} pages", v["pages"]));
        }
        for w in v["warnings"].as_array().unwrap() {
            want.push_str(&format!("\nwarning: {}", w.as_str().unwrap()));
        }
        assert_eq!(text.trim_end(), want, "{format}");
    }
}

#[test]
fn ec11_no_cli_path_writes_a_format_3_tree() {
    let s = golden_served();
    let dir = s.dir.path();
    let out = s.rdy(&["export", "--out", "x"]);
    assert!(!out.status.success(), "--format is required");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--format"), "{err}");
    assert!(!dir.join("x").exists());

    let out = s.rdy(&["export", "--out", "y", "--format", "yaml"]);
    assert!(
        !out.status.success(),
        "a format outside the list is refused"
    );
    assert!(!dir.join("y").exists());

    let out = s.rdy(&[
        "export", "--out", "z.pdf", "--format", "pdf", "--layout", "mkdocs",
    ]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--layout mkdocs applies to the md format only"),
        "{err}"
    );
    assert!(!dir.join("z.pdf").exists());

    for (o, f) in [("t.pdf", "pdf"), ("t.html", "html"), ("t-md", "md")] {
        stdout(&s.rdy(&["export", "--out", o, "--format", f]));
    }
    stdout(&s.rdy(&[
        "export", "--out", "t-mk", "--format", "md", "--layout", "mkdocs",
    ]));
    let f3: Vec<String> = names(dir)
        .into_iter()
        .filter(|p| p.ends_with("rdy-export.yaml"))
        .collect();
    assert!(f3.is_empty(), "no format-3 manifest anywhere: {f3:?}");
}
