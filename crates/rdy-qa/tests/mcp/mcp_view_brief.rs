//! mvcq1: over MCP, rdy_view include returns consequences and rdy_brief shows the other view
//! that changes a covered block.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::*;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use serde_json::json;

#[test]
fn mvcq1_rdy_view_include_returns_consequences_and_rdy_brief_shows_the_other_view() {
    let s = Served::new();
    let (sp, cb) = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let sp = spec_doc(&mut r);
        let cb = write_as(
            &mut r,
            &as_agent("agent:b", "r1"),
            "vb",
            json!([{"op": "update", "id": sp.a.to_string(), "body": "A, by b."}]),
        )
        .unwrap()
        .change;
        r.close().unwrap();
        (sp, cb)
    };
    let mut c = McpClient::start_proxy(&s);
    let r = c.call(
        "rdy_session",
        json!({"actor": "agent:m", "run": "m1", "role": "writer"}),
    );
    assert!(r.code().is_none());
    let v = c
        .call("rdy_view", json!({"op": "include", "changes": [cb]}))
        .ok();
    assert!(
        v["consequences"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["block"] == sp.b.to_string()
                && x["kind"] == "suspect"
                && x["id"].as_str().unwrap().starts_with("cq-")),
        "{v}"
    );
    let b = c.call("rdy_brief", json!({"ids": [sp.a.to_string()]})).ok();
    let views = b["inflight"]["views"].as_array().unwrap();
    assert!(
        views.iter().any(|x| x["view"] == "vb"
            && x["owner"]["actor"] == "agent:b"
            && x["blocks"] == json!([sp.a.to_string()])),
        "{b}"
    );
    drop(c);
    s.stop();
}
