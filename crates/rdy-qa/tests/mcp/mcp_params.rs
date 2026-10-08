//! mparm1: every MCP tool answers a wrong-typed or missing parameter with an rdy error
//! (E_BAD_INPUT naming the parameter and its shape), over stdio and through the daemon.
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::project;
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Map, Value, json};

/// Tools that take no parameters, so none can be wrong-typed.
const NO_PARAMS: [&str; 1] = ["rdy_tags"];

/// Whether the property's schema accepts a JSON string (its own type, or an anyOf branch's).
fn takes_string(prop: &Value) -> bool {
    let ty = |s: &Value| match &s["type"] {
        Value::String(t) => t == "string",
        Value::Array(ts) => ts.iter().any(|t| t == "string"),
        _ => false,
    };
    ty(prop) || prop["anyOf"].as_array().is_some_and(|bs| bs.iter().any(ty))
}

fn tool_error(r: Reply) -> Value {
    match r {
        Reply::ToolError(v) => v,
        other => panic!("expected an rdy tool error, got {other:?}"),
    }
}

#[test]
fn mparm1_every_tool_answers_a_wrong_typed_parameter_with_e_bad_input() {
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    let tools = c.tools();
    assert!(tools.len() >= 17, "{} tools", tools.len());
    let mut covered = vec![];
    for t in &tools {
        let name = t["name"].as_str().unwrap().to_string();
        let props = t["inputSchema"]["properties"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        let Some((param, schema)) = props.iter().next() else {
            assert!(
                NO_PARAMS.contains(&name.as_str()),
                "{name} has no parameters but is not in NO_PARAMS"
            );
            continue;
        };
        let wrong = if takes_string(schema) {
            json!([1])
        } else {
            json!("x")
        };
        let mut args = Map::new();
        args.insert(param.clone(), wrong);
        let e = tool_error(c.call(&name, Value::Object(args)));
        assert_eq!(e["code"], "E_BAD_INPUT", "{name}: {e}");
        let msg = e["message"].as_str().unwrap();
        assert!(
            msg.starts_with(&format!("{name}: parameter \"{param}\" must be ")),
            "{name}: {msg}"
        );
        assert!(
            e["fix"]
                .as_str()
                .unwrap()
                .starts_with(&format!("pass \"{param}\" as ")),
            "{name}: {e}"
        );
        assert!(
            !e.to_string().contains("failed to deserialize"),
            "{name}: {e}"
        );
        covered.push(name);
    }
    for n in NO_PARAMS {
        assert!(
            tools.iter().any(|t| t["name"] == n),
            "{n} is in NO_PARAMS but is not a tool"
        );
    }
    assert_eq!(covered.len() + NO_PARAMS.len(), tools.len(), "{covered:?}");
    c.alive();
}

#[test]
fn mparm1_a_missing_parameter_is_named_over_stdio_and_the_server_stays_usable() {
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    let e = tool_error(c.call("rdy_session", json!({})));
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert_eq!(e["message"], "rdy_session: parameter \"actor\" is required");
    assert_eq!(e["fix"], "pass \"actor\" as a string");
    let ok = c.call(
        "rdy_session",
        json!({"actor": "agent:qa", "role": "writer"}),
    );
    assert!(ok.code().is_none(), "{ok:?}");
    c.alive();
}

#[test]
fn mparm1_the_founders_case_is_an_rdy_error_through_the_daemon_too() {
    let s = Served::new();
    let mut c = McpClient::start_proxy(&s);
    let e = tool_error(c.call("rdy_propose", json!({"title": ["a", "b"]})));
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert_eq!(
        e["message"],
        "rdy_propose: parameter \"title\" must be a string, not a list"
    );
    assert_eq!(e["fix"], "pass \"title\" as a string");
    c.alive();
}
