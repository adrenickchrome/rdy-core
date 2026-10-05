//! id@version and ref lines in rdy_read and rdy_context (refv1).
use rdy_qa::{golden::*, mcp::*};

fn err(c: &mut McpClient, tool: &str, args: serde_json::Value) -> serde_json::Value {
    match c.call(tool, args) {
        Reply::ToolError(v) => v,
        other => panic!("expected a tool error, got {other:?}"),
    }
}

#[test]
fn refv1_rdy_read_accepts_a_pinned_ref_and_reads_under_that_version() {
    let (dir, mut r, ids) = decided_site();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    write(&mut r, "work", serde_json::json!([{"op":"update","id":ids.requirement.to_string(),"body":"Ledgers are atomic always."}])).unwrap();
    land(&mut r, "work").unwrap();
    drop(r);
    let mut c = McpClient::start(dir.path());
    c.call(
        "rdy_session",
        serde_json::json!({"actor": "agent:qa", "role": "reader"}),
    )
    .ok();
    let pinned = c.call("rdy_read", serde_json::json!({"id": format!("[{}@v1.0.0] Ledgers › Rules: \"Ledgers are atomic when settled.\"", ids.requirement)})).ok();
    assert!(
        pinned
            .to_string()
            .contains("Ledgers are atomic when settled."),
        "{pinned}"
    );
    let now = c
        .call(
            "rdy_read",
            serde_json::json!({"id": ids.requirement.to_string()}),
        )
        .ok();
    assert!(
        now.to_string().contains("Ledgers are atomic always."),
        "{now}"
    );
    let short = c
        .call(
            "rdy_context",
            serde_json::json!({"id": format!("{}@v1.0.0", ids.requirement)}),
        )
        .ok();
    assert!(short.to_string().contains("when settled"), "{short}");
    let clash = err(
        &mut c,
        "rdy_read",
        serde_json::json!({"id": format!("{}@v1.0.0", ids.requirement), "scope": "canon"}),
    );
    assert_eq!(clash["code"], "E_BAD_INPUT");
    assert!(
        clash["message"]
            .as_str()
            .unwrap()
            .contains("pins version v1.0.0")
    );
    let same = c
        .call(
            "rdy_read",
            serde_json::json!({"id": format!("{}@v1.0.0", ids.requirement), "scope": "tag:v1.0.0"}),
        )
        .ok();
    assert!(same.to_string().contains("when settled"), "{same}");
    let unknown = err(
        &mut c,
        "rdy_read",
        serde_json::json!({"id": format!("{}@v9.9.9", ids.requirement)}),
    );
    assert_eq!(unknown["code"], "E_NOT_FOUND");
}
