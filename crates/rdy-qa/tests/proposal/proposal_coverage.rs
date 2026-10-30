//! cc16: the coverage change on writes and proposals (FW-CH3); unapplied decisions warn (FW-CH5).
use rdy_engine::consequence::{propose_and_report, write_and_report};
use rdy_engine::{Engine, Local, Rdy};
use rdy_qa::fixtures::*;
use rdy_qa::tensions::{j_typo, put_j, try_put_j};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// `rdy_qa::fixtures::write` with an editorial justification: the fixture writes stable normative content, and an
/// unjustified change to stable content blocks its landing (D-ME2HD).
fn clarified(
    r: &mut rdy_engine::Rdy,
    view: &str,
    ops: serde_json::Value,
) -> Result<rdy_engine::WriteResult, rdy_engine::RdyError> {
    r.write(
        &writer(),
        rdy_engine::WriteRequest {
            view: view.into(),
            ops: parse_test_ops(ops)?,
            justification: Some(rdy_model::Justification::Editorial {
                class: rdy_model::EditorialClass::Clarify,
                reason: "seed the test world".into(),
            }),
            message: String::new(),
        },
    )
}

/// Doc `ops` (overview, with a stable Summary section) holds term Ledger, claim c1 (names Ledger,
/// not linked) and claim c2 (describes Ledger). Landed.
fn world(r: &mut Rdy) -> BTreeMap<String, String> {
    let w = clarified(r, "work", json!([
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
    propose_and_report(e, None, move |r| r.propose(&writer(), &name, &name, None)).unwrap()
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
    let info = r.propose(&writer(), "fix", "fix", None).unwrap();
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

/// The batch card of version `pref` as the person's queue shows it, and its Coverage fact.
fn card_coverage(e: &Local, pref: &str) -> String {
    let q = e
        .read()
        .pending(&rdy_engine::pending::PendingQuery {
            person: "human:qa".into(),
            session_ref: None,
            max_tokens: rdy_engine::pending::CARD_MAX_TOKENS,
        })
        .unwrap();
    let card = q
        .items
        .iter()
        .find(|i| i.r#ref == pref)
        .unwrap_or_else(|| panic!("the card of {pref}: {q:#?}"));
    card.facts
        .iter()
        .find(|f| f.name == "Coverage")
        .expect("the Coverage fact")
        .value
        .clone()
}

/// The reply's coverage as the card writes it: `<before>% → <after>%`.
fn reply_coverage(p: &Value) -> String {
    let c = &p["coverage"];
    format!(
        "{}% \u{2192} {}%",
        c["before"].as_u64().expect("before"),
        c["after"].as_u64().expect("after")
    )
}

/// View `v` adds one claim to the ops doc in one change, then 20 bare docs in another: the
/// claim's change.
fn claim_and_bare_docs(r: &mut Rdy, c: &BTreeMap<String, String>, v: &str) -> String {
    let claim = put_j(
        r,
        v,
        json!([{"op": "create", "type": "claim", "parent": c["$s"],
                "body": "Balances are read from the Ledger.",
                "links": {"describes": [{"target": c["$t"]}]}}]),
        // a new claim in stable content: justified, so the version is ready (D-ME2HD)
        Some(rdy_model::Justification::Editorial {
            class: rdy_model::EditorialClass::Clarify,
            reason: "read balances from the Ledger".into(),
        }),
    );
    let docs: Vec<Value> = (0..20)
        .map(|k| {
            json!({"op": "create", "type": "doc", "fields": {"name": format!("bare-{k}"),
                "title": format!("Bare {k}"), "doc_type": "overview"}})
        })
        .collect();
    put_j(r, v, Value::Array(docs), None);
    claim.change
}

/// Ruling final (FW-CV2 D-XZXCE, FW-G7c D-4PG9Y): the propose reply's coverage is the proposal
/// candidate's (canon with exactly the version's changes), worked out by the function the card
/// uses, so a subset proposal's reply and its card give the same numbers. The view's own
/// coverage, with the 20 bare docs the subset leaves out, is not the reply's.
#[test]
fn di_final_subset_proposal_reply_matches_its_card() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    let claim = claim_and_bare_docs(&mut r, &c, "sub");
    let view_after = r
        .read()
        .scope_rollup(&rdy_engine::Scope::View("sub".into()))
        .unwrap()
        .score;
    let e = Local::new(r);
    let p = propose_and_report(&e, None, move |r| {
        r.propose_subset(
            &writer(),
            "sub",
            "sub",
            Some(&[claim]),
            Some("Adds a claim on the Ledger."),
        )
    })
    .unwrap();
    assert_eq!(p["changes"], 1, "{p:#}");
    let pref = p["ref"].as_str().expect("the ref").to_string();
    assert_eq!(reply_coverage(&p), card_coverage(&e, &pref), "{p:#}");
    assert_ne!(
        p["coverage"]["after"].as_u64(),
        Some(u64::from(view_after)),
        "the view's bare docs are not in the proposal: {p:#}"
    );
    // the touched page's score is the candidate's too
    assert_eq!(p["coverage"]["pages"][0]["path"], "ops", "{p:#}");
    assert!(p.get("view_coverage").is_none(), "{p:#}");
}

/// The same for a full proposal: the reply's coverage and the card's agree.
#[test]
fn di_final_full_proposal_reply_matches_its_card() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    claim_and_bare_docs(&mut r, &c, "full");
    let e = Local::new(r);
    let p = propose_and_report(&e, None, move |r| {
        r.propose_subset(
            &writer(),
            "full",
            "full",
            None,
            Some("Adds a claim and 20 docs."),
        )
    })
    .unwrap();
    assert_eq!(p["changes"], 2, "{p:#}");
    let pref = p["ref"].as_str().expect("the ref").to_string();
    assert_eq!(reply_coverage(&p), card_coverage(&e, &pref), "{p:#}");
}
