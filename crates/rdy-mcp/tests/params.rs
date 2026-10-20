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

/// ec12: `rdy_clone` takes `dir`, `file` and `fork` and nothing else.
#[test]
fn ec12_rdy_clone_refuses_an_unknown_key() {
    let (code, message, _) = refusal::<CloneParams>("rdy_clone", json!({"dirs": "x"}));
    assert_eq!(code, "E_BAD_INPUT");
    assert!(message.contains("dir"), "{message}");
    let (code, message, _) =
        refusal::<CloneParams>("rdy_clone", json!({"dir": "x", "forks": true}));
    assert_eq!(code, "E_BAD_INPUT");
    assert!(message.contains("fork"), "{message}");
}

/// ec11: `rdy_export` takes `out`, `format`, `layout` and `scope` and nothing else; `format` and
/// `layout` are closed sets.
#[test]
fn ec11_rdy_export_refuses_an_unknown_key() {
    let (code, message, _) = refusal::<ExportParams>(
        "rdy_export",
        json!({"out": "x", "format": "md", "layouts": "mkdocs"}),
    );
    assert_eq!(code, "E_BAD_INPUT");
    assert!(message.contains("layout"), "{message}");
    let (code, message, _) =
        refusal::<ExportParams>("rdy_export", json!({"out": "x", "format": "docx"}));
    assert_eq!(code, "E_BAD_INPUT");
    assert!(message.contains("format"), "{message}");
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
    // The 54 `*Params` types, nested ones included (`EditorialParams`, `AltMemberParams`,
    // `AltPickParams`, `DismissParams`, `DecideParams`, `MentionParams`, `AcceptAdviceParams`,
    // `RelayParams`, `RecommendedParams`).
    check!(
        AcceptAdviceParams,
        AcceptParams,
        AltMemberParams,
        AltPickParams,
        AltParams,
        AnswerParams,
        AssetParams,
        AuthParams,
        BriefParams,
        CloneParams,
        CoverageParams,
        DecideParams,
        DismissParams,
        DumpParams,
        EditorialParams,
        ExportParams,
        FindingParams,
        IdParams,
        InboxParams,
        IntentParams,
        KeyBindParams,
        KeyRetireParams,
        KeyRevokeParams,
        KeyShowParams,
        LoadParams,
        LogParams,
        MentionParams,
        PendingParams,
        ProposeParams,
        PublicationCheckParams,
        PublishParams,
        ReadParams,
        RecommendedParams,
        RedactParams,
        RelayParams,
        RemoteParams,
        RenderParams,
        ResolveParams,
        ReviewNextParams,
        ReviewSubmitParams,
        RevokeParams,
        ScopeParams,
        SchemaParams,
        SearchParams,
        SessionParams,
        SignParams,
        SiteParams,
        StreamCheckParams,
        SupersedeParams,
        TagParams,
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

/// rdx12: a project with one secret prose block in the agent's view `r1`, and the agent's MCP
/// state; returns the block's id.
fn redact_state() -> (tempfile::TempDir, McpState, String) {
    let dir = tempfile::tempdir().unwrap();
    let rdy = rdy_engine::Rdy::init(dir.path(), "rdy-standard").unwrap();
    let mut st = McpState::new(std::sync::Arc::new(rdy_engine::engine::Local::new(rdy)));
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:a", "run": "r1", "role": "writer"})).unwrap(),
    )
    .unwrap();
    let w = rdy_write(
        &mut st,
        serde_json::from_value(json!({"ops": [
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Doc", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "The launch code is ZEBRA-7731."}
        ]}))
        .unwrap(),
    )
    .unwrap();
    let id = w["created"]["$p"].as_str().unwrap().to_string();
    (dir, st, id)
}

fn redact_args(v: Value) -> RedactParams {
    RedactParams(v.as_object().expect("an object").clone())
}

/// The keys of rdy_redact, in the interface's order (plan Redaction).
const REDACT_KEYS: &str = "targets, reason_class, reason, dry_run, view, max_tokens";

/// rdx12: an unknown key gets the shared did-you-mean with context `redact` and the interface's
/// key order (the daemon op's own text); an approval switch is no key at all; a wrong reason and
/// a selector outside the closed grammar get the engine's exact refusal.
#[test]
fn rdx12_unknown_keys_and_selectors_fail_with_exact_text() {
    let (_d, mut st, id) = redact_state();
    // near keys: the nearest key, every key in the interface's order
    for (near, want) in [
        ("dryrun", "dry_run"),
        ("target", "targets"),
        ("reason_clas", "reason_class"),
        ("max_token", "max_tokens"),
        ("views", "view"),
    ] {
        let mut args = json!({"targets": [id], "reason_class": "secret", "reason": "x"});
        args[near] = json!(true);
        let e = parse_redact(args.as_object().unwrap().clone()).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                "E_BAD_INPUT",
                format!("redact: unknown field \"{near}\"").as_str(),
                format!("did you mean \"{want}\"? the fields are {REDACT_KEYS}").as_str()
            ),
            "{near}"
        );
        // the tool gives the same refusal, and before it asks for a session
        let mut fresh = McpState::new(std::sync::Arc::clone(&st.engine));
        let t = rdy_redact(&mut fresh, redact_args(args)).unwrap_err();
        assert_eq!((t.code, t.message, t.fix), (e.code, e.message, e.fix));
    }
    // no approval or policy switch exists: each is an unknown key
    for forbidden in ["force", "yes", "policy", "approve", "apply"] {
        let mut args = json!({"targets": [id], "reason_class": "secret", "reason": "x"});
        args[forbidden] = json!(true);
        let e = rdy_redact(&mut st, redact_args(args)).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{forbidden}");
        assert_eq!(
            e.message,
            format!("redact: unknown field \"{forbidden}\""),
            "{forbidden}"
        );
        assert!(
            e.fix.ends_with(&format!("the fields are {REDACT_KEYS}")),
            "{forbidden}: {}",
            e.fix
        );
    }
    // a wrong-typed key is the tool parameter refusal every MCP tool gives
    let e = parse_redact(
        json!({"targets": [id], "reason_class": "secret", "reason": "x", "dry_run": "yes"})
            .as_object()
            .unwrap()
            .clone(),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_BAD_INPUT",
            "rdy_redact: parameter \"dry_run\" must be a boolean, not a string",
            "pass \"dry_run\" as a boolean"
        )
    );
    let e = parse_redact(json!({"targets": [id]}).as_object().unwrap().clone()).unwrap_err();
    assert_eq!(
        e.message, "rdy_redact: parameter \"reason_class\" is required",
        "{e}"
    );
    // an unknown reason class, and a reason that trims to nothing: the engine's refusal
    for (class, reason) in [("other", "x"), ("Secret", "x"), ("secret", "  \n ")] {
        let e = rdy_redact(
            &mut st,
            redact_args(
                json!({"targets": [id], "reason_class": class, "reason": reason, "dry_run": true}),
            ),
        )
        .unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                "E_REDACTION_REASON",
                "The redaction needs a reason and a reason class.",
                "Use secret, personal, legal, or retraction, with a reason of 1 to 4096 UTF-8 bytes."
            ),
            "{class:?} {reason:?}"
        );
    }
    // the closed selector grammar
    let short = format!("{id}@abc");
    let upper = format!("{id}@{}", "A".repeat(64));
    let cases: Vec<(Value, String)> = vec![
        (json!([]), "Cannot redact []: invalid selector.".into()),
        (
            json!(["design/doc"]),
            "Cannot redact \"design/doc\": invalid selector.".into(),
        ),
        (
            json!([short]),
            format!("Cannot redact \"{short}\": invalid selector."),
        ),
        (
            json!([upper]),
            format!("Cannot redact \"{upper}\": invalid selector."),
        ),
        (
            json!(["blob:abc"]),
            "Cannot redact \"blob:abc\": invalid selector.".into(),
        ),
        (
            json!(["tag:v1.0.0"]),
            "Cannot redact \"tag:v1.0.0\": metadata is not content.".into(),
        ),
        (
            json!(["meta/x"]),
            "Cannot redact \"meta/x\": metadata is not content.".into(),
        ),
        (
            json!(["p-ZZZZZ"]),
            "Cannot redact p-ZZZZZ: unknown target.".into(),
        ),
        (
            json!([format!("blob:{}", "0".repeat(64))]),
            format!("Cannot redact blob:{}: unknown target.", "0".repeat(64)),
        ),
    ];
    for (targets, want) in cases {
        let e = rdy_redact(
            &mut st,
            redact_args(json!({"targets": targets, "reason_class": "secret", "reason": "x", "dry_run": true})),
        )
        .unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                "E_REDACTION_TARGET",
                want.as_str(),
                "Use a block id, id@full-version-hash, or blob:full-manifest-hash."
            ),
            "{targets}"
        );
    }
    // 257 selectors and a 257-byte selector are over the bounds
    let many: Vec<String> = (0..257).map(|_| id.clone()).collect();
    let long = "x".repeat(257);
    for (targets, limit) in [
        (json!(many), "256 targets"),
        (json!([long]), "256 bytes per target"),
    ] {
        let e = rdy_redact(
            &mut st,
            redact_args(json!({"targets": targets, "reason_class": "secret", "reason": "x", "dry_run": true})),
        )
        .unwrap_err();
        assert_eq!(e.code, "E_REDACTION_LIMIT");
        assert_eq!(e.message, format!("The redaction exceeds {limit}."));
        assert_eq!(e.fix, "Select fewer targets, then run the dry run again.");
    }
    // a valid dry run of the same block passes the same parser
    let v = rdy_redact(
        &mut st,
        redact_args(
            json!({"targets": [id, id], "reason_class": "secret", "reason": "x", "dry_run": true}),
        ),
    )
    .unwrap();
    assert_eq!(
        v["report"]["targets"],
        json!([{"all_versions": id}]),
        "deduplicated"
    );
}

/// rdx12: rdy_redact's input schema publishes the required keys, the closed reason classes and
/// the defaults and bounds; every key it publishes parses; rdy_schema's error table has a row
/// for every redaction error and warning code, in the shared texts.
#[test]
fn rdx12_tool_schema_and_error_rows_cover_redaction() {
    let schema =
        rmcp::handler::server::common::schema_for_input::<RedactParams>().expect("an input schema");
    let schema = Value::Object((*schema).clone());
    assert_eq!(
        schema["required"],
        json!(["targets", "reason_class", "reason"]),
        "{schema}"
    );
    assert_eq!(schema["additionalProperties"], false, "{schema}");
    let props = schema["properties"].as_object().unwrap();
    let mut keys: Vec<&str> = props.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "dry_run",
            "max_tokens",
            "reason",
            "reason_class",
            "targets",
            "view"
        ]
    );
    assert_eq!(
        props["reason_class"]["enum"],
        json!(["secret", "personal", "legal", "retraction"])
    );
    assert_eq!(props["targets"]["type"], "array");
    assert_eq!(props["targets"]["items"]["type"], "string");
    assert_eq!(props["dry_run"]["default"], false);
    assert_eq!(props["max_tokens"]["default"], 4000);
    assert_eq!(props["max_tokens"]["minimum"], 256);
    assert_eq!(props["max_tokens"]["maximum"], 20000);
    // no approval switch is published
    for k in ["force", "yes", "policy", "approve", "approved_by"] {
        assert!(!props.contains_key(k), "{k}");
    }
    // every published key parses (a null optional is absent)
    let p = parse_redact(
        json!({"targets": ["p-AAAAA"], "reason_class": "legal", "reason": "r", "dry_run": null, "view": null, "max_tokens": null})
            .as_object()
            .unwrap()
            .clone(),
    )
    .unwrap();
    assert_eq!(
        (p.dry_run, p.view, p.max_tokens, p.reason_class.as_str()),
        (None, None, None, "legal")
    );
    // the error rows: the shared message as when, the shared fix; warnings have empty fixes
    let row = |code: &str| {
        ERROR_ROWS
            .iter()
            .find(|(c, _, _)| *c == code)
            .map(|(_, w, f)| (w.to_string(), f.to_string()))
            .unwrap_or_else(|| panic!("no row for {code}"))
    };
    let errors = [
        (
            "E_REDACTION_TARGET",
            "Cannot redact {target}: {cause}.",
            "Use a block id, id@full-version-hash, or blob:full-manifest-hash.",
        ),
        (
            "E_REDACTION_LIMIT",
            "The redaction exceeds {limit}.",
            "Select fewer targets, then run the dry run again.",
        ),
        (
            "E_REDACTION_REASON",
            "The redaction needs a reason and a reason class.",
            "Use secret, personal, legal, or retraction, with a reason of 1 to 4096 UTF-8 bytes.",
        ),
        (
            "E_REDACTION_STALE",
            "The redaction report changed after the request was recorded.",
            "Record a new request, show its report, and ask for approval again.",
        ),
        (
            "E_REDACTION_HUMAN",
            "Content removal needs a person's approval.",
            "Show the redaction card and relay the person's answer, or approve from the CLI as a human.",
        ),
        (
            "E_REDACTION_PENDING",
            "Content removal was authorized but has not finished.",
            "Close the project and reopen it to finish the removal. Do not delete the log.",
        ),
        (
            "E_REDACTION_CORRUPT",
            "The redaction record does not authorize the missing payload at {segment}:{offset}.",
            "Keep the store unchanged and inspect it with rdy fsck.",
        ),
        (
            "E_REDACTED_STATE",
            "This read state predates content removal.",
            "Read the project again, or run the dump again.",
        ),
        (
            "E_REDACTED",
            "Content removed by redaction {hash}.",
            "Read the redaction details in the source history.",
        ),
        (
            "E_REDACTION_BUDGET",
            "The output budget is too small for the redaction warning.",
            "Increase max_tokens, or propose fewer redaction requests at once.",
        ),
    ];
    for (code, when, fix) in errors {
        assert_eq!(row(code), (when.to_string(), fix.to_string()), "{code}");
    }
    let warnings = [
        ("W_REDACTION_COPIES", rdy_engine::W_REDACTION_COPIES_MESSAGE),
        (
            "W_REDACTION_INVENTORY",
            rdy_engine::W_REDACTION_INVENTORY_MESSAGE,
        ),
        ("W_REDACTION_QUOTES", rdy_engine::W_REDACTION_QUOTES_MESSAGE),
        ("W_REDACTION_REUSE", rdy_engine::W_REDACTION_REUSE_MESSAGE),
        (
            "W_REDACTION_QUARANTINE",
            rdy_engine::W_REDACTION_QUARANTINE_MESSAGE,
        ),
        ("W_REDACTION_SECRET", rdy_engine::W_REDACTION_SECRET_MESSAGE),
    ];
    for (code, message) in warnings {
        assert_eq!(row(code), (message.to_string(), String::new()), "{code}");
    }
    // the rows sit in the global order: the ten errors, then the six warnings
    let codes: Vec<&str> = ERROR_ROWS
        .iter()
        .map(|(c, _, _)| *c)
        .filter(|c| c.contains("REDACT"))
        .collect();
    let want: Vec<&str> = errors
        .iter()
        .map(|(c, _, _)| *c)
        .chain(warnings.iter().map(|(c, _)| *c))
        .collect();
    assert_eq!(codes, want);
}

/// hsa8 (D-KQ4CA, n-XZDQD): `rdy_auth` publishes exactly its eight fields and its two closed
/// enums, refuses any other key with the nearest-key text, and checks which fields each op
/// takes in one fixed order: a missing field needs `pass {field}`, an extra one `remove {field}`.
/// There is no confirmed_host, token, relayed_by or complete-link input.
#[test]
fn hsa8_auth_tool_schema_is_strict_and_complete() {
    let schema = rmcp::handler::server::common::schema_for_input::<AuthParams>().unwrap();
    let schema = Value::Object((*schema).clone());
    let mut props: Vec<&str> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    props.sort();
    assert_eq!(
        props,
        [
            "answer",
            "card_hash",
            "evidence",
            "install",
            "machine_name",
            "op",
            "question",
            "session_ref"
        ]
    );
    assert_eq!(schema["additionalProperties"], json!(false), "{schema}");
    assert_eq!(schema["required"], json!(["op"]), "{schema}");
    let text = schema.to_string();
    for word in ["\"start\"", "\"state\"", "\"sign_out\"", "\"installs\""] {
        assert!(text.contains(word), "{word}: {text}");
    }
    assert!(
        text.contains("\"confirm\"") && text.contains("\"cancel\""),
        "{text}"
    );
    // every published key parses, and the values of both enums are closed
    let full = json!({"op": "start", "install": "https://127.0.0.1:1", "machine_name": "m",
        "card_hash": "h", "answer": "confirm", "evidence": "e", "question": "q",
        "session_ref": "s"});
    let p = parse_params::<AuthParams>("rdy_auth", full.as_object().unwrap().clone()).unwrap();
    assert_eq!(
        (p.op, p.answer),
        (AuthOp::Start, Some(AuthAnswerParam::Confirm))
    );
    for op in ["state", "sign_out", "installs"] {
        let v = json!({ "op": op });
        assert!(parse_params::<AuthParams>("rdy_auth", v.as_object().unwrap().clone()).is_ok());
    }
    for (args, message) in [
        (
            json!({"op": "login"}),
            "rdy_auth: parameter \"op\" must be one of",
        ),
        (
            json!({"op": "start", "answer": "yes"}),
            "rdy_auth: parameter \"answer\" must be one of",
        ),
    ] {
        let (code, m, _) = refusal::<AuthParams>("rdy_auth", args.clone());
        assert_eq!(code, "E_BAD_INPUT", "{args}");
        assert!(m.starts_with(message), "{args}: {m}");
    }
    // unknown keys: the shared nearest-key refusal, for near and far names alike
    let all = "answer, card_hash, evidence, install, machine_name, op, question, session_ref";
    assert_eq!(
        refusal::<AuthParams>("rdy_auth", json!({"op": "start", "instal": "x"})),
        bad(
            "rdy_auth: unknown parameter \"instal\"",
            &format!("did you mean \"install\"? the parameters here are {all}")
        )
    );
    assert_eq!(
        refusal::<AuthParams>("rdy_auth", json!({"op": "start", "card_hsh": "x"})),
        bad(
            "rdy_auth: unknown parameter \"card_hsh\"",
            &format!("did you mean \"card_hash\"? the parameters here are {all}")
        )
    );
    for key in [
        "confirmed_host",
        "token",
        "access_token",
        "relayed_by",
        "verification_uri_complete",
    ] {
        let (code, m, fix) = refusal::<AuthParams>("rdy_auth", json!({"op": "start", key: "x"}));
        assert_eq!(code, "E_BAD_INPUT", "{key}");
        assert_eq!(m, format!("rdy_auth: unknown parameter \"{key}\""));
        assert!(
            fix.ends_with(&format!("the parameters here are {all}")),
            "{key}: {fix}"
        );
    }

    // which fields each op takes, checked in order
    let check = |args: Value| -> Result<AuthRequest, (String, String, String)> {
        let p = parse_params::<AuthParams>("rdy_auth", args.as_object().unwrap().clone())
            .expect("parses");
        auth_request(p, None).map_err(|e| (e.code.to_string(), e.message, e.fix))
    };
    let needs = |op: &str, f: &str| bad(&format!("auth {op} needs {f}"), &format!("pass {f}"));
    let unused = |op: &str, f: &str| {
        bad(
            &format!("{f} is not used by auth {op}"),
            &format!("remove {f}"),
        )
    };
    let o = "https://127.0.0.1:1";
    // start: install, then machine_name; card_hash and answer together; question and
    // session_ref need evidence
    assert_eq!(
        check(json!({"op": "start"})).unwrap_err(),
        needs("start", "install")
    );
    assert_eq!(
        check(json!({"op": "start", "machine_name": "m", "answer": "confirm"})).unwrap_err(),
        needs("start", "install"),
        "install is checked first"
    );
    assert_eq!(
        check(json!({"op": "start", "install": o})).unwrap_err(),
        needs("start", "machine_name")
    );
    assert_eq!(
        check(json!({"op": "start", "install": o, "machine_name": "m", "answer": "confirm"}))
            .unwrap_err(),
        needs("start", "card_hash")
    );
    assert_eq!(
        check(json!({"op": "start", "install": o, "machine_name": "m", "card_hash": "h"}))
            .unwrap_err(),
        needs("start", "answer")
    );
    assert_eq!(
        check(json!({"op": "start", "install": o, "machine_name": "m", "evidence": "yes"}))
            .unwrap_err(),
        needs("start", "card_hash"),
        "a relay field makes it an answer"
    );
    for f in ["question", "session_ref"] {
        assert_eq!(
            check(
                json!({"op": "start", "install": o, "machine_name": "m", "card_hash": "h",
                "answer": "cancel", f: "x"})
            )
            .unwrap_err(),
            needs("start", "evidence"),
            "{f}"
        );
    }
    // a card request, an answer with and without the relay
    assert_eq!(
        check(json!({"op": "start", "install": format!("{o}/"), "machine_name": "m"})).unwrap(),
        AuthRequest::Start {
            install: rdy_engine::hub_types::Origin::parse(o).unwrap(),
            machine_name: "m".into(),
            confirmation: None,
        }
    );
    let Ok(AuthRequest::Start {
        confirmation: Some(c),
        ..
    }) = check(
        json!({"op": "start", "install": o, "machine_name": "m", "card_hash": "h",
        "answer": "cancel", "evidence": "yes please", "question": "sign in?",
        "session_ref": "chat-1"}),
    )
    else {
        panic!("an answer");
    };
    assert_eq!(
        (c.card_hash.as_str(), c.answer),
        ("h", rdy_engine::hub_credentials::AuthAnswer::Cancel)
    );
    assert_eq!(
        (
            c.relay.relayed_by,
            c.relay.evidence.as_deref(),
            c.relay.question.as_deref(),
            c.relay.session_ref.as_deref()
        ),
        (None, Some("yes please"), Some("sign in?"), Some("chat-1"))
    );
    // an install that is not an HTTPS origin is the shared address refusal
    let e = check(json!({"op": "state", "install": "http://127.0.0.1:1"})).unwrap_err();
    assert_eq!(e.0, "E_BAD_INPUT");
    assert_eq!(e.1, rdy_engine::hub_types::BAD_URL_MESSAGE);
    // state and sign_out: install only
    for op in ["state", "sign_out"] {
        assert_eq!(
            check(json!({ "op": op })).unwrap_err(),
            needs(op, "install")
        );
        assert_eq!(
            check(json!({"op": op, "machine_name": "m"})).unwrap_err(),
            needs(op, "install"),
            "install first"
        );
        for f in [
            "machine_name",
            "card_hash",
            "evidence",
            "question",
            "session_ref",
        ] {
            assert_eq!(
                check(json!({"op": op, "install": o, f: "x"})).unwrap_err(),
                unused(op, f),
                "{op} {f}"
            );
        }
        assert_eq!(
            check(json!({"op": op, "install": o, "answer": "confirm"})).unwrap_err(),
            unused(op, "answer")
        );
        assert!(check(json!({"op": op, "install": o})).is_ok());
    }
    // installs: nothing
    assert_eq!(
        check(json!({"op": "installs"})).unwrap(),
        AuthRequest::Installs
    );
    for f in [
        "install",
        "machine_name",
        "card_hash",
        "evidence",
        "question",
        "session_ref",
    ] {
        assert_eq!(
            check(json!({"op": "installs", f: "x"})).unwrap_err(),
            unused("installs", f),
            "{f}"
        );
    }
    assert_eq!(
        check(json!({"op": "installs", "machine_name": "m", "install": o})).unwrap_err(),
        unused("installs", "install"),
        "install first"
    );
}

#[test]
fn hsa9_remote_and_key_schemas_are_strict() {
    let props_of = |schema: std::sync::Arc<serde_json::Map<String, Value>>| {
        let schema = Value::Object((*schema).clone());
        let mut props: Vec<String> = schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        props.sort();
        (schema, props)
    };
    let schema_for =
        |s: Result<std::sync::Arc<serde_json::Map<String, Value>>, _>| props_of(s.unwrap());
    // the four schemas: exact fields, closed, only the named fields required
    let cases: [(Value, Vec<String>, Value); 4] = {
        let (r, rp) = schema_for(rmcp::handler::server::common::schema_for_input::<
            RemoteParams,
        >());
        let (b, bp) = schema_for(rmcp::handler::server::common::schema_for_input::<
            KeyBindParams,
        >());
        let (t, tp) = schema_for(rmcp::handler::server::common::schema_for_input::<
            KeyRetireParams,
        >());
        let (v, vp) = schema_for(rmcp::handler::server::common::schema_for_input::<
            KeyRevokeParams,
        >());
        [
            (r, rp, json!(["op"])),
            (b, bp, json!(["kind"])),
            (t, tp, json!(["key"])),
            (v, vp, json!(["key"])),
        ]
    };
    let want: [&[&str]; 4] = [
        &["level", "mode", "name", "op", "url"],
        &["kind", "label", "remote"],
        &["key", "remote"],
        &["compromised_since", "key", "remote"],
    ];
    for ((schema, props, required), want) in cases.iter().zip(want) {
        assert_eq!(props, want, "{schema}");
        assert_eq!(schema["additionalProperties"], json!(false), "{schema}");
        assert_eq!(schema["required"], *required, "{schema}");
        let text = schema.to_string();
        for banned in [
            "expires",
            "key_expiry",
            "private_key",
            "token",
            "confirm\"",
            "\"proof",
        ] {
            assert!(!text.contains(banned), "{banned}: {text}");
        }
    }
    let remote_text = cases[0].0.to_string();
    for word in [
        "\"list\"",
        "\"add\"",
        "\"rm\"",
        "\"set_mode\"",
        "\"share\"",
        "\"publish\"",
        "\"project\"",
        "\"local\"",
        "\"user\"",
    ] {
        assert!(remote_text.contains(word), "{word}: {remote_text}");
    }
    let bind_text = cases[1].0.to_string();
    assert!(
        bind_text.contains("\"person\"") && bind_text.contains("\"agent\""),
        "{bind_text}"
    );

    // every published key parses; the enums are closed
    let parse = |tool: &str, args: Value| -> Result<(), (String, String, String)> {
        let map = args.as_object().unwrap().clone();
        let r = match tool {
            "rdy_remote" => parse_params::<RemoteParams>(tool, map).map(|_| ()),
            "rdy_key_bind" => parse_params::<KeyBindParams>(tool, map).map(|_| ()),
            "rdy_key_retire" => parse_params::<KeyRetireParams>(tool, map).map(|_| ()),
            _ => parse_params::<KeyRevokeParams>(tool, map).map(|_| ()),
        };
        r.map_err(|e| (e.code.to_string(), e.message, e.fix))
    };
    assert!(
        parse(
            "rdy_remote",
            json!({"op": "add", "name": "origin", "url": "https://127.0.0.1:1/p",
                "mode": "share", "level": "user"})
        )
        .is_ok()
    );
    assert!(
        parse(
            "rdy_key_bind",
            json!({"kind": "agent", "label": "l", "remote": "origin"})
        )
        .is_ok()
    );
    assert!(parse("rdy_key_retire", json!({"key": "k", "remote": "origin"})).is_ok());
    assert!(
        parse(
            "rdy_key_revoke",
            json!({"key": "k", "compromised_since": "2026-10-17T11:00:00Z", "remote": "o"})
        )
        .is_ok()
    );
    for (tool, args, message) in [
        (
            "rdy_remote",
            json!({"op": "remove"}),
            "rdy_remote: parameter \"op\" must be one of",
        ),
        (
            "rdy_remote",
            json!({"op": "add", "mode": "mirror"}),
            "rdy_remote: parameter \"mode\" must be one of",
        ),
        (
            "rdy_remote",
            json!({"op": "add", "level": "default"}),
            "rdy_remote: parameter \"level\" must be one of",
        ),
        (
            "rdy_key_bind",
            json!({"kind": "machine"}),
            "rdy_key_bind: parameter \"kind\" must be one of",
        ),
        (
            "rdy_key_bind",
            json!({}),
            "rdy_key_bind: parameter \"kind\" is required",
        ),
        (
            "rdy_key_retire",
            json!({}),
            "rdy_key_retire: parameter \"key\" is required",
        ),
        (
            "rdy_key_revoke",
            json!({"compromised_since": "x"}),
            "rdy_key_revoke: parameter \"key\" is required",
        ),
    ] {
        let (code, m, _) = parse(tool, args.clone()).unwrap_err();
        assert_eq!(code, "E_BAD_INPUT", "{args}");
        assert!(m.starts_with(message), "{args}: {m}");
    }

    // unknown keys: the shared nearest-key refusal, near and far, and no secret or expiry
    // argument exists on any of them
    let near = [
        (
            "rdy_remote",
            json!({"op": "add", "nmae": "x"}),
            "nmae",
            "name",
            "level, mode, name, op, url",
        ),
        (
            "rdy_key_bind",
            json!({"kind": "person", "lable": "x"}),
            "lable",
            "label",
            "kind, label, remote",
        ),
        (
            "rdy_key_retire",
            json!({"key": "k", "remot": "x"}),
            "remot",
            "remote",
            "key, remote",
        ),
        (
            "rdy_key_revoke",
            json!({"key": "k", "compromised_snce": "x"}),
            "compromised_snce",
            "compromised_since",
            "compromised_since, key, remote",
        ),
    ];
    for (tool, args, wrong, right, all) in near {
        assert_eq!(
            parse(tool, args.clone()).unwrap_err(),
            bad(
                &format!("{tool}: unknown parameter \"{wrong}\""),
                &format!("did you mean \"{right}\"? the parameters here are {all}")
            ),
            "{args}"
        );
    }
    for (tool, base, all) in [
        (
            "rdy_remote",
            json!({"op": "list"}),
            "level, mode, name, op, url",
        ),
        (
            "rdy_key_bind",
            json!({"kind": "person"}),
            "kind, label, remote",
        ),
        ("rdy_key_retire", json!({"key": "k"}), "key, remote"),
        (
            "rdy_key_revoke",
            json!({"key": "k"}),
            "compromised_since, key, remote",
        ),
    ] {
        for key in [
            "expires",
            "key_expiry",
            "private_key",
            "token",
            "access_token",
            "confirm",
            "proof",
            "statement",
            "user_file",
        ] {
            let mut args = base.clone();
            args[key] = json!("x");
            let (code, m, fix) = parse(tool, args).unwrap_err();
            assert_eq!(code, "E_BAD_INPUT", "{tool} {key}");
            assert_eq!(m, format!("{tool}: unknown parameter \"{key}\""));
            assert!(
                fix.ends_with(&format!("the parameters here are {all}")),
                "{tool} {key}: {fix}"
            );
        }
    }

    // which fields each remote op takes, checked in the order name, url, mode, level
    let check = |args: Value| -> Result<RemoteRequest, (String, String, String)> {
        let p = parse_params::<RemoteParams>("rdy_remote", args.as_object().unwrap().clone())
            .expect("parses");
        remote_request(p).map_err(|e| (e.code.to_string(), e.message, e.fix))
    };
    let needs = |op: &str, f: &str| bad(&format!("remote {op} needs {f}"), &format!("pass {f}"));
    let unused = |op: &str, f: &str| {
        bad(
            &format!("{f} is not used by remote {op}"),
            &format!("remove {f}"),
        )
    };
    let u = "https://127.0.0.1:1/p";
    // list: nothing
    assert_eq!(check(json!({"op": "list"})).unwrap(), RemoteRequest::List);
    for (f, v) in [
        ("name", json!("o")),
        ("url", json!(u)),
        ("mode", json!("share")),
        ("level", json!("project")),
    ] {
        assert_eq!(
            check(json!({"op": "list", f: v})).unwrap_err(),
            unused("list", f),
            "{f}"
        );
    }
    assert_eq!(
        check(json!({"op": "list", "level": "user", "name": "o"})).unwrap_err(),
        unused("list", "name"),
        "name first"
    );
    // add: name, url and mode; level is optional and defaults to project
    assert_eq!(
        check(json!({"op": "add", "url": u, "mode": "share"})).unwrap_err(),
        needs("add", "name")
    );
    assert_eq!(
        check(json!({"op": "add", "mode": "share", "level": "user"})).unwrap_err(),
        needs("add", "name"),
        "name first"
    );
    assert_eq!(
        check(json!({"op": "add", "name": "o", "mode": "share"})).unwrap_err(),
        needs("add", "url")
    );
    assert_eq!(
        check(json!({"op": "add", "name": "o", "url": u})).unwrap_err(),
        needs("add", "mode")
    );
    assert_eq!(
        check(json!({"op": "add", "name": "o", "url": u, "mode": "publish"})).unwrap(),
        RemoteRequest::Add {
            name: "o".into(),
            url: u.into(),
            mode: rdy_engine::hub_config::RemoteMode::Publish,
            level: rdy_engine::hub_remotes::RemoteLevel::Project,
        }
    );
    assert_eq!(
        check(json!({"op": "add", "name": "o", "url": u, "mode": "share", "level": "local"}))
            .unwrap(),
        RemoteRequest::Add {
            name: "o".into(),
            url: u.into(),
            mode: rdy_engine::hub_config::RemoteMode::Share,
            level: rdy_engine::hub_remotes::RemoteLevel::Local,
        }
    );
    // rm: name; url and mode are not used
    assert_eq!(check(json!({"op": "rm"})).unwrap_err(), needs("rm", "name"));
    assert_eq!(
        check(json!({"op": "rm", "url": u})).unwrap_err(),
        needs("rm", "name"),
        "name first"
    );
    assert_eq!(
        check(json!({"op": "rm", "name": "o", "url": u})).unwrap_err(),
        unused("rm", "url")
    );
    assert_eq!(
        check(json!({"op": "rm", "name": "o", "mode": "share"})).unwrap_err(),
        unused("rm", "mode")
    );
    assert_eq!(
        check(json!({"op": "rm", "name": "o", "level": "user"})).unwrap(),
        RemoteRequest::Rm {
            name: "o".into(),
            level: rdy_engine::hub_remotes::RemoteLevel::User,
        }
    );
    // set_mode: name and mode; url is not used
    assert_eq!(
        check(json!({"op": "set_mode", "mode": "share"})).unwrap_err(),
        needs("set_mode", "name")
    );
    assert_eq!(
        check(json!({"op": "set_mode", "name": "o", "url": u, "mode": "share"})).unwrap_err(),
        unused("set_mode", "url"),
        "url before mode"
    );
    assert_eq!(
        check(json!({"op": "set_mode", "name": "o"})).unwrap_err(),
        needs("set_mode", "mode")
    );
    assert_eq!(
        check(json!({"op": "set_mode", "name": "o", "mode": "share"})).unwrap(),
        RemoteRequest::SetMode {
            name: "o".into(),
            mode: rdy_engine::hub_config::RemoteMode::Share,
            level: rdy_engine::hub_remotes::RemoteLevel::Project,
        }
    );
}

/// hpb15 (D-Q0AVZ, D-KQ4CA, D-8QV9G, D-50JXR): `rdy_publish` publishes exactly `{tag, to, site,
/// keep_latest}`. The first three are required strings; `keep_latest` is an optional boolean
/// whose absence and null are both "not given". A string for it is the strict-input type
/// refusal, and an unknown key is the shared nearest-key refusal. No profile, audience, token or
/// approval argument exists.
#[test]
fn hpb15_publish_params_are_closed_and_keep_latest_is_optional() {
    let schema = rmcp::handler::server::common::schema_for_input::<PublishParams>().unwrap();
    let schema = Value::Object((*schema).clone());
    let mut props: Vec<&str> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    props.sort();
    assert_eq!(props, ["keep_latest", "site", "tag", "to"]);
    let mut required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    required.sort();
    assert_eq!(required, ["site", "tag", "to"]);
    assert_eq!(schema["additionalProperties"], json!(false), "{schema}");
    assert_eq!(
        schema["properties"]["keep_latest"]["description"],
        "Optional: true or false. rdy sends it to the hub beside the record. Omit it to use the hub's default."
    );
    for k in ["tag", "to", "site"] {
        assert_eq!(schema["properties"][k]["type"], "string", "{k}");
    }

    let parse = |args: Value| {
        parse_params::<PublishParams>("rdy_publish", args.as_object().unwrap().clone())
    };
    let base = json!({"tag": "v1.0.0", "to": "pub", "site": "docs"});
    let p = parse(base.clone()).unwrap();
    assert_eq!(
        (
            p.tag.as_str(),
            p.to.as_str(),
            p.site.as_str(),
            p.keep_latest
        ),
        ("v1.0.0", "pub", "docs", None)
    );
    for (given, want) in [
        (json!(null), None),
        (json!(true), Some(true)),
        (json!(false), Some(false)),
    ] {
        let mut args = base.clone();
        args["keep_latest"] = given.clone();
        assert_eq!(parse(args).unwrap().keep_latest, want, "{given}");
    }
    // a string is the strict-input type refusal, never read as a boolean
    for given in [json!("true"), json!("false"), json!(1)] {
        let mut args = base.clone();
        args["keep_latest"] = given.clone();
        let e = parse(args).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{given}");
        assert!(
            e.message
                .starts_with("rdy_publish: parameter \"keep_latest\" must be "),
            "{given}: {}",
            e.message
        );
    }
    // each required string is named
    for k in ["tag", "to", "site"] {
        let mut args = base.clone();
        args.as_object_mut().unwrap().remove(k);
        assert_eq!(
            refusal::<PublishParams>("rdy_publish", args),
            bad(
                &format!("rdy_publish: parameter \"{k}\" is required"),
                &format!("pass \"{k}\" as a string")
            )
        );
    }
    // the nearest key for a misspelling, and no override of the profile or the audience
    let mut args = base.clone();
    args.as_object_mut().unwrap().remove("site");
    args["ste"] = json!("docs");
    assert_eq!(
        refusal::<PublishParams>("rdy_publish", args),
        bad(
            "rdy_publish: unknown parameter \"ste\"",
            "did you mean \"site\"? the parameters here are keep_latest, site, tag, to"
        )
    );
    for key in [
        "profile",
        "audience",
        "ratify",
        "authorize",
        "token",
        "origin",
        "format",
        "out",
        "force",
        "resume",
    ] {
        let mut args = base.clone();
        args[key] = json!("x");
        let e = parse(args).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            (
                "E_BAD_INPUT",
                &*format!("rdy_publish: unknown parameter \"{key}\"")
            ),
            "{key}"
        );
    }
}
