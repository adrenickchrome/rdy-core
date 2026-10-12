//! ib10: the MCP resources and `resources/subscribe` over the real server (no daemon).
use rdy_qa::fixtures::project;
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};

fn server() -> (tempfile::TempDir, McpClient) {
    let (dir, r) = project();
    drop(r); // release the store lock for the server
    let c = McpClient::start(dir.path());
    (dir, c)
}

/// Opens the session and returns its view.
fn session(c: &mut McpClient) -> String {
    let s = c
        .call(
            "rdy_session",
            json!({"actor": "agent:qa", "run": "r1", "role": "writer"}),
        )
        .ok();
    s["view"].as_str().unwrap().to_string()
}

/// The JSON a successful `resources/read` carried, and its mime type.
fn json_of(reply: &Value) -> (Value, String) {
    let c = &reply["result"]["contents"][0];
    let text = c["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text in {reply}"));
    (
        serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}: {text}")),
        c["mimeType"].as_str().unwrap_or_default().to_string(),
    )
}

fn names(list: &[Value], key: &str) -> Vec<String> {
    list.iter()
        .map(|r| r[key].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn ib10_get_info_declares_resources_with_subscribe() {
    let (_d, c) = server();
    let caps = &c.initialize_result()["result"]["capabilities"];
    assert_eq!(caps["resources"]["subscribe"], true, "{caps}");
    assert!(caps["tools"].is_object(), "{caps}");
}

#[test]
fn ib10_the_six_resources_are_listed() {
    let (_d, mut c) = server();
    let uris = names(&c.list_resources(), "uri");
    for want in ["rdy://project", "rdy://pack", "rdy://inbox"] {
        assert!(uris.contains(&want.to_string()), "{want} in {uris:?}");
    }
    let templates = names(&c.list_resource_templates(), "uriTemplate");
    for want in ["rdy://doc/{id}", "rdy://view/{name}", "rdy://proposal/{id}"] {
        assert!(
            templates.contains(&want.to_string()),
            "{want} in {templates:?}"
        );
    }
    assert_eq!(uris.len() + templates.len(), 6, "{uris:?} {templates:?}");
}

#[test]
fn ib10_each_resource_reads_as_its_tool() {
    let (_d, mut c) = server();
    let own_view = session(&mut c);
    let w = c
        .call(
            "rdy_write",
            json!({"ops": [{"op": "create", "type": "doc", "id": "$d",
                "fields": {"name": "a", "title": "A", "doc_type": "glossary"}}]}),
        )
        .ok();
    let doc = w["created"]["$d"].as_str().unwrap().to_string();

    let overview = c.call("rdy_overview", json!({})).ok();
    let (project, mime) = json_of(&c.read_resource("rdy://project"));
    assert_eq!(mime, "application/json");
    let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    assert_eq!(keys(&project), keys(&overview), "{project}");

    let schema = c.call("rdy_schema", json!({})).ok();
    let (pack, _) = json_of(&c.read_resource("rdy://pack"));
    assert_eq!(keys(&pack), keys(&schema), "{pack}");

    let (read, _) = json_of(&c.read_resource(&format!("rdy://doc/{doc}")));
    assert!(read.to_string().contains(&doc), "{read}");

    let (view, _) = json_of(&c.read_resource(&format!("rdy://view/{own_view}")));
    assert!(view.is_object(), "{view}");

    let (inbox, _) = json_of(&c.read_resource("rdy://inbox"));
    assert!(inbox["entries"].is_array(), "{inbox}");
    assert!(inbox["cursor"].is_string(), "{inbox}");
    // a resource read never acks
    assert!(
        inbox.get("acked").is_none_or(|a| a != &json!(true)),
        "{inbox}"
    );
}

#[test]
fn ib10_a_proposal_reads_as_rdy_read_does() {
    let (_d, mut c) = server();
    session(&mut c);
    let w = c
        .call(
            "rdy_write",
            json!({"ops": [{"op": "create", "type": "doc",
                "fields": {"name": "a", "title": "A", "doc_type": "glossary"}}]}),
        )
        .ok();
    assert!(w["change"].is_string(), "{w}");
    let p = c.call("rdy_propose", json!({"title": "t"})).ok();
    let id = p["ref"].as_str().unwrap().to_string();
    let (via_resource, _) = json_of(&c.read_resource(&format!("rdy://proposal/{id}")));
    let via_tool = c.call("rdy_read", json!({"id": id})).ok();
    assert_eq!(via_resource["id"], via_tool["id"], "{via_resource}");
    assert!(via_resource["proposal"].is_object(), "{via_resource}");
}

#[test]
fn ib10_a_read_without_a_session_is_refused() {
    let (_d, mut c) = server();
    let r = c.read_resource("rdy://inbox");
    let message = r["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("{r}"));
    assert!(message.contains("E_NO_SESSION"), "{r}");
    assert_eq!(r["error"]["code"], -32600, "{r}");
    c.alive();
}

#[test]
fn ib10_an_unknown_uri_is_refused() {
    let (_d, mut c) = server();
    session(&mut c);
    for r in [c.read_resource("rdy://nope"), c.subscribe("file:///etc")] {
        let message = r["error"]["message"]
            .as_str()
            .unwrap_or_else(|| panic!("{r}"));
        assert!(message.starts_with("unknown resource"), "{r}");
    }
    // a known URI subscribes
    let ok = c.subscribe("rdy://inbox");
    assert!(ok.get("error").is_none(), "{ok}");
}
