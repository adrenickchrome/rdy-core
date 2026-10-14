//! ls7: `rdy_site` gives the person the live site's link, for canon, a view or a proposal version
//! (D-N5NSN, D-FV6G9, D-KQ4CA); with no daemon there is no site.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::golden::{FIXTURE_DIR, Ids, approver, land, write, writer};
use rdy_qa::http;
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::path::Path;

/// A served project holding the golden corpus, landed, and a view `editing` with one unlanded
/// change, proposed once. Returns the project and the proposal's `P-x@1` text.
fn golden_served() -> (Served, String) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    r.import(&approver(), "import", Path::new(FIXTURE_DIR))
        .unwrap();
    land(&mut r, "import").unwrap();
    let ids = Ids::new();
    write(
        &mut r,
        "editing",
        json!([{"op": "update", "id": ids.tutorial_prose.to_string(),
            "body": "So far we have used three simple patterns. They compose."}]),
    )
    .unwrap();
    let p = r.propose(&writer(), "editing", "tutorial").unwrap();
    r.close().unwrap();
    (s, p.r#ref)
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

fn page(url: &str) -> http::Response {
    http::get(url)
}

#[test]
fn ls7_rdy_site_gives_the_link() {
    let (s, _) = golden_served();
    let mut c = agent(&s);
    let v = c.call("rdy_site", json!({})).ok();
    assert_eq!(v["scope"], "canon", "{v}");
    assert!(v.get("warning").is_none(), "{v}");
    let url = v["url"].as_str().unwrap();
    assert!(url.starts_with("http://127.0.0.1:"), "{v}");
    assert_eq!(page(url).status, 200, "{v}");
}

#[test]
fn ls7_rdy_site_for_a_view_and_a_proposal() {
    let (s, proposal) = golden_served();
    let mut c = agent(&s);
    let v = c.call("rdy_site", json!({"scope": "editing"})).ok();
    assert_eq!(v["scope"], "view:editing", "{v}");
    let url = v["url"].as_str().unwrap();
    assert!(url.ends_with("~view/editing/"), "{v}");
    let got = page(url);
    assert_eq!(got.status, 200, "{v}");
    assert!(
        String::from_utf8_lossy(&got.body).contains("Show changes"),
        "a view serves in review mode"
    );
    let same = c.call("rdy_site", json!({"scope": "view:editing"})).ok();
    assert_eq!(same["url"], v["url"], "{same}");

    let scope = format!("proposal:{proposal}");
    let p = c.call("rdy_site", json!({ "scope": scope })).ok();
    assert_eq!(p["scope"], scope.as_str(), "{p}");
    let url = p["url"].as_str().unwrap();
    assert!(url.ends_with(&format!("~proposal/{proposal}/")), "{p}");
    assert_eq!(page(url).status, 200, "{p}");

    let bad = c.call("rdy_site", json!({"scope": "nosuch"}));
    assert_eq!(bad.code(), Some("E_NOT_FOUND"), "{bad:?}");
    let bad = c.call("rdy_site", json!({"scope": "proposal:P-ffff@9"}));
    assert!(!bad.is_ok(), "{bad:?}");
}

#[test]
fn ls7_rdy_site_needs_a_session() {
    let (s, _) = golden_served();
    let mut c = McpClient::start_proxy(&s);
    let r = c.call("rdy_site", json!({}));
    assert_eq!(r.code(), Some("E_NO_SESSION"), "{r:?}");
}

#[test]
fn ls7_no_daemon_has_no_site() {
    let (s, _) = golden_served();
    let mut c = McpClient::start(s.dir.path());
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer"}),
    )
    .ok();
    let r = c.call("rdy_site", json!({}));
    let Reply::ToolError(e) = r else {
        panic!("expected a tool error, got {r:?}")
    };
    assert_eq!(e["code"], "E_SITE", "{e}");
    assert_eq!(
        e["message"], "the live site is served by the daemon, and this session runs without one",
        "{e}"
    );
    assert_eq!(
        e["fix"], "drop --no-daemon; for a static site, run rdy render --out <dir>",
        "{e}"
    );
    let _: Value = e;
}
