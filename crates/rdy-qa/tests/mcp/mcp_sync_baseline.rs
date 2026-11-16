//! scc7 (D-CDQF7, D-6WD17, D-2VM48): the chat tool `rdy_publication_baseline` and the
//! `against_baseline` input of `rdy_publication_check` take strict requests, and the tool maps to
//! the `rdy:publish` ruleset.
use rdy_engine::Rdy;
use rdy_engine::kungfu::{self, Response};
use rdy_engine::publication::Baseline;
use rdy_qa::fixtures::{approver, land, write};
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::path::Path;

/// An org profile `site` over the doc `a`, and a tag `v1.0.0`.
fn seed(dir: &Path) {
    let mut r = Rdy::open(dir).unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    let (a, admin) = (w.created["$a"].clone(), w.created["$admin"].clone());
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "prose", "parent": a, "body": "Alpha holds."},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "site", "title": "Site", "audience": "org", "select": [a]}}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    r.close().unwrap();
}

fn tool_error(r: Reply) -> Value {
    match r {
        Reply::ToolError(v) => v,
        other => panic!("expected a tool error, got {other:?}"),
    }
}

/// D-2VM48: the baseline tool is under `rdy:publish` with the response `warn`, and the URL form
/// of `rdy_clone` stays under `rdy:project-ops`. The tool is listed, with a closed schema.
#[test]
fn scc7_baseline_ruleset_is_publish_warn_and_clone_stays_project_ops() {
    let row = kungfu::policy("rdy_publication_baseline").expect("the tool has a row");
    assert_eq!(row.ruleset, Some("rdy:publish"));
    assert_eq!(row.response, Response::Warn);
    let clone = kungfu::policy("rdy_clone").expect("the row of rdy_clone");
    assert_eq!(clone.ruleset, Some("rdy:project-ops"));
    assert_eq!(clone.response, Response::Warn);
    // the check keeps its owner
    assert_eq!(
        kungfu::policy("rdy_publication_check").unwrap().ruleset,
        Some("rdy:publish")
    );

    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    seed(d.path());
    let mut c = McpClient::start(d.path());
    let tools = c.tools();
    let t = tools
        .iter()
        .find(|t| t["name"] == "rdy_publication_baseline")
        .expect("the tool is listed");
    let schema = &t["inputSchema"];
    assert_eq!(schema["additionalProperties"], json!(false), "{schema}");
    let mut props: Vec<&str> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    props.sort();
    assert_eq!(props, ["out", "profile", "scope"]);
    assert_eq!(schema["required"], json!(["profile", "scope"]));
    let desc = t["description"].as_str().unwrap();
    assert!(desc.contains("complete path"), "{desc}");
    let check = tools
        .iter()
        .find(|t| t["name"] == "rdy_publication_check")
        .unwrap();
    let against = &check["inputSchema"]["properties"]["against_baseline"];
    assert!(
        against["description"]
            .as_str()
            .unwrap()
            .contains("complete path"),
        "{against}"
    );
}

/// An unknown key is refused with the nearest key named, on the new tool and on the new input.
#[test]
fn scc7_baseline_inputs_refuse_unknown_keys_with_suggestions() {
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    seed(d.path());
    let mut c = McpClient::start(d.path());
    c.call(
        "rdy_session",
        json!({"actor": "agent:p", "role": "reader", "view": "canon"}),
    )
    .ok();
    let e = tool_error(c.call(
        "rdy_publication_baseline",
        json!({"profile": "site", "scope": "tag:v1.0.0", "ou": "x"}),
    ));
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert_eq!(
        e["message"], "rdy_publication_baseline: unknown parameter \"ou\"",
        "{e}"
    );
    assert!(
        e["fix"]
            .as_str()
            .unwrap()
            .starts_with("did you mean \"out\"?"),
        "{e}"
    );
    let e = tool_error(c.call(
        "rdy_publication_check",
        json!({"profile": "site", "against_basline": "/x"}),
    ));
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert!(
        e["fix"]
            .as_str()
            .unwrap()
            .starts_with("did you mean \"against_baseline\"?"),
        "{e}"
    );
    // the scope is required
    let e = tool_error(c.call("rdy_publication_baseline", json!({"profile": "site"})));
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    // a made baseline verifies, and the check takes it back by its path
    let side = tempfile::tempdir().unwrap();
    let file = side.path().join("b.json");
    let made = c
        .call(
            "rdy_publication_baseline",
            json!({"profile": "site", "scope": "tag:v1.0.0", "out": file.to_string_lossy()}),
        )
        .ok();
    let baseline = Baseline::read(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(
        made["baseline"]["baseline_hash"],
        baseline.baseline_hash().to_string()
    );
    let report = c
        .call(
            "rdy_publication_check",
            json!({"profile": "site", "against_baseline": file.to_string_lossy()}),
        )
        .ok();
    assert_eq!(
        report["disclosure"]["baseline"]["baseline_hash"],
        baseline.baseline_hash().to_string()
    );
}
