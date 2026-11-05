//! ab6 (D-WJ0SJ): the ctl op `publication_check` and the daemon's MCP proxy share the one engine
//! release check. The op decodes strictly, reads one snapshot, and the daemon-backed chat result
//! equals the standalone one. Nothing in the log or the publication records changes.
use rdy_engine::Rdy;
use rdy_qa::daemon::{Hosted, Served};
use rdy_qa::fixtures::{approver, land, write};
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::path::Path;

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
             "fields": {"key": "open", "title": "Open", "audience": "public", "select": [a]}}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    r.close().unwrap();
}

/// The bytes of every file under the log and the refs: the record a check must leave alone.
fn record(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = vec![];
    for sub in [".rdy/store/segments", ".rdy/store/refs"] {
        let Ok(rd) = std::fs::read_dir(dir.join(sub)) else {
            continue;
        };
        for e in rd.flatten() {
            if e.path().is_file() {
                out.push((
                    format!("{sub}/{}", e.file_name().to_string_lossy()),
                    std::fs::read(e.path()).unwrap(),
                ));
            }
        }
    }
    out.sort();
    assert!(!out.is_empty());
    out
}

#[test]
fn ab6_ctl_and_daemon_mcp_share_release_check() {
    // the ctl op: strict keys, one snapshot, no write
    let dir = rdy_qa::daemon::short_tempdir();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    seed(dir.path());
    let before = record(dir.path());
    let mut h = Hosted::start_in(dir);
    let rt = Hosted::rt();
    let ctl: Value = rt.block_on(async {
        let mut a = h.client(None, None).await;
        let v = a
            .ctl(
                "publication_check",
                json!({"profile": "open", "release": "1.0.0"}),
            )
            .await
            .unwrap();
        let again = a
            .ctl(
                "publication_check",
                json!({"profile": "open", "release": "v1.0.0"}),
            )
            .await
            .unwrap();
        assert_eq!(v, again, "the v prefix is spelling");
        let none = a
            .ctl(
                "publication_check",
                json!({"profile": "open", "release": null}),
            )
            .await
            .unwrap();
        let absent = a
            .ctl("publication_check", json!({"profile": "open"}))
            .await
            .unwrap();
        assert_eq!(none, absent);
        assert!(absent["release"].is_null(), "{absent}");
        // a misspelt key is refused by name, and a typed release must be a string
        let e = a
            .ctl(
                "publication_check",
                json!({"profile": "open", "relase": "1.0.0"}),
            )
            .await
            .unwrap_err();
        assert!(format!("{e:?}").contains("release"), "{e:?}");
        let e = a
            .ctl(
                "publication_check",
                json!({"profile": "open", "release": 1}),
            )
            .await
            .unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{e:?}");
        let e = a
            .ctl(
                "publication_check",
                json!({"profile": "open", "release": "x"}),
            )
            .await
            .unwrap_err();
        assert_eq!(e.message, "Release must be a release version.");
        v
    });
    assert_eq!(ctl["status"], "blocked");
    assert_eq!(ctl["release"]["blocks_later"], 2);
    assert_eq!(ctl["release"]["decisions_later"], 1);
    h.stop();
    assert_eq!(record(h.dir.path()), before, "the op wrote");

    // the daemon's MCP proxy and the standalone server return the same value
    let s = Served::new();
    seed(s.dir.path());
    let mut proxy = McpClient::start_proxy(&s);
    proxy
        .call(
            "rdy_session",
            json!({"actor": "agent:p", "role": "reader", "view": "canon"}),
        )
        .ok();
    let args = json!({"profile": "open", "release": "1.0.0", "against": "tag:v1.0.0"});
    let daemon = proxy.call("rdy_publication_check", args.clone()).ok();
    assert_eq!(daemon["status"], "blocked");
    let (ok, cli) = s.json(&[
        "--json",
        "publication",
        "check",
        "--profile",
        "open",
        "--release",
        "1.0.0",
        "--against",
        "tag:v1.0.0",
    ]);
    assert!(ok, "{cli}");
    assert_eq!(daemon, cli);
    drop(proxy);
    let after_daemon = record(s.dir.path());
    s.stop();
    let mut alone = McpClient::start(s.dir.path());
    alone
        .call(
            "rdy_session",
            json!({"actor": "agent:p", "role": "reader", "view": "canon"}),
        )
        .ok();
    let standalone = alone.call("rdy_publication_check", args).ok();
    assert_eq!(
        daemon, standalone,
        "the daemon and the standalone server agree"
    );
    drop(alone);
    assert_eq!(
        record(s.dir.path()).len(),
        after_daemon.len(),
        "no publication record or log segment appeared"
    );
}
