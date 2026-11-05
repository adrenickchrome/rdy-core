//! ab6 (D-WJ0SJ, D-KQ4CA): `rdy publication check --release` and `rdy_publication_check` return
//! one report. A blocked release is a result, so the command exits 0; the text adds three lines
//! only when a release is given; the check adds no publish flag and leaves `--against` alone.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{approver, land, write};
use rdy_qa::mcp::McpClient;
use serde_json::json;
use std::path::Path;

/// The public profile `open` and the org profile `site` over the doc `a`, which holds content
/// since 3.0.0 and a claim whose decision is built in 2.0.0.
fn seed(dir: &Path) {
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

fn stdout(o: &std::process::Output) -> String {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn ab6_cli_and_mcp_release_results_match() {
    let s = Served::new();
    seed(s.dir.path());
    let mut c = McpClient::start_proxy(&s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:p", "role": "reader", "view": "canon"}),
    )
    .ok();

    // the JSON is the chat value, key for key, with and without a release and an against
    for (profile, release, against) in [
        ("open", Some("1.0.0"), None),
        ("site", Some("v1.0.0"), None),
        ("open", Some("2.0.0"), Some("tag:v1.0.0")),
        ("open", Some("3.0.0"), None),
        ("open", None, Some("canon")),
        ("site", None, None),
    ] {
        let mut args = vec!["--json", "publication", "check", "--profile", profile];
        let mut chat = json!({"profile": profile});
        if let Some(r) = release {
            args.extend(["--release", r]);
            chat["release"] = json!(r);
        }
        if let Some(a) = against {
            args.extend(["--against", a]);
            chat["against"] = json!(a);
        }
        let (ok, cli) = s.json(&args);
        assert!(ok, "{profile} {release:?}: {cli}");
        let mcp = c.call("rdy_publication_check", chat).ok();
        assert_eq!(cli, mcp, "{profile} {release:?} {against:?}");
        assert_eq!(cli.to_string(), mcp.to_string());
    }

    // the text: the same first line, then the release lines after the link line; a blocked
    // release exits 0
    let t = stdout(&s.rdy(&[
        "publication",
        "check",
        "--profile",
        "open",
        "--release",
        "1.0.0",
    ]));
    assert_eq!(
        t,
        "Publication profile open: blocked.\n4 blocks in 1 docs and 0 folders.\nReferences outside selection: 0.\nLinks outside selection: 0.\nRelease: 1.0.0.\nBlocks after release: 2.\nDecisions after release: 1.\n"
    );
    let t = stdout(&s.rdy(&[
        "publication",
        "check",
        "--profile",
        "site",
        "--release",
        "v1.0.0",
        "--against",
        "tag:v1.0.0",
    ]));
    assert_eq!(
        t,
        "Publication profile site: blocked.\n4 blocks in 1 docs and 0 folders.\nReferences outside selection: 0.\nLinks outside selection: 0.\nRelease: 1.0.0.\nBlocks after release: 2.\nDecisions after release: 1.\nProfile revision changed: no.\nDisclosure changes: 0 added docs, 0 removed docs, 0 changed docs, 0 changed folders, 0 changed blocks outside docs, 0 attachment changes, 0 new references.\n"
    );
    let t = stdout(&s.rdy(&[
        "publication",
        "check",
        "--profile",
        "open",
        "--release",
        "3.0.0",
    ]));
    assert_eq!(
        t,
        "Publication profile open: clear.\n4 blocks in 1 docs and 0 folders.\nReferences outside selection: 0.\nLinks outside selection: 0.\nRelease: 3.0.0.\nBlocks after release: 0.\nDecisions after release: 0.\n"
    );
    // no release adds no line, and against alone never makes one
    let t = stdout(&s.rdy(&[
        "publication",
        "check",
        "--profile",
        "open",
        "--against",
        "tag:v1.0.0",
    ]));
    assert_eq!(t.lines().count(), 6, "{t}");
    assert!(!t.contains("Release"), "{t}");
    let t = stdout(&s.rdy(&["publication", "check", "--profile", "open"]));
    assert_eq!(t.lines().count(), 4, "{t}");

    // usage errors: a bad version and a missing value fail with the engine's words
    let out = s.rdy(&[
        "publication",
        "check",
        "--profile",
        "open",
        "--release",
        "soon",
    ]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Release must be a release version."), "{err}");
    assert!(
        err.contains("Use a version such as 1.2.0 or 2.0.0-rc.1; one leading v is allowed, and +build metadata is not."),
        "{err}"
    );
    let out = s.rdy(&["publication", "check", "--profile", "open", "--release"]);
    assert!(!out.status.success());
    // an against that is a view is still refused, release or not
    let out = s.rdy(&[
        "publication",
        "check",
        "--profile",
        "open",
        "--release",
        "1.0.0",
        "--against",
        "work",
    ]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("Publication check against must be canon, tag:<name>, or landing:<hash>.")
    );

    // no new publish flag: the release belongs to the check alone
    let help = stdout(&s.rdy(&["publish", "--help"]));
    assert!(!help.contains("--release"), "{help}");
    let help = stdout(&s.rdy(&["publication", "check", "--help"]));
    assert!(help.contains("--release <RELEASE>"), "{help}");
}
