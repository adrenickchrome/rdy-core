//! bwr1: a write to stable content cites a current briefing; drafts are encouraged to.
use rdy_engine::brief::*;
use rdy_engine::*;
use rdy_model::{BlockId, Hash};
use rdy_qa::edit_loop::*;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

fn upd(id: &BlockId, body: &str) -> Value {
    json!([{"op": "update", "id": id.to_string(), "body": body}])
}

#[test]
fn bwr1_a_stable_write_without_a_briefing_is_refused_and_the_errors_token_lets_the_retry_land() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let e = write_cited(&mut r, "w", upd(&s.a, "A, revised."), None).unwrap_err();
    assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
    assert!(e.message.contains(&s.a.to_string()), "{e}");
    assert!(
        r.read().included("w").is_empty(),
        "a failed write changes nothing"
    );
    assert_eq!(
        e.briefing.as_ref().unwrap()["ids"],
        json!([s.a.to_string()])
    );
    let (e, t) = token_in(&r, e, "w");
    assert!(e.fix.contains(&t), "{}", e.fix);
    assert_eq!(
        e.briefing.as_ref().unwrap()["covers"],
        1,
        "exactly the blocks the write touches"
    );
    write_cited(&mut r, "w", upd(&s.a, "A, revised."), Some(&t)).expect("the retry lands");
}

#[test]
fn bwr1_a_draft_write_lands_without_a_briefing_and_a_stale_cited_one_only_warns() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let t = brief_of(&r, &Scope::Canon, &[&s.b]).base.token;
    let ok = write_cited(&mut r, "w", upd(&s.c, "C, no briefing."), None).unwrap();
    assert!(ok.warnings.iter().all(|w| w.code != W_BRIEF_STALE));
    write_cited(&mut r, "w", upd(&s.a, "A moves."), None).unwrap();
    let ok = write_cited(&mut r, "w", upd(&s.b, "B, after A moved."), Some(&t)).unwrap();
    let warn = ok
        .warnings
        .iter()
        .find(|w| w.code == W_BRIEF_STALE)
        .expect("W_BRIEF_STALE");
    assert!(warn.message.contains(&s.a.to_string()), "{}", warn.message);
    let ok = write_cited(&mut r, "w", upd(&s.c, "C again."), Some(&"0".repeat(64))).unwrap();
    assert!(
        ok.warnings
            .iter()
            .any(|w| w.code == W_BRIEF_STALE && w.message.contains("unknown"))
    );
}

#[test]
fn bwr1_a_write_after_a_dependency_moved_is_brief_stale_naming_it() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let tb = brief_of(&r, &Scope::Canon, &[&s.b]).base.token;
    let ta = brief_of(&r, &Scope::Canon, &[&s.a]).base.token;
    write_cited(&mut r, "w", upd(&s.a, "A moves first."), Some(&ta)).unwrap();
    let e = write_cited(&mut r, "w", upd(&s.b, "B, too late."), Some(&tb)).unwrap_err();
    assert_eq!(e.code, E_BRIEF_STALE, "{e}");
    assert!(e.message.contains(&s.a.to_string()), "{e}");
    assert!(
        e.issues.iter().any(|i| i.block.as_ref() == Some(&s.a)),
        "{:?}",
        e.issues
    );
    let (_, t2) = token_in(&r, e, "w");
    write_cited(&mut r, "w", upd(&s.b, "B, too late."), Some(&t2)).expect("the fresh token lands");
}

#[test]
fn bwr1_two_agents_brief_the_same_block_one_writes_and_the_others_token_is_stale() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let (x, y) = (agent("agent:x", None), agent("agent:y", None));
    let tx = brief_of(&r, &Scope::Canon, &[&s.a]).base.token;
    let ty = brief_of(&r, &Scope::Canon, &[&s.a]).base.token;
    assert_eq!(
        tx, ty,
        "the same blocks at the same versions brief to the same token"
    );
    write_cited_as(
        &mut r,
        &x,
        &x.default_view(),
        upd(&s.a, "X's A."),
        Some(&tx),
    )
    .unwrap();
    land(&mut r, &x.default_view()).unwrap();
    let e = write_cited_as(
        &mut r,
        &y,
        &y.default_view(),
        upd(&s.a, "Y's A."),
        Some(&ty),
    )
    .unwrap_err();
    assert_eq!(e.code, E_BRIEF_STALE, "{e}");
    assert!(e.message.contains(&s.a.to_string()), "{e}");
}

#[test]
fn bwr1_the_token_survives_an_unrelated_write() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let ta = brief_of(&r, &Scope::Canon, &[&s.a]).base.token;
    let tc = brief_of(&r, &Scope::Canon, &[&s.c]).base.token;
    write_cited(&mut r, "w", upd(&s.c, "C moves."), Some(&tc)).unwrap();
    write_cited(&mut r, "w", upd(&s.a, "A moves."), Some(&ta)).expect("C is not A's dependency");
}

#[test]
fn bwr1_a_token_that_does_not_cover_the_block_is_brief_required() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let tc = brief_of(&r, &Scope::Canon, &[&s.c]).base.token;
    let e = write_cited(&mut r, "w", upd(&s.a, "A."), Some(&tc)).unwrap_err();
    assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
    assert!(e.message.contains("does not cover"), "{e}");
    assert_eq!(
        e.issues
            .iter()
            .filter_map(|i| i.block.clone())
            .collect::<Vec<_>>(),
        vec![s.a.clone()]
    );
}

#[test]
fn bwr1_an_unknown_token_is_stale_and_a_restart_forgets_every_token() {
    let (d, mut r) = project();
    let s = stable_spec(&mut r);
    let e = write_cited(
        &mut r,
        "w",
        upd(&s.a, "A."),
        Some(&Hash::of(b"never").to_string()),
    )
    .unwrap_err();
    assert_eq!(e.code, E_BRIEF_STALE, "{e}");
    assert!(e.message.contains("unknown"), "{e}");
    let t = brief_of(&r, &Scope::Canon, &[&s.a]).base.token;
    r.close().unwrap();
    let mut r = Rdy::open(d.path()).unwrap();
    let e = write_cited(&mut r, "w", upd(&s.a, "A."), Some(&t)).unwrap_err();
    assert_eq!(e.code, E_BRIEF_STALE, "the registry is not stored: {e}");
}

#[test]
fn bwr1_creating_under_a_stable_parent_needs_the_parent_briefed() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let create =
        json!([{"op": "create", "type": "claim", "parent": s.section.to_string(), "body": "New."}]);
    let e = write_cited(&mut r, "w", create.clone(), None).unwrap_err();
    assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
    assert_eq!(
        e.briefing.as_ref().unwrap()["ids"],
        json!([s.section.to_string()])
    );
    let t = brief_of(&r, &Scope::Canon, &[&s.section]).base.token;
    write_cited(&mut r, "w", create, Some(&t))
        .expect("briefing the parent covers its new children");
}

#[test]
fn bwr1_one_doc_briefing_covers_a_doc_wide_rewrite() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let doc = r
        .read()
        .brief(
            &Scope::Canon,
            &Coverage {
                ids: vec![],
                path: Some("spec".into()),
            },
            None,
        )
        .unwrap();
    doc.register(&r.briefs());
    let ops = json!([
        {"op": "update", "id": s.a.to_string(), "body": "A, rewritten."},
        {"op": "update", "id": s.b.to_string(), "body": "B, rewritten."},
        {"op": "delete", "id": s.c.to_string()},
        {"op": "create", "type": "section", "id": "$sub", "parent": s.section.to_string(), "fields": {"title": "More"}},
        {"op": "create", "type": "claim", "parent": "$sub", "body": "Under the new subsection."}
    ]);
    write_cited(&mut r, "w", ops, Some(&doc.base.token)).expect("one briefing, one write");
}

#[test]
fn bwr1_decide_supersede_revoke_and_edit_block_cite_a_briefing() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let sec = s.section.to_string();
    let input = || DecisionInput {
        parent: sec.clone(),
        statement: "Prefer C".into(),
        ..Default::default()
    };
    assert_eq!(
        r.decide(&writer(), "w", input()).unwrap_err().code,
        E_BRIEF_REQUIRED
    );
    let t = brief_of(&r, &Scope::Canon, &[&s.section]).base.token;
    r.decide(
        &writer(),
        "w",
        DecisionInput {
            brief: Some(t.clone()),
            ..input()
        },
    )
    .unwrap();
    let old = s.decision.to_string();
    assert_eq!(
        r.supersede(&writer(), "w", &old, None, input())
            .unwrap_err()
            .code,
        E_BRIEF_REQUIRED
    );
    r.supersede(
        &writer(),
        "w",
        &old,
        None,
        DecisionInput {
            brief: Some(t.clone()),
            ..input()
        },
    )
    .unwrap();
    assert_eq!(
        r.revoke(&writer(), "w2", &old, "no longer true", &sec, None)
            .unwrap_err()
            .code,
        E_BRIEF_REQUIRED
    );
    r.revoke(&writer(), "w2", &old, "no longer true", &sec, Some(t))
        .unwrap();
    let mut edited = r.read().block(&Scope::Canon, &s.a).unwrap();
    edited.body = "A, edited.".into();
    assert_eq!(
        r.edit_block(&writer(), "w3", edited.clone(), None)
            .unwrap_err()
            .code,
        E_BRIEF_REQUIRED
    );
    let ta = brief_of(&r, &Scope::Canon, &[&s.a]).base.token;
    r.edit_block(&writer(), "w3", edited, Some(ta)).unwrap();
}

#[test]
fn bwr1_clearing_stable_or_moving_into_it_touches_stable_content() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let sec = s.section.to_string();
    let e = write_cited(
        &mut r,
        "w",
        json!([{"op": "update", "id": sec, "unset": ["lifecycle"]}]),
        None,
    )
    .unwrap_err();
    assert_eq!(e.code, E_BRIEF_REQUIRED, "stable before the op: {e}");
    let n = write_cited(&mut r, "w", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
        {"op": "create", "type": "claim", "id": "$n", "parent": "$d", "body": "N."}
    ]), None).expect("a top-level create never touches stable content");
    let nid = BlockId::parse(&n.created["$n"]).unwrap();
    let mv = json!([{"op": "move", "id": nid.to_string(), "parent": sec}]);
    assert_eq!(
        write_cited(&mut r, "w", mv.clone(), None).unwrap_err().code,
        E_BRIEF_REQUIRED,
        "stable after the op"
    );
    let t = brief_of(&r, &Scope::View("w".into()), &[&nid]).base.token;
    write_cited(&mut r, "w", mv, Some(&t)).unwrap();
}

#[test]
fn bwr1_presentational_blocks_are_exempt() {
    let (_d, mut r, ids) = rdy_qa::golden::presentational_site();
    let t = brief_of(&r, &Scope::Canon, &[&ids.doc]).base.token;
    write_cited(
        &mut r,
        "w",
        json!([{"op": "update", "id": ids.doc.to_string(), "fields": {"lifecycle": "stable"}}]),
        Some(&t),
    )
    .unwrap();
    write_cited(&mut r, "w", upd(&ids.aside, "Still decorative."), None)
        .expect("Q6d: presentational is exempt");
    let e = write_cited(
        &mut r,
        "w",
        upd(&ids.claim, "Depends on the aside, still."),
        None,
    )
    .unwrap_err();
    assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
}

#[test]
fn bwr1_a_mixed_batch_without_a_briefing_lands_nothing() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let ops = json!([
        {"op": "create", "type": "doc", "fields": {"name": "draft", "title": "Draft", "doc_type": "overview"}},
        {"op": "update", "id": s.a.to_string(), "body": "A."}
    ]);
    assert_eq!(
        write_cited(&mut r, "w", ops, None).unwrap_err().code,
        E_BRIEF_REQUIRED
    );
    assert!(r.read().included("w").is_empty(), "not even the draft op");
}

#[test]
fn bwr1_currency_is_judged_in_the_writes_target_view() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let t = brief_of(&r, &Scope::Canon, &[&s.a]).base.token;
    write_cited(&mut r, "x", upd(&s.a, "X moved A."), Some(&t)).unwrap();
    assert_eq!(
        write_cited(&mut r, "x", upd(&s.a, "X again."), Some(&t))
            .unwrap_err()
            .code,
        E_BRIEF_STALE
    );
    write_cited(&mut r, "y", upd(&s.a, "Y's A."), Some(&t)).expect("A has not moved in y");
}

#[test]
fn bwr1_a_briefing_refusal_comes_before_a_stale_base() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let stale_base = "0".repeat(64);
    let e = write_cited(
        &mut r,
        "w",
        json!([{"op": "update", "id": s.a.to_string(), "base": stale_base, "body": "A."}]),
        None,
    )
    .unwrap_err();
    assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
}
