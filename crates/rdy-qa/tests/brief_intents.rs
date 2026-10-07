//! ibrf1: a briefing's inflight lists the open intents on its blocks or their areas.
use rdy_engine::brief::{Coverage, PAGE_SECTIONS};
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::packs::qa_project;
use serde_json::json;

#[test]
fn ibrf1_intents_on_covered_blocks_or_their_area_are_listed_and_not_in_the_token() {
    let (_d, mut r) = qa_project();
    r.new_doc(
        &writer(),
        "seed",
        "payments/fees",
        Some("glossary"),
        Some("Fees"),
    )
    .unwrap();
    let doc = r
        .read()
        .resolve_path(&Scope::View("seed".into()), "payments/fees")
        .unwrap()
        .to_string();
    let res = write(&mut r, "seed", json!([
        {"op": "create", "type": "section", "id": "$s", "parent": doc, "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "fee"}
    ])).unwrap();
    land(&mut r, "seed").unwrap();
    let claim = res.created["$c"].clone();
    let cov = Coverage {
        ids: vec![claim.clone()],
        path: None,
    };
    let work = Scope::View("work".into());
    let before = r.read().brief(&work, &cov, None).unwrap();
    assert!(before.inflight.intents.is_empty());
    let a = agent("agent:a", Some("ra"));
    let ia = r
        .intent_post(&a, None, Some("payments"), &[], "fees", None)
        .unwrap();
    let ib = r
        .intent_post(
            &agent("agent:b", Some("rb")),
            None,
            None,
            std::slice::from_ref(&claim),
            "wording",
            None,
        )
        .unwrap();
    r.intent_post(
        &agent("agent:c", Some("work")),
        Some("work"),
        None,
        std::slice::from_ref(&claim),
        "mine",
        None,
    )
    .unwrap();
    let b = r.read().brief(&work, &cov, None).unwrap();
    let ids: Vec<&str> = b.inflight.intents.iter().map(|i| i.id.as_str()).collect();
    let mut want = vec![ia.id.as_str(), ib.id.as_str()];
    want.sort();
    assert_eq!(ids, want, "the briefing's own view's intent is left out");
    assert!(
        b.inflight
            .intents
            .iter()
            .all(|i| i.blocks == vec![claim.clone()])
    );
    assert_eq!(
        b.base.token, before.base.token,
        "intents are not part of the token"
    );
    r.intent_withdraw(&a, &ia.id).unwrap();
    assert_eq!(
        r.read()
            .brief(&work, &cov, None)
            .unwrap()
            .inflight
            .intents
            .len(),
        1
    );
}

#[test]
fn ibrf1_paging_has_an_intents_section_before_the_base() {
    assert_eq!(PAGE_SECTIONS.len(), 9);
    assert_eq!(PAGE_SECTIONS[6], "inflight.proposals");
    assert_eq!(PAGE_SECTIONS[7], "inflight.intents");
    assert_eq!(PAGE_SECTIONS[8], "base.blocks");
}
