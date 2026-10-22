//! cp16 (D-CQMA4, D-EJBPD, D-GV465, D-AAQ9Z, D-FDE74, D-KQ4CA; concepts/decision-intake p-WQB0V,
//! concepts/decisions p-3W7KT, reference/mcp-tools p-D3P76, reference/store-format p-N50DS,
//! p-CST21): each block that made accepted decisions of canon suspect stands in the queue as one
//! re-affirmation card. Its answer is the approval of its own isolated landing: `They still hold`
//! lands the members' re-affirmation alone, in a fresh internal view, with the card as evidence;
//! `Keep them suspect` records one KeepSuspect answer and no landing. The card binds every member,
//! version and retirement, also where its text is cut, and its private view takes no public write.
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::engine::{Engine, Local, with_writer_result};
use rdy_engine::pending::reaffirm::{
    MAX_CARD_BYTES, MAX_CARD_MEMBERS, MAX_CARD_PAIRS, check_card_bounds, reaffirm_binding_len,
};
use rdy_engine::pending::{
    CARD_MAX_TOKENS, HOLD_LABEL, Item, ItemKind, KEEP_LABEL, Pending, PendingQuery,
    PlanCardBinding, REAFFIRM_WHY, ReaffirmMember, ReaffirmSuspect, card_hash, render,
};
use rdy_engine::signing_act::{SigningAct, run_signed_act};
use rdy_engine::signing_crypto::{AgentIdentity, SigningAgent};
use rdy_engine::{
    DecisionInput, E_BAD_INPUT, E_ITEM_MOVED, E_LIMIT, E_RESERVED_VIEW, Rdy, RdyError, Scope,
    Session,
};
use rdy_graph::DerivedStatus;
use rdy_mcp::tools::{McpState, rdy_answer, rdy_pending, rdy_session};
use rdy_model::engine_fields::{Against, REAFFIRMED, decode_reaffirmed};
use rdy_model::plan::{Plan, PlanKind, PlanOutcome};
use rdy_model::{
    Answer, AnswerAct, ApprovalEvidence, Assurance, BlockId, Change, EvidenceKind, Hash,
    LandingRecord, Op, Role,
};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{approver, land, project, write, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY};
use rdy_qa::signing::{QaAgent, TestKey};
use rdy_store::kinds;
use rdy_store::segment::Location;
use rdy_store::view::ViewOpKind;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};

// ---- the project ----

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// Canon: doc `spec`, section `Rules`, claims a, c and z.
struct World {
    s: String,
    a: String,
    c: String,
    z: String,
}

fn world(r: &mut Rdy) -> World {
    let w = write(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C stands."},
            {"op": "create", "type": "claim", "id": "$z", "parent": "$s", "body": "Z stands alone."}
        ]),
    )
    .unwrap();
    land(r, "seed").unwrap();
    let get = |k: &str| w.created[k].clone();
    World {
        s: get("$s"),
        a: get("$a"),
        c: get("$c"),
        z: get("$z"),
    }
}

/// A decision of `statement` (titled `title`) in `view` that depends on `on`, not landed.
fn decide_in(
    r: &mut Rdy,
    view: &str,
    section: &str,
    statement: &str,
    title: Option<&str>,
    on: &[&str],
    reversibility: &str,
) -> String {
    r.decide(
        &writer(),
        view,
        DecisionInput {
            parent: section.into(),
            statement: statement.into(),
            title: title.map(str::to_string),
            depends_on: on.iter().map(|s| s.to_string()).collect(),
            reversibility: Some(reversibility.into()),
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("decide {statement}: {e}"))
    .created["$decision"]
        .clone()
}

/// [`decide_in`] in a view of its own, landed: an accepted decision of canon.
fn decide(r: &mut Rdy, s: &str, statement: &str, title: Option<&str>, on: &[&str]) -> String {
    let view = format!("d-{}", statement.replace(' ', "-"));
    let d = decide_in(r, &view, s, statement, title, on, "two_way");
    land(r, &view).unwrap();
    d
}

/// A new body for `target`, landed through `view`; returns the landing.
fn change(r: &mut Rdy, view: &str, target: &str, body: &str) -> String {
    write(
        r,
        view,
        json!([{"op": "update", "id": target, "body": body}]),
    )
    .unwrap_or_else(|e| panic!("edit {target}: {e}"));
    land(r, view).unwrap()
}

/// The public re-affirmation of `d` against `target` in `view`, landed.
fn reaffirm_landed(r: &mut Rdy, view: &str, d: &str, target: &str) {
    write(
        r,
        view,
        json!([{"op": "reaffirm", "id": d, "targets": [target], "reason": "Checked by hand."}]),
    )
    .unwrap_or_else(|e| panic!("reaffirm {d}: {e}"));
    land(r, view).unwrap();
}

fn queue(r: &Rdy) -> Pending {
    r.read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .expect("pending")
}

/// The re-affirmation card of `cause`, if the queue holds one.
fn card(r: &Rdy, cause: &str) -> Option<Item> {
    queue(r).items.into_iter().find(|i| {
        matches!(&i.plan_binding, Some(PlanCardBinding::Reaffirm { cause: c, .. }) if c.as_str() == cause)
    })
}

fn the_card(r: &Rdy, cause: &str) -> Item {
    card(r, cause).unwrap_or_else(|| panic!("no card of {cause}: {:#?}", queue(r).items))
}

fn members(i: &Item) -> Vec<String> {
    match &i.plan_binding {
        Some(PlanCardBinding::Reaffirm { members, .. }) => {
            members.iter().map(|m| m.id.to_string()).collect()
        }
        other => panic!("not a re-affirmation card: {other:?}"),
    }
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

fn log_end(r: &Rdy) -> Location {
    r.store().log_end()
}

fn version(r: &Rdy, b: &str) -> Hash {
    r.read()
        .materialized(&Scope::Canon)
        .unwrap()
        .get(&id(b))
        .unwrap()
        .version_hash()
}

fn card_json(i: &Item) -> Value {
    json!({"ref": i.r#ref, "card_hash": i.card_hash, "card": i.card})
}

/// A local answer by the person.
fn answer(
    r: &mut Rdy,
    item: &str,
    option: &str,
    note: Option<&str>,
    shown: Option<&str>,
) -> Result<Value, RdyError> {
    r.answer(
        &approver(),
        PERSON,
        AnswerRequest {
            r#ref: item.into(),
            option: option.into(),
            card_hash: shown.map(str::to_string),
            note: note.map(str::to_string),
            ..AnswerRequest::default()
        },
    )
    .map(|i| serde_json::to_value(i).unwrap())
}

/// The re-affirmation views of the project.
fn internal_views(r: &Rdy) -> Vec<String> {
    r.store()
        .refs_with_prefix("view/reaffirm-")
        .unwrap()
        .into_iter()
        .map(|(n, _)| n)
        .collect()
}

/// The reason `d` records for `cause` in canon.
fn reaffirmed_reason(r: &Rdy, d: &str, cause: &str) -> Option<String> {
    let m = r.read().materialized(&Scope::Canon).unwrap();
    let b = m.get(&id(d)).unwrap();
    let rec = decode_reaffirmed(b.fields.get(REAFFIRMED)?).ok()?;
    rec.get(&id(cause)).map(|x| x.reason.clone())
}

/// The latest seal that names decision `d`.
fn seal_of(r: &Rdy, d: &str) -> Plan {
    let st = r.read();
    let h = *st
        .reader()
        .plans_of_decision(&id(d), st.seq())
        .unwrap()
        .last()
        .expect("a seal names the decision");
    st.reader().plan(&h).unwrap()
}

/// The structural suspect rows of canon on `block`.
fn suspects_of(r: &Rdy, block: &str) -> Vec<rdy_engine::SuspectReply> {
    r.read()
        .suspect_details(&Scope::Canon)
        .unwrap()
        .into_iter()
        .filter(|s| s.block.as_str() == block && s.target.is_some())
        .collect()
}

/// Two accepted decisions d1 ("One") and d2 ("Two") that depend on a; a then changes, so both are
/// suspect on it.
struct Pair {
    w: World,
    d1: String,
    d2: String,
}

fn pair(r: &mut Rdy) -> Pair {
    let w = world(r);
    let d1 = decide(r, &w.s, "One rule", Some("One"), &[&w.a]);
    let d2 = decide(r, &w.s, "Two rule", Some("Two"), &[&w.a]);
    change(r, "x", &w.a, "A holds 60 days.");
    Pair { w, d1, d2 }
}

/// An MCP state on `r` with the agent session `agent:relay`, run `r1`.
fn agent_state(r: Rdy) -> McpState {
    let mut st = McpState::new(Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:relay", "run": "r1", "role": "writer"}))
            .unwrap(),
    )
    .unwrap();
    st
}

fn mcp_card(st: &mut McpState, r#ref: &str) -> Value {
    let q = rdy_pending(
        st,
        serde_json::from_value(json!({"max_tokens": 20000})).unwrap(),
    )
    .unwrap();
    q["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["ref"] == r#ref)
        .unwrap_or_else(|| panic!("no card {ref}: {q:#}"))
        .clone()
}

fn mcp_answer(
    st: &mut McpState,
    card: &Value,
    option: &str,
    note: Option<&str>,
) -> Result<Value, RdyError> {
    rdy_answer(
        st,
        serde_json::from_value(json!({
            "ref": card["ref"], "option": option, "card_hash": card["card_hash"], "note": note,
            "evidence": "Yes, they hold.", "question": "Do they still hold?", "session_ref": "chat-7",
        }))
        .unwrap(),
    )
}

fn on_engine<T: Send + 'static>(
    e: &dyn Engine,
    f: impl FnOnce(&mut Rdy) -> T + Send + 'static,
) -> T {
    with_writer_result(e, move |r| Ok(f(r))).expect("write")
}

// ---- tests ----

/// p-WQB0V: the cards group canon's suspect live or partly superseded accepted decisions by
/// cause. A decision that a latest open proposal re-affirms or fully retires is on no card; an
/// accepted deferral leaves out only its own cause/member pair. The card's fields are exact, and
/// the cards stand after the batches.
#[test]
fn cp16_card_membership_excludes_open_work_and_accepted_deferrals() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    // d6 depends on a and c; a changes, and the person keeps d6 suspect on a: (d6, a) is deferred
    let d6 = decide(&mut r, &w.s, "Six rule", None, &[&w.a, &w.c]);
    change(&mut r, "x1", &w.a, "A holds 60 days.");
    let first = the_card(&r, &w.a);
    assert_eq!(members(&first), vec![d6.clone()]);
    answer(
        &mut r,
        &first.r#ref,
        "o2",
        Some("d6 waits for the audit."),
        None,
    )
    .unwrap();
    assert!(card(&r, &w.a).is_none(), "a deferred pair makes no card");
    // d1 to d4 depend on a as it is now; d5 supersedes d2 in part
    let d1 = decide(&mut r, &w.s, "One rule", Some("One"), &[&w.a]);
    let d2 = decide(&mut r, &w.s, "Two rule", Some("Two"), &[&w.a]);
    let d3 = decide(&mut r, &w.s, "Three rule", None, &[&w.a]);
    let d4 = decide(&mut r, &w.s, "Four rule", None, &[&w.a]);
    let d5 = decide_in(&mut r, "p5", &w.s, "Five rule", None, &[], "two_way");
    write(
        &mut r,
        "p5",
        json!([{"op": "update", "id": d5, "links": {"supersedes": [{"target": d2, "part": "the timing"}]}}]),
    )
    .unwrap();
    land(&mut r, "p5").unwrap();
    let landing = change(&mut r, "x2", &w.a, "A holds 90 days.");
    assert!(matches!(
        r.read().graph(&Scope::Canon).unwrap().status(&id(&d2)),
        Some(DerivedStatus::PartiallySuperseded { .. })
    ));
    // open work: w re-affirms d3; v supersedes d4 whole
    write(
        &mut r,
        "w",
        json!([{"op": "reaffirm", "id": d3, "targets": [w.a], "reason": "Still fits."}]),
    )
    .unwrap();
    r.propose(&writer(), "w", "w", Some(TEST_SUMMARY)).unwrap();
    let d7 = decide_in(&mut r, "v", &w.s, "Seven rule", None, &[], "two_way");
    write(
        &mut r,
        "v",
        json!([{"op": "update", "id": d7, "links": {"supersedes": [{"target": d4}]}}]),
    )
    .unwrap();
    r.propose(&writer(), "v", "v", Some(TEST_SUMMARY)).unwrap();
    change(&mut r, "x3", &w.c, "C stands firm.");

    // the card of a: d1 and d2 (partly superseded); d3, d4 and d6 are left out
    let a = the_card(&r, &w.a);
    assert_eq!(members(&a), sorted(vec![d1.clone(), d2.clone()]));
    let av = version(&r, &w.a);
    assert_eq!(
        a.r#ref,
        format!("reaffirm:{}@{}", w.a, &av.to_string()[..12])
    );
    assert_eq!(a.kind, ItemKind::Reaffirm);
    assert_eq!(a.title, format!("Re-affirm after {}", w.a));
    assert_eq!(a.why, REAFFIRM_WHY);
    assert_eq!(
        a.summary.as_deref(),
        Some(format!("Re-affirm the listed decisions after {}.", w.a).as_str())
    );
    let facts: Vec<(&str, &str)> = a
        .facts
        .iter()
        .map(|f| (f.name.as_ref(), f.value.as_str()))
        .collect();
    assert_eq!(facts, vec![("Cause", w.a.as_str())]);
    let mut lines = vec![format!("{d1}  One"), format!("{d2}  Two")];
    lines.sort();
    assert_eq!(a.members, lines);
    let opts: Vec<(&str, &str, bool)> = a
        .options
        .iter()
        .map(|o| (o.id.as_str(), o.label.as_str(), o.recommended.is_some()))
        .collect();
    assert_eq!(
        opts,
        vec![("o1", HOLD_LABEL, false), ("o2", KEEP_LABEL, false)]
    );
    assert_eq!(
        (a.batch.as_deref(), a.exception, a.blocking),
        (None, None, false)
    );
    let rec = r.store().landing(&landing.parse().unwrap()).unwrap();
    assert_eq!(a.raised_by, rec.approved_by.actor);
    assert_eq!(a.raised_at, rdy_engine::rfc3339(rec.approved_by.at_ms));
    let Some(PlanCardBinding::Reaffirm {
        cause,
        version: bound,
        retirements,
        members: bound_members,
    }) = &a.plan_binding
    else {
        panic!("{:?}", a.plan_binding)
    };
    assert_eq!((cause, *bound, retirements.len()), (&id(&w.a), av, 0));
    for m in bound_members {
        assert_eq!(m.version, version(&r, m.id.as_str()));
        assert_eq!(
            m.suspects,
            vec![ReaffirmSuspect {
                target: id(&w.a),
                reason: "changed".into()
            }]
        );
    }
    // the card of c: d6, whose (d6, a) pair stays deferred; a member with no title shows its id
    let c = the_card(&r, &w.c);
    assert_eq!(members(&c), vec![d6.clone()]);
    assert_eq!(c.members, vec![format!("{d6}  {d6}")]);
    let Some(PlanCardBinding::Reaffirm { members: six, .. }) = &c.plan_binding else {
        panic!()
    };
    let mut both = vec![
        ReaffirmSuspect {
            target: id(&w.a),
            reason: "changed".into(),
        },
        ReaffirmSuspect {
            target: id(&w.c),
            reason: "changed".into(),
        },
    ];
    both.sort();
    assert_eq!(six[0].suspects, both, "the binding holds every suspect");
    // order: the batches and the items beside them, then the cards
    let items = queue(&r).items;
    let last_batch = items
        .iter()
        .rposition(|i| i.kind != ItemKind::Reaffirm)
        .unwrap();
    let first_card = items
        .iter()
        .position(|i| i.kind == ItemKind::Reaffirm)
        .unwrap();
    assert!(last_batch < first_card, "{items:#?}");
    assert_eq!(
        items.iter().filter(|i| i.kind == ItemKind::Approve).count(),
        2
    );
}

/// O10: the card's hash binds each member's full version and suspects and the cause's whole
/// retirement set, also where the text leaves them out.
#[test]
fn cp16_card_hash_binds_omitted_members_versions_and_retirements() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    // x is retired by y; m1 and m2 depend on x, m1 on c too
    let x = decide(&mut r, &w.s, "X rule", Some("X"), &[]);
    let m1 = decide(&mut r, &w.s, "M1 rule", Some("M1"), &[&x, &w.c]);
    let m2 = decide(&mut r, &w.s, "M2 rule", Some("M2"), &[&x]);
    let y = decide_in(&mut r, "y", &w.s, "Y rule", None, &[], "two_way");
    write(
        &mut r,
        "y",
        json!([{"op": "update", "id": y, "links": {"supersedes": [{"target": x}]}}]),
    )
    .unwrap();
    land(&mut r, "y").unwrap();
    change(&mut r, "xc", &w.c, "C stands firm.");
    let before = the_card(&r, &x);
    assert_eq!(members(&before), sorted(vec![m1.clone(), m2.clone()]));
    assert_eq!(before.title, "Re-affirm after X");
    let Some(PlanCardBinding::Reaffirm { retirements, .. }) = &before.plan_binding else {
        panic!()
    };
    assert_eq!(retirements, &vec![id(&y)]);

    // a member the text leaves out is still bound: the cut card is the same, the hash is not
    let cut = render(&before, 1);
    assert!(!cut.contains(&m1), "{cut}");
    let mut moved = before.clone();
    if let Some(PlanCardBinding::Reaffirm { members, .. }) = &mut moved.plan_binding {
        members[0].version = Hash::of(b"another version");
    }
    assert_eq!(render(&moved, 1), cut);
    assert_ne!(card_hash(&moved), card_hash(&before));

    // a hidden change of a member: m1 is re-affirmed on c only; its line on x's card is the
    // same, its version and suspects are not
    reaffirm_landed(&mut r, "rc", &m1, &w.c);
    let after = the_card(&r, &x);
    assert_eq!((&after.r#ref, &after.card), (&before.r#ref, &before.card));
    assert_ne!(after.card_hash, before.card_hash);

    // a new retirer: z revokes x too; the ref and the text stay, the hash moves
    let z = decide_in(&mut r, "z", &w.s, "Z rule", None, &[], "two_way");
    write(
        &mut r,
        "z",
        json!([{"op": "update", "id": z, "links": {"revokes": [{"target": x}]}}]),
    )
    .unwrap();
    land(&mut r, "z").unwrap();
    let last = the_card(&r, &x);
    assert_eq!((&last.r#ref, &last.card), (&after.r#ref, &after.card));
    assert_ne!(last.card_hash, after.card_hash);
    let Some(PlanCardBinding::Reaffirm { retirements, .. }) = &last.plan_binding else {
        panic!()
    };
    assert_eq!(
        retirements,
        &sorted(vec![y, z]).iter().map(|s| id(s)).collect::<Vec<_>>()
    );
}

/// A card whose membership, a full version or a suspect moved is `E_ITEM_MOVED` with the fresh
/// card in `corrected`, and nothing is written; so is a ref whose cause moved.
#[test]
fn cp16_moved_card_returns_fresh_card_before_writing() {
    let (_d, mut r) = project();
    let p = pair(&mut r);
    let shown = the_card(&r, &p.w.a);
    // the membership moves: d2 is re-affirmed by hand
    reaffirm_landed(&mut r, "h", &p.d2, &p.w.a);
    let fresh = the_card(&r, &p.w.a);
    assert_eq!(fresh.r#ref, shown.r#ref);
    assert_eq!(members(&fresh), vec![p.d1.clone()]);
    for (option, note) in [("o1", None), ("o2", Some("Wait."))] {
        let end = log_end(&r);
        let e = answer(&mut r, &shown.r#ref, option, note, Some(&shown.card_hash)).unwrap_err();
        assert_eq!(e.code, E_ITEM_MOVED, "{e}");
        assert_eq!(
            e.message,
            format!("{} changed since it was shown", shown.r#ref)
        );
        assert_eq!(e.fix, "show the person the fresh card and ask again");
        assert_eq!(e.corrected, Some(card_json(&fresh)));
        assert_eq!(log_end(&r), end, "zero new frames");
        assert!(internal_views(&r).is_empty());
    }
    // the cause moves: the old ref is gone, and the fresh card is the new ref's
    change(&mut r, "x2", &p.w.a, "A holds 90 days.");
    let newer = the_card(&r, &p.w.a);
    assert_ne!(newer.r#ref, shown.r#ref);
    let end = log_end(&r);
    let e = answer(&mut r, &shown.r#ref, "o1", None, Some(&newer.card_hash)).unwrap_err();
    assert_eq!(e.code, E_ITEM_MOVED, "{e}");
    assert_eq!(e.corrected, Some(card_json(&newer)));
    assert_eq!(log_end(&r), end, "zero new frames");
}

/// `Keep them suspect`: one KeepSuspect ANSWER with the card's hash, the note and the member
/// versions as targets; no plan, change or landing. The suspects stay, with the person's
/// deferral, and a reindex rebuilds it from the answer.
#[test]
fn cp16_keep_suspect_records_one_answer_and_survives_reindex() {
    let (_d, mut r) = project();
    let p = pair(&mut r);
    let c = the_card(&r, &p.w.a);
    let v = answer(
        &mut r,
        &c.r#ref,
        "o2",
        Some("  Wait for the audit.  "),
        None,
    )
    .unwrap();
    assert_eq!(
        (
            v["act"].as_str(),
            v["assurance"].as_str(),
            v["label"].as_str()
        ),
        (Some("keep_suspect"), Some("local"), Some(KEEP_LABEL))
    );
    assert!(v["landing"].is_null(), "{v}");
    let kinds_written: Vec<u8> = r.store().last_batch().iter().map(|(k, _)| *k).collect();
    for k in [
        kinds::PLAN,
        kinds::CHANGE,
        kinds::LANDING,
        kinds::PROPOSAL,
        kinds::VIEW_OP,
    ] {
        assert!(!kinds_written.contains(&k), "kind {k}: {kinds_written:?}");
    }
    assert_eq!(
        kinds_written
            .iter()
            .filter(|k| **k == kinds::ANSWER)
            .count(),
        1
    );
    let (h, a) = r.read().reader().answer_of(&c.r#ref).unwrap().unwrap();
    assert_eq!(v["answer"], h.to_string());
    let mut targets = vec![version(&r, &p.d1), version(&r, &p.d2)];
    targets.sort();
    assert_eq!(
        a,
        Answer {
            item: c.r#ref.clone(),
            act: AnswerAct::KeepSuspect,
            option: Some("o2".into()),
            label: Some(KEEP_LABEL.into()),
            options: vec![HOLD_LABEL.into(), KEEP_LABEL.into()],
            card_hash: Some(c.card_hash.parse().unwrap()),
            targets,
            person: a.person.clone(),
            relayed_by: None,
            relay_evidence: None,
            evidence_kind: None,
            note: Some("Wait for the audit.".into()),
        }
    );
    assert_eq!(a.person.actor, PERSON);
    let check = |r: &Rdy| {
        for d in [&p.d1, &p.d2] {
            let rows = suspects_of(r, d);
            assert_eq!(rows.len(), 1, "{rows:#?}");
            let def = rows[0].deferred.as_ref().expect("deferred");
            assert_eq!(
                (
                    def.by.actor.as_str(),
                    def.reason.as_str(),
                    def.landing,
                    def.accepted_by.as_str()
                ),
                (PERSON, "Wait for the audit.", None, PERSON)
            );
        }
        assert!(card(r, &p.w.a).is_none(), "the card is answered");
    };
    check(&r);
    r.reindex().unwrap();
    check(&r);
    r.fsck().unwrap();
}

/// `They still hold`, local: one batch holds the fresh view's Create op before its change, the
/// proposal, its bound plan, the landing, the seal and the view's Close op after the seal. The
/// landing records the person and the card; only the members' metadata changes, and other
/// views' work stays as it was.
#[test]
fn cp16_local_answer_lands_only_the_card_members() {
    let (_d, mut r) = project();
    let p = pair(&mut r);
    // unrelated work: a committed change in view u, and an open proposal of view o
    write(
        &mut r,
        "u",
        json!([{"op": "update", "id": p.w.z, "body": "Z stands, u says."}]),
    )
    .unwrap();
    write(
        &mut r,
        "o",
        json!([{"op": "update", "id": p.w.c, "body": "C stands, o says."}]),
    )
    .unwrap();
    let open = r.propose(&writer(), "o", "o", None).unwrap();
    let (u_before, o_before) = (r.read().included("u"), r.read().included("o"));
    let canon_before = r.read().materialized(&Scope::Canon).unwrap();
    let c = the_card(&r, &p.w.a);
    let v = answer(&mut r, &c.r#ref, "o1", None, Some(&c.card_hash)).unwrap();
    assert_eq!(
        (
            v["act"].as_str(),
            v["assurance"].as_str(),
            v["option"].as_str()
        ),
        (Some("reaffirm"), Some("local"), Some("o1"))
    );
    assert!(v["answer"].is_null(), "the landing records the answer: {v}");
    let landing: Hash = v["landing"]["landing"].as_str().unwrap().parse().unwrap();
    assert_eq!(r.read().canon_head(), Some(landing));
    // one batch, in order
    let batch = r.store().last_batch().to_vec();
    let view = format!("reaffirm-{}", &c.card_hash[..12]);
    let at = |pred: &dyn Fn(u8, &Hash) -> bool| {
        batch
            .iter()
            .position(|(k, h)| pred(*k, h))
            .unwrap_or_else(|| panic!("{batch:?}"))
    };
    let op_of = |h: &Hash| r.store().view_op(h).unwrap();
    let create = at(&|k, h| {
        k == kinds::VIEW_OP
            && op_of(h).view == view
            && matches!(op_of(h).op, ViewOpKind::Create { .. })
    });
    let change_at = at(&|k, _| k == kinds::CHANGE);
    let proposal = at(&|k, _| k == kinds::PROPOSAL);
    let plan = |h: &Hash| r.read().reader().plan(h).unwrap();
    let bound = at(&|k, h| k == kinds::PLAN && plan(h).seal.is_none());
    let landing_at = at(&|k, h| k == kinds::LANDING && *h == landing);
    let seal = at(&|k, h| k == kinds::PLAN && plan(h).seal.is_some());
    let close = at(&|k, h| {
        k == kinds::VIEW_OP
            && op_of(h).view == view
            && matches!(op_of(h).op, ViewOpKind::Close { .. })
    });
    assert!(
        create < change_at
            && change_at < proposal
            && proposal < bound
            && bound < landing_at
            && landing_at < seal
            && seal < close,
        "{batch:?}"
    );
    assert_eq!(
        batch.iter().filter(|(k, _)| *k == kinds::CHANGE).count(),
        1,
        "one change: the members'"
    );
    assert!(!batch.iter().any(|(k, _)| *k == kinds::ANSWER), "no ANSWER");
    // the landing: the person, local, the card as evidence
    let rec: LandingRecord = r.store().landing(&landing).unwrap();
    assert_eq!(rec.approved_by.actor, PERSON);
    assert_eq!(
        (rec.assurance, rec.relayed_by.as_ref()),
        (Assurance::Local, None)
    );
    assert_eq!(
        rec.evidence,
        Some(ApprovalEvidence {
            kind: EvidenceKind::Card,
            options: vec![HOLD_LABEL.into(), KEEP_LABEL.into()],
            choice: Some(HOLD_LABEL.into()),
            card_hash: Some(c.card_hash.parse().unwrap()),
        })
    );
    let prop = r.store().proposal(&rec.proposal).unwrap();
    assert_eq!(
        (
            prop.view.as_str(),
            prop.title.as_str(),
            prop.summary.as_deref()
        ),
        (view.as_str(), c.title.as_str(), c.summary.as_deref())
    );
    // the change: the members only, metadata and the cause's pin only
    let ch: Change = r.store().change(&rec.changes[0]).unwrap();
    assert_eq!(rec.changes.len(), 1);
    let mut written: Vec<String> = ch.ops.iter().map(|o| o.block_id().to_string()).collect();
    written.sort();
    assert_eq!(written, sorted(vec![p.d1.clone(), p.d2.clone()]));
    for op in &ch.ops {
        let Op::Update { base, block } = op else {
            panic!("{op:?}")
        };
        let old = canon_before.get(&block.id).unwrap();
        assert_eq!(*base, old.version_hash());
        assert!(rdy_model::decision::only_reaffirmation_changed(old, block));
        let rec = decode_reaffirmed(block.fields.get(REAFFIRMED).unwrap()).unwrap();
        assert_eq!(rec.len(), 1);
        assert_eq!(rec[&id(&p.w.a)].against, Against::Retiring(vec![]));
    }
    // the view is private and closed; the suspects are gone, and so is the card
    assert_eq!(internal_views(&r), vec![format!("view/{view}")]);
    assert!(r.read().view_head(&view).unwrap().closed);
    assert!(suspects_of(&r, &p.d1).is_empty() && suspects_of(&r, &p.d2).is_empty());
    assert!(card(&r, &p.w.a).is_none());
    // the seal names every member and seals each cause/member item as re-affirmed
    let s = seal_of(&r, &p.d1);
    let sealed = s.seal.as_ref().unwrap();
    assert_eq!(sealed.landing, landing);
    assert_eq!(
        sealed.decisions,
        vec![id(&p.d1), id(&p.d2)]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );
    assert_eq!(sealed.items.len(), 2);
    for i in &sealed.items {
        assert_eq!(
            (i.kind, i.outcome, &i.cause, i.answer),
            (
                PlanKind::Reaffirm,
                PlanOutcome::Reaffirmed,
                &id(&p.w.a),
                None
            )
        );
    }
    // other views' work is unchanged
    assert_eq!(r.read().included("u"), u_before);
    assert_eq!(r.read().included("o"), o_before);
    assert_eq!(
        r.resolve_proposal(&open.r#ref).unwrap().version.to_string(),
        open.id
    );
    r.fsck().unwrap();
}

/// A relayed answer records the person as approver, the relaying agent and its words, and the
/// card; a relayed `Keep them suspect` records the relay and card evidence.
#[test]
fn cp16_relayed_answer_records_person_and_card() {
    let (d, mut r) = project();
    let p = pair(&mut r);
    let other = decide(&mut r, &p.w.s, "Three rule", None, &[&p.w.c]);
    change(&mut r, "xc", &p.w.c, "C stands firm.");
    let (ra, rc) = (the_card(&r, &p.w.a).r#ref, the_card(&r, &p.w.c).r#ref);
    let person = rdy_engine::human_actor_for_env(d.path());
    let mut st = agent_state(r);
    let card = mcp_card(&mut st, &ra);
    let v = mcp_answer(&mut st, &card, "o1", None).unwrap();
    assert_eq!(
        (v["act"].as_str(), v["assurance"].as_str()),
        (Some("reaffirm"), Some("relayed")),
        "{v}"
    );
    let landing: Hash = v["landing"]["landing"].as_str().unwrap().parse().unwrap();
    let card_c = mcp_card(&mut st, &rc);
    let k = mcp_answer(&mut st, &card_c, "o2", Some("Ask the owner first.")).unwrap();
    assert_eq!(k["act"], "keep_suspect", "{k}");
    let e = st.engine.clone();
    on_engine(&*e, move |r| {
        let rec = r.store().landing(&landing).unwrap();
        assert_eq!(rec.assurance, Assurance::Relayed);
        assert_eq!(rec.approved_by.actor, person);
        let by = rec.relayed_by.as_ref().unwrap();
        assert_eq!(
            (by.actor.as_str(), by.run.as_deref()),
            ("agent:relay", Some("r1"))
        );
        assert_eq!(
            rec.relay_evidence.as_ref().unwrap().quoted,
            "Yes, they hold."
        );
        let e = rec.evidence.unwrap();
        assert_eq!(
            (
                e.kind,
                e.choice.as_deref(),
                e.card_hash.map(|h| h.to_string())
            ),
            (
                EvidenceKind::Card,
                Some(HOLD_LABEL),
                card["card_hash"].as_str().map(str::to_string)
            )
        );
        assert_eq!(
            e.options,
            vec![HOLD_LABEL.to_string(), KEEP_LABEL.to_string()]
        );
        let (_, a) = r.read().reader().answer_of(&rc).unwrap().unwrap();
        assert_eq!(a.act, AnswerAct::KeepSuspect);
        assert_eq!(a.person.actor, person);
        assert_eq!(a.evidence_kind, Some(EvidenceKind::Card));
        assert_eq!(a.relayed_by.as_ref().unwrap().actor, "agent:relay");
        assert_eq!(
            a.card_hash.map(|h| h.to_string()).as_deref(),
            card_c["card_hash"].as_str()
        );
        assert_eq!(a.targets, vec![version(r, &other)]);
        r.fsck().unwrap();
    });
}

/// The answer is the approval: a one_way member lands with no second approval and leaves no
/// exception behind.
#[test]
fn cp16_one_way_member_needs_no_second_approval() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    let one = decide_in(
        &mut r,
        "d1",
        &w.s,
        "Never drop A",
        Some("Keep A"),
        &[&w.a],
        "one_way",
    );
    land(&mut r, "d1").unwrap();
    change(&mut r, "x", &w.a, "A holds 60 days.");
    let c = the_card(&r, &w.a);
    assert_eq!(members(&c), vec![one.clone()]);
    let v = answer(&mut r, &c.r#ref, "o1", None, Some(&c.card_hash)).unwrap();
    assert_eq!(v["act"], "reaffirm", "{v}");
    assert!(suspects_of(&r, &one).is_empty());
    let q = queue(&r);
    assert!(q.items.is_empty(), "{:#?}", q.items);
    assert!(q.not_ready.is_empty(), "{:#?}", q.not_ready);
}

/// With no note the reason is the option's label; a note is the reason.
#[test]
fn cp16_absent_note_uses_the_option_label() {
    let (_d, mut r) = project();
    let p = pair(&mut r);
    let d3 = decide(&mut r, &p.w.s, "Three rule", None, &[&p.w.c]);
    change(&mut r, "xc", &p.w.c, "C stands firm.");
    let a = the_card(&r, &p.w.a);
    answer(&mut r, &a.r#ref, "o1", None, None).unwrap();
    for d in [&p.d1, &p.d2] {
        assert_eq!(
            reaffirmed_reason(&r, d, &p.w.a).as_deref(),
            Some("They still hold")
        );
    }
    let c = the_card(&r, &p.w.c);
    answer(&mut r, &c.r#ref, "o1", Some("  C still fits.  "), None).unwrap();
    assert_eq!(
        reaffirmed_reason(&r, &d3, &p.w.c).as_deref(),
        Some("C still fits.")
    );
}

/// `reaffirm-` names are private: a create, a fork, a session, a write and a view op are
/// refused with the exact text; only the card's private writer creates one, never over an
/// existing view, and a stale answer is `E_ITEM_MOVED` before that check.
#[test]
fn cp16_reserved_views_are_private_on_every_entry() {
    let (_d, mut r) = project();
    let p = pair(&mut r);
    let reserved = |e: RdyError| {
        assert_eq!(e.code, E_RESERVED_VIEW, "{e}");
        assert_eq!(
            e.message,
            "view names that start with reaffirm- are reserved for re-affirmation cards"
        );
        assert_eq!(e.fix, "pick a view name that does not start with reaffirm-");
    };
    let end = log_end(&r);
    reserved(
        r.view_create(&writer(), "reaffirm-x", None, false)
            .unwrap_err(),
    );
    reserved(r.view_fork(&writer(), "x", "reaffirm-x").unwrap_err());
    reserved(
        write(
            &mut r,
            "reaffirm-x",
            json!([{"op": "update", "id": p.w.z, "body": "Z."}]),
        )
        .unwrap_err(),
    );
    reserved(r.view_include(&writer(), "reaffirm-x", &[]).unwrap_err());
    reserved(
        Session::agent("agent:a".into(), Some("reaffirm-x".into()), Role::Writer).unwrap_err(),
    );
    assert_eq!(log_end(&r), end);
    // the MCP session
    let c = the_card(&r, &p.w.a);
    let mut st = agent_state(r);
    reserved(
        rdy_session(
            &mut st,
            serde_json::from_value(
                json!({"actor": "agent:a", "view": "reaffirm-x", "role": "writer"}),
            )
            .unwrap(),
        )
        .unwrap_err(),
    );
    reserved(
        rdy_session(
            &mut st,
            serde_json::from_value(
                json!({"actor": "agent:a", "run": "reaffirm-y", "role": "writer"}),
            )
            .unwrap(),
        )
        .unwrap_err(),
    );
    // an internal view of the card's name that exists already is never reused
    let e = st.engine.clone();
    let view = format!("reaffirm-{}", &c.card_hash[..12]);
    let (ref_, hash) = (c.r#ref.clone(), c.card_hash.clone());
    on_engine(&*e, move |r| {
        let by = writer().provenance();
        let base = r.read().canon_head();
        r.store_mut()
            .view_create(&view, base, None, false, &by)
            .unwrap();
        r.invalidate();
        let end = log_end(r);
        // a stale card is moved first
        let stale = answer(r, &ref_, "o1", None, Some(&Hash::of(b"x").to_string())).unwrap_err();
        assert_eq!(stale.code, E_ITEM_MOVED, "{stale}");
        let e = answer(r, &ref_, "o1", None, Some(&hash)).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{e}");
        assert_eq!(
            e.message,
            format!("reserved re-affirmation view already exists: {view}")
        );
        assert_eq!(e.fix, "read the fresh re-affirmation card and answer it");
        assert_eq!(log_end(r), end, "nothing is written");
        assert_eq!(
            r.store().view_op_ids(&view).unwrap().len(),
            1,
            "its log is kept"
        );
    });
}

/// An agent whose first signature waits for `hook`, which runs with the writer free.
struct Racing {
    inner: QaAgent,
    hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl SigningAgent for Racing {
    fn identities(&self) -> Result<Vec<AgentIdentity>, RdyError> {
        self.inner.identities()
    }

    fn sign(&self, key: &[u8], payload: &[u8]) -> Result<Vec<u8>, RdyError> {
        let hook = self.hook.lock().unwrap().take();
        if let Some(h) = hook {
            h();
        }
        self.inner.sign(key, payload)
    }
}

fn sign_always(dir: &Path) {
    std::fs::write(
        dir.join(".rdy/local.yaml"),
        format!(
            "signing:\n  key: \"{}\"\n  agent_key: null\n  sign: always\n  allowed_signers: null\n",
            QaAgent::fingerprint(TestKey::Person)
        ),
    )
    .unwrap();
}

/// A failed append, or a project that moves while the agent signs, leaves no internal view,
/// ref, proposal or landing: the answer is prepared again from the card, or refused as moved.
#[test]
fn cp16_failure_and_signing_race_leave_no_internal_view() {
    let (d, mut r) = project();
    let p = pair(&mut r);
    let c = the_card(&r, &p.w.a);
    // an append that fails at the fourth frame of the batch
    let (end, head) = (log_end(&r), r.read().canon_head());
    r.store_mut().fail_append_at_frame_for_tests(Some((3, 1)));
    answer(&mut r, &c.r#ref, "o1", None, Some(&c.card_hash)).unwrap_err();
    assert_eq!((log_end(&r), r.read().canon_head()), (end, head));
    assert!(internal_views(&r).is_empty());
    assert!(
        r.store()
            .refs_with_prefix("plan/reaffirm-")
            .unwrap()
            .is_empty()
    );
    assert!(
        r.store()
            .refs_with_prefix("open/reaffirm-")
            .unwrap()
            .is_empty()
    );
    assert_eq!(the_card(&r, &p.w.a), c, "the card stands as it was");

    // a race that moves the log but not the card: the act is prepared and signed again
    sign_always(d.path());
    let e = Arc::new(Local::new(r));
    let racer = Arc::clone(&e);
    let z = p.w.z.clone();
    let agent = Racing {
        inner: QaAgent::new(),
        hook: Mutex::new(Some(Box::new(move || {
            on_engine(&*racer, move |r| {
                write(
                    r,
                    "u",
                    json!([{"op": "update", "id": z, "body": "Z moves."}]),
                )
                .unwrap();
            });
        }))),
    };
    let req = |hash: &str| {
        SigningAct::Answer(AnswerRequest {
            r#ref: c.r#ref.clone(),
            option: "o1".into(),
            card_hash: Some(hash.to_string()),
            note: None,
            relay: RelayInput::default(),
            brief: None,
        })
    };
    let v = run_signed_act(&*e, &agent, &approver(), PERSON, req(&c.card_hash)).unwrap();
    assert_eq!(v["act"], "reaffirm", "{v}");
    assert_eq!(v["signatures"].as_array().map(Vec::len), Some(1), "{v}");
    let views = on_engine(&*e, |r| internal_views(r));
    assert_eq!(views, vec![format!("view/reaffirm-{}", &c.card_hash[..12])]);

    // a race that moves the card: the retry is refused as moved, and nothing is written
    let (d3, a, cv) = on_engine(&*e, {
        let w = p.w.s.clone();
        let a = p.w.a.clone();
        move |r| {
            // two more members, so the card stays once d3 is re-affirmed
            let d3 = decide(r, &w, "Three rule", None, &[&a]);
            decide(r, &w, "Four rule", None, &[&a]);
            change(r, "x9", &a, "A holds 120 days.");
            let cv = the_card(r, &a);
            (d3, a, cv)
        }
    });
    let racer = Arc::clone(&e);
    let agent = Racing {
        inner: QaAgent::new(),
        hook: Mutex::new(Some(Box::new(move || {
            on_engine(&*racer, move |r| reaffirm_landed(r, "h3", &d3, &a));
        }))),
    };
    let req = SigningAct::Answer(AnswerRequest {
        r#ref: cv.r#ref.clone(),
        option: "o1".into(),
        card_hash: Some(cv.card_hash.clone()),
        ..AnswerRequest::default()
    });
    let err = run_signed_act(&*e, &agent, &approver(), PERSON, req).unwrap_err();
    assert_eq!(err.code, E_ITEM_MOVED, "{err}");
    let views = on_engine(&*e, |r| internal_views(r));
    assert_eq!(views.len(), 1, "only the first card's view: {views:?}");
    on_engine(&*e, |r| {
        r.fsck().unwrap();
    });
}

/// The bounds of a card and the notes of its answers, exact.
#[test]
fn cp16_card_bounds_and_note_errors_are_exact() {
    let limit = |e: RdyError| {
        assert_eq!(e.code, E_LIMIT, "{e}");
        assert_eq!(e.message, "re-affirmation card exceeds its record limit");
        assert_eq!(
            e.fix,
            "re-affirm smaller groups with rdy_write / rdy reaffirm"
        );
    };
    assert_eq!(
        (MAX_CARD_MEMBERS, MAX_CARD_PAIRS, MAX_CARD_BYTES),
        (100_000, 100_000, 16 << 20)
    );
    check_card_bounds(100_000, 100_000, 16 << 20).unwrap();
    limit(check_card_bounds(100_001, 1, 1).unwrap_err());
    limit(check_card_bounds(1, 100_001, 1).unwrap_err());
    limit(check_card_bounds(1, 1, (16 << 20) + 1).unwrap_err());
    // the byte count is the binding's postcard encoding
    let members = vec![
        ReaffirmMember {
            id: id("D-00001"),
            version: Hash::of(b"1"),
            suspects: vec![ReaffirmSuspect {
                target: id("c-00001"),
                reason: "changed".into(),
            }],
        },
        ReaffirmMember {
            id: id("D-00002"),
            version: Hash::of(b"2"),
            suspects: vec![],
        },
    ];
    let binding = PlanCardBinding::Reaffirm {
        cause: id("c-00001"),
        version: Hash::of(b"c"),
        retirements: vec![id("D-00009")],
        members: members.clone(),
    };
    let mut buf = vec![0u8; 4096];
    let encoded = rdy_model::redaction::encode_into(&binding, &mut buf)
        .unwrap()
        .len();
    assert_eq!(
        reaffirm_binding_len(&id("c-00001"), &[id("D-00009")], &members),
        encoded
    );

    let (_d, mut r) = project();
    let p = pair(&mut r);
    let c = the_card(&r, &p.w.a);
    let end = log_end(&r);
    let long = "n".repeat(2001);
    for note in [None, Some("   "), Some(long.as_str())] {
        let e = answer(&mut r, &c.r#ref, "o2", note, None).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{e}");
        assert_eq!(e.message, "Keep them suspect needs a note");
        assert_eq!(e.fix, "pass note with a reason of 1 to 2000 characters");
    }
    for note in ["", "   ", long.as_str()] {
        let e = answer(&mut r, &c.r#ref, "o1", Some(note), None).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{e}");
        assert_eq!(
            e.message,
            "a re-affirmation needs a reason of 1 to 2000 characters"
        );
        assert_eq!(e.fix, "give a reason of 1 to 2000 characters");
    }
    // a list of recommended answers that names the card is refused whole
    let e = r
        .answer_all_recommended(
            &approver(),
            PERSON,
            &[c.r#ref.clone()],
            &[],
            RelayInput::default(),
        )
        .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_BAD_INPUT,
            format!("{} has no recommended option", c.r#ref).as_str(),
            "answer it on its own"
        )
    );
    assert_eq!(log_end(&r), end, "a refusal writes nothing");
    // 2000 characters is a note
    let most = "m".repeat(2000);
    answer(&mut r, &c.r#ref, "o2", Some(&most), None).unwrap();
}

/// The chat path and the command line seal the same work: each lands the card's members alone,
/// and the seal of each names them and their re-affirmed items.
#[test]
fn cp16_end_to_end_mcp_and_cli_answer_seal_the_same_work() {
    let check = |r: &Rdy, p: &Pair, landing: &str| {
        let s = seal_of(r, &p.d1);
        let sealed = s.seal.as_ref().unwrap();
        assert_eq!(sealed.landing.to_string(), landing);
        assert_eq!(
            sealed
                .decisions
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            sorted(vec![p.d1.clone(), p.d2.clone()])
        );
        let mut subjects: Vec<&str> = sealed.items.iter().map(|i| i.subject.as_str()).collect();
        subjects.sort();
        assert_eq!(subjects, sorted(vec![p.d1.clone(), p.d2.clone()]));
        assert!(
            sealed
                .items
                .iter()
                .all(|i| i.outcome == PlanOutcome::Reaffirmed && i.cause == id(&p.w.a))
        );
        assert_eq!(seal_of(r, &p.d2), s);
        assert!(suspects_of(r, &p.d1).is_empty() && suspects_of(r, &p.d2).is_empty());
    };
    // the chat path: a relayed answer over MCP
    let (_d, mut r) = project();
    let p = pair(&mut r);
    let ra = the_card(&r, &p.w.a).r#ref;
    let mut st = agent_state(r);
    let card = mcp_card(&mut st, &ra);
    let v = mcp_answer(&mut st, &card, "o1", None).unwrap();
    let landing = v["landing"]["landing"].as_str().unwrap().to_string();
    on_engine(&*st.engine.clone(), move |r| check(r, &p, &landing));

    // the command line, as the person
    let srv = Served::new();
    let (p, ra) = {
        srv.stop();
        let mut r = Rdy::open(srv.dir.path()).unwrap();
        let p = pair(&mut r);
        let ra = the_card(&r, &p.w.a).r#ref;
        r.close().unwrap();
        (p, ra)
    };
    let (ok, v) = srv.json(&["--json", "answer", &ra, "o1"]);
    assert!(ok, "{v}");
    assert_eq!(
        (v["act"].as_str(), v["assurance"].as_str()),
        (Some("reaffirm"), Some("local")),
        "{v}"
    );
    let landing = v["landing"]["landing"].as_str().unwrap().to_string();
    srv.stop();
    let r = Rdy::open(srv.dir.path()).unwrap();
    check(&r, &p, &landing);
    r.fsck().unwrap();
    r.close().unwrap();
}
