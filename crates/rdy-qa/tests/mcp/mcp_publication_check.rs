//! pub9 (D-0VCQ5, D-KDFZ3, D-KQ4CA): `rdy_publication_check` takes a strict request from any
//! session role, returns success for a blocked policy result, and carries no receipt or approval.
use rdy_engine::Rdy;
use rdy_mcp::tools::ERROR_ROWS;
use rdy_qa::fixtures::{approver, land, write};
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};

const DESCRIPTION: &str = "Check a publication profile's selected docs and references. Scope may be canon, a tag, a landing, a view or a proposal. Pass implicit:org for every doc. Against accepts canon, a tag or a landing with the same profile id and audience. This check does not authorize publication or find the last publication. Relay warnings to the person.";

/// A public profile `open` that refers outside its selection, an org profile `site` over the same
/// doc, and a tag `v1.0.0`.
fn seed(dir: &std::path::Path) {
    let mut r = Rdy::open(dir).unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$out", "fields": {"name": "out", "title": "Out", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    let (a, out, admin) = (
        w.created["$a"].clone(),
        w.created["$out"].clone(),
        w.created["$admin"].clone(),
    );
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "prose", "parent": a, "body": format!("See {{{{{out}}}}}.")},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "open", "title": "Open", "audience": "public", "select": [a]}},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "site", "title": "Site", "audience": "org", "select": [a]}}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    r.close().unwrap();
}

/// The bytes of every log segment, by name: the log is the record, and a check adds nothing to it.
fn log_of(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir.join(".rdy/store/segments"))
        .unwrap()
        .flatten()
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect();
    out.sort();
    assert!(!out.is_empty());
    out
}

/// Every object key in `v`, at any depth.
fn all_keys(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                out.push(k.clone());
                all_keys(x, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| all_keys(x, out)),
        _ => {}
    }
}

#[test]
fn pub9_check_inputs_are_strict_and_policy_is_not_authorization() {
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    seed(d.path());
    let mut c = McpClient::start(d.path());

    // the tool and its description, exactly
    let tools = c.tools();
    let t = tools
        .iter()
        .find(|t| t["name"] == "rdy_publication_check")
        .expect("the tool is listed");
    assert_eq!(t["description"], DESCRIPTION);
    let props: Vec<&str> = {
        let mut p: Vec<&str> = t["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        p.sort();
        p
    };
    assert_eq!(props, ["against", "profile", "scope"]);
    assert_eq!(t["inputSchema"]["required"], json!(["profile"]));

    // a session is required
    let r = c.call("rdy_publication_check", json!({"profile": "site"}));
    assert_eq!(r.code(), Some("E_NO_SESSION"), "{r:?}");

    // one in-process server holds the store at a time
    drop(c);

    // every role the tool offers inspects: reader, writer, reviewer
    for (i, role) in ["reader", "writer", "reviewer"].into_iter().enumerate() {
        let mut m = McpClient::start(d.path());
        m.call(
            "rdy_session",
            json!({"actor": format!("agent:{role}"), "run": format!("r{i}"), "role": role}),
        )
        .ok();
        let before = log_of(d.path());
        let v = m
            .call("rdy_publication_check", json!({"profile": "site"}))
            .ok();
        assert_eq!(v["references"]["status"], "warning", "{role}: {v}");
        drop(m);
        assert_eq!(
            log_of(d.path()),
            before,
            "{role}: the check wrote to the log"
        );
    }

    let mut c = McpClient::start(d.path());
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "r1", "role": "writer"}),
    )
    .ok();
    let log = log_of(d.path());
    // a misspelt key names the key it was close to
    let r = c.call(
        "rdy_publication_check",
        json!({"profile": "site", "againts": "canon"}),
    );
    assert_eq!(r.code(), Some("E_BAD_INPUT"), "{r:?}");
    assert!(format!("{r:?}").contains("against"), "{r:?}");
    let r = c.call("rdy_publication_check", json!({"scope": "canon"}));
    assert_eq!(r.code(), Some("E_BAD_INPUT"), "{r:?}");
    // a candidate may be an existing view (D-CWC91): bare and explicit are one report
    let in_view = c
        .call(
            "rdy_publication_check",
            json!({"profile": "site", "scope": "view:work"}),
        )
        .ok();
    assert_eq!(
        in_view["scope"],
        json!({"kind": "view", "landing": null, "view": "work"}),
        "{in_view}"
    );
    let bare = c
        .call(
            "rdy_publication_check",
            json!({"profile": "site", "scope": "work"}),
        )
        .ok();
    assert_eq!(bare, in_view);
    // the in-flight scope and an empty text are no candidate
    for bad in ["inflight", ""] {
        let r = c.call(
            "rdy_publication_check",
            json!({"profile": "site", "scope": bad}),
        );
        assert_eq!(r.code(), Some("E_BAD_INPUT"), "{bad}: {r:?}");
        assert!(
            format!("{r:?}").contains(
                "Publication check scope must be canon, tag:<name>, landing:<hash>, a view, or proposal:P-x@n."
            ),
            "{bad}: {r:?}"
        );
    }
    // a view name is case-sensitive: `Canon` names a view, and none exists
    let r = c.call(
        "rdy_publication_check",
        json!({"profile": "site", "scope": "Canon"}),
    );
    assert_eq!(r.code(), Some("E_NOT_FOUND"), "{r:?}");
    assert!(format!("{r:?}").contains(r#"no view \"Canon\""#), "{r:?}");
    // a baseline is canon, a tag or a landing: a view or a proposal is not
    for bad in ["work", "view:work", "inflight", "proposal:P-ABCDE@1"] {
        let r = c.call(
            "rdy_publication_check",
            json!({"profile": "site", "against": bad}),
        );
        assert_eq!(r.code(), Some("E_BAD_INPUT"), "{bad}: {r:?}");
        assert!(
            format!("{r:?}").contains(
                "Publication check against must be canon, tag:<name>, or landing:<hash>."
            ),
            "{bad}: {r:?}"
        );
    }
    let r = c.call(
        "rdy_publication_check",
        json!({"profile": "site", "scope": "tag:9.9.9"}),
    );
    assert_eq!(r.code(), Some("E_NOT_FOUND"), "{r:?}");
    let r = c.call("rdy_publication_check", json!({"profile": "nope"}));
    assert_eq!(r.code(), Some("E_NOT_FOUND"), "{r:?}");

    // a blocked policy is a result: no error, no warning, no receipt, no approval
    let v = c
        .call(
            "rdy_publication_check",
            json!({"profile": "open", "against": "tag:v1.0.0"}),
        )
        .ok();
    assert_eq!(v["references"]["status"], "blocked", "{v}");
    assert_eq!(v["warnings"], json!([]));
    let mut keys = vec![];
    all_keys(&v, &mut keys);
    for k in &keys {
        for word in ["receipt", "approv", "authoriz", "publish", "record", "card"] {
            assert!(!k.contains(word), "{k} in {v}");
        }
    }
    // an org policy warns with the engine's text
    let v = c
        .call("rdy_publication_check", json!({"profile": "site"}))
        .ok();
    assert_eq!(v["references"]["status"], "warning", "{v}");
    assert_eq!(v["warnings"][0]["code"], "W_PUBLICATION_REFERENCE");
    assert_eq!(
        v["warnings"][0]["fix"],
        "Review these references before you share the output."
    );

    drop(c);
    assert_eq!(
        log_of(d.path()),
        log,
        "no check, success or refusal, wrote to the log"
    );

    // the error catalogue: two rows, last, with the exact words
    let rows: Vec<&(&str, &str, &str)> = ERROR_ROWS.iter().rev().take(2).collect::<Vec<_>>();
    assert_eq!(
        *rows[1],
        (
            "E_PUBLICATION_REFERENCE",
            "a public profile refers to blocks outside its selection",
            "Include the target docs or remove these references. Run the publication check again."
        )
    );
    assert_eq!(
        *rows[0],
        (
            "W_PUBLICATION_REFERENCE",
            "an org profile has references outside its selection, or a live public profile has blocked references",
            "follow the warning's own fix"
        )
    );
}
