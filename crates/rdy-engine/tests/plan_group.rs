//! Grouped plan answers and the bulk repin (D-MPJM0, D-ME2HD, O13, O14): a group expands to one
//! answer per open item it matches, in one plan version; a group that matches nothing or holds
//! an item the act does not answer is refused whole; `repin: true` on a confirm group writes
//! one change of repin ops, then the answers.
mod common;

use rdy_engine::plan::api::PlanAnswerSpec;
use rdy_engine::plan::group::PlanGroup;
use rdy_engine::plan::{PlanAnswerInput, PlanQuery};
use rdy_engine::{
    DecisionInput, E_BAD_INPUT, E_PLAN_ITEM, ProposalInfo, Rdy, RdyError, Scope, Session,
    WriteRequest, WriteResult, parse_ops,
};
use rdy_model::plan::{Plan, PlanAct, PlanKind, item_display, item_hash};
use rdy_model::{BlockId, Hash, Role};
use serde_json::{Value, json};

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
    }
}

fn item_spec(item: &str, act: PlanAct, reason: Option<&str>) -> PlanAnswerSpec {
    PlanAnswerSpec {
        item: Some(item.into()),
        group: None,
        act,
        reason: reason.map(str::to_string),
        repin: false,
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
    let row = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == item)
        .unwrap_or_else(|| panic!("no item {item}: {v:#}"))
        .clone();
    row
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
    // a refused answer after the repin is not the case: the answers were checked before the write
    let before = log_state(&r);
    let e = r
        .answer_plan_specs(
            &writer(),
            "w",
            &[spec_of(
                group(Some(PlanKind::Confirm), None),
                PlanAct::Confirm,
                Some("x"),
                true,
            )],
        )
        .unwrap_err();
    assert_eq!(e.code, E_PLAN_ITEM, "{e}");
    assert_eq!(log_state(&r), before);
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
