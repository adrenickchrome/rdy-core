//! rtc5 (FW-MH.rules, D-ZSRBN): the input of `rdy_rule`. The tool's published schema and the one
//! parser behind it close the same forms: the same keys, the same enums and the same bounds. The
//! parser is the engine's, so the daemon, the command line and both servers refuse alike.
use rdy_engine::rule_types::{ANSWER_KEYS, KEYS, RuleRequest};
use rdy_mcp::params::parse_params;
use rdy_mcp::tools::{RULE_DOC, RuleParams, checked_request};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn schema() -> Value {
    serde_json::to_value(schemars::schema_for!(RuleParams)).expect("a schema")
}

fn answer_schema(root: &Value) -> Value {
    let r = root["properties"]["answer"]["$ref"]
        .as_str()
        .unwrap_or_else(|| panic!("a reference in {}", root["properties"]["answer"]));
    let name = r.strip_prefix("#/$defs/").expect("a local reference");
    root["$defs"][name].clone()
}

fn keys(v: &Value) -> BTreeSet<String> {
    v["properties"]
        .as_object()
        .expect("properties")
        .keys()
        .cloned()
        .collect()
}

/// The refusal of `args` as the server gives it: parameters first, then the request.
fn refused(args: Value) -> (String, String, String) {
    let e = parse_params::<RuleParams>("rdy_rule", args.as_object().expect("an object").clone())
        .and_then(checked_request)
        .expect_err("a refusal");
    (e.code.to_string(), e.message.clone(), e.fix.clone())
}

fn accepted(args: Value) -> RuleRequest {
    parse_params::<RuleParams>("rdy_rule", args.as_object().expect("an object").clone())
        .and_then(checked_request)
        .unwrap_or_else(|e| panic!("{} {} ({args})", e.code, e.message))
}

fn add() -> Value {
    json!({"scope": "person", "op": "add", "text": "Answer in British English.",
        "why": "The person writes it.", "evidence": "Please always use British English."})
}

fn answer() -> Value {
    json!({"scope": "project", "answer": {"ref": "P-ABCDE@1", "option": "approve",
        "card_hash": "a".repeat(64), "evidence": "Yes, land it."}})
}

fn with(mut base: Value, key: &str, value: Value) -> Value {
    base[key] = value;
    base
}

#[test]
fn rtc5_schema_and_parser_share_both_closed_forms() {
    let root = schema();
    // the schema is one closed object with the engine's keys, and the answer is closed likewise
    assert_eq!(root["additionalProperties"], json!(false));
    assert_eq!(keys(&root), KEYS.iter().map(|k| k.to_string()).collect());
    assert_eq!(root["required"], json!(["scope"]));
    let a = answer_schema(&root);
    assert_eq!(a["additionalProperties"], json!(false));
    assert_eq!(
        keys(&a),
        ANSWER_KEYS.iter().map(|k| k.to_string()).collect()
    );
    let mut required: Vec<&str> = a["required"]
        .as_array()
        .expect("required")
        .iter()
        .map(|k| k.as_str().unwrap())
        .collect();
    required.sort_unstable();
    assert_eq!(required, ["card_hash", "evidence", "option", "ref"]);
    assert!(!RULE_DOC.contains("kungfu"));
    // no property offers a null, because the parser refuses one for every key
    assert!(!root.to_string().contains("\"null\""), "{root}");

    // the enums are the parser's choices, word for word
    assert_eq!(
        root["properties"]["scope"]["enum"],
        json!(["person", "project"])
    );
    assert_eq!(
        root["properties"]["op"]["enum"],
        json!(["add", "edit", "remove"])
    );
    for (key, op) in [("scope", "bogus"), ("op", "bogus")] {
        let args = if key == "scope" {
            with(add(), "scope", json!(op))
        } else {
            with(add(), "op", json!(op))
        };
        let (code, message, fix) = refused(args);
        assert_eq!(code, "E_BAD_INPUT");
        let want = root["properties"][key]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap())
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(message, format!("rdy_rule: \"{key}\" must be {want}"));
        assert_eq!(fix, format!("pass one of: {want}"));
    }

    // both forms parse
    assert!(matches!(accepted(add()), RuleRequest::Add(_)));
    assert!(matches!(accepted(answer()), RuleRequest::Answer(_)));
    // a top-level `kungfu` is the check's, never the rule's
    assert!(matches!(
        accepted(with(add(), "kungfu", json!("kf-00000000"))),
        RuleRequest::Add(_)
    ));

    // a close key gets the nearest key, at the top and inside the answer
    let (code, message, fix) = refused(with(add(), "evidnce", json!("x")));
    assert_eq!(code, "E_BAD_INPUT");
    assert_eq!(message, "rdy_rule: unknown parameter \"evidnce\"");
    assert!(fix.starts_with("did you mean \"evidence\"?"), "{fix}");
    let mut nested = answer();
    nested["answer"]["optoin"] = json!("approve");
    let (code, message, fix) = refused(nested);
    assert_eq!(code, "E_BAD_INPUT");
    assert_eq!(message, "rdy_rule: unknown parameter \"answer.optoin\"");
    assert!(fix.starts_with("did you mean \"option\"?"), "{fix}");

    // a null is wrong for every supplied key, top-level and nested
    for key in KEYS {
        let (code, message, _) = refused(with(add(), key, Value::Null));
        assert_eq!(code, "E_BAD_INPUT", "{key}");
        assert!(message.contains(&format!("\"{key}\"")), "{key}: {message}");
        assert!(message.ends_with("not null"), "{key}: {message}");
    }
    for key in ANSWER_KEYS {
        let mut nulled = answer();
        nulled["answer"][key] = Value::Null;
        let (code, message, _) = refused(nulled);
        assert_eq!(code, "E_BAD_INPUT", "{key}");
        assert!(
            message.contains(&format!("\"answer.{key}\"")),
            "{key}: {message}"
        );
    }

    // each character bound in the schema is the parser's: the longest value passes, one more is
    // refused
    for key in ["why", "evidence", "question", "session_ref"] {
        let max = root["properties"][key]["maxLength"]
            .as_u64()
            .unwrap_or_else(|| panic!("{key} has no maxLength"));
        let long = |n: u64| "é".repeat(n as usize);
        accepted(with(add(), key, json!(long(max))));
        let (code, ..) = refused(with(add(), key, json!(long(max + 1))));
        assert!(code == "E_BAD_INPUT" || code == "E_LIMIT", "{key}: {code}");
    }
    for key in ["question", "evidence"] {
        let max = a["properties"][key]["maxLength"].as_u64().unwrap();
        let mut ok = answer();
        ok["answer"][key] = json!("é".repeat(max as usize));
        accepted(ok);
        let mut over = answer();
        over["answer"][key] = json!("é".repeat(max as usize + 1));
        refused(over);
    }
    // the byte bounds: the text, the id and the ruleset
    accepted(with(add(), "text", json!("x".repeat(65_536))));
    let (code, message, fix) = refused(with(add(), "text", json!("x".repeat(65_537))));
    assert_eq!(
        (code.as_str(), message.as_str(), fix.as_str()),
        (
            "E_LIMIT",
            "rdy_rule: \"text\" exceeds 65536 bytes",
            "use at most 65536 bytes for \"text\""
        )
    );
    let id_max = root["properties"]["id"]["maxLength"].as_u64().unwrap();
    assert_eq!(id_max, 96);
    let (code, ..) = refused(
        json!({"scope": "person", "op": "remove", "id": "a".repeat(97),
        "why": "x", "evidence": "x"}),
    );
    assert_eq!(code, "E_LIMIT");
    let (code, ..) = refused(with(add(), "ruleset", json!("r".repeat(1_025))));
    assert_eq!(code, "E_LIMIT");
    // the list bounds
    let items = &root["properties"]["applies_to"];
    assert_eq!(items["minItems"], json!(1));
    assert_eq!(items["maxItems"], json!(1024));
    accepted(with(add(), "applies_to", json!(vec!["all"; 1024])));
    let (code, ..) = refused(with(add(), "applies_to", json!(vec!["all"; 1025])));
    assert_eq!(code, "E_LIMIT");
    let (code, ..) = refused(with(add(), "applies_to", json!([])));
    assert!(code == "E_BAD_INPUT" || code == "E_LIMIT", "{code}");
    // the hash is a full lowercase hash, as its schema pattern says
    assert_eq!(a["properties"]["card_hash"]["pattern"], "^[0-9a-f]{64}$");
    let mut short = answer();
    short["answer"]["card_hash"] = json!("a".repeat(63));
    let (code, message, _) = refused(short);
    assert_eq!(code, "E_BAD_INPUT");
    assert_eq!(
        message,
        "rdy_rule: \"answer.card_hash\" must be a full lowercase hash"
    );
    // the forms stay apart: the answer takes no mutation key
    let (code, message, _) = refused(with(answer(), "text", json!("x")));
    assert_eq!(code, "E_BAD_INPUT");
    assert!(message.contains("\"text\" is not allowed"), "{message}");
}

/// D-ZSRBN: a rule the person already stated is saved on their words; the tool asks first only for
/// an inferred rule or an unsure scope, and shows wording that goes beyond their words.
#[test]
fn wx32_the_tool_doc_saves_a_stated_rule_and_asks_only_for_an_inferred_one() {
    assert!(RULE_DOC.contains("Save a rule the person already stated, on their words"));
    assert!(
        RULE_DOC.contains(
            "Ask first only for a rule you inferred or when you are unsure of the scope."
        )
    );
    assert!(RULE_DOC.contains("Show the wording first when it goes beyond their words."));
    assert!(!RULE_DOC.contains("ask the person first"));
}
