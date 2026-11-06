//! kfu5 (FW-MH.kungfu, D-Y18E1): `rdy_overview` carries the local kungfu summary from the
//! recorder's memory, at every content scope, after its bounded content and with no wait for a
//! flush; `rdy_schema` documents the `W_KUNGFU` code without a parameter, a value or a hint.
use rdy_engine::Rdy;
use rdy_engine::kungfu_counts as kc;
use rdy_mcp::tools::ERROR_ROWS;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::instructions::World;
use rdy_qa::mcp::{DEADLINE, McpClient, Reply, fixture_value, supply_kungfu};
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, Instant};

fn body(r: Reply) -> Value {
    match r {
        Reply::Ok(v) | Reply::ToolError(v) => v,
        Reply::RpcError(e) => panic!("a protocol error: {e}"),
    }
}

/// `args` with this source's value for `tool`.
fn right(tool: &str, mut args: Value) -> Value {
    supply_kungfu(tool, &mut args);
    args
}

fn raw_client(w: &World, dir: &Path, actor: &str, run: &str) -> McpClient {
    let env = w.env();
    let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut c = McpClient::start_with_env(dir, &pairs).raw();
    let r = c.call(
        "rdy_session",
        json!({"actor": actor, "run": run, "role": "writer"}),
    );
    assert!(r.is_ok(), "{r:?}");
    c
}

fn line(not_loaded: u64, sessions: u64) -> String {
    format!("kungfu: {not_loaded} not loaded, 0 lost, 0 wrong, in {sessions} sessions.")
}

#[test]
fn kfu5_overview_uses_cached_counts_at_every_scope() {
    let w = World::new();
    let pd = w.project();
    {
        let mut r = Rdy::open(pd.path()).expect("open");
        spec_doc(&mut r);
        r.close().expect("close");
    }
    let mut c = raw_client(&w, pd.path(), "agent:o", "o1");
    // the landing the spec doc made: the historical scope
    let log = body(c.call("rdy_log", right("rdy_log", json!({"limit": 1}))));
    let landing = log[0]["landing"]
        .as_str()
        .unwrap_or_else(|| panic!("a landing in {log}"))
        .to_string();
    let scopes = ["canon".to_string(), format!("landing:{landing}")];

    // no miss yet: the overview omits the string at every scope
    for scope in &scopes {
        let o = body(c.call(
            "rdy_overview",
            right("rdy_overview", json!({"scope": scope})),
        ));
        assert!(o.get("kungfu").is_none(), "{scope}: {o}");
    }

    // hold the real file lock so that no count can reach the file, then miss twice
    let miss = body(c.call("rdy_tags", json!({})));
    assert!(miss.get("versions").is_some(), "{miss}");
    let lock_path = pd.path().join(".rdy/run").join(kc::LOCK_NAME);
    let start = Instant::now();
    // the first flush makes the lock file; wait for it, then take the lock
    let lock = loop {
        if let Ok(f) = std::fs::File::open(&lock_path)
            && f.try_lock().is_ok()
        {
            break f;
        }
        assert!(start.elapsed() < DEADLINE, "the lock file never appeared");
        std::thread::sleep(Duration::from_millis(5));
    };
    body(c.call("rdy_tags", json!({})));
    for scope in &scopes {
        let o = body(c.call(
            "rdy_overview",
            right("rdy_overview", json!({"scope": scope})),
        ));
        assert_eq!(o["kungfu"], line(2, 1).as_str(), "{scope}: {o}");
        assert!(o.get("kungfu_counts").is_none(), "{scope}: {o}");
    }

    // bounded replies: a small budget cuts the tree, not the string, and the string adds one line
    let small = body(c.call(
        "rdy_overview",
        right("rdy_overview", json!({"max_tokens": 400})),
    ));
    assert_eq!(small["kungfu"], line(2, 1).as_str(), "{small}");
    assert!(small["kungfu"].as_str().unwrap().len() < 512);
    lock.unlock().unwrap();
    drop(lock);
}

#[test]
fn kfu5_errors_reference_names_code_without_parameter() {
    let row = ERROR_ROWS
        .iter()
        .find(|(c, _, _)| *c == "W_KUNGFU")
        .expect("a W_KUNGFU row");
    assert_eq!(
        *row,
        (
            "W_KUNGFU",
            "A call lacks the current value from its tool's ruleset. The call still runs. The warning asks the agent to reload its instructions.",
            "Call rdy_instructions and load the ruleset named by the warning."
        )
    );
    let n = ERROR_ROWS.len();
    assert_eq!(
        ERROR_ROWS[n - 3].0,
        "W_KUNGFU",
        "before the publication rows"
    );

    let w = World::new();
    let pd = w.project();
    let mut c = raw_client(&w, pd.path(), "agent:s", "s1");
    let schema = body(c.call("rdy_schema", right("rdy_schema", json!({}))));
    assert_eq!(
        schema["errors"]["W_KUNGFU"],
        json!({"when": row.1, "fix": row.2}),
        "{schema}"
    );
    // the code name is the only mention: no parameter row, no way to send it, no value
    let text = schema.to_string().replace("W_KUNGFU", "");
    assert!(!text.to_lowercase().contains("kungfu"), "{text}");
    assert!(!text.contains("kf-"), "no value in the reference");
    let value = fixture_value("rdy_tags").unwrap();
    assert!(!schema.to_string().contains(value));

    // no tool's input schema gains the key
    for tool in c.tools() {
        let schema = tool["inputSchema"].to_string();
        assert!(
            !schema.to_lowercase().contains("kungfu"),
            "{}",
            tool["name"]
        );
    }
    let all = c.tools().iter().map(Value::to_string).collect::<String>();
    assert!(
        !all.to_lowercase().contains("kungfu"),
        "no discovery hint in the tool list"
    );
}
