//! Grouped plan answers and the bulk repin (D-MPJM0, D-ME2HD, O13, O14): a group expands to one
//! answer per open item it matches, in one plan version; a group that matches nothing or holds
//! an item the act does not answer is refused whole; `repin: true` on a confirm group writes
//! one change of repin ops, then the answers.
mod common;

use rdy_engine::plan::api::{EditorialArg, PlanAnswerSpec};
use rdy_engine::plan::group::PlanGroup;
use rdy_engine::plan::{PlanAnswerInput, PlanQuery};
use rdy_engine::{
    DecisionInput, E_BAD_INPUT, E_PLAN_ITEM, ProposalInfo, Rdy, RdyError, Scope, Session,
    WriteRequest, WriteResult, parse_ops,
};
use rdy_model::plan::{Plan, PlanAct, PlanKind, item_display, item_hash};
use rdy_model::{BlockId, EditorialClass, Hash, Justification, Role};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn writer() -> Session {
    Session::human("human:qa".into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human("human:qa".into(), Role::Approver).unwrap()
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn put(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: parse_ops(ops.as_array().unwrap().clone()).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap_or_else(|e| panic!("write to {view}: {e}"))
}

fn land(r: &mut Rdy, view: &str) -> ProposalInfo {
    let p = r
        .propose(&writer(), view, view, Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &approver(), &p);
    r.land(&approver(), &p.id).unwrap();
    p
}

/// Canon: doc `spec` with section `Rules` and claims a ("A holds 30 days."), c, b and p, which
/// cite a (b also c), and decision d1 with claim q decided by it; doc `other` with claim e,
/// which cites a. The view `view` then edits a and supersedes d1, so it owes a confirm item
/// each for b and e, an update item for p and a rejustify item for q.
struct World {
    doc: String,
    other: String,
    a: String,
    b: String,
    e: String,
    p: String,
    q: String,
    d1: String,
}

fn world(r: &mut Rdy, view: &str) -> World {
    let w = put(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C stands."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}} and {{$c}}."},
            {"op": "create", "type": "claim", "id": "$p", "parent": "$s", "body": "Wait 30 days per {{$a}}."},
            {"op": "create", "type": "doc", "id": "$d2", "fields": {"name": "other", "title": "Other", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s2", "parent": "$d2", "fields": {"title": "Notes"}}
        ]),
    );
    land(r, "seed");
    let get = |k: &str| w.created[k].clone();
    let (sec, a, d2s) = (get("$s"), get("$a"), get("$s2"));
    let d1 = r
        .decide(
            &writer(),
            "seed2",
            DecisionInput {
                parent: sec.clone(),
                statement: "Keep 30 days".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let e_body = format!("E relies on {{{{{a}}}}}.");
    let w2 = put(
        r,
        "seed2",
        json!([
            {"op": "create", "type": "claim", "id": "$q", "parent": sec, "body": "Teams follow it.",
             "links": {"decided_by": [{"target": d1}]}},
            {"op": "create", "type": "claim", "id": "$e", "parent": d2s, "body": e_body}
        ]),
    );
    land(r, "seed2");
    put(
        r,
        view,
        json!([{"op": "update", "id": a, "body": "A holds 60 days."}]),
    );
    r.supersede(
        &writer(),
        view,
        &d1,
        None,
        DecisionInput {
            parent: sec,
            statement: "Keep 60 days".into(),
            ..Default::default()
        },
    )
    .unwrap();
    World {
        doc: get("$d"),
        other: get("$d2"),
        a,
        b: get("$b"),
        e: w2.created["$e"].clone(),
        p: get("$p"),
        q: w2.created["$q"].clone(),
        d1,
    }
}

/// Canon: doc `spec` with section `Rules` and claims a, c, x, and b which cites a and c; an
/// accepted decision dx that affects x, and an accepted decision dz that decides claim z. The
/// view `view` edits a and c, supersedes dx in full, and adds a decision dr that refines dz. So
/// it owes confirm items: b for a and for c (two causes of one block, each stale); x for dx (a
/// reach through `affects`, which x does not cite); z for dz (a reach through `refines`, which z
/// cites with a pin that is current).
struct RepinWorld {
    a: String,
    c: String,
    b: String,
    x: String,
    z: String,
    dx: String,
    dz: String,
}

fn repin_world(r: &mut Rdy, view: &str) -> RepinWorld {
    let w = put(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C stands."},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "X stands."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}} and {{$c}}."}
        ]),
    );
    let get = |k: &str| w.created[k].clone();
    let (sec, a, c, x) = (get("$s"), get("$a"), get("$c"), get("$x"));
    let dx = r
        .decide(
            &writer(),
            "seed",
            DecisionInput {
                parent: sec.clone(),
                statement: "Keep X".into(),
                affects: vec![x.clone()],
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let dz = r
        .decide(
            &writer(),
            "seed",
            DecisionInput {
                parent: sec.clone(),
                statement: "Keep Z".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(r, "seed");
    let z = put(
        r,
        "seed2",
        json!([{"op": "create", "type": "claim", "id": "$z", "parent": sec, "body": "Z follows.",
                "links": {"decided_by": [{"target": dz}]}}]),
    )
    .created["$z"]
        .clone();
    land(r, "seed2");
    put(
        r,
        view,
        json!([
            {"op": "update", "id": a, "body": "A holds 60 days."},
            {"op": "update", "id": c, "body": "C stands firm."}
        ]),
    );
    r.supersede(
        &writer(),
        view,
        &dx,
        None,
        DecisionInput {
            parent: sec.clone(),
            statement: "Keep X, differently".into(),
            ..Default::default()
        },
    )
    .unwrap();
    r.decide(
        &writer(),
        view,
        DecisionInput {
            parent: sec,
            statement: "Keep Z in short".into(),
            refines: vec![dz.clone()],
            ..Default::default()
        },
    )
    .unwrap();
    RepinWorld {
        a,
        c,
        b: get("$b"),
        x,
        z,
        dx,
        dz,
    }
}

fn pi(cause: &str, subject: &str, kind: PlanKind) -> String {
    item_display(&item_hash(&id(cause), subject, kind))
}

fn group(kind: Option<PlanKind>, follow_up: Option<&str>) -> PlanGroup {
    PlanGroup {
        kind,
        follow_up: follow_up.map(str::to_string),
        ..Default::default()
    }
}

fn spec_of(g: PlanGroup, act: PlanAct, reason: Option<&str>, repin: bool) -> PlanAnswerSpec {
    PlanAnswerSpec {
        item: None,
        group: Some(g),
        act,
        reason: reason.map(str::to_string),
        repin,
        implements: None,
        editorial: None,
        reflects: None,
    }
}

fn item_spec(item: &str, act: PlanAct, reason: Option<&str>) -> PlanAnswerSpec {
    PlanAnswerSpec {
        item: Some(item.into()),
        group: None,
        act,
        reason: reason.map(str::to_string),
        repin: false,
        implements: None,
        editorial: None,
        reflects: None,
    }
}

fn plan_ref(r: &Rdy, view: &str) -> Option<Hash> {
    r.read()
        .reader()
        .get_ref(&rdy_store::plan::plan_ref(view))
        .unwrap()
        .map(|v| Hash(v.as_slice().try_into().unwrap()))
}

fn plan_of(r: &Rdy, h: &Hash) -> Plan {
    r.read().reader().plan(h).unwrap()
}

fn state_of(r: &Rdy, view: &str, item: &str) -> Value {
    let v = serde_json::to_value(
        r.read()
            .change_plan(
                &PlanQuery {
                    view: Some(view.into()),
                    max_tokens: Some(100_000),
                    ..Default::default()
                },
                "work",
            )
            .unwrap(),
    )
    .unwrap();
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == item)
        .unwrap_or_else(|| panic!("no item {item}: {v:#}"))
        .clone()
}

fn log_state(r: &Rdy) -> (rdy_engine::Location, Vec<(String, Vec<u8>)>) {
    (
        r.store().log_end(),
        r.read().reader().refs_with_prefix("").unwrap(),
    )
}

fn expect_err(e: RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e}"
    );
}

/// O13: a group answers every open item it matches, one answer each, in one plan version.
#[test]
fn rg9_one_group_answer_covers_every_matched_item() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = world(&mut r, "w");
    assert_eq!(plan_ref(&r, "w"), None);
    let h = r
        .answer_plan_specs(
            &writer(),
            "w",
            &[spec_of(
                group(Some(PlanKind::Confirm), None),
                PlanAct::Defer,
                Some("later"),
                false,
            )],
        )
        .unwrap();
    assert_eq!(plan_ref(&r, "w"), Some(h));
    let plan = plan_of(&r, &h);
    let mut got: Vec<(Hash, PlanAct, Option<String>)> = plan
        .answers
        .iter()
        .map(|a| (a.item, a.act, a.reason.clone()))
        .collect();
    let mut want: Vec<(Hash, PlanAct, Option<String>)> = [&w.b, &w.e]
        .iter()
        .map(|s| {
            (
                item_hash(&id(&w.a), s, PlanKind::Confirm),
                PlanAct::Defer,
                Some("later".to_string()),
            )
        })
        .collect();
    got.sort_by_key(|x| x.0);
    want.sort_by_key(|x| x.0);
    assert_eq!(got, want);
    for s in [&w.b, &w.e] {
        let row = state_of(&r, "w", &pi(&w.a, s, PlanKind::Confirm));
        assert_eq!(row["state"], "deferred", "{row}");
        assert_eq!(row["answer"]["reason"], "later");
    }
    // the update and rejustify items are not in the group
    for (cause, subject, kind) in [
        (&w.a, &w.p, PlanKind::Update),
        (&w.d1, &w.q, PlanKind::Rejustify),
    ] {
        assert_eq!(
            state_of(&r, "w", &pi(cause, subject, kind))["state"],
            "open"
        );
    }
}

/// O13: the fields of a group AND together, and a group matches open items only.
#[test]
fn rg9_group_fields_and_together() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = world(&mut r, "w");
    let confirm_b = pi(&w.a, &w.b, PlanKind::Confirm);
    let confirm_e = pi(&w.a, &w.e, PlanKind::Confirm);
    let none = |r: &mut Rdy, g: PlanGroup| {
        r.answer_plan_specs(
            &writer(),
            "w",
            &[spec_of(g, PlanAct::Defer, Some("x"), false)],
        )
        .unwrap_err()
    };
    let no_match = |e: RdyError| {
        expect_err(
            e,
            E_PLAN_ITEM,
            "the group matches no open item of the change plan for view:w",
            "read the plan with rdy_plan / rdy plan and pass a group it holds",
        )
    };
    // each field alone holds items; together with a field that disagrees, they match none
    no_match(none(
        &mut r,
        PlanGroup {
            kind: Some(PlanKind::Confirm),
            doc: Some(w.doc.clone()),
            edge: Some("depends".into()),
            ..Default::default()
        },
    ));
    no_match(none(
        &mut r,
        PlanGroup {
            kind: Some(PlanKind::Update),
            doc: Some(w.other.clone()),
            ..Default::default()
        },
    ));
    // kind and doc: the confirm item of the other doc only
    let h = r
        .answer_plan_specs(
            &writer(),
            "w",
            &[spec_of(
                PlanGroup {
                    kind: Some(PlanKind::Confirm),
                    doc: Some(w.other.clone()),
                    edge: Some("references".into()),
                    ..Default::default()
                },
                PlanAct::Defer,
                Some("other doc"),
                false,
            )],
        )
        .unwrap();
    let items: Vec<Hash> = plan_of(&r, &h).answers.iter().map(|a| a.item).collect();
    assert_eq!(items, [item_hash(&id(&w.a), &w.e, PlanKind::Confirm)]);
    assert_eq!(state_of(&r, "w", &confirm_e)["state"], "deferred");
    assert_eq!(state_of(&r, "w", &confirm_b)["state"], "open");
    // the answered item is not open: the same group matches none now
    no_match(none(
        &mut r,
        PlanGroup {
            kind: Some(PlanKind::Confirm),
            doc: Some(w.other.clone()),
            ..Default::default()
        },
    ));
    // the spec doc, alone, holds the update, the confirm and no rejustify item of the other doc
    let h = r
        .answer_plan_specs(
            &writer(),
            "w",
            &[spec_of(
                PlanGroup {
                    doc: Some(w.doc.clone()),
                    ..Default::default()
                },
                PlanAct::Defer,
                Some("spec doc"),
                false,
            )],
        )
        .unwrap();
    let plan = plan_of(&r, &h);
    let mut items: Vec<Hash> = plan.answers.iter().map(|a| a.item).collect();
    let mut want = vec![
        item_hash(&id(&w.a), &w.e, PlanKind::Confirm),
        item_hash(&id(&w.a), &w.b, PlanKind::Confirm),
        item_hash(&id(&w.a), &w.p, PlanKind::Update),
        item_hash(&id(&w.d1), &w.q, PlanKind::Rejustify),
    ];
    items.sort();
    want.sort();
    assert_eq!(items, want);
}

/// O13: a group that matches nothing, a group with no field and an unknown follow_up are
/// refused, with nothing written.
#[test]
fn rg9_a_group_that_matches_nothing_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    world(&mut r, "w");
    let before = log_state(&r);
    expect_err(
        r.answer_plan_specs(
            &writer(),
            "w",
            &[spec_of(
                group(Some(PlanKind::UpdateImplementation), None),
                PlanAct::Done,
                None,
                false,
            )],
        )
        .unwrap_err(),
        E_PLAN_ITEM,
        "the group matches no open item of the change plan for view:w",
        "read the plan with rdy_plan / rdy plan and pass a group it holds",
    );
    assert_eq!(log_state(&r), before);
    // no field, and a follow_up that is none of the three
    expect_err(
        r.answer_plan_specs(
            &writer(),
            "w",
            &[spec_of(
                PlanGroup::default(),
                PlanAct::Defer,
                Some("x"),
                false,
            )],
        )
        .unwrap_err(),
        E_BAD_INPUT,
        "a group names at least one of kind, follow_up, doc and edge",
        "pass one of kind, follow_up, doc and edge",
    );
    expect_err(
        r.answer_plan_specs(
            &writer(),
            "w",
            &[spec_of(
                group(None, Some("fix")),
                PlanAct::Defer,
                Some("x"),
                false,
            )],
        )
        .unwrap_err(),
        E_BAD_INPUT,
        "group follow_up fix is not update, reaffirm or confirm",
        "pass update, reaffirm or confirm",
    );
    assert_eq!(log_state(&r), before);
}

/// O13: one item the act does not answer refuses the whole group; no answer is written.
#[test]
fn rg9_a_group_with_an_act_some_items_refuse_is_refused_whole() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = world(&mut r, "w");
    let before = log_state(&r);
    // confirm answers a confirm item, and not a rejustify item
    expect_err(
        r.answer_plan_specs(
            &writer(),
            "w",
            &[spec_of(
                group(None, Some("confirm")),
                PlanAct::Confirm,
                Some("fine"),
                false,
            )],
        )
        .unwrap_err(),
        E_PLAN_ITEM,
        &format!(
            "confirm does not answer 1 items of the group: {}",
            pi(&w.d1, &w.q, PlanKind::Rejustify)
        ),
        "narrow the group, or answer those items one by one",
    );
    // a group of update items: all of them refuse it
    expect_err(
        r.answer_plan_specs(
            &writer(),
            "w",
            &[spec_of(
                group(Some(PlanKind::Update), None),
                PlanAct::Confirm,
                Some("fine"),
                false,
            )],
        )
        .unwrap_err(),
        E_PLAN_ITEM,
        &format!(
            "confirm does not answer 1 items of the group: {}",
            pi(&w.a, &w.p, PlanKind::Update)
        ),
        "narrow the group, or answer those items one by one",
    );
    assert_eq!(log_state(&r), before);
    assert_eq!(plan_ref(&r, "w"), None);
    // a later spec that fails refuses the specs before it too
    let e = r
        .answer_plan_specs(
            &writer(),
            "w",
            &[
                spec_of(
                    group(Some(PlanKind::Confirm), None),
                    PlanAct::Defer,
                    Some("later"),
                    false,
                ),
                spec_of(
                    group(Some(PlanKind::Update), None),
                    PlanAct::Confirm,
                    Some("x"),
                    false,
                ),
            ],
        )
        .unwrap_err();
    assert_eq!(e.code, E_PLAN_ITEM);
    assert_eq!(log_state(&r), before);
}

/// O13: an answer names exactly one of item and group.
#[test]
fn rg9_an_answer_with_item_and_group_is_bad_input() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = world(&mut r, "w");
    let before = log_state(&r);
    let both = PlanAnswerSpec {
        item: Some(pi(&w.a, &w.b, PlanKind::Confirm)),
        ..spec_of(
            group(Some(PlanKind::Confirm), None),
            PlanAct::Defer,
            Some("x"),
            false,
        )
    };
    let neither = PlanAnswerSpec {
        item: None,
        group: None,
        act: PlanAct::Defer,
        reason: Some("x".into()),
        repin: false,
        implements: None,
        editorial: None,
        reflects: None,
    };
    for bad in [both, neither] {
        expect_err(
            r.answer_plan_specs(&writer(), "w", &[bad]).unwrap_err(),
            E_BAD_INPUT,
            "an answer names one of item and group",
            "pass item for one item, or group for many",
        );
    }
    assert_eq!(log_state(&r), before);
    // the wire form refuses a key it does not know
    let e = serde_json::from_value::<PlanAnswerSpec>(json!({"act": "defer", "groups": {}}))
        .unwrap_err();
    assert!(e.to_string().contains("unknown field `groups`"), "{e}");
    let e = serde_json::from_value::<PlanAnswerSpec>(
        json!({"act": "defer", "group": {"follow": "update"}}),
    )
    .unwrap_err();
    assert!(e.to_string().contains("unknown field `follow`"), "{e}");
}

/// O14: `repin: true` on a confirm group writes one change, one repin op per matched subject
/// block, then one plan version with the confirm answers.
#[test]
fn rg9_a_repin_group_writes_one_change() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = world(&mut r, "w");
    let tracking = r.store().tracking().clone();
    let pinned = |r: &Rdy, block: &str| -> Option<Hash> {
        let st = r.read();
        let m = st.materialized(&Scope::View("w".into())).unwrap();
        m.get(&id(block)).unwrap().pins.get(&id(&w.a)).copied()
    };
    let current = |r: &Rdy| -> Hash {
        let st = r.read();
        let m = st.materialized(&Scope::View("w".into())).unwrap();
        m.get(&id(&w.a)).unwrap().content_hash_with(&tracking)
    };
    for s in [&w.b, &w.e] {
        assert_ne!(pinned(&r, s), Some(current(&r)), "{s} starts stale");
    }
    let changes = r.read().included("w").len();
    let h = r
        .answer_plan_specs(
            &writer(),
            "w",
            &[spec_of(
                group(Some(PlanKind::Confirm), None),
                PlanAct::Confirm,
                Some("repinned in bulk"),
                true,
            )],
        )
        .unwrap();
    // one change more, with one repin (pin-only update) op per block
    let included = r.read().included("w");
    assert_eq!(included.len(), changes + 1);
    let change = r.store().change(included.last().unwrap()).unwrap();
    let mut blocks: Vec<String> = change
        .ops
        .iter()
        .map(|op| match op {
            rdy_model::Op::Update { block, .. } => block.id.to_string(),
            other => panic!("not a repin op: {other:?}"),
        })
        .collect();
    blocks.sort();
    let mut want = vec![w.b.clone(), w.e.clone()];
    want.sort();
    assert_eq!(blocks, want);
    for s in [&w.b, &w.e] {
        assert_eq!(
            pinned(&r, s),
            Some(current(&r)),
            "{s} pins a's current version"
        );
    }
    // the answers are one plan version that starts the chain
    let plan = plan_of(&r, &h);
    assert_eq!((plan.prev, plan.stable), (None, None));
    assert_eq!(plan_ref(&r, "w"), Some(h));
    let mut got: Vec<(Hash, PlanAct)> = plan.answers.iter().map(|a| (a.item, a.act)).collect();
    let mut want: Vec<(Hash, PlanAct)> = [&w.b, &w.e]
        .iter()
        .map(|s| (item_hash(&id(&w.a), s, PlanKind::Confirm), PlanAct::Confirm))
        .collect();
    got.sort_by_key(|x| x.0);
    want.sort_by_key(|x| x.0);
    assert_eq!(got, want);
    for s in [&w.b, &w.e] {
        let row = state_of(&r, "w", &pi(&w.a, s, PlanKind::Confirm));
        assert_eq!(row["state"], "closed", "{row}");
    }
}

/// O14: a repin goes with a confirm group; the refusal writes nothing, not even a repin.
#[test]
fn rg9_repin_without_confirm_is_bad_input() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = world(&mut r, "w");
    let before = log_state(&r);
    let item = PlanAnswerSpec {
        repin: true,
        ..item_spec(
            &pi(&w.a, &w.b, PlanKind::Confirm),
            PlanAct::Confirm,
            Some("x"),
        )
    };
    for bad in [
        spec_of(
            group(Some(PlanKind::Confirm), None),
            PlanAct::Defer,
            Some("x"),
            true,
        ),
        spec_of(
            group(Some(PlanKind::Update), None),
            PlanAct::Done,
            None,
            true,
        ),
        item,
    ] {
        expect_err(
            r.answer_plan_specs(&writer(), "w", &[bad]).unwrap_err(),
            E_BAD_INPUT,
            "repin goes with a confirm group",
            "set act to confirm and pass a group",
        );
    }
    assert_eq!(log_state(&r), before);
}

/// O13: `follow_up: confirm` names the confirm and the rejustify items (the map of `kind_of`).
#[test]
fn rg9_follow_up_confirm_matches_rejustify_items_too() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = world(&mut r, "w");
    let h = r
        .answer_plan_specs(
            &writer(),
            "w",
            &[spec_of(
                group(None, Some("confirm")),
                PlanAct::Defer,
                Some("later"),
                false,
            )],
        )
        .unwrap();
    let mut got: Vec<Hash> = plan_of(&r, &h).answers.iter().map(|a| a.item).collect();
    let mut want = vec![
        item_hash(&id(&w.a), &w.b, PlanKind::Confirm),
        item_hash(&id(&w.a), &w.e, PlanKind::Confirm),
        item_hash(&id(&w.d1), &w.q, PlanKind::Rejustify),
    ];
    got.sort();
    want.sort();
    assert_eq!(got, want);
    // update names the update item; reaffirm names none here
    let h = r
        .answer_plan_specs(
            &writer(),
            "w",
            &[spec_of(
                group(None, Some("update")),
                PlanAct::Defer,
                Some("later"),
                false,
            )],
        )
        .unwrap();
    assert_eq!(plan_of(&r, &h).answers.len(), 4);
    expect_err(
        r.answer_plan_specs(
            &writer(),
            "w",
            &[spec_of(
                group(None, Some("reaffirm")),
                PlanAct::Defer,
                Some("x"),
                false,
            )],
        )
        .unwrap_err(),
        E_PLAN_ITEM,
        "the group matches no open item of the change plan for view:w",
        "read the plan with rdy_plan / rdy plan and pass a group it holds",
    );
}

/// An `item` spec answers as [`PlanAnswerInput`] does: the same answers in the same chain.
#[test]
fn rg9_an_item_spec_answers_as_plan_answer_input_does() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = world(&mut r, "one");
    // the same edits in a second view: item ids depend on cause, subject and kind only
    put(
        &mut r,
        "two",
        json!([{"op": "update", "id": w.a, "body": "A holds 60 days."}]),
    );
    let item = pi(&w.a, &w.b, PlanKind::Confirm);
    let by_spec = r
        .answer_plan_specs(
            &writer(),
            "one",
            &[item_spec(&item, PlanAct::Confirm, Some("as is"))],
        )
        .unwrap();
    let by_input = r
        .answer_plan(
            &writer(),
            "two",
            &[PlanAnswerInput {
                item: item.clone(),
                act: PlanAct::Confirm,
                reason: Some("as is".into()),
            }],
        )
        .unwrap();
    let (x, y) = (plan_of(&r, &by_spec), plan_of(&r, &by_input));
    assert_eq!(x.answers.len(), 1);
    let key = |p: &Plan| {
        p.answers
            .iter()
            .map(|a| (a.item, a.act, a.reason.clone(), a.subject, a.cause))
            .collect::<Vec<_>>()
    };
    assert_eq!(key(&x), key(&y));
    assert_eq!((x.prev, x.stable, x.heads), (y.prev, y.stable, y.heads));
    assert_eq!((x.view.as_str(), y.view.as_str()), ("one", "two"));
}

/// Which confirm items the view owes, as (cause, subject) display ids with state; a probe of the
/// fixture that the repin tests rely on.
#[test]
fn rg9_repin_world_owes_the_items_the_repin_tests_name() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = repin_world(&mut r, "w");
    for (cause, subject, edge) in [
        (&w.a, &w.b, "references"),
        (&w.c, &w.b, "references"),
        (&w.dx, &w.x, "supersedes"),
        (&w.dz, &w.z, "refines"),
    ] {
        let row = state_of(&r, "w", &pi(cause, subject, PlanKind::Confirm));
        assert_eq!(
            (&row["state"], &row["edge"]),
            (&json!("open"), &json!(edge)),
            "{row}"
        );
    }
}

/// O14: the repin change is self-briefed, holds one op per subject block with one target per
/// stale cause, and skips an item whose subject does not cite its cause (the `affects` reach of
/// a full supersede) and an item whose pin is current (the `refines` reach). Those two still get
/// their answers.
#[test]
fn rg9_a_repin_group_changes_only_stale_cited_pins() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = repin_world(&mut r, "w");
    let pin_of = |r: &Rdy, block: &str, cause: &str| -> Option<Hash> {
        let st = r.read();
        let m = st.materialized(&Scope::View("w".into())).unwrap();
        m.get(&id(block)).unwrap().pins.get(&id(cause)).copied()
    };
    let tracking = r.store().tracking().clone();
    let current = |r: &Rdy, block: &str| -> Hash {
        let st = r.read();
        let m = st.materialized(&Scope::View("w".into())).unwrap();
        m.get(&id(block)).unwrap().content_hash_with(&tracking)
    };
    // the z pin on dz is current already; x holds no pin on dx and does not cite it
    assert_eq!(pin_of(&r, &w.z, &w.dz), Some(current(&r, &w.dz)));
    assert_eq!(pin_of(&r, &w.x, &w.dx), None);
    for cause in [&w.a, &w.c] {
        assert_ne!(pin_of(&r, &w.b, cause), Some(current(&r, cause)));
    }
    // the superseded dx still materializes, so x/dx hits the 'subject does not cite its cause'
    // skip, not the 'cause gone' one
    {
        let st = r.read();
        let m = st.materialized(&Scope::View("w".into())).unwrap();
        assert!(
            m.get(&id(&w.dx)).is_some(),
            "superseded dx still materializes"
        );
    }
    let changes = r.read().included("w").len();
    let h = r
        .answer_plan_specs(
            &writer(),
            "w",
            &[spec_of(
                group(Some(PlanKind::Confirm), None),
                PlanAct::Confirm,
                Some("repinned in bulk"),
                true,
            )],
        )
        .unwrap();
    let included = r.read().included("w");
    assert_eq!(included.len(), changes + 1, "one change");
    let change = r.store().change(included.last().unwrap()).unwrap();
    // self-briefed, as the daemon's repin is
    assert!(change.links.briefing.is_some(), "{:?}", change.links);
    // one op, for b, whose block now pins both causes at their current version
    assert_eq!(change.ops.len(), 1, "{:?}", change.ops);
    assert_eq!(change.ops[0].block_id().to_string(), w.b);
    for cause in [&w.a, &w.c] {
        assert_eq!(
            pin_of(&r, &w.b, cause),
            Some(current(&r, cause)),
            "b pins {cause}"
        );
    }
    // no pin moved for x or z
    assert_eq!(pin_of(&r, &w.z, &w.dz), Some(current(&r, &w.dz)));
    assert_eq!(pin_of(&r, &w.x, &w.dx), None);
    // every matched item has its answer, the skipped ones too
    let answered: BTreeSet<Hash> = plan_of(&r, &h).answers.iter().map(|a| a.item).collect();
    for (cause, subject) in [(&w.a, &w.b), (&w.c, &w.b), (&w.dx, &w.x), (&w.dz, &w.z)] {
        assert!(
            answered.contains(&item_hash(&id(cause), subject, PlanKind::Confirm)),
            "no answer for {cause} -> {subject}"
        );
    }
}

/// O14: the answers are checked before the repin is written. A group that matches open confirm
/// items and holds an answer the check refuses (a confirm needs a reason) leaves the log as it
/// was: no repin change, no plan version.
#[test]
fn rg9_a_refused_answer_leaves_no_repin_change() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = repin_world(&mut r, "w");
    let before = log_state(&r);
    let changes = r.read().included("w").len();
    let e = r
        .answer_plan_specs(
            &writer(),
            "w",
            &[spec_of(
                group(Some(PlanKind::Confirm), None),
                PlanAct::Confirm,
                None,
                true,
            )],
        )
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert_eq!(r.read().included("w").len(), changes);
    assert_eq!(log_state(&r), before);
    assert_eq!(plan_ref(&r, "w"), None);
    // the stale pins of b are still stale
    let st = r.read();
    let m = st.materialized(&Scope::View("w".into())).unwrap();
    assert_ne!(
        m.get(&id(&w.b)).unwrap().pins.get(&id(&w.a)).copied(),
        Some(
            m.get(&id(&w.a))
                .unwrap()
                .content_hash_with(r.store().tracking())
        )
    );
}

/// A group spec with a reason one past `MAX_REASON_CHARS` is refused with the reason text,
/// whole, and writes nothing: no repin change, no plan version (tb-A4DAV).
#[test]
fn rg9_a_group_reason_over_the_limit_is_refused_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    repin_world(&mut r, "w");
    let before = log_state(&r);
    let changes = r.read().included("w").len();
    let long = "x".repeat(rdy_model::engine_fields::MAX_REASON_CHARS + 1);
    for repin in [false, true] {
        let e = r
            .answer_plan_specs(
                &writer(),
                "w",
                &[spec_of(
                    group(Some(PlanKind::Confirm), None),
                    PlanAct::Confirm,
                    Some(&long),
                    repin,
                )],
            )
            .unwrap_err();
        expect_err(
            e,
            E_BAD_INPUT,
            "confirm needs a reason of 1 to 2000 characters",
            "give a reason of 1 to 2000 characters",
        );
        assert_eq!(r.read().included("w").len(), changes);
        assert_eq!(log_state(&r), before);
        assert_eq!(plan_ref(&r, "w"), None);
    }
}

/// Runs git in `dir` with a fixed identity and no hooks; returns trimmed stdout.
fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
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

/// A done group of `update_implementation` items keeps no reason, whatever its length
/// (tb-A4DAV): a reason over `MAX_REASON_CHARS` on a done group is not refused, and no expanded
/// answer holds one, so the group never carries N copies of it.
#[test]
fn rg9_a_done_group_keeps_no_reason() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = world(&mut r, "w");
    // a code file citing a: its item is an update_implementation item the view owes
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "--initial-branch=main"]);
    let mut cfg = std::fs::read_to_string(dir.path().join(".rdy.yaml")).unwrap_or_default();
    cfg.push_str("code_repos: [app]\n");
    std::fs::write(dir.path().join(".rdy.yaml"), cfg).unwrap();
    std::fs::write(
        dir.path().join(".rdy/local.yaml"),
        format!(
            "code_paths:\n  app: {}\n",
            serde_json::to_string(&repo.path().display().to_string()).unwrap()
        ),
    )
    .unwrap();
    std::fs::write(repo.path().join("f.rs"), "fn f() {}\n").unwrap();
    git(repo.path(), &["add", "-A"]);
    let msg = format!("cite\n\nImplements: {}", w.a);
    git(repo.path(), &["commit", "-q", "--no-verify", "-m", &msg]);
    let item = pi(&w.a, "app:f.rs", PlanKind::UpdateImplementation);
    assert_eq!(state_of(&r, "w", &item)["state"], "open");
    let long = "x".repeat(rdy_model::engine_fields::MAX_REASON_CHARS + 1);
    let h = r
        .answer_plan_specs(
            &writer(),
            "w",
            &[spec_of(
                group(Some(PlanKind::UpdateImplementation), None),
                PlanAct::Done,
                Some(&long),
                false,
            )],
        )
        .unwrap();
    let plan = plan_of(&r, &h);
    assert_eq!(plan.answers.len(), 1);
    for a in &plan.answers {
        assert_eq!(a.act, PlanAct::Done);
        assert_eq!(a.reason, None);
    }
}

/// Canon: a stable doc `spec` with claims a ("A holds 30 days.") and b, which cites a. The view
/// `view` makes a briefed, justified edit of a: b owes a confirm item on stable content.
fn stable_world(r: &mut Rdy, view: &str) -> (String, String) {
    let clarify = || {
        Some(Justification::Editorial {
            class: EditorialClass::Clarify,
            reason: "the rule moved".into(),
        })
    };
    let w = r
        .write(
            &writer(),
            WriteRequest {
                view: "seed".into(),
                ops: parse_ops(
                    json!([
                        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page", "lifecycle": "stable"}},
                        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
                        {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
                        {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}}."}
                    ])
                    .as_array()
                    .unwrap()
                    .clone(),
                )
                .unwrap(),
                justification: clarify(),
                message: String::new(),
            },
        )
        .unwrap();
    land(r, "seed");
    let (a, b) = (w.created["$a"].clone(), w.created["$b"].clone());
    let brief = r
        .read()
        .brief(
            &Scope::Canon,
            &rdy_engine::Coverage {
                ids: vec![a.clone()],
                path: None,
            },
            None,
        )
        .unwrap();
    brief.register(&r.briefs());
    r.write_briefed(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: parse_ops(vec![
                json!({"op": "update", "id": a, "body": "A holds 60 days."}),
            ])
            .unwrap(),
            justification: clarify(),
            message: String::new(),
        },
        Some(brief.base.token.clone()),
    )
    .unwrap();
    (a, b)
}

/// A confirm group that repins, with justification `j`: (implements, editorial, reflects).
fn repin_spec(
    implements: Option<Vec<String>>,
    editorial: Option<EditorialArg>,
    reflects: Option<rdy_engine::ReflectsArg>,
) -> PlanAnswerSpec {
    PlanAnswerSpec {
        implements,
        editorial,
        reflects,
        ..spec_of(
            group(Some(PlanKind::Confirm), None),
            PlanAct::Confirm,
            Some("b still follows a"),
            true,
        )
    }
}

fn clarify_arg() -> EditorialArg {
    EditorialArg {
        class: "clarify".into(),
        reason: "b pins a's new text".into(),
    }
}

/// Ruling final F2 (D-ME2HD, D-MPJM0): a repin group on stable content takes the justification
/// of `rdy_write`. rdy records it on the repin change, so the version is ready, and it lands.
#[test]
fn rg_final_a_stable_repin_group_with_a_justification_lands() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let (a, b) = stable_world(&mut r, "w");
    let changes = r.read().included("w").len();
    r.answer_plan_specs(
        &writer(),
        "w",
        &[repin_spec(None, Some(clarify_arg()), None)],
    )
    .unwrap();
    let included = r.read().included("w");
    assert_eq!(included.len(), changes + 1, "one repin change");
    let change = r.store().change(included.last().unwrap()).unwrap();
    assert_eq!(
        change.justification,
        Some(Justification::Editorial {
            class: EditorialClass::Clarify,
            reason: "b pins a's new text".into(),
        })
    );
    let row = state_of(&r, "w", &pi(&a, &b, PlanKind::Confirm));
    assert_eq!(row["state"], "closed", "{row}");
    let p = r
        .propose(&writer(), "w", "w", Some(common::TEST_SUMMARY))
        .unwrap();
    assert!(p.ready, "{:#?}", p.issues);
    common::confirm_exceptions(&mut r, &approver(), &p);
    r.land(&approver(), &p.id)
        .unwrap_or_else(|e| panic!("land w: {e} {:?}", e.issues));
}

/// Ruling final F2: with no justification, a repin of stable normative content is refused before
/// anything is written: no repin change and no plan version.
#[test]
fn rg_final_a_stable_repin_group_without_a_justification_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    stable_world(&mut r, "w");
    let before = log_state(&r);
    let changes = r.read().included("w").len();
    expect_err(
        r.answer_plan_specs(&writer(), "w", &[repin_spec(None, None, None)])
            .unwrap_err(),
        E_BAD_INPUT,
        "a repin of stable content needs a justification",
        "pass implements, editorial or reflects, as rdy_write takes",
    );
    assert_eq!(log_state(&r), before);
    assert_eq!(r.read().included("w").len(), changes);
    assert_eq!(plan_ref(&r, "w"), None);
    // the justification rules are rdy_write's
    expect_err(
        r.answer_plan_specs(
            &writer(),
            "w",
            &[repin_spec(Some(vec![]), Some(clarify_arg()), None)],
        )
        .unwrap_err(),
        E_BAD_INPUT,
        &rdy_engine::one_justification_error().message,
        &rdy_engine::one_justification_error().fix,
    );
    expect_err(
        r.answer_plan_specs(&writer(), "w", &[repin_spec(Some(vec![]), None, None)])
            .unwrap_err(),
        E_BAD_INPUT,
        "implements is empty",
        "name the decision this change carries out, or leave implements out",
    );
    assert_eq!(log_state(&r), before);
}

/// Ruling final F2: a repin of draft content needs no justification, as today.
#[test]
fn rg_final_a_draft_repin_group_needs_no_justification() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = repin_world(&mut r, "w");
    let changes = r.read().included("w").len();
    r.answer_plan_specs(&writer(), "w", &[repin_spec(None, None, None)])
        .unwrap();
    let included = r.read().included("w");
    assert_eq!(included.len(), changes + 1, "one repin change");
    let change = r.store().change(included.last().unwrap()).unwrap();
    assert_eq!(change.justification, None);
    let row = state_of(&r, "w", &pi(&w.a, &w.b, PlanKind::Confirm));
    assert_eq!(row["state"], "closed", "{row}");
}

/// Ruling final F2: a justification goes with `repin: true`; without it the answer is refused
/// whole and nothing is written.
#[test]
fn rg_final_a_justification_without_repin_is_bad_input() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = world(&mut r, "w");
    let before = log_state(&r);
    let unpinned = |s: PlanAnswerSpec| PlanAnswerSpec { repin: false, ..s };
    for bad in [
        unpinned(repin_spec(None, Some(clarify_arg()), None)),
        unpinned(repin_spec(Some(vec![w.d1.clone()]), None, None)),
        unpinned(repin_spec(
            None,
            None,
            Some(rdy_engine::ReflectsArg {
                sources: None,
                code: Some(true),
            }),
        )),
        PlanAnswerSpec {
            editorial: Some(clarify_arg()),
            ..item_spec(
                &pi(&w.a, &w.b, PlanKind::Confirm),
                PlanAct::Confirm,
                Some("x"),
            )
        },
    ] {
        expect_err(
            r.answer_plan_specs(&writer(), "w", &[bad]).unwrap_err(),
            E_BAD_INPUT,
            "a justification goes with repin",
            "set repin to true, or remove the justification",
        );
    }
    assert_eq!(log_state(&r), before);
}
