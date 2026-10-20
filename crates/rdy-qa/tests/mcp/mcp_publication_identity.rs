//! hpb8 (D-Z8Q4N, D-075QD, D-3M81W, D-KDFZ3, D-KQ4CA): the MCP tool `rdy_publication_check`, in
//! process, reports the selection identity beside the selection hash, the applied removal
//! requests (200 listed, the rest counted) and a removed doc's id in place of its name: the
//! engine's one report, key for key, as the command line reports it (cli_publication_identity).
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, publication};
use rdy_qa::fixtures::{approver, land, write, writer};
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};

/// The removed name: no surface may show it.
const SENTINEL: &str = "SENTINEL-QA-DOCNAME";

/// Tags `v1.0.0` and `v2.0.0` of an org profile `site` over folder `public-area`, then 201
/// approved removal requests: 200 prose blocks of doc `a`, and doc `d`, whose name is
/// [`SENTINEL`]. Returns `d`'s id. The project is closed again.
fn seed(dir: &std::path::Path) -> String {
    let mut r = Rdy::open(dir).unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$p", "fields": {"name": "public-area"}},
            {"op": "create", "type": "doc", "id": "$a", "parent": "$p", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$p", "fields": {"name": SENTINEL, "title": "D", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    let (p, a, d, admin) = (
        w.created["$p"].clone(),
        w.created["$a"].clone(),
        w.created["$d"].clone(),
        w.created["$admin"].clone(),
    );
    let mut ops: Vec<Value> = (0..200)
        .map(|i| json!({"op": "create", "type": "prose", "id": format!("$x{i}"), "parent": a, "body": format!("Line {i}.")}))
        .collect();
    ops.push(
        json!({"op": "create", "type": "publication_profile", "parent": admin,
            "fields": {"key": "site", "title": "Site", "audience": "org", "select": [p]}}),
    );
    let w = write(&mut r, "work", Value::Array(ops)).unwrap();
    let prose: Vec<String> = (0..200)
        .map(|i| w.created[&format!("$x{i}")].clone())
        .collect();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "prose", "parent": d, "body": "Later text."}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v2.0.0", None, None).unwrap();
    for target in prose.iter().chain([&d]) {
        r.redact_in(
            &writer(),
            "redact",
            RedactInput {
                targets: vec![target.clone()],
                reason_class: "secret".into(),
                reason: "a leaked value".into(),
                dry_run: false,
            },
        )
        .unwrap();
    }
    land(&mut r, "redact").unwrap();
    r.close().unwrap();
    d
}

/// The engine's own report of `site` at v2 against v1, read with the project closed again.
fn engine_report(dir: &std::path::Path) -> Value {
    let r = Rdy::open(dir).unwrap();
    let v = publication::check(&r.read(), "site", Some("tag:v2.0.0"), Some("tag:v1.0.0")).unwrap();
    r.close().unwrap();
    v
}

/// The identity, request and masked-name facts every surface must show.
fn assert_facts(v: &Value, d: &str) {
    let text = v.to_string();
    assert!(!text.contains(SENTINEL), "{text}");
    let s = &v["selection"];
    let disc = &v["disclosure"];
    assert!(s["identity"].as_str().is_some_and(|h| h.len() == 64));
    assert_ne!(s["identity"], s["hash"]);
    assert_eq!(disc["after_selection_identity"], s["identity"]);
    assert_eq!(disc["after_selection_hash"], s["hash"]);
    assert!(disc["before_selection_identity"].is_string());
    // 201 requests: the first 200, sorted and lowercase, then the count of the rest
    let listed: Vec<&str> = disc["redactions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h.as_str().unwrap())
        .collect();
    assert_eq!(listed.len(), 200);
    assert_eq!(disc["omitted"]["redactions"], 1);
    assert!(listed.windows(2).all(|w| w[0] < w[1]));
    assert!(listed.iter().all(|h| {
        h.len() == 64
            && h.bytes()
                .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
    }));
    // the removed doc's row shows its id, flagged
    let row = disc["changed_docs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == d)
        .expect("d changed");
    let path = format!("public-area/{d}");
    assert_eq!(
        (&row["before_path"], &row["after_path"], &row["redacted"]),
        (&json!(path), &json!(path), &json!(true))
    );
}

#[test]
fn hpb8_cli_and_mcp_report_the_same_identity_and_masked_names() {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let d = seed(dir.path());
    let want = engine_report(dir.path());
    assert_facts(&want, &d);

    let mut c = McpClient::start(dir.path());
    c.call(
        "rdy_session",
        json!({"actor": "agent:p", "role": "reader", "view": "canon"}),
    )
    .ok();
    let mcp = c
        .call(
            "rdy_publication_check",
            json!({"profile": "site", "scope": "tag:v2.0.0", "against": "tag:v1.0.0"}),
        )
        .ok();
    assert_eq!(mcp, want);
    assert_eq!(mcp.to_string(), want.to_string());
    assert_facts(&mcp, &d);
    // with no baseline the report has the identity and no disclosure
    let alone = c
        .call(
            "rdy_publication_check",
            json!({"profile": "site", "scope": "tag:v2.0.0"}),
        )
        .ok();
    assert!(alone["disclosure"].is_null());
    assert_eq!(alone["selection"], want["selection"]);
    assert!(!alone.to_string().contains(SENTINEL));
}
