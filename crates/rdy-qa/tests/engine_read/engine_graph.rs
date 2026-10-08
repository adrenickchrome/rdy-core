//! Graph and query output: duplicate suspects (sus1), question summaries (sum1), pins (pin1).
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::json;

fn section_project() -> (tempfile::TempDir, Rdy, String) {
    let (d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Log", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}}
    ]))
    .unwrap();
    let s = res.created["$s"].clone();
    (d, r, s)
}

#[test]
fn sus1_a_block_citing_a_target_twice_is_one_suspect() {
    let (_d, mut r, s) = section_project();
    let did = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: s.clone(),
                statement: "Wallets move".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "work").unwrap();
    // a reference and a typed link to the same decision
    let c = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": s,
        "body": format!("Per {{{{{did}}}}}."), "links": {"decided_by": [{"target": did}]}}]),
    )
    .unwrap()
    .created["op0"]
        .clone();
    r.supersede(
        &writer(),
        "work",
        &did,
        None,
        DecisionInput {
            parent: s.clone(),
            statement: "Wallets stay".into(),
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let mine: Vec<_> = r
        .read()
        .suspects(&Scope::Canon)
        .unwrap()
        .into_iter()
        .filter(|x| x.block.to_string() == c)
        .collect();
    assert_eq!(mine.len(), 1, "{mine:?}");
    assert_eq!(mine[0].reason, "superseded");
    let ctx = r
        .read()
        .context(&Scope::Canon, &BlockId::parse(&c).unwrap())
        .unwrap();
    assert_eq!(ctx.suspects.len(), 1, "{:?}", ctx.suspects);
}

#[test]
fn sum1_a_questions_summary_is_its_prompt() {
    let (_d, mut r, s) = section_project();
    let q = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "question", "parent": s,
        "fields": {"prompt": "Who approves refunds?"}}]),
    )
    .unwrap()
    .created["op0"]
        .clone();
    let scope = Scope::View("work".into());
    let block = r
        .read()
        .block(&scope, &BlockId::parse(&q).unwrap())
        .unwrap();
    assert_eq!(summary(&block), "Who approves refunds?");
    let m = r.read().materialized(&scope).unwrap().clone();
    assert_eq!(summary_in(&block, &m), "Who approves refunds?");
}

#[test]
fn pin1_pins_print_as_hex_in_json() {
    let (_d, mut r, s) = section_project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "term", "id": "$t", "parent": s, "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "claim", "id": "$c", "parent": s, "body": "Uses the {{$t}}."}
    ]))
    .unwrap();
    let scope = Scope::View("work".into());
    let c = BlockId::parse(&res.created["$c"]).unwrap();
    let ctx = serde_json::to_value(r.read().context(&scope, &c).unwrap()).unwrap();
    let edge = ctx["outgoing"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "References")
        .unwrap()
        .clone();
    let pin = edge["pin"]
        .as_str()
        .unwrap_or_else(|| panic!("pin is not a string: {edge}"));
    assert_eq!(pin.len(), 64, "{pin}");
    assert!(
        pin.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "{pin}"
    );
    let t = r
        .read()
        .block(&scope, &BlockId::parse(&res.created["$t"]).unwrap())
        .unwrap();
    assert_eq!(pin, t.content_hash().to_string());
}
