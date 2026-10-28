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
    // x rewords stable normative content: a clarification lands (D-ME2HD)
    let req = WriteRequest {
        view: x.default_view(),
        ops: serde_json::from_value(upd(&s.a, "X's A.")).unwrap(),
        justification: Some(rdy_model::Justification::Editorial {
            class: rdy_model::EditorialClass::Clarify,
            reason: "reword".into(),
        }),
        message: String::new(),
    };
    r.write_briefed(&x, req, Some(tx)).unwrap();
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

/// A create's subject is its parent (its coverage target), so the exemption follows the
/// parent's type, as for `rebalance`: a presentational block created under stable content
/// needs the parent briefed, and anything created under a presentational parent is exempt.
#[test]
fn bwr1_a_create_is_exempt_by_its_parents_type_not_its_own() {
    let (_d, mut r, ids) = rdy_qa::golden::presentational_site();
    let t = brief_of(&r, &Scope::Canon, &[&ids.doc]).base.token;
    write_cited(
        &mut r,
        "w",
        json!([{"op": "update", "id": ids.doc.to_string(), "fields": {"lifecycle": "stable"}}]),
        Some(&t),
    )
    .unwrap();
    let section = r
        .read()
        .block(&Scope::Canon, &ids.aside)
        .unwrap()
        .parent
        .unwrap();
    let aside = json!([{"op": "create", "type": "aside", "parent": section.to_string(), "body": "More decoration."}]);
    let e = write_cited(&mut r, "w", aside, None).unwrap_err();
    assert_eq!(
        e.code, E_BRIEF_REQUIRED,
        "the stable section is the subject: {e}"
    );
    assert_eq!(
        e.briefing.as_ref().unwrap()["ids"],
        json!([section.to_string()])
    );

    // a pack whose presentational `aside` may hold claims
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let pack = rdy_qa::golden::PRESENTATIONAL_PACK
        .replace("body: required }", "body: required, children: [claim] }");
    std::fs::write(dir.path().join(".rdy/packs/p.yaml"), pack).unwrap();
    std::fs::write(
        dir.path().join(".rdy.yaml"),
        "pack: p\nname: Presentational\n",
    )
    .unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let base = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "t", "title": "T", "doc_type": "page", "lifecycle": "stable"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "aside", "id": "$a", "parent": "$s", "body": "Decorative."}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let container = &base.created["$a"];
    write_cited(
        &mut r,
        "w",
        json!([{"op": "create", "type": "claim", "parent": container, "body": "Inside the aside."}]),
        None,
    )
    .expect("Q6d: a presentational parent is exempt");
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

// ---- plb1: a refusal names the parent a create needs briefed (PL 58) ----

/// The fix of a refused create's parent issue.
const PARENT_FIX: &str = "brief this parent; a briefing of a parent covers its new children";

/// The fix of a refusal before the tools expand it.
const ENGINE_FIX: &str = "brief these blocks (rdy_brief) and retry with the token";

/// The fix of a refusal the tools expanded into a fresh briefing whose token is `t`.
fn fresh_fix(t: &str) -> String {
    format!(
        "retry with brief: \"{t}\"; this error's briefing covers exactly the blocks the write touches"
    )
}

/// A briefing issue on `id`, as the error serializes it.
fn brief_issue(id: &BlockId, message: &str, fix: &str) -> Value {
    json!({"severity": "error", "code": E_BRIEF_REQUIRED, "block": id.to_string(),
        "message": message, "fix": fix})
}

/// The issue on a parent the write creates a `ty` under.
fn parent_issue(id: &BlockId, ty: &str) -> Value {
    brief_issue(
        id,
        &format!("not covered by the cited briefing: the write creates a {ty} under it"),
        PARENT_FIX,
    )
}

/// The issue on a block the write touches in any other way.
fn plain_issue(id: &BlockId) -> Value {
    brief_issue(
        id,
        "not covered by the cited briefing",
        "brief it (or, to create under it, its parent)",
    )
}

/// The refusal's top message for these `entries`.
fn not_covered(entries: &[String]) -> String {
    format!(
        "the cited briefing does not cover {}, which this write touches on stable content",
        entries.join(", ")
    )
}

fn issues_of(e: &RdyError) -> Value {
    serde_json::to_value(&e.issues).unwrap()
}

#[test]
fn plb1_create_refusal_names_its_parent_and_type() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let ta = brief_of(&r, &Scope::Canon, &[&s.a]).base.token;
    let create = json!([{"op": "create", "type": "prose", "id": "$p",
        "parent": s.section.to_string(), "body": "New prose."}]);
    let e = write_cited(&mut r, "w", create.clone(), Some(&ta)).unwrap_err();
    assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
    let message = not_covered(&[format!("{} (the parent of the new prose)", s.section)]);
    assert_eq!(e.message, message);
    assert_eq!(e.fix, ENGINE_FIX);
    assert_eq!(issues_of(&e), json!([parent_issue(&s.section, "prose")]));
    assert_eq!(
        e.briefing,
        Some(json!({"ids": [s.section.to_string()]})),
        "only the parent"
    );
    assert!(
        r.read().included("w").is_empty(),
        "a failed write changes nothing"
    );
    let (e, t) = token_in(&r, e, "w");
    assert_eq!(e.message, message, "the expansion keeps the message");
    assert_eq!(e.fix, fresh_fix(&t));
    let ok = write_cited(&mut r, "w", create, Some(&t)).expect("the retry lands");
    assert_eq!(ok.created.len(), 1, "{:?}", ok.created);
}

#[test]
fn plb1_mixed_creates_keep_one_sorted_issue_per_target() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let sec = s.section.to_string();
    // a second stable parent: a subsection of the stable section, landed
    let ts = brief_of(&r, &Scope::Canon, &[&s.section]).base.token;
    let w = write_cited(
        &mut r,
        "sub",
        json!([{"op": "create", "type": "section", "id": "$sub", "parent": sec,
            "fields": {"title": "More rules"}}]),
        Some(&ts),
    )
    .unwrap();
    let sub = BlockId::parse(&w.created["$sub"]).unwrap();
    land(&mut r, "sub").unwrap();
    let tc = brief_of(&r, &Scope::Canon, &[&s.c]).base.token;
    let ops = json!([
        {"op": "update", "id": sec, "fields": {"title": "Rules, revised"}},
        {"op": "create", "type": "prose", "parent": sec, "body": "P."},
        {"op": "create", "type": "claim", "parent": sec, "body": "Q."},
        {"op": "create", "type": "section", "id": "$n", "parent": sub.to_string(),
            "fields": {"title": "Nested"}},
        {"op": "create", "type": "claim", "parent": "$n", "body": "Under the new section."},
        {"op": "update", "id": s.a.to_string(), "body": "A, revised."}
    ]);
    let e = write_cited(&mut r, "w", ops, Some(&tc)).unwrap_err();
    assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
    let order: std::collections::BTreeSet<BlockId> =
        [s.section.clone(), sub.clone(), s.a.clone()].into();
    let (mut entries, mut issues) = (vec![], vec![]);
    for id in &order {
        if *id == s.section {
            // the first create's type: an update of the same target does not count
            entries.push(format!("{id} (the parent of the new prose)"));
            issues.push(parent_issue(id, "prose"));
        } else if *id == sub {
            // the create under $n hangs from $n's own anchor, sub, after $n's create
            entries.push(format!("{id} (the parent of the new section)"));
            issues.push(parent_issue(id, "section"));
        } else {
            entries.push(id.to_string());
            issues.push(plain_issue(id));
        }
    }
    assert_eq!(e.message, not_covered(&entries));
    assert_eq!(issues_of(&e), Value::Array(issues));
    assert_eq!(
        e.briefing,
        Some(json!({"ids": order.iter().map(ToString::to_string).collect::<Vec<_>>()})),
        "no id for the block the batch creates"
    );
    assert!(r.read().included("w").is_empty(), "not even the draft op");
}

#[test]
fn plb1_decision_refusal_explains_covered_old_decision() {
    // supersede and revoke, each on a fresh fixture; the token covers the old decision only
    for revoke in [false, true] {
        let (_d, mut r) = project();
        let s = stable_spec(&mut r);
        let (sec, old) = (s.section.to_string(), s.decision.to_string());
        let run = |r: &mut Rdy, brief: Option<String>| {
            if revoke {
                r.revoke(&writer(), "w", &old, "no longer true", &sec, brief)
            } else {
                let d = DecisionInput {
                    parent: sec.clone(),
                    statement: "Keep A, firmly".into(),
                    brief,
                    ..Default::default()
                };
                r.supersede(&writer(), "w", &old, None, d)
            }
        };
        let td = brief_of(&r, &Scope::Canon, &[&s.decision]).base.token;
        let e = run(&mut r, Some(td)).unwrap_err();
        assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
        let message = format!(
            "{}. The briefing covers {old}, but a new decision needs a briefing of its parent.",
            not_covered(&[format!("{sec} (the parent of the new decision)")])
        );
        assert_eq!(e.message, message, "revoke: {revoke}");
        assert_eq!(issues_of(&e), json!([parent_issue(&s.section, "decision")]));
        assert_eq!(e.fix, ENGINE_FIX, "a direct engine error is not expanded");
        assert_eq!(e.briefing, Some(json!({"ids": [sec]})));
        assert!(
            r.read().included("w").is_empty(),
            "a failed write changes nothing"
        );
        let (e, t) = token_in(&r, e, "w");
        assert_eq!(e.message, message);
        assert_eq!(e.fix, fresh_fix(&t));
        let rec = r
            .briefs()
            .get(&t)
            .expect("the fresh briefing is registered");
        assert!(rec.covers(&s.section) && !rec.covers(&s.decision));
        run(&mut r, Some(t)).expect("the retry lands");
    }

    // none of these gets the sentence
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let (sec, old) = (s.section.to_string(), s.decision.to_string());
    let input = |brief: Option<&str>| DecisionInput {
        parent: sec.clone(),
        statement: "Prefer C".into(),
        brief: brief.map(str::to_string),
        ..Default::default()
    };
    let plain = not_covered(&[format!("{sec} (the parent of the new decision)")]);
    let td = brief_of(&r, &Scope::Canon, &[&s.decision]).base.token;
    let e = r.decide(&writer(), "w", input(Some(&td))).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_BRIEF_REQUIRED, plain.as_str()),
        "decide"
    );
    let no_token = format!("this write touches stable content ({sec}) and cites no briefing");
    let e = r
        .supersede(&writer(), "w", &old, None, input(None))
        .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_BRIEF_REQUIRED, no_token.as_str())
    );
    let e = r
        .revoke(&writer(), "w", &old, "no longer true", &sec, None)
        .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_BRIEF_REQUIRED, no_token.as_str())
    );
    let unknown = Hash::of(b"never").to_string();
    for e in [
        r.supersede(&writer(), "w", &old, None, input(Some(&unknown)))
            .unwrap_err(),
        r.revoke(
            &writer(),
            "w",
            &old,
            "no longer true",
            &sec,
            Some(unknown.clone()),
        )
        .unwrap_err(),
    ] {
        assert_eq!(e.code, E_BRIEF_STALE, "{e}");
        assert!(e.message.contains("unknown"), "{e}");
        assert!(!e.message.contains("The briefing covers"), "{e}");
    }
    // a briefing of A records the decision that governs A, but does not cover it
    let ta = brief_of(&r, &Scope::Canon, &[&s.a]).base.token;
    let rec = r.briefs().get(&ta).unwrap();
    assert!(rec.recorded.contains_key(&s.decision) && !rec.covers(&s.decision));
    for e in [
        r.supersede(&writer(), "w", &old, None, input(Some(&ta)))
            .unwrap_err(),
        r.revoke(
            &writer(),
            "w",
            &old,
            "no longer true",
            &sec,
            Some(ta.clone()),
        )
        .unwrap_err(),
    ] {
        assert_eq!(
            (e.code, e.message.as_str()),
            (E_BRIEF_REQUIRED, plain.as_str()),
            "context only"
        );
    }
    // a covering token gone stale is E_BRIEF_STALE, with no sentence
    let tsd = brief_of(&r, &Scope::Canon, &[&s.section, &s.decision])
        .base
        .token;
    write_cited(
        &mut r,
        "w",
        json!([{"op": "update", "id": sec, "fields": {"title": "Rules, moved"}}]),
        Some(&tsd),
    )
    .unwrap();
    let e = r
        .supersede(&writer(), "w", &old, None, input(Some(&tsd)))
        .unwrap_err();
    assert_eq!(e.code, E_BRIEF_STALE, "{e}");
    assert!(!e.message.contains("The briefing covers"), "{e}");
}
