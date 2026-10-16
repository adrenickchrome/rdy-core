//! pshow1: rdy_read on a proposal version.
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use serde_json::json;

#[test]
fn pshow1_rdy_read_reads_a_proposal_version() {
    let (dir, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "a"}]),
    )
    .unwrap();
    r.propose(&writer(), "work", "first", None).unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "b"}]),
    )
    .unwrap();
    let p2 = r.propose(&writer(), "work", "second", None).unwrap();
    drop(r);
    let mut c = McpClient::start(dir.path());
    let s = c.call(
        "rdy_session",
        json!({"actor": "agent:qa", "role": "reader"}),
    );
    assert!(s.code().is_none(), "{s:?}");
    let v = c.call("rdy_read", json!({"id": p2.r#ref})).ok();
    assert_eq!(v["id"], p2.r#ref.as_str(), "{v}");
    assert_eq!(v["format"], "compact");
    assert!(
        v["text"]
            .as_str()
            .unwrap()
            .starts_with(&format!("proposal {} ", p2.r#ref)),
        "{v}"
    );
    assert_eq!(v["proposal"]["version"], 2);
    assert_eq!(v["proposal"]["changes"].as_array().unwrap().len(), 2);
    assert_eq!(v["proposal"]["versions"][1]["latest"], true);
    let stable = p2.r#ref.split('@').next().unwrap();
    let latest = c
        .call("rdy_read", json!({"id": stable, "scope": "canon"}))
        .ok();
    assert_eq!(
        latest["proposal"]["version"], 2,
        "P-<id> reads the latest; scope is ignored"
    );
    let e = c.call("rdy_read", json!({"id": p2.r#ref, "format": "yaml"}));
    assert_eq!(e.code(), Some("E_BAD_INPUT"), "{e:?}");
    let e = c.call("rdy_read", json!({"id": format!("{stable}@9")}));
    assert_eq!(e.code(), Some("E_NOT_FOUND"), "{e:?}");
    let e = c.call("rdy_read", json!({"id": "c-ZZZZZ"}));
    assert_eq!(
        e.code(),
        Some("E_UNKNOWN_BLOCK"),
        "guard: block reads unchanged: {e:?}"
    );
}
