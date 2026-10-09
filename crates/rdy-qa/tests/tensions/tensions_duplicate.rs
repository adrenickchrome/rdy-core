//! tdup1: duplicate definition: two blocks define one name, or two normative blocks share text.
use rdy_engine::Scope;
use rdy_engine::tensions::*;
use rdy_qa::edit_loop::{brief_of, spec_doc, stable_spec, write_cited_as};
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use serde_json::json;

#[test]
fn tdup1_two_terms_defining_one_name_in_draft_are_advisory_until_one_is_renamed() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", term(&s.section, "Fee"));
    let wb = put(&mut r, "b", term(&s.section, "  fee "));
    let t = one(&r, TensionKind::DuplicateDefinition);
    assert_eq!(change_sides(&t).len(), 2);
    assert_eq!(t.severity, TensionSeverity::Advisory);
    assert!(t.summary.contains("\"fee\""), "{}", t.summary);
    put(
        &mut r,
        "b",
        json!([{"op": "update", "id": created(&wb, "$t").to_string(),
        "fields": {"name": "Charge"}}]),
    );
    assert!(of_kind(&r, TensionKind::DuplicateDefinition).is_empty());
}

#[test]
fn tdup1_in_stable_content_a_duplicate_blocks() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let tok = brief_of(&r, &Scope::Canon, &[&s.section]).base.token;
    write_cited_as(&mut r, &ag("a"), "a", term(&s.section, "Fee"), Some(&tok)).unwrap();
    write_cited_as(&mut r, &ag("b"), "b", term(&s.section, "FEE"), Some(&tok)).unwrap();
    assert_eq!(
        one(&r, TensionKind::DuplicateDefinition).severity,
        TensionSeverity::Blocking
    );
}

#[test]
fn tdup1_a_term_matching_a_canon_term_is_a_duplicate_with_canon() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    write(&mut r, "work", term(&s.section, "Fee")).unwrap();
    land(&mut r, "work").unwrap();
    let cb = change_of(&put(&mut r, "b", term(&s.section, "FEE")));
    let t = one(&r, TensionKind::DuplicateDefinition);
    assert_eq!(change_sides(&t), vec![cb]);
    assert!(has_canon(&t));
}

#[test]
fn tdup1_two_normative_blocks_with_the_same_text_are_duplicates_and_prose_is_not() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let claim = |body: &str| {
        json!([{"op": "create", "type": "claim",
        "parent": s.section.to_string(), "body": body}])
    };
    put(&mut r, "a", claim("Fees are charged monthly."));
    put(&mut r, "b", claim("fees are   charged MONTHLY."));
    put(
        &mut r,
        "c",
        json!([{"op": "create", "type": "prose",
        "parent": s.section.to_string(), "body": "Fees are charged monthly."}]),
    );
    let t = one(&r, TensionKind::DuplicateDefinition);
    assert!(t.summary.contains("same text"), "{}", t.summary);
}

#[test]
fn tdup1_texts_under_three_words_are_not_duplicates() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let claim = |body: &str| {
        json!([{"op": "create", "type": "claim",
        "parent": s.section.to_string(), "body": body}])
    };
    put(&mut r, "a", claim("Fees apply."));
    put(&mut r, "b", claim("fees  APPLY."));
    assert!(
        of_kind(&r, TensionKind::DuplicateDefinition).is_empty(),
        "two words"
    );
    put(&mut r, "a", claim("Fees apply monthly."));
    put(&mut r, "b", claim("fees apply MONTHLY."));
    let t = one(&r, TensionKind::DuplicateDefinition);
    assert!(t.summary.contains("same text"), "{}", t.summary);
}

#[test]
fn tdup1_two_landed_duplicate_terms_stay_a_duplicate_in_canon() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", term(&s.section, "Fee"));
    put(&mut r, "b", term(&s.section, "fee"));
    let before = one(&r, TensionKind::DuplicateDefinition);
    land(&mut r, "a").unwrap();
    land(&mut r, "b").unwrap();
    let t = one(&r, TensionKind::DuplicateDefinition);
    assert!(
        change_sides(&t).is_empty() && has_canon(&t),
        "every side is canon: {t:#?}"
    );
    assert_eq!(t.vkey, before.vkey, "re-keyed, not resolved");
    assert_eq!(t.severity, TensionSeverity::Advisory, "draft content");
}

/// Checklist line 31: two blocks that share both a name and a normative text are one tension,
/// reported as the name, carried or rebuilt.
#[test]
fn tdup1_a_pair_sharing_a_name_and_a_text_is_one_tension_carried_or_rebuilt() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let iface = |name: &str| {
        json!([{"op": "create", "type": "interface", "id": "$i", "parent": s.section.to_string(),
                "fields": {"name": name}, "body": "Charges a card once per order."}])
    };
    put(&mut r, "a", iface("Pay API"));
    r.read().tensions().unwrap();
    put(&mut r, "b", iface("pay  api"));
    let st = r.read();
    let pick = |set: &TensionSet| -> Vec<(rdy_model::Hash, String)> {
        set.iter()
            .filter(|t| t.kind == TensionKind::DuplicateDefinition)
            .map(|t| (t.id, t.summary.clone()))
            .collect()
    };
    let carried = pick(&st.tensions().unwrap());
    assert_eq!(st.tensions_carried(), Some(true));
    assert_eq!(carried.len(), 1, "{carried:#?}");
    assert!(
        carried[0].1.contains("both define \"pay api\""),
        "{carried:?}"
    );
    assert_eq!(carried, pick(&st.tensions_rebuilt().unwrap()));
}

/// Records a proposed decision in view `who` under `parent`.
fn decide(
    r: &mut rdy_engine::Rdy,
    who: &str,
    parent: &rdy_model::BlockId,
    statement: &str,
    body: &str,
) {
    r.decide(
        &ag(who),
        who,
        rdy_engine::DecisionInput {
            parent: parent.to_string(),
            statement: statement.into(),
            body: body.into(),
            ..Default::default()
        },
    )
    .unwrap();
}

/// Founder ruling 2026-10-08: a decision's text is its statement, never its body. Register
/// boilerplate in two bodies does not make two decisions duplicates.
#[test]
fn tdup1_decisions_with_the_same_body_and_different_statements_are_not_duplicates() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let body = "Disposition: rdyhub / Trigger: rdyhub spec";
    decide(&mut r, "a", &s.section, "Sync runs on the hub only", body);
    decide(&mut r, "b", &s.section, "Teams are a hub feature", body);
    assert!(
        of_kind(&r, TensionKind::DuplicateDefinition).is_empty(),
        "{:#?}",
        of_kind(&r, TensionKind::DuplicateDefinition)
    );
}

#[test]
fn tdup1_decisions_with_the_same_statement_are_duplicates() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    decide(
        &mut r,
        "a",
        &s.section,
        "Sync runs on the hub only",
        "Disposition: M2b",
    );
    decide(
        &mut r,
        "b",
        &s.section,
        "sync runs on  the HUB only",
        "Notes: a different body here",
    );
    let t = one(&r, TensionKind::DuplicateDefinition);
    assert_eq!(change_sides(&t).len(), 2);
    assert!(t.summary.contains("same text"), "{}", t.summary);
}

/// FW-TN2: one claim with `body` under `parent`.
fn claim_op(parent: &rdy_model::BlockId, body: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "claim", "id": "$c", "parent": parent.to_string(), "body": body}])
}

/// The carried set's duplicate tensions equal a rebuild's (ids).
fn assert_carry_matches(r: &rdy_engine::Rdy) {
    let st = r.read();
    let ids = |set: &TensionSet| -> Vec<rdy_model::Hash> {
        set.iter()
            .filter(|t| t.kind == TensionKind::DuplicateDefinition)
            .map(|t| t.id)
            .collect()
    };
    assert_eq!(
        ids(&st.tensions().unwrap()),
        ids(&st.tensions_rebuilt().unwrap())
    );
}

/// FW-TN2: k blocks with one normalized text are one tension whose members are every block.
#[test]
fn tdup1_k_identical_texts_are_one_tension() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let mut blocks = vec![];
    for (i, v) in ["a", "b", "c", "d", "e"].into_iter().enumerate() {
        let body = if i % 2 == 0 {
            "Fees are charged monthly."
        } else {
            "fees ARE charged  monthly."
        };
        blocks.push(created(&put(&mut r, v, claim_op(&s.section, body)), "$c"));
        r.read().tensions().unwrap();
    }
    assert_carry_matches(&r);
    let t = one(&r, TensionKind::DuplicateDefinition);
    blocks.sort();
    assert_eq!(t.blocks(), blocks);
    assert_eq!(t.involved.len(), 5);
    assert_eq!(change_sides(&t).len(), 5);
    assert!(t.summary.contains("same text"), "{}", t.summary);
}

/// FW-TN2: a new member resolves the group's tension and opens one with a new id.
#[test]
fn tdup1_adding_a_member_resolves_and_opens_a_new_group_id() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(
        &mut r,
        "a",
        claim_op(&s.section, "Fees are charged monthly."),
    );
    put(
        &mut r,
        "b",
        claim_op(&s.section, "Fees are charged monthly."),
    );
    let two = one(&r, TensionKind::DuplicateDefinition);
    put(
        &mut r,
        "c",
        claim_op(&s.section, "Fees are charged monthly."),
    );
    assert_carry_matches(&r);
    let three = one(&r, TensionKind::DuplicateDefinition);
    assert_eq!(three.blocks().len(), 3);
    assert_ne!(three.id, two.id);
    assert_ne!(three.vkey, two.vkey);
}

/// FW-TN2: a member edited away changes the group; down to one member, no tension is left.
#[test]
fn tdup1_removing_a_member_down_to_one_resolves_it() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let mut ids = vec![];
    for v in ["a", "b", "c"] {
        let w = put(&mut r, v, claim_op(&s.section, "Fees are charged monthly."));
        ids.push(created(&w, "$c"));
    }
    let three = one(&r, TensionKind::DuplicateDefinition);
    r.read().tensions().unwrap();
    put(&mut r, "c", upd(&ids[2], "Fees are charged yearly."));
    assert_carry_matches(&r);
    let two = one(&r, TensionKind::DuplicateDefinition);
    assert_ne!(two.id, three.id);
    assert_eq!(two.blocks(), vec![ids[0].clone(), ids[1].clone()]);
    put(&mut r, "b", upd(&ids[1], "Fees are charged weekly."));
    assert_carry_matches(&r);
    assert!(of_kind(&r, TensionKind::DuplicateDefinition).is_empty());
}

/// FW-TN2: a dismissal binds the group's members; any membership change voids it.
#[test]
fn tdup1_a_dismissed_group_is_void_after_a_membership_change() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(
        &mut r,
        "a",
        claim_op(&s.section, "Fees are charged monthly."),
    );
    put(
        &mut r,
        "b",
        claim_op(&s.section, "Fees are charged monthly."),
    );
    let t = one(&r, TensionKind::DuplicateDefinition);
    r.dismiss_tension(&ag("a"), &tension_label(&t.id), "fine")
        .unwrap();
    assert!(matches!(
        r.read().tension_status(&t),
        TensionStatus::Dismissed(_)
    ));
    put(
        &mut r,
        "c",
        claim_op(&s.section, "Fees are charged monthly."),
    );
    let t2 = one(&r, TensionKind::DuplicateDefinition);
    assert_eq!(t2.blocks().len(), 3);
    assert_eq!(r.read().tension_status(&t2), TensionStatus::Open);
}

/// FW-TN2: k `defines` blocks with one normalized name are one tension; the summary names the
/// members and the name.
#[test]
fn tdup1_a_name_group() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let mut ids = vec![];
    for (v, n) in [("a", "Fee"), ("b", "fee"), ("c", " FEE ")] {
        ids.push(created(&put(&mut r, v, term(&s.section, n)), "$t"));
    }
    assert_carry_matches(&r);
    let t = one(&r, TensionKind::DuplicateDefinition);
    assert_eq!(t.blocks().len(), 3);
    assert_eq!(change_sides(&t).len(), 3);
    assert!(t.summary.contains("define \"fee\""), "{}", t.summary);
    for id in &ids {
        assert!(t.summary.contains(&id.to_string()), "{}", t.summary);
    }
}
