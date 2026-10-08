//! Clearing a suspect: the repin op (sus3).
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use serde_json::json;

fn claim_on_term(r: &mut Rdy) -> (String, String, String) {
    let res = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Uses the {{$t}}."}
    ]))
    .unwrap();
    (
        res.created["$s"].clone(),
        res.created["$t"].clone(),
        res.created["$c"].clone(),
    )
}

#[test]
fn sus3_repin_clears_a_changed_suspect_as_a_recorded_change() {
    let (_d, mut r) = project();
    let (_s, t, c) = claim_on_term(&mut r);
    let scope = Scope::View("work".into());
    let (tid, cid) = (BlockId::parse(&t).unwrap(), BlockId::parse(&c).unwrap());
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": t, "body": "Balances in pence."}]),
    )
    .unwrap();
    assert_eq!(r.read().context(&scope, &cid).unwrap().suspects.len(), 1);
    let before = r.store().view("work").unwrap().changes.len();
    write(&mut r, "work", json!([{"op": "repin", "id": c}])).unwrap();
    assert!(r.read().context(&scope, &cid).unwrap().suspects.is_empty());
    assert_eq!(
        r.store().view("work").unwrap().changes.len(),
        before + 1,
        "recorded as a change"
    );
    assert_eq!(
        r.read().block(&scope, &cid).unwrap().pins[&tid],
        r.read().block(&scope, &tid).unwrap().content_hash()
    );
    check(&r, &scope).unwrap();
}

#[test]
fn sus3_repin_takes_only_targets_the_block_cites() {
    let (_d, mut r) = project();
    let (s, t, c) = claim_on_term(&mut r);
    let e = write(
        &mut r,
        "work",
        json!([{"op": "repin", "id": c, "targets": [s]}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    let e = write(
        &mut r,
        "work",
        json!([
            {"op": "update", "id": t, "body": "Balances in pence."},
            {"op": "repin", "id": c, "targets": [t]}
        ]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(e.fix.contains("separate write"), "{e}");
}

/// Review Focus 3.
#[test]
fn sus3_repin_of_a_deleted_target_says_to_remove_the_reference() {
    let (_d, mut r) = project();
    let (_s, t, c) = claim_on_term(&mut r);
    write(&mut r, "work", json!([{"op": "delete", "id": t}])).unwrap();
    let e = write(&mut r, "work", json!([{"op": "repin", "id": c}])).unwrap_err();
    assert_eq!(e.code, "E_UNKNOWN_REF", "{e}");
    assert!(e.fix.contains("remove the reference"), "{e}");
}

#[test]
fn sus3_a_superseded_decision_stays_suspect_after_repin() {
    let (_d, mut r) = project();
    let (s, _t, _c) = claim_on_term(&mut r);
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
    let c2 = write(&mut r, "work", json!([{"op": "create", "type": "claim", "parent": s, "body": format!("Per {{{{{did}}}}}.")}]))
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
    // the citation did not change, so a repin has nothing to do: it is refused, and the
    // suspect stays (a superseded or revoked suspect clears only when the citation changes)
    let e = write(&mut r, "work", json!([{"op": "repin", "id": c2}])).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(e.fix.contains("only when the citation changes"), "{e}");
    let sus: Vec<_> = r
        .read()
        .suspects(&Scope::Canon)
        .unwrap()
        .into_iter()
        .filter(|x| x.block.to_string() == c2)
        .collect();
    assert_eq!(sus.len(), 1, "{sus:?}");
    assert_eq!(sus[0].reason, "superseded");
}

#[test]
fn sus3_the_repin_op_is_documented() {
    assert!(
        rdy_mcp::tools::WRITE_OPS
            .iter()
            .any(|(n, s)| *n == "repin" && s.contains("\"targets\""))
    );
}

/// A repin that would change no pin is refused per op, like a no-op rebalance, not committed as
/// an empty change.
#[test]
fn sus3_a_repin_that_changes_no_pin_is_refused() {
    let (_d, mut r) = project();
    let (_s, t, c) = claim_on_term(&mut r);
    let before = r.store().view("work").unwrap().changes.len();
    // already pins the current version of what it cites
    let e = write(&mut r, "work", json!([{"op": "repin", "id": c}])).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(
        e.message.contains("already pins the current version"),
        "{e}"
    );
    assert!(e.message.contains(&c), "{e}");
    assert!(e.fix.contains("only when the citation changes"), "{e}");
    // cites nothing
    let e = write(&mut r, "work", json!([{"op": "repin", "id": t}])).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert_eq!(r.store().view("work").unwrap().changes.len(), before);
    // one op that would change a pin does not excuse a sibling that would not
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": t, "body": "Balances in pence."}]),
    )
    .unwrap();
    let before = r.store().view("work").unwrap().changes.len();
    let e = write(
        &mut r,
        "work",
        json!([{"op": "repin", "id": c}, {"op": "repin", "id": t}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(e.message.contains(&t), "names the no-op block: {e}");
    assert_eq!(r.store().view("work").unwrap().changes.len(), before);
    // the repin that does change a pin still lands
    write(&mut r, "work", json!([{"op": "repin", "id": c}])).unwrap();
    assert_eq!(r.store().view("work").unwrap().changes.len(), before + 1);
}
