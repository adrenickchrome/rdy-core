//! df8: `rdy_brief` takes `<doc>#<heading-slug>` in `path` and in an `ids` entry (F10).
use rdy_qa::edit_loop::*;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// The stable spec doc, plus a second section `Extras` with one claim and a folder `notes`.
fn project_with_extras() -> (tempfile::TempDir, Spec, String, String) {
    let (dir, mut r) = project();
    let sp = stable_spec(&mut r);
    let w = write(
        &mut r,
        "more",
        json!([
            {"op": "create", "type": "section", "id": "$x", "parent": sp.doc.to_string(), "fields": {"title": "Extras"}},
            {"op": "create", "type": "claim", "id": "$x1", "parent": "$x", "body": "An extra claim."},
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "notes", "title": "Notes"}}
        ]),
    )
    .expect("extras");
    let (x1, folder) = (w.created["$x1"].clone(), w.created["$f"].clone());
    land(&mut r, "more").expect("land extras");
    r.close().unwrap();
    (dir, sp, x1, folder)
}

fn session(c: &mut McpClient) {
    let r = c.call(
        "rdy_session",
        json!({"actor": "agent:df8", "role": "writer"}),
    );
    assert!(r.code().is_none(), "{:?}", r.ok());
}

fn tool_error(r: Reply) -> Value {
    match r {
        Reply::ToolError(v) => v,
        other => panic!("expected a tool error, got {other:?}"),
    }
}

fn base_ids(b: &Value) -> BTreeSet<String> {
    b["base"]["blocks"]
        .as_array()
        .unwrap_or_else(|| panic!("{b}"))
        .iter()
        .map(|x| x["id"].as_str().unwrap().to_string())
        .collect()
}

fn rules_section(sp: &Spec) -> BTreeSet<String> {
    [&sp.section, &sp.a, &sp.b, &sp.c, &sp.decision]
        .iter()
        .map(|i| i.to_string())
        .collect()
}

#[test]
fn df8_brief_section_slug() {
    let (dir, sp, x1, _) = project_with_extras();
    let mut c = McpClient::start(dir.path());
    session(&mut c);
    // the repro: rdy_read takes the address, and rdy_brief takes it too
    let r = c.call("rdy_read", json!({"id": "spec#rules"})).ok();
    assert_eq!(r["id"], sp.section.to_string(), "{r}");
    let b = c.call("rdy_brief", json!({"path": "spec#rules"})).ok();
    let got = base_ids(&b);
    assert_eq!(got, rules_section(&sp), "{b}");
    assert!(
        !got.contains(&x1) && !got.contains(&sp.doc.to_string()),
        "{b}"
    );
    // a write that cites the token and updates a claim under Rules lands
    let token = b["base"]["token"].as_str().unwrap().to_string();
    let w = c.call(
        "rdy_write",
        json!({"brief": token, "ops": [{"op": "update", "id": sp.c.to_string(), "body": "C, by the agent."}]}),
    );
    assert!(w.is_ok(), "{w:?}");
}

#[test]
fn df8_an_ids_entry_with_a_slug_covers_the_section() {
    let (dir, sp, x1, _) = project_with_extras();
    let mut c = McpClient::start(dir.path());
    session(&mut c);
    let b = c.call("rdy_brief", json!({"ids": ["spec#rules"]})).ok();
    assert_eq!(base_ids(&b), rules_section(&sp), "{b}");
    // an entry without a # covers only itself, beside one with a slug
    let b = c
        .call(
            "rdy_brief",
            json!({"ids": [sp.doc.to_string(), "spec#rules", x1.clone()]}),
        )
        .ok();
    let mut want = rules_section(&sp);
    want.insert(sp.doc.to_string());
    want.insert(x1);
    assert_eq!(base_ids(&b), want, "{b}");
}

#[test]
fn df8_a_slug_after_a_folder_or_a_block_is_refused() {
    let (dir, sp, _, folder) = project_with_extras();
    let mut c = McpClient::start(dir.path());
    session(&mut c);
    let not_doc = "not a doc: #<heading> reads a section of a doc";
    let e = tool_error(c.call("rdy_brief", json!({"path": format!("{folder}#x")})));
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert!(e["message"].as_str().unwrap().contains(not_doc), "{e}");
    let e = tool_error(c.call("rdy_brief", json!({"ids": [format!("{}#x", sp.a)]})));
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert!(e["message"].as_str().unwrap().contains(not_doc), "{e}");
    let e = tool_error(c.call("rdy_brief", json!({"path": "spec#nope"})));
    assert_eq!(e["code"], "E_UNKNOWN_BLOCK", "{e}");
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .contains("no section #nope in "),
        "{e}"
    );
    let e = tool_error(c.call("rdy_brief", json!({"path": "spec#"})));
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .contains("a # needs a heading slug after it"),
        "{e}"
    );
}
