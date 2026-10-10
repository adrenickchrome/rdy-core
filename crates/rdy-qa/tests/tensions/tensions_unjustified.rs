//! tunj1: unjustified change: a change to stable normative content has no justification.
use rdy_engine::tensions::*;
use rdy_engine::{DecisionInput, OpInput, ProposalInfo, Scope, WriteRequest};
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

/// Checklist line 30: a delete of stable normative content without a justification is
/// unjustified (the type comes from the deleted block).
#[test]
fn tunj1_an_unjustified_delete_of_stable_normative_content_counts() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let tok = brief_of(&r, &Scope::Canon, &[&s.c]).base.token;
    let w = write_cited_as(
        &mut r,
        &ag("a"),
        "a",
        json!([{"op": "delete", "id": s.c.to_string()}]),
        Some(&tok),
    )
    .unwrap();
    let t = one(&r, TensionKind::UnjustifiedChange);
    assert_eq!(change_sides(&t), vec![change_of(&w)]);
    assert_eq!(t.blocks(), vec![s.c.clone()]);
}

/// FW-JU1: a decision's creation (decide, supersede) and an edit of it while proposed never
/// count. A new claim under the stable section still does.
#[test]
fn msw21_creating_a_decision_and_editing_it_while_proposed_are_not_unjustified() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let d = try_decide_in(&mut r, "a", &s.section, "Keep C short").unwrap();
    put_j(&mut r, "a", upd(&d, "Short claims read better."), None);
    try_briefed(&mut r, "b", |r, t| {
        r.supersede(
            &ag("b"),
            "b",
            &s.decision.to_string(),
            None,
            DecisionInput {
                parent: s.section.to_string(),
                statement: "Keep A and B".into(),
                brief: t,
                ..Default::default()
            },
        )
    })
    .unwrap();
    let found = of_kind(&r, TensionKind::UnjustifiedChange);
    assert!(found.is_empty(), "{found:#?}");
    let w = put_j(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "parent": s.section.to_string(),
                "body": "New rule."}]),
        None,
    );
    let t = one(&r, TensionKind::UnjustifiedChange);
    assert_eq!(change_sides(&t), vec![change_of(&w)]);
}

/// FW-JU1: a later justified rewrite of the block in the same view clears the tension on the
/// earlier unjustified change, and the carried set equals a rebuild.
#[test]
fn msw21_a_later_justified_rewrite_in_the_view_clears_the_tension() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let c1 = change_of(&put_j(&mut r, "a", upd(&s.c, "C changed."), None));
    assert_eq!(
        change_sides(&one(&r, TensionKind::UnjustifiedChange)),
        vec![c1]
    );
    put_j(
        &mut r,
        "a",
        upd(&s.c, "C, as D says."),
        j_implements(&s.decision),
    );
    let found = of_kind(&r, TensionKind::UnjustifiedChange);
    assert!(found.is_empty(), "{found:#?}");
    let st = r.read();
    assert_eq!(st.tensions_carried(), Some(true));
    let ids = |set: &TensionSet| set.iter().map(|t| t.id).collect::<Vec<_>>();
    assert_eq!(
        ids(&st.tensions().unwrap()),
        ids(&st.tensions_rebuilt().unwrap())
    );
}

/// FW-JU1: a justification on another change of the view that writes the block covers the
/// view's later unjustified rewrite. It covers only that block: a change on two blocks is one
/// tension per block.
#[test]
fn msw21_a_justification_on_another_change_of_the_view_covers_the_block() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    put_j(&mut r, "a", upd(&s.c, "C, typo fixed."), j_typo());
    put_j(&mut r, "a", upd(&s.c, "C, reworded."), None);
    let found = of_kind(&r, TensionKind::UnjustifiedChange);
    assert!(found.is_empty(), "{found:#?}");
    let w = put_j(
        &mut r,
        "a",
        json!([{"op": "update", "id": s.a.to_string(), "body": "A changed."},
               {"op": "update", "id": s.c.to_string(), "body": "C, once more."}]),
        None,
    );
    let t = one(&r, TensionKind::UnjustifiedChange);
    assert_eq!(t.blocks(), vec![s.a.clone()]);
    assert_eq!(change_sides(&t), vec![change_of(&w)]);
}

/// The tension's id follows the view's net version of the block: an unrelated write keeps it
/// and its dismissal; an unjustified rewrite of the block replaces it with an open one.
#[test]
fn msw21_a_dismissal_holds_until_the_view_rewrites_the_block() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    put_j(&mut r, "a", upd(&s.c, "C changed."), None);
    let t = one(&r, TensionKind::UnjustifiedChange);
    r.dismiss_tension(&ag("a"), &tension_label(&t.id), "a small fix")
        .unwrap();
    put_j(
        &mut r,
        "a",
        upd(&s.a, "A, as D says."),
        j_implements(&s.decision),
    );
    let same = one_involving(&r, TensionKind::UnjustifiedChange, &s.c);
    assert_eq!(same.id, t.id);
    assert!(matches!(
        r.read().tension_status(&same),
        TensionStatus::Dismissed(_)
    ));
    let w = put_j(&mut r, "a", upd(&s.c, "C changed again."), None);
    let next = one_involving(&r, TensionKind::UnjustifiedChange, &s.c);
    assert_eq!(change_sides(&next), vec![change_of(&w)]);
    assert_eq!(r.read().tension_status(&next), TensionStatus::Open);
}

/// FW-JU1, per view: view b includes a's unjustified change and justifies its own rewrite. a's
/// tension stays; b's proposal does not list it; a's proposal does.
#[test]
fn msw21_another_views_unjustified_change_is_not_cleared_by_this_views_justification() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let c1 = change_of(&put_j(&mut r, "a", upd(&s.c, "C changed."), None));
    r.view_include(&ag("b"), "b", &[c1.to_string()]).unwrap();
    put_j(
        &mut r,
        "b",
        upd(&s.c, "C, as D says."),
        j_implements(&s.decision),
    );
    let t = one(&r, TensionKind::UnjustifiedChange);
    assert_eq!(change_sides(&t), vec![c1]);
    let unjustified = |p: &ProposalInfo| {
        p.tensions
            .iter()
            .filter(|t| t.kind == TensionKind::UnjustifiedChange)
            .count()
    };
    let pb = r.propose(&writer(), "b", "b").unwrap();
    assert_eq!(unjustified(&pb), 0, "{:?}", pb.tensions);
    let pa = r.propose(&writer(), "a", "a").unwrap();
    assert_eq!(unjustified(&pa), 1, "{:?}", pa.tensions);
}
