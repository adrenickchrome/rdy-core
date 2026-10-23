//! rf6 (D-VPATZ, D-X4RYX, D-Z8P09): the briefing's governs rows follow the four routes with
//! proposed decisions and name the entity routes in `through`; a held source or reflects globs
//! govern a stable block for coverage without a decision; a carried coverage layer equals a
//! rebuild across source deletes and view edits; an entity route never widens a decision's
//! direct reach.
use rdy_engine::brief::FollowUp;
use rdy_engine::consequence::*;
use rdy_engine::coverage::{
    CoverageIssues, IssueCode, issues_rebuilt_for_tests, rebuilt_for_tests, structural_for_tests,
};
use rdy_engine::*;
use rdy_model::{BlockId, Hash};
use rdy_qa::edit_loop::*;
use rdy_qa::fixtures::*;
use rdy_qa::tensions::{ag, j_typo, try_put_j};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn view(name: &str) -> Scope {
    Scope::View(name.into())
}

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn decide_in(
    r: &mut Rdy,
    v: &str,
    parent: &BlockId,
    statement: &str,
    affects: &[&BlockId],
) -> BlockId {
    let w = r
        .decide(
            &writer(),
            v,
            DecisionInput {
                parent: parent.to_string(),
                statement: statement.into(),
                affects: affects.iter().map(|a| a.to_string()).collect(),
                ..Default::default()
            },
        )
        .unwrap();
    bid(&w.created["$decision"])
}

/// A governs row by decision: status, blocks, and its (block, entity) route pairs.
type Row = (String, Vec<String>, Vec<(String, String)>);

/// The exact `ungrounded` fix (D-X4RYX).
fn ungrounded_fix(b: &BlockId) -> String {
    format!(
        "link {b} to the decision that governs it, with decided_by or implements, or justify its next change with reflects; rdy_trace shows the landing that introduced it"
    )
}

fn ungrounded<'a>(i: &'a CoverageIssues, b: &BlockId) -> Option<&'a str> {
    i.blocks[b]
        .issues
        .iter()
        .find(|x| x.code == IssueCode::Ungrounded)
        .map(|x| x.fix.as_str())
}

#[test]
fn rf6_brief_governs_keeps_proposed_and_through() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Hub"}, "body": "The centre."},
        {"op": "create", "type": "term", "id": "$u", "parent": "$s", "fields": {"name": "Spoke"}, "body": "An arm."},
        {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds.",
         "links": {"describes": [{"target": "$t"}, {"target": "$u"}]}},
        {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B holds.",
         "links": {"describes": [{"target": "$t"}, {"target": "$u"}]}}
    ]))
    .unwrap();
    let c = |k: &str| bid(&w.created[k]);
    let (s, t, u, a, b) = (c("$s"), c("$t"), c("$u"), c("$a"), c("$b"));
    land(&mut r, "work").unwrap();
    // D1 affects both entities and A is decided by it: A's route is decided_by, B's the lower
    // entity id
    let d1 = decide_in(&mut r, "work", &s, "Hubs stay small", &[&t, &u]);
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": a.to_string(), "links": {"decided_by": [{"target": d1.to_string()}]}}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let low = std::cmp::min(&t, &u).clone();
    let before = brief_of(&r, &view("w"), &[&a, &b]);
    // P, proposed in view w, affects the spoke only
    let p = decide_in(&mut r, "w", &s, "Spokes stay short", &[&u]);
    let got = brief_of(&r, &view("w"), &[&a, &b]);
    let rows: BTreeMap<String, Row> = got
        .governs
        .iter()
        .map(|g| {
            (
                g.decision.clone(),
                (
                    g.status.clone(),
                    g.blocks.clone(),
                    g.through
                        .iter()
                        .map(|x| (x.block.clone(), x.entity.clone()))
                        .collect(),
                ),
            )
        })
        .collect();
    let mut both = vec![a.to_string(), b.to_string()];
    both.sort();
    let mut pairs = vec![
        (a.to_string(), u.to_string()),
        (b.to_string(), u.to_string()),
    ];
    pairs.sort();
    let mut want = BTreeMap::new();
    want.insert(
        d1.to_string(),
        (
            "live".to_string(),
            both.clone(),
            vec![(b.to_string(), low.to_string())],
        ),
    );
    want.insert(p.to_string(), ("proposed".to_string(), both.clone(), pairs));
    assert_eq!(rows, want);
    // the wire omits an empty through
    let v = serde_json::to_value(&before).unwrap();
    assert!(
        v["governs"][0].get("through").is_some(),
        "D1 governs B through an entity: {v}"
    );
    // the entity route is a recorded dependency: the token changes with P; the covered ids do
    // not
    assert_ne!(got.base.token, before.base.token);
    assert_eq!(got.base.blocks, before.base.blocks);
    assert_eq!(got.record.covers, before.record.covers);
    for x in [&d1, &p, &u, &low] {
        assert!(got.record.recorded.contains_key(x), "{x} is recorded");
    }
    // a change to P's affects list moves the briefing
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": p.to_string(), "links": {"affects": [{"target": t.to_string()}]}}]),
    )
    .unwrap();
    let m = r.read().materialized(&view("w")).unwrap();
    assert_eq!(got.record.moved(&m), vec![p.clone()]);
    // so does a change to the entity the route runs through
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": u.to_string(), "body": "An arm, renamed."}]),
    )
    .unwrap();
    let m = r.read().materialized(&view("w")).unwrap();
    let moved: BTreeSet<BlockId> = got.record.moved(&m).into_iter().collect();
    assert_eq!(moved, BTreeSet::from([p.clone(), u.clone()]));
}

/// Doc `spec` with stable section Summary, in view `w`: source S, source G (deleted by a later
/// write), claim c1 reflecting S, claim c2 with reflects globs, claim c3 reflecting G, claim c4
/// reflecting claim c1 (no source), claim c5 with an empty glob list.
#[test]
fn rf6_sources_and_globs_govern_without_decisions() {
    let (_d, mut r) = project();
    let w = try_put_j(&mut r, "w", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Summary", "lifecycle": "stable"}},
        {"op": "create", "type": "source", "id": "$src", "parent": "$s", "fields": {"title": "Release 2", "kind": "release", "locator": "v2"}},
        {"op": "create", "type": "source", "id": "$gone", "parent": "$s", "fields": {"title": "Release 1", "kind": "release", "locator": "v1"}},
        {"op": "create", "type": "claim", "id": "$c1", "parent": "$s", "body": "One holds.", "links": {"reflects": [{"target": "$src"}]}},
        {"op": "create", "type": "claim", "id": "$c2", "parent": "$s", "body": "Two holds.", "fields": {"reflects": ["src/*.rs"]}},
        {"op": "create", "type": "claim", "id": "$c3", "parent": "$s", "body": "Three holds.", "links": {"reflects": [{"target": "$gone"}]}},
        {"op": "create", "type": "claim", "id": "$c4", "parent": "$s", "body": "Four holds.", "links": {"reflects": [{"target": "$c1"}]}},
        {"op": "create", "type": "claim", "id": "$c5", "parent": "$s", "body": "Five holds.", "fields": {"reflects": []}}
    ]), j_typo())
    .unwrap();
    let c = |k: &str| bid(&w.created[k]);
    try_put_j(
        &mut r,
        "w",
        json!([{"op": "delete", "id": c("$gone").to_string()}]),
        j_typo(),
    )
    .unwrap();
    let st = r.read();
    let i = st.coverage_issues(&view("w")).unwrap();
    for k in ["$c1", "$c2"] {
        assert_eq!(ungrounded(&i, &c(k)), None, "{k} is governed");
        let ctx = st.context(&view("w"), &c(k)).unwrap();
        assert!(
            ctx.governing.is_empty(),
            "{k}: a source is no governing decision"
        );
    }
    for k in ["$c3", "$c4", "$c5"] {
        assert_eq!(
            ungrounded(&i, &c(k)),
            Some(ungrounded_fix(&c(k)).as_str()),
            "{k} is ungrounded"
        );
    }
}

/// The memoised (possibly carried) structural layer and issues equal a full rebuild.
fn same(r: &Rdy, scope: &Scope) {
    let st = r.read();
    let carried = structural_for_tests(&st, scope).unwrap();
    let rebuilt = rebuilt_for_tests(&st, scope).unwrap();
    assert_eq!(carried.blocks, rebuilt.blocks);
    assert_eq!(carried.mentions, rebuilt.mentions);
    let warm = st.coverage_issues(scope).unwrap();
    let cold = issues_rebuilt_for_tests(&st, scope).unwrap();
    assert_eq!(warm.blocks, cold.blocks);
}

#[test]
fn rf6_coverage_carry_matches_rebuild_after_source_delete() {
    let (_d, mut r) = project();
    let next = view("next");
    let w = try_put_j(&mut r, "next", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Summary", "lifecycle": "stable"}},
        {"op": "create", "type": "source", "id": "$src", "parent": "$s", "fields": {"title": "Release 2", "kind": "release", "locator": "v2"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "It holds.", "links": {"reflects": [{"target": "$src"}]}}
    ]), j_typo())
    .unwrap();
    let k = |x: &str| bid(&w.created[x]);
    let (s, src, c) = (k("$s"), k("$src"), k("$c"));
    let governed = |r: &Rdy| ungrounded(&r.read().coverage_issues(&next).unwrap(), &c).is_none();
    assert!(governed(&r));
    same(&r, &next);
    // delete the source: the reflecting claim is ungrounded, warm and cold
    let put = |r: &mut Rdy, ops: Value| try_put_j(r, "next", ops, j_typo()).unwrap();
    put(&mut r, json!([{"op": "delete", "id": src.to_string()}]));
    assert!(!governed(&r), "a deleted source governs nothing");
    same(&r, &next);
    // a new source and a link to it: governed again
    let w2 = put(
        &mut r,
        json!([{"op": "create", "type": "source", "id": "$s2", "parent": s.to_string(),
            "fields": {"title": "Release 3", "kind": "release", "locator": "v3"}}]),
    );
    let s2 = bid(&w2.created["$s2"]);
    assert!(!governed(&r));
    same(&r, &next);
    let linked = put(
        &mut r,
        json!([{"op": "update", "id": c.to_string(), "links": {"reflects": [{"target": s2.to_string()}]}}]),
    );
    assert!(governed(&r));
    same(&r, &next);
    // exclude the change that created the source (with the link that needs it), then include
    // both again
    let changes = vec![w2.change.clone()];
    r.view_exclude(&ag("next"), "next", &changes, true).unwrap();
    assert!(!governed(&r), "excluded: no source");
    same(&r, &next);
    r.view_include(
        &ag("next"),
        "next",
        &[w2.change.clone(), linked.change.clone()],
    )
    .unwrap();
    assert!(governed(&r), "included again");
    same(&r, &next);
}

fn consequences_of(r: &Rdy, change: &str) -> Consequences {
    r.read()
        .consequences(&view("w"), &change.parse::<Hash>().unwrap(), None)
        .unwrap()
}

#[test]
fn rf6_direct_reach_does_not_expand_with_governing() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    // a term Hub that C describes, and D2 that affects the term, landed
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "term", "id": "$t", "parent": s.section.to_string(), "fields": {"name": "Hub"}, "body": "The centre."}
    ]))
    .unwrap();
    let t = bid(&w.created["$t"]);
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": s.c.to_string(), "links": {"describes": [{"target": t.to_string()}]}}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let d2 = decide_in(&mut r, "d", &s.section, "C stays short", &[&t]);
    land(&mut r, "d").unwrap();
    // the entity route governs C
    let ctx = r.read().context(&Scope::Canon, &s.c).unwrap();
    assert_eq!(ctx.governing, vec![d2.to_string()]);
    // a decision that refines D2 reaches the term it affects, never C
    let res = r
        .decide(
            &writer(),
            "w",
            DecisionInput {
                parent: s.section.to_string(),
                statement: "Keep the hub small".into(),
                refines: vec![d2.to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    let new = bid(&res.created["$decision"]);
    let cons = consequences_of(&r, &res.change);
    let on = |b: &BlockId| {
        cons.items
            .iter()
            .filter(|x| x.block == b.to_string())
            .count()
    };
    assert_eq!(on(&t), 1, "{cons:?}");
    assert!(
        cons.items
            .iter()
            .any(|x| x.block == t.to_string() && x.follow_up == FollowUp::Confirm)
    );
    assert_eq!(on(&s.c), 0, "{cons:?}");
    let b = brief_of(&r, &view("w"), &[&new]);
    let reached: Vec<&String> = b.reaches.iter().flat_map(|x| x.blocks.iter()).collect();
    assert_eq!(reached, vec![&t.to_string()], "{:?}", b.reaches);
    let plan = serde_json::to_string(&r.read().view_plan("w").unwrap()).unwrap();
    assert!(!plan.contains(&s.c.to_string()), "{plan}");
}

/// The spec of [`spec_doc`] with a term Hub that claim C describes, and `affects` (a landed
/// decision under the doc, which is draft) affecting the term: so D governs C through the
/// entity. The section is then made stable and landed, so C is stable content.
fn governed_through_hub(r: &mut Rdy) -> (Spec, BlockId, BlockId) {
    let s = spec_doc(r);
    let w = write(r, "work", json!([
        {"op": "create", "type": "term", "id": "$t", "parent": s.section.to_string(), "fields": {"name": "Hub"}, "body": "The centre."}
    ]))
    .unwrap();
    let t = bid(&w.created["$t"]);
    write(
        r,
        "work",
        json!([{"op": "update", "id": s.c.to_string(), "links": {"describes": [{"target": t.to_string()}]}}]),
    )
    .unwrap();
    land(r, "work").unwrap();
    let d = decide_in(r, "d", &s.doc, "C stays short", &[&t]);
    land(r, "d").unwrap();
    let token = brief_of(r, &Scope::Canon, &[&s.section]).base.token;
    write_cited(
        r,
        "st",
        json!([{"op": "update", "id": s.section.to_string(), "fields": {"lifecycle": "stable"}}]),
        Some(&token),
    )
    .expect("make the section stable");
    land(r, "st").unwrap();
    (s, t, d)
}

/// A stable update of C in view `v` citing `token`: refused with E_BRIEF_STALE.
fn refused_stale(r: &mut Rdy, v: &str, c: &BlockId, token: &str) -> RdyError {
    let e = write_cited(
        r,
        v,
        json!([{"op": "update", "id": c.to_string(), "body": "C stands alone, still."}]),
        Some(token),
    )
    .expect_err("the old briefing is stale");
    assert_eq!(e.code, brief::E_BRIEF_STALE, "{e}");
    e
}

#[test]
fn rf6_a_removed_direct_affects_link_refuses_the_old_briefing() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    // D, proposed in view w under the (draft) doc, affects stable claim C directly
    let d = decide_in(&mut r, "w", &s.doc, "C stays short", &[&s.c]);
    let old = brief_of(&r, &view("w"), &[&s.c]);
    assert!(
        old.governs.iter().any(|g| g.decision == d.to_string()),
        "{:?}",
        old.governs
    );
    assert!(old.record.recorded.contains_key(&d), "D is recorded");
    // D's affects link moves to B: D no longer governs C
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": d.to_string(), "links": {"affects": [{"target": s.b.to_string()}]}}]),
    )
    .unwrap();
    let fresh = brief_of(&r, &view("w"), &[&s.c]);
    assert!(fresh.governs.iter().all(|g| g.decision != d.to_string()));
    assert_ne!(fresh.base.token, old.base.token);
    let e = refused_stale(&mut r, "w", &s.c, &old.base.token);
    assert!(e.message.contains(&d.to_string()), "{e}");
    // the fresh briefing is current
    write_cited(
        &mut r,
        "w",
        json!([{"op": "update", "id": s.c.to_string(), "body": "C stands alone, still."}]),
        Some(&fresh.base.token),
    )
    .unwrap();
}

#[test]
fn rf6_a_new_superseder_refuses_the_old_briefing() {
    let (_d, mut r) = project();
    let (s, _t, d) = governed_through_hub(&mut r);
    let old = brief_of(&r, &Scope::Canon, &[&s.c]);
    assert!(old.governs.iter().any(|g| g.decision == d.to_string()));
    // an accepted decision supersedes D, landed; D's own version does not move
    r.supersede(
        &writer(),
        "x",
        &d.to_string(),
        None,
        DecisionInput {
            parent: s.doc.to_string(),
            statement: "C stays very short".into(),
            no_inherit: vec!["affects".into()],
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "x").unwrap();
    let fresh = brief_of(&r, &Scope::Canon, &[&s.c]);
    assert!(fresh.governs.iter().all(|g| g.decision != d.to_string()));
    assert_ne!(fresh.base.token, old.base.token);
    refused_stale(&mut r, "w", &s.c, &old.base.token);
}

#[test]
fn rf6_a_new_decision_on_the_entity_refuses_the_old_briefing() {
    let (_d, mut r) = project();
    let (s, t, _d1) = governed_through_hub(&mut r);
    let old = brief_of(&r, &Scope::Canon, &[&s.c]);
    // another decision, proposed in view w, affects the entity
    let d2 = decide_in(&mut r, "w", &s.doc, "Hubs stay small", &[&t]);
    let fresh = brief_of(&r, &view("w"), &[&s.c]);
    assert!(fresh.governs.iter().any(|g| g.decision == d2.to_string()));
    assert_ne!(fresh.base.token, old.base.token);
    refused_stale(&mut r, "w", &s.c, &old.base.token);
}

#[test]
fn rf6_a_pack_that_drops_defines_refuses_the_old_briefing() {
    let (dir, mut r) = project();
    let (s, _t, d) = governed_through_hub(&mut r);
    let old = brief_of(&r, &Scope::Canon, &[&s.c]);
    assert!(old.governs.iter().any(|g| g.decision == d.to_string()));
    // a valid pack whose term defines nothing: the term is no entity, so no route
    std::fs::write(
        dir.path().join(".rdy/packs/plain.yaml"),
        "name: plain\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  term: { prefix: t, traits: [informative], body: required }\n",
    )
    .unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: plain\n").unwrap();
    assert!(matches!(r.reload_pack().unwrap(), ReloadOutcome::Reloaded));
    let fresh = brief_of(&r, &Scope::Canon, &[&s.c]);
    assert!(fresh.governs.iter().all(|g| g.decision != d.to_string()));
    assert_ne!(fresh.base.token, old.base.token);
    refused_stale(&mut r, "w", &s.c, &old.base.token);
}

#[test]
fn rf6_a_part_supersede_of_a_direct_governor_moves_the_token() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    // A is decided by the spec's decision D, a direct route with no entity
    let old = brief_of(&r, &Scope::Canon, &[&s.a]);
    let row = |b: &Briefing| {
        b.governs
            .iter()
            .find(|g| g.decision == s.decision.to_string())
            .map(|g| g.status.clone())
    };
    assert_eq!(row(&old).as_deref(), Some("live"));
    r.supersede(
        &writer(),
        "x",
        &s.decision.to_string(),
        Some("the first half".into()),
        DecisionInput {
            parent: s.doc.to_string(),
            statement: "Keep half of A".into(),
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "x").unwrap();
    // no recorded version moved, yet the row's status did: a fresh token, and the old one is
    // refused
    let fresh = brief_of(&r, &Scope::Canon, &[&s.a]);
    assert_eq!(row(&fresh).as_deref(), Some("partially_superseded"));
    assert_eq!(fresh.record.recorded, old.record.recorded);
    assert_ne!(fresh.base.token, old.base.token);
    refused_stale(&mut r, "w", &s.a, &old.base.token);
}
