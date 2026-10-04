use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use serde_json::json;

fn section_project() -> (tempfile::TempDir, Rdy, String) {
    let (d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "claim", "parent": "$s", "body": "first"}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    let s = res.created["$s"].clone();
    (d, r, s)
}

fn children(r: &mut Rdy, scope: &Scope, parent: &str) -> Vec<(String, String)> {
    let p = rdy_model::BlockId::parse(parent).unwrap();
    r.materialized(scope)
        .unwrap()
        .children(Some(&p))
        .iter()
        .map(|b| (b.order.clone(), b.body.clone()))
        .collect()
}

#[test]
fn ord1_append_after_a_tie_goes_after_the_whole_tie() {
    let (_d, mut r, s) = section_project();
    for (view, body) in [("va", "from a"), ("vb", "from b")] {
        write(
            &mut r,
            view,
            json!([{"op": "create", "type": "claim", "parent": s, "body": body}]),
        )
        .unwrap();
    }
    land(&mut r, "va").unwrap();
    land(&mut r, "vb").unwrap();
    let kids = children(&mut r, &Scope::Canon, &s);
    assert_eq!(
        kids[1].0, kids[2].0,
        "the two views produced tied keys: {kids:?}"
    );
    // insert after the first twin (by (order, id)) — must not panic, must land after both twins
    let first_twin = {
        let p = rdy_model::BlockId::parse(&s).unwrap();
        r.materialized(&Scope::Canon).unwrap().children(Some(&p))[1]
            .id
            .to_string()
    };
    write(&mut r, "vc", json!([{"op": "create", "type": "claim", "parent": s, "after": first_twin, "body": "after twin"}])).unwrap();
    let kids = children(&mut r, &Scope::View("vc".into()), &s);
    assert_eq!(kids.last().unwrap().1, "after twin", "{kids:?}");
    check(&mut r, &Scope::View("vc".into())).unwrap();
}

#[test]
fn ord3_inserting_at_one_spot_hits_the_cap_then_rebalance_restores_room() {
    let (_d, mut r, s) = section_project();
    let p = rdy_model::BlockId::parse(&s).unwrap();
    let anchor = r.materialized(&Scope::Canon).unwrap().children(Some(&p))[0]
        .id
        .to_string();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": s, "body": "end"}]),
    )
    .unwrap();
    let mut refused = None;
    for i in 0..300 {
        match write(
            &mut r,
            "work",
            json!([{"op": "create", "type": "claim", "parent": s, "after": anchor, "body": format!("n{i}")}]),
        ) {
            Ok(_) => {}
            Err(e) => {
                refused = Some(e);
                break;
            }
        }
    }
    let e = refused.expect("the cap is reached");
    assert_eq!(e.code, "E_ORDER_DEPTH", "{e}");
    assert!(e.fix.contains("rebalance"), "{e}");
    let before: Vec<String> = children(&mut r, &Scope::View("work".into()), &s)
        .into_iter()
        .map(|(_, b)| b)
        .collect();
    write(&mut r, "work", json!([{"op": "rebalance", "parent": s}])).unwrap();
    let after = children(&mut r, &Scope::View("work".into()), &s);
    assert_eq!(
        after.iter().map(|(_, b)| b.clone()).collect::<Vec<_>>(),
        before,
        "order unchanged"
    );
    assert!(after.iter().all(|(k, _)| k.len() <= 4), "{after:?}");
    write(&mut r, "work", json!([{"op": "create", "type": "claim", "parent": s, "after": anchor, "body": "fits again"}])).unwrap();
    check(&mut r, &Scope::View("work".into())).unwrap();
}

#[test]
fn ord3_rebalance_keeps_frozen_decisions_as_anchors() {
    let (_d, mut r, s) = section_project();
    let d = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: s.clone(),
                statement: "Frozen".into(),
                ..Default::default()
            },
        )
        .unwrap();
    land(&mut r, "work").unwrap();
    let did = rdy_model::BlockId::parse(&d.created["$decision"]).unwrap();
    let frozen_order = r.block(&Scope::Canon, &did).unwrap().order;
    // a rebalance re-keys every movable child, so it can't share a write with an op that already touched one
    let e = write(&mut r, "work", json!([{"op": "create", "type": "claim", "parent": s, "body": "x"}, {"op": "rebalance", "parent": s}])).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": s, "body": "x"}]),
    )
    .unwrap();
    write(&mut r, "work", json!([{"op": "rebalance", "parent": s}])).unwrap();
    assert_eq!(
        r.block(&Scope::View("work".into()), &did).unwrap().order,
        frozen_order
    );
    land(&mut r, "work").unwrap();
    check(&mut r, &Scope::Canon).unwrap();
}

#[test]
fn ord3_legacy_long_keys_are_repaired_by_rebalance() {
    let (_d, mut r) = project();
    let long = format!("z{}", "z".repeat(29)); // an M1-era key over the cap (all valid digits, 30 long)
    raw_commit(
        &mut r,
        "work",
        vec![
            raw(
                "d-AAAAA",
                "doc",
                None,
                "V",
                &[("name", "legacy"), ("title", "L"), ("doc_type", "glossary")],
                "",
            ),
            raw(
                "s-AAAAA",
                "section",
                Some("d-AAAAA"),
                "V",
                &[("title", "Terms")],
                "",
            ),
            raw("c-AAAAA", "claim", Some("s-AAAAA"), &long, &[], "long key"),
        ],
    );
    let e = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": "c-AAAAA", "body": "edited"}]),
    )
    .unwrap_err();
    assert!(
        e.issues.iter().any(|i| i.code == "E_ORDER_KEY") || e.code == "E_ORDER_KEY",
        "{e:?}"
    );
    assert!(
        e.fix.contains("rebalance") || e.issues.iter().any(|i| i.fix.contains("rebalance")),
        "{e:?}"
    );
    write(
        &mut r,
        "work",
        json!([{"op": "rebalance", "parent": "s-AAAAA"}]),
    )
    .unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": "c-AAAAA", "body": "edited"}]),
    )
    .unwrap();
    check(&mut r, &Scope::View("work".into())).unwrap();
}

#[test]
fn ord3_rebalance_refuses_a_frozen_decision_with_an_invalid_key() {
    let (_d, mut r) = project();
    let long = "z".repeat(30); // an M1-era key over the cap on a decision that can no longer change
    raw_commit(
        &mut r,
        "work",
        vec![
            raw(
                "d-AAAAA",
                "doc",
                None,
                "V",
                &[("name", "legacy"), ("title", "L"), ("doc_type", "glossary")],
                "",
            ),
            raw(
                "s-AAAAA",
                "section",
                Some("d-AAAAA"),
                "V",
                &[("title", "Terms")],
                "",
            ),
            raw("c-AAAAA", "claim", Some("s-AAAAA"), "V", &[], "movable"),
            raw(
                "D-AAAAA",
                "decision",
                Some("s-AAAAA"),
                &long,
                &[("status", "accepted"), ("statement", "Frozen")],
                "",
            ),
        ],
    );
    let e = write(
        &mut r,
        "work",
        json!([{"op": "rebalance", "parent": "s-AAAAA"}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_ORDER_DEPTH", "{e}");
    assert!(e.message.contains("D-AAAAA"), "{e}");
    assert!(e.fix.contains("move"), "{e}");
    // nothing changed
    let kids = children(&mut r, &Scope::View("work".into()), "s-AAAAA");
    assert_eq!(
        kids,
        vec![
            ("V".to_string(), "movable".to_string()),
            (long, String::new())
        ]
    );
}

#[test]
fn ord3_rebalance_at_the_top_level_keeps_m1_doc_names() {
    let (_d, mut r) = project();
    let long = "z".repeat(30);
    raw_commit(
        &mut r,
        "work",
        vec![
            raw(
                "d-AAAAA",
                "doc",
                None,
                &long,
                &[("title", "Alpha"), ("doc_type", "glossary")],
                "",
            ),
            raw(
                "d-BBBBB",
                "doc",
                None,
                "V",
                &[("title", "Beta"), ("doc_type", "glossary")],
                "",
            ),
        ],
    );
    let work = Scope::View("work".into());
    let id = rdy_model::BlockId::parse("d-AAAAA").unwrap();
    let name = r
        .materialized(&work)
        .unwrap()
        .name_of(&id)
        .expect("an M1 doc has a read name");
    write(&mut r, "work", json!([{"op": "rebalance"}])).unwrap();
    let b = r.block(&work, &id).unwrap();
    assert_eq!(b.field_str("name"), Some(name.as_str()), "{b:?}");
    check(&mut r, &work).unwrap();
}
