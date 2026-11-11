//! m4a4 (FW-HOOK D-NNQ0A, D-GHR0Y; FW-L3 D-54CKQ refined by D-DC5D1): the input of `rdy_hook`.
//! The tool's published schema and the engine's one parser, which the daemon's `hook` op and
//! `rdy hook` also run, close the same four forms: the same keys, the same enums and the same
//! bounds. The tool refuses before it touches a session or a host.
use rdy_engine::hooks::{
    HOOK_KEYS, HookRequest, MAX_HOOK_CHECKOUT, MAX_HOOK_CITATIONS, MAX_HOOK_HEAD, MAX_HOOK_INPUT,
    MAX_HOOK_MESSAGE, MAX_HOOK_PATH, MAX_HOOK_PATHS,
};
use rdy_mcp::params::parse_params;
use rdy_mcp::tools::{HOOK_DOC, HookParams};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn schema() -> Value {
    serde_json::to_value(schemars::schema_for!(HookParams)).expect("a schema")
}

/// The refusal of `args` as the tool gives it: parameters first, then the request.
fn refused(args: Value) -> (String, String, String) {
    let e = parse_params::<HookParams>("rdy_hook", args.as_object().expect("an object").clone())
        .and_then(|p| HookRequest::from_value(&Value::Object(p.0)))
        .expect_err("a refusal");
    // the engine's parser gives the same refusal to the daemon and the command line
    let direct = HookRequest::from_value(&args).expect_err("a refusal");
    assert_eq!(
        (&e.code, &e.message, &e.fix),
        (&direct.code, &direct.message, &direct.fix),
        "{args}"
    );
    (e.code.to_string(), e.message.clone(), e.fix.clone())
}

fn accepted(args: Value) -> HookRequest {
    parse_params::<HookParams>("rdy_hook", args.as_object().expect("an object").clone())
        .and_then(|p| HookRequest::from_value(&Value::Object(p.0)))
        .unwrap_or_else(|e| panic!("{} {} ({args})", e.code, e.message))
}

fn head() -> String {
    "a".repeat(40)
}

fn check_code() -> Value {
    json!({"op": "check", "kind": "code", "repo": "app", "checkout": absolute("app"),
        "head": head(), "paths": ["src/a.rs"], "message": "feat: a"})
}

fn absolute(name: &str) -> String {
    std::env::temp_dir().join(name).display().to_string()
}

fn with(mut base: Value, key: &str, value: Value) -> Value {
    base[key] = value;
    base
}

fn form(message: &str) -> String {
    format!("Invalid rdy_hook form: {message}.")
}

const FORM_FIX: &str = "Use one documented check, install or remove form.";

#[test]
fn m4a4_hook_schema_is_closed_and_matches_the_cli() {
    let root = schema();
    // one closed object with the engine's nine keys, and only op and kind always required
    assert_eq!(root["additionalProperties"], json!(false));
    let keys: BTreeSet<String> = root["properties"]
        .as_object()
        .expect("properties")
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        keys,
        HOOK_KEYS
            .iter()
            .map(|k| k.to_string())
            .collect::<BTreeSet<_>>()
    );
    assert_eq!(root["required"], json!(["op", "kind"]));
    assert!(!HOOK_DOC.contains("kungfu"));
    assert!(!root.to_string().contains("kungfu"));
    // the enums are the parser's choices, word for word
    assert_eq!(
        root["properties"]["op"]["enum"],
        json!(["check", "install", "remove"])
    );
    assert_eq!(root["properties"]["kind"]["enum"], json!(["code", "edit"]));
    assert_eq!(
        root["properties"]["harness"]["enum"],
        json!(["claude-code"])
    );
    // the bounds are the parser's
    assert_eq!(root["properties"]["paths"]["maxItems"], MAX_HOOK_PATHS);
    assert_eq!(
        root["properties"]["checkout"]["maxLength"],
        MAX_HOOK_CHECKOUT
    );
    assert_eq!(root["properties"]["head"]["maxLength"], MAX_HOOK_HEAD);
    assert_eq!(root["properties"]["message"]["maxLength"], MAX_HOOK_MESSAGE);

    // every form parses, on the tool's path as on the engine's
    accepted(check_code());
    accepted(json!({"op": "check", "kind": "edit", "paths": [absolute("x.md")]}));
    accepted(with(check_code(), "head", Value::Null));
    for op in ["install", "remove"] {
        accepted(json!({"op": op, "kind": "code", "repo": "app"}));
        accepted(json!({"op": op, "kind": "edit", "harness": "claude-code"}));
        accepted(json!({"op": op, "kind": "edit", "harness": "claude-code", "worktree": "w1"}));
    }

    // a field of another form is refused, by name
    for (args, key) in [
        (
            with(check_code(), "harness", json!("claude-code")),
            "harness",
        ),
        (with(check_code(), "worktree", json!("w")), "worktree"),
        (
            json!({"op": "check", "kind": "edit", "paths": [absolute("x")], "repo": "app"}),
            "repo",
        ),
        (
            json!({"op": "install", "kind": "code", "repo": "app", "message": "m"}),
            "message",
        ),
        (
            json!({"op": "remove", "kind": "edit", "harness": "claude-code", "paths": []}),
            "paths",
        ),
        (
            json!({"op": "install", "kind": "edit", "harness": "claude-code", "repo": "app"}),
            "repo",
        ),
    ] {
        let (code, message, fix) = refused(args);
        let (op, kind) = (
            message.contains("the check edit form"),
            message.contains("code form"),
        );
        let _ = (op, kind);
        assert_eq!(code, "E_BAD_INPUT");
        assert!(
            message.starts_with("Invalid rdy_hook form: remove ")
                && message.contains(&format!("remove {key}:")),
            "{message}"
        );
        assert_eq!(fix, FORM_FIX);
    }
    // a missing field is named
    let mut missing = check_code();
    missing.as_object_mut().unwrap().remove("message");
    assert_eq!(
        refused(missing),
        ("E_BAD_INPUT".into(), form("pass message"), FORM_FIX.into())
    );
    assert_eq!(
        refused(json!({"op": "install", "kind": "edit"})),
        ("E_BAD_INPUT".into(), form("pass harness"), FORM_FIX.into())
    );
    assert_eq!(
        refused(json!({"kind": "code"})),
        ("E_BAD_INPUT".into(), form("pass op"), FORM_FIX.into())
    );
    assert_eq!(
        refused(json!({"op": "run", "kind": "code"})),
        (
            "E_BAD_INPUT".into(),
            form("op is check, install or remove"),
            FORM_FIX.into()
        )
    );
    assert_eq!(
        refused(json!({"op": "check", "kind": "both"})),
        (
            "E_BAD_INPUT".into(),
            form("kind is code or edit"),
            FORM_FIX.into()
        )
    );
    assert_eq!(
        refused(json!({"op": "install", "kind": "edit", "harness": "cursor"})).1,
        form("harness is claude-code")
    );

    // a typo names the nearest key; a stranger lists the fields
    assert_eq!(
        refused(json!({"op": "check", "kind": "edit", "path": [absolute("x")]})),
        (
            "E_BAD_INPUT".into(),
            form("unknown field \"path\" (did you mean \"paths\"?)"),
            FORM_FIX.into()
        )
    );
    assert_eq!(
        refused(json!({"op": "install", "kind": "edit", "harnes": "claude-code"})).1,
        form("unknown field \"harnes\" (did you mean \"harness\"?)")
    );
    assert_eq!(
        refused(json!({"op": "check", "kind": "edit", "zzzzzzzz": 1})).1,
        form(&format!(
            "unknown field \"zzzzzzzz\" (the fields are {})",
            HOOK_KEYS.join(", ")
        ))
    );

    // a wrong type is a form error, never a crash
    for args in [
        json!({"op": 1, "kind": "code"}),
        with(check_code(), "paths", json!("src/a.rs")),
        with(check_code(), "paths", json!([1])),
        with(check_code(), "message", json!(7)),
        with(check_code(), "head", json!("not-a-hash")),
        with(check_code(), "head", json!("A".repeat(40))),
        with(check_code(), "checkout", json!("relative/app")),
        with(check_code(), "paths", json!(["../escape"])),
        json!({"op": "install", "kind": "code", "repo": ""}),
        json!({"op": "install", "kind": "edit", "harness": "claude-code", "worktree": "a/b"}),
    ] {
        let (code, message, fix) = refused(args.clone());
        assert_eq!(code, "E_BAD_INPUT", "{args}");
        assert!(message.starts_with("Invalid rdy_hook form: "), "{message}");
        assert_eq!(fix, FORM_FIX);
    }
}

#[test]
fn m4a4_hook_limits_refuse_before_any_work() {
    let limit = |message: &str| {
        (
            "E_LIMIT".to_string(),
            message.to_string(),
            "Pass a smaller hook input.".to_string(),
        )
    };
    // 257 paths, a long path, a long message, a long checkout and a long head
    let many: Vec<String> = (0..=MAX_HOOK_PATHS)
        .map(|i| format!("src/{i}.rs"))
        .collect();
    assert_eq!(
        refused(with(check_code(), "paths", json!(many))),
        limit("The hook input exceeds 256 paths.")
    );
    assert_eq!(
        refused(with(
            check_code(),
            "paths",
            json!(["a".repeat(MAX_HOOK_PATH + 1)])
        )),
        limit("The hook input exceeds 4096 path bytes.")
    );
    assert_eq!(
        refused(with(
            check_code(),
            "message",
            json!("m".repeat(MAX_HOOK_MESSAGE + 1))
        )),
        limit("The hook input exceeds 65536 message bytes.")
    );
    assert_eq!(
        refused(with(
            check_code(),
            "checkout",
            json!("/".to_string() + &"c".repeat(MAX_HOOK_CHECKOUT))
        )),
        limit("The hook input exceeds 4096 checkout bytes.")
    );
    assert_eq!(
        refused(with(
            check_code(),
            "head",
            json!("a".repeat(MAX_HOOK_HEAD + 1))
        )),
        limit("The hook input exceeds 64 head bytes.")
    );
    // the whole request is bounded before any field is read
    assert_eq!(
        refused(with(
            check_code(),
            "message",
            json!("m".repeat(MAX_HOOK_INPUT))
        )),
        limit("The hook input exceeds 1048576 bytes.")
    );
    // the largest allowed message and path list still parse
    accepted(with(
        check_code(),
        "message",
        json!("m".repeat(MAX_HOOK_MESSAGE)),
    ));
    let max: Vec<String> = (0..MAX_HOOK_PATHS).map(|i| format!("src/{i}.rs")).collect();
    accepted(with(check_code(), "paths", json!(max)));
    assert_eq!(MAX_HOOK_CITATIONS, 128);
}
