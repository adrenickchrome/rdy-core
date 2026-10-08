//! tunj1: unjustified change: a change to stable normative content has no justification.
use rdy_engine::tensions::*;
use rdy_engine::{OpInput, Scope, WriteRequest};
use rdy_model::{EditorialClass, Justification};
use rdy_qa::edit_loop::{brief_of, spec_doc, stable_spec, write_cited_as};
use rdy_qa::fixtures::*;
use rdy_qa::packs::qa_project;
use rdy_qa::tensions::*;
use serde_json::json;

#[test]
fn tunj1_an_unjustified_change_to_stable_normative_content_is_advisory_by_default() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let tok = brief_of(&r, &Scope::Canon, &[&s.c]).base.token;
    let w = write_cited_as(&mut r, &ag("a"), "a", upd(&s.c, "C changed."), Some(&tok)).unwrap();
    let t = one(&r, TensionKind::UnjustifiedChange);
    assert_eq!(change_sides(&t), vec![change_of(&w)]);
    assert_eq!(
        (t.severity, t.overridden),
        (TensionSeverity::Advisory, false)
    );
    assert_eq!(t.blocks(), vec![s.c.clone()]);
}

#[test]
fn tunj1_a_justified_change_draft_content_or_prose_is_not_unjustified() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let tok = brief_of(&r, &Scope::Canon, &[&s.c, &s.section]).base.token;
    let ops: Vec<OpInput> = serde_json::from_value(upd(&s.c, "C, typo fixed.")).unwrap();
    r.write_briefed(
        &ag("a"),
        WriteRequest {
            view: "a".into(),
            ops,
            justification: Some(Justification::Editorial {
                class: EditorialClass::Typo,
                reason: "typo".into(),
            }),
            message: String::new(),
        },
        Some(tok.clone()),
    )
    .unwrap();
    write_cited_as(
        &mut r,
        &ag("b"),
        "b",
        json!([{"op": "create", "type": "prose",
        "parent": s.section.to_string(), "body": "Context."}]),
        Some(&tok),
    )
    .unwrap();
    assert!(of_kind(&r, TensionKind::UnjustifiedChange).is_empty());
    let (_d2, mut draft) = project();
    let d = spec_doc(&mut draft);
    put(&mut draft, "a", upd(&d.c, "C changed."));
    assert!(of_kind(&draft, TensionKind::UnjustifiedChange).is_empty());
}

#[test]
fn tunj1_the_qa_pack_makes_it_blocking() {
    let (_d, mut r) = qa_project();
    let s = stable_spec(&mut r);
    let tok = brief_of(&r, &Scope::Canon, &[&s.c]).base.token;
    write_cited_as(&mut r, &ag("a"), "a", upd(&s.c, "C changed."), Some(&tok)).unwrap();
    let t = one(&r, TensionKind::UnjustifiedChange);
    assert_eq!(
        (t.severity, t.overridden),
        (TensionSeverity::Blocking, true)
    );
}

#[test]
fn tunj1_landing_an_unjustified_change_resolves_it() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let tok = brief_of(&r, &Scope::Canon, &[&s.c]).base.token;
    write_cited_as(&mut r, &ag("a"), "a", upd(&s.c, "C changed."), Some(&tok)).unwrap();
    one(&r, TensionKind::UnjustifiedChange);
    land(&mut r, "a").unwrap();
    assert!(of_kind(&r, TensionKind::UnjustifiedChange).is_empty());
}
