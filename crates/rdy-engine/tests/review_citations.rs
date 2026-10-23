//! rf2: a review names its entries citations (D-8QCDH); D-XQVM1 keeps finding for a supporting
//! record. The stored review layout stays as it was.
use rdy_engine::engine::Local;
use rdy_engine::pending::PendingQuery;
use rdy_engine::semantic::case::case_hash;
use rdy_engine::semantic::submit::{CitationRequest, ReviewSubmitRequest};
use rdy_engine::tensions::api::{TensionsRequest, run_tensions};
use rdy_engine::{Engine, OpInput, Rdy, Session, W_REDACTION_QUOTES_MESSAGE, WriteRequest};
use rdy_model::Role;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn agent(name: &str, role: Role) -> Session {
    Session::agent(format!("agent:{name}"), Some(name.into()), role).unwrap()
}

fn ops(v: Value) -> Vec<OpInput> {
    rdy_engine::parse_ops(serde_json::from_value(v).unwrap()).unwrap()
}

const NOTE: &str = "these two cannot both hold";

/// rf2: request schema, review submit, tensions, proposal and pending advice expose citations
/// and no findings.
#[test]
fn rf2_citations_reach_every_review_reply() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = agent("a", Role::Writer);
    r.write(
        &w,
        WriteRequest {
            view: "a".into(),
            ops: ops(json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
                {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
                {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "A holds."},
                {"op": "create", "type": "claim", "id": "$y", "parent": "$s", "body": "A does not hold."}
            ])),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    let st = r.read();
    let q = st
        .next_request("agent:r", &BTreeSet::new())
        .unwrap()
        .expect("a request");
    let case = st.case_of(&q.key, &q.items).unwrap();
    let schema = serde_json::to_string(&case.schema).unwrap();
    assert!(
        schema.contains("citations") && !schema.contains("findings"),
        "{schema}"
    );
    let hash = case_hash(&case).to_string();
    let label = rdy_engine::semantic::candidates::request_label(&q.key);
    let block = q.items[0].block.to_string();
    drop(st);
    let info = r
        .submit_verdict(
            &agent("r", Role::Reviewer),
            ReviewSubmitRequest {
                request: label,
                case_hash: hash,
                verdict: "tension".into(),
                citations: vec![CitationRequest {
                    block,
                    note: NOTE.into(),
                }],
                severity: Some("blocking".into()),
                reason: Some("the claims contradict".into()),
            },
        )
        .unwrap();
    let reply = serde_json::to_string(&info).unwrap();
    assert!(!reply.contains("findings"), "{reply}");
    // proposal show
    let p = r
        .propose(&w, "a", "a", Some("Two claims that disagree."))
        .unwrap();
    let shown = serde_json::to_value(r.read().proposal_show(&p.r#ref).unwrap()).unwrap();
    let advised = shown["advised"][0].clone();
    assert_eq!(advised["citations"][0]["note"], NOTE, "{advised}");
    assert!(advised.get("findings").is_none(), "{advised}");
    // pending advice
    let pending = r.read().pending(&PendingQuery::default()).unwrap();
    let advice = pending
        .items
        .iter()
        .find(|i| i.r#ref.starts_with("advice:"))
        .expect("an advice item");
    assert!(advice.members[0].ends_with(NOTE), "{:?}", advice.members);
    // tensions
    let e = Local::new(r);
    let t = run_tensions(
        &e as &dyn Engine,
        &agent("r", Role::Reviewer),
        TensionsRequest {
            kind: Some("semantic".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let judged = &t["tensions"][0]["judged"];
    assert_eq!(judged["citations"][0]["note"], NOTE, "{judged}");
    assert!(judged.get("findings").is_none(), "{judged}");
}

/// rf2: the redaction warning names citations, and keeps its other words.
#[test]
fn rf2_redaction_quotes_names_citations() {
    assert_eq!(
        W_REDACTION_QUOTES_MESSAGE,
        "Quote matches use complete source strings. Partial quotes may still hold content. Snapshot issue lists and summaries, review citations, tension advice, answer notes and change-plan versions are not erased."
    );
}
