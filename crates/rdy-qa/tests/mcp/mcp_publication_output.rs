//! pub10 (D-5CVWB, D-0VCQ5, D-KQ4CA): the chat path of the local outputs. `rdy_render` and
//! `rdy_export` take a strict `profile`; a scope defaults to canon whatever the session's view is;
//! the daemon writes the files through the engine's output seam, and a session with no daemon is
//! refused with the same words as `rdy_export`.
use rdy_engine::engine::{Engine, Local, RenderRequest};
use rdy_engine::{E_BAD_INPUT, Rdy};
use rdy_mcp::params::parse_params;
use rdy_mcp::tools::{McpState, RenderParams, rdy_render, rdy_session};
use rdy_qa::daemon::{Served, short_tempdir};
use rdy_qa::fixtures::{approver, land, write};
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

/// A folder `area` with the doc `chosen`, a doc `outdoc` outside it, and the profiles `orgp`
/// (org) and `openp` (public) on `area`; `area` refers to nothing outside, so both succeed. A tag
/// `v1.0.0`, then an unlanded draft view `mine` that adds the doc `draftdoc` the session reads.
fn seed(dir: &Path) {
    let mut r = Rdy::open(dir).unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$area", "fields": {"name": "area", "title": "Area"}},
            {"op": "create", "type": "doc", "id": "$chosen", "parent": "$area", "fields": {"name": "chosen", "title": "Chosen page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$chosen", "body": "Chosen prose words."},
            {"op": "create", "type": "doc", "id": "$out", "fields": {"name": "outdoc", "title": "OUTCANARY doc", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$out", "body": "OUTCANARY words."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    let id = |k: &str| w.created[k].clone();
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "publication_profile", "parent": id("$admin"),
             "fields": {"key": "orgp", "title": "Title of orgp", "audience": "org", "select": [id("$area")]}},
            {"op": "create", "type": "publication_profile", "parent": id("$admin"),
             "fields": {"key": "openp", "title": "Title of openp", "audience": "public", "select": [id("$area")]}}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    write(
        &mut r,
        "mine",
        json!([
            {"op": "create", "type": "doc", "parent": id("$area"), "fields": {"name": "draftdoc", "title": "DRAFTDOC title", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    r.close().unwrap();
}

fn files_of(p: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    if p.is_file() {
        out.insert(String::new(), std::fs::read(p).unwrap());
        return out;
    }
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let f = e.unwrap().path();
            if f.is_dir() {
                stack.push(f);
            } else {
                let rel = f
                    .strip_prefix(p)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&f).unwrap());
            }
        }
    }
    out
}

/// A session whose own view is `mine`, which holds an unlanded draft doc.
fn session(s: &Served) -> McpClient {
    let mut c = McpClient::start_proxy(s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:p", "run": "p", "role": "writer", "view": "mine"}),
    )
    .ok();
    c
}

fn refused(r: Reply) -> (String, String, String) {
    let Reply::ToolError(e) = r else {
        panic!("expected a tool error, got {r:?}")
    };
    let s = |k: &str| e[k].as_str().unwrap_or_default().to_string();
    (s("code"), s("message"), s("fix"))
}

#[test]
fn pub10_output_parameters_are_strict_and_scope_defaults_to_canon() {
    let s = Served::new();
    seed(s.dir.path());
    let mut c = session(&s);
    let scratch = short_tempdir();
    let out = |name: &str| scratch.path().join(name);
    let p = |path: &Path| path.to_str().unwrap().to_string();

    // the parameters: `profile` is a known key; near keys and other keys are refused with a hint,
    // and nothing is written
    for tool in ["rdy_render", "rdy_export"] {
        for (key, near) in [
            ("profil", "profile"),
            ("Profile", "profile"),
            ("profiles", "profile"),
        ] {
            let target = out(&format!("{tool}-{key}"));
            let mut args = json!({"out": p(&target), key: "orgp"});
            if tool == "rdy_export" {
                args["format"] = json!("html");
            }
            let (code, message, fix) = refused(c.call(tool, args));
            assert_eq!(code, "E_BAD_INPUT", "{tool} {key}");
            assert_eq!(message, format!("{tool}: unknown parameter \"{key}\""));
            assert!(
                fix.starts_with(&format!("did you mean \"{near}\"?")),
                "{tool} {key}: {fix}"
            );
            assert!(!target.exists(), "{tool} {key}");
        }
    }
    // `rdy_render` takes `out`, `scope` and `profile` and nothing else: no format, no layout
    for key in ["format", "layout", "view", "dir"] {
        let target = out(&format!("render-{key}"));
        let (code, ..) = refused(c.call(
            "rdy_render",
            json!({"out": p(&target), key: "x", "profile": "orgp"}),
        ));
        assert_eq!(code, "E_BAD_INPUT", "{key}");
        assert!(!target.exists(), "{key}");
    }
    // `out` is required, and a bad type is refused
    let (code, ..) = refused(c.call("rdy_render", json!({"profile": "orgp"})));
    assert_eq!(code, "E_BAD_INPUT");
    let (code, ..) = refused(c.call("rdy_render", json!({"out": p(&out("x")), "profile": 7})));
    assert_eq!(code, "E_BAD_INPUT");
    // the live site tool, the dump and the source reads take no profile
    for (tool, args) in [
        ("rdy_site", json!({"profile": "orgp"})),
        (
            "rdy_dump",
            json!({"out": p(&out("dump")), "profile": "orgp"}),
        ),
        (
            "rdy_clone",
            json!({"dir": p(&out("clone")), "profile": "orgp"}),
        ),
        ("rdy_read", json!({"id": "x", "profile": "orgp"})),
    ] {
        let (code, message, _) = refused(c.call(tool, args));
        assert_eq!(code, "E_BAD_INPUT", "{tool}");
        assert_eq!(message, format!("{tool}: unknown parameter \"profile\""));
    }
    assert!(!out("dump").exists() && !out("clone").exists());

    // layout stays md-only with a profile, in either order of the keys
    for format in ["pdf", "html"] {
        let target = out(&format!("layout-{format}"));
        let (code, message, fix) = refused(c.call(
            "rdy_export",
            json!({"out": p(&target), "format": format, "layout": "mkdocs", "profile": "orgp"}),
        ));
        assert_eq!(code, "E_BAD_INPUT");
        assert_eq!(message, "--layout mkdocs applies to the md format only");
        assert_eq!(fix, "drop --layout, or export with --format md");
        assert!(!target.exists());
    }

    // the serde types accept every documented key
    let ok = parse_params::<RenderParams>(
        "rdy_render",
        json!({"out": "x", "scope": "canon", "profile": "orgp"})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert!(ok.is_ok(), "{ok:?}");

    // a scope defaults to canon, whatever the session's own view reads: the draft doc of the view
    // `mine` is in no output, with a profile or without
    let (a, b) = (out("default-scope"), out("canon-scope"));
    c.call("rdy_render", json!({"out": p(&a), "profile": "orgp"}))
        .ok();
    c.call(
        "rdy_render",
        json!({"out": p(&b), "profile": "orgp", "scope": "canon"}),
    )
    .ok();
    assert!(files_of(&a) == files_of(&b), "the default scope is canon");
    for (path, bytes) in files_of(&a) {
        assert!(
            !String::from_utf8_lossy(&bytes).contains("DRAFTDOC") && !path.contains("draftdoc"),
            "{path}: a session view is not the default scope"
        );
    }
    let (a, b) = (out("e-default.html"), out("e-canon.html"));
    c.call(
        "rdy_export",
        json!({"out": p(&a), "format": "html", "profile": "openp"}),
    )
    .ok();
    c.call(
        "rdy_export",
        json!({"out": p(&b), "format": "html", "profile": "openp", "scope": "canon"}),
    )
    .ok();
    assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
    assert!(!String::from_utf8_lossy(&std::fs::read(&a).unwrap()).contains("DRAFTDOC"));
    // the session view is not a scope a profile takes, even by name
    for scope in ["mine", "view:mine"] {
        let target = out(&format!("view-{}", scope.replace(':', "-")));
        let (code, ..) = refused(c.call(
            "rdy_render",
            json!({"out": p(&target), "profile": "orgp", "scope": scope}),
        ));
        assert_eq!(code, "E_BAD_INPUT", "{scope}");
        assert!(!target.exists());
    }
    // without a profile, the session's view still reads as it did
    let own = out("own-view");
    c.call("rdy_render", json!({"out": p(&own), "scope": "mine"}))
        .ok();
    assert!(
        files_of(&own)
            .values()
            .any(|b| String::from_utf8_lossy(b).contains("DRAFTDOC title")),
        "{:?}",
        files_of(&own).keys()
    );

    // the tool list: the exact descriptions, and `profile` documented on both tools
    let tools = c.tools();
    let find = |name: &str| -> &Value {
        tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("no tool {name}"))
    };
    let render = find("rdy_render")["description"].as_str().unwrap();
    assert!(
        render.starts_with("Build a local static docs site at out."),
        "{render}"
    );
    assert!(
        render.ends_with("Read and write through the tools yourself, not the rendered site."),
        "{render}"
    );
    assert_eq!(
        render,
        "Build a local static docs site at out. Pass profile to select whole docs and folders. A profile uses canon, a tag, or a landing. This creates local files; it does not publish them or control who can read them. Relay each entry of warnings to the person. Read and write through the tools yourself, not the rendered site."
    );
    let export = find("rdy_export")["description"].as_str().unwrap();
    assert!(export.contains("MkDocs and Backstage TechDocs"), "{export}");
    assert!(
        export.ends_with("Read and write through the tools yourself, not the export."),
        "{export}"
    );
    assert_eq!(
        export,
        "Print the docs for a person to read: one PDF file, one HTML file, or a Markdown tree (layout mkdocs for MkDocs and Backstage TechDocs). Pass profile to use its selected view and reference check. This creates local files; it does not publish them or control who can read them. It never overwrites. Relay each entry of warnings to the person. Read and write through the tools yourself, not the export."
    );
    let help = "A publication profile id or key in the selected scope, or implicit:org for every doc. Selects whole docs and folders for local output.";
    for tool in ["rdy_render", "rdy_export"] {
        let schema = &find(tool)["inputSchema"];
        assert_eq!(
            schema["properties"]["profile"]["description"], help,
            "{tool}: {schema}"
        );
        assert!(
            !schema["required"]
                .as_array()
                .is_some_and(|r| r.iter().any(|k| k == "profile")),
            "{tool}: profile is optional"
        );
    }
    // the live site tool still takes a scope alone
    let site = &find("rdy_site")["inputSchema"]["properties"];
    assert!(site.get("profile").is_none(), "{site}");
}

#[test]
fn pub10_static_chat_uses_the_daemon_output_seam() {
    // the engine's default refuses, with the words and the fix of the other output seam
    let d = tempfile::tempdir().unwrap();
    let r = Rdy::init(d.path(), "rdy-standard").unwrap();
    let local = Local::new(r);
    let req = RenderRequest {
        out: d.path().join("site"),
        scope: None,
        profile: Some("orgp".into()),
        view: None,
    };
    let e = local.render(req).expect_err("no daemon, no render");
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(
        e.message,
        "rdy_render runs in the daemon, and this session runs without one"
    );
    assert_eq!(e.fix, "drop --no-daemon, or run rdy render --out <path>");
    assert!(!d.path().join("site").exists());
    drop(local);

    // the tool, over a session with no daemon, refuses the same way and writes nothing
    let d = tempfile::tempdir().unwrap();
    let r = Rdy::init(d.path(), "rdy-standard").unwrap();
    let mut st = McpState::new(Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:a", "run": "a1", "role": "writer"})).unwrap(),
    )
    .unwrap();
    let e = rdy_render(
        &mut st,
        RenderParams {
            out: "site".into(),
            scope: None,
            profile: None,
        },
    )
    .expect_err("no daemon, no render");
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(
        e.message,
        "rdy_render runs in the daemon, and this session runs without one"
    );
    assert!(!d.path().join("site").exists());

    // the daemon writes: a relative `out` is under the project directory, and the reply is the
    // render summary
    let s = Served::new();
    seed(s.dir.path());
    let mut c = session(&s);
    let v = c
        .call("rdy_render", json!({"out": "site-rel", "profile": "orgp"}))
        .ok();
    assert!(v["pages"].as_u64().unwrap() > 0, "{v}");
    assert!(s.dir.path().join("site-rel/index.html").is_file());
    // a profile of the daemon's output is the engine value, key for key, of the CLI's
    let (ok, cv) = s.json(&[
        "--json",
        "render",
        "--profile",
        "orgp",
        "--out",
        s.dir.path().join("site-cli").to_str().unwrap(),
    ]);
    assert!(ok, "{cv}");
    assert_eq!(cv, v);
    assert!(files_of(&s.dir.path().join("site-rel")) == files_of(&s.dir.path().join("site-cli")));

    // no reverse edge: the tool crate and the engine keep no dependency on the renderer
    for krate in ["rdy-mcp", "rdy-engine"] {
        let manifest = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join(krate)
                .join("Cargo.toml"),
        )
        .unwrap();
        assert!(
            !manifest.contains("rdy-render"),
            "{krate} must not depend on rdy-render"
        );
    }
}
