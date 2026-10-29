//! rg14 (D-ME2HD, D-33JR0, D-EPK1Z): a landing row names its assessment, and the text log prints
//! it. A landing of a proposed version records the version's assessment; `rdy_log` and
//! `rdy log --json` show it as the hash of the stored record, `null` when the landing holds none.
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_mcp::tools::*;
use rdy_mcp::view_tools::rdy_log;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use serde_json::{from_value, json};
use std::sync::Arc;

fn seed(r: &mut Rdy) {
    write(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "One."}
        ]),
    )
    .unwrap();
    land(r, "seed").unwrap();
}

/// The assessment the store holds for the proposal version `version`.
fn stored_assessment(r: &Rdy, version: &str) -> String {
    let st = r.read();
    st.reader()
        .assessment_of_proposal(&version.parse().unwrap(), st.seq())
        .unwrap()
        .expect("the propose stored an assessment")
        .to_string()
}

/// The proposal version of the newest landing, and its stored assessment.
fn head_assessment(r: &Rdy) -> String {
    let st = r.read();
    let landing = st.reader().landing(&st.canon_head().unwrap()).unwrap();
    stored_assessment(r, &landing.proposal.to_string())
}

#[test]
fn rg14_rdy_log_shows_the_landing_assessment() {
    let (_d, mut r) = project();
    seed(&mut r);
    let want = head_assessment(&r);
    let mut st = McpState::new(Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        from_value(json!({"actor": "agent:qa", "role": "writer"})).unwrap(),
    )
    .unwrap();
    let rows = rdy_log(&mut st, from_value(json!({})).unwrap()).unwrap();
    // the row names the stored assessment of the landed version (D-ME2HD)
    assert_eq!(rows[0]["assessment"], json!(want), "{}", rows[0]);
}

#[test]
fn rg14_rdy_log_text_prints_the_assessment_line() {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    seed(&mut r);
    let want = head_assessment(&r);
    r.close().unwrap();
    let (ok, rows) = s.json(&["--json", "log"]);
    assert!(ok, "{rows}");
    assert_eq!(rows[0]["assessment"], json!(want), "{rows}");
    let o = s.rdy(&["log"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    let lines: Vec<&str> = text.lines().collect();
    // four spaces and the short hash, right under the head line (this landing has no evidence)
    assert_eq!(
        lines[1],
        format!("    assessment {}", &want[..12]),
        "{text}"
    );
}
