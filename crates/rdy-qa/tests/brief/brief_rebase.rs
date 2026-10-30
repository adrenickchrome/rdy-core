//! rbst1: briefing tokens across a rebase (Q6a, amendment §4.5 item 3): a landing that rebases a
//! view makes a token stale exactly when it moves a block the token recorded in that view.
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::edit_loop::*;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

fn upd(id: &BlockId, body: &str) -> Value {
    json!([{"op": "update", "id": id.to_string(), "body": body}])
}
fn w() -> Scope {
    Scope::View("w".into())
}

/// The stable spec; view `w` (unpinned) holds one change: a new claim under the stable section.
fn with_w() -> (tempfile::TempDir, Rdy, Spec) {
    let (d, mut r) = project();
    let s = stable_spec(&mut r);
    let t = brief_of(&r, &Scope::Canon, &[&s.section]).base.token;
    write_cited(
        &mut r,
        "w",
        json!([{"op": "create", "type": "claim", "parent": s.section.to_string(), "body": "W's own claim."}]),
        Some(&t),
    )
    .unwrap();
    (d, r, s)
}

/// Lands an update of `id` from view `o`, briefed in canon. The update rewords stable normative
/// content, so it is justified as a clarification (D-ME2HD).
fn land_update(r: &mut Rdy, id: &BlockId, body: &str) {
    let t = brief_of(r, &Scope::Canon, &[id]).base.token;
    let req = WriteRequest {
        view: "o".into(),
        ops: serde_json::from_value(upd(id, body)).unwrap(),
        justification: Some(rdy_model::Justification::Editorial {
            class: rdy_model::EditorialClass::Clarify,
            reason: "reword".into(),
        }),
        message: String::new(),
    };
    r.write_briefed(&writer(), req, Some(t)).unwrap();
    land(r, "o").unwrap();
}

#[test]
fn rbst1_a_rebase_that_moves_a_covered_block_makes_the_token_stale() {
    let (_d, mut r, s) = with_w();
    let t = brief_of(&r, &w(), &[&s.a]).base.token;
    land_update(&mut r, &s.a, "A, landed from o.");
    assert_eq!(
        r.read().effective_base("w"),
        r.read().canon_head(),
        "w was rebased"
    );
    let e = write_cited(&mut r, "w", upd(&s.a, "A, from w."), Some(&t)).unwrap_err();
    assert_eq!(e.code, E_BRIEF_STALE, "{e}");
    let moved: Vec<String> = e
        .issues
        .iter()
        .filter_map(|i| i.block.as_ref().map(|b| b.to_string()))
        .collect();
    assert_eq!(moved, vec![s.a.to_string()]);
    let (_e, fresh) = token_in(&r, e, "w");
    write_cited(&mut r, "w", upd(&s.a, "A, from w."), Some(&fresh))
        .expect("the fresh token lands the retry");
}

#[test]
fn rbst1_a_rebase_that_moves_a_dependency_makes_the_token_stale() {
    let (_d, mut r, s) = with_w();
    let t = brief_of(&r, &w(), &[&s.b]).base.token;
    land_update(&mut r, &s.a, "A, landed from o.");
    let e = write_cited(&mut r, "w", upd(&s.b, "B, from w."), Some(&t)).unwrap_err();
    assert_eq!(e.code, E_BRIEF_STALE, "{e}");
    assert!(e.message.contains(&s.a.to_string()), "{e}");
}

#[test]
fn rbst1_a_rebase_that_moves_nothing_recorded_keeps_the_token() {
    let (_d, mut r, s) = with_w();
    let t = brief_of(&r, &w(), &[&s.b]).base.token;
    land_update(&mut r, &s.c, "C, landed from o.");
    assert_eq!(
        r.read().effective_base("w"),
        r.read().canon_head(),
        "w was rebased"
    );
    write_cited(&mut r, "w", upd(&s.b, "B, from w."), Some(&t)).expect("C is not recorded");
}

#[test]
fn rbst1_a_pinned_view_is_not_rebased_and_its_token_stays_current() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    r.view_create(&writer(), "wp", None, true).unwrap();
    let wp = Scope::View("wp".into());
    let ts = brief_of(&r, &wp, &[&s.section]).base.token;
    write_cited(
        &mut r,
        "wp",
        json!([{"op": "create", "type": "claim", "parent": s.section.to_string(), "body": "Pinned claim."}]),
        Some(&ts),
    )
    .unwrap();
    let t = brief_of(&r, &wp, &[&s.a]).base.token;
    let base = r.read().effective_base("wp");
    land_update(&mut r, &s.a, "A, landed from o.");
    assert_eq!(
        r.read().effective_base("wp"),
        base,
        "a pinned view keeps its base"
    );
    write_cited(&mut r, "wp", upd(&s.a, "A, from wp."), Some(&t)).expect("nothing moved in wp");
}
