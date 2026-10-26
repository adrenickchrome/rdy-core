//! mparm1: every MCP tool answers a wrong-typed or missing parameter with an rdy error
//! (E_BAD_INPUT naming the parameter and its shape), over stdio and through the daemon.
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::project;
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Map, Value, json};

/// Tools that take no parameters, so none can be wrong-typed.
const NO_PARAMS: [&str; 2] = ["rdy_tags", "rdy_key_show"];

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
    // ok11: rdy_upgrade's one parameter, a boolean, given a string
    assert!(covered.iter().any(|n| n == "rdy_upgrade"), "{covered:?}");
    let e = tool_error(c.call("rdy_upgrade", json!({"dry_run": "x"})));
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .starts_with("rdy_upgrade: parameter \"dry_run\" must be "),
        "{e}"
    );
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

#[test]
fn hd7_an_unknown_parameter_is_refused_with_a_did_you_mean() {
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    c.call(
        "rdy_session",
        json!({"actor": "agent:qa", "run": "qa", "role": "writer"}),
    )
    .ok();
    let e = tool_error(c.call("rdy_write", json!({"ops": [], "mesage": "m"})));
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert_eq!(e["message"], "rdy_write: unknown parameter \"mesage\"");
    assert!(
        e["fix"]
            .as_str()
            .unwrap()
            .starts_with("did you mean \"message\"? the parameters here are "),
        "{e}"
    );
    let e = tool_error(c.call("rdy_search", json!({"query": "x", "type": ["claim"]})));
    assert_eq!(
        e["message"], "rdy_search: unknown parameter \"type\"",
        "a read tool is strict too"
    );
    let e = tool_error(c.call(
        "rdy_write",
        json!({"ops": [], "editorial": {"clas": "typo", "reason": "r"}}),
    ));
    assert_eq!(
        e["message"], "rdy_write: unknown parameter \"editorial.clas\"",
        "a nested key names its path"
    );
    let e = tool_error(c.call("rdy_tags", json!({"x": 1})));
    assert_eq!(e["fix"], "remove \"x\"; rdy_tags takes no parameters");
    let e = tool_error(c.call(
        "rdy_write",
        json!({"ops": [{"op": "move", "id": "x", "parent_id": "y"}]}),
    ));
    assert_eq!(
        e["message"], "op 0: unknown field \"parent_id\"",
        "an op inside is Task 3's text"
    );
    c.alive(); // panics when the server no longer answers
}

#[test]
fn hd7_every_tool_publishes_a_closed_schema() {
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    let tools = c.tools();
    for t in &tools {
        assert_eq!(
            t["inputSchema"]["additionalProperties"], false,
            "{}",
            t["name"]
        );
    }
    // ok11: rdy_upgrade publishes dry_run and nothing else
    let upgrade = tools
        .iter()
        .find(|t| t["name"] == "rdy_upgrade")
        .expect("rdy_upgrade is listed");
    let keys: Vec<&String> = upgrade["inputSchema"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(keys, ["dry_run"]);
}

#[test]
fn hd7_every_tool_accepts_its_documented_parameters() {
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    c.call(
        "rdy_session",
        json!({"actor": "agent:qa", "run": "qa", "role": "writer"}),
    )
    .ok();
    for t in c.tools() {
        let name = t["name"].as_str().unwrap().to_string();
        let props = t["inputSchema"]["properties"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        for key in props.keys() {
            let mut args = serde_json::Map::new();
            args.insert(key.clone(), serde_json::Value::Null);
            let r = c.call(&name, serde_json::Value::Object(args));
            if let Reply::ToolError(e) = &r {
                // `unknown parameter` is the schema walk's text; `unknown field` is the serde
                // fallback's. The serde-layer guard is `hd7_serde_accepts_every_key_each_params_
                // schema_publishes` in rdy-mcp, which does not depend on the order of the checks.
                let msg = e["message"].as_str().unwrap_or("");
                assert!(
                    !msg.contains("unknown parameter") && !msg.contains("unknown field"),
                    "{name}: documented parameter {key} is refused as unknown: {e}"
                );
            }
        }
    }
    // ok11: rdy_upgrade's documented input, given in full
    let r = c.call("rdy_upgrade", json!({"dry_run": true}));
    assert!(r.code().is_none(), "{r:?}");
}
