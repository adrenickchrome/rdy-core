//! wx22: native Windows paths and results through the MCP tools. The tests run on every
//! platform: a path written the Windows way is plain text to the tools, and no tool reads a
//! backslash, a drive letter or a long-path prefix as anything but text.
use rdy_engine::Rdy;
use rdy_engine::engine::Local;
use rdy_mcp::tools::*;
use serde_json::{Value, json};

/// Paths as Windows writes them: a drive path with a space, a UNC share, a long-path prefix and
/// a mix of both slashes.
const NATIVE: [&str; 4] = [
    r"C:\Users\dev\My Project\src\lib.rs",
    r"\\server\share\docs\a.md",
    r"\\?\C:\very\long\path.txt",
    r"D:/mixed/slash\back.rs",
];

fn state() -> (tempfile::TempDir, McpState) {
    let dir = tempfile::tempdir().unwrap();
    let rdy = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let mut st = McpState::new(std::sync::Arc::new(Local::new(rdy)));
    rdy_session(
        &mut st,
        serde_json::from_value(
            json!({"actor": "agent:designer", "run": "run-22", "role": "writer"}),
        )
        .unwrap(),
    )
    .unwrap();
    (dir, st)
}

fn call<P: serde::de::DeserializeOwned>(
    f: fn(&mut McpState, P) -> Result<Value, rdy_engine::RdyError>,
    st: &mut McpState,
    args: Value,
) -> Value {
    f(st, serde_json::from_value(args).unwrap()).unwrap()
}

fn seed(st: &mut McpState) -> Value {
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Paths", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Native"}}),
    ];
    for (i, path) in NATIVE.iter().enumerate() {
        ops.push(json!({"op": "create", "type": "claim", "id": format!("$c{i}"), "parent": "$s", "body": path}));
    }
    call(rdy_write, st, json!({ "ops": ops }))
}

#[test]
fn wx22_native_paths_survive_json_and_markdown() {
    let (_d, mut st) = state();
    let made = seed(&mut st);
    let doc = made["created"]["$doc"].as_str().unwrap().to_string();
    for format in ["compact", "markdown", "yaml"] {
        let read = call(
            rdy_read,
            &mut st,
            json!({"id": doc, "scope": "run-22", "format": format}),
        );
        let text = read["text"].as_str().unwrap();
        for path in NATIVE {
            assert!(text.contains(path), "{format} lost {path:?}:\n{text}");
        }
    }
    // the reply is JSON: it serializes and parses back with every path whole
    let found = call(
        rdy_query,
        &mut st,
        json!({"filter": "type = claim", "scope": "run-22"}),
    );
    let again: Value = serde_json::from_str(&found.to_string()).unwrap();
    assert_eq!(again, found);
    let shown = found.to_string();
    for path in NATIVE {
        let encoded = serde_json::to_string(path).unwrap();
        let inner = &encoded[1..encoded.len() - 1];
        assert!(shown.contains(inner), "query lost {path:?}: {found}");
    }
    let one = call(
        rdy_read,
        &mut st,
        json!({"id": made["created"]["$c1"], "scope": "run-22"}),
    );
    assert!(one["text"].as_str().unwrap().contains(NATIVE[1]), "{one}");
}

#[test]
fn wx22_a_dump_path_comes_back_as_it_was_written() {
    let (d, mut st) = state();
    seed(&mut st);
    let out = d.path().join("backup dir");
    std::fs::create_dir(&out).unwrap();
    let file = out.join("project.dump");
    let text = file.to_str().unwrap();
    let summary = call(rdy_dump, &mut st, json!({"out": text}));
    assert_eq!(summary["out"], json!(text), "{summary}");
    assert!(file.is_file());
}
