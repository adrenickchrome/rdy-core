//! rg12 (D-WQVGT, D-ME2HD, D-SDSV2, D-AAQ9Z, D-CQMA4; RULING F1; O7, O8, O10, O11, O12; C7):
//! after approval, landing checks only freshness against the version's stored assessment. A
//! landing since the assessment's base that wrote a block of its `reach`, a pack change or a
//! gate config change refuses with `E_PROPOSAL_MOVED` and a fresh card in `corrected` (rdy writes
//! a fresh `ASSESSMENT` in its own batch, and the next approval lands and names it). A fresh
//! readiness that fails gives its issues and no card. `--strict` and
//! `reviews.require_up_to_date` keep today's texts and give no fresh card. The later refusals
//! need no candidate; a version with no stored assessment is not ready. Every landing of a
//! proposed version names its assessment; a re-affirmation card's landing names none.
use rdy_engine::answer::AnswerRequest;
use rdy_engine::engine::{Engine, Local, with_writer_result};
use rdy_engine::pending::{CARD_MAX_TOKENS, ExceptionKind, Item, ItemKind, PendingQuery};
use rdy_engine::readiness::assessment::{GateConfig, config_digest, pack_digest};
use rdy_engine::readiness::{
    Candidate, candidate_builds_for_tests, readiness, readiness_calls_for_tests,
};
use rdy_engine::tensions::api::AcceptRequest;
use rdy_engine::{
    CoverageGate, DecisionInput, E_BLOCKING_TENSION, E_CONFLICT_UNRESOLVED, E_PROPOSAL_MOVED,
    E_PROPOSAL_NOT_READY, E_VALIDATION, LandInfo, LandOptions, ProposalInfo, Rdy, RdyError,
    ReloadOutcome,
};
use rdy_mcp::tools::{McpState, rdy_answer, rdy_session};
use rdy_model::assessment::Assessment;
use rdy_model::{BlockId, Hash, LandingRecord};
use rdy_qa::fixtures::{approver, land, write, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY, confirm_before_landing, decide_in};
use rdy_qa::packs::use_pack;
use rdy_qa::tensions::{j_typo, put_j, upd};
use rdy_schema::{Issue, Severity};
use rdy_store::kinds;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::sync::Arc;

/// The fix of a refusal with a fresh card (RULING F1).
fn fresh_fix(pref: &str) -> String {
    format!("review the fresh card in corrected and approve {pref} again")
}

/// The fix of a refusal whose fresh readiness failed (O11).
const O11_FIX: &str = "fix each issue and propose again: rdy_propose / rdy propose";

/// Canon: a draft doc whose claim `y` references claim `x`. A version that edits `x` has the
/// plan item "review y" on drafts (no issue), so `y` is in its `reach`.
struct World {
    dir: tempfile::TempDir,
    r: Rdy,
    t: BlockId,
    x: BlockId,
    y: BlockId,
}

fn world() -> World {
    let (dir, mut r) = rdy_qa::fixtures::project();
    let w = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$t", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$t", "body": "X holds."},
            {"op": "create", "type": "claim", "id": "$y", "parent": "$t", "body": "Y builds on {{$x}}."}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let id = |k: &str| BlockId::parse(&w.created[k]).unwrap();
    World {
        t: id("$t"),
        x: id("$x"),
        y: id("$y"),
        dir,
        r,
    }
}

fn propose(r: &mut Rdy, view: &str, summary: Option<&str>) -> ProposalInfo {
    r.propose(&writer(), view, view, summary)
        .unwrap_or_else(|e| panic!("propose {view}: {e}"))
}

/// View `a` edits `x` and is proposed, ready, with its assessment.
fn ready_a(w: &mut World) -> ProposalInfo {
    put_j(&mut w.r, "a", upd(&w.x, "X holds twice."), j_typo());
    let p = propose(&mut w.r, "a", Some(TEST_SUMMARY));
    assert!(p.ready, "{:#?}", p.issues);
    assert!(p.assessment.is_some(), "{p:#?}");
    p
}

/// Lands an unrelated new doc through view `u`; the landing.
fn land_unrelated(r: &mut Rdy) -> Hash {
    put_j(
        r,
        "u",
        json!([{"op": "create", "type": "doc", "id": "$o", "fields": {"name": "other", "title": "Other", "doc_type": "overview"}}]),
        j_typo(),
    );
    land(r, "u").unwrap().parse().unwrap()
}

/// Lands an edit of `id` through view `view`; the landing.
fn land_edit(r: &mut Rdy, view: &str, id: &BlockId, body: &str) -> Hash {
    put_j(r, view, upd(id, body), j_typo());
    land(r, view).unwrap().parse().unwrap()
}

fn approve(r: &mut Rdy, p: &ProposalInfo) -> Result<LandInfo, RdyError> {
    r.land_report(&approver(), &p.id)
}

fn h(s: &str) -> Hash {
    s.parse().unwrap()
}

/// The assessment the store names for version `id` now, and the record.
fn stored(r: &Rdy, id: &str) -> Option<(Hash, Assessment)> {
    let st = r.read();
    let a = st
        .reader()
        .assessment_of_proposal(&h(id), st.seq())
        .unwrap()?;
    Some((a, st.reader().assessment(&a).unwrap()))
}

/// The landing record of canon's head.
fn head_record(r: &Rdy) -> LandingRecord {
    let st = r.read();
    let head = st.canon_head().expect("a landing");
    st.reader().landing(&head).unwrap()
}

/// The logical kinds of the frames written since `from`.
fn kinds_from(r: &Rdy, from: Location) -> Vec<u8> {
    let mut out = vec![];
    r.store()
        .reader()
        .for_each_frame_from(from, r.store().log_end(), |_, kind, _, _, _| {
            out.push(rdy_store::codec::logical_kind(kind));
            Ok(())
        })
        .unwrap();
    out
}

fn assessments_from(r: &Rdy, from: Location) -> usize {
    kinds_from(r, from)
        .into_iter()
        .filter(|k| *k == kinds::ASSESSMENT)
        .count()
}

/// The approve item of `pref` in the person's queue.
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
        .find(|i| i.kind == ItemKind::Approve && i.r#ref == pref)
        .unwrap_or_else(|| panic!("no approve item of {pref}"))
}

/// Checks `e` is the refusal with a fresh card of `p` (message `message`), that the card is the
/// queue's approve item of `p` now, and that the fresh assessment it stands on is the store's
/// latest for `p`, on canon's head, with `p` as its version. Returns the fresh assessment.
fn assert_fresh_card(r: &Rdy, e: &RdyError, p: &ProposalInfo, message: &str) -> Hash {
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
    assert!(card["card"].as_str().is_some_and(|c| !c.is_empty()));
    let (fresh, a) = stored(r, &p.id).expect("the fresh assessment");
    assert_ne!(Some(fresh.to_string()), p.assessment, "a fresh record");
    assert_eq!(a.proposal, h(&p.id));
    assert_eq!(a.base, r.read().canon_head(), "on canon's head");
    fresh
}

#[test]
fn rg12_a_landing_outside_reach_lands() {
    let mut w = world();
    let p = ready_a(&mut w);
    land_unrelated(&mut w.r);
    let from = w.r.store().log_end();
    let l = approve(&mut w.r, &p).expect("a landing outside reach lands");
    assert_eq!(
        assessments_from(&w.r, from),
        0,
        "no fresh assessment: {:?}",
        kinds_from(&w.r, from)
    );
    assert_eq!(head_record(&w.r).assessment, p.assessment.map(|a| h(&a)));
    assert_eq!(l.approved, p.r#ref);
}

#[test]
fn rg12_a_landing_in_reach_refuses_with_a_fresh_card() {
    let mut w = world();
    let p = ready_a(&mut w);
    let (_, first) = stored(&w.r, &p.id).unwrap();
    assert!(
        first.reach.contains(&w.y),
        "y is in reach: {:?}",
        first.reach
    );
    let body = format!("Y builds on {{{{{}}}}} twice.", w.x);
    let l = land_edit(&mut w.r, "l", &w.y, &body);
    let head = w.r.read().canon_head();
    let from = w.r.store().log_end();
    let e = approve(&mut w.r, &p).unwrap_err();
    let message = format!(
        "proposal {} moved: landing {} changed {} after its assessment",
        p.r#ref,
        l.short(),
        w.y
    );
    let fresh = assert_fresh_card(&w.r, &e, &p, &message);
    assert_eq!(w.r.read().canon_head(), head, "nothing landed");
    assert_eq!(
        kinds_from(&w.r, from),
        vec![kinds::ASSESSMENT, kinds::BATCH_COMMIT],
        "the fresh assessment, in a batch of its own"
    );
    // the next approval lands and names the fresh assessment
    approve(&mut w.r, &p).expect("the next approval lands");
    assert_eq!(head_record(&w.r).assessment, Some(fresh));
}

#[test]
fn rg12_a_pack_change_refuses_with_a_fresh_card() {
    let mut w = world();
    let p = ready_a(&mut w);
    use_pack(
        w.dir.path(),
        "other",
        "name: other\nversion: \"1\"\nextends: rdy-standard\n",
        "",
    );
    assert!(matches!(
        w.r.reload_pack().unwrap(),
        ReloadOutcome::Reloaded
    ));
    let e = approve(&mut w.r, &p).unwrap_err();
    let message = format!(
        "proposal {} moved: the pack changed after its assessment",
        p.r#ref
    );
    let fresh = assert_fresh_card(&w.r, &e, &p, &message);
    let a = w.r.read().reader().assessment(&fresh).unwrap();
    assert_eq!(a.pack.digest, pack_digest(w.r.read().pack()));
    assert_eq!(a.pack.name, "other");
    approve(&mut w.r, &p).expect("the next approval lands");
    assert_eq!(head_record(&w.r).assessment, Some(fresh));
}

/// The gate settings with `coverage.gate` off and the given `reviews.require_up_to_date`.
fn gate(require_up_to_date: bool) -> GateConfig {
    GateConfig {
        require_up_to_date,
        coverage_gate: CoverageGate::Off,
    }
}

#[test]
fn rg12_a_config_change_refuses_with_a_fresh_card() {
    let mut w = world();
    let p = ready_a(&mut w);
    let (_, first) = stored(&w.r, &p.id).unwrap();
    assert_eq!(first.config, config_digest(&gate(false)));
    let cfg = w.dir.path().join(".rdy.yaml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(
        &cfg,
        format!("{text}reviews:\n  require_up_to_date: true\n"),
    )
    .unwrap();
    assert!(matches!(
        w.r.reload_pack().unwrap(),
        ReloadOutcome::Reloaded
    ));
    let e = approve(&mut w.r, &p).unwrap_err();
    let message = format!(
        "proposal {} moved: the gate config changed after its assessment",
        p.r#ref
    );
    let fresh = assert_fresh_card(&w.r, &e, &p, &message);
    let a = w.r.read().reader().assessment(&fresh).unwrap();
    assert_eq!(a.config, config_digest(&gate(true)));
    approve(&mut w.r, &p).expect("the next approval lands");
    assert_eq!(head_record(&w.r).assessment, Some(fresh));
}

#[test]
fn rg12_a_fresh_readiness_that_fails_gives_issues_and_no_card() {
    let mut w = world();
    let p = ready_a(&mut w);
    // a landing that edits x too: the version now carries a conflict
    let l = land_edit(&mut w.r, "l", &w.x, "X holds three times.");
    let want = {
        let pref = w.r.resolve_proposal(&p.r#ref).unwrap();
        let st = w.r.read();
        let v = st.reader().proposal(&h(&p.id)).unwrap();
        readiness(&st, &Candidate::stored(&st, pref, v).unwrap())
            .unwrap()
            .issues
    };
    assert!(
        want.iter().any(|i| i.code == E_CONFLICT_UNRESOLVED),
        "{want:#?}"
    );
    let from = w.r.store().log_end();
    let e = approve(&mut w.r, &p).unwrap_err();
    assert_eq!(
        (e.code, e.message.clone(), e.fix.as_str()),
        (
            E_PROPOSAL_MOVED,
            format!(
                "proposal {} moved: landing {} changed {} after its assessment",
                p.r#ref,
                l.short(),
                w.x
            ),
            O11_FIX
        ),
        "{e:#?}"
    );
    assert_eq!(e.issues, want);
    assert!(e.corrected.is_none(), "no card: {e:#?}");
    assert_eq!(assessments_from(&w.r, from), 0, "no fresh assessment");
}

#[test]
fn rg12_require_up_to_date_gives_no_fresh_card() {
    let mut w = world();
    let cfg = w.dir.path().join(".rdy.yaml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(
        &cfg,
        format!("{text}reviews:\n  require_up_to_date: true\n"),
    )
    .unwrap();
    assert!(matches!(
        w.r.reload_pack().unwrap(),
        ReloadOutcome::Reloaded
    ));
    let p = ready_a(&mut w);
    let l = land_unrelated(&mut w.r);
    let from = w.r.store().log_end();
    let e = approve(&mut w.r, &p).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e:#?}");
    assert_eq!(
        e.message,
        format!(
            "proposal {} moved: canon has 1 landing(s) since it was proposed ({})",
            p.r#ref,
            l.short()
        )
    );
    assert_eq!(
        e.fix,
        "this project requires proposals on the current canon (reviews.require_up_to_date): its author must propose again (rdy propose --view a); then approve the new version"
    );
    assert!(e.corrected.is_none(), "no fresh card: {e:#?}");
    assert_eq!(assessments_from(&w.r, from), 0, "no new ASSESSMENT frame");
}

#[test]
fn rg12_the_landing_names_the_assessment() {
    let mut w = world();
    let p = ready_a(&mut w);
    approve(&mut w.r, &p).expect("land");
    let (a, _) = stored(&w.r, &p.id).unwrap();
    assert_eq!(Some(a.to_string()), p.assessment);
    let rec = head_record(&w.r);
    assert_eq!(rec.assessment, Some(a));
    assert_eq!(rec.approved_version, Some(h(&p.id)));
}

/// The confirm items beside batch `pref`.
fn exceptions_of(r: &Rdy, pref: &str) -> Vec<Item> {
    r.read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap()
        .items
        .into_iter()
        .filter(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(pref))
        .collect()
}

fn answer_local(r: &mut Rdy, item: &str, option: &str) -> String {
    r.answer(
        &approver(),
        PERSON,
        AnswerRequest {
            r#ref: item.into(),
            option: option.into(),
            ..AnswerRequest::default()
        },
    )
    .unwrap_or_else(|e| panic!("answer {item}: {e}"))
    .act
}

/// The refused approval of `id` and how many readiness runs and candidate builds it made.
fn counted_land(r: &mut Rdy, id: &str) -> (Result<LandInfo, RdyError>, u64, u64) {
    let (calls, builds) = (readiness_calls_for_tests(), candidate_builds_for_tests());
    let res = r.land_report(&approver(), id);
    (
        res,
        readiness_calls_for_tests() - calls,
        candidate_builds_for_tests() - builds,
    )
}

#[test]
fn rg12_a_later_refusal_is_not_ready() {
    let (_d, mut r, b) = rdy_qa::intake::batch_project();
    let one_way = exceptions_of(&r, &b.r#ref)
        .into_iter()
        .find(|i| i.r#ref.starts_with(&format!("{}@", b.one_way)))
        .expect("the one_way exception")
        .r#ref;
    assert_eq!(answer_local(&mut r, &one_way, "reject"), "reject_decision");
    let (res, calls, builds) = counted_land(&mut r, &b.r#ref);
    let e = res.unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_NOT_READY, "{e:#?}");
    assert_eq!(
        e.issues,
        vec![Issue {
            severity: Severity::Error,
            code: E_VALIDATION,
            block: Some(b.one_way.clone()),
            message: format!(
                "{} was rejected; drop or revise it, then propose again",
                b.one_way
            ),
            fix: format!(
                "in view {}, drop or revise {}, then propose again",
                rdy_qa::intake::BATCH_VIEW,
                b.one_way
            ),
        }]
    );
    assert_eq!((calls, builds), (0, 0), "built with no candidate (I8)");
}

/// Ruling task-12 R1 (D-WMCFW, D-ME2HD, D-JQD5X): a person's acceptance of a reviewer's advice
/// on a tension of the version, written after its assessment, is a later refusal. The approval
/// refuses with `E_PROPOSAL_NOT_READY` and one `E_BLOCKING_TENSION` issue per accepted tension,
/// read with no candidate, and nothing lands.
#[test]
fn rg12_an_accepted_advice_after_propose_is_a_later_refusal() {
    let (_d, mut r, b) = rdy_qa::intake::batch_project();
    let version = b.report["id"].as_str().unwrap().to_string();
    assert!(
        stored(&r, &version).is_some(),
        "the ready version has its assessment"
    );
    let accepted = r
        .accept_advice(
            &writer(),
            PERSON,
            AcceptRequest {
                proposal: b.r#ref.clone(),
                advised_digest: b.report["advised_digest"].as_str().unwrap().into(),
                ..Default::default()
            },
        )
        .unwrap()
        .accepted;
    assert!(!accepted.is_empty());
    let head = r.read().canon_head();
    let (res, calls, builds) = counted_land(&mut r, &b.r#ref);
    let e = res.unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_NOT_READY, "{e:#?}");
    assert_eq!(e.issues.len(), accepted.len(), "{:#?}", e.issues);
    for (issue, row) in e.issues.iter().zip(&accepted) {
        assert_eq!(issue.code, E_BLOCKING_TENSION, "{issue:#?}");
        assert!(
            issue.message.starts_with(&format!("{} semantic: ", row.id)),
            "{issue:#?}"
        );
        assert!(issue.fix.contains(&row.id), "{issue:#?}");
        assert!(issue.block.is_some(), "{issue:#?}");
    }
    assert_eq!((calls, builds), (0, 0), "built with no candidate (I8)");
    assert_eq!(r.read().canon_head(), head, "nothing landed");
}

#[test]
fn rg12_a_rejected_one_way_reaffirmation_is_a_later_refusal() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let w = write(
        &mut r,
        "rules",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
            {"op": "create", "type": "claim", "id": "$z", "parent": "$s", "body": "Z stands alone."}
        ]),
    )
    .unwrap();
    land(&mut r, "rules").unwrap();
    let (section, z) = (w.created["$s"].clone(), w.created["$z"].clone());
    let one = r
        .decide(
            &writer(),
            "one-way",
            DecisionInput {
                parent: section,
                statement: "Never drop Z".into(),
                depends_on: vec![z.clone()],
                reversibility: Some("one_way".into()),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "one-way").unwrap();
    write(
        &mut r,
        "edit-z",
        json!([{"op": "update", "id": z, "body": "Z stands still."}]),
    )
    .unwrap();
    land(&mut r, "edit-z").unwrap();
    write(
        &mut r,
        "reaffirm",
        json!([{"op": "reaffirm", "id": one, "targets": [z], "reason": "Z still fits."}]),
    )
    .unwrap();
    let p = propose(&mut r, "reaffirm", Some(TEST_SUMMARY));
    assert!(p.ready, "{:#?}", p.issues);
    let ex = exceptions_of(&r, &p.r#ref);
    assert_eq!(ex.len(), 1, "{ex:#?}");
    assert_eq!(ex[0].exception, Some(ExceptionKind::ReaffirmOneWay));
    assert_eq!(
        answer_local(&mut r, &ex[0].r#ref, "reject"),
        "reject_reaffirm"
    );
    let (res, calls, builds) = counted_land(&mut r, &p.id);
    let e = res.unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_NOT_READY, "{e:#?}");
    assert_eq!(
        e.issues,
        vec![Issue {
            severity: Severity::Error,
            code: E_VALIDATION,
            block: Some(BlockId::parse(&one).unwrap()),
            message: format!("{one}'s re-affirmation was rejected"),
            fix: "drop the re-affirmation and propose again".into(),
        }]
    );
    assert_eq!((calls, builds), (0, 0), "built with no candidate (I8)");
}

#[test]
fn rg12_an_approval_of_a_version_with_no_assessment_is_not_ready() {
    let mut w = world();
    put_j(&mut w.r, "a", upd(&w.x, "X holds twice."), j_typo());
    let p = propose(&mut w.r, "a", None);
    assert!(stored(&w.r, &p.id).is_none(), "not ready: no assessment");
    let e = approve(&mut w.r, &p).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_NOT_READY, "{e:#?}");
    assert_eq!(
        e.issues,
        vec![Issue {
            severity: Severity::Error,
            code: E_VALIDATION,
            block: None,
            message: format!("{} has no summary", p.r#ref),
            fix: "propose it again with a summary in plain words for the person who approves it"
                .into(),
        }],
        "its issue only, no O10 issue"
    );
}

#[test]
fn rg12_a_ready_version_with_no_assessment_gets_the_o10_issue() {
    let mut w = world();
    let r = &mut w.r;
    // b cites x; an edit of x lands, so b's version carries a blocking premise shift when it
    // is proposed
    put_j(
        r,
        "b",
        json!([{"op": "create", "type": "claim", "parent": w.t.to_string(),
                "body": format!("Z builds on {{{{{}}}}}.", w.x)}]),
        j_typo(),
    );
    land_edit(r, "e", &w.x, "X holds now.");
    let p = propose(r, "b", Some(TEST_SUMMARY));
    assert_eq!(
        p.issues.iter().map(|i| i.code).collect::<Vec<_>>(),
        vec![E_BLOCKING_TENSION],
        "{p:#?}"
    );
    assert!(stored(r, &p.id).is_none(), "not ready: no assessment");
    // a landing puts x back as b read it: the tension is gone, and the version is ready now
    land_edit(r, "f", &w.x, "X holds.");
    let e = approve(r, &p).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_NOT_READY, "{e:#?}");
    assert_eq!(
        e.issues,
        vec![Issue {
            severity: Severity::Error,
            code: E_VALIDATION,
            block: None,
            message: format!("{} has no stored assessment", p.r#ref),
            fix: "propose it again: rdy_propose / rdy propose".into(),
        }]
    );
    assert!(e.corrected.is_none(), "{e:#?}");
}

#[test]
fn rg12_landing_runs_no_readiness_when_fresh() {
    let mut w = world();
    // a version with no decision op: no readiness, no candidate build
    let p = ready_a(&mut w);
    let (res, calls, builds) = counted_land(&mut w.r, &p.id);
    res.expect("land a");
    assert_eq!((calls, builds), (0, 0));
    // a version with a decision: no readiness, one candidate build (the holds, I-b)
    let t = w.t.to_string();
    decide_in(
        &mut w.r,
        "d",
        DecisionInput {
            parent: t,
            statement: "Notes stay short".into(),
            title: Some("Short notes".into()),
            why: Some("short notes read fast".into()),
            reversibility: Some("two_way".into()),
            affects: vec![w.y.to_string()],
            ..Default::default()
        },
    );
    let d = propose(&mut w.r, "d", Some(TEST_SUMMARY));
    assert!(d.ready, "{:#?}", d.issues);
    confirm_before_landing(&mut w.r, &approver(), &d);
    let (res, calls, builds) = counted_land(&mut w.r, &d.id);
    res.expect("land d");
    assert_eq!((calls, builds), (0, 1));
}

#[test]
fn rg12_strict_still_refuses_any_moved_base() {
    let mut w = world();
    let p = ready_a(&mut w);
    let l = land_unrelated(&mut w.r);
    let from = w.r.store().log_end();
    let e =
        w.r.land_with(
            &approver(),
            &p.id,
            LandOptions {
                strict: true,
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(
        (e.code, e.message.clone(), e.fix.as_str()),
        (
            E_PROPOSAL_MOVED,
            format!(
                "proposal {} moved: canon has 1 landing(s) since it was proposed ({})",
                p.r#ref,
                l.short()
            ),
            "approve without --strict to accept a clean rebase, or ask its author to propose again on the current canon"
        )
    );
    assert!(e.corrected.is_none(), "no fresh card: {e:#?}");
    assert_eq!(assessments_from(&w.r, from), 0);
    approve(&mut w.r, &p).expect("without --strict it lands");
}

fn on_engine<T: Send + 'static>(
    e: &dyn Engine,
    f: impl FnOnce(&mut Rdy) -> T + Send + 'static,
) -> T {
    with_writer_result(e, move |r| Ok(f(r))).expect("on the engine")
}

fn relayed_approve(st: &mut McpState, pref: &str, card_hash: &str) -> Result<Value, RdyError> {
    rdy_answer(
        st,
        serde_json::from_value(json!({
            "ref": pref, "option": "approve", "card_hash": card_hash,
            "evidence": "Yes, land it.", "question": "Shall I land it?", "session_ref": "chat-7",
        }))
        .unwrap(),
    )
}

#[test]
fn rg12_rdy_answer_carries_the_fresh_card_in_corrected() {
    let mut w = world();
    let p = ready_a(&mut w);
    let shown = approve_item(&w.r, &p.r#ref).card_hash;
    let body = format!("Y builds on {{{{{}}}}} twice.", w.x);
    let l = land_edit(&mut w.r, "l", &w.y, &body);
    let World {
        dir: _dir, r, y, ..
    } = w;
    let engine = Arc::new(Local::new(r));
    let mut st = McpState::new(engine.clone());
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:relay", "run": "r1", "role": "writer"}))
            .unwrap(),
    )
    .unwrap();
    let e = relayed_approve(&mut st, &p.r#ref, &shown).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e:#?}");
    assert_eq!(
        e.message,
        format!(
            "proposal {} moved: landing {} changed {y} after its assessment",
            p.r#ref,
            l.short()
        )
    );
    assert_eq!(e.fix, fresh_fix(&p.r#ref));
    let card = e.corrected.as_ref().expect("the fresh card")["card"].clone();
    assert_eq!(card["ref"], json!(p.r#ref), "{e:#?}");
    let id = p.id.clone();
    let fresh = on_engine(&*engine, move |r| stored(r, &id).map(|(a, _)| a));
    assert_ne!(fresh.map(|a| a.to_string()), p.assessment, "a fresh record");
    // the approval of the fresh card lands and names the fresh assessment
    let v = relayed_approve(&mut st, &p.r#ref, card["card_hash"].as_str().unwrap())
        .expect("the fresh card's approval lands");
    assert!(v["landing"].is_object(), "{v:#}");
    let rec = on_engine(&*engine, |r| head_record(r));
    assert_eq!(rec.assessment, fresh);
}

#[test]
fn rg12_a_reaffirm_card_landing_names_no_assessment() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."}
        ]),
    )
    .unwrap();
    land(&mut r, "seed").unwrap();
    let (s, a) = (w.created["$s"].clone(), w.created["$a"].clone());
    r.decide(
        &writer(),
        "d-one",
        DecisionInput {
            parent: s,
            statement: "One rule".into(),
            title: Some("One".into()),
            depends_on: vec![a.clone()],
            reversibility: Some("two_way".into()),
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "d-one").unwrap();
    write(
        &mut r,
        "x",
        json!([{"op": "update", "id": a, "body": "A holds 60 days."}]),
    )
    .unwrap();
    land(&mut r, "x").unwrap();
    let card = r
        .read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap()
        .items
        .into_iter()
        .find(|i| i.kind == ItemKind::Reaffirm)
        .expect("the re-affirmation card");
    let info = r
        .answer(
            &approver(),
            PERSON,
            AnswerRequest {
                r#ref: card.r#ref.clone(),
                option: "o1".into(),
                card_hash: Some(card.card_hash.clone()),
                ..AnswerRequest::default()
            },
        )
        .expect("They still hold");
    let landing: Hash = info.landing.expect("a landing").landing.parse().unwrap();
    assert_eq!(r.read().canon_head(), Some(landing));
    let rec = head_record(&r);
    assert_eq!(rec.assessment, None, "C7: no assessment");
    assert!(stored(&r, &rec.proposal.to_string()).is_none());
}
