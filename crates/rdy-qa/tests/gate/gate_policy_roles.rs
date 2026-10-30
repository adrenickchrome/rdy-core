//! ap12 (D-TF78H, D-ZWP7Q; M3c O10, O11): a person's queue shows the items whose required policy
//! role holds that person, and every item when no role applies; a project with no policy sees
//! everything. An approval or another answer from a person outside the item's required policy
//! role is refused with `E_PERMISSION`, local or relayed, on every approval path (`rdy approve`,
//! `rdy_answer`, `all_recommended`). A Later answer is never refused.
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::engine::Local;
use rdy_engine::pending::{CARD_MAX_TOKENS, Item, ItemKind, Pending, PendingQuery};
use rdy_engine::plan::{PlanAnswerInput, ProposePlanRequest};
use rdy_engine::{
    DecisionInput, E_PERMISSION, LandOptions, ProposalInfo, Rdy, RdyError, Session, WriteRequest,
};
use rdy_mcp::tools::{McpState, rdy_answer, rdy_session};
use rdy_model::plan::{PlanAct, PlanKind, item_display, item_hash};
use rdy_model::{BlockId, Justification, Role};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{approver, land, parse_test_ops, project, write, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY, confirm_exceptions};
use rdy_qa::tensions::ag;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::sync::Arc;

/// The person a policy role names; [`PERSON`] is not in it.
const BOSS: &str = "human:boss";

fn boss() -> Session {
    Session::human(BOSS.into(), Role::Approver).unwrap()
}

fn person(actor: &str) -> Session {
    Session::human(actor.into(), Role::Approver).unwrap()
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// The refusal text of D-TF78H for `who` outside `role` on item `r`.
fn outside(e: &RdyError, who: &str, role: &str, r: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_PERMISSION,
            format!("{who} is not in the policy role {role} for {r}").as_str(),
            format!("ask a person in the policy role {role}, or relay their answer").as_str(),
        ),
        "{e}"
    );
}

// ---- the project ----

/// Canon: doc `spec`, section `Rules`, claims a ("A holds 30 days."), b and q, which reference a,
/// and c, which nothing references.
struct World {
    s: String,
    a: String,
    c: String,
    b: String,
    q: String,
}

fn world(r: &mut Rdy) -> World {
    let w = write(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}}."},
            {"op": "create", "type": "claim", "id": "$q", "parent": "$s", "body": "Q builds on {{$a}}."},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C stands."}
        ]),
    )
    .unwrap();
    land(r, "seed").unwrap();
    let get = |k: &str| w.created[k].clone();
    World {
        s: get("$s"),
        a: get("$a"),
        b: get("$b"),
        q: get("$q"),
        c: get("$c"),
    }
}

/// An authority policy with no agent rule, the roles `roles`, the entries `require` and the owner
/// entries `owners`.
fn policy(roles: Value, require: Value, owners: Value) -> Value {
    json!({
        "key": "authority-policy",
        "agent_may_accept": [],
        "roles": roles,
        "require": require,
        "owners": owners
    })
}

/// Lands a doc `rules` with a decision that adopts the policy and the policy `fields`, whose
/// change implements that decision (O19), as the person (no policy is live yet).
fn land_policy(r: &mut Rdy, fields: Value) {
    let w = write(
        r,
        "pol",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$d", "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}}
        ]),
    )
    .unwrap_or_else(|e| panic!("write the rules: {e}"));
    r.write(
        &writer(),
        WriteRequest {
            view: "pol".into(),
            ops: parse_test_ops(json!([
                {"op": "create", "type": "authority_policy", "id": "$p", "parent": w.created["$d"], "fields": fields}
            ]))
            .unwrap(),
            justification: Some(Justification::Implements(vec![id(&w.created["$dec"])])),
            message: String::new(),
        },
    )
    .unwrap_or_else(|e| panic!("write the policy: {e}"));
    land(r, "pol").unwrap_or_else(|e| panic!("land the policy: {e}"));
}

/// A new body for `target` in `view`, as the writer.
fn edit(r: &mut Rdy, view: &str, target: &str, body: &str) {
    write(
        r,
        view,
        json!([{"op": "update", "id": target, "body": body}]),
    )
    .unwrap_or_else(|e| panic!("edit {target} in {view}: {e}"));
}

/// Proposes `view` as the writer with a summary and `answers`.
fn propose(r: &mut Rdy, view: &str, answers: &[PlanAnswerInput]) -> ProposalInfo {
    let p = r
        .propose_plan(
            &writer(),
            ProposePlanRequest {
                summary: Some(TEST_SUMMARY),
                plan_answers: answers,
                ..ProposePlanRequest::new(view, view)
            },
        )
        .unwrap_or_else(|e| panic!("propose {view}: {e}"));
    assert!(p.ready, "{view}: {:#?}", p.issues);
    p
}

/// Proposes an edit of claim c in view `view`: a version that writes no decision.
fn edit_version(r: &mut Rdy, w: &World, view: &str) -> ProposalInfo {
    edit(r, view, &w.c, &format!("C stands, per {view}."));
    propose(r, view, &[])
}

/// Proposes a `two_way` decision under section `Rules` in view `view`.
fn decision_version(r: &mut Rdy, w: &World, view: &str) -> ProposalInfo {
    r.decide(
        &writer(),
        view,
        DecisionInput {
            parent: w.s.clone(),
            statement: format!("Adopt {view}"),
            reversibility: Some("two_way".into()),
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("decide in {view}: {e}"));
    propose(r, view, &[])
}

fn pi(cause: &str, subject: &str, kind: PlanKind) -> String {
    item_display(&item_hash(&id(cause), subject, kind))
}

fn defer(item: &str, reason: &str) -> PlanAnswerInput {
    PlanAnswerInput {
        item: item.into(),
        act: PlanAct::Defer,
        reason: Some(reason.into()),
    }
}

/// Proposes an edit of claim a in view `w`, whose plan defers the confirm items of b and q: a
/// version with a deferral card.
fn deferred(r: &mut Rdy, w: &World) -> ProposalInfo {
    edit(r, "w", &w.a, "A holds 60 days.");
    propose(
        r,
        "w",
        &[
            defer(&pi(&w.a, &w.b, PlanKind::Confirm), "b waits for the review"),
            defer(
                &pi(&w.a, &w.q, PlanKind::Confirm),
                "q waits for the rewrite",
            ),
        ],
    )
}

/// The queue as `who` sees it: every card whole, no session.
fn queue_for(r: &Rdy, who: &str) -> Pending {
    r.read()
        .pending(&PendingQuery {
            person: who.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .expect("pending")
}

fn refs(q: &Pending) -> Vec<String> {
    q.items.iter().map(|i| i.r#ref.clone()).collect()
}

fn item_for(r: &Rdy, who: &str, r#ref: &str) -> Item {
    queue_for(r, who)
        .items
        .into_iter()
        .find(|i| i.r#ref == r#ref)
        .unwrap_or_else(|| panic!("no item {ref} for {who}"))
}

fn log_end(r: &Rdy) -> Location {
    r.store().log_end()
}

/// The landing of `pref` by `s`, refused, writing nothing.
fn land_err(r: &mut Rdy, s: &Session, pref: &str) -> RdyError {
    let before = log_end(r);
    let e = r.land_with(s, pref, LandOptions::default()).unwrap_err();
    assert_eq!(log_end(r), before, "a refused landing writes nothing");
    e
}

/// The answer of `option` to `item`, relayed by agent `relay` for `who` with the card's hash.
fn relayed(r: &mut Rdy, who: &str, item: &Item, option: &str) -> Result<Value, RdyError> {
    r.answer(
        &ag("relay"),
        who,
        AnswerRequest {
            r#ref: item.r#ref.clone(),
            option: option.into(),
            card_hash: Some(item.card_hash.clone()),
            relay: RelayInput {
                evidence: Some("Yes, go ahead.".into()),
                question: Some("Shall I?".into()),
                session_ref: Some("chat-7".into()),
                ..RelayInput::default()
            },
            ..AnswerRequest::default()
        },
    )
    .map(|i| serde_json::to_value(i).unwrap())
}

/// The policy that needs a person from `approvers`, of members `members`, for each item that
/// matches `when`.
fn one_role(when: &str, members: &[&str]) -> Value {
    policy(
        json!({"approvers": members}),
        json!([{"when": when, "role": "approvers"}]),
        json!([]),
    )
}

// ---- tests ----

/// Guard: with no policy, every person sees every item, and any person approves.
#[test]
fn ap12_with_no_policy_every_item_shows_and_any_person_approves() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    let e1 = edit_version(&mut r, &w, "e1");
    let d1 = decision_version(&mut r, &w, "d1");
    let qa = queue_for(&r, PERSON);
    let bob = queue_for(&r, "human:bob");
    assert_eq!(refs(&qa), refs(&bob));
    for p in [&e1, &d1] {
        assert!(refs(&qa).contains(&p.r#ref), "{:#?}", refs(&qa));
    }
    let counts = r
        .read()
        .pending_counts(&PendingQuery {
            person: "human:bob".into(),
            ..PendingQuery::default()
        })
        .unwrap();
    assert_eq!(counts.items, bob.items.len());
    let bob_s = person("human:bob");
    confirm_exceptions(&mut r, &bob_s, &d1.r#ref);
    r.land_with(&bob_s, &d1.r#ref, LandOptions::default())
        .expect("any person approves with no policy");
    r.land_with(&bob_s, &e1.r#ref, LandOptions::default())
        .expect("any person approves with no policy");
}

/// D-TF78H: the queue of a person outside an item's required role leaves the item out; the
/// person in the role sees it. A version that writes no decision needs no role here.
#[test]
fn ap12_pending_hides_an_item_whose_role_does_not_hold_the_person() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    land_policy(&mut r, one_role("decision", &[BOSS]));
    let e1 = edit_version(&mut r, &w, "e1");
    let d1 = decision_version(&mut r, &w, "d1");
    let qa = queue_for(&r, PERSON);
    assert!(refs(&qa).contains(&e1.r#ref), "{:#?}", refs(&qa));
    assert!(
        qa.items
            .iter()
            .all(|i| i.r#ref != d1.r#ref && i.batch.as_deref() != Some(d1.r#ref.as_str())),
        "{:#?}",
        refs(&qa)
    );
    assert_eq!(qa.snoozed, 0, "a role hides; it does not snooze");
    let boss_q = queue_for(&r, BOSS);
    for p in [&e1, &d1] {
        assert!(refs(&boss_q).contains(&p.r#ref), "{:#?}", refs(&boss_q));
    }
    // the decision version's own items need the role too
    assert!(boss_q.items.len() > qa.items.len());
}

/// The counts a session summary shows are the routed queue's.
#[test]
fn ap12_pending_counts_match_the_routed_items() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    land_policy(&mut r, one_role("decision", &[BOSS]));
    edit_version(&mut r, &w, "e1");
    decision_version(&mut r, &w, "d1");
    for who in [PERSON, BOSS] {
        let q = queue_for(&r, who);
        let c = r
            .read()
            .pending_counts(&PendingQuery {
                person: who.into(),
                ..PendingQuery::default()
            })
            .unwrap();
        assert_eq!(c.items, q.items.len(), "{who}: {:#?}", refs(&q));
        assert_eq!(
            c.blocking,
            q.items.iter().filter(|i| i.blocking).count(),
            "{who}"
        );
    }
    let qa = r
        .read()
        .pending_counts(&PendingQuery {
            person: PERSON.into(),
            ..PendingQuery::default()
        })
        .unwrap();
    let bs = r
        .read()
        .pending_counts(&PendingQuery {
            person: BOSS.into(),
            ..PendingQuery::default()
        })
        .unwrap();
    assert!(qa.items < bs.items, "{qa:?} {bs:?}");
}

/// D-TF78H: `rdy approve` by a person outside the approve item's required role is
/// `E_PERMISSION`, in the process and through the binary; the person in the role lands it.
#[test]
fn ap12_an_approval_outside_the_role_is_e_permission() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    land_policy(&mut r, one_role("decision", &[BOSS]));
    let d1 = decision_version(&mut r, &w, "d1");
    let e = land_err(&mut r, &approver(), &d1.r#ref);
    outside(&e, PERSON, "approvers", &d1.r#ref);
    confirm_exceptions(&mut r, &boss(), &d1.r#ref);
    r.land_with(&boss(), &d1.r#ref, LandOptions::default())
        .expect("the person in the role approves");

    // through the binary
    let srv = Served::new();
    srv.stop();
    let pref = {
        let mut r = Rdy::open(srv.dir.path()).unwrap();
        let w = world(&mut r);
        land_policy(&mut r, one_role("decision", &[BOSS]));
        let p = decision_version(&mut r, &w, "d1");
        r.close().unwrap();
        p.r#ref
    };
    let before = {
        let r = Rdy::open(srv.dir.path()).unwrap();
        let end = log_end(&r);
        r.close().unwrap();
        end
    };
    let (ok, e) = srv.json(&["approve", &pref, "--json"]);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], E_PERMISSION, "{e}");
    assert_eq!(
        e["message"],
        format!("{PERSON} is not in the policy role approvers for {pref}"),
        "{e}"
    );
    assert_eq!(
        e["fix"], "ask a person in the policy role approvers, or relay their answer",
        "{e}"
    );
    srv.stop();
    let r = Rdy::open(srv.dir.path()).unwrap();
    assert_eq!(log_end(&r), before, "a refused approval writes nothing");
    r.close().unwrap();
}

/// D-TF78H: a relayed answer to a deferral card (`when: deferral`) for a person outside the role
/// is `E_PERMISSION` and writes nothing; the same answer for the person in the role is recorded.
#[test]
fn ap12_a_relayed_answer_outside_the_role_is_e_permission() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    land_policy(
        &mut r,
        policy(
            json!({"deferrers": [BOSS]}),
            json!([{"when": "deferral", "role": "deferrers"}]),
            json!([]),
        ),
    );
    let p = deferred(&mut r, &w);
    let dr = format!("defer:{}", p.r#ref);
    assert!(!refs(&queue_for(&r, PERSON)).contains(&dr));
    assert!(refs(&queue_for(&r, PERSON)).contains(&p.r#ref));
    let card = item_for(&r, BOSS, &dr);
    assert_eq!(card.kind, ItemKind::Defer);
    let before = log_end(&r);
    let e = relayed(&mut r, PERSON, &card, "o1").unwrap_err();
    outside(&e, PERSON, "deferrers", &dr);
    assert_eq!(log_end(&r), before, "a refused answer writes nothing");
    let v = relayed(&mut r, BOSS, &card, "o1").expect("the person in the role answers");
    assert_eq!(v["act"], "accept_deferrals", "{v}");
    assert_eq!(v["assurance"], "relayed", "{v}");
}

/// D-TF78H: a relayed `rdy_answer` approval for a person outside the approve item's required
/// role is `E_PERMISSION` and writes nothing.
#[test]
fn ap12_rdy_answer_approve_outside_the_role_is_e_permission() {
    let (d, mut r) = project();
    std::fs::write(d.path().join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
    let w = world(&mut r);
    land_policy(&mut r, one_role("any", &[BOSS]));
    let p = edit_version(&mut r, &w, "e1");
    let card = item_for(&r, BOSS, &p.r#ref);
    let mut st = McpState::new(Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:relay", "run": "r1", "role": "writer"}))
            .unwrap(),
    )
    .unwrap();
    let published = st.engine.read().seq();
    let e = rdy_answer(
        &mut st,
        serde_json::from_value(json!({
            "ref": p.r#ref, "option": "approve", "card_hash": card.card_hash,
            "evidence": "Yes.", "question": "Shall I?", "session_ref": "chat-7",
        }))
        .unwrap(),
    )
    .unwrap_err();
    outside(&e, PERSON, "approvers", &p.r#ref);
    assert_eq!(
        st.engine.read().seq(),
        published,
        "a refused answer writes nothing"
    );
}

/// D-ME2HD names `all_recommended` as an approval path: an `all_recommended` answer from outside
/// the role is refused with the same text and lands nothing.
#[test]
fn ap12_all_recommended_outside_the_role_is_e_permission() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    land_policy(&mut r, one_role("any", &[BOSS]));
    let p = edit_version(&mut r, &w, "e1");
    let before = log_end(&r);
    let e = r
        .answer_all_recommended(
            &approver(),
            PERSON,
            std::slice::from_ref(&p.r#ref),
            &[],
            RelayInput::default(),
        )
        .unwrap_err();
    outside(&e, PERSON, "approvers", &p.r#ref);
    assert_eq!(e.corrected, Some(json!({"done": []})), "{e}");
    assert_eq!(log_end(&r), before, "nothing lands");
    let done = r
        .answer_all_recommended(
            &boss(),
            BOSS,
            std::slice::from_ref(&p.r#ref),
            &[],
            RelayInput::default(),
        )
        .expect("the person in the role approves");
    assert_eq!(done[0].act, "approve");
}

/// O10: a re-affirmation card matches `when: reaffirm`; it routes to the role's people, and an
/// answer from outside the role is refused.
#[test]
fn ap12_a_reaffirm_card_routes_by_when_reaffirm() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    r.decide(
        &writer(),
        "dec",
        DecisionInput {
            parent: w.s.clone(),
            statement: "A stays short".into(),
            depends_on: vec![w.a.clone()],
            reversibility: Some("two_way".into()),
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "dec").unwrap();
    land_policy(
        &mut r,
        policy(
            json!({"keepers": [BOSS]}),
            json!([{"when": "reaffirm", "role": "keepers"}]),
            json!([]),
        ),
    );
    edit(&mut r, "chg", &w.a, "A holds 45 days.");
    land(&mut r, "chg").expect("an approve item matches no reaffirm entry");
    let boss_q = queue_for(&r, BOSS);
    let card = boss_q
        .items
        .iter()
        .find(|i| i.kind == ItemKind::Reaffirm)
        .unwrap_or_else(|| panic!("no re-affirmation card: {:#?}", refs(&boss_q)))
        .clone();
    let qa = queue_for(&r, PERSON);
    assert!(
        qa.items.iter().all(|i| i.kind != ItemKind::Reaffirm),
        "{:#?}",
        refs(&qa)
    );
    let before = log_end(&r);
    let e = r
        .answer(
            &approver(),
            PERSON,
            AnswerRequest {
                r#ref: card.r#ref.clone(),
                option: "o2".into(),
                note: Some("Not now.".into()),
                ..AnswerRequest::default()
            },
        )
        .unwrap_err();
    outside(&e, PERSON, "keepers", &card.r#ref);
    assert_eq!(log_end(&r), before, "a refused answer writes nothing");
}

/// O10: `when: owner` names the owners of the most specific owner entry of the first covered
/// block, labelled `owners of <path>`.
#[test]
fn ap12_an_owner_entry_routes_to_the_owners() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    land_policy(
        &mut r,
        policy(
            json!({"approvers": [PERSON]}),
            json!([{"when": "owner"}]),
            json!([{"path": "spec", "owners": [BOSS]}]),
        ),
    );
    let p = edit_version(&mut r, &w, "e1");
    assert!(!refs(&queue_for(&r, PERSON)).contains(&p.r#ref));
    assert!(refs(&queue_for(&r, BOSS)).contains(&p.r#ref));
    let e = land_err(&mut r, &approver(), &p.r#ref);
    outside(&e, PERSON, "owners of spec", &p.r#ref);
    r.land_with(&boss(), &p.r#ref, LandOptions::default())
        .expect("an owner approves");
}

/// O11: a `team:<name>` member holds every person locally: the hub checks the team. A role of
/// humans alone still holds only them.
#[test]
fn ap12_a_team_member_lets_the_person_approve() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    land_policy(
        &mut r,
        policy(
            json!({"bosses": [BOSS], "core": ["team:core"]}),
            json!([
                {"when": "decision", "role": "bosses"},
                {"when": "any", "role": "core"}
            ]),
            json!([]),
        ),
    );
    let d1 = decision_version(&mut r, &w, "d1");
    let p = edit_version(&mut r, &w, "e1");
    let qa = refs(&queue_for(&r, PERSON));
    assert!(qa.contains(&p.r#ref), "{qa:#?}");
    assert!(!qa.contains(&d1.r#ref), "{qa:#?}");
    let e = land_err(&mut r, &approver(), &d1.r#ref);
    outside(&e, PERSON, "bosses", &d1.r#ref);
    r.land_with(&approver(), &p.r#ref, LandOptions::default())
        .expect("a team member approves locally");
}

/// O10: a Later answer is never refused, on an approve item or a deferral card, where every
/// other answer from outside the role is.
#[test]
fn ap12_later_is_never_refused() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    land_policy(&mut r, one_role("any", &[BOSS]));
    let p = deferred(&mut r, &w);
    let dr = format!("defer:{}", p.r#ref);
    let card = item_for(&r, BOSS, &dr);
    let e = relayed(&mut r, PERSON, &card, "o2").unwrap_err();
    outside(&e, PERSON, "approvers", &dr);
    for r#ref in [&dr, &p.r#ref] {
        let card = item_for(&r, BOSS, r#ref);
        let v = relayed(&mut r, PERSON, &card, "later")
            .unwrap_or_else(|e| panic!("Later on {ref}: {e}"));
        assert_eq!(v["act"], "later", "{v}");
    }
}

/// D-AAQ9Z, D-TF78H: a refused deferral card leaves the queue and still takes a later answer, so
/// the card bound outside the queue keeps its required role.
#[test]
fn ap12_a_refused_deferral_card_keeps_its_role() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    land_policy(
        &mut r,
        policy(
            json!({"deferrers": [BOSS]}),
            json!([{"when": "deferral", "role": "deferrers"}]),
            json!([]),
        ),
    );
    let p = deferred(&mut r, &w);
    let dr = format!("defer:{}", p.r#ref);
    let card = item_for(&r, BOSS, &dr);
    let v = relayed(&mut r, BOSS, &card, "o2").expect("the person in the role refuses");
    assert_eq!(v["act"], "refuse_deferrals", "{v}");
    assert!(!refs(&queue_for(&r, BOSS)).contains(&dr));
    let before = log_end(&r);
    let e = relayed(&mut r, PERSON, &card, "o1").unwrap_err();
    outside(&e, PERSON, "deferrers", &dr);
    assert_eq!(log_end(&r), before, "a refused answer writes nothing");
    let v = relayed(&mut r, BOSS, &card, "o1").expect("the person in the role accepts later");
    assert_eq!(v["act"], "accept_deferrals", "{v}");
}

/// D-TF78H: a confirm item and a rejection of an approve item are answers too: from outside the
/// role each is refused and writes nothing.
#[test]
fn ap12_a_confirm_or_a_rejection_outside_the_role_is_e_permission() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    land_policy(&mut r, one_role("decision", &[BOSS]));
    let d1 = decision_version(&mut r, &w, "d1");
    let confirm = queue_for(&r, BOSS)
        .items
        .into_iter()
        .find(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(d1.r#ref.as_str()))
        .expect("a confirm item beside the batch");
    let batch = item_for(&r, BOSS, &d1.r#ref);
    let before = log_end(&r);
    let e = relayed(&mut r, PERSON, &confirm, "confirm").unwrap_err();
    outside(&e, PERSON, "approvers", &confirm.r#ref);
    let e = relayed(&mut r, PERSON, &batch, "reject").unwrap_err();
    outside(&e, PERSON, "approvers", &d1.r#ref);
    assert_eq!(log_end(&r), before, "a refused answer writes nothing");
    let v = relayed(&mut r, BOSS, &confirm, "confirm").expect("the person in the role confirms");
    assert_eq!(v["act"], "confirm", "{v}");
}
