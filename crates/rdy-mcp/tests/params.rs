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

/// ls7: `rdy_site` takes `scope` and nothing else; an unknown key names the parameter it was
/// close to.
#[test]
fn ls7_rdy_site_refuses_an_unknown_key() {
    let (code, message, _) = refusal::<SiteParams>("rdy_site", json!({"scopes": "x"}));
    assert_eq!(code, "E_BAD_INPUT");
    assert!(message.contains("scope"), "{message}");
}

/// dl7: `rdy_dump` takes `out` and nothing else.
#[test]
fn dl7_rdy_dump_refuses_an_unknown_key() {
    let (code, message, _) = refusal::<DumpParams>("rdy_dump", json!({"outs": "x"}));
    assert_eq!(code, "E_BAD_INPUT");
    assert!(message.contains("out"), "{message}");
}

/// dl7: `rdy_load` takes `file` and `dir` and nothing else.
#[test]
fn dl7_rdy_load_refuses_an_unknown_key() {
    let (code, message, _) = refusal::<LoadParams>("rdy_load", json!({"file": "x", "dirs": "y"}));
    assert_eq!(code, "E_BAD_INPUT");
    assert!(message.contains("dir"), "{message}");
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
            "statement".into(),
            refusal::<DecideParams>("rdy_decide", json!({"statement": null})),
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
            "rdy_decide: parameter \"statement\" must be a string, not null",
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
        (p.parent.as_deref(), p.kind, p.options.map(|o| o.len())),
        (Some("s-1"), None, Some(2))
    );
    assert!(parse_params::<TagsParams>("rdy_tags", serde_json::Map::new()).is_ok());
}

/// Every key of `P`'s published input schema that serde refuses as an unknown field. serde reads
/// a field identifier before its value, so `{key: null}` names an unknown field first whatever
/// the type of the value is. Any other error text (a wrong type, a missing field) is fine here.
fn schema_keys_serde_refuses<P>() -> Vec<String>
where
    P: serde::de::DeserializeOwned + schemars::JsonSchema + 'static,
{
    let schema = rmcp::handler::server::common::schema_for_input::<P>().expect("an input schema");
    let props = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut refused = Vec::new();
    for key in props.keys() {
        let probe = json!({ key.as_str(): null });
        if let Err(e) = P::deserialize(&probe)
            && e.to_string().starts_with("unknown field")
        {
            refused.push(format!("{}: {e}", std::any::type_name::<P>()));
        }
    }
    refused
}

#[test]
fn hd7_serde_accepts_every_key_each_params_schema_publishes() {
    use rdy_mcp::alt_tools::{AltMemberParams, AltParams, AltPickParams};
    use rdy_mcp::entity_tools::AcceptParams;
    use rdy_mcp::intent_tools::IntentParams;
    use rdy_mcp::search_tools::SearchParams;
    use rdy_mcp::view_tools::LogParams;
    let mut refused = Vec::new();
    let mut checked: Vec<&str> = Vec::new();
    macro_rules! check {
        ($($t:ident),* $(,)?) => { $(
            refused.extend(schema_keys_serde_refuses::<$t>());
            checked.push(stringify!($t));
        )* };
    }
    // The 35 `*Params` types, nested ones included (`EditorialParams`, `AltMemberParams`,
    // `AltPickParams`, `DismissParams`, `DecideParams`, `MentionParams`, `AcceptAdviceParams`).
    check!(
        AcceptAdviceParams,
        AcceptParams,
        AltMemberParams,
        AltPickParams,
        AltParams,
        AssetParams,
        BriefParams,
        CoverageParams,
        DecideParams,
        DismissParams,
        DumpParams,
        EditorialParams,
        FindingParams,
        IdParams,
        InboxParams,
        IntentParams,
        LoadParams,
        LogParams,
        MentionParams,
        ProposeParams,
        ReadParams,
        ResolveParams,
        ReviewNextParams,
        ReviewSubmitParams,
        RevokeParams,
        ScopeParams,
        SchemaParams,
        SearchParams,
        SessionParams,
        SiteParams,
        SupersedeParams,
        TagsParams,
        TensionsParams,
        ViewParams,
        WriteParams,
    );
    assert!(
        refused.is_empty(),
        "a documented parameter is refused as an unknown field: {refused:#?}"
    );
    // every `struct *Params` in the crate's source is in the list above
    let declared = params_structs(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
    let unchecked: Vec<&String> = declared
        .iter()
        .filter(|n| !checked.contains(&n.as_str()))
        .collect();
    assert!(
        unchecked.is_empty(),
        "add these *Params types to the check above: {unchecked:?}"
    );
    assert_eq!(declared.len(), checked.len(), "{declared:?}");
}

/// The name of every `struct <Name>Params` declared in the `.rs` files under `dir`.
fn params_structs(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(params_structs(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            for rest in text.split("struct ").skip(1) {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if name.ends_with("Params") {
                    out.push(name);
                }
            }
        }
    }
    out.sort();
    out
}

#[test]
fn hd7_the_serde_probe_catches_a_schema_key_serde_does_not_know() {
    // The guard above must be able to fail: here the schema says `shown` and serde says `hidden`.
    #[derive(serde::Deserialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Mismatch {
        #[serde(rename = "hidden")]
        #[schemars(rename = "shown")]
        field: Option<String>,
    }
    let refused = schema_keys_serde_refuses::<Mismatch>();
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(refused[0].contains("unknown field `shown`"), "{refused:?}");
}

/// The keys serde's `deny_unknown_fields` lists for `T`: the "expected one of" part of the error
/// an unknown key gives.
fn serde_keys<T: serde::de::DeserializeOwned>() -> Vec<String> {
    let probe = json!({ "__no_such_key__": null });
    let Err(e) = T::deserialize(&probe) else {
        panic!("{} accepts an unknown key", std::any::type_name::<T>());
    };
    let text = e.to_string();
    let (_, list) = text
        .split_once("expected one of ")
        .or_else(|| text.split_once("expected "))
        .unwrap_or_else(|| panic!("no key list in {text:?}"));
    let mut keys: Vec<String> = list
        .split(", ")
        .filter_map(|k| k.trim().strip_prefix('`')?.split('`').next())
        .map(str::to_string)
        .collect();
    keys.sort();
    keys
}

/// Final review fix round 1 (Minor 10): `InboxParams` copies the engine's `InboxRequest` by hand,
/// so every request key must be a published key of the tool, and the tool adds only `person`.
#[test]
fn ib7_every_inbox_request_key_is_an_inbox_params_key() {
    let schema =
        rmcp::handler::server::common::schema_for_input::<InboxParams>().expect("an input schema");
    let mut params: Vec<String> = schema["properties"]
        .as_object()
        .expect("properties")
        .keys()
        .filter(|k| k.as_str() != "person")
        .cloned()
        .collect();
    params.sort();
    let request = serde_keys::<rdy_engine::inbox::api::InboxRequest>();
    assert_eq!(request.len(), 3, "{request:?}");
    assert_eq!(request, params);
}
