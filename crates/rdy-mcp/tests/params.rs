//! mparm1: an MCP parameter error is an rdy E_BAD_INPUT that names the parameter and the shape
//! the tool's input schema asks for (the parser, without a server).
use rdy_mcp::params::parse_params;
use rdy_mcp::tools::*;
use rdy_mcp::view_tools::ViewParams;
use serde_json::{Value, json};

fn refusal<P>(tool: &str, args: Value) -> (String, String, String)
where
    P: serde::de::DeserializeOwned + schemars::JsonSchema + std::fmt::Debug + 'static,
{
    let e = parse_params::<P>(tool, args.as_object().expect("an object").clone())
        .expect_err("a refusal");
    (e.code.to_string(), e.message.clone(), e.fix.clone())
}

fn bad(message: &str, fix: &str) -> (String, String, String) {
    ("E_BAD_INPUT".into(), message.into(), fix.into())
}

#[test]
fn mparm1_a_wrong_type_names_the_parameter_and_its_shape() {
    assert_eq!(
        refusal::<ProposeParams>("rdy_propose", json!({"title": ["a", "b"]})),
        bad(
            "rdy_propose: parameter \"title\" must be a string, not a list",
            "pass \"title\" as a string"
        )
    );
}

#[test]
fn mparm1_a_missing_parameter_is_named() {
    assert_eq!(
        refusal::<DecideParams>("rdy_decide", json!({"parent": "s-1"})),
        bad(
            "rdy_decide: parameter \"statement\" is required",
            "pass \"statement\" as a string"
        )
    );
    assert_eq!(
        refusal::<SessionParams>("rdy_session", json!({})),
        bad(
            "rdy_session: parameter \"actor\" is required",
            "pass \"actor\" as a string"
        ),
        "no arguments at all names the first required parameter"
    );
}

#[test]
fn mparm1_a_wrong_type_wins_over_a_missing_parameter() {
    assert_eq!(
        refusal::<SessionParams>("rdy_session", json!({"actor": 7})),
        bad(
            "rdy_session: parameter \"actor\" must be a string, not a number",
            "pass \"actor\" as a string"
        )
    );
}

#[test]
fn mparm1_nested_and_list_paths_point_inside() {
    assert_eq!(
        refusal::<SupersedeParams>(
            "rdy_supersede",
            json!({"old": "D-AAAAA", "decision": {"parent": "s-1", "statement": 5}})
        ),
        bad(
            "rdy_supersede: parameter \"decision.statement\" must be a string, not a number",
            "pass \"decision.statement\" as a string"
        )
    );
    assert_eq!(
        refusal::<SupersedeParams>(
            "rdy_supersede",
            json!({"old": "D-AAAAA", "decision": {"parent": "s-1"}})
        ),
        bad(
            "rdy_supersede: parameter \"decision.statement\" is required",
            "pass \"decision.statement\" as a string"
        )
    );
    assert_eq!(
        refusal::<WriteParams>(
            "rdy_write",
            json!({"ops": [], "editorial": {"class": 5, "reason": "x"}})
        ),
        bad(
            "rdy_write: parameter \"editorial.class\" must be a string, not a number",
            "pass \"editorial.class\" as a string"
        )
    );
    assert_eq!(
        refusal::<BriefParams>("rdy_brief", json!({"ids": ["c-AAAAA", 3]})),
        bad(
            "rdy_brief: parameter \"ids[1]\" must be a string, not a number",
            "pass \"ids[1]\" as a string"
        )
    );
}

#[test]
fn mparm1_shapes_read_like_the_schema() {
    let cases: Vec<(String, (String, String, String))> = vec![
        (
            "depth".into(),
            refusal::<IdParams>("rdy_impact", json!({"id": "x", "depth": -1})),
        ),
        (
            "changes".into(),
            refusal::<ViewParams>("rdy_view", json!({"op": "list", "changes": "c1"})),
        ),
        (
            "editorial".into(),
            refusal::<WriteParams>("rdy_write", json!({"ops": [], "editorial": "typo"})),
        ),
        (
            "pin".into(),
            refusal::<ViewParams>("rdy_view", json!({"op": "list", "pin": "yes"})),
        ),
        (
            "ops".into(),
            refusal::<WriteParams>("rdy_write", json!({"ops": "x"})),
        ),
        (
            "parent".into(),
            refusal::<DecideParams>("rdy_decide", json!({"parent": null, "statement": "x"})),
        ),
    ];
    let messages: Vec<&str> = cases.iter().map(|(_, (_, m, _))| m.as_str()).collect();
    assert_eq!(
        messages,
        vec![
            "rdy_impact: parameter \"depth\" must be a non-negative integer, not -1",
            "rdy_view: parameter \"changes\" must be a list of strings, not a string",
            "rdy_write: parameter \"editorial\" must be an object, not a string",
            "rdy_view: parameter \"pin\" must be a boolean, not a string",
            "rdy_write: parameter \"ops\" must be a list, not a string",
            "rdy_decide: parameter \"parent\" must be a string, not null",
        ]
    );
    for (param, (code, _, fix)) in &cases {
        assert_eq!(code, "E_BAD_INPUT");
        assert!(fix.starts_with(&format!("pass \"{param}\" as ")), "{fix}");
    }
}

#[test]
fn mparm1_an_unlocatable_failure_falls_back_to_the_serde_detail() {
    assert_eq!(
        refusal::<IdParams>("rdy_impact", json!({"id": "x", "depth": 5_000_000_000u64})),
        bad(
            "rdy_impact: the parameters do not parse: invalid value: integer `5000000000`, expected u32",
            "check each parameter against the tool's input schema (tools/list)"
        )
    );
}

#[test]
fn mparm1_valid_parameters_still_parse() {
    // guard: the success path is unchanged, null still means "absent" for an optional parameter
    let p = parse_params::<DecideParams>(
        "rdy_decide",
        json!({"parent": "s-1", "statement": "x", "kind": null, "options": ["a", "b"]})
            .as_object()
            .unwrap()
            .clone(),
    )
    .unwrap();
    assert_eq!(
        (p.parent.as_str(), p.kind, p.options.map(|o| o.len())),
        ("s-1", None, Some(2))
    );
    assert!(parse_params::<TagsParams>("rdy_tags", serde_json::Map::new()).is_ok());
}
