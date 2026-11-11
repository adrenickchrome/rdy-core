//! ui6 (D-Y9DNZ, D-KQ4CA, D-XR09R, D-ZWP7Q): `rdy_import {dir, view}` is the chat path of the
//! import. Its schema is closed to the two keys and the three optional Markdown options (m4bc5),
//! and carries the exact words. A writer's import of
//! a decision recorded as accepted elsewhere reaches the person as a card that shows Imported
//! after Policy, and `rdy_land` cannot land it: the person's relayed answer does.
use rdy_engine::{Rdy, Scope, Session, WriteRequest};
use rdy_mcp::params::parse_params;
use rdy_mcp::tools::ImportParams;
use rdy_model::{BlockId, Justification};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{land, parse_test_ops, writer};
use rdy_qa::mcp::{McpClient, Reply};
use rdy_qa::packs::QA_PACK_YAML;
use serde_json::{Value, json};
use std::path::Path;

/// The tool description of `rdy_import`.
const DESCRIPTION: &str = "Import a format-3 tree from dir into view. Needs a writer session. The tree's authors stay recorded, and imported_by names this session. Imported accepted or rejected decisions need a person's approval before they land. With md: true, dir is a Markdown source: dry_run previews it and writes nothing, and expected_source (the source_digest of a preview) refuses the import when the source changed.";
const DIR_HELP: &str = "The import directory, absolute or relative to the project directory.";
const VIEW_HELP: &str = "The view to import into. Import never writes canon.";
const MD_HELP: &str = "Read dir as a Markdown source, not a format-3 tree. Default false.";
const DRY_RUN_HELP: &str =
    "Preview the Markdown import and write nothing. Needs md: true. Default false.";
const EXPECTED_HELP: &str =
    "The source_digest of a preview. The import refuses when the source changed. Needs md: true.";

#[test]
fn ui6_import_tool_has_the_exact_closed_schema() {
    let s = Served::new();
    let mut c = McpClient::start_proxy(&s);
    let tools = c.tools();
    let tool = tools
        .iter()
        .find(|t| t["name"] == "rdy_import")
        .expect("rdy_import is listed");
    assert_eq!(tool["description"], DESCRIPTION);
    let schema = &tool["inputSchema"];
    let props = schema["properties"].as_object().unwrap();
    let mut keys: Vec<&str> = props.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["dir", "dry_run", "expected_source", "md", "view"],
        "no hidden relay key"
    );
    assert_eq!(props["dir"]["description"], DIR_HELP);
    assert_eq!(props["view"]["description"], VIEW_HELP);
    assert_eq!(props["md"]["description"], MD_HELP);
    assert_eq!(props["dry_run"]["description"], DRY_RUN_HELP);
    assert_eq!(props["expected_source"]["description"], EXPECTED_HELP);
    let mut required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    required.sort_unstable();
    assert_eq!(required, ["dir", "view"], "only these two are required");
    assert_eq!(schema["additionalProperties"], json!(false), "{schema}");
    // serde accepts each key the schema publishes, and refuses any other
    for key in &keys {
        let mut args = serde_json::Map::new();
        args.insert("dir".into(), json!("x"));
        args.insert("view".into(), json!("y"));
        let value = match *key {
            "md" | "dry_run" => json!(true),
            _ => json!("0".repeat(64)),
        };
        args.insert((*key).into(), value);
        assert!(
            parse_params::<ImportParams>("rdy_import", args).is_ok(),
            "{key}"
        );
    }
    for hidden in [
        "relayed_by",
        "evidence",
        "question",
        "session_ref",
        "card_hash",
    ] {
        let args = json!({"dir": "x", "view": "y", hidden: "z"});
        let e = parse_params::<ImportParams>("rdy_import", args.as_object().unwrap().clone())
            .expect_err(hidden);
        assert_eq!(e.code, "E_BAD_INPUT", "{hidden}");
    }
    // the dir is absolute or relative to the project directory
    let rel = s.dir.path().join("rel/tree");
    tree(
        &rel,
        "relative",
        "d-RE001",
        "s-RE001",
        &[("D-RE001", "proposed")],
    );
    session(&mut c, "agent:rel", "rl1", "writer");
    let v = c
        .call("rdy_import", json!({"dir": "rel/tree", "view": "rl1"}))
        .ok();
    assert_eq!(v["created"].as_object().unwrap().len(), 3, "{v:#}");
}

// ---------------------------------------------------------------- the card and the policy

/// One block row of an export tree's doc file.
fn block_yaml(
    id: &str,
    ty: &str,
    parent: Option<&str>,
    order: &str,
    fields: &[(&str, &str)],
) -> String {
    let mut s = format!("- id: {id}\n  type: {ty}\n");
    if let Some(p) = parent {
        s.push_str(&format!("  parent: {p}\n"));
    }
    s.push_str(&format!("  order: {order}\n  fields:\n"));
    for (k, v) in fields {
        s.push_str(&format!("    {k}: {v}\n"));
    }
    s
}

/// A format-3 export tree at `out`: doc `name` (id `doc`) with section `section` and one decision
/// per `(id, status)`, each `two_way` with basis `precedent`, all by agent:src (run s1) at 100.
pub(crate) fn tree(out: &Path, name: &str, doc: &str, section: &str, decisions: &[(&str, &str)]) {
    std::fs::create_dir_all(out.join("docs")).unwrap();
    std::fs::write(
        out.join("rdy-export.yaml"),
        "format: 3\npack: rdy-standard\nscope: work\n",
    )
    .unwrap();
    let mut docs = block_yaml(
        doc,
        "doc",
        None,
        "a1",
        &[
            ("doc_type", "overview"),
            ("key", name),
            ("title", "Imported"),
        ],
    );
    docs.push_str(&block_yaml(
        section,
        "section",
        Some(doc),
        "a1",
        &[("title", "Rulings")],
    ));
    for (n, (id, status)) in decisions.iter().enumerate() {
        let statement = format!("Decision {id} holds");
        docs.push_str(&block_yaml(
            id,
            "decision",
            Some(section),
            &format!("a{}", n + 1),
            &[
                ("authority_basis", "precedent"),
                ("needs_content", "false"),
                ("reversibility", "two_way"),
                ("statement", &statement),
                ("status", status),
            ],
        ));
    }
    std::fs::write(out.join("docs").join(format!("{name}.yaml")), docs).unwrap();
    let mut authors = String::new();
    for id in [doc, section]
        .into_iter()
        .chain(decisions.iter().map(|(id, _)| *id))
    {
        authors.push_str(&format!(
            "- id: {id}\n  actor: agent:src\n  run: s1\n  role: writer\n  at_ms: 100\n"
        ));
    }
    std::fs::write(out.join("authors.yaml"), authors).unwrap();
}

pub(crate) fn session(c: &mut McpClient, actor: &str, run: &str, role: &str) {
    c.call(
        "rdy_session",
        json!({"actor": actor, "run": run, "role": role}),
    )
    .ok();
}

/// The policy of one rule: agents `agent:*` may land `two_way` decisions in area `payments`
/// with basis `precedent`; a decision otherwise needs a person from `approvers`.
fn policy_fields() -> Value {
    json!({
        "key": "authority-policy",
        "agent_may_accept": [{
            "name": "decides",
            "actors": ["agent:*"],
            "lifecycle": "any",
            "justifications": ["implements", "editorial"],
            "decisions": "two_way",
            "two_way_areas": ["payments"],
            "basis": ["precedent"],
            "max_blocks": 20,
            "max_items": 20
        }],
        "roles": {"approvers": [rdy_qa::intake::PERSON]},
        "require": [{"when": "decision", "role": "approvers"}],
        "owners": []
    })
}

/// The served project, with pack `rdy-qa` and canon holding the policy of [`policy_fields`].
pub(crate) fn policy_served() -> Served {
    let s = Served::new();
    s.stop();
    let dir = s.dir.path();
    std::fs::write(dir.join(".rdy/packs/rdy-qa.yaml"), QA_PACK_YAML).unwrap();
    std::fs::write(
        dir.join(".rdy.yaml"),
        "pack: rdy-qa\nserve:\n  idle_timeout: 60\n",
    )
    .unwrap();
    let mut r = Rdy::open(dir).unwrap();
    let write_as = |r: &mut Rdy, s: &Session, ops: Value, j: Option<Justification>| {
        r.write(
            s,
            WriteRequest {
                view: "pol".into(),
                ops: parse_test_ops(ops).unwrap(),
                justification: j,
                message: String::new(),
            },
        )
        .unwrap_or_else(|e| panic!("write the policy: {e:?}"))
    };
    let w = write_as(
        &mut r,
        &writer(),
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "overview"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$d", "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}}
        ]),
        None,
    );
    let (doc, dec) = (w.created["$d"].clone(), w.created["$dec"].clone());
    write_as(
        &mut r,
        &writer(),
        json!([{"op": "create", "type": "authority_policy", "id": "$p", "parent": doc, "fields": policy_fields()}]),
        Some(Justification::Implements(vec![
            BlockId::parse(&dec).unwrap(),
        ])),
    );
    land(&mut r, "pol").unwrap();
    r.close().unwrap();
    s
}

/// The code, message and fix of a tool error.
fn refused(r: Reply) -> (String, String, String) {
    let Reply::ToolError(e) = r else {
        panic!("expected a tool error, got {r:?}")
    };
    let s = |k: &str| e[k].as_str().unwrap_or_default().to_string();
    (s("code"), s("message"), s("fix"))
}

/// The facts of a card as `name: value` lines.
fn lines(item: &Value) -> Vec<String> {
    item["facts"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|f| {
            format!(
                "{}: {}",
                f["name"].as_str().unwrap_or_default(),
                f["value"].as_str().unwrap_or_default()
            )
        })
        .collect()
}

#[test]
fn ui6_import_card_and_policy_survive_the_transport_loop() {
    let s = policy_served();
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("wallets");
    // doc `wallets` is in area payments: the policy's rule would accept an agent's decision there
    tree(
        &src,
        "wallets",
        "d-WA001",
        "s-WA001",
        &[("D-AC001", "accepted")],
    );
    let mut c = McpClient::start_proxy(&s);
    session(&mut c, "agent:a1", "a1", "writer");
    let imported = c
        .call(
            "rdy_import",
            json!({"dir": src.to_str().unwrap(), "view": "a1"}),
        )
        .ok();
    assert_eq!(
        imported["created"].as_object().unwrap().len(),
        3,
        "{imported:#}"
    );

    let proposed = c
        .call(
            "rdy_propose",
            json!({"view": "a1", "title": "Wallets", "summary": "Imports the wallets doc and its accepted ruling."}),
        )
        .ok();
    let pref = proposed["ref"]
        .as_str()
        .expect("the version's ref")
        .to_string();
    assert_eq!(proposed["ready"], json!(true), "{proposed:#}");

    // the card shows Imported after Policy, and the policy needs a person
    let queue = c.call("rdy_pending", json!({})).ok();
    let card = queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["ref"] == pref.as_str() && i["kind"] == "approve")
        .unwrap_or_else(|| panic!("an approve card for {pref}: {queue:#}"))
        .clone();
    let facts = lines(&card);
    let policy = facts
        .iter()
        .position(|l| l.starts_with("Policy: "))
        .unwrap_or_else(|| panic!("a Policy fact: {facts:#?}"));
    assert!(
        facts[policy]
            .ends_with("needs a person from approvers: it carries an imported decision status"),
        "{facts:#?}"
    );
    assert_eq!(
        facts[policy + 1],
        "Imported: 1 decision recorded as accepted or rejected elsewhere, by agent:a1",
        "Imported follows Policy: {facts:#?}"
    );

    // rdy_land refuses it, whoever asks
    let (code, message, _) = refused(c.call("rdy_land", json!({"proposal": pref})));
    assert_eq!(code, "E_POLICY_HUMAN");
    assert_eq!(
        message,
        format!(
            "the authority policy needs a person for {pref}: it carries an imported decision status"
        )
    );

    // the person's words, relayed with the hash the card shows, land it
    let words = |extra: Value| {
        let mut v = json!({
            "evidence": "Yes, land the wallets doc.",
            "question": "Shall I land the wallets doc and its accepted ruling?",
            "session_ref": "chat-6"
        });
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    };
    for _ in 0..8 {
        let queue = c.call("rdy_pending", json!({})).ok();
        let Some(item) = queue["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["kind"] == "confirm")
            .cloned()
        else {
            break;
        };
        c.call(
            "rdy_answer",
            words(json!({"ref": item["ref"], "option": "confirm", "card_hash": item["card_hash"]})),
        )
        .ok();
    }
    let queue = c.call("rdy_pending", json!({})).ok();
    let card = queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["ref"] == pref.as_str() && i["kind"] == "approve")
        .unwrap_or_else(|| panic!("the card for {pref} stands: {queue:#}"))
        .clone();
    assert!(
        lines(&card).iter().any(|l| l.starts_with("Imported: ")),
        "{card:#}"
    );
    let landed = c
        .call(
            "rdy_answer",
            words(json!({
                "ref": pref,
                "option": card["options"][0]["id"],
                "card_hash": card["card_hash"],
            })),
        )
        .ok();
    assert_eq!(landed["assurance"], "relayed", "{landed:#}");
    drop(c);
    // the ruling is in canon with the status the tree gave it, landed by the person's answer
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    let status = r
        .read()
        .block(&Scope::Canon, &BlockId::parse("D-AC001").unwrap())
        .unwrap()
        .field_str("status")
        .map(str::to_string);
    assert_eq!(status.as_deref(), Some("accepted"));
}
