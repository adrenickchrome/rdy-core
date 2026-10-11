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
fn tdep1_an_existing_reference_plus_a_new_impact_link_to_the_same_target_raises() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    deprecate_section(&mut r, &s);
    // B already references A in its body; the write adds a link of another kind to A
    let cb = change_of(&put(
        &mut r,
        "b",
        json!([{"op": "update", "id": s.b.to_string(),
        "body": format!("B builds on {{{{{}}}}} firmly.", s.a),
        "links": {"depends_on": [{"target": s.a.to_string()}]}}]),
    ));
    let t = one_involving(&r, TensionKind::DeprecatedReference, &s.b);
    assert_eq!(change_sides(&t), vec![cb]);
    assert!(t.involved.iter().any(|i| i.block == s.a));
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

// cc3 (FW-CH2): a plain-text mention of a deprecated definition's name or alias.

/// A doc with a live section `Rules` and a section `Glossary`, which holds the term `Retry limit`
/// (alias `retries`). The glossary is deprecated when `deprecate` is set. Landed. Returns the
/// live section, for new content.
fn term_world(deprecate: bool) -> (tempfile::TempDir, rdy_engine::Rdy, rdy_model::BlockId) {
    let (d, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d",
             "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d",
             "fields": {"title": "Rules"}},
            {"op": "create", "type": "section", "id": "$g", "parent": "$d",
             "fields": {"title": "Glossary"}},
            {"op": "create", "type": "term", "id": "$t", "parent": "$g",
             "fields": {"name": "Retry limit", "aliases": ["retries"]},
             "body": "How many times a job runs again."}
        ]),
    )
    .expect("the glossary");
    land(&mut r, "work").expect("land the glossary");
    if deprecate {
        write(
            &mut r,
            "work",
            json!([{"op": "update", "id": created(&w, "$g").to_string(),
                    "fields": {"lifecycle": "deprecated"}}]),
        )
        .expect("deprecate the glossary");
        land(&mut r, "work").expect("land the deprecation");
    }
    let s = created(&w, "$s");
    (d, r, s)
}

fn deprecated_term_world() -> (tempfile::TempDir, rdy_engine::Rdy, rdy_model::BlockId) {
    term_world(true)
}

fn live_term_world() -> (tempfile::TempDir, rdy_engine::Rdy, rdy_model::BlockId) {
    term_world(false)
}

/// The id of the one term in canon.
fn deprecated_term_id(r: &rdy_engine::Rdy) -> rdy_model::BlockId {
    let m = r
        .read()
        .materialized(&rdy_engine::Scope::Canon)
        .expect("canon");
    let mut ts = m.iter().filter(|b| b.block_type == "term");
    let t = ts.next().expect("a term");
    assert!(ts.next().is_none(), "one term");
    t.id.clone()
}

#[test]
fn cc3_a_new_plain_mention_of_a_deprecated_term_is_a_tension() {
    let (_d, mut r, s) = deprecated_term_world();
    let w = put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "id": "$y", "parent": s.to_string(),
        "body": "Each job keeps its retries small."}]),
    );
    let y = created(&w, "$y");
    let t = one_involving(&r, TensionKind::DeprecatedReference, &y);
    assert!(t.summary.contains("names deprecated"), "{}", t.summary);
    assert_eq!(t.severity, TensionSeverity::Advisory);
}

#[test]
fn cc3_a_landed_mention_stays_open_in_canon() {
    let (_d, mut r, s) = deprecated_term_world();
    put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "id": "$y", "parent": s.to_string(),
        "body": "The Retry limit applies."}]),
    );
    land(&mut r, "a").unwrap();
    let ts = of_kind(&r, TensionKind::DeprecatedReference);
    assert_eq!(ts.len(), 1);
    assert!(has_canon(&ts[0]) && change_sides(&ts[0]).is_empty());
}

/// guard (pre-flight): passes before the change, because the edge form already gives one
/// tension; it pins that the mention form never adds a second one for the same pair.
#[test]
fn cc3_a_linked_mention_is_the_edge_form_only() {
    let (_d, mut r, s) = deprecated_term_world();
    let x = deprecated_term_id(&r);
    put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "id": "$y", "parent": s.to_string(),
        "body": format!("The Retry limit {{{{{x}}}}} applies.")}]),
    );
    assert_eq!(
        of_kind(&r, TensionKind::DeprecatedReference).len(),
        1,
        "one tension, not two"
    );
}

/// guard: a mention of a live term raises nothing; a key match is not a mention.
#[test]
fn cc3_live_terms_and_keys_raise_nothing() {
    let (_d, mut r, s) = live_term_world();
    put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "parent": s.to_string(),
        "body": "The Retry limit applies."}]),
    );
    assert!(of_kind(&r, TensionKind::DeprecatedReference).is_empty());
}
