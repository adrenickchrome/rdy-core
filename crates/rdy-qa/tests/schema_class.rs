//! tcls1: every block type is a doc asset or supporting (Q4, FW-D10); a child pack keeps or sets
//! the class; rationale may sit under a decision (FW-F4); reflects is refused by class; rdy pack
//! check and rdy_schema show the class.
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_mcp::tools::*;
use rdy_model::{Block, BlockId, FieldValue};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_schema::{PackSet, REFLECTS_FIELD, TypeClass, validate_block};
use serde_json::json;

fn standard() -> rdy_schema::Pack {
    PackSet::with_builtin().resolve("rdy-standard").unwrap()
}

#[test]
fn tcls1_the_standard_pack_classes_its_types() {
    let p = standard();
    for t in ["decision", "question", "rationale"] {
        assert_eq!(p.class_of(t), TypeClass::Supporting, "{t}");
    }
    for t in [
        "doc", "folder", "section", "prose", "claim", "note", "figure", "table", "code",
    ] {
        assert_eq!(p.class_of(t), TypeClass::DocAsset, "{t}");
    }
    assert_eq!(p.class_of("no-such-type"), TypeClass::DocAsset);
    assert_eq!(p.class_of_prefix("D"), Some(TypeClass::Supporting));
    assert_eq!(p.class_of_prefix("ra"), Some(TypeClass::Supporting));
    assert_eq!(p.class_of_prefix("c"), Some(TypeClass::DocAsset));
    assert_eq!(p.class_of_prefix("zz"), None);
    assert_eq!(TypeClass::Supporting.as_str(), "supporting");
    assert_eq!(TypeClass::DocAsset.as_str(), "doc_asset");
}

#[test]
fn tcls1_a_child_pack_keeps_or_sets_the_class_and_engine_types_stay_supporting() {
    let mut set = PackSet::with_builtin();
    set.add_yaml(
        "name: child\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  rationale: { prefix: ra, traits: [informative], body: required }\n  note: { prefix: n, traits: [informative], body: required, class: supporting }\n  memo: { prefix: mo, traits: [informative], body: required }\n  decision: { prefix: D, class: doc_asset }\n",
    )
    .unwrap();
    let p = set.resolve("child").unwrap();
    assert_eq!(
        p.class_of("rationale"),
        TypeClass::Supporting,
        "kept from the parent"
    );
    assert_eq!(
        p.class_of("note"),
        TypeClass::Supporting,
        "set by the child"
    );
    assert_eq!(p.class_of("memo"), TypeClass::DocAsset, "the default");
    assert_eq!(
        p.class_of("decision"),
        TypeClass::Supporting,
        "the engine forces it"
    );
    let e = set.add_yaml(
        "name: bad\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  memo: { prefix: mo, class: maybe }\n",
    );
    assert!(e.is_err(), "an unknown class is refused: {e:?}");
}

#[test]
fn tcls1_rationale_may_sit_under_a_decision() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "work");
    let d = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: section,
                statement: "Keep it".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let decision = d.created["$decision"].clone();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "rationale", "parent": decision, "body": "Because."}]),
    )
    .unwrap();
    let e = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "prose", "parent": decision, "body": "No."}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_VALIDATION, "{e}");
}

#[test]
fn tcls1_reflects_is_refused_on_a_type_classed_supporting() {
    let mut set = PackSet::with_builtin();
    set.add_yaml(
        "name: c2\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  memo: { prefix: mo, body: required, class: supporting }\n",
    )
    .unwrap();
    let p = set.resolve("c2").unwrap();
    let b = Block {
        id: BlockId::parse("mo-AAAAA").unwrap(),
        block_type: "memo".into(),
        parent: None,
        order: "a1".into(),
        fields: [(
            REFLECTS_FIELD.to_string(),
            FieldValue::List(vec![FieldValue::Str("src/*.rs".into())]),
        )]
        .into_iter()
        .collect(),
        body: "x".into(),
        links: Default::default(),
        pins: Default::default(),
    };
    let issues = validate_block(&p, &b, None);
    assert!(
        issues
            .iter()
            .any(|i| i.code == "E_FIELD_UNKNOWN" && i.fix.contains("Implements")),
        "{issues:?}"
    );
}

#[test]
fn tcls1_pack_check_and_rdy_schema_show_the_class() {
    let s = Served::new();
    let (ok, v) = s.json(&["--json", "pack", "check"]);
    assert!(ok, "{v}");
    assert_eq!(v["classes"]["rationale"], "supporting", "{v}");
    assert_eq!(v["classes"]["claim"], "doc_asset", "{v}");
    assert!(v["types"].is_number(), "types stays the count: {v}");
    let out = s.rdy(&["pack", "check"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.starts_with("pack rdy-standard "), "{text}");
    assert!(text.contains("\n  rationale  supporting"), "{text}");
    assert!(text.contains("\n  claim  doc_asset"), "{text}");
    drop(s);

    let dir = tempfile::tempdir().unwrap();
    let rdy = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let mut st = McpState::new(std::sync::Arc::new(Local::new(rdy)));
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:x", "run": "r1", "role": "writer"})).unwrap(),
    )
    .unwrap();
    let all = rdy_schema(&mut st, serde_json::from_value(json!({})).unwrap()).unwrap();
    let rationale = all["types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["type"] == "rationale")
        .cloned()
        .unwrap();
    assert_eq!(rationale["class"], "supporting", "{rationale}");
    let one = rdy_schema(
        &mut st,
        serde_json::from_value(json!({"block_type": "decision"})).unwrap(),
    )
    .unwrap();
    assert_eq!(one["class"], "supporting", "{one}");
}
