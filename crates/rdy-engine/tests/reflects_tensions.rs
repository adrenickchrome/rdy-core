//! rf10 (D-4WC6R, D-ZWT4H): a `Reflects` justification counts only when every source it cites is
//! a source the judged view holds. The rewrites are typed store commits with the normalized model
//! `Reflects` and explicit `reflects` links (no raw reflects input); every other step goes
//! through the engine, so the warm tension set is the one a view op or a landing carries.
mod common;

use rdy_engine::semantic::candidates::Rule;
use rdy_engine::tensions::index::held_source_builds;
use rdy_engine::tensions::{Tension, TensionKind};
use rdy_engine::{Coverage, Rdy, Scope, Session, WriteRequest, WriteResult, parse_ops};
use rdy_model::{BlockId, Change, EditorialClass, Hash, Justification, Link, Op, Provenance, Role};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;

fn writer() -> Session {
    Session::human("human:qa".into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human("human:qa".into(), Role::Approver).unwrap()
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn view(v: &str) -> Scope {
    Scope::View(v.into())
}

/// A registered briefing of `ids` in `scope`: its token.
fn brief(r: &Rdy, scope: &Scope, ids: &[&BlockId]) -> String {
    let b = r
        .read()
        .brief(
            scope,
            &Coverage {
                ids: ids.iter().map(|i| i.to_string()).collect(),
                path: None,
            },
            None,
        )
        .unwrap();
    b.register(&r.briefs());
    b.base.token
}

/// An engine write in `v`, citing a briefing of `briefed` (in canon) when any.
fn put(r: &mut Rdy, v: &str, ops: Value, briefed: &[&BlockId]) -> WriteResult {
    let token = (!briefed.is_empty()).then(|| brief(r, &Scope::Canon, briefed));
    r.write_briefed(
        &writer(),
        WriteRequest {
            view: v.into(),
            ops: parse_ops(ops.as_array().unwrap().clone()).unwrap(),
            justification: None,
            message: String::new(),
        },
        token,
    )
    .unwrap_or_else(|e| panic!("write to {v}: {e}"))
}

fn land(r: &mut Rdy, v: &str) {
    let p = r
        .propose(&writer(), v, v, Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &approver(), &p);
    r.land(&approver(), &p.id).unwrap();
}

/// Commits one op to `v` straight through the store (the raw-commit fixture), justified by `j`,
/// with `deps` (`None`: every change `v` includes).
fn commit(r: &mut Rdy, v: &str, op: Op, j: Option<Justification>, deps: Option<Vec<Hash>>) -> Hash {
    let deps = deps.unwrap_or_else(|| r.read().included(v).to_vec());
    let h = r
        .store_mut()
        .commit_to_view(
            v,
            Change {
                view: v.into(),
                ops: vec![op],
                deps,
                provenance: Provenance {
                    actor: "human:qa".into(),
                    run: None,
                    role: Role::Writer,
                    at_ms: 5,
                },
                justification: j,
                message: String::new(),
                links: Default::default(),
                read_identity: None,
            },
        )
        .unwrap();
    r.invalidate();
    h
}

/// Rewrites `b` in `v` with `body`, justified by `j`. `links` replaces the named link kinds,
/// each target pinned at its content in `v`; an empty list removes that kind.
fn rewrite(
    r: &mut Rdy,
    v: &str,
    b: &BlockId,
    body: &str,
    j: Option<Justification>,
    links: &[(&str, &[&BlockId])],
    deps: Option<Vec<Hash>>,
) -> Hash {
    let st = r.read();
    let mut next = st.block(&view(v), b).unwrap();
    let base = next.version_hash();
    next.body = body.into();
    for (kind, targets) in links {
        if let Some(old) = next.links.remove(*kind) {
            for l in old {
                next.pins.remove(&l.target);
            }
        }
        if targets.is_empty() {
            continue;
        }
        let mut list = vec![];
        for t in *targets {
            let target = st.block(&view(v), t).unwrap();
            next.pins.insert(
                (*t).clone(),
                target.content_hash_with(st.reader().tracking()),
            );
            list.push(Link {
                target: (*t).clone(),
                part: None,
            });
        }
        next.links.insert((*kind).to_string(), list);
    }
    drop(st);
    commit(r, v, Op::Update { base, block: next }, j, deps)
}

fn reflects(sources: &[&BlockId], code: bool) -> Option<Justification> {
    Some(Justification::Reflects {
        sources: sources.iter().map(|s| (*s).clone()).collect(),
        code,
    })
}

fn editorial() -> Option<Justification> {
    Some(Justification::Editorial {
        class: EditorialClass::Clarify,
        reason: "clearer".into(),
    })
}

/// Canon: doc `spec` with the stable section `Rules` and its claims, and the draft doc `refs`
/// with section `Sources` and its sources s1 and s2.
struct Fx {
    claims: Vec<BlockId>,
    sources_sec: BlockId,
    s1: BlockId,
    s2: BlockId,
}

fn source_op(name: &str, title: &str, parent: &str) -> Value {
    json!({"op": "create", "type": "source", "id": name, "parent": parent,
        "fields": {"title": title, "kind": "release", "locator": "v1"},
        "body": "Retention is 30 days."})
}

fn fixture(r: &mut Rdy) -> Fx {
    let w = put(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$c1", "parent": "$s", "body": "One holds."},
            {"op": "create", "type": "claim", "id": "$c2", "parent": "$s", "body": "Two holds."},
            {"op": "create", "type": "claim", "id": "$c3", "parent": "$s", "body": "Three holds."},
            {"op": "create", "type": "claim", "id": "$c4", "parent": "$s", "body": "Four holds."},
            {"op": "create", "type": "claim", "id": "$c5", "parent": "$s", "body": "Five holds."},
            {"op": "create", "type": "doc", "id": "$r", "fields": {"name": "refs", "title": "Refs", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$rs", "parent": "$r", "fields": {"title": "Sources"}},
            source_op("$s1", "Release 1", "$rs"),
            source_op("$s2", "Release 2", "$rs"),
        ]),
        &[],
    );
    land(r, "seed");
    let sec = id(&w.created["$s"]);
    put(
        r,
        "st",
        json!([{"op": "update", "id": sec.to_string(), "fields": {"lifecycle": "stable"}}]),
        &[&sec],
    );
    land(r, "st");
    Fx {
        claims: ["$c1", "$c2", "$c3", "$c4", "$c5"]
            .iter()
            .map(|k| id(&w.created[*k]))
            .collect(),
        sources_sec: id(&w.created["$rs"]),
        s1: id(&w.created["$s1"]),
        s2: id(&w.created["$s2"]),
    }
}

fn project() -> (tempfile::TempDir, Rdy, Fx) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let fx = fixture(&mut r);
    (dir, r, fx)
}

/// The unjustified-change tensions of `ts`: (block, change sides), sorted.
fn unjustified<'a>(ts: impl IntoIterator<Item = &'a Arc<Tension>>) -> Vec<(BlockId, Vec<String>)> {
    let mut out: Vec<(BlockId, Vec<String>)> = ts
        .into_iter()
        .filter(|t| t.kind == TensionKind::UnjustifiedChange)
        .map(|t| {
            (
                t.blocks()[0].clone(),
                t.sides.iter().map(|s| s.label()).collect(),
            )
        })
        .collect();
    out.sort();
    out
}

fn inflight_unjustified(r: &Rdy) -> Vec<(BlockId, Vec<String>)> {
    unjustified(r.read().tensions().unwrap().iter())
}

fn net_unjustified(r: &Rdy, v: &str) -> Vec<(BlockId, Vec<String>)> {
    unjustified(r.read().view_net_tensions(v).unwrap().iter())
}

/// The warm set (built, or carried across the last step) holds exactly what a full build holds.
fn warm_matches_rebuilt(r: &Rdy) {
    let st = r.read();
    let warm: Vec<Hash> = st.tensions().unwrap().iter().map(|t| t.id).collect();
    let cold: Vec<Hash> = st
        .tensions_rebuilt()
        .unwrap()
        .iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(warm, cold, "warm set differs from a rebuild");
}

#[test]
fn rf10_source_presence_is_materialized_once_per_view_per_run() {
    let (_d, mut r, fx) = project();
    let c = &fx.claims;
    for (i, b) in c.iter().take(4).enumerate() {
        rewrite(
            &mut r,
            "a",
            b,
            &format!("Rule {i} per release 1."),
            reflects(&[&fx.s1], false),
            &[],
            None,
        );
    }
    rewrite(
        &mut r,
        "b",
        &c[0],
        "One per release 2.",
        reflects(&[&fx.s2], false),
        &[],
        None,
    );
    rewrite(
        &mut r,
        "b",
        &c[1],
        "Two per release 2.",
        reflects(&[&fx.s2, &fx.s1], true),
        &[],
        None,
    );
    // a view whose justifications cite no source materializes nothing
    rewrite(&mut r, "c", &c[4], "Five, clearer.", editorial(), &[], None);
    let st = r.read();
    let before = held_source_builds();
    let set = st.tensions_rebuilt().unwrap();
    assert_eq!(
        held_source_builds() - before,
        2,
        "one held-source set per view"
    );
    assert!(
        unjustified(set.iter()).is_empty(),
        "{:?}",
        unjustified(set.iter())
    );
    drop(st);
    // a new run after the source is gone builds the sets again, and judges with them
    put(
        &mut r,
        "a",
        json!([{"op": "delete", "id": fx.s1.to_string()}]),
        &[],
    );
    let st = r.read();
    let before = held_source_builds();
    let set = st.tensions_rebuilt().unwrap();
    assert_eq!(held_source_builds() - before, 2);
    let blocks: Vec<BlockId> = unjustified(set.iter())
        .into_iter()
        .map(|(b, _)| b)
        .collect();
    let mut want: Vec<BlockId> = c.iter().take(4).cloned().collect();
    want.sort();
    assert_eq!(blocks, want);
}

#[test]
fn rf10_reflects_raises_suspect_without_premise_tension() {
    let (_d, mut r, fx) = project();
    let (x, y, n) = (&fx.claims[0], &fx.claims[1], &fx.claims[2]);
    // a reflecting rewrite and a depends_on control, each its own change in view r
    let cx = rewrite(
        &mut r,
        "r",
        x,
        "One per release 1.",
        reflects(&[&fx.s1], false),
        &[("reflects", &[&fx.s1, n])],
        None,
    );
    let cy = rewrite(
        &mut r,
        "r",
        y,
        "Two per release 1.",
        editorial(),
        &[("depends_on", &[&fx.s1, n])],
        None,
    );
    // the source edit, in another view
    put(
        &mut r,
        "p",
        json!([{"op": "update", "id": fx.s1.to_string(), "body": "Retention is 7 days."}]),
        &[],
    );
    let st = r.read();
    let suspects: BTreeSet<(BlockId, BlockId, String)> = st
        .suspects(&Scope::Inflight)
        .unwrap()
        .into_iter()
        .map(|s| (s.block, s.target, s.reason))
        .collect();
    assert!(
        suspects.contains(&(x.clone(), fx.s1.clone(), "changed".into())),
        "{suspects:?}"
    );
    assert!(suspects.contains(&(y.clone(), fx.s1.clone(), "changed".into())));
    let set = st.tensions().unwrap();
    let shifts: Vec<Vec<BlockId>> = set
        .iter()
        .filter(|t| t.kind == TensionKind::PremiseShift)
        .map(|t| t.blocks())
        .collect();
    assert!(
        shifts.iter().all(|b| !b.contains(x)),
        "reflects is no premise: {shifts:?}"
    );
    assert!(
        shifts.iter().any(|b| b.contains(y) && b.contains(&fx.s1)),
        "depends_on is: {shifts:?}"
    );
    // the semantic candidates: no premise pair through reflects, one through depends_on
    let premise_pairs = |c: &Hash| -> Vec<Vec<BlockId>> {
        st.requests_from(c)
            .unwrap()
            .into_iter()
            .filter(|q| q.rule == Rule::Premise)
            .map(|q| q.items.into_iter().map(|i| i.block).collect())
            .collect()
    };
    assert!(premise_pairs(&cx).is_empty(), "{:?}", premise_pairs(&cx));
    assert!(
        premise_pairs(&cy)
            .iter()
            .any(|p| p.contains(y) && p.contains(n)),
        "{:?}",
        premise_pairs(&cy)
    );
    assert_ne!(st.pair_rule(x, n).unwrap(), Some(Rule::Premise));
    assert_eq!(st.pair_rule(y, n).unwrap(), Some(Rule::Premise));
}

#[test]
fn rf10_missing_source_is_judged_per_view() {
    let (_d, mut r, fx) = project();
    let x = &fx.claims[0];
    // view a creates s3; C1 reflects it (and code) but depends on nothing
    let w = put(
        &mut r,
        "a",
        json!([source_op("$s3", "Release 3", &fx.sources_sec.to_string())]),
        &[],
    );
    let s3 = id(&w.created["$s3"]);
    let c1 = rewrite(
        &mut r,
        "a",
        x,
        "One per release 3.",
        reflects(&[&s3], true),
        &[],
        Some(vec![]),
    );
    assert!(
        inflight_unjustified(&r).is_empty(),
        "{:?}",
        inflight_unjustified(&r)
    );
    // view b holds C1 alone: s3 is missing there, and code: true does not hide it
    r.view_include(&writer(), "b", &[c1.to_string()]).unwrap();
    assert_eq!(r.read().included("b").to_vec(), vec![c1]);
    let want = vec![(x.clone(), vec![c1.to_string()])];
    assert_eq!(inflight_unjustified(&r), want);
    assert!(net_unjustified(&r, "a").is_empty());
    assert_eq!(net_unjustified(&r, "b"), want);
    warm_matches_rebuilt(&r);
    // a reflects with no source counts only with code
    let c2 = rewrite(
        &mut r,
        "c",
        &fx.claims[1],
        "Two, as is.",
        reflects(&[], false),
        &[],
        None,
    );
    rewrite(
        &mut r,
        "d",
        &fx.claims[2],
        "Three, as coded.",
        reflects(&[], true),
        &[],
        None,
    );
    assert_eq!(
        net_unjustified(&r, "c"),
        vec![(fx.claims[1].clone(), vec![c2.to_string()])]
    );
    assert!(net_unjustified(&r, "d").is_empty());
    // a cited block that is no source does not count
    let c3 = rewrite(
        &mut r,
        "e",
        &fx.claims[3],
        "Four, as five.",
        reflects(&[&fx.claims[4]], false),
        &[],
        None,
    );
    assert_eq!(
        net_unjustified(&r, "e"),
        vec![(fx.claims[3].clone(), vec![c3.to_string()])]
    );
}

#[test]
fn rf10_net_justification_survives_an_unjustified_later_rewrite() {
    let (_d, mut r, fx) = project();
    let x = &fx.claims[0];
    rewrite(
        &mut r,
        "a",
        x,
        "One per release 1.",
        reflects(&[&fx.s1], false),
        &[],
        None,
    );
    let c2 = rewrite(&mut r, "a", x, "One, per release 1.", None, &[], None);
    assert!(net_unjustified(&r, "a").is_empty());
    assert!(inflight_unjustified(&r).is_empty());
    // the source goes: the earlier justification no longer holds
    put(
        &mut r,
        "a",
        json!([{"op": "delete", "id": fx.s1.to_string()}]),
        &[],
    );
    let want = vec![(x.clone(), vec![c2.to_string()])];
    assert_eq!(net_unjustified(&r, "a"), want);
    assert_eq!(inflight_unjustified(&r), want);
    warm_matches_rebuilt(&r);
    // restoring the source resolves it
    r.undo(&writer(), "a", None, false).unwrap();
    assert!(net_unjustified(&r, "a").is_empty());
    assert!(inflight_unjustified(&r).is_empty());
    warm_matches_rebuilt(&r);
    // so does another valid justification after the source goes again
    put(
        &mut r,
        "a",
        json!([{"op": "delete", "id": fx.s1.to_string()}]),
        &[],
    );
    assert_eq!(inflight_unjustified(&r), want);
    rewrite(&mut r, "a", x, "One, clearer.", editorial(), &[], None);
    assert!(net_unjustified(&r, "a").is_empty());
    assert!(inflight_unjustified(&r).is_empty());
    // a source valid in another view does not count in this one
    put(
        &mut r,
        "f",
        json!([source_op("$s9", "Release 9", &fx.sources_sec.to_string())]),
        &[],
    );
    let s9 = r
        .read()
        .materialized(&view("f"))
        .unwrap()
        .iter()
        .find(|b| b.field_str("title") == Some("Release 9"))
        .map(|b| b.id.clone())
        .unwrap();
    let c9 = rewrite(
        &mut r,
        "g",
        &fx.claims[1],
        "Two per release 9.",
        reflects(&[&s9], false),
        &[],
        None,
    );
    assert_eq!(
        inflight_unjustified(&r),
        vec![(fx.claims[1].clone(), vec![c9.to_string()])]
    );
}

#[test]
fn rf10_source_only_delta_matches_full_tension_rebuild() {
    let (_d, mut r, fx) = project();
    let c = &fx.claims;
    // view a: C0 creates s3; C1 reflects s3 (with the link); C2 rewrites with the link removed
    let w = put(
        &mut r,
        "a",
        json!([source_op("$s3", "Release 3", &fx.sources_sec.to_string())]),
        &[],
    );
    let (s3, c0) = (id(&w.created["$s3"]), w.change.clone());
    let c1 = rewrite(
        &mut r,
        "a",
        &c[0],
        "One per release 3.",
        reflects(&[&s3], false),
        &[("reflects", &[&s3])],
        Some(vec![]),
    );
    let c2 = rewrite(
        &mut r,
        "a",
        &c[0],
        "One, per release 3.",
        None,
        &[("reflects", &[])],
        Some(vec![c1]),
    );
    let st = r.read();
    let g = st.graph(&view("a")).unwrap();
    assert!(
        g.outgoing(&c[0]).iter().all(|e| e.to != s3),
        "the link is gone"
    );
    drop(st);
    assert!(inflight_unjustified(&r).is_empty());
    warm_matches_rebuilt(&r);
    let one = vec![(c[0].clone(), vec![c2.to_string()])];
    // exclude the source's change: no recipient block changes, the set still moves
    r.view_exclude(&writer(), "a", std::slice::from_ref(&c0), false)
        .unwrap();
    assert_eq!(inflight_unjustified(&r), one);
    assert_eq!(r.read().tensions_carried(), Some(true));
    warm_matches_rebuilt(&r);
    // include it again
    r.view_include(&writer(), "a", std::slice::from_ref(&c0))
        .unwrap();
    assert!(inflight_unjustified(&r).is_empty());
    assert_eq!(r.read().tensions_carried(), Some(true));
    warm_matches_rebuilt(&r);
    // undo the include
    r.undo(&writer(), "a", None, false).unwrap();
    assert_eq!(inflight_unjustified(&r), one);
    warm_matches_rebuilt(&r);
    r.view_include(&writer(), "a", std::slice::from_ref(&c0))
        .unwrap();
    assert!(inflight_unjustified(&r).is_empty());
    // retype: a change of view t makes s2 a claim; including it in view b, which reflects s2
    let cb = rewrite(
        &mut r,
        "b",
        &c[1],
        "Two per release 2.",
        reflects(&[&fx.s2], false),
        &[],
        None,
    );
    let mut retyped = r.read().block(&Scope::Canon, &fx.s2).unwrap();
    let base = retyped.version_hash();
    retyped.block_type = "claim".into();
    retyped.fields.clear();
    let rt = commit(
        &mut r,
        "t",
        Op::Update {
            base,
            block: retyped,
        },
        None,
        None,
    );
    r.read().tensions().unwrap();
    assert!(net_unjustified(&r, "b").is_empty());
    r.view_include(&writer(), "b", &[rt.to_string()]).unwrap();
    assert_eq!(
        net_unjustified(&r, "b"),
        vec![(c[1].clone(), vec![cb.to_string()])]
    );
    assert!(inflight_unjustified(&r).contains(&(c[1].clone(), vec![cb.to_string()])));
    assert_eq!(r.read().tensions_carried(), Some(true));
    warm_matches_rebuilt(&r);
    // delete: view b deletes s1, which its rewrite of claim 3 reflects
    let c3 = rewrite(
        &mut r,
        "b",
        &c[2],
        "Three per release 1.",
        reflects(&[&fx.s1], false),
        &[],
        None,
    );
    r.read().tensions().unwrap();
    put(
        &mut r,
        "b",
        json!([{"op": "delete", "id": fx.s1.to_string()}]),
        &[],
    );
    assert!(inflight_unjustified(&r).contains(&(c[2].clone(), vec![c3.to_string()])));
    warm_matches_rebuilt(&r);
    // canon landing: view l creates s4; view m reflects it, unjustified until l lands
    let w = put(
        &mut r,
        "l",
        json!([source_op("$s4", "Release 4", &fx.sources_sec.to_string())]),
        &[],
    );
    let s4 = id(&w.created["$s4"]);
    let c4 = rewrite(
        &mut r,
        "m",
        &c[3],
        "Four per release 4.",
        reflects(&[&s4], false),
        &[],
        None,
    );
    assert!(inflight_unjustified(&r).contains(&(c[3].clone(), vec![c4.to_string()])));
    land(&mut r, "l");
    assert!(
        !inflight_unjustified(&r).contains(&(c[3].clone(), vec![c4.to_string()])),
        "{:?}",
        inflight_unjustified(&r)
    );
    warm_matches_rebuilt(&r);
}
