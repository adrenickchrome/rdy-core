//! ib3: `rdy_session` declares inbox areas over the real server.
use rdy_qa::mcp::*;
use rdy_qa::packs::qa_project;
use serde_json::json;

#[test]
fn ib3_rdy_session_takes_areas_and_returns_them() {
    let (dir, r) = qa_project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    let first = c
        .call(
            "rdy_session",
            json!({"actor": "agent:a", "run": "a", "role": "writer", "areas": ["payments"]}),
        )
        .ok();
    assert_eq!(first["areas"], json!(["payments"]), "{first}");
    let second = c
        .call(
            "rdy_session",
            json!({"actor": "agent:a", "run": "a", "role": "writer"}),
        )
        .ok();
    assert_eq!(second["areas"], json!(["payments"]), "{second}");
    let bad = c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer", "areas": ["nope"]}),
    );
    assert_eq!(bad.code(), Some("E_BAD_INPUT"), "{bad:?}");
}
