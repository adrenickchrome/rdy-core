//! rf8 (D-4WC6R, D-XC9B9): rdy_write takes `reflects: {sources?, code?}`, a closed object. Its
//! sources resolve in the write's overlay; the reply and the stored change carry the normalized
//! justification, the same one the engine's edit path stores. A false code, an unknown key, a
//! second justification and the bounds are refused before anything is written.
use rdy_engine::{ReflectsInput, Scope};
use rdy_mcp::tools::JUSTIFICATION_GUIDE;
use rdy_model::{BlockId, Hash, Justification};
use rdy_qa::fixtures::*;
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};

fn tool_error(r: Reply) -> Value {
    match r {
        Reply::ToolError(v) => v,
        other => panic!("expected a tool error, got {other:?}"),
    }
}

/// A landed draft doc with section `s`, claims `c` and `c2`, and source `src` (key SRC-ONE).
fn seeded() -> (tempfile::TempDir, Value) {
    let (dir, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$notes", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$notes", "fields": {"title": "Retention"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Retention is thirty days."},
        {"op": "create", "type": "claim", "id": "$c2", "parent": "$s", "body": "Backups follow retention."},
        {"op": "create", "type": "source", "id": "$src", "parent": "$s",
            "fields": {"title": "Release 2", "kind": "release", "locator": "v2", "key": "SRC-ONE"}}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    drop(r);
    (dir, serde_json::to_value(&w.created).unwrap())
}

fn change_of(dir: &std::path::Path, change: &str) -> rdy_model::Change {
    let r = rdy_engine::Rdy::open(dir).unwrap();
    let c = r
        .read()
        .reader()
        .change(&change.parse::<Hash>().unwrap())
        .unwrap();
    r.close().unwrap();
    c
}

#[test]
fn rf8_mcp_cli_and_recovered_commit_record_one_justification() {
    let (dir, ids) = seeded();
    let id = |k: &str| ids[k].as_str().unwrap().to_string();
    let (c, c2, src) = (id("$c"), id("$c2"), id("$src"));
    let mut cl = McpClient::start(dir.path());
    let ok = cl.call(
        "rdy_session",
        json!({"actor": "agent:qa", "run": "qa", "role": "writer"}),
    );
    assert!(ok.code().is_none(), "{ok:?}");
    // the guidance names the third justification
    let tools = cl.tools();
    let w = tools.iter().find(|t| t["name"] == "rdy_write").unwrap();
    assert!(
        w["description"]
            .as_str()
            .unwrap()
            .contains(JUSTIFICATION_GUIDE),
        "{w}"
    );
    assert!(
        w["inputSchema"]["properties"]["reflects"].is_object(),
        "{w}"
    );
    // refusals, each before anything is written
    let update = json!([{"op": "update", "id": c, "body": "Retention is seven days."}]);
    let refusals = [
        (
            json!({"ops": update, "reflects": {"code": false}}),
            "E_BAD_INPUT",
            "reflects code must be true when given".to_string(),
            "pass code: true, or omit code".to_string(),
        ),
        (
            json!({"ops": update, "reflects": {"sources": [src]}, "implements": [src]}),
            "E_BAD_INPUT",
            "give one of implements, editorial or reflects".to_string(),
            "implements for decision-backed changes, editorial for typo, restructure or clarify, reflects for changes that follow a source".to_string(),
        ),
        (
            json!({"ops": update, "reflects": {}}),
            "E_BAD_INPUT",
            "reflects names no source and no code".to_string(),
            "name a source (S-…), or pass code: true for a block with reflects globs".to_string(),
        ),
        (
            json!({"ops": update, "reflects": {"sources": [c2]}}),
            "E_BAD_INPUT",
            format!("reflects names {c2}, a claim; reflects names a source"),
            "name a source (S-…), or create one with a source op".to_string(),
        ),
        (
            json!({"ops": update, "reflects": {"code": true}}),
            "E_BAD_INPUT",
            "reflects code: no block this write changes declares reflects globs".to_string(),
            "set reflects on the block, or name a source".to_string(),
        ),
        (
            json!({"ops": update, "reflects": {"sources": vec![src.clone(); 5001]}}),
            "E_LIMIT",
            "reflects takes at most 5000 sources".to_string(),
            "split the write into smaller changes".to_string(),
        ),
    ];
    for (args, code, message, fix) in refusals {
        let e = tool_error(cl.call("rdy_write", args));
        assert_eq!(
            (e["code"].as_str(), e["message"].as_str(), e["fix"].as_str()),
            (Some(code), Some(message.as_str()), Some(fix.as_str())),
            "{e}"
        );
    }
    // the nested keys are closed, with a did-you-mean naming sources and code
    for (bad, near) in [("sorces", "sources"), ("cod", "code")] {
        let e = tool_error(cl.call("rdy_write", json!({"ops": update, "reflects": {bad: true}})));
        assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
        assert_eq!(
            e["message"],
            format!("rdy_write: unknown parameter \"reflects.{bad}\"")
        );
        let fix = e["fix"].as_str().unwrap();
        assert!(fix.contains(&format!("did you mean \"{near}\"")), "{fix}");
        assert!(fix.contains("code") && fix.contains("sources"), "{fix}");
    }
    // an id and the key of the same source: one source, in the reply and the change
    let v = cl
        .call(
            "rdy_write",
            json!({"ops": update, "reflects": {"sources": [src, "src-one"]}}),
        )
        .ok();
    assert_eq!(
        v["justification"],
        json!({"reflects": {"sources": [src], "code": false}}),
        "{v}"
    );
    assert!(v.get("code_recorded").is_none(), "{v}");
    let change = v["change"].as_str().unwrap().to_string();
    cl.alive();
    drop(cl);
    let want = Justification::Reflects {
        sources: vec![BlockId::parse(&src).unwrap()],
        code: false,
    };
    assert_eq!(
        change_of(dir.path(), &change).justification,
        Some(want.clone())
    );
    // the engine's edit path stores the same normalized form
    let mut r = rdy_engine::Rdy::open(dir.path()).unwrap();
    let mut edited = r
        .read()
        .block(&Scope::View("work".into()), &BlockId::parse(&c2).unwrap())
        .unwrap();
    edited.body = "Backups follow release 2.".into();
    let w = r
        .edit_block_with(
            &writer(),
            "work",
            edited,
            None,
            None,
            Some(ReflectsInput {
                sources: vec!["SRC-ONE".into()],
                code: false,
            }),
        )
        .unwrap();
    let stored = r
        .read()
        .reader()
        .change(&w.change.parse::<Hash>().unwrap())
        .unwrap()
        .justification;
    assert_eq!(stored, Some(want));
    r.close().unwrap();
}
