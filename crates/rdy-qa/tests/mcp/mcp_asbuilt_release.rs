//! ab6 (D-WJ0SJ, FW-PB2): `rdy_publication_check` takes an optional typed `release`, returns the
//! aggregate `status` and a `release` object, works from a reader session with no daemon, and
//! writes nothing. A blocked release is a successful result and authorizes nothing.
use rdy_engine::Rdy;
use rdy_qa::fixtures::{approver, land, write};
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};

/// The public profile `open` and the org profile `site` over the doc `a`, which holds content
/// since 3.0.0 and a claim whose decision is built in 2.0.0.
fn seed(dir: &std::path::Path) {
    let mut r = Rdy::open(dir).unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}},
            {"op": "create", "type": "decision", "id": "$d", "fields": {"statement": "Two.", "status": "proposed", "built": "2.0.0"}}
        ]),
    )
    .unwrap();
    let (a, admin, d) = (
        w.created["$a"].clone(),
        w.created["$admin"].clone(),
        w.created["$d"].clone(),
    );
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "claim", "parent": a, "body": "Three.", "fields": {"since": "3.0.0"}},
            {"op": "create", "type": "claim", "parent": a, "body": "Two.", "links": {"decided_by": [d]}},
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

fn reader(d: &std::path::Path) -> McpClient {
    let mut c = McpClient::start(d);
    c.call(
        "rdy_session",
        json!({"actor": "agent:reader", "role": "reader"}),
    )
    .ok();
    c
}

#[test]
fn ab6_chat_release_check_is_strict_and_needs_no_daemon() {
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    seed(d.path());
    let mut c = McpClient::start(d.path());

    // the schema: one typed optional release, nothing else new, and no new tool
    let tools = c.tools();
    let t = tools
        .iter()
        .find(|t| t["name"] == "rdy_publication_check")
        .unwrap();
    let mut props: Vec<&str> = t["inputSchema"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    props.sort();
    assert_eq!(props, ["against", "profile", "release", "scope"]);
    assert_eq!(t["inputSchema"]["required"], json!(["profile"]));
    assert!(
        t["inputSchema"]["properties"]["release"]
            .to_string()
            .contains("string"),
        "{}",
        t["inputSchema"]["properties"]["release"]
    );
    assert!(
        t["description"]
            .as_str()
            .unwrap()
            .contains("Inspect the aggregate status and relay warnings to the person. A blocked result is a successful read and authorizes nothing."),
        "{}",
        t["description"]
    );
    assert!(
        !tools.iter().any(|t| t["name"]
            .as_str()
            .is_some_and(|n| n.contains("release") || n == "rdy_release_check")),
        "no new tool"
    );
    drop(tools);

    // a session is required, and a reader holds it
    let r = c.call(
        "rdy_publication_check",
        json!({"profile": "open", "release": "1.0.0"}),
    );
    assert_eq!(r.code(), Some("E_NO_SESSION"), "{r:?}");
    drop(c);
    let mut c = reader(d.path());
    // the session record is the reader's own; the checks add nothing after it
    let log = log_of(d.path());

    // typed: a number or a list is no release; a misspelt key is named; an unknown key is refused
    for bad in [json!(1), json!(["1.0.0"]), json!(true)] {
        let r = c.call(
            "rdy_publication_check",
            json!({"profile": "open", "release": bad}),
        );
        assert_eq!(r.code(), Some("E_BAD_INPUT"), "{bad}: {r:?}");
    }
    let r = c.call(
        "rdy_publication_check",
        json!({"profile": "open", "relase": "1.0.0"}),
    );
    assert_eq!(r.code(), Some("E_BAD_INPUT"), "{r:?}");
    assert!(format!("{r:?}").contains("release"), "{r:?}");
    let r = c.call(
        "rdy_publication_check",
        json!({"profile": "open", "version": "1.0.0"}),
    );
    assert_eq!(r.code(), Some("E_BAD_INPUT"), "{r:?}");

    // a bad version string is refused with the exact words, before any profile is resolved
    for bad in ["", "1.2", "latest", "1.0.0+b"] {
        let r = c.call(
            "rdy_publication_check",
            json!({"profile": "nope", "release": bad}),
        );
        assert_eq!(r.code(), Some("E_BAD_INPUT"), "{bad}: {r:?}");
        let text = format!("{r:?}");
        assert!(
            text.contains("Release must be a release version."),
            "{text}"
        );
        assert!(
            text.contains("Use a version such as 1.2.0 or 2.0.0-rc.1; one leading v is allowed, and +build metadata is not."),
            "{text}"
        );
    }
    let r = c.call(
        "rdy_publication_check",
        json!({"profile": "open", "release": format!("1.0.0-{}", "a".repeat(260))}),
    );
    assert_eq!(r.code(), Some("E_LIMIT"), "{r:?}");
    assert!(
        format!("{r:?}").contains("Release exceeds 256 bytes."),
        "{r:?}"
    );

    // absence and null are one thing: no release object, the status of the references alone
    let absent = c
        .call("rdy_publication_check", json!({"profile": "open"}))
        .ok();
    let null = c
        .call(
            "rdy_publication_check",
            json!({"profile": "open", "release": null}),
        )
        .ok();
    assert_eq!(absent, null);
    assert!(absent["release"].is_null(), "{absent}");
    assert_eq!(absent["status"], "clear");
    assert_eq!(absent["status"], absent["references"]["status"]);

    // a blocked release is a success for both audiences; the references stay what they were
    for profile in ["open", "site"] {
        let v = c
            .call(
                "rdy_publication_check",
                json!({"profile": profile, "release": "v1.0.0"}),
            )
            .ok();
        assert_eq!(v["status"], "blocked", "{profile}: {v}");
        assert_eq!(v["references"]["status"], "clear", "{profile}: {v}");
        assert_eq!(v["warnings"], json!([]));
        let rel = &v["release"];
        assert_eq!(rel["version"], "1.0.0");
        assert_eq!(rel["status"], "blocked");
        assert_eq!(rel["blocks_later"], 2);
        assert_eq!(rel["decisions_later"], 1);
        assert_eq!(rel["omitted_rows"], 0);
        let row = &rel["rows"][0];
        let mut keys: Vec<&str> = row
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort();
        assert_eq!(keys, ["field", "fix", "id", "message", "version"]);
        let mut all = vec![];
        collect(&v, &mut all);
        for k in &all {
            for word in ["receipt", "approv", "authoriz", "publish", "card"] {
                assert!(!k.contains(word), "{k} in {v}");
            }
        }
    }
    // a release that ships it all is clear
    let v = c
        .call(
            "rdy_publication_check",
            json!({"profile": "open", "release": "3.0.0"}),
        )
        .ok();
    assert_eq!(v["status"], "clear", "{v}");
    assert_eq!(v["release"]["status"], "clear");

    // an unknown profile and a tag that does not exist keep their own refusals
    let r = c.call(
        "rdy_publication_check",
        json!({"profile": "nope", "release": "1.0.0"}),
    );
    assert_eq!(r.code(), Some("E_NOT_FOUND"), "{r:?}");

    // rdy_publish takes no release: the explicit check is a preflight and publish does not read it
    let tools = c.tools();
    let publish = tools.iter().find(|t| t["name"] == "rdy_publish").unwrap();
    assert!(
        !publish["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("release"),
        "{publish}"
    );
    drop(tools);

    drop(c);
    assert_eq!(log_of(d.path()), log, "no check wrote to the log");
}

fn collect(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                out.push(k.clone());
                collect(x, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect(x, out)),
        _ => {}
    }
}
