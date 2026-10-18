//! pub12 (D-48NYM, D-KQ4CA; rdyhub D-45W02 FW-PUB3; FW-PB9a D-QYCRB): `rdy publication check
//! --profile implicit:org` and `rdy_publication_check {profile: "implicit:org"}` return the same
//! report, with null `id`, `version` and `key`; the text names the implicit profile and its
//! audience, which is always org.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{approver, land, write};
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::path::Path;

/// A landed doc `a` in the folder `pubs` and a doc `admin` holding the org profile `site`
/// (selecting `pubs`), tagged `v1.0.0`; then a second landing adds the doc `b`. No reference
/// leaves the project, so the implicit profile's check is clear.
fn seed(dir: &Path) {
    let mut r = Rdy::open(dir).unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$pubs", "fields": {"name": "pubs"}},
            {"op": "create", "type": "doc", "id": "$a", "parent": "$pubs", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    let (pubs, a, admin) = (
        w.created["$pubs"].clone(),
        w.created["$a"].clone(),
        w.created["$admin"].clone(),
    );
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "prose", "parent": a, "body": "Words."},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "site", "title": "Site", "audience": "org", "select": [pubs]}}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "doc", "parent": pubs, "fields": {"name": "b", "title": "B", "doc_type": "page"}}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    r.close().unwrap();
}

fn stdout(o: &std::process::Output) -> String {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn pub12_check_names_the_implicit_profile_on_cli_and_mcp() {
    let s = Served::new();
    seed(s.dir.path());
    let mut c = McpClient::start_proxy(&s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:p", "role": "reader", "view": "canon"}),
    )
    .ok();
    for against in [None, Some("tag:v1.0.0")] {
        let mut args = vec![
            "--json",
            "publication",
            "check",
            "--profile",
            "implicit:org",
        ];
        let mut chat = json!({"profile": "implicit:org"});
        if let Some(a) = against {
            args.extend(["--against", a]);
            chat["against"] = json!(a);
        }
        let (ok, cli) = s.json(&args);
        assert!(ok, "{cli}");
        let mcp = c.call("rdy_publication_check", chat).ok();
        assert_eq!(cli, mcp, "{against:?}");
        assert_eq!(cli.to_string(), mcp.to_string(), "{against:?}");
        let p = &cli["profile"];
        for k in ["id", "version", "key"] {
            assert_eq!(p[k], Value::Null, "{k}: {cli}");
        }
        assert_eq!(p["audience"], "org");
        assert_eq!(p["trail"], true);
        assert_eq!(cli["references"]["status"], "clear");
        if against.is_some() {
            let d = &cli["disclosure"];
            assert_eq!(d["profile_id"], Value::Null);
            assert_eq!(d["profile_changed"], false);
            assert_eq!(d["added_docs"].as_array().unwrap().len(), 1, "{d}");
        }
    }

    // the text: the first line names the implicit profile and its audience
    let t = stdout(&s.rdy(&["publication", "check", "--profile", "implicit:org"]));
    assert!(
        t.starts_with("Implicit publication profile (org): clear.\n"),
        "{t}"
    );
    assert_eq!(t.lines().count(), 3, "{t}");
    assert!(
        t.lines()
            .nth(1)
            .is_some_and(|l| l.ends_with(" docs and 1 folders.")),
        "{t}"
    );
    assert_eq!(t.lines().nth(2), Some("References outside selection: 0."));
    let t = stdout(&s.rdy(&[
        "publication",
        "check",
        "--profile",
        "implicit:org",
        "--against",
        "tag:v1.0.0",
    ]));
    assert!(
        t.starts_with("Implicit publication profile (org): clear.\n"),
        "{t}"
    );
    assert!(t.contains("\nProfile revision changed: no.\n"), "{t}");
    assert!(t.contains("Disclosure changes: 1 added docs, "), "{t}");
    // a named profile keeps Task 9's first line
    let t = stdout(&s.rdy(&["publication", "check", "--profile", "site"]));
    assert!(t.starts_with("Publication profile site: clear.\n"), "{t}");

    // implicit:public and any other implicit value are refused, on both paths
    for (value, message) in [
        (
            "implicit:public",
            "the implicit profile is always org (FW-PB9a)",
        ),
        (
            "implicit:all",
            "Implicit profile \"implicit:all\" is not known.",
        ),
    ] {
        let out = s.rdy(&["publication", "check", "--profile", value]);
        assert!(!out.status.success(), "{value}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("E_BAD_INPUT"), "{err}");
        assert!(err.contains(message), "{err}");
        let e = match c.call("rdy_publication_check", json!({"profile": value})) {
            Reply::ToolError(e) => e,
            other => panic!("{value}: {other:?}"),
        };
        assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
        assert_eq!(e["message"], message, "{e}");
    }
}
