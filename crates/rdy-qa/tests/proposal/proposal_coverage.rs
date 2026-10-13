//! cc16: the coverage change on writes and proposals (FW-CH3); unapplied decisions warn (FW-CH5).
use rdy_engine::consequence::{propose_and_report, write_and_report};
use rdy_engine::{Engine, Local, Rdy};
use rdy_qa::fixtures::*;
use rdy_qa::tensions::{j_typo, put_j, try_put_j};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Doc `ops` (overview, with a stable Summary section) holds term Ledger, claim c1 (names Ledger,
/// not linked) and claim c2 (describes Ledger). Landed.
fn world(r: &mut Rdy) -> BTreeMap<String, String> {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Summary", "lifecycle": "stable"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."},
        {"op": "create", "type": "claim", "id": "$c1", "parent": "$s", "body": "The Ledger is append-only, and the Ledger never shrinks."},
        {"op": "create", "type": "claim", "id": "$c2", "parent": "$s", "body": "Balances are kept in one place.",
         "links": {"describes": [{"target": "$t"}]}}
    ]))
    .unwrap();
    land(r, "work").unwrap();
    w.created
}

/// `write_and_report` of `ops` as agent `view` in view `view` (briefed as needed).
fn write_and_report_json(e: &Local, view: &str, ops: Value) -> Value {
    let name = view.to_string();
    write_and_report(e, view, None, move |r| try_put_j(r, &name, ops, j_typo())).unwrap()
}

/// `propose_and_report` of view `view`.
fn propose_json(e: &Local, view: &str) -> Value {
    let name = view.to_string();
    propose_and_report(e, None, move |r| r.propose(&writer(), &name, &name)).unwrap()
}

#[test]
fn cc16_a_write_reports_the_coverage_change_of_its_pages() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    let e = Local::new(r);
    let v = write_and_report_json(
        &e,
        "fix",
        json!([
            {"op": "update", "id": c["$c1"], "body": "The Ledger is append-only.", "links": {"describes": [{"target": c["$t"]}]}}
        ]),
    );
    let cov = &v["coverage"];
    assert!(cov["after"].as_u64() > cov["before"].as_u64(), "{v:#}");
    assert_eq!(cov["pages"][0]["path"], "ops", "{cov}");
    assert!(
        cov["pages"][0]["after"].as_u64() > cov["pages"][0]["before"].as_u64(),
        "{cov}"
    );
    assert!(v.get("coverage_error").is_none(), "{v:#}");
}

#[test]
fn cc16_a_proposal_that_lands_an_unapplied_decision_warns_and_still_proposes() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    rdy_qa::tensions::try_decide_in(
        &mut r,
        "dec",
        &rdy_model::BlockId::parse(&c["$s"]).unwrap(),
        "Refunds expire after a year",
    )
    .unwrap();
    let e = Local::new(r);
    let p = propose_json(&e, "dec");
    let w: Vec<&Value> = p["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "W_UNAPPLIED_DECISION")
        .collect();
    assert_eq!(w.len(), 1, "{p:#}");
    assert!(
        w[0]["message"]
            .as_str()
            .unwrap()
            .ends_with("lands with no content that applies it")
    );
    let d = w[0]["block"].as_str().unwrap();
    assert_eq!(
        w[0]["fix"],
        format!(
            "write the content that carries it with implements: {d}, or link that content to it with decided_by; for a process ruling, set needs_content: false"
        )
    );
    assert_eq!(w[0]["severity"], "warning");
    assert!(
        p["coverage"]["before"].is_u64() && p["coverage"]["after"].is_u64(),
        "{p:#}"
    );
}

/// guard: a decision with needs_content: false, or one applied in the same proposal, does not warn.
#[test]
fn cc16_an_exempt_or_applied_decision_does_not_warn() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    let s = c["$s"].clone();
    put_j(
        &mut r,
        "exempt",
        json!([
            {"op": "create", "type": "decision", "parent": s, "fields": {"statement": "Standups are short", "status": "proposed", "needs_content": false}}
        ]),
        j_typo(),
    );
    put_j(
        &mut r,
        "applied",
        json!([
            {"op": "create", "type": "decision", "id": "$x", "parent": s, "fields": {"statement": "Refunds expire after a year", "status": "proposed"}},
            {"op": "create", "type": "claim", "parent": s, "body": "A refund older than a year is refused.", "links": {"decided_by": [{"target": "$x"}]}}
        ]),
        j_typo(),
    );
    let e = Local::new(r);
    for view in ["exempt", "applied"] {
        let p = propose_json(&e, view);
        assert!(
            p["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .all(|w| w["code"] != "W_UNAPPLIED_DECISION"),
            "{view}: {p}"
        );
    }
}

/// A detector that always fails: the tension set, which coverage reads, cannot be built.
struct Failing;
impl rdy_engine::tensions::Detector for Failing {
    fn kind(&self) -> rdy_engine::tensions::TensionKind {
        rdy_engine::tensions::TensionKind::EditCollision
    }
    fn detect(
        &self,
        _cx: &rdy_engine::tensions::DetectCx<'_>,
        _seeds: Option<&std::collections::BTreeSet<rdy_model::BlockId>>,
        _out: &mut Vec<rdy_engine::tensions::Found>,
    ) -> Result<(), rdy_engine::RdyError> {
        Err(rdy_engine::RdyError::new(
            rdy_engine::E_INTERNAL,
            "boom",
            "a test detector",
        ))
    }
}
static FAILING: Failing = Failing;
static FAILING_DETS: [&dyn rdy_engine::tensions::Detector; 1] = [&FAILING];

/// A coverage read error never fails a write reply: the reply carries `coverage_error` instead
/// of `coverage`, and the write stands.
#[test]
fn cc16_a_coverage_read_error_never_fails_the_reply() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    r.set_detectors_for_tests(&FAILING_DETS);
    let e = Local::new(r);
    let v = write_and_report_json(
        &e,
        "fix",
        json!([{"op": "update", "id": c["$c1"], "body": "The Ledger is append-only."}]),
    );
    assert!(v.get("coverage").is_none(), "{v:#}");
    assert!(
        v["coverage_error"]
            .as_str()
            .is_some_and(|m| m.starts_with("E_INTERNAL: ") && m.contains("boom")),
        "{v:#}"
    );
    assert!(v["change"].is_string(), "the write stands: {v:#}");
}

/// A tension read error never fails a propose reply (the ruling of 2026-10-11): the proposal is
/// stored and published before the tensions are read, so the reply keeps it, with no tensions,
/// `tensions_error` and `coverage_error`, and no `coverage`.
#[test]
fn cc16_a_tension_read_error_never_fails_the_propose_reply() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    put_j(
        &mut r,
        "fix",
        json!([{"op": "update", "id": c["$c1"], "body": "The Ledger is append-only."}]),
        j_typo(),
    );
    r.set_detectors_for_tests(&FAILING_DETS);
    let e = Local::new(r);
    let p = propose_json(&e, "fix");
    let id = p["id"].as_str().expect("the proposal id");
    assert_eq!(p["tensions"], json!([]), "{p:#}");
    for key in ["tensions_error", "coverage_error"] {
        assert!(
            p[key]
                .as_str()
                .is_some_and(|m| m.starts_with("E_INTERNAL: ") && m.contains("boom")),
            "{key}: {p:#}"
        );
    }
    assert!(p.get("coverage").is_none(), "{p:#}");
    // the proposal is stored: a later read finds it
    let found = e.read().reader().proposal(&id.parse().unwrap()).unwrap();
    assert_eq!(found.view, "fix");
}

/// The proposal's coverage change is computed after the write, not on the writer: the
/// `Rdy::propose` result carries none; the reply of `propose_and_report` does.
#[test]
fn cc16_the_proposal_coverage_is_read_after_the_write() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    put_j(
        &mut r,
        "fix",
        json!([{"op": "update", "id": c["$c1"], "body": "The Ledger is append-only.", "links": {"describes": [{"target": c["$t"]}]}}]),
        j_typo(),
    );
    let info = r.propose(&writer(), "fix", "fix").unwrap();
    let on_writer = serde_json::to_value(&info).unwrap();
    assert!(on_writer.get("coverage").is_none(), "{on_writer:#}");
    let e = Local::new(r);
    let p = propose_json(&e, "fix");
    assert!(
        p["coverage"]["before"].is_u64() && p["coverage"]["after"].is_u64(),
        "{p:#}"
    );
    assert!(p["coverage"]["pages"].is_array(), "{p:#}");
}
