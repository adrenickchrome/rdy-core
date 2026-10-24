//! rf10 (D-4WC6R, D-Z8P09, D-VPATZ, D-4XTRC, D-7M06Q, D-ATBP6): what a change to a source or a
//! finding leaves to do, and the follow-up an accepted decision gets by how the change reaches
//! it. The consequences and the change plan share one candidate visitor, so each pin reads both.
//! Explicit links and the normalized model `Reflects` are written as typed store commits (the
//! raw-commit fixture); no raw reflects input is involved.
mod common;

use rdy_engine::brief::FollowUp;
use rdy_engine::consequence::{
    ConsequenceKind, Consequences, LINEAGE_DECISION_FIX, LINEAGE_FIX, REACH_CONFIRM_FIX,
    REAFFIRM_FIX,
};
use rdy_engine::plan::{PlanAnswerInput, PlanQuery};
use rdy_engine::{
    Coverage, DecisionInput, Rdy, Scope, Session, WriteRequest, WriteResult, parse_ops,
};
use rdy_model::plan::{PlanAct, PlanKind, item_display, item_hash};
use rdy_model::{BlockId, Change, Hash, Justification, Link, Op, Provenance, Role};
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;

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

fn put(r: &mut Rdy, v: &str, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: v.into(),
            ops: parse_ops(ops.as_array().unwrap().clone()).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap_or_else(|e| panic!("write to {v}: {e}"))
}

fn land(r: &mut Rdy, v: &str) {
    let p = r
        .propose(&writer(), v, v, Some(common::TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {v}: {e}"));
    common::confirm_exceptions(r, &approver(), &p);
    r.land(&approver(), &p.id)
        .unwrap_or_else(|e| panic!("land {v}: {e}"));
}

fn decide(r: &mut Rdy, v: &str, d: DecisionInput) -> BlockId {
    let w = r
        .decide(&writer(), v, d)
        .unwrap_or_else(|e| panic!("decide in {v}: {e}"));
    id(&w.created["$decision"])
}

/// A typed store commit of `b` in `v`, rewritten by `edit`, justified by `j` (the raw-commit
/// fixture): every change `v` includes is a dep.
fn commit(
    r: &mut Rdy,
    v: &str,
    b: &BlockId,
    j: Option<Justification>,
    edit: impl FnOnce(&mut rdy_model::Block),
) -> Hash {
    let mut next = r.read().block(&view(v), b).unwrap();
    let base = next.version_hash();
    edit(&mut next);
    let deps = r.read().included(v).to_vec();
    let h = r
        .store_mut()
        .commit_to_view(
            v,
            Change {
                view: v.into(),
                ops: vec![Op::Update { base, block: next }],
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

/// Sets link kind `kind` of `b` in `v` to `targets`, each pinned at its content in `v` when
/// `pinned` (a typed store commit).
fn link(r: &mut Rdy, v: &str, b: &BlockId, kind: &str, targets: &[&BlockId], pinned: bool) {
    let st = r.read();
    let t = std::sync::Arc::clone(st.reader().tracking());
    let pins: Vec<(BlockId, Hash)> = targets
        .iter()
        .map(|x| {
            let blk = st.block(&view(v), x).unwrap();
            ((*x).clone(), blk.content_hash_with(&t))
        })
        .collect();
    drop(st);
    commit(r, v, b, None, |blk| {
        blk.links.insert(
            kind.to_string(),
            targets
                .iter()
                .map(|x| Link {
                    target: (*x).clone(),
                    part: None,
                })
                .collect(),
        );
        for (x, h) in pins {
            if pinned {
                blk.pins.insert(x, h);
            } else {
                blk.pins.remove(&x);
            }
        }
    });
}

fn change_of(w: &WriteResult) -> Hash {
    w.change.parse().unwrap()
}

fn consequences(r: &Rdy, v: &str, change: &Hash) -> Consequences {
    r.read()
        .consequences(&view(v), change, Some(100_000))
        .unwrap()
}

fn entry<'a>(c: &'a Consequences, b: &BlockId) -> Option<&'a rdy_engine::consequence::Consequence> {
    c.items.iter().find(|x| x.block == b.to_string())
}

fn whole(r: &Rdy, v: &str) -> Value {
    let p = r
        .read()
        .change_plan(
            &PlanQuery {
                view: Some(v.into()),
                max_tokens: Some(100_000),
                ..Default::default()
            },
            "work",
        )
        .unwrap();
    serde_json::to_value(p).unwrap()
}

fn pi(cause: &BlockId, subject: &str, kind: PlanKind) -> String {
    item_display(&item_hash(cause, subject, kind))
}

/// The plan rows of `v` as (kind, subject, cause, edge, state).
fn rows(v: &Value) -> Vec<(String, String, String, String, String)> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            let s = |k: &str| i[k].as_str().unwrap_or_default().to_string();
            (s("kind"), s("subject"), s("cause"), s("edge"), s("state"))
        })
        .collect()
}

fn row<'v>(v: &'v Value, item: &str) -> &'v Value {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == item)
        .unwrap_or_else(|| panic!("no item {item}: {v:#}"))
}

fn no_row_for(v: &Value, subject: &BlockId) {
    assert!(
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["subject"] != subject.to_string()),
        "{subject} has an item: {v:#}"
    );
}

/// Canon: draft doc `spec` with section `Rules` and claim t ("T holds 30 days."), and doc `refs`
/// with section `Sources`, source s1 ("Retention is 30 days.") and finding f1.
struct Fx {
    sec: BlockId,
    t: BlockId,
    s1: BlockId,
    f1: BlockId,
}

fn fixture(r: &mut Rdy) -> Fx {
    let w = put(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$t", "parent": "$s", "body": "T holds 30 days."},
            {"op": "create", "type": "doc", "id": "$r", "fields": {"name": "refs", "title": "Refs", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$rs", "parent": "$r", "fields": {"title": "Sources"}},
            {"op": "create", "type": "source", "id": "$s1", "parent": "$rs",
                "fields": {"title": "Release 1", "kind": "release", "locator": "v1"},
                "body": "Retention is 30 days."},
            {"op": "create", "type": "finding", "id": "$f1", "parent": "$rs",
                "fields": {"statement": "Retention is 30 days in production.",
                           "evidence": [{"quote": "We keep thirty days."}]}}
        ]),
    );
    land(r, "seed");
    let g = |k: &str| id(&w.created[k]);
    Fx {
        sec: g("$s"),
        t: g("$t"),
        s1: g("$s1"),
        f1: g("$f1"),
    }
}

fn project() -> (tempfile::TempDir, Rdy, Fx) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let fx = fixture(&mut r);
    (dir, r, fx)
}

fn claim(r: &mut Rdy, v: &str, parent: &BlockId, body: &str) -> BlockId {
    let w = put(
        r,
        v,
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": parent.to_string(), "body": body}]),
    );
    id(&w.created["$c"])
}

fn statement(s: &str, parent: &BlockId) -> DecisionInput {
    DecisionInput {
        parent: parent.to_string(),
        statement: s.into(),
        ..Default::default()
    }
}

fn update_t(r: &mut Rdy, v: &str, fx: &Fx) -> Hash {
    change_of(&put(
        r,
        v,
        json!([{"op": "update", "id": fx.t.to_string(), "body": "T holds 7 days."}]),
    ))
}

#[test]
fn rf10_source_and_finding_changes_have_direct_consequences() {
    let (_d, mut r, fx) = project();
    // canon: x and y reflect s1 (x states its value, y does not), z states it with no link;
    // dq depends on f1 (pinned); de affects entity en, which f1 describes
    let x = claim(&mut r, "k", &fx.sec, "Keep 30 days.");
    let y = claim(&mut r, "k", &fx.sec, "Keep it as released.");
    let z = claim(&mut r, "k", &fx.sec, "Keep 30 days anyway.");
    let w = put(
        &mut r,
        "k",
        json!([{"op": "create", "type": "entity", "id": "$e", "parent": fx.sec.to_string(), "fields": {"name": "Retention"}}]),
    );
    let en = id(&w.created["$e"]);
    link(&mut r, "k", &x, "reflects", &[&fx.s1], true);
    link(&mut r, "k", &y, "reflects", &[&fx.s1], true);
    link(&mut r, "k", &fx.f1, "describes", &[&en], false);
    let dq = decide(
        &mut r,
        "k",
        DecisionInput {
            depends_on: vec![fx.f1.to_string()],
            ..statement("Keep what production keeps", &fx.sec)
        },
    );
    let de = decide(
        &mut r,
        "k",
        DecisionInput {
            affects: vec![en.to_string()],
            ..statement("Retention is short", &fx.sec)
        },
    );
    land(&mut r, "k");

    // a source edit: each block that reflects it is suspect through reflects; the literal
    // matcher gives update to the one that states the removed value, confirm to the other
    let c = change_of(&put(
        &mut r,
        "w",
        json!([{"op": "update", "id": fx.s1.to_string(), "body": "Retention is 7 days."}]),
    ));
    let cq = consequences(&r, "w", &c);
    let ex = entry(&cq, &x).expect("x");
    assert_eq!(
        (ex.kind, ex.follow_up),
        (ConsequenceKind::Suspect, FollowUp::Update)
    );
    assert_eq!(ex.because.edge, "reflects");
    assert_eq!(ex.because.edges, Some(vec!["reflects".to_string()]));
    assert_eq!(ex.because.changed, fx.s1.to_string());
    assert_eq!(ex.because.values, Some(vec!["30 days".to_string()]));
    assert!(!ex.matched.is_empty());
    let ey = entry(&cq, &y).expect("y");
    assert_eq!(
        (ey.kind, ey.follow_up),
        (ConsequenceKind::Suspect, FollowUp::Confirm)
    );
    assert_eq!(ey.because.edge, "reflects");
    assert_eq!(ey.fix, None);
    assert!(entry(&cq, &z).is_none(), "no link, no entry: {cq:?}");
    assert_eq!(cq.items.len(), 2, "{cq:?}");
    let p = whole(&r, "w");
    let mut got = rows(&p);
    got.sort();
    let mut want = vec![
        (
            "update".into(),
            x.to_string(),
            fx.s1.to_string(),
            "reflects".into(),
            "open".into(),
        ),
        (
            "confirm".into(),
            y.to_string(),
            fx.s1.to_string(),
            "reflects".into(),
            "open".into(),
        ),
    ];
    want.sort();
    assert_eq!(got, want);

    // a finding edit reaches the decision that depends on it, by its status (accepted, and
    // suspect on it: reaffirm); the decision that affects an entity it describes is no route
    let c = change_of(&put(
        &mut r,
        "w2",
        json!([{"op": "update", "id": fx.f1.to_string(),
            "fields": {"statement": "Retention is 7 days in production."}}]),
    ));
    let cq = consequences(&r, "w2", &c);
    let eq = entry(&cq, &dq).expect("dq");
    assert_eq!(
        (eq.kind, eq.follow_up),
        (ConsequenceKind::Suspect, FollowUp::Reaffirm)
    );
    assert_eq!(eq.because.edge, "depends_on");
    assert_eq!(eq.fix.as_deref(), Some(REAFFIRM_FIX));
    assert!(entry(&cq, &de).is_none(), "{cq:?}");
    assert!(entry(&cq, &en).is_none(), "{cq:?}");
    let p = whole(&r, "w2");
    assert_eq!(
        rows(&p),
        vec![(
            "reaffirm".to_string(),
            dq.to_string(),
            fx.f1.to_string(),
            "depends_on".to_string(),
            "open".to_string()
        )]
    );

    // deleting the source: each reflecting block is told to update
    let c = change_of(&put(
        &mut r,
        "w3",
        json!([{"op": "delete", "id": fx.s1.to_string()}]),
    ));
    let cq = consequences(&r, "w3", &c);
    for b in [&x, &y] {
        let e = entry(&cq, b).expect("reflecting block");
        assert_eq!(
            (e.kind, e.follow_up),
            (ConsequenceKind::Suspect, FollowUp::Update)
        );
        assert_eq!(e.because.edge, "reflects");
    }
    let p = whole(&r, "w3");
    let mut got = rows(&p);
    got.sort();
    let mut want: Vec<_> = [&x, &y]
        .iter()
        .map(|b| {
            (
                "update".to_string(),
                b.to_string(),
                fx.s1.to_string(),
                "reflects".to_string(),
                "open".to_string(),
            )
        })
        .collect();
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn rf10_reflects_can_close_rejustify_without_retired_links() {
    let (_d, mut r, fx) = project();
    let d = decide(&mut r, "k", statement("Keep T", &fx.sec));
    let a = claim(&mut r, "k", &fx.sec, "A keeps T.");
    let b = claim(&mut r, "k", &fx.sec, "B keeps T.");
    link(&mut r, "k", &a, "decided_by", &[&d], true);
    link(&mut r, "k", &b, "decided_by", &[&d], true);
    land(&mut r, "k");
    r.supersede(
        &writer(),
        "w",
        &d.to_string(),
        None,
        statement("Drop T", &fx.sec),
    )
    .unwrap();
    let p = whole(&r, "w");
    for s in [&a, &b] {
        assert_eq!(
            row(&p, &pi(&d, &s.to_string(), PlanKind::Rejustify))["state"],
            "open"
        );
    }
    let reflects = || {
        Some(Justification::Reflects {
            sources: vec![fx.s1.clone()],
            code: false,
        })
    };
    // a: every link and reference to d removed, backed by the source: closed
    commit(&mut r, "w", &a, reflects(), |blk| {
        blk.body = "A follows release 1.".into();
        blk.links.remove("decided_by");
        blk.pins.remove(&d);
    });
    // b: backed by the source, but its text still names d: open
    commit(&mut r, "w", &b, reflects(), |blk| {
        blk.body = format!("B follows release 1, not {{{{{d}}}}}.");
        blk.links.remove("decided_by");
        blk.pins.remove(&d);
    });
    let p = whole(&r, "w");
    let ra = row(&p, &pi(&d, &a.to_string(), PlanKind::Rejustify));
    assert_eq!(ra["state"], "closed", "{p:#}");
    let rb = row(&p, &pi(&d, &b.to_string(), PlanKind::Rejustify));
    assert_eq!(rb["state"], "open", "{p:#}");
}

/// The PL 96 case (D-4XTRC), with its two neighbours.
#[test]
fn rf10_own_affects_link_reaches_no_decision() {
    let (_d, mut r, fx) = project();
    let d = decide(
        &mut r,
        "k",
        DecisionInput {
            affects: vec![fx.t.to_string()],
            ..statement("T is short", &fx.sec)
        },
    );
    let d2 = decide(
        &mut r,
        "k",
        DecisionInput {
            affects: vec![fx.t.to_string()],
            depends_on: vec![fx.t.to_string()],
            ..statement("T stays as stated", &fx.sec)
        },
    );
    land(&mut r, "k");
    let c = update_t(&mut r, "w", &fx);
    let cq = consequences(&r, "w", &c);
    assert!(entry(&cq, &d).is_none(), "own affects: {cq:?}");
    let e2 = entry(&cq, &d2).expect("d2");
    assert_eq!(
        (e2.kind, e2.follow_up),
        (ConsequenceKind::Suspect, FollowUp::Reaffirm)
    );
    assert_eq!(e2.fix.as_deref(), Some(REAFFIRM_FIX));
    assert_eq!(e2.because.edge, "depends_on");
    assert_eq!(
        e2.because.edges,
        Some(vec!["affects".to_string(), "depends_on".to_string()])
    );
    let p = whole(&r, "w");
    no_row_for(&p, &d);
    let item = pi(&fx.t, &d2.to_string(), PlanKind::Reaffirm);
    assert_eq!(row(&p, &item)["state"], "open");
    // a valid reaffirm closes it
    put(
        &mut r,
        "w",
        json!([{"op": "reaffirm", "id": d2.to_string(), "targets": [fx.t.to_string()], "reason": "Still holds."}]),
    );
    let p = whole(&r, "w");
    assert_eq!(row(&p, &item)["state"], "closed", "{p:#}");
    no_row_for(&p, &d);

    // a decision that refines d still reaches t
    let w = r
        .decide(
            &writer(),
            "r",
            DecisionInput {
                refines: vec![d.to_string()],
                ..statement("T is short, in days", &fx.sec)
            },
        )
        .unwrap();
    let cq = consequences(&r, "r", &change_of(&w));
    let et = entry(&cq, &fx.t).expect("t");
    assert_eq!(
        (et.kind, et.follow_up),
        (ConsequenceKind::Dependent, FollowUp::Confirm)
    );
    assert_eq!(et.because.edge, "refines");
    assert_eq!(et.because.changed, d.to_string());
    assert_eq!(et.fix.as_deref(), Some(LINEAGE_FIX));
    let p = whole(&r, "r");
    let item = pi(&d, &fx.t.to_string(), PlanKind::Confirm);
    assert_eq!(row(&p, &item)["edge"], "refines");

    // the briefing of t is as it was: d is a dependent, and will not go stale
    let b = r
        .read()
        .brief(
            &Scope::Canon,
            &Coverage {
                ids: vec![fx.t.to_string()],
                path: None,
            },
            None,
        )
        .unwrap();
    assert!(
        b.dependents.iter().any(|x| x.block == d.to_string()),
        "{:?}",
        b.dependents
    );
    assert!(
        b.will_go_stale.iter().all(|x| x.block != d.to_string()),
        "{:?}",
        b.will_go_stale
    );
}

/// An accepted `dg` that `d2` affects, a claim `n` decided by `d2`, a proposed decision that
/// `d2` governs and a fully superseded one: refining `d2` reaches each.
#[test]
fn rf10_refine_reach_gives_an_accepted_decision_confirm() {
    let (_d, mut r, fx) = project();
    let dg = decide(&mut r, "k", statement("Keep T", &fx.sec));
    let ds = decide(&mut r, "k", statement("Keep U", &fx.sec));
    let d2 = decide(
        &mut r,
        "k",
        DecisionInput {
            affects: vec![dg.to_string(), ds.to_string()],
            ..statement("T and U stay", &fx.sec)
        },
    );
    let n = claim(&mut r, "k", &fx.sec, "N follows D2.");
    link(&mut r, "k", &n, "decided_by", &[&d2], true);
    land(&mut r, "k");
    r.supersede(
        &writer(),
        "x",
        &ds.to_string(),
        None,
        statement("Drop U", &fx.sec),
    )
    .unwrap();
    land(&mut r, "x");
    // in view w: a proposed decision d2 governs, then the refine
    let pp = decide(&mut r, "w", statement("V is new", &fx.sec));
    link(&mut r, "w", &pp, "decided_by", &[&d2], true);
    let w = r
        .decide(
            &writer(),
            "w",
            DecisionInput {
                refines: vec![d2.to_string()],
                ..statement("T and U stay, in days", &fx.sec)
            },
        )
        .unwrap();
    let cq = consequences(&r, "w", &change_of(&w));
    let eg = entry(&cq, &dg).expect("dg");
    assert_eq!(
        (eg.kind, eg.follow_up),
        (ConsequenceKind::Dependent, FollowUp::Confirm)
    );
    assert_eq!(eg.fix.as_deref(), Some(LINEAGE_DECISION_FIX));
    assert_eq!(eg.because.edge, "refines");
    assert_eq!(eg.because.changed, d2.to_string());
    let en = entry(&cq, &n).expect("n");
    assert_eq!(en.follow_up, FollowUp::Confirm);
    assert_eq!(en.fix.as_deref(), Some(LINEAGE_FIX));
    let ep = entry(&cq, &pp).expect("pp");
    assert_eq!(ep.follow_up, FollowUp::Update);
    assert!(entry(&cq, &ds).is_none(), "{cq:?}");
    let p = whole(&r, "w");
    let item = pi(&d2, &dg.to_string(), PlanKind::Confirm);
    assert_eq!(row(&p, &item)["state"], "open");
    assert_eq!(row(&p, &item)["edge"], "refines");
    assert!(
        rows(&p)
            .iter()
            .all(|(k, s, ..)| !(k == "reaffirm" && *s == dg.to_string())),
        "{p:#}"
    );
    no_row_for(&p, &ds);
    // it stays open until a confirm answer records its reason
    r.answer_plan(
        &writer(),
        "w",
        &[PlanAnswerInput {
            item: item.clone(),
            act: PlanAct::Confirm,
            reason: Some("Keep T still holds in days.".into()),
        }],
    )
    .unwrap();
    assert_eq!(row(&whole(&r, "w"), &item)["state"], "closed");
}

#[test]
fn rf10_describes_reach_gives_an_accepted_decision_confirm() {
    let (_d, mut r, fx) = project();
    let dd = decide(&mut r, "k", statement("T is read daily", &fx.sec));
    link(&mut r, "k", &dd, "describes", &[&fx.t], true);
    land(&mut r, "k");
    let c = update_t(&mut r, "w", &fx);
    let cq = consequences(&r, "w", &c);
    let e = entry(&cq, &dd).expect("dd");
    assert_eq!(
        (e.kind, e.follow_up),
        (ConsequenceKind::Dependent, FollowUp::Confirm)
    );
    assert_eq!(e.fix.as_deref(), Some(REACH_CONFIRM_FIX));
    assert_eq!(e.because.edge, "describes");
    let p = whole(&r, "w");
    assert_eq!(
        row(&p, &pi(&fx.t, &dd.to_string(), PlanKind::Confirm))["state"],
        "open"
    );
    no_reaffirm(&p);
}

#[test]
fn rf10_unpinned_premise_reach_gives_confirm() {
    let (_d, mut r, fx) = project();
    let du = decide(&mut r, "k", statement("T is the base", &fx.sec));
    link(&mut r, "k", &du, "depends_on", &[&fx.t], false);
    land(&mut r, "k");
    let c = update_t(&mut r, "w", &fx);
    let cq = consequences(&r, "w", &c);
    let e = entry(&cq, &du).expect("du");
    assert_eq!(
        (e.kind, e.follow_up),
        (ConsequenceKind::Dependent, FollowUp::Confirm)
    );
    assert_eq!(e.fix.as_deref(), Some(REACH_CONFIRM_FIX));
    assert_eq!(e.because.edge, "depends_on");
    let p = whole(&r, "w");
    assert_eq!(
        row(&p, &pi(&fx.t, &du.to_string(), PlanKind::Confirm))["state"],
        "open"
    );
    no_reaffirm(&p);
}

fn no_reaffirm(p: &Value) {
    assert!(rows(p).iter().all(|(k, ..)| k != "reaffirm"), "{p:#}");
}

/// D-ATBP6: in one version with a suspect, a refine, a describes, an unpinned premise and an own
/// affects reach, every reaffirm item is one the reaffirm op accepts, and only the suspect gives
/// one.
#[test]
fn rf10_every_reaffirm_item_has_a_suspect() {
    let (_d, mut r, fx) = project();
    let s = decide(
        &mut r,
        "k",
        DecisionInput {
            depends_on: vec![fx.t.to_string()],
            ..statement("T stays", &fx.sec)
        },
    );
    let dd = decide(&mut r, "k", statement("T is read daily", &fx.sec));
    let du = decide(&mut r, "k", statement("T is the base", &fx.sec));
    let da = decide(
        &mut r,
        "k",
        DecisionInput {
            affects: vec![fx.t.to_string()],
            ..statement("T is short", &fx.sec)
        },
    );
    let dh = decide(&mut r, "k", statement("H holds", &fx.sec));
    let dg = decide(
        &mut r,
        "k",
        DecisionInput {
            affects: vec![dh.to_string()],
            ..statement("H is governed", &fx.sec)
        },
    );
    link(&mut r, "k", &dd, "describes", &[&fx.t], true);
    link(&mut r, "k", &du, "depends_on", &[&fx.t], false);
    land(&mut r, "k");
    let c = update_t(&mut r, "w", &fx);
    let refine = r
        .decide(
            &writer(),
            "w",
            DecisionInput {
                refines: vec![dg.to_string()],
                ..statement("H is governed, in days", &fx.sec)
            },
        )
        .unwrap();
    // the consequences: reaffirm only for the suspect
    let mut follow = vec![];
    for ch in [c, change_of(&refine)] {
        for e in consequences(&r, "w", &ch).items {
            follow.push((e.block.clone(), e.follow_up));
        }
    }
    follow.sort_by(|a, b| a.0.cmp(&b.0));
    let reaffirms: Vec<&String> = follow
        .iter()
        .filter(|(_, f)| *f == FollowUp::Reaffirm)
        .map(|(b, _)| b)
        .collect();
    assert_eq!(reaffirms, vec![&s.to_string()], "{follow:?}");
    assert!(
        follow.iter().all(|(b, _)| *b != da.to_string()),
        "{follow:?}"
    );
    for b in [&dd, &du, &dh] {
        assert!(
            follow.contains(&(b.to_string(), FollowUp::Confirm)),
            "{b}: {follow:?}"
        );
    }
    // the plan: one reaffirm item, on the suspect; the other reaches are confirm items
    let p = whole(&r, "w");
    let reaffirm_rows: Vec<_> = rows(&p)
        .into_iter()
        .filter(|(k, ..)| k == "reaffirm")
        .collect();
    assert_eq!(reaffirm_rows.len(), 1, "{p:#}");
    no_row_for(&p, &da);
    for (b, cause) in [(&dd, &fx.t), (&du, &fx.t), (&dh, &dg)] {
        assert_eq!(
            row(&p, &pi(cause, &b.to_string(), PlanKind::Confirm))["state"],
            "open"
        );
    }
    // every reaffirm item is accepted by the reaffirm op, which closes it
    for (_, subject, cause, ..) in &reaffirm_rows {
        put(
            &mut r,
            "w",
            json!([{"op": "reaffirm", "id": subject, "targets": [cause], "reason": "Still holds."}]),
        );
        let item = pi(&id(cause), subject, PlanKind::Reaffirm);
        assert_eq!(row(&whole(&r, "w"), &item)["state"], "closed");
    }
}

/// A pack whose `aside` type is presentational (a section may hold one).
const PRESENTATIONAL_PACK: &str = "name: p\nversion: '1'\nextends: rdy-standard\nblock_types:\n  aside: { prefix: as, traits: [presentational], body: required }\n  section:\n    prefix: s\n    traits: [structural]\n    children: [section, prose, claim, requirement, constraint, term, component, actor, interface, flow, example, note, rationale, table, diagram, figure, code, decision, question, aside]\n";

/// Fix round 1 (D-ATBP6): a pinned `depends_on` to a presentational block is no suspect, so its
/// change gives an accepted decision a confirm item that agrees with the consequences, never a
/// reaffirm item the reaffirm op refuses.
#[test]
fn rf10_presentational_cause_gives_a_confirm_item_not_reaffirm() {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(dir.path().join(".rdy/packs/p.yaml"), PRESENTATIONAL_PACK).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: p\n").unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "t", "title": "T", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
            {"op": "create", "type": "aside", "id": "$n", "parent": "$s", "body": "Decorative."}
        ]),
    );
    land(&mut r, "seed");
    let (sec, n) = (id(&w.created["$s"]), id(&w.created["$n"]));
    let d = decide(
        &mut r,
        "k",
        DecisionInput {
            depends_on: vec![n.to_string()],
            ..statement("Asides stay", &sec)
        },
    );
    land(&mut r, "k");
    let c = change_of(&put(
        &mut r,
        "w",
        json!([{"op": "update", "id": n.to_string(), "body": "Still decorative."}]),
    ));
    // the live suspect list holds no suspect for d
    assert!(
        r.read()
            .suspects(&view("w"))
            .unwrap()
            .iter()
            .all(|s| s.block != d),
        "{:?}",
        r.read().suspects(&view("w")).unwrap()
    );
    // the consequences: confirm
    let cq = consequences(&r, "w", &c);
    let e = entry(&cq, &d).expect("d");
    assert_eq!(e.follow_up, FollowUp::Confirm, "{cq:?}");
    assert_eq!(e.fix.as_deref(), Some(REACH_CONFIRM_FIX));
    // the plan agrees: a confirm item, and no reaffirm item
    let p = whole(&r, "w");
    no_reaffirm(&p);
    let item = pi(&n, &d.to_string(), PlanKind::Confirm);
    assert_eq!(row(&p, &item)["state"], "open", "{p:#}");
    // a confirm answer closes it
    r.answer_plan(
        &writer(),
        "w",
        &[PlanAnswerInput {
            item: item.clone(),
            act: PlanAct::Confirm,
            reason: Some("Asides stay; the edit is decorative.".into()),
        }],
    )
    .unwrap();
    assert_eq!(row(&whole(&r, "w"), &item)["state"], "closed");
}

// ---- code ----

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=qa",
            "-c",
            "user.email=qa",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn git_commit(dir: &Path, path: &str, text: &str, message: &str) {
    std::fs::write(dir.join(path), text).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--no-verify", "-m", message]);
}

/// A fresh checkout linked to the project at `project` as code repo `app`.
fn linked_repo(project: &Path) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "--initial-branch=main"]);
    let cfg = project.join(".rdy.yaml");
    let mut text = std::fs::read_to_string(&cfg).unwrap_or_default();
    text.push_str("code_repos: [app]\n");
    std::fs::write(&cfg, text).unwrap();
    std::fs::write(
        project.join(".rdy/local.yaml"),
        format!(
            "code_paths:\n  app: {}\n",
            serde_json::to_string(&tmp.path().display().to_string()).unwrap()
        ),
    )
    .unwrap();
    tmp
}

#[test]
fn rf10_code_suspects_and_unanchored_findings_add_no_implementation_items() {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let repo = linked_repo(dir.path());
    git_commit(repo.path(), "f.rs", "fn a() {}\n", "start");
    let mut r = Rdy::open(dir.path()).unwrap();
    let fx = fixture(&mut r);
    // k follows f.rs: its code state is recorded, then the code moves (a code suspect)
    let w = put(
        &mut r,
        "k",
        json!([{"op": "create", "type": "claim", "id": "$k", "parent": fx.sec.to_string(),
            "body": "K follows the code.", "fields": {"reflects": ["f.rs"]}}]),
    );
    let k = id(&w.created["$k"]);
    put(
        &mut r,
        "k",
        json!([{"op": "repin", "id": k.to_string(), "code": true}]),
    );
    land(&mut r, "k");
    git_commit(repo.path(), "f.rs", "fn a() { b() }\n", "move the code");
    let code = rdy_engine::reflects::code_suspects(&r.read(), &Scope::Canon, None).unwrap();
    assert!(
        code.iter().any(|s| s.block == k),
        "a code suspect on k: {code:?}"
    );
    // the finding and the source change; nothing cites either in code
    put(
        &mut r,
        "w",
        json!([
            {"op": "update", "id": fx.f1.to_string(), "fields": {"statement": "Retention is 7 days in production."}},
            {"op": "update", "id": fx.s1.to_string(), "body": "Retention is 7 days."}
        ]),
    );
    let p = whole(&r, "w");
    assert!(
        rows(&p)
            .iter()
            .all(|(kind, ..)| kind != "update_implementation"),
        "{p:#}"
    );
    no_row_for(&p, &k);
    assert_eq!(p["code"]["files"], 0, "{p:#}");
    assert_eq!(p["code"].get("unavailable"), None, "{p:#}");
    // a code citation the trailer API already accepts still gives its item
    git_commit(
        repo.path(),
        "g.rs",
        "fn g() {}\n",
        &format!("cite the source\n\nImplements: {}", fx.s1),
    );
    let p = whole(&r, "w");
    let item = pi(&fx.s1, "app:g.rs", PlanKind::UpdateImplementation);
    assert_eq!(row(&p, &item)["state"], "open", "{p:#}");
}
