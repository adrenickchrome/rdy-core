//! psshow1: a proposal version's summary (FW-G7c) shows in `rdy show P-x@n` and in `rdy_read`
//! on that version.
use rdy_engine::{ProposalInfo, Rdy};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use serde_json::json;

const SUMMARY: &str = "Adds claim a for the approver.";

/// A landed seed doc, then one proposal from view "work" that carries [`SUMMARY`].
fn summarised(r: &mut Rdy) -> ProposalInfo {
    let (_doc, section, _c) = seed_doc(r, "seed");
    land(r, "seed").unwrap();
    write(
        r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "a"}]),
    )
    .unwrap();
    let p = r
        .propose_subset(&writer(), "work", "first", None, Some(SUMMARY))
        .unwrap();
    assert_eq!(
        r.read().proposal_show(&p.r#ref).unwrap().summary.as_deref(),
        Some(SUMMARY)
    );
    p
}

#[test]
fn psshow1_rdy_show_prints_the_summary() {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let p = summarised(&mut r);
    r.close().unwrap();
    let out = s.rdy(&["show", &p.r#ref]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{text}");
    assert!(text.contains(&format!("summary: {SUMMARY}\n")), "{text}");
}

#[test]
fn psshow1_rdy_read_on_a_proposal_version_carries_the_summary() {
    let (dir, mut r) = project();
    let p = summarised(&mut r);
    drop(r);
    let mut c = McpClient::start(dir.path());
    let ses = c.call(
        "rdy_session",
        json!({"actor": "agent:qa", "role": "reader"}),
    );
    assert!(ses.code().is_none(), "{ses:?}");
    let v = c.call("rdy_read", json!({"id": p.r#ref})).ok();
    assert_eq!(v["proposal"]["summary"], SUMMARY, "{v}");
}
