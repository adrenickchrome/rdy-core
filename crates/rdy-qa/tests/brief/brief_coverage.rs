//! cc13: the briefing's coverage section (FW-CV1, FW-CH3, FW-CV2).
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Doc `ops` (overview: needs Summary and Goals; has Summary, stable, and Empty) holds term
/// Ledger, claim c1 (names Ledger twice, unlinked, no decision), claim c2 (describes Ledger,
/// decided_by d1), question q (unanswered), decision d1 (live, applied by c2), decision d2
/// (live, nothing applies it), decision d3 (live, needs_content: false). Doc `lone` holds one
/// prose block. Landed.
fn world(r: &mut Rdy) -> BTreeMap<String, String> {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Summary", "lifecycle": "stable"}},
        {"op": "create", "type": "section", "id": "$empty", "parent": "$o", "fields": {"title": "Empty"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."},
        {"op": "create", "type": "claim", "id": "$c1", "parent": "$s", "body": "The Ledger is append-only, and the Ledger never shrinks."},
        {"op": "create", "type": "decision", "id": "$d1", "parent": "$s", "fields": {"statement": "Balances live in one store", "status": "proposed"}},
        {"op": "create", "type": "claim", "id": "$c2", "parent": "$s", "body": "Balances are kept in one place.",
         "links": {"describes": [{"target": "$t"}], "decided_by": [{"target": "$d1"}]}},
        {"op": "create", "type": "decision", "id": "$d2", "parent": "$s", "fields": {"statement": "Refunds need two approvers", "status": "proposed"}},
        {"op": "create", "type": "decision", "id": "$d3", "parent": "$s", "fields": {"statement": "Reviews happen on Fridays", "status": "proposed", "needs_content": false}},
        {"op": "create", "type": "question", "id": "$q", "parent": "$s", "fields": {"prompt": "Who approves refunds?"}},
        {"op": "create", "type": "doc", "id": "$l", "fields": {"name": "lone", "title": "Lone", "doc_type": "page"}},
        {"op": "create", "type": "prose", "parent": "$l", "body": "Nothing here."}
    ]))
    .unwrap();
    land(r, "work").unwrap(); // the landing accepts d1, d2 and d3: they are live
    w.created
}

fn id(text: &str) -> BlockId {
    BlockId::parse(text).unwrap()
}

/// A landed claim that cites a target at a pin, then a landed edit of the target. The claim and
/// the target.
fn landed_citation_then_target_changed(r: &mut Rdy) -> (BlockId, BlockId) {
    let w = write(r, "cite", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "pay", "title": "Pay", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Rules"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Wallet"}, "body": "Holds cards."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Payment uses {{$t}}."}
    ]))
    .unwrap();
    land(r, "cite").unwrap();
    let (c, t) = (id(&w.created["$c"]), id(&w.created["$t"]));
    write(
        r,
        "edit",
        json!([{"op": "update", "id": t.to_string(), "body": "Holds cards and vouchers."}]),
    )
    .unwrap();
    land(r, "edit").unwrap();
    (c, t)
}

/// The canon briefing on blocks `ids`.
fn brief_ids(r: &Rdy, ids: &[&str]) -> Briefing {
    r.read()
        .brief(
            &Scope::Canon,
            &Coverage {
                ids: ids.iter().map(|s| s.to_string()).collect(),
                path: None,
            },
            None,
        )
        .unwrap()
}

/// Doc `hub` with claim H; doc `many` with `n` claims that each cite H; landed. H's doc path.
fn hub(r: &mut Rdy, n: usize) {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "hub", "title": "Hub", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$h", "parent": "$s", "body": "H holds."}
    ]))
    .unwrap();
    let h = w.created["$h"].clone();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$m", "fields": {"name": "many", "title": "Many", "doc_type": "overview"}}),
        json!({"op": "create", "type": "section", "id": "$ms", "parent": "$m", "fields": {"title": "S"}}),
    ];
    for i in 0..n {
        ops.push(json!({"op": "create", "type": "claim", "parent": "$ms", "body": format!("Claim {i} cites {{{{{h}}}}}.")}));
    }
    write(r, "work", Value::Array(ops)).unwrap();
    land(r, "work").unwrap();
}

/// Every page of the canon briefing of the doc at `path`, cut at `max_tokens`, as JSON.
fn brief_all_pages(r: &Rdy, path: &str, max_tokens: usize) -> Vec<Value> {
    let cov = Coverage {
        ids: vec![],
        path: Some(path.to_string()),
    };
    let first = r
        .read()
        .brief(&Scope::Canon, &cov, Some(max_tokens))
        .unwrap();
    first.register(&r.briefs());
    let mut pages = vec![serde_json::to_value(&first).unwrap()];
    let mut next = first.next.clone();
    while let Some(c) = next {
        let p = r
            .read()
            .brief_page(&Scope::Canon, &r.briefs(), &c, Some(max_tokens))
            .unwrap();
        next = p.next.clone();
        pages.push(serde_json::to_value(&p).unwrap());
        assert!(pages.len() < 200, "paging does not end");
    }
    pages
}

#[test]
fn cc13_a_briefing_carries_score_pages_issues_and_confidence() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    let b = brief_ids(&r, &[&c["$c1"]]);
    let v = serde_json::to_value(&b).unwrap();
    let cov = &v["coverage"];
    assert!(cov["score"].as_u64().unwrap() < 100, "{cov}");
    assert!(cov["dimensions"].is_object(), "{cov}");
    assert_eq!(cov["pages"][0]["path"], "ops", "{cov}");
    assert_eq!(cov["confidence"], "medium", "{cov}");
    assert_eq!(cov["reasons"][0]["code"], "untied_content", "{cov}");
    assert!(
        cov["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "unlinked_mention"),
        "{cov}"
    );
    assert!(cov.get("issues_omitted").is_none(), "{cov}");
    assert!(
        cov.get("grounded").is_none(),
        "the graph measures are gone (fix forward)"
    );
}

#[test]
fn cc13_an_open_suspect_on_a_dependent_makes_the_briefing_low() {
    let (_d, mut r) = project();
    let (citer, target) = landed_citation_then_target_changed(&mut r);
    let v = serde_json::to_value(brief_ids(&r, &[&target.to_string()])).unwrap();
    assert_eq!(
        v["coverage"]["confidence"], "low",
        "{citer} depends on the target and is suspect"
    );
    assert_eq!(v["coverage"]["reasons"][0]["code"], "open_suspect");
}

#[test]
fn cc13_issues_past_the_cap_are_counted_not_listed() {
    let (_d, mut r) = project();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$o", "fields": {"name": "wide", "title": "Wide", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "S"}}),
        json!({"op": "create", "type": "term", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."}),
    ];
    for i in 0..30 {
        ops.push(json!({"op": "create", "type": "claim", "parent": "$s", "body": format!("Note {i} mentions the Ledger.")}));
    }
    write(&mut r, "work", Value::Array(ops)).unwrap();
    land(&mut r, "work").unwrap();
    let b = r
        .read()
        .brief(
            &Scope::Canon,
            &Coverage {
                ids: vec![],
                path: Some("wide".into()),
            },
            Some(100_000),
        )
        .unwrap();
    let v = serde_json::to_value(&b).unwrap();
    let cov = &v["coverage"];
    let shown = cov["issues"].as_array().unwrap().len();
    assert_eq!(shown, rdy_engine::brief::BRIEF_ISSUES_CAP, "{cov}");
    assert!(cov["issues_omitted"].as_u64().unwrap() > 0, "{cov}");
}

/// guard (pre-flight): the section is already never paged, so this passes before the change; it
/// pins that the new section stays the same on every page.
#[test]
fn cc13_every_page_carries_the_same_section() {
    let (_d, mut r) = project();
    hub(&mut r, 60);
    let pages = brief_all_pages(&r, "hub", 300); // a doc-wide briefing cut into pages
    assert!(pages.len() > 1);
    assert!(pages[0]["coverage"].is_object(), "{}", pages[0]);
    for p in &pages[1..] {
        assert_eq!(p["coverage"], pages[0]["coverage"]);
    }
}
