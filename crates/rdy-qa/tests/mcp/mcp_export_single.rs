//! rfr3 (D-ESHXM, D-6PR3Y, D-H1FRY): `rdy_export` with `layout: "single"` and `assets` gives the
//! same result, the same refusals and the same bytes over stdio and over admitted MCP HTTP, as
//! the CLI does. Each server reads a person home of its own; no test reads the real one.
use rdy_engine::Rdy;
use rdy_model::BlockId;
use rdy_qa::daemon::{Hosted, Served, short_tempdir};
use rdy_qa::fixtures::{land, write};
use rdy_qa::mcp::{McpClient, Reply, supply_kungfu};
use rdy_qa::mcp_http::{Endpoint, LEGACY, call, claim, legacy, pairs};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

const ON: &str = "serve:\n  mcp_http: true\n";

/// The project of `cli_export_single`: doc `Guide` with a figure and refs into its collapsed
/// section, doc `Other`, and profile `onep` of the guide. It is written, then closed.
fn project(r: &mut Rdy) -> BTreeMap<String, BlockId> {
    let blob = r
        .store_mut()
        .put_blob(b"\x89PNG\r\n\x1a\nsingle image bytes")
        .unwrap();
    let w = write(
        r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$fold", "parent": "$g", "fields": {"title": "Folded", "collapsed": true}},
            {"op": "create", "type": "prose", "id": "$hid", "parent": "$fold", "body": "Folded words."},
            {"op": "create", "type": "section", "id": "$s", "parent": "$g", "fields": {"title": "Use"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "See {{$hid|the fold}}, {{$fold}} and {{$s}}."},
            {"op": "create", "type": "figure", "id": "$f", "parent": "$s", "fields": {"blob": blob.to_string(), "media_type": "image/png", "alt": "A picture"}},
            {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "other", "title": "Other", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$o", "body": "Other words."}
        ]),
    )
    .unwrap();
    land(r, "w1").unwrap();
    let ids: BTreeMap<String, BlockId> = w
        .created
        .iter()
        .map(|(k, v)| (k.clone(), BlockId::parse(v).unwrap()))
        .collect();
    write(
        r,
        "w2",
        json!([{"op": "create", "type": "publication_profile", "parent": ids["$o"].to_string(),
            "fields": {"key": "onep", "title": "One", "audience": "org", "select": [ids["$g"].to_string()]}}]),
    )
    .unwrap();
    land(r, "w2").unwrap();
    ids
}

/// Copies the tree at `from` into `to`.
fn copy(from: &Path, to: &Path) {
    for e in std::fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let dest = to.join(p.file_name().unwrap());
        if p.is_dir() {
            std::fs::create_dir(&dest).unwrap();
            copy(&p, &dest);
        } else {
            std::fs::copy(&p, &dest).unwrap();
        }
    }
}

/// Every entry under `dir`, by its path relative to it, with a file's bytes.
fn entries(dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let rel = p
                .strip_prefix(dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if p.is_dir() {
                stack.push(p.clone());
                out.insert(rel + "/", None);
            } else {
                out.insert(rel, std::fs::read(&p).ok());
            }
        }
    }
    out
}

/// One transport's answer: whether it is an error, and its body (an error's code, message and
/// fix, or the result without `out`, which names each transport's own path).
fn answer(err: bool, v: Value) -> (bool, Value) {
    if err {
        (
            true,
            json!({"code": v["code"], "message": v["message"], "fix": v["fix"]}),
        )
    } else {
        let mut v = v;
        v.as_object_mut().unwrap().remove("out");
        (false, v)
    }
}

fn stdio(c: &mut McpClient, args: Value) -> (bool, Value) {
    match c.call("rdy_export", args) {
        Reply::Ok(v) => answer(false, v),
        Reply::ToolError(e) => answer(true, e),
        Reply::RpcError(e) => panic!("a protocol error: {e}"),
    }
}

fn http(ep: &Endpoint, mut args: Value) -> (bool, Value) {
    supply_kungfu("rdy_export", &mut args);
    let (err, v) = ep
        .post(
            &pairs(&legacy(LEGACY[0])),
            &call(1, "rdy_export", args, claim("agent:h", "h1")),
        )
        .tool();
    answer(err, v)
}

#[test]
fn rfr3_mcp_single_matches_cli_on_both_transports() {
    // one project, copied before any server opens it, so both copies hold the same ids; the
    // stdio proxy and its daemon run as the served person, whose home is a scratch one
    let served = Served::new();
    let mut r = Rdy::open(served.dir.path()).unwrap();
    let ids = project(&mut r);
    r.close().unwrap();
    let two = tempfile::tempdir().unwrap();
    copy(served.dir.path(), two.path());
    std::fs::write(two.path().join(".rdy/local.yaml"), ON).unwrap();
    let mut c = McpClient::start_proxy(&served);
    c.call(
        "rdy_session",
        json!({"actor": "agent:s", "run": "s1", "role": "writer"}),
    )
    .ok();
    let hosted = Hosted::start_in(two);
    let url = hosted
        .run
        .as_ref()
        .expect("running")
        .shared()
        .site_info
        .get()
        .and_then(|i| i.url.clone().ok())
        .expect("the site listens");
    let token = std::fs::read_to_string(hosted.dir.path().join(".rdy/run/mcp-token")).unwrap();
    let ep = Endpoint::of(&url, token.trim_end());
    let (a, b) = (short_tempdir(), short_tempdir());
    let both = |c: &mut McpClient, args: &dyn Fn(&Path) -> Value| {
        // each transport names its own folder in a message; the folder is matched as JSON
        // writes it, so a Windows path's backslashes match too
        let named = |v: (bool, Value), d: &Path| {
            let dir = serde_json::to_string(d.to_str().unwrap()).unwrap();
            let t = v.1.to_string().replace(dir.trim_matches('"'), "<dir>");
            (v.0, serde_json::from_str::<Value>(&t).unwrap())
        };
        let x = named(stdio(c, args(a.path())), a.path());
        let y = named(http(&ep, args(b.path())), b.path());
        assert_eq!(x, y, "the transports agree");
        assert_eq!(entries(a.path()), entries(b.path()), "the same bytes");
        x
    };
    let at = |dir: &Path, name: &str| dir.join(name).to_str().unwrap().to_string();
    let warning = |t: &str| {
        format!(
            "warning: guide: {}: the ref to {} is not in this file; it is written as its label",
            ids["$p"], ids[t]
        )
    };

    // the result
    let (err, v) = both(
        &mut c,
        &|d| json!({"out": at(d, "README.md"), "format": "md", "layout": "single", "profile": "onep", "assets": "img"}),
    );
    assert!(!err, "{v}");
    let file = std::fs::read(a.path().join("README.md")).unwrap();
    assert_eq!(
        v,
        json!({"format": "md", "layout": "single", "docs": 1, "folders": 0, "assets": 1,
            "pages": null, "bytes": file.len(), "warnings": [warning("$hid"), warning("$fold")]})
    );

    // the refusals, with the CLI's words
    let refusal = |code: &str, message: &str, fix: &str| {
        (true, json!({"code": code, "message": message, "fix": fix}))
    };
    let cases: Vec<(Value, (bool, Value))> = vec![
        (
            json!({"format": "md", "layout": "single", "assets": "img2"}),
            refusal(
                "E_BAD_INPUT",
                "a single-file Markdown export takes one doc; the selection holds 2",
                "choose a profile that selects one doc",
            ),
        ),
        (
            json!({"format": "md", "layout": "single", "profile": "onep"}),
            refusal(
                "E_BAD_INPUT",
                &format!("figure {} needs --assets <dir>", ids["$f"]),
                "pass --assets with a folder beside the file",
            ),
        ),
        (
            json!({"format": "md", "layout": "single", "profile": "onep", "assets": "../x"}),
            refusal(
                "E_BAD_INPUT",
                "--assets ../x must be a folder below the folder of --out",
                "pass a relative folder path with no .. part, such as assets",
            ),
        ),
        (
            json!({"format": "md", "layout": "single", "profile": "onep", "assets": "img"}),
            refusal(
                "E_BAD_INPUT",
                "img is not empty",
                "choose an empty directory (or one that does not exist yet) so no stale files mix into the export",
            ),
        ),
        (
            json!({"format": "pdf", "layout": "single", "assets": "x"}),
            refusal(
                "E_BAD_INPUT",
                "--layout single applies to the md format only",
                "drop --layout, or export with --format md",
            ),
        ),
        (
            json!({"format": "md", "assets": "x"}),
            refusal(
                "E_BAD_INPUT",
                "--assets applies to --format md --layout single only",
                "drop --assets, or export with --format md --layout single",
            ),
        ),
    ];
    for (args, want) in cases {
        let got = both(&mut c, &|d| {
            let mut v = args.clone();
            v["out"] = json!(at(d, "other.md"));
            v
        });
        assert_eq!(got, want, "{args}");
    }
    // an output that is there
    let got = both(
        &mut c,
        &|d| json!({"out": at(d, "README.md"), "format": "md", "layout": "single", "profile": "onep", "assets": "img9"}),
    );
    assert_eq!(got.1["code"], "E_ALREADY_EXISTS", "{got:?}");
    // a misspelt key gets the same hint on both transports
    let (err, v) = both(
        &mut c,
        &|d| json!({"out": at(d, "x.md"), "format": "md", "layout": "single", "asset": "img"}),
    );
    assert!(err, "{v}");
    assert!(
        v["message"]
            .as_str()
            .unwrap()
            .contains("did you mean \"assets\"")
            || v["fix"]
                .as_str()
                .unwrap()
                .contains("did you mean \"assets\""),
        "{v}"
    );
    // the tool's schema lists the layout and the folder
    let tools = c.tools();
    let export = tools
        .iter()
        .find(|t| t["name"] == "rdy_export")
        .expect("rdy_export is listed");
    let props = &export["inputSchema"]["properties"];
    assert!(props["assets"].is_object(), "{props}");
    assert!(props["layout"].to_string().contains("single"), "{props}");
}
