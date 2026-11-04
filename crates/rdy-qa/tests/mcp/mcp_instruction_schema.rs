//! ain1 (D-4W1ZR, D-8X04F): over MCP, rdy_schema describes the instruction type and gives a
//! valid rdy_write example; an agent authors and reads an instruction with its tools. An `area:`
//! selector the pack lacks warns in the normal write result and keeps the instruction.
use rdy_mcp::tools::ERROR_ROWS;
use rdy_qa::mcp::{McpClient, Reply};
use rdy_qa::packs::qa_project;
use serde_json::{Value, json};

fn session(c: &mut McpClient) {
    let r = c.call(
        "rdy_session",
        json!({"actor": "agent:ai", "run": "ai1", "role": "writer"}),
    );
    assert!(r.code().is_none(), "{:?}", r.ok());
}

fn tool_error(r: Reply) -> Value {
    match r {
        Reply::ToolError(v) => v,
        other => panic!("expected a tool error, got {other:?}"),
    }
}

/// A draft doc `rules` and a proposed decision under it, written over MCP: their ids.
fn rules(c: &mut McpClient) -> (String, String) {
    let w = c
        .call(
            "rdy_write",
            json!({"ops": [{"op": "create", "type": "doc", "id": "$d",
                "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}}]}),
        )
        .ok();
    let doc = w["created"]["$d"].as_str().unwrap().to_string();
    let d = c
        .call(
            "rdy_decide",
            json!({"statement": "Run the tests before every proposal", "parent": doc}),
        )
        .ok();
    (doc, d["created"]["$decision"].as_str().unwrap().to_string())
}

/// `v` with every string equal to `from` replaced by `to`.
fn subst(v: &Value, from: &str, to: &str) -> Value {
    match v {
        Value::String(s) if s == from => json!(to),
        Value::Array(a) => Value::Array(a.iter().map(|x| subst(x, from, to)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), subst(x, from, to)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[test]
fn ain1_schema_example_creates_a_complete_instruction() {
    let (dir, r) = qa_project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    session(&mut c);
    let s = c
        .call("rdy_schema", json!({"block_type": "instruction"}))
        .ok();
    assert_eq!(s["type"], "instruction");
    assert_eq!(s["prefix"], "ai");
    assert_eq!(s["class"], "doc_asset");
    assert_eq!(s["traits"], json!(["normative"]));
    assert_eq!(s["body"], "required");
    assert_eq!(s["children"], json!([]));
    assert_eq!(
        s["fields"],
        json!({
            "applies_to": {"kind": "list", "required": true, "values": []},
            "locked": {"kind": "bool", "required": false, "values": []}
        })
    );
    let rule = s["instruction"].as_str().unwrap();
    for word in [
        "all, area:<name>, type:<name> or act:<write|decide|propose|relay|review|publish|redact>",
        "1 to 1024 selectors",
        "W_INSTRUCTION_SELECTOR",
        "decided_by",
        "locked",
        "no key",
        "never filters",
    ] {
        assert!(rule.contains(word), "{word}: {rule}");
    }
    let example = s["example"].clone();
    assert_eq!(example["op"], "create");
    assert_eq!(example["parent"], "<doc or section id>");
    assert_eq!(
        example["links"],
        json!({"decided_by": [{"target": "<decision id>"}]})
    );
    let body = example["body"].as_str().unwrap().to_string();
    assert_eq!(body.split("\n\n").count(), 3, "{body}");
    // the doc's required field, with an rdy_write example
    let req = &s["required_doc"];
    assert!(
        req["rule"].as_str().unwrap().contains("required: true"),
        "{req}"
    );
    assert_eq!(
        req["example"],
        json!({"op": "update", "id": "<doc id>", "fields": {"required": true}})
    );
    // the doc type lists the field
    let d = c.call("rdy_schema", json!({"block_type": "doc"})).ok();
    assert_eq!(
        d["fields"]["required"],
        json!({"kind": "bool", "required": false, "values": []})
    );
    // the selector warning has its row
    let all = c.call("rdy_schema", json!({})).ok();
    assert!(
        all["errors"]["W_INSTRUCTION_SELECTOR"]["when"]
            .as_str()
            .unwrap()
            .contains("area"),
        "{}",
        all["errors"]["W_INSTRUCTION_SELECTOR"]
    );
    assert!(
        ERROR_ROWS
            .iter()
            .any(|(code, _, _)| *code == "W_INSTRUCTION_SELECTOR")
    );

    // the example, with its two placeholders filled, is a valid write
    let (doc, decision) = rules(&mut c);
    let op = subst(
        &subst(&example, "<doc or section id>", &doc),
        "<decision id>",
        &decision,
    );
    let w = c.call("rdy_write", json!({"ops": [op]})).ok();
    assert_eq!(w["warnings"], json!([]), "{w}");
    let id = w["created"]["op0"].as_str().unwrap().to_string();
    assert!(id.starts_with("ai-"), "{id}");
    let set = subst(&req["example"], "<doc id>", &doc);
    c.call("rdy_write", json!({"ops": [set]})).ok();

    // read back through tools: every paragraph, the selectors, the reason and the doc field
    let ctx = c.call("rdy_context", json!({"id": id})).ok();
    assert_eq!(ctx["block_type"], "instruction", "{ctx}");
    assert_eq!(ctx["parent"], doc, "{ctx}");
    let yaml = c.call("rdy_read", json!({"id": id, "format": "yaml"})).ok();
    let text = yaml["text"].as_str().unwrap();
    for para in body.split("\n\n") {
        assert!(text.contains(para), "{para}: {text}");
    }
    assert!(text.contains(&decision), "{text}");
    let md = c
        .call("rdy_read", json!({"id": doc, "format": "markdown"}))
        .ok();
    let md = md["text"].as_str().unwrap();
    for para in body.split("\n\n") {
        assert!(md.contains(para), "{para}: {md}");
    }
    let doc_yaml = c
        .call("rdy_read", json!({"id": doc, "format": "yaml"}))
        .ok();
    assert!(
        doc_yaml["text"]
            .as_str()
            .unwrap()
            .contains("required: true"),
        "{doc_yaml}"
    );
    c.alive();
}

#[test]
fn ain1_unknown_area_warns_without_losing_the_instruction() {
    let (dir, r) = qa_project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    session(&mut c);
    let (doc, decision) = rules(&mut c);
    let warning = |id: &str, name: &str| {
        json!({"severity": "warning", "code": "W_INSTRUCTION_SELECTOR", "block": id,
            "message": format!("area:{name} matches nothing: the pack has no area {name}"),
            "fix": format!("define area {name} in the pack, or correct the selector")})
    };
    // create: one warning per unknown area, in selector order, once each; the instruction is
    // written with every selector
    let sels = json!([
        "area:payments",
        "area:billing",
        "type:claim",
        "area:billing",
        "area:ops"
    ]);
    let w = c
        .call(
            "rdy_write",
            json!({"ops": [{"op": "create", "type": "instruction", "id": "$i", "parent": doc,
                "fields": {"applies_to": sels}, "body": "Check the ledger.",
                "links": {"decided_by": [{"target": decision}]}}]}),
        )
        .ok();
    let id = w["created"]["$i"].as_str().unwrap().to_string();
    assert_eq!(
        w["warnings"],
        json!([warning(&id, "billing"), warning(&id, "ops")]),
        "{w}"
    );
    let ctx = c.call("rdy_context", json!({"id": id})).ok();
    assert_eq!(ctx["fields"]["applies_to"], sels, "{ctx}");

    // update: the body only, then the selectors; in source order across ops
    let w = c
        .call(
            "rdy_write",
            json!({"ops": [
                {"op": "update", "id": id, "body": "Check the ledger twice."},
                {"op": "create", "type": "instruction", "id": "$j", "parent": doc,
                 "fields": {"applies_to": ["area:audit"]}, "body": "Keep the audit trail.",
                 "links": {"decided_by": [{"target": decision}]}}
            ]}),
        )
        .ok();
    let j = w["created"]["$j"].as_str().unwrap().to_string();
    assert_eq!(
        w["warnings"],
        json!([
            warning(&id, "billing"),
            warning(&id, "ops"),
            warning(&j, "audit")
        ]),
        "{w}"
    );
    // a known area, and no area at all: no warning
    let w = c
        .call(
            "rdy_write",
            json!({"ops": [
                {"op": "update", "id": id, "fields": {"applies_to": ["area:payments", "area:identity"]}},
                {"op": "update", "id": j, "fields": {"applies_to": ["all"]}}
            ]}),
        )
        .ok();
    assert_eq!(w["warnings"], json!([]), "{w}");
    // an unknown area warns again on the update that writes it, and a move after it, which
    // changes no selector, does not
    let w = c
        .call(
            "rdy_write",
            json!({"ops": [{"op": "update", "id": j, "fields": {"applies_to": ["area:gone"]}}]}),
        )
        .ok();
    assert_eq!(w["warnings"], json!([warning(&j, "gone")]), "{w}");
    let w = c
        .call(
            "rdy_write",
            json!({"ops": [{"op": "move", "id": j, "parent": doc, "before": id}]}),
        )
        .ok();
    assert_eq!(w["warnings"], json!([]), "{w}");

    // a refused batch writes nothing: bodies, selectors and links stay as they were
    let before_i = c.call("rdy_context", json!({"id": id})).ok();
    let before_j = c.call("rdy_context", json!({"id": j})).ok();
    let e = tool_error(c.call(
        "rdy_write",
        json!({"ops": [
            {"op": "update", "id": id, "body": "Changed.", "fields": {"applies_to": ["area:billing"]}},
            {"op": "update", "id": j, "links": {"decided_by": []}},
            {"op": "create", "type": "instruction", "parent": doc,
             "fields": {"applies_to": ["act:land"]}, "body": "Bad.",
             "links": {"decided_by": [{"target": decision}]}}
        ]}),
    ));
    assert_eq!(e["code"], "E_VALIDATION", "{e}");
    assert_eq!(c.call("rdy_context", json!({"id": id})).ok(), before_i);
    assert_eq!(c.call("rdy_context", json!({"id": j})).ok(), before_j);
    c.alive();
}
