//! mht7 (D-6M0AF, D-QG54Z, D-4PHYQ): how an MCP client finds MCP over HTTP. The overview and the
//! site tool name the endpoint, the serving build and the HTTP activity, and never the token. The
//! facts are inside the overview's token budget, the default budget included. A projectless stdio
//! server has no site, no listener and no token, and keeps its dynamic tool list. Each URL is read
//! from the daemon's own answer (W5); the HTTP client is `rdy_qa::mcp_http`.
use rdy_qa::daemon::{Hosted, short_tempdir};
use rdy_qa::fixtures::{land, output_within, write};
use rdy_qa::golden::tiny;
use rdy_qa::instructions::World;
use rdy_qa::mcp::{McpClient, Reply};
use rdy_qa::mcp_http::{Endpoint, LEGACY, call, claim, legacy, listen, modern, modern_meta, pairs};
use serde_json::{Value, json};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

const T: Duration = Duration::from_secs(60);
const ON: &str = "serve:\n  mcp_http: true\n";

/// A fresh project with `config` in its `.rdy/local.yaml` and `docs` extra docs in canon.
fn project(config: &str, docs: usize) -> tempfile::TempDir {
    let (dir, mut r) = tiny();
    if docs > 0 {
        let ops: Vec<Value> = (0..docs)
            .map(|n| {
                json!({"op": "create", "type": "doc", "fields": {
                    "name": format!("page-{n}"),
                    "title": format!("A page with a long enough title to cost tokens number {n}"),
                    "doc_type": "page"}})
            })
            .collect();
        write(&mut r, "w1", Value::Array(ops)).unwrap();
        land(&mut r, "w1").unwrap();
    }
    drop(r);
    std::fs::write(dir.path().join(".rdy/local.yaml"), config).unwrap();
    dir
}

/// The site's loopback link as the daemon reports it, and the bearer its token file holds.
fn endpoint(h: &Hosted) -> (Endpoint, String) {
    let url = h
        .run
        .as_ref()
        .expect("running")
        .shared()
        .site_info
        .get()
        .and_then(|i| i.url.clone().ok())
        .expect("the site listens");
    let text = std::fs::read_to_string(h.dir.path().join(".rdy/run/mcp-token")).expect("the token");
    let token = text.strip_suffix('\n').expect("one LF").to_string();
    (Endpoint::of(&url, &token), token)
}

/// A legacy call of `name` for the claim: its error flag and the structured result.
fn legacy_call(ep: &Endpoint, name: &str, args: Value, actor: &str) -> (bool, Value) {
    ep.post(
        &pairs(&legacy(LEGACY[0])),
        &call(1, name, args, claim(actor, "r1")),
    )
    .tool()
}

#[test]
fn mht7_overview_reports_serving_build_and_http_activity() {
    let h = Hosted::start_in(project(ON, 0));
    let (ep, token) = endpoint(&h);
    let (err, v) = legacy_call(&ep, "rdy_overview", json!({}), "agent:o");
    assert!(!err, "{v}");
    // the serving build: the daemon's own version and build id
    assert_eq!(
        v["daemon"],
        format!("rdy t build {}", rdy_daemon::BUILD),
        "{v}"
    );
    // the endpoint is the site's link and the route; this call is the one admitted call
    let site = h
        .run
        .as_ref()
        .unwrap()
        .shared()
        .site_info
        .get()
        .unwrap()
        .url
        .clone()
        .unwrap();
    assert_eq!(
        v["mcp_http"],
        json!({
            "enabled": true,
            "url": format!("{}/~mcp", site.trim_end_matches('/')),
            "active_calls": 1,
            "listen_streams": 0,
        }),
        "{v}"
    );
    // an open listen counts as a stream, and it holds no call
    let headers = modern("subscriptions/listen", None);
    let mut stream = ep.open(
        &pairs(&headers),
        &listen(9, &["rdy://inbox"], modern_meta(claim("agent:l", "l1"))),
    );
    assert_eq!(stream.status, 200, "{}", stream.head);
    assert!(stream.next(T).is_some(), "the acknowledgement");
    let (err, v) = legacy_call(&ep, "rdy_overview", json!({}), "agent:o");
    assert!(!err, "{v}");
    assert_eq!(v["mcp_http"]["listen_streams"], 1, "{v}");
    assert_eq!(v["mcp_http"]["active_calls"], 1, "{v}");
    drop(stream);
    // the site tool names the same endpoint, without the token
    let (err, site_reply) = legacy_call(&ep, "rdy_site", json!({}), "agent:o");
    assert!(!err, "{site_reply}");
    assert_eq!(site_reply["mcp_url"], v["mcp_http"]["url"], "{site_reply}");
    for shown in [v.to_string(), site_reply.to_string()] {
        assert!(!shown.contains(&token), "the token reached {shown}");
    }
}

#[test]
fn mht7_http_facts_fit_overview_budget() {
    // many docs, so that the tree is what the budget cuts
    let h = Hosted::start_in(project(ON, 120));
    let (ep, _) = endpoint(&h);
    let (err, small) = legacy_call(&ep, "rdy_overview", json!({"max_tokens": 700}), "agent:b");
    assert!(!err, "{small}");
    assert!(
        small.to_string().len() / 4 <= 700,
        "{}",
        small.to_string().len()
    );
    // the facts are whole inside the stated budget
    assert_eq!(small["mcp_http"]["enabled"], true, "{small}");
    assert!(small["mcp_http"]["url"].is_string(), "{small}");
    assert_eq!(small["mcp_http"]["active_calls"], 1, "{small}");
    // the default budget is the effective one when the call states none
    let (err, plain) = legacy_call(&ep, "rdy_overview", json!({}), "agent:c");
    assert!(!err, "{plain}");
    assert!(
        plain.to_string().len() / 4 <= 4000,
        "{}",
        plain.to_string().len()
    );
    assert_eq!(plain["mcp_http"], small["mcp_http"], "{plain}");
    // the tree gave way for the facts, not the other way round
    assert!(
        small["tree"].to_string().len() < plain["tree"].to_string().len(),
        "the small budget cut the tree"
    );
}

/// The command `rdy mcp` in `cwd`, as the world's person, with its stderr kept.
fn mcp_at(w: &World, cwd: &Path) -> McpClient {
    let mut cmd = rdy_qa::fixtures::rdy_command();
    cmd.args(["mcp"])
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(w.env())
        .current_dir(cwd);
    cmd.stderr(Stdio::piped());
    McpClient::launch(cmd)
}

fn names(c: &mut McpClient) -> Vec<String> {
    let mut n: Vec<String> = c
        .tools()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    n.sort();
    n
}

#[test]
fn mht7_projectless_stdio_keeps_its_dynamic_tool_list() {
    let w = World::new();
    let outside = short_tempdir();
    let mut c = mcp_at(&w, outside.path());
    // no project, no meta project: the two tools, no site, no listener, no token
    assert_eq!(names(&mut c), ["rdy_instructions", "rdy_session"]);
    let r = c.call(
        "rdy_session",
        json!({"actor": "agent:p", "run": "p1", "role": "reader"}),
    );
    assert!(r.is_ok(), "{r:?}");
    let shown = format!("{r:?}");
    assert!(
        !shown.contains("mcp_http") && !shown.contains("/~mcp"),
        "{shown}"
    );
    match c.call("rdy_overview", json!({})) {
        Reply::ToolError(e) => assert_eq!(e["code"], "E_NO_PROJECT", "{e}"),
        other => panic!("expected E_NO_PROJECT, got {other:?}"),
    }
    // the connection command makes no project, no meta project and no token
    let mut cmd = rdy_qa::fixtures::rdy_command();
    cmd.args(["--json", "mcp", "--http-config"])
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(w.env())
        .current_dir(outside.path());
    let o = output_within(&mut cmd, Duration::from_secs(120)).expect("rdy ran");
    assert!(!o.status.success());
    let v: Value = serde_json::from_slice(&o.stderr).expect("JSON error");
    assert_eq!(v["code"], "E_NO_PROJECT", "{v}");
    assert!(!w.default_meta().exists());
    assert!(!outside.path().join(".rdy").exists());
    // setup runs on the person's side; the same connection keeps its dynamic list (rtc5)
    let mut cmd = rdy_qa::fixtures::rdy_command();
    cmd.args(["--json", "setup"])
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(w.env())
        .current_dir(outside.path());
    let o = output_within(&mut cmd, Duration::from_secs(120)).expect("rdy ran");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        names(&mut c),
        ["rdy_instructions", "rdy_rule", "rdy_session"]
    );
    let t = c.finish(Duration::from_secs(120));
    for line in t.stdout.lines() {
        let v: Value = serde_json::from_str(line).unwrap_or_else(|_| panic!("not JSON: {line}"));
        assert_eq!(v["jsonrpc"], "2.0", "{line}");
    }
    assert!(!t.stderr.contains("panicked"), "{}", t.stderr);
    w.stop(&w.default_meta());
}
