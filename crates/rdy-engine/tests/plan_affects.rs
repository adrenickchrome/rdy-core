//! rg4 (D-B55PS, D-SCJGF, D-NJ2SP): a full supersede reaches the old decision's `affects`
//! targets in the change plan, each as a `confirm` item with the edge `supersedes` and the old
//! decision as its cause. A revoke does not (D-B55PS names supersede only); a part supersede
//! keeps its reach through everything the old decision governs.
mod common;

use rdy_engine::plan::PlanQuery;
use rdy_engine::{DecisionInput, Rdy, Session, WriteRequest, WriteResult, parse_ops};
use rdy_model::plan::{PlanKind, item_display, item_hash};
use rdy_model::{BlockId, Role};
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

fn statement(s: &str, parent: &BlockId) -> DecisionInput {
    DecisionInput {
        parent: parent.to_string(),
        statement: s.into(),
        ..Default::default()
    }
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

fn pi(cause: &BlockId, subject: &BlockId, kind: PlanKind) -> String {
    item_display(&item_hash(cause, &subject.to_string(), kind))
}

fn item<'v>(plan: &'v Value, cause: &BlockId, subject: &BlockId) -> Option<&'v Value> {
    let want = pi(cause, subject, PlanKind::Confirm);
    plan["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == want)
}

/// The items of `plan` whose subject is `subject`.
fn items_for<'v>(plan: &'v Value, subject: &BlockId) -> Vec<&'v Value> {
    plan["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["subject"] == subject.to_string())
        .collect()
}

/// Canon: a section with claims p1 and p2, and an accepted decision d1 that affects both.
struct Fx {
    sec: BlockId,
    p1: BlockId,
    p2: BlockId,
    d1: BlockId,
}

fn project() -> (tempfile::TempDir, Rdy, Fx) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$p1", "parent": "$s", "body": "P1 holds 30 days."},
            {"op": "create", "type": "claim", "id": "$p2", "parent": "$s", "body": "P2 holds 30 days."}
        ]),
    );
    let g = |k: &str| id(&w.created[k]);
    let (sec, p1, p2) = (g("$s"), g("$p1"), g("$p2"));
    let d = r
        .decide(
            &writer(),
            "seed",
            DecisionInput {
                affects: vec![p1.to_string(), p2.to_string()],
                ..statement("Keep 30 days", &sec)
            },
        )
        .unwrap();
    let d1 = id(&d.created["$decision"]);
    land(&mut r, "seed");
    (dir, r, Fx { sec, p1, p2, d1 })
}

#[test]
fn rg4_a_full_supersede_gives_each_affects_target_a_confirm_item() {
    let (_d, mut r, fx) = project();
    let w = r
        .supersede(
            &writer(),
            "w",
            &fx.d1.to_string(),
            None,
            statement("Keep 60 days", &fx.sec),
        )
        .unwrap();
    let d2 = id(&w.created["$decision"]);
    let p = whole(&r, "w");
    for target in [&fx.p1, &fx.p2] {
        let i =
            item(&p, &fx.d1, target).unwrap_or_else(|| panic!("no confirm for {target}: {p:#}"));
        assert_eq!(i["kind"], "confirm", "{p:#}");
        assert_eq!(i["edge"], "supersedes", "{p:#}");
        assert_eq!(i["cause"], fx.d1.to_string(), "{p:#}");
        assert_eq!(i["state"], "open", "{p:#}");
        assert_eq!(items_for(&p, target).len(), 1, "{p:#}");
    }
    // the superseding decision is no subject of its own supersede
    assert!(items_for(&p, &d2).is_empty(), "{p:#}");
}

#[test]
fn rg4_a_revoke_gives_no_affects_item() {
    let (_d, mut r, fx) = project();
    r.revoke(
        &writer(),
        "w",
        &fx.d1.to_string(),
        "No longer true.",
        &fx.sec.to_string(),
        None,
    )
    .unwrap();
    let p = whole(&r, "w");
    for target in [&fx.p1, &fx.p2] {
        assert!(items_for(&p, target).is_empty(), "{target}: {p:#}");
    }
}

/// Guard: a part supersede reaches what the old decision governs, `affects` targets included,
/// as it did before.
#[test]
fn rg4_a_part_supersede_keeps_its_reach() {
    let (_d, mut r, fx) = project();
    let w = r
        .supersede(
            &writer(),
            "w",
            &fx.d1.to_string(),
            Some("the duration".into()),
            statement("Keep 60 days", &fx.sec),
        )
        .unwrap();
    let d2 = id(&w.created["$decision"]);
    let p = whole(&r, "w");
    for target in [&fx.p1, &fx.p2] {
        let i =
            item(&p, &fx.d1, target).unwrap_or_else(|| panic!("no confirm for {target}: {p:#}"));
        assert_eq!(i["edge"], "supersedes", "{p:#}");
        assert_eq!(i["state"], "open", "{p:#}");
    }
    assert!(items_for(&p, &d2).is_empty(), "{p:#}");
}

#[test]
fn rg4_an_affects_target_the_version_rewrites_closes_as_updated() {
    let (_d, mut r, fx) = project();
    r.supersede(
        &writer(),
        "w",
        &fx.d1.to_string(),
        None,
        statement("Keep 60 days", &fx.sec),
    )
    .unwrap();
    // the version deletes p2: its own change to the target, so the item closes as updated
    put(
        &mut r,
        "w",
        json!([{"op": "delete", "id": fx.p2.to_string()}]),
    );
    let p = whole(&r, "w");
    let open = item(&p, &fx.d1, &fx.p1).unwrap_or_else(|| panic!("no item for p1: {p:#}"));
    assert_eq!(open["state"], "open", "{p:#}");
    // rewriting p1 in place is not that: a reach always needs a reasoned confirm (p-0HXT0)
    put(
        &mut r,
        "w",
        json!([{"op": "update", "id": fx.p1.to_string(), "body": "P1 holds 60 days."}]),
    );
    let p = whole(&r, "w");
    let open = item(&p, &fx.d1, &fx.p1).unwrap_or_else(|| panic!("no item for p1: {p:#}"));
    assert_eq!(open["state"], "open", "{p:#}");
    let closed = item(&p, &fx.d1, &fx.p2).unwrap_or_else(|| panic!("no item for p2: {p:#}"));
    assert_eq!(closed["state"], "closed", "{p:#}");
    assert_eq!(closed["how"], "updated", "{p:#}");
}
