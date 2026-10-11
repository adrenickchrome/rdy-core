//! se7: rdy_review_submit: the verdict, the schema, the case hash, precedence and actor independence.
use rdy_engine::semantic::candidates::request_label;
use rdy_engine::semantic::case::case_hash;
use rdy_engine::semantic::submit::*;
use rdy_engine::tensions::*;
use rdy_engine::{
    E_BAD_INPUT, E_PERMISSION, E_REVIEW_INVALID, E_REVIEW_STALE, E_SELF_REVIEW, Rdy, Session,
};
use rdy_model::{BlockId, Role, Verdict};
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use std::collections::BTreeSet;

fn reviewer(actor: &str) -> Session {
    Session::agent(actor.into(), Some("r".into()), Role::Reviewer).unwrap()
}
/// The first request agent:r may take, its id and case hash.
fn take(r: &Rdy) -> (String, String, Vec<BlockId>) {
    let st = r.read();
    let q = st
        .next_request("agent:r", &BTreeSet::new())
        .unwrap()
        .unwrap();
    let h = case_hash(&st.case_of(&q.key, &q.items).unwrap());
    (
        request_label(&q.key),
        h.to_string(),
        q.items.iter().map(|i| i.block.clone()).collect(),
    )
}
fn request(req: &str, hash: &str, verdict: &str, on: &[BlockId]) -> ReviewSubmitRequest {
    ReviewSubmitRequest {
        request: req.into(),
        case_hash: hash.into(),
        verdict: verdict.into(),
        findings: on
            .iter()
            .map(|b| FindingRequest {
                block: b.to_string(),
                note: "these two cannot both hold".into(),
            })
            .collect(),
        severity: None,
        reason: None,
    }
}
fn submit(
    r: &mut Rdy,
    who: &Session,
    req: &str,
    hash: &str,
    verdict: &str,
    on: &[BlockId],
) -> Result<SubmitInfo, rdy_engine::RdyError> {
    r.submit_verdict(who, request(req, hash, verdict, on))
}
fn edited_c() -> (tempfile::TempDir, Rdy, Spec) {
    let (d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.c, "C contradicts B."));
    (d, r, s)
}

#[test]
fn se7_a_tension_verdict_opens_an_advisory_tension_and_releases_the_lease() {
    let (_d, mut r, _s) = edited_c();
    let (rq, h, blocks) = take(&r);
    let q = r.read().review_requests().unwrap()[0].clone();
    let pair = q
        .items
        .iter()
        .map(|i| (i.block.clone(), i.content))
        .collect();
    let now = rdy_engine::now_ms();
    assert!(r.leases().claim(q.key, pair, 5, now, 600_000));
    let info = submit(
        &mut r,
        &reviewer("agent:r"),
        &rq,
        &h,
        "tension",
        &blocks[..1],
    )
    .unwrap();
    assert_eq!(
        (info.verdict.as_str(), info.severity),
        ("tension", Some(TensionSeverity::Advisory))
    );
    let t = one(&r, TensionKind::Semantic);
    assert_eq!(info.tension, Some(tension_label(&t.id)));
    assert!(
        r.leases().leased(now).is_empty(),
        "the submit released the lease"
    );
    let review = r.store().review(&info.review.parse().unwrap()).unwrap();
    assert_eq!(review.verdict, Verdict::Tension);
    assert_eq!(review.findings.len(), 1);
}

#[test]
fn se7_no_tension_suppresses_the_pair_until_a_side_changes() {
    let (_d, mut r, s) = edited_c();
    let n = r.read().review_requests().unwrap().len();
    let (rq, h, _) = take(&r);
    let info = submit(&mut r, &reviewer("agent:r"), &rq, &h, "no_tension", &[]).unwrap();
    assert!(info.suppressed && info.tension.is_none());
    assert_eq!(r.read().review_requests().unwrap().len(), n - 1);
    assert!(of_kind(&r, TensionKind::Semantic).is_empty());
    put(&mut r, "a", upd(&s.c, "C contradicts B again."));
    assert_eq!(
        r.read().review_requests().unwrap().len(),
        n,
        "a content change makes a new pair"
    );
}

#[test]
fn se7_precedence_tension_wins_and_nothing_else_overrides() {
    let (_d, mut r, _s) = edited_c();
    let (rq, h, blocks) = take(&r);
    submit(&mut r, &reviewer("agent:r"), &rq, &h, "no_tension", &[]).unwrap();
    let e = submit(&mut r, &reviewer("agent:r2"), &rq, &h, "no_tension", &[]).unwrap_err();
    assert_eq!(e.code, E_REVIEW_STALE);
    assert!(
        e.message.ends_with("by agent:r found no tension"),
        "{}",
        e.message
    );
    submit(
        &mut r,
        &reviewer("agent:r2"),
        &rq,
        &h,
        "tension",
        &blocks[..1],
    )
    .unwrap();
    assert_eq!(
        of_kind(&r, TensionKind::Semantic).len(),
        1,
        "a tension over a no_tension opens it"
    );
    for v in ["tension", "no_tension"] {
        let e = submit(&mut r, &reviewer("agent:r3"), &rq, &h, v, &blocks[..1]).unwrap_err();
        assert!(
            e.message.ends_with("by agent:r2 found a tension"),
            "{}",
            e.message
        );
    }
}

#[test]
fn se7_a_submit_without_a_lease_is_accepted_when_current() {
    let (_d, mut r, _s) = edited_c();
    let (rq, h, blocks) = take(&r);
    assert!(r.leases().leased(0).is_empty());
    submit(
        &mut r,
        &reviewer("agent:r"),
        &rq,
        &h,
        "tension",
        &blocks[..1],
    )
    .unwrap();
}

#[test]
fn se7_a_stale_case_hash_or_a_landed_pair_is_e_review_stale() {
    let (_d, mut r, s) = edited_c();
    let (rq, h, blocks) = take(&r);
    put(
        &mut r,
        "a",
        serde_json::json!([{"op": "update", "id": s.section.to_string(), "fields": {"title": "Rules, renamed"}}]),
    );
    let e = submit(
        &mut r,
        &reviewer("agent:r"),
        &rq,
        &h,
        "tension",
        &blocks[..1],
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.message.clone()),
        (
            E_REVIEW_STALE,
            format!("the case of {rq} changed since you read it")
        )
    );
    let (rq, h, blocks) = take(&r);
    land(&mut r, "a").unwrap();
    let e = submit(
        &mut r,
        &reviewer("agent:r"),
        &rq,
        &h,
        "tension",
        &blocks[..1],
    )
    .unwrap_err();
    assert_eq!(e.code, E_REVIEW_STALE);
    assert!(
        e.message == format!("request {rq} ended: both blocks are in canon now")
            || e.message
                .starts_with(&format!("request {rq} is no longer current")),
        "{}",
        e.message
    );
}

#[test]
fn se7_the_schema_is_checked() {
    let (_d, mut r, s) = edited_c();
    let (rq, h, blocks) = take(&r);
    let who = reviewer("agent:r");
    assert_eq!(
        submit(&mut r, &who, &rq, &h, "maybe", &[])
            .unwrap_err()
            .code,
        E_REVIEW_INVALID
    );
    let e = submit(&mut r, &who, &rq, &h, "tension", &[]).unwrap_err();
    assert_eq!(e.message, "a tension verdict needs at least one finding");
    let e = submit(
        &mut r,
        &who,
        &rq,
        &h,
        "tension",
        std::slice::from_ref(&s.doc),
    )
    .unwrap_err();
    assert_eq!(
        e.message,
        format!(
            "finding 1: block {} is not one of {} and {}",
            s.doc, blocks[0], blocks[1]
        )
    );
    let many: Vec<BlockId> = std::iter::repeat_n(blocks[0].clone(), 21).collect();
    assert_eq!(
        submit(&mut r, &who, &rq, &h, "tension", &many)
            .unwrap_err()
            .message,
        "a verdict takes at most 20 findings"
    );
    let mut blank = request(&rq, &h, "tension", &blocks[..1]);
    blank.findings[0].note = "  ".into();
    let e = r.submit_verdict(&who, blank).unwrap_err();
    assert_eq!(e.message, "finding 1: a note needs 1 to 2000 characters");
    assert_eq!(
        submit(&mut r, &who, "rq-xyz", &h, "tension", &blocks[..1])
            .unwrap_err()
            .code,
        E_BAD_INPUT
    );
    assert_eq!(
        submit(&mut r, &who, &rq, "abc", "tension", &blocks[..1])
            .unwrap_err()
            .code,
        E_BAD_INPUT
    );
    assert_eq!(
        submit(&mut r, &ag("w"), &rq, &h, "tension", &blocks[..1])
            .unwrap_err()
            .code,
        E_PERMISSION
    );
}

#[test]
fn se7_advice_to_block_is_stored_and_gates_nothing() {
    let (_d, mut r, _s) = edited_c();
    let (rq, h, blocks) = take(&r);
    let who = reviewer("agent:r");
    let mut q = request(&rq, &h, "tension", &blocks[..1]);
    q.severity = Some("blocking".into());
    let e = r.submit_verdict(&who, q.clone()).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            E_REVIEW_INVALID,
            "advice to block needs a reason of 1 to 2000 characters"
        )
    );
    let mut nt = request(&rq, &h, "no_tension", &[]);
    nt.severity = Some("blocking".into());
    nt.reason = Some("x".into());
    assert_eq!(
        r.submit_verdict(&who, nt).unwrap_err().message,
        "severity applies to a tension verdict only"
    );
    let mut odd = request(&rq, &h, "tension", &blocks[..1]);
    odd.severity = Some("urgent".into());
    assert_eq!(
        r.submit_verdict(&who, odd).unwrap_err().message,
        "severity \"urgent\" is not advisory or blocking"
    );
    let mut stray = request(&rq, &h, "tension", &blocks[..1]);
    stray.reason = Some("x".into());
    assert_eq!(
        r.submit_verdict(&who, stray).unwrap_err().code,
        E_REVIEW_INVALID
    );
    q.reason = Some("  it contradicts the gate rule ".into());
    let info = r.submit_verdict(&who, q).unwrap();
    assert_eq!(
        info.severity,
        Some(TensionSeverity::Advisory),
        "advice gates nothing"
    );
    assert_eq!(info.advice.as_deref(), Some("it contradicts the gate rule"));
    assert_eq!(
        one(&r, TensionKind::Semantic).severity,
        TensionSeverity::Advisory
    );
    let key = rdy_engine::tensions::semantic::pair_of(&one(&r, TensionKind::Semantic)).unwrap();
    let st = r.read();
    let rec = &st.semantic().tension(&key).unwrap().record;
    assert_eq!(
        (rec.advice.as_deref(), rec.blocking),
        (Some("it contradicts the gate rule"), false)
    );
    assert_eq!(
        r.store().review(&rec.review).unwrap().reason,
        "",
        "the review's reason stays empty (O5)"
    );
}

#[test]
fn se7_the_author_of_either_block_cannot_judge_the_pair() {
    let (_d, mut r, _s) = edited_c();
    let (rq, h, blocks) = take(&r);
    // agent:a wrote C (another run of the same actor is not independent)
    let e = submit(
        &mut r,
        &Session::agent("agent:a".into(), Some("other".into()), Role::Reviewer).unwrap(),
        &rq,
        &h,
        "tension",
        &blocks[..1],
    )
    .unwrap_err();
    assert_eq!(e.code, E_SELF_REVIEW);
    // human:qa landed the canon side (spec_doc writes as the human writer)
    let e = submit(
        &mut r,
        &reviewer("human:qa"),
        &rq,
        &h,
        "tension",
        &blocks[..1],
    )
    .unwrap_err();
    assert_eq!(e.code, E_SELF_REVIEW);
    submit(
        &mut r,
        &reviewer("agent:r"),
        &rq,
        &h,
        "tension",
        &blocks[..1],
    )
    .unwrap();
}

#[test]
fn se7_a_verdict_is_a_review_submitted_event() {
    let (_d, mut r, _s) = edited_c();
    let (rq, h, blocks) = take(&r);
    let rx = r.subscribe();
    let info = submit(
        &mut r,
        &reviewer("agent:r"),
        &rq,
        &h,
        "tension",
        &blocks[..1],
    )
    .unwrap();
    let kinds: Vec<_> = rx.try_iter().map(|e| e.kind.clone()).collect();
    assert!(
        kinds.iter().any(
            |k| matches!(k, rdy_engine::EventKind::ReviewSubmitted { review, verdict, .. }
        if review.to_string() == info.review && verdict == "tension")
        ),
        "{kinds:?}"
    );
}

/// The daemon resolves a lease-less submit's request on a read: the writer computes no
/// candidate requests (global constraint; FW-PF2/SW3: write-path cost must not grow with the
/// corpus).
#[test]
fn se7_a_lease_less_daemon_submit_computes_no_requests_on_the_writer() {
    use rdy_engine::engine::{Engine, write_typed};
    use rdy_engine::semantic::candidates::request_builds;
    let (_d, r, _s) = edited_c();
    let (rq, h, blocks) = take(&r);
    assert!(r.leases().leased(0).is_empty());
    let shared = rdy_daemon::shared::Shared::new(r);
    let e = shared.engine();
    let on_writer = |e: &dyn Engine| {
        write_typed(e, |_| Ok(request_builds()))
            .unwrap()
            .as_u64()
            .unwrap()
    };
    let (writer_before, here_before) = (on_writer(&*e), request_builds());
    let v = rdy_engine::semantic::api::run_review_submit(
        &*e,
        &reviewer("agent:r"),
        request(&rq, &h, "tension", &blocks[..1]),
    )
    .unwrap();
    assert!(v["tension"].is_string(), "{v}");
    assert_eq!(
        on_writer(&*e),
        writer_before,
        "the writer computed candidate requests"
    );
    assert!(
        request_builds() > here_before,
        "the lookup ran on the caller's read"
    );
    shared.shutdown().unwrap();
}

/// Fails every detection once a semantic tension is recorded: the tension set of the state
/// after a `tension` verdict cannot be built.
struct FailsOnceATensionIsRecorded;
impl Detector for FailsOnceATensionIsRecorded {
    fn kind(&self) -> TensionKind {
        TensionKind::Semantic
    }
    fn stored(&self) -> bool {
        true
    }
    fn detect(
        &self,
        cx: &DetectCx<'_>,
        _seeds: Option<&BTreeSet<BlockId>>,
        _out: &mut Vec<Found>,
    ) -> Result<(), rdy_engine::RdyError> {
        if cx.st.semantic().has_tensions() {
            return Err(rdy_engine::RdyError::new(
                rdy_engine::E_IO,
                "boom",
                "a test detector",
            ));
        }
        Ok(())
    }
}
static FAILS_ONCE_RECORDED: FailsOnceATensionIsRecorded = FailsOnceATensionIsRecorded;
static FAILS_ONCE_RECORDED_DETS: [&dyn Detector; 1] = [&FAILS_ONCE_RECORDED];

/// The reply after the write never fails a stored verdict: a failed tension lookup gives no
/// tension label, and the review id stands.
#[test]
fn se7_a_failed_lookup_after_the_write_still_answers_with_the_review() {
    let (_d, mut r, _s) = edited_c();
    r.set_detectors_for_tests(&FAILS_ONCE_RECORDED_DETS);
    let (rq, h, blocks) = take(&r);
    let info = submit(
        &mut r,
        &reviewer("agent:r"),
        &rq,
        &h,
        "tension",
        &blocks[..1],
    )
    .unwrap();
    assert_eq!(info.tension, None, "the lookup failed");
    assert_eq!(info.severity, None, "no tension, so no severity");
    let review = r.store().review(&info.review.parse().unwrap()).unwrap();
    assert_eq!(review.verdict, Verdict::Tension, "the verdict is stored");
    assert!(r.read().tensions().is_err(), "the lookup does fail");
}

/// The writer re-proves a key found on a read: a pair that changed between the lookup and the
/// write is refused as no longer current, and nothing is recorded.
#[test]
fn se7_a_pair_that_changes_after_the_lookup_is_e_review_stale() {
    let (_d, mut r, s) = edited_c();
    let (rq, h, blocks) = take(&r);
    let who = reviewer("agent:r");
    let checked = CheckedSubmit::new(&who, request(&rq, &h, "tension", &blocks[..1])).unwrap();
    let found = r
        .read()
        .find_request(&r.leases(), &checked, rdy_engine::now_ms())
        .unwrap();
    put(&mut r, "a", upd(&s.c, "C contradicts B, rewritten."));
    let e = r.submit_found(checked, found).unwrap_err();
    assert_eq!(e.code, E_REVIEW_STALE);
    assert_eq!(
        e.message,
        format!("request {rq} is no longer current: a side changed, landed or left every view")
    );
    assert!(of_kind(&r, TensionKind::Semantic).is_empty());
}
