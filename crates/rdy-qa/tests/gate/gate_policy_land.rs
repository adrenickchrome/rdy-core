//! ap11 (D-ZWP7Q "The agent's landing act", "The coverage gate"; D-ME2HD freshness; D-DKAMT;
//! D-CC90E; M3c O6, O14, O15, O16, O27; M3b RULING F1): an agent session of role writer or
//! reviewer lands a ready version that the authority policy accepts with `rdy_land` / `rdy land`.
//! The landing records `assurance: policy`, the live policy version and the assessment, with no
//! relay. A version the policy does not accept is `E_POLICY_HUMAN`. Freshness adds two checks for
//! every landing of a proposed version (the policy version, and the pages under
//! `coverage.gate: no_lower`) and one for `rdy_land` (a written block in stable content now under
//! a `lifecycle: draft` rule); each refuses with `E_PROPOSAL_MOVED` and a fresh card.
use rdy_engine::engine::Local;
use rdy_engine::pending::{CARD_MAX_TOKENS, Item, ItemKind, PendingQuery};
use rdy_engine::policy::{LivePolicy, live_policy};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{
    E_BAD_INPUT, E_PERMISSION, E_POLICY_HUMAN, E_PROPOSAL_MOVED, E_PROPOSAL_NOT_READY,
    E_REDACTION_HUMAN, LandInfo, POLICY_HUMAN_FIX, ProposalInfo, Rdy, RdyError, Scope, Session,
    WriteRequest, WriteResult,
};
use rdy_mcp::tools::{McpState, rdy_land, rdy_session};
use rdy_model::assessment::{AssessedPolicy, Assessment, PolicyVerdict};
use rdy_model::{Assurance, BlockId, Hash, Justification, LandingRecord, Role};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{approver, land, parse_test_ops, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY};
use rdy_qa::packs::QA_PACK_YAML;
use rdy_qa::tensions::{ag, j_typo, put_j, upd};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

/// The fix of a refusal with a fresh card (RULING F1).
fn fresh_fix(pref: &str) -> String {
    format!("review the fresh card in corrected and approve {pref} again")
}

/// The policy of two rules. `drafts`: agents `agent:a*` may land draft edits with `implements`
/// or `editorial` changes and no decision. `decides`: the same agents may land any content and
/// `two_way` decisions in area `payments` with basis `precedent`. A version that writes a
/// decision otherwise needs a person from `approvers`.
fn policy_fields() -> Value {
    json!({
        "key": "authority-policy",
        "agent_may_accept": [
            {
                "name": "drafts",
                "actors": ["agent:a*"],
                "lifecycle": "draft",
                "justifications": ["implements", "editorial"],
                "decisions": "none",
                "max_blocks": 20,
                "max_items": 20
            },
            {
                "name": "decides",
                "actors": ["agent:a*"],
                "lifecycle": "any",
                "justifications": ["implements", "editorial"],
                "decisions": "two_way",
                "two_way_areas": ["payments"],
                "basis": ["precedent"],
                "max_blocks": 20,
                "max_items": 20
            }
        ],
        "roles": {"approvers": [PERSON]},
        "require": [{"when": "decision", "role": "approvers"}],
        "owners": []
    })
}

/// Canon, in an `rdy-qa` pack project (areas `payments`, `identity`, `glossary`): a draft doc
/// `notes` with a section `s` and claims `x` and `z`; a draft doc `wallets` (area `payments`)
/// with a section `ws`; a draft doc `other` with a claim `o`. With `policy`, also a doc `rules`
/// that holds the policy block, landed with the decision it implements.
struct World {
    dir: tempfile::TempDir,
    r: Rdy,
    notes: BlockId,
    s: BlockId,
    x: BlockId,
    z: BlockId,
    ws: BlockId,
    o: BlockId,
    rules: Option<BlockId>,
    policy: Option<BlockId>,
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn h(s: &str) -> Hash {
    s.parse().unwrap()
}

/// Writes `ops` in `view` as `session` with justification `j`.
fn write_j(
    r: &mut Rdy,
    session: &Session,
    view: &str,
    ops: Value,
    j: Option<Justification>,
) -> WriteResult {
    r.write(
        session,
        WriteRequest {
            view: view.into(),
            ops: parse_test_ops(ops).unwrap(),
            justification: j,
            message: String::new(),
        },
    )
    .unwrap_or_else(|e| panic!("write in {view}: {e}"))
}

fn implements(d: &BlockId) -> Option<Justification> {
    Some(Justification::Implements(vec![d.clone()]))
}

/// A project of pack `rdy-qa` in `dir`, with `extra` after the `pack:` line of `.rdy.yaml`.
fn qa_project_in(dir: &Path, extra: &str) {
    drop(Rdy::init(dir, "rdy-standard").expect("init"));
    std::fs::write(dir.join(".rdy/packs/rdy-qa.yaml"), QA_PACK_YAML).expect("pack");
    std::fs::write(dir.join(".rdy.yaml"), format!("pack: rdy-qa\n{extra}")).expect("config");
}

/// Seeds `r` (canon of [`World`]) and, with `policy`, lands the policy; the ids.
fn seed(r: &mut Rdy, policy: bool) -> SeedIds {
    let w = write_j(
        r,
        &writer(),
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "X holds."},
            {"op": "create", "type": "claim", "id": "$z", "parent": "$s", "body": "Z holds."},
            {"op": "create", "type": "doc", "id": "$w", "fields": {"name": "wallets", "title": "Wallets", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$ws", "parent": "$w", "fields": {"title": "Fees"}},
            {"op": "create", "type": "doc", "id": "$od", "fields": {"name": "other", "title": "Other", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$os", "parent": "$od", "fields": {"title": "More"}},
            {"op": "create", "type": "claim", "id": "$o", "parent": "$os", "body": "O holds."}
        ]),
        None,
    );
    land(r, "seed").unwrap();
    let (rules, policy) = if policy {
        let (doc, d) = rules_and_decision(r, "pol");
        let p = write_j(
            r,
            &writer(),
            "pol",
            json!([{"op": "create", "type": "authority_policy", "id": "$p", "parent": doc.to_string(), "fields": policy_fields()}]),
            implements(&d),
        );
        land(r, "pol").unwrap();
        (Some(doc), Some(id(&p.created["$p"])))
    } else {
        (None, None)
    };
    SeedIds {
        notes: id(&w.created["$n"]),
        s: id(&w.created["$s"]),
        x: id(&w.created["$x"]),
        z: id(&w.created["$z"]),
        ws: id(&w.created["$ws"]),
        o: id(&w.created["$o"]),
        rules,
        policy,
    }
}

struct SeedIds {
    notes: BlockId,
    s: BlockId,
    x: BlockId,
    z: BlockId,
    ws: BlockId,
    o: BlockId,
    rules: Option<BlockId>,
    policy: Option<BlockId>,
}

/// In view `view`, as the human writer: a doc `rules` and a decision under it that needs no
/// content; returns (doc, decision).
fn rules_and_decision(r: &mut Rdy, view: &str) -> (BlockId, BlockId) {
    let w = write_j(
        r,
        &writer(),
        view,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "overview"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$d", "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}}
        ]),
        None,
    );
    (id(&w.created["$d"]), id(&w.created["$dec"]))
}

/// The world, seeded with the gate off; then `.rdy.yaml` gets `extra` and the project reopens.
fn world_with(policy: bool, extra: &str) -> World {
    let dir = tempfile::tempdir().unwrap();
    qa_project_in(dir.path(), "");
    let mut r = Rdy::open(dir.path()).unwrap();
    let ids = seed(&mut r, policy);
    let r = if extra.is_empty() {
        r
    } else {
        r.close().unwrap();
        std::fs::write(
            dir.path().join(".rdy.yaml"),
            format!("pack: rdy-qa\n{extra}"),
        )
        .unwrap();
        Rdy::open(dir.path()).unwrap()
    };
    World {
        dir,
        r,
        notes: ids.notes,
        s: ids.s,
        x: ids.x,
        z: ids.z,
        ws: ids.ws,
        o: ids.o,
        rules: ids.rules,
        policy: ids.policy,
    }
}

fn world(policy: bool) -> World {
    world_with(policy, "")
}

fn live(r: &Rdy) -> LivePolicy {
    let st = r.read();
    let canon = st.materialized(&Scope::Canon).unwrap();
    live_policy(&st, &canon).expect("a policy in canon")
}

/// Agent `agent:<name>` proposes its view `<name>` with a summary.
fn propose_as(r: &mut Rdy, name: &str) -> ProposalInfo {
    let p = r
        .propose(&ag(name), name, name, Some(TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {name}: {e}"));
    assert!(p.ready, "{name}: {:#?}", p.issues);
    p
}

/// Agent `agent:<name>` fixes a typo in claim `x`: a draft edit the rule `drafts` accepts.
fn typo_edit(w: &mut World, name: &str) {
    put_j(&mut w.r, name, upd(&w.x, "X holds, now."), j_typo());
}

/// Agent `agent:<name>` records a decision that needs no content under section `s` of `notes`
/// (outside area `payments`): a version neither rule covers. Returns the decision.
fn decision_edit(w: &mut World, name: &str) -> BlockId {
    let res = put_j(
        &mut w.r,
        name,
        json!([{"op": "create", "type": "decision", "id": "$dec", "parent": w.s.to_string(),
                "fields": {"statement": "Ideas stay short", "status": "proposed", "needs_content": false,
                           "reversibility": "two_way", "authority_basis": "precedent"}}]),
        j_typo(),
    );
    id(&res.created["$dec"])
}

/// Agent `agent:<name>` records a `two_way` decision with basis `precedent` under section `ws`
/// of `wallets` (area `payments`): a decision the rule `decides` accepts. Returns it.
fn payments_decision(w: &mut World, name: &str) -> BlockId {
    let res = put_j(
        &mut w.r,
        name,
        json!([{"op": "create", "type": "decision", "id": "$dec", "parent": w.ws.to_string(),
                "fields": {"statement": "Fees round up", "status": "proposed", "needs_content": false,
                           "reversibility": "two_way", "authority_basis": "precedent"}}]),
        j_typo(),
    );
    id(&res.created["$dec"])
}

/// The assessment the store names for version `p` now.
fn stored(r: &Rdy, p: &ProposalInfo) -> (Hash, Assessment) {
    let st = r.read();
    let a = st
        .reader()
        .assessment_of_proposal(&h(&p.id), st.seq())
        .unwrap()
        .unwrap_or_else(|| panic!("{} stores an assessment", p.r#ref));
    (a, st.reader().assessment(&a).unwrap())
}

/// The landing record of canon's head.
fn head_record(r: &Rdy) -> LandingRecord {
    let st = r.read();
    let head = st.canon_head().expect("a landing");
    st.reader().landing(&head).unwrap()
}

fn rdy_land_as(r: &mut Rdy, s: &Session, p: &ProposalInfo) -> Result<LandInfo, RdyError> {
    r.land_by_policy(s, &p.r#ref)
}

fn accepts(rule: &str) -> PolicyVerdict {
    PolicyVerdict::Accepts {
        rule: rule.to_string(),
    }
}

fn approve_item(r: &Rdy, pref: &str) -> Item {
    r.read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap()
        .items
        .into_iter()
        .find(|i| i.r#ref == pref && i.kind == ItemKind::Approve)
        .unwrap_or_else(|| panic!("an approve item for {pref}"))
}

/// `e` is RULING F1's refusal of `p` with `message` and a fresh card; the fresh assessment's
/// hash and record.
fn assert_fresh_card(r: &Rdy, e: &RdyError, p: &ProposalInfo, message: &str) -> Assessment {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (E_PROPOSAL_MOVED, message, fresh_fix(&p.r#ref).as_str()),
        "{e:#?}"
    );
    assert!(e.issues.is_empty(), "{e:#?}");
    let card = &e.corrected.as_ref().expect("a fresh card")["card"];
    let item = approve_item(r, &p.r#ref);
    assert_eq!(
        card,
        &json!({"ref": p.r#ref, "card_hash": item.card_hash, "card": card["card"]}),
        "{e:#?}"
    );
    let (fresh, a) = stored(r, p);
    assert_ne!(Some(fresh.to_string()), p.assessment, "a fresh record");
    assert_eq!(a.base, r.read().canon_head(), "on canon's head");
    a
}

/// A landing whose record is the policy landing of `p` by `by` under `policy`.
fn assert_policy_landing(
    r: &Rdy,
    l: &LandInfo,
    p: &ProposalInfo,
    by: &Session,
    policy: &LivePolicy,
) {
    assert_eq!(l.assurance, "policy");
    assert_eq!(l.approved, p.r#ref);
    let rec = head_record(r);
    assert_eq!(
        r.read().canon_head().map(|c| c.to_string()),
        Some(l.landing.clone())
    );
    assert_eq!(rec.assurance, Assurance::Policy);
    assert_eq!(rec.policy, Some(policy.reference()));
    assert_eq!(rec.assessment, Some(stored(r, p).0));
    assert_eq!(rec.relayed_by, None);
    assert_eq!(rec.relay_evidence, None);
    assert_eq!(rec.evidence, None);
    assert_eq!(rec.approved_by.actor, by.actor);
    assert_eq!(rec.approved_by.run, by.run);
    assert_eq!(rec.approved_by.role, by.role);
}

#[test]
fn ap11_rdy_land_lands_a_version_the_policy_accepts() {
    let mut w = world(true);
    typo_edit(&mut w, "a1");
    let d = payments_decision(&mut w, "a1");
    let p = propose_as(&mut w.r, "a1");
    let policy = live(&w.r);
    assert_eq!(
        stored(&w.r, &p).1.policy,
        Some(AssessedPolicy {
            policy: Some(policy.reference()),
            verdict: accepts("decides"),
        })
    );
    let by = ag("a1");
    assert_eq!(by.role, Role::Writer);
    let l = rdy_land_as(&mut w.r, &by, &p).expect("the policy accepts it");
    assert_policy_landing(&w.r, &l, &p, &by, &policy);
    // the version's proposed decision is accepted, as an approval accepts it
    let st = w.r.read();
    let canon = st.materialized(&Scope::Canon).unwrap();
    let dec = canon.get(&d).expect("the decision landed");
    assert_eq!(dec.field_str("status"), Some("accepted"));
    assert!(dec.field_str("decided_at").is_some());
    assert_eq!(canon.get(&w.x).unwrap().body, "X holds, now.");
}

#[test]
fn ap11_rdy_land_refuses_a_version_the_policy_does_not_accept() {
    let mut w = world(true);
    decision_edit(&mut w, "a1");
    let p = propose_as(&mut w.r, "a1");
    let PolicyVerdict::NeedsPerson { reasons, .. } = stored(&w.r, &p).1.policy.unwrap().verdict
    else {
        panic!("the policy needs a person");
    };
    assert!(!reasons.is_empty());
    let head = w.r.read().canon_head();
    let e = rdy_land_as(&mut w.r, &ag("a1"), &p).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_POLICY_HUMAN,
            format!(
                "the authority policy needs a person for {}: {}",
                p.r#ref,
                reasons.join("; ")
            )
            .as_str(),
            POLICY_HUMAN_FIX
        ),
        "{e:#?}"
    );
    assert_eq!(
        POLICY_HUMAN_FIX,
        "show the card to the person and relay their answer: rdy_pending, then rdy_answer"
    );
    assert_eq!(w.r.read().canon_head(), head, "nothing landed");
}

#[test]
fn ap11_rdy_land_with_no_policy_is_e_policy_human_with_no_authority_policy_is_set() {
    let mut w = world(false);
    typo_edit(&mut w, "a1");
    let p = propose_as(&mut w.r, "a1");
    let e = rdy_land_as(&mut w.r, &ag("a1"), &p).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_POLICY_HUMAN,
            format!(
                "the authority policy needs a person for {}: no authority policy is set",
                p.r#ref
            )
            .as_str(),
            POLICY_HUMAN_FIX
        ),
        "{e:#?}"
    );
}

#[test]
fn ap11_rdy_land_in_a_human_session_is_e_bad_input() {
    let mut w = world(true);
    typo_edit(&mut w, "a1");
    let p = propose_as(&mut w.r, "a1");
    for s in [writer(), approver()] {
        let e = rdy_land_as(&mut w.r, &s, &p).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_BAD_INPUT,
                "rdy land is for agent sessions; a person approves",
                "approve with rdy approve"
            ),
            "{e:#?}"
        );
    }
}

#[test]
fn ap11_rdy_land_in_a_reader_session_is_e_permission() {
    let mut w = world(true);
    typo_edit(&mut w, "a1");
    let p = propose_as(&mut w.r, "a1");
    let reader = Session::agent("agent:a1".into(), Some("a1".into()), Role::Reader).unwrap();
    let e = rdy_land_as(&mut w.r, &reader, &p).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_PERMISSION,
            "role Reader cannot land proposals",
            "start the session as a writer"
        ),
        "{e:#?}"
    );
}

#[test]
fn ap11_rdy_land_in_a_reviewer_session_lands() {
    let mut w = world(true);
    typo_edit(&mut w, "a1");
    let p = propose_as(&mut w.r, "a1");
    let reviewer = Session::agent("agent:a1".into(), Some("a1".into()), Role::Reviewer).unwrap();
    let l = rdy_land_as(&mut w.r, &reviewer, &p).expect("a reviewer lands");
    let policy = live(&w.r);
    assert_policy_landing(&w.r, &l, &p, &reviewer, &policy);
}

#[test]
fn ap11_rdy_land_of_a_not_ready_version_is_e_proposal_not_ready() {
    let mut w = world(true);
    typo_edit(&mut w, "a1");
    // no summary: not ready
    let p = w.r.propose(&ag("a1"), "a1", "a1", None).unwrap();
    assert!(!p.ready);
    let e = rdy_land_as(&mut w.r, &ag("a1"), &p).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_NOT_READY, "{e:#?}");
}

#[test]
fn ap11_rdy_land_of_a_moved_version_is_e_proposal_moved() {
    let mut w = world(true);
    typo_edit(&mut w, "a1");
    let first = propose_as(&mut w.r, "a1");
    put_j(
        &mut w.r,
        "a1",
        upd(&w.x, "X holds, now and then."),
        j_typo(),
    );
    let second = propose_as(&mut w.r, "a1");
    assert_ne!(first.r#ref, second.r#ref);
    let e = rdy_land_as(&mut w.r, &ag("a1"), &first).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e:#?}");
    rdy_land_as(&mut w.r, &ag("a1"), &second).expect("the latest version lands");
}

#[test]
fn ap11_a_lander_outside_the_rules_actors_is_e_policy_human() {
    let mut w = world(true);
    typo_edit(&mut w, "a1");
    let p = propose_as(&mut w.r, "a1");
    assert_eq!(
        stored(&w.r, &p).1.policy.unwrap().verdict,
        accepts("drafts")
    );
    let other = Session::agent("agent:b1".into(), Some("b1".into()), Role::Writer).unwrap();
    let e = rdy_land_as(&mut w.r, &other, &p).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_POLICY_HUMAN,
            format!(
                "the authority policy needs a person for {}: actor agent:b1 matches none of rule drafts's actors",
                p.r#ref
            )
            .as_str(),
            POLICY_HUMAN_FIX
        ),
        "{e:#?}"
    );
    rdy_land_as(&mut w.r, &ag("a1"), &p).expect("a rule's actor lands it");
}

#[test]
fn ap11_rdy_land_of_a_removal_is_e_redaction_human() {
    let mut w = world(true);
    let s = w.s.to_string();
    let prose = write_j(
        &mut w.r,
        &writer(),
        "secret",
        json!([{"op": "create", "type": "prose", "id": "$p", "parent": s, "body": "The code is 1234."}]),
        None,
    );
    land(&mut w.r, "secret").unwrap();
    w.r.redact_in(
        &writer(),
        "redact",
        RedactInput {
            targets: vec![prose.created["$p"].clone()],
            reason_class: "secret".into(),
            reason: "a leaked code".into(),
            dry_run: false,
        },
    )
    .unwrap();
    let p =
        w.r.propose(
            &writer(),
            "redact",
            "Remove the code",
            Some("Removes the leaked code."),
        )
        .unwrap();
    let e = rdy_land_as(&mut w.r, &ag("a1"), &p).unwrap_err();
    assert_eq!(e.code, E_REDACTION_HUMAN, "{e:#?}");
}

/// A person changes the policy (`max_blocks` 30) and lands it; the new live policy.
fn change_policy(w: &mut World) -> LivePolicy {
    let rules = w.rules.clone().unwrap();
    let policy = w.policy.clone().unwrap();
    let dec = write_j(
        &mut w.r,
        &writer(),
        "chg",
        json!([{"op": "create", "type": "decision", "id": "$dec", "parent": rules.to_string(),
                "fields": {"statement": "Widen the policy", "status": "proposed", "needs_content": false}}]),
        None,
    );
    let d = id(&dec.created["$dec"]);
    let mut fields = policy_fields();
    fields["agent_may_accept"][0]["max_blocks"] = json!(30);
    write_j(
        &mut w.r,
        &writer(),
        "chg",
        json!([{"op": "update", "id": policy.to_string(), "fields": fields}]),
        implements(&d),
    );
    land(&mut w.r, "chg").unwrap();
    live(&w.r)
}

#[test]
fn ap11_a_policy_change_after_propose_refuses_with_a_fresh_card() {
    let mut w = world(true);
    let before = live(&w.r);
    typo_edit(&mut w, "a1");
    let p = propose_as(&mut w.r, "a1");
    // a version a person approves, proposed before the change too
    let s = w.s.to_string();
    put_j(
        &mut w.r,
        "a2",
        json!([{"op": "create", "type": "claim", "parent": s, "body": "W holds."}]),
        j_typo(),
    );
    let q = propose_as(&mut w.r, "a2");
    let after = change_policy(&mut w);
    assert_ne!(after.version, before.version);

    // rdy_land
    let e = rdy_land_as(&mut w.r, &ag("a1"), &p).unwrap_err();
    let message = format!(
        "proposal {} moved: the authority policy changed after its assessment",
        p.r#ref
    );
    let fresh = assert_fresh_card(&w.r, &e, &p, &message);
    assert_eq!(
        fresh.policy,
        Some(AssessedPolicy {
            policy: Some(after.reference()),
            verdict: accepts("drafts"),
        }),
        "the fresh assessment holds the new verdict"
    );
    let l = rdy_land_as(&mut w.r, &ag("a1"), &p).expect("the next rdy_land lands");
    assert_policy_landing(&w.r, &l, &p, &ag("a1"), &after);

    // an approval
    let e = w.r.land_report(&approver(), &q.id).unwrap_err();
    let message = format!(
        "proposal {} moved: the authority policy changed after its assessment",
        q.r#ref
    );
    let fresh = assert_fresh_card(&w.r, &e, &q, &message);
    assert_eq!(fresh.policy.unwrap().policy, Some(after.reference()));
    w.r.land_report(&approver(), &q.id)
        .expect("the next approval lands");
}

#[test]
fn ap11_a_landing_on_a_page_under_no_lower_refuses_with_a_fresh_card() {
    let mut w = world_with(true, "coverage:\n  gate: no_lower\n");
    typo_edit(&mut w, "a1");
    let p = propose_as(&mut w.r, "a1");
    let (_, a) = stored(&w.r, &p);
    assert!(a.coverage.pages.iter().any(|g| g.doc == w.notes), "{a:#?}");
    assert!(!a.reach.contains(&w.z), "z is outside the version's reach");
    // a person's typo fix of z, on the same page
    put_j(&mut w.r, "zz", upd(&w.z, "Z holds, now."), j_typo());
    let l: Hash = land(&mut w.r, "zz").unwrap().parse().unwrap();
    let e = rdy_land_as(&mut w.r, &ag("a1"), &p).unwrap_err();
    let message = format!(
        "proposal {} moved: landing {} changed page notes after its assessment",
        p.r#ref,
        l.short()
    );
    assert_fresh_card(&w.r, &e, &p, &message);
    rdy_land_as(&mut w.r, &ag("a1"), &p).expect("the next rdy_land lands");
}

#[test]
fn ap11_a_landing_on_a_page_under_no_lower_refuses_an_approval_too() {
    let mut w = world_with(true, "coverage:\n  gate: no_lower\n");
    typo_edit(&mut w, "a1");
    let p = propose_as(&mut w.r, "a1");
    put_j(&mut w.r, "zz", upd(&w.z, "Z holds, now."), j_typo());
    let l: Hash = land(&mut w.r, "zz").unwrap().parse().unwrap();
    let e = w.r.land_report(&approver(), &p.id).unwrap_err();
    let message = format!(
        "proposal {} moved: landing {} changed page notes after its assessment",
        p.r#ref,
        l.short()
    );
    assert_fresh_card(&w.r, &e, &p, &message);
}

#[test]
fn ap11_a_landing_elsewhere_under_no_lower_lands() {
    let mut w = world_with(true, "coverage:\n  gate: no_lower\n");
    typo_edit(&mut w, "a1");
    let p = propose_as(&mut w.r, "a1");
    put_j(&mut w.r, "oo", upd(&w.o, "O holds, now."), j_typo());
    land(&mut w.r, "oo").unwrap();
    let l = rdy_land_as(&mut w.r, &ag("a1"), &p).expect("a landing on another page");
    let policy = live(&w.r);
    assert_policy_landing(&w.r, &l, &p, &ag("a1"), &policy);
}

#[test]
fn ap11_a_doc_made_stable_after_propose_refuses_rdy_land() {
    let mut w = world(true);
    typo_edit(&mut w, "a1");
    let p = propose_as(&mut w.r, "a1");
    assert_eq!(
        stored(&w.r, &p).1.policy.unwrap().verdict,
        accepts("drafts")
    );
    // another landing makes the notes doc stable
    let n = w.notes.to_string();
    put_j(
        &mut w.r,
        "m",
        json!([{"op": "update", "id": n, "fields": {"lifecycle": "stable"}}]),
        j_typo(),
    );
    land(&mut w.r, "m").unwrap();
    let e = rdy_land_as(&mut w.r, &ag("a1"), &p).unwrap_err();
    let message = format!(
        "proposal {} moved: block {} is in stable content now",
        p.r#ref, w.x
    );
    let fresh = assert_fresh_card(&w.r, &e, &p, &message);
    // the fresh verdict judges the stable block again: the rule drafts no longer covers it, and
    // the rule decides (lifecycle any) does
    assert_eq!(
        fresh.policy.map(|a| a.verdict),
        Some(accepts("decides")),
        "the fresh assessment holds the new verdict"
    );
    let l = rdy_land_as(&mut w.r, &ag("a1"), &p).expect("the next rdy_land lands");
    let policy = live(&w.r);
    assert_policy_landing(&w.r, &l, &p, &ag("a1"), &policy);
}

#[test]
fn ap11_a_doc_made_stable_after_propose_does_not_refuse_an_approval() {
    // O16 (3) is rdy_land's alone: a person's approval of a draft edit stays fresh
    let mut w = world(true);
    typo_edit(&mut w, "a1");
    let p = propose_as(&mut w.r, "a1");
    let n = w.notes.to_string();
    put_j(
        &mut w.r,
        "m",
        json!([{"op": "update", "id": n, "fields": {"lifecycle": "stable"}}]),
        j_typo(),
    );
    land(&mut w.r, "m").unwrap();
    w.r.land_report(&approver(), &p.id)
        .expect("the approval lands");
    assert_eq!(head_record(&w.r).assessment, Some(stored(&w.r, &p).0));
}

#[test]
fn ap11_a_policy_change_lands_after_approval() {
    let mut w = world(true);
    let before = live(&w.r);
    let rules = w.rules.clone().unwrap();
    let policy = w.policy.clone().unwrap();
    let dec = write_j(
        &mut w.r,
        &writer(),
        "chg",
        json!([{"op": "create", "type": "decision", "id": "$dec", "parent": rules.to_string(),
                "fields": {"statement": "Widen the policy", "status": "proposed", "needs_content": false}}]),
        None,
    );
    let d = id(&dec.created["$dec"]);
    let mut fields = policy_fields();
    fields["agent_may_accept"][0]["max_blocks"] = json!(30);
    write_j(
        &mut w.r,
        &writer(),
        "chg",
        json!([{"op": "update", "id": policy.to_string(), "fields": fields}]),
        implements(&d),
    );
    let p =
        w.r.propose(&writer(), "chg", "Widen", Some(TEST_SUMMARY))
            .unwrap();
    assert!(p.ready, "{:#?}", p.issues);
    assert_eq!(
        stored(&w.r, &p).1.policy.unwrap().policy,
        Some(before.reference()),
        "judged under canon's policy"
    );
    // the policy's own version is not a policy change for freshness: it lands
    let l =
        w.r.land_report(&approver(), &p.id)
            .expect("the approval lands");
    assert_eq!(l.assurance, "local");
    assert_eq!(head_record(&w.r).policy, None);
    let after = live(&w.r);
    assert_ne!(
        after.version, before.version,
        "the landing records the change"
    );
    // the next version is judged under the new policy version
    typo_edit(&mut w, "a1");
    let q = propose_as(&mut w.r, "a1");
    assert_eq!(
        stored(&w.r, &q).1.policy,
        Some(AssessedPolicy {
            policy: Some(after.reference()),
            verdict: accepts("drafts"),
        })
    );
    rdy_land_as(&mut w.r, &ag("a1"), &q).expect("it lands under the new policy");
    assert_eq!(head_record(&w.r).policy, Some(after.reference()));
}

/// `.rdy/local.yaml` with `signing.sign: always` and no key.
fn signing_always(dir: &Path) {
    std::fs::write(
        dir.join(".rdy/local.yaml"),
        "signing:\n  key: null\n  agent_key: null\n  sign: always\n  allowed_signers: null\n",
    )
    .unwrap();
}

/// An MCP session of agent `agent:<name>` (role writer) on `engine`.
fn mcp_as(engine: &Arc<Local>, name: &str) -> McpState {
    let mut st = McpState::new(engine.clone());
    rdy_session(
        &mut st,
        serde_json::from_value(
            json!({"actor": format!("agent:{name}"), "run": name, "role": "writer"}),
        )
        .unwrap(),
    )
    .unwrap();
    st
}

fn mcp_land(st: &mut McpState, pref: &str) -> Result<Value, RdyError> {
    rdy_land(
        st,
        serde_json::from_value(json!({"proposal": pref})).unwrap(),
    )
}

#[test]
fn ap11_signing_always_never_refuses_rdy_land() {
    let mut w = world(true);
    typo_edit(&mut w, "a1");
    let p = propose_as(&mut w.r, "a1");
    signing_always(w.dir.path());
    let engine = Arc::new(Local::new(w.r));
    let mut st = mcp_as(&engine, "a1");
    let v = mcp_land(&mut st, &p.r#ref).expect("rdy_land lands under signing always");
    assert_eq!(v["assurance"], "policy", "{v:#}");
    assert_eq!(v["signatures"], json!([]), "{v:#}");
    assert!(
        !v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "W_SIGNING_FAILED"),
        "{v:#}"
    );
}

#[test]
fn ap11_mcp_and_cli_land_give_the_same_landing_report() {
    let s = Served::new();
    std::fs::write(s.dir.path().join(".rdy/packs/rdy-qa.yaml"), QA_PACK_YAML).unwrap();
    let cfg = s.dir.path().join(".rdy.yaml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(&cfg, text.replace("pack: rdy-standard", "pack: rdy-qa")).unwrap();
    let (p1, p2, p3) = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let ids = seed(&mut r, true);
        put_j(&mut r, "a1", upd(&ids.x, "X holds, now."), j_typo());
        put_j(&mut r, "a2", upd(&ids.z, "Z holds, now."), j_typo());
        put_j(&mut r, "a3", upd(&ids.o, "O holds, now."), j_typo());
        let ps = (
            propose_as(&mut r, "a1"),
            propose_as(&mut r, "a2"),
            propose_as(&mut r, "a3"),
        );
        r.close().unwrap();
        ps
    };
    // the chat path, in process
    let mcp = {
        let engine = Arc::new(Local::open(s.dir.path()).unwrap());
        let mut st = mcp_as(&engine, "a1");
        let v = mcp_land(&mut st, &p1.r#ref).expect("rdy_land");
        drop(st);
        Arc::into_inner(engine).unwrap().close().unwrap();
        v
    };
    // the CLI, through the daemon
    let (ok, cli) = s.json_as("agent:a2", Some("a2"), &["land", &p2.r#ref, "--json"]);
    assert!(ok, "{cli:#}");
    let keys = |v: &Value| -> Vec<String> { v.as_object().unwrap().keys().cloned().collect() };
    assert_eq!(keys(&mcp), keys(&cli), "{mcp:#}\n{cli:#}");
    for (v, p, name) in [(&mcp, &p1, "a1"), (&cli, &p2, "a2")] {
        assert_eq!(v["approved"], p.r#ref, "{v:#}");
        assert_eq!(v["version"], p.id, "{v:#}");
        assert_eq!(v["assurance"], "policy", "{v:#}");
        assert_eq!(v["changes"], 1, "{v:#}");
        assert_eq!(v["title"], name, "{v:#}");
        assert_eq!(v["signatures"], json!([]), "{v:#}");
    }
    assert_eq!(mcp["warnings"], cli["warnings"]);
    // each landing rebased its own view, whose change dropped out
    for (v, name) in [(&mcp, "a1"), (&cli, "a2")] {
        assert_eq!(
            v["rebased"][0],
            json!({"view": name, "dropped": 1, "conflicts": 0}),
            "{v:#}"
        );
    }
    // the text prints the landing as rdy approve does, with the assurance
    let out = s.rdy_as("agent:a3", Some("a3"), &["land", &p3.r#ref]);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.starts_with(&format!(
            "approving {}: a3 (1 change(s))\nsummary: {TEST_SUMMARY}\nlanded ",
            p3.r#ref
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!(" ({})\nassurance policy", p3.r#ref)),
        "{text}"
    );
    // a person's session is refused, with the text of O14
    let (ok, e) = s.json(&["land", &p3.r#ref, "--json"]);
    assert!(!ok);
    assert_eq!(e["code"], E_BAD_INPUT, "{e:#}");
}

#[test]
fn ap11_fsck_passes_after_a_policy_landing() {
    let mut w = world(true);
    typo_edit(&mut w, "a1");
    let p = propose_as(&mut w.r, "a1");
    rdy_land_as(&mut w.r, &ag("a1"), &p).expect("lands");
    w.r.fsck().expect("fsck passes");
}
