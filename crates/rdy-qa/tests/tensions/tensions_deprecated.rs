//! tdep1: deprecated reference: new content references deprecated content.
use rdy_engine::DecisionInput;
use rdy_engine::tensions::*;
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use serde_json::json;

fn deprecate_section(r: &mut rdy_engine::Rdy, s: &Spec) {
    write(
        r,
        "work",
        json!([{"op": "update", "id": s.section.to_string(),
        "fields": {"lifecycle": "deprecated"}}]),
    )
    .unwrap();
    land(r, "work").unwrap();
}

#[test]
fn tdep1_new_content_referencing_deprecated_content_is_advisory() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    deprecate_section(&mut r, &s);
    let cb = change_of(&put(&mut r, "b", cite(&s.section, &s.c)));
    let t = one(&r, TensionKind::DeprecatedReference);
    assert_eq!(change_sides(&t), vec![cb]);
    assert!(has_canon(&t));
    assert_eq!(t.severity, TensionSeverity::Advisory);
    assert!(t.involved.iter().any(|i| i.block == s.c));
}

#[test]
fn tdep1_a_reference_that_was_already_there_is_not_new() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    deprecate_section(&mut r, &s);
    put(
        &mut r,
        "b",
        upd(&s.b, &format!("B builds on {{{{{}}}}} firmly.", s.a)),
    );
    assert!(of_kind(&r, TensionKind::DeprecatedReference).is_empty());
}

#[test]
fn tdep1_a_superseded_decision_is_deprecated() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    r.supersede(
        &ag("a"),
        "a",
        &s.decision.to_string(),
        None,
        DecisionInput {
            parent: s.section.to_string(),
            statement: "Keep A, differently".into(),
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "a").unwrap();
    let y = created(
        &put(
            &mut r,
            "b",
            json!([{"op": "create", "type": "claim", "id": "$y",
        "parent": s.section.to_string(), "body": "Y.",
        "links": {"decided_by": [{"target": s.decision.to_string()}]}}]),
        ),
        "$y",
    );
    let t = one_involving(&r, TensionKind::DeprecatedReference, &y);
    assert!(t.involved.iter().any(|i| i.block == s.decision));
}

#[test]
fn tdep1_landing_the_reference_rekeys_it_to_canon() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    deprecate_section(&mut r, &s);
    // Y directly under the doc: outside the deprecated section, so live in canon after landing
    put(&mut r, "b", cite(&s.doc, &s.c));
    let before = one(&r, TensionKind::DeprecatedReference);
    land(&mut r, "b").unwrap();
    let t = one(&r, TensionKind::DeprecatedReference);
    assert!(
        change_sides(&t).is_empty() && has_canon(&t),
        "every side is canon: {t:#?}"
    );
    assert_eq!(t.vkey, before.vkey, "re-keyed, not resolved");
    assert_ne!(t.id, before.id);
    assert_eq!(t.severity, TensionSeverity::Advisory);
}

#[test]
fn tdep1_deprecated_content_referencing_deprecated_content_is_not_a_canon_tension() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    deprecate_section(&mut r, &s);
    // canon's B references A, both deprecated: nothing in the canon scope
    let canon = r.read().canon_tensions_rebuilt().unwrap();
    assert!(
        canon
            .iter()
            .all(|t| t.kind != TensionKind::DeprecatedReference),
        "{canon:#?}"
    );
    // new content inside the deprecated section: an in-flight reference, which resolves when
    // it lands (Y is deprecated in canon, so the canon form skips it)
    put(&mut r, "b", cite(&s.section, &s.c));
    one(&r, TensionKind::DeprecatedReference);
    land(&mut r, "b").unwrap();
    assert!(of_kind(&r, TensionKind::DeprecatedReference).is_empty());
}

#[test]
fn tdep1_canon_content_referencing_deprecated_canon_content_is_an_all_canon_tension() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let w = r
        .supersede(
            &ag("a"),
            "a",
            &s.decision.to_string(),
            None,
            DecisionInput {
                parent: s.section.to_string(),
                statement: "Keep A, differently".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let d2 = created(&w, "$decision");
    land(&mut r, "a").unwrap();
    // canon's A (live: the section is not deprecated) is decided_by the superseded decision
    let t = one_involving(&r, TensionKind::DeprecatedReference, &s.a);
    assert!(change_sides(&t).is_empty() && has_canon(&t), "{t:#?}");
    assert!(t.involved.iter().any(|i| i.block == s.decision));
    assert_eq!(t.severity, TensionSeverity::Advisory);
    assert!(
        r.read()
            .canon_tensions_rebuilt()
            .unwrap()
            .iter()
            .any(|x| x.id == t.id)
    );
    // an unrelated write leaves it; a fix in flight does not end it until it lands
    put(&mut r, "c", upd(&s.c, "C changed."));
    put(
        &mut r,
        "b",
        json!([{"op": "update", "id": s.a.to_string(),
        "links": {"decided_by": [{"target": d2.to_string()}]}}]),
    );
    assert!(r.read().tensions().unwrap().get(&t.id).is_some());
    land(&mut r, "b").unwrap();
    assert!(
        of_kind(&r, TensionKind::DeprecatedReference)
            .iter()
            .all(|x| !x.blocks().contains(&s.decision))
    );
}
