//! ab3 (D-YAY1G, D-4JMPH): an as-built confirm item goes through the existing plan path: the
//! plan a propose binds, the cards beside the version, answers carried to a new revision, the
//! seal a landing writes, and a reopen, a reindex and a dump and load. No new record kind, field
//! or format: the items are ordinary confirm items in ordinary plan records.
use rdy_engine::as_built::{AS_BUILT_FIX_NO_ITEM, AS_BUILT_FIX_OPEN_ITEM};
use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, PendingQuery};
use rdy_engine::plan::compute::{BUILT_EDGE, built_cause_hash};
use rdy_engine::plan::{PlanAnswerInput, PlanQuery, ProposePlanRequest};
use rdy_engine::{ProposalInfo, Rdy, Scope, WriteRequest, parse_ops};
use rdy_model::plan::{Plan, PlanAct, PlanKind, PlanOutcome, item_hash};
use rdy_model::{BlockId, EditorialClass, Hash, Justification};
use rdy_qa::fixtures::{approver, land, project, write, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY, confirm_exceptions};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn h(s: &str) -> Hash {
    s.parse().unwrap()
}

fn pi(cause: &str, subject: &str) -> String {
    format!("pi-{}", item_hash(&id(cause), subject, PlanKind::Confirm))
}

fn ans(cause: &str, subject: &str, act: PlanAct, reason: &str) -> PlanAnswerInput {
    PlanAnswerInput {
        item: pi(cause, subject),
        act,
        reason: Some(reason.into()),
    }
}

fn propose(r: &mut Rdy, view: &str, answers: &[PlanAnswerInput]) -> ProposalInfo {
    r.propose_plan(
        &writer(),
        ProposePlanRequest {
            summary: Some(TEST_SUMMARY),
            plan_answers: answers,
            ..ProposePlanRequest::new(view, view)
        },
    )
    .unwrap_or_else(|e| panic!("propose {view}: {e}"))
}

fn bound(r: &Rdy, version: &str) -> Plan {
    let st = r.read();
    let reader = st.reader();
    let b = reader
        .plan_of_proposal(&h(version), r.store().log_end())
        .unwrap()
        .expect("a bound plan");
    reader.plan(&b).unwrap()
}

fn seal(r: &Rdy, landing: &Hash) -> (Hash, Plan) {
    let st = r.read();
    let reader = st.reader();
    let s = reader
        .plan_of_landing(landing, r.store().log_end())
        .unwrap()
        .expect("a seal");
    (s, reader.plan(&s).unwrap())
}

/// The as-built rows of a plan reply: (cause, subject, state or outcome).
fn built_rows(v: &Value) -> Vec<(String, String, String)> {
    let mut rows: Vec<_> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["edge"] == BUILT_EDGE)
        .map(|i| {
            assert_eq!(i["kind"], "confirm", "{i}");
            let state = i
                .get("outcome")
                .or_else(|| i.get("state"))
                .and_then(Value::as_str)
                .unwrap()
                .to_string();
            (
                i["cause"].as_str().unwrap().to_string(),
                i["subject"].as_str().unwrap().to_string(),
                state,
            )
        })
        .collect();
    rows.sort();
    rows
}

fn row(cause: &str, subject: &str, state: &str) -> (String, String, String) {
    (cause.into(), subject.into(), state.into())
}

fn read_plan(r: &Rdy, q: PlanQuery) -> Value {
    let q = PlanQuery {
        max_tokens: Some(100_000),
        ..q
    };
    serde_json::to_value(r.read().change_plan(&q, "work").unwrap()).unwrap()
}

/// The as-built warnings of each of `ids` in `scope`: (block, fix), sorted.
fn warns(r: &Rdy, scope: &Scope, ids: &[&String]) -> Vec<(String, String)> {
    let st = r.read();
    let mut out: Vec<_> = ids
        .iter()
        .flat_map(|i| st.context(scope, &id(i)).unwrap().warnings)
        .filter(|w| w.code == "W_AS_BUILT_OVERRIDE")
        .map(|w| (w.block.unwrap().to_string(), w.fix))
        .collect();
    out.sort();
    out
}

#[test]
fn ab3_plan_survives_propose_land_reopen_and_dump() {
    let (dir, mut r) = project();
    let seed = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Plain"}},
            {"op": "create", "type": "section", "id": "$p", "parent": "$doc", "fields": {"title": "Planned", "planned": true}},
            {"op": "create", "type": "decision", "id": "$d", "fields": {"statement": "Ship D.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "decision", "id": "$d2", "fields": {"statement": "Ship D2.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "claim", "id": "$j1", "parent": "$p", "body": "J1.", "links": {"decided_by": ["$d"]}},
            {"op": "create", "type": "claim", "id": "$j2", "parent": "$p", "body": "J2.", "links": {"decided_by": ["$d"]}},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B.", "fields": {"planned": true}, "links": {"decided_by": ["$d2"]}}
        ]),
    )
    .unwrap();
    let k: BTreeMap<String, String> = seed.created.clone();
    let (p, d, d2, j1, j2, b) = (
        &k["$p"], &k["$d"], &k["$d2"], &k["$j1"], &k["$j2"], &k["$b"],
    );
    land(&mut r, "seed").unwrap();
    r.write(
        &writer(),
        WriteRequest {
            view: "v".into(),
            ops: parse_ops(vec![
                json!({"op": "update", "id": d, "fields": {"built": "1.1.0"}}),
                json!({"op": "update", "id": d2, "fields": {"built": "1.1.0"}}),
            ])
            .unwrap(),
            justification: Some(Justification::Editorial {
                class: EditorialClass::Clarify,
                reason: "the record follows the release".into(),
            }),
            message: String::new(),
        },
    )
    .unwrap();

    // the propose binds the plan with its confirm answer, in its batch
    let p1 = propose(
        &mut r,
        "v",
        &[ans(d, p, PlanAct::Confirm, "the section ships later")],
    );
    let plan = bound(&r, &p1.id);
    let a = plan
        .answers
        .iter()
        .find(|a| a.item == item_hash(&id(d), p, PlanKind::Confirm))
        .expect("the confirm answer");
    assert_eq!(a.act, PlanAct::Confirm);
    assert_eq!(a.reason.as_deref(), Some("the section ships later"));
    // it binds the section's content and the decision's built fact
    let st = r.read();
    let view = Scope::View("v".into());
    let t = st.reader().tracking().clone();
    let dv = st.block(&view, &id(d)).unwrap();
    assert_eq!(
        a.cause,
        built_cause_hash(&dv.content_hash_with(&t), Some("1.1.0"))
    );
    assert_eq!(
        a.subject,
        Some(st.block(&view, &id(p)).unwrap().content_hash_with(&t))
    );
    drop(st);
    assert_eq!(
        p1.plan
            .items
            .iter()
            .filter(|i| i.edge == BUILT_EDGE)
            .map(|i| (i.cause.clone(), i.subject.clone()))
            .collect::<Vec<_>>(),
        vec![(d2.clone(), b.clone())]
    );
    // the proposal's plan, and its warnings: an open item names it, a confirmed one no item
    let pq = PlanQuery {
        proposal: Some(p1.r#ref.clone()),
        ..Default::default()
    };
    assert_eq!(
        built_rows(&read_plan(&r, pq)),
        vec![row(d, p, "closed"), row(d2, b, "open")]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );
    let ps = r
        .read()
        .resolve_scope(&format!("proposal:{}", p1.r#ref))
        .unwrap();
    assert_eq!(warns(&r, &ps, &[j1, b]), {
        let mut w = vec![
            (j1.clone(), AS_BUILT_FIX_NO_ITEM.to_string()),
            (b.clone(), AS_BUILT_FIX_OPEN_ITEM.to_string()),
        ];
        w.sort();
        w
    });

    // a defer in the view, then a new revision: the answers carry
    r.answer_plan(
        &writer(),
        "v",
        &[ans(d2, b, PlanAct::Defer, "review with the release")],
    )
    .unwrap();
    let p2 = propose(&mut r, "v", &[]);
    assert_eq!((p2.stable.as_str(), p2.version), (p1.stable.as_str(), 2));
    let carried = bound(&r, &p2.id);
    let acts: Vec<(Hash, PlanAct)> = carried.answers.iter().map(|a| (a.item, a.act)).collect();
    assert!(
        acts.contains(&(item_hash(&id(d), p, PlanKind::Confirm), PlanAct::Confirm)),
        "{acts:?}"
    );
    assert!(
        acts.contains(&(item_hash(&id(d2), b, PlanKind::Confirm), PlanAct::Defer)),
        "{acts:?}"
    );
    assert_eq!(p2.plan.open, 0, "{:?}", p2.plan);
    assert_eq!(p2.plan.deferred, 1, "{:?}", p2.plan);
    // the cards beside the version: its deferral card
    let q = r
        .read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap();
    assert!(
        q.items
            .iter()
            .any(|i| i.kind == ItemKind::Defer && i.r#ref == format!("defer:{}", p2.r#ref)),
        "{:#?}",
        q.items
    );

    // the landing seals both items as ordinary confirm items
    let accepted = r
        .answer(
            &approver(),
            PERSON,
            rdy_engine::answer::AnswerRequest {
                r#ref: format!("defer:{}", p2.r#ref),
                option: "o1".into(),
                ..Default::default()
            },
        )
        .unwrap();
    confirm_exceptions(&mut r, &approver(), &p2.r#ref);
    let landing = h(&r.land(&approver(), &p2.id).unwrap());
    let (seal_id, sealed) = seal(&r, &landing);
    let items: BTreeMap<(String, String), (PlanKind, String, PlanOutcome, Option<Hash>)> = sealed
        .seal
        .as_ref()
        .unwrap()
        .items
        .iter()
        .filter(|i| i.edge == BUILT_EDGE)
        .map(|i| {
            (
                (i.cause.to_string(), i.subject.clone()),
                (i.kind, i.edge.clone(), i.outcome, i.answer),
            )
        })
        .collect();
    let accept = accepted.answer.as_deref().map(h);
    assert_eq!(
        items,
        BTreeMap::from([
            (
                (d.clone(), p.clone()),
                (
                    PlanKind::Confirm,
                    BUILT_EDGE.to_string(),
                    PlanOutcome::Confirmed,
                    None
                )
            ),
            (
                (d2.clone(), b.clone()),
                (
                    PlanKind::Confirm,
                    BUILT_EDGE.to_string(),
                    PlanOutcome::Deferred,
                    accept
                )
            ),
        ])
    );
    assert_eq!(
        built_rows(&read_plan(
            &r,
            PlanQuery {
                landing: Some(landing.to_string()),
                ..Default::default()
            }
        )),
        {
            let mut want = vec![row(d, p, "confirmed"), row(d2, b, "deferred")];
            want.sort();
            want
        }
    );

    // canon: the confirmed section's children are quiet, the deferred claim still warns
    let quiet = |r: &Rdy, why: &str| {
        assert_eq!(
            warns(r, &Scope::Canon, &[j1, j2, b]),
            vec![(b.clone(), AS_BUILT_FIX_NO_ITEM.to_string())],
            "{why}"
        );
        assert_eq!(seal(r, &landing).0, seal_id, "{why}");
    };
    quiet(&r, "as landed");
    r.close().unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    quiet(&r, "after a reopen");
    r.reindex().unwrap();
    quiet(&r, "after a reindex");
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("p.rdydump");
    r.read().dump(&file).unwrap();
    r.close().unwrap();
    let to = scratch.path().join("loaded");
    rdy_engine::load::load(&file, &to).unwrap();
    let r = Rdy::open(&to).unwrap();
    quiet(&r, "after a dump and a load");
}
