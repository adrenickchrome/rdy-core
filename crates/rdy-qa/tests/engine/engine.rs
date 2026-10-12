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
    r.read()
        .materialized(scope)
        .unwrap()
        .children(Some(&p))
        .iter()
        .map(|b| (b.order.clone(), b.body.clone()))
        .collect()
}

#[test]
fn ord1_an_insert_after_a_tied_claim_lands_directly_after_it() {
    let (_d, mut r, s) = section_project();
    for (view, body) in [("va", "from a"), ("vb", "from b")] {
        write(
            &mut r,
            view,
            json!([{"op": "create", "type": "claim", "parent": s, "body": body}]),
        )
        .unwrap();
    }
    // the two claims share an order key (made by hand: see `land_with_a_tie`)
    land_with_a_tie(&mut r, "va");
    land_with_a_tie(&mut r, "vb");
    let kids = children(&mut r, &Scope::Canon, &s);
    assert_eq!(
        kids[1].0, kids[2].0,
        "the two views produced tied keys: {kids:?}"
    );
    // insert after the first twin (by (order, id)): it lands directly after it, between the
    // twins (an exact `after`, also under a tie; ruling of 2026-10-12 on D-8EH6E)
    let first_twin = {
        let p = rdy_model::BlockId::parse(&s).unwrap();
        r.read()
            .materialized(&Scope::Canon)
            .unwrap()
            .children(Some(&p))[1]
            .id
            .to_string()
    };
    let twins: Vec<String> = children(&mut r, &Scope::Canon, &s)
        .into_iter()
        .map(|(_, body)| body)
        .collect();
    write(&mut r, "vc", json!([{"op": "create", "type": "claim", "parent": s, "after": first_twin, "body": "after twin"}])).unwrap();
    let kids: Vec<String> = children(&mut r, &Scope::View("vc".into()), &s)
        .into_iter()
        .map(|(_, body)| body)
        .collect();
    let mut want = twins;
    want.insert(2, "after twin".to_string());
    assert_eq!(kids, want);
    check(&r, &Scope::View("vc".into())).unwrap();
}

#[test]
fn ord3_inserting_at_one_spot_hits_the_cap_then_rebalance_restores_room() {
    let (_d, mut r, s) = section_project();
    let p = rdy_model::BlockId::parse(&s).unwrap();
    let anchor = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .children(Some(&p))[0]
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
    check(&r, &Scope::View("work".into())).unwrap();
}

#[test]
fn ord3_rebalance_keeps_frozen_decisions_as_anchors() {
    let (_d, mut r, s) = section_project();
    let p = rdy_model::BlockId::parse(&s).unwrap();
    let first = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .children(Some(&p))[0]
        .id
        .to_string();
    // movables before the anchor, one of them squeezed in between two others
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": s, "body": "second"}]),
    )
    .unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": s, "after": first, "body": "mid"}]),
    )
    .unwrap();
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
    let frozen_order = r.read().block(&Scope::Canon, &did).unwrap().order;
    // a rebalance re-keys every movable child, so it can't share a write with an op that already touched one
    let e = write(&mut r, "work", json!([{"op": "create", "type": "claim", "parent": s, "body": "x"}, {"op": "rebalance", "parent": s}])).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    // movables after the anchor, one squeezed in between the anchor and another
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": s, "body": "y"}]),
    )
    .unwrap();
    write(&mut r, "work", json!([{"op": "create", "type": "claim", "parent": s, "after": did.to_string(), "body": "x"}])).unwrap();
    let work = Scope::View("work".into());
    let ids = |r: &mut Rdy| -> Vec<(String, String, String)> {
        r.read()
            .materialized(&work)
            .unwrap()
            .children(Some(&p))
            .iter()
            .map(|b| (b.id.to_string(), b.order.clone(), b.body.clone()))
            .collect()
    };
    let before = ids(&mut r);
    let order_of =
        |v: &[(String, String, String)]| v.iter().map(|(id, _, _)| id.clone()).collect::<Vec<_>>();
    let movable_keys = |v: &[(String, String, String)]| {
        v.iter()
            .filter(|(id, _, _)| *id != did.to_string())
            .map(|(_, k, _)| k.clone())
            .collect::<Vec<_>>()
    };
    let anchor_at = before
        .iter()
        .position(|(id, _, _)| *id == did.to_string())
        .unwrap();
    assert!(
        anchor_at > 0 && anchor_at < before.len() - 1,
        "movables on both sides: {before:?}"
    );
    write(&mut r, "work", json!([{"op": "rebalance", "parent": s}])).unwrap();
    let after = ids(&mut r);
    assert_eq!(order_of(&after), order_of(&before), "child order unchanged");
    assert_ne!(
        movable_keys(&after),
        movable_keys(&before),
        "movable keys re-keyed: {after:?}"
    );
    assert!(
        movable_keys(&after).iter().all(|k| k.len() <= 4),
        "{after:?}"
    );
    assert_eq!(r.read().block(&work, &did).unwrap().order, frozen_order);
    land(&mut r, "work").unwrap();
    check(&r, &Scope::Canon).unwrap();
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
    check(&r, &Scope::View("work".into())).unwrap();
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
        .read()
        .materialized(&work)
        .unwrap()
        .name_of(&id)
        .expect("an M1 doc has a read name");
    write(&mut r, "work", json!([{"op": "rebalance"}])).unwrap();
    let b = r.read().block(&work, &id).unwrap();
    assert_eq!(b.field_str("name"), Some(name.as_str()), "{b:?}");
    check(&r, &work).unwrap();
}

#[test]
fn ord3_rebalance_between_tied_frozen_decisions_is_order_depth() {
    let (_d, mut r) = project();
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
            raw(
                "D-AAAAA",
                "decision",
                Some("s-AAAAA"),
                "V",
                &[("status", "accepted"), ("statement", "A")],
                "",
            ),
            // movable (proposed), and between the two frozen ones by id
            raw(
                "D-BBBBB",
                "decision",
                Some("s-AAAAA"),
                "V",
                &[("status", "proposed"), ("statement", "B")],
                "",
            ),
            raw(
                "D-CCCCC",
                "decision",
                Some("s-AAAAA"),
                "V",
                &[("status", "accepted"), ("statement", "C")],
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
    assert!(!e.fix.contains("\"op\":\"rebalance\""), "{e}");
    assert!(
        e.fix
            .contains("move the movable siblings under s-AAAAA to another parent"),
        "{e}"
    );
}

#[test]
fn ord3_a_rebalance_that_changes_nothing_is_refused() {
    let (_d, mut r, s) = section_project();
    // one appended child: already tidy
    let e = write(&mut r, "work", json!([{"op": "rebalance", "parent": s}])).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(
        e.message
            .contains(&format!("nothing to rebalance under {s}")),
        "{e}"
    );
    // no children at all
    let res = write(&mut r, "work", json!([{"op": "create", "type": "section", "id": "$e", "parent": s, "fields": {"title": "Empty"}}])).unwrap();
    let empty = res.created["$e"].clone();
    let e = write(
        &mut r,
        "work",
        json!([{"op": "rebalance", "parent": empty}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    // a no-op rebalance alongside a real op is fine
    write(&mut r, "work", json!([{"op": "rebalance", "parent": empty}, {"op": "create", "type": "claim", "parent": s, "body": "more"}])).unwrap();
}

#[test]
fn ord3_rebalance_landed_under_an_open_proposal_warns_siblings_rekeyed() {
    let (_d, mut r, s) = section_project();
    let p = rdy_model::BlockId::parse(&s).unwrap();
    let first = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .children(Some(&p))[0]
        .id
        .to_string();
    // make the parent untidy in canon so a rebalance has work to do
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": s, "body": "second"}]),
    )
    .unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": s, "after": first, "body": "mid"}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    // B places a block relative to the current keys
    // vb is pinned: a landing rebases open unpinned views onto the new keys (reb1), so only a
    // pinned view still sees siblings re-keyed under it (Task 8 pins the unpinned counterpart)
    let ch = r.read().canon_head();
    r.store_mut()
        .view_create("vb", ch, None, true, &writer().provenance())
        .unwrap();
    r.invalidate();
    write(
        &mut r,
        "vb",
        json!([{"op": "create", "type": "claim", "parent": s, "after": first, "body": "from b"}]),
    )
    .unwrap();
    // A rebalances and lands first
    write(&mut r, "va", json!([{"op": "rebalance", "parent": s}])).unwrap();
    land(&mut r, "va").unwrap();
    let prop = r.propose(&writer(), "vb", "b").unwrap();
    let w = prop
        .warnings
        .iter()
        .find(|i| i.code == "W_SIBLINGS_REKEYED")
        .unwrap_or_else(|| panic!("{:?}", prop.warnings));
    assert_eq!(w.severity, rdy_schema::Severity::Warning);
    assert!(w.message.contains(&s), "{w:?}");
    let landed = r.land_report(&approver(), &prop.id).unwrap();
    assert!(
        landed
            .warnings
            .iter()
            .any(|i| i.code == "W_SIBLINGS_REKEYED"),
        "{:?}",
        landed.warnings
    );
    check(&r, &Scope::Canon).unwrap();
}

/// W_SIBLINGS_REKEYED is broader than rebalance: any landed re-key of a sibling (here an
/// ordinary move of one block) warns a proposal that places blocks under that parent.
#[test]
fn ord3_a_landed_move_of_one_sibling_also_warns_siblings_rekeyed() {
    let (_d, mut r, s) = section_project();
    let p = rdy_model::BlockId::parse(&s).unwrap();
    let first = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .children(Some(&p))[0]
        .id
        .to_string();
    let res = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": s, "body": "second"}]),
    )
    .unwrap();
    let second = res.created["$c"].clone();
    land(&mut r, "work").unwrap();
    // vb is pinned: a landing rebases open unpinned views onto the new keys (reb1), so only a
    // pinned view still sees siblings re-keyed under it (Task 8 pins the unpinned counterpart)
    let ch = r.read().canon_head();
    r.store_mut()
        .view_create("vb", ch, None, true, &writer().provenance())
        .unwrap();
    r.invalidate();
    write(
        &mut r,
        "vb",
        json!([{"op": "create", "type": "claim", "parent": s, "after": first, "body": "from b"}]),
    )
    .unwrap();
    // A moves `first` after `second` (a re-key, no rebalance) and lands first
    write(
        &mut r,
        "va",
        json!([{"op": "move", "id": first, "parent": s, "after": second}]),
    )
    .unwrap();
    land(&mut r, "va").unwrap();
    let prop = r.propose(&writer(), "vb", "b").unwrap();
    assert!(
        prop.warnings.iter().any(|i| i.code == "W_SIBLINGS_REKEYED"),
        "{:?}",
        prop.warnings
    );
}

#[test]
fn ord3_a_frozen_decision_with_a_bad_key_is_told_to_move_its_siblings() {
    let (_d, mut r) = project();
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
            raw(
                "D-AAAAA",
                "decision",
                Some("s-AAAAA"),
                &"z".repeat(30),
                &[("status", "accepted"), ("statement", "A")],
                "",
            ),
        ],
    );
    let e = r.propose(&writer(), "work", "legacy").unwrap_err();
    let i = e
        .issues
        .iter()
        .find(|i| i.code == "E_ORDER_KEY")
        .unwrap_or_else(|| panic!("{e:?}"));
    assert!(
        i.fix.contains("frozen")
            && i.fix
                .contains("move its movable siblings to another parent"),
        "{i:?}"
    );
    assert!(!i.fix.contains("rebalance"), "{i:?}");
}
