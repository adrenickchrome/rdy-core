//! di17 (plan Decision intake, Task 17; D-DKJS5 F2, D-T5S9A): every approval path waits for the
//! exceptions. `land_with` refuses with `E_EXCEPTION_OPEN` while an exception of the version has
//! no `confirm` answer, so `rdy approve`, a relayed approval, `rdy_answer` and
//! `rdy answer --all-recommended` all wait. Every earlier refusal keeps its code and comes first.
use rdy_engine::answer::AnswerRequest;
use rdy_engine::engine::Local;
use rdy_engine::pending::{CARD_MAX_TOKENS, ExceptionKind, Item, ItemKind, PendingQuery};
use rdy_engine::{
    DecisionInput, E_CONFLICT_UNRESOLVED, E_EXCEPTION_OPEN, E_PROPOSAL_MOVED, E_PROPOSAL_NOT_READY,
    E_VALIDATION, LandOptions, Rdy, RdyError, Scope,
};
use rdy_mcp::tools::{McpState, rdy_answer, rdy_pending, rdy_session};
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::{approver, land};
use rdy_qa::intake::*;
use rdy_qa::tensions::{ag, cite, put, upd};
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::sync::Arc;

/// The view of the guest proposal, and its summary.
const GUEST: &str = "guest";
const SUMMARY: &str = "Shows guests a banner.";

/// A decision under the spec's section that no content applies (an `unapplied` exception).
fn unapplied(s: &Spec, statement: &str) -> DecisionInput {
    DecisionInput {
        affects: vec![],
        ..decision(s, statement, statement)
    }
}

/// Proposes view `view` as `agent:<view>` with a summary; the version's `P-x@n`.
fn propose(r: &mut Rdy, view: &str) -> String {
    r.propose_subset(&ag(view), view, view, None, Some(SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e}"))
        .r#ref
}

/// The spec doc in canon, then view [`GUEST`] decides one decision that no content applies and
/// proposes it with a summary: the version's ref and the decision.
fn guest(r: &mut Rdy) -> (String, rdy_model::BlockId) {
    let spec = spec_doc(r);
    let d = decide_in(r, GUEST, unapplied(&spec, "Guests see a banner"));
    (propose(r, GUEST), d)
}

/// The exception items beside batch `pref`, in queue order, as the person sees them.
fn exceptions_of(r: &Rdy, pref: &str) -> Vec<Item> {
    pending(r)
        .items
        .into_iter()
        .filter(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(pref))
        .collect()
}

fn card_json(i: &Item) -> Value {
    json!({"ref": i.r#ref, "card_hash": i.card_hash, "card": i.card})
}

/// Checks `e` is the F2 refusal of `pref` naming each of `ex` with its card.
fn exception_open(e: &RdyError, pref: &str, ex: &[Item]) {
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            E_EXCEPTION_OPEN,
            format!("{} exceptions of {pref} have no answer", ex.len()).as_str()
        ),
        "{e}"
    );
    let refs: Vec<&str> = ex.iter().map(|i| i.r#ref.as_str()).collect();
    assert_eq!(
        e.fix,
        format!("answer each exception card first: {}", refs.join(", "))
    );
    let cards: Vec<Value> = ex.iter().map(card_json).collect();
    assert_eq!(e.corrected, Some(json!({"exceptions": cards})));
}

fn log_end(r: &Rdy) -> Location {
    r.store().log_end()
}

/// Runs `f` on the served project with the daemon stopped (it holds the store while it runs).
fn with_rdy<T>(s: &Served, f: impl FnOnce(&mut Rdy) -> T) -> T {
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let out = f(&mut r);
    r.close().unwrap();
    out
}

/// A served project holding [`guest`]: the version's ref, the decision, and the exception's item
/// ref with its card as the person's queue shows it.
fn served_guest() -> (Served, String, rdy_model::BlockId, Item) {
    let s = Served::new();
    let (pref, d, ex) = with_rdy(&s, |r| {
        let (pref, d) = guest(r);
        let ex = exceptions_of(r, &pref);
        assert_eq!(ex.len(), 1, "{ex:#?}");
        (pref, d, ex[0].clone())
    });
    (s, pref, d, ex)
}

#[test]
fn di17_rdy_approve_refuses_an_open_exception_too() {
    let (s, pref, d, ex) = served_guest();
    let before = with_rdy(&s, |r| log_end(r));
    // the person's own rdy approve waits for the exception, and shows its card
    let (ok, e) = s.json(&["--json", "approve", &pref]);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_EXCEPTION_OPEN", "{e}");
    assert_eq!(
        e["message"],
        format!("1 exceptions of {pref} have no answer"),
        "{e}"
    );
    assert_eq!(
        e["fix"],
        format!("answer each exception card first: {}", ex.r#ref),
        "{e}"
    );
    assert_eq!(
        e["corrected"],
        json!({"exceptions": [card_json(&ex)]}),
        "{e}"
    );
    let out = s.rdy(&["approve", &pref]);
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains(&ex.card), "the person sees the card: {text}");
    assert_eq!(
        with_rdy(&s, |r| log_end(r)),
        before,
        "a refusal writes nothing"
    );
    // the person confirms the exception, then the approval lands
    let (ok, v) = s.json(&["--json", "answer", &ex.r#ref, "confirm"]);
    assert!(ok, "{v}");
    assert_eq!(v["act"], "confirm", "{v}");
    let (ok, v) = s.json(&["--json", "approve", &pref]);
    assert!(ok, "{v}");
    assert_eq!(v["approved"], pref.as_str(), "{v}");
    with_rdy(&s, |r| {
        let canon = r.read().materialized(&Scope::Canon).unwrap();
        assert_eq!(canon.get(&d).unwrap().field_str("status"), Some("accepted"));
        r.fsck().expect("fsck");
    });
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

#[test]
fn di17_every_approval_path_waits_for_the_exceptions() {
    // in process: a local land_with, then rdy_answer's relayed approval
    let (d, mut r, b) = batch_project();
    std::fs::write(d.path().join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
    let ex = exceptions_of(&r, &b.r#ref);
    assert_eq!(ex.len(), 4, "{ex:#?}");
    let before = log_end(&r);
    let e = r
        .land_with(&approver(), &b.r#ref, LandOptions::default())
        .unwrap_err();
    exception_open(&e, &b.r#ref, &ex);
    assert_eq!(log_end(&r), before, "a refused landing writes nothing");
    // rdy_session may record the session: read the log end after it
    let mut st = agent_state(r);
    let published = st.engine.read().seq();
    let q = rdy_pending(&mut st, serde_json::from_value(json!({})).unwrap()).unwrap();
    let batch = q["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["ref"] == b.r#ref.as_str())
        .expect("the batch card")
        .clone();
    let e = rdy_answer(
        &mut st,
        serde_json::from_value(json!({
            "ref": b.r#ref, "option": "approve", "card_hash": batch["card_hash"],
            "evidence": "Yes, land it.", "question": "Shall I land it?", "session_ref": "chat-7",
        }))
        .unwrap(),
    )
    .unwrap_err();
    assert_eq!(e.code, E_EXCEPTION_OPEN, "{e}");
    assert_eq!(
        e.corrected.as_ref().unwrap()["exceptions"],
        json!(ex.iter().map(card_json).collect::<Vec<_>>())
    );
    assert_eq!(
        st.engine.read().seq(),
        published,
        "a refused answer writes nothing"
    );
    // through the binary: a relayed rdy approve from an agent session, and the person's own
    // rdy answer --all-recommended over the batch alone
    let (s, pref, _, ex) = served_guest();
    let before = with_rdy(&s, |r| log_end(r));
    let (ok, e) = s.json_as(
        "agent:relay",
        Some("r1"),
        &[
            "--json",
            "approve",
            &pref,
            "--relayed-by",
            "agent:relay",
            "--evidence",
            "Yes, land it.",
        ],
    );
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_EXCEPTION_OPEN", "{e}");
    assert_eq!(e["corrected"]["exceptions"], json!([card_json(&ex)]), "{e}");
    let (ok, e) = s.json(&["--json", "answer", "--all-recommended", &pref]);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_EXCEPTION_OPEN", "{e}");
    assert_eq!(e["corrected"]["exceptions"], json!([card_json(&ex)]), "{e}");
    assert_eq!(with_rdy(&s, |r| log_end(r)), before, "nothing is written");
}

#[test]
fn di17_refusals_keep_their_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let spec = spec_doc(&mut r);
    // a conflict: view x edits C from canon's version and decides an unapplied decision; another
    // view edits C too, and lands
    put(&mut r, "x", upd(&spec.c, "C as x has it."));
    put(&mut r, "edit-c", upd(&spec.c, "C as edit-c has it."));
    land(&mut r, "edit-c").expect("land edit-c");
    decide_in(&mut r, "x", unapplied(&spec, "Guests see a banner"));
    let x = propose(&mut r, "x");
    let before = log_end(&r);
    let e = r
        .land_with(&approver(), &x, LandOptions::default())
        .unwrap_err();
    // D-ME2HD: the conflict is a readiness issue, which comes before the exception hold
    assert_eq!(e.code, E_PROPOSAL_NOT_READY, "{e}");
    let nr = pending(&r)
        .not_ready
        .into_iter()
        .find(|n| n.r#ref == x)
        .expect("x is not ready");
    assert_eq!(e.issues, nr.issues, "{e}");
    assert_eq!(
        e.issues.iter().map(|i| i.code).collect::<Vec<_>>(),
        vec![E_CONFLICT_UNRESOLVED]
    );
    // a version that moved: view y proposes, then proposes again
    decide_in(&mut r, "y", unapplied(&spec, "Guests see a footer"));
    let first = propose(&mut r, "y");
    put(
        &mut r,
        "y",
        json!([{"op": "create", "type": "claim", "parent": spec.section.to_string(),
                "body": "A claim of y."}]),
    );
    let second = propose(&mut r, "y");
    assert_ne!(first, second);
    let before_y = log_end(&r);
    let e = r
        .land_with(&approver(), &first, LandOptions::default())
        .unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e}");
    // the latest version, with no earlier refusal, waits for its exception
    let ex = exceptions_of(&r, &second);
    assert_eq!(ex.len(), 1, "{ex:#?}");
    let e = r
        .land_with(&approver(), &second, LandOptions::default())
        .unwrap_err();
    exception_open(&e, &second, &ex);
    assert_eq!(log_end(&r), before_y, "nothing is written");
    assert!(before <= before_y);
}

/// A guard: a version whose decisions carry no exception lands as it did before F2 moved into
/// `land_with`, and so does a version with no decision.
#[test]
fn di17_a_version_with_no_exception_lands_as_before() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let spec = spec_doc(&mut r);
    let d = decide_in(
        &mut r,
        GUEST,
        decision(&spec, "Guests read C", "Guests read C"),
    );
    let pref = propose(&mut r, GUEST);
    assert!(exceptions_of(&r, &pref).is_empty());
    let q = r
        .read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap();
    assert!(
        q.items.iter().any(|i| i.r#ref == pref),
        "the batch is ready"
    );
    let info = r
        .land_with(&approver(), &pref, LandOptions::default())
        .expect("lands");
    assert_eq!(info.approved, pref);
    let canon = r.read().materialized(&Scope::Canon).unwrap();
    assert_eq!(canon.get(&d).unwrap().field_str("status"), Some("accepted"));
    // a version with no decision
    put(
        &mut r,
        "plain",
        json!([{"op": "create", "type": "claim", "parent": spec.section.to_string(),
                "body": "A plain claim."}]),
    );
    land(&mut r, "plain").expect("a version with no decision lands");
    r.fsck().expect("fsck");
}

/// Ruling task-17, option D: the queue lists a version's exception cards only once the version is
/// ready, and readiness needs a summary (D-4PG9Y, D-3JZP4). So a version with no summary is not
/// ready: its approval is refused with the summary issue, before the exception hold (D-ME2HD).
/// Once it has one, the fix names each card.
#[test]
fn di17_a_version_with_no_summary_is_told_to_propose_again_with_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let spec = spec_doc(&mut r);
    let d = decide_in(&mut r, GUEST, unapplied(&spec, "Guests see a banner"));
    let bare = r
        .propose_subset(&ag(GUEST), GUEST, GUEST, None, None)
        .unwrap()
        .r#ref;
    assert!(
        exceptions_of(&r, &bare).is_empty(),
        "a version with no summary is not ready, so its exceptions are no items"
    );
    let before = log_end(&r);
    let e = r
        .land_with(&approver(), &bare, LandOptions::default())
        .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_PROPOSAL_NOT_READY,
            format!("{bare} is not ready: 1 issues").as_str(),
            "fix each issue and propose again: rdy_propose / rdy propose"
        ),
        "{e}"
    );
    let nr = pending(&r)
        .not_ready
        .into_iter()
        .find(|n| n.r#ref == bare)
        .expect("the bare version is not ready");
    assert_eq!(e.issues, nr.issues, "{e}");
    assert_eq!(
        (e.issues[0].code, e.issues[0].message.as_str()),
        (E_VALIDATION, format!("{bare} has no summary").as_str())
    );
    assert!(e.corrected.is_none(), "no card: {e}");
    assert_eq!(log_end(&r), before, "a refused landing writes nothing");
    // proposed again with a summary, the version is ready and the fix names each card
    let pref = propose(&mut r, GUEST);
    assert_ne!(pref, bare);
    let ex = exceptions_of(&r, &pref);
    assert_eq!(ex.len(), 1, "{ex:#?}");
    assert!(ex[0].r#ref.starts_with(&format!("{d}@")), "{ex:#?}");
    let e = r
        .land_with(&approver(), &pref, LandOptions::default())
        .unwrap_err();
    exception_open(&e, &pref, &ex);
    confirm_exceptions(&mut r, &approver(), &pref);
    r.land_with(&approver(), &pref, LandOptions::default())
        .expect("lands once the exception is confirmed");
}

/// Final review fix round 1 (R1; D-DKJS5 F1): a decision the person rejected never lands while
/// it is the same version, even once it is no longer an exception. Here the view applies an
/// unapplied decision the person rejected and proposes again without changing it: the version is
/// not ready, and the landing refuses with the same issue the queue gives (one rule for both),
/// on every approval path. Once the author revises the decision, the version lands as before.
#[test]
fn di17_a_rejected_decision_never_lands_once_it_stops_being_an_exception() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    std::fs::write(dir.path().join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
    let spec = spec_doc(&mut r);
    let d = decide_in(&mut r, GUEST, unapplied(&spec, "Guests see a banner"));
    let first = propose(&mut r, GUEST);
    let ex = exceptions_of(&r, &first);
    assert_eq!(ex.len(), 1, "{ex:#?}");
    assert_eq!(ex[0].exception, Some(ExceptionKind::Unapplied));
    let reject = |r: &mut Rdy, item: &str| {
        r.answer(
            &approver(),
            PERSON,
            AnswerRequest {
                r#ref: item.into(),
                option: "reject".into(),
                ..AnswerRequest::default()
            },
        )
        .expect("the person rejects the decision")
    };
    assert_eq!(reject(&mut r, &ex[0].r#ref).act, "reject_decision");
    // the view applies the decision, unchanged, and proposes again: no exception is left
    put(&mut r, GUEST, cite(&spec.section, &d));
    let second = propose(&mut r, GUEST);
    assert_ne!(second, first);
    assert!(exceptions_of(&r, &second).is_empty());
    let q = pending(&r);
    assert!(q.items.iter().all(|i| i.r#ref != second), "{:#?}", q.items);
    let nr = q
        .not_ready
        .iter()
        .find(|n| n.r#ref == second)
        .expect("the version is not ready");
    assert_eq!(nr.issues.len(), 1, "{:#?}", nr.issues);
    assert_eq!(
        nr.issues[0].message,
        format!("{d} was rejected; drop or revise it, then propose again")
    );
    // a local approval refuses with the queue's issue, and writes nothing (D-ME2HD)
    let before = log_end(&r);
    let e = r
        .land_with(&approver(), &second, LandOptions::default())
        .unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_NOT_READY, "{e}");
    assert_eq!(e.message, format!("{second} is not ready: 1 issues"), "{e}");
    assert_eq!(
        e.fix, "fix each issue and propose again: rdy_propose / rdy propose",
        "{e}"
    );
    assert_eq!(e.issues, nr.issues, "{e}");
    assert_eq!(log_end(&r), before, "a refused landing writes nothing");
    // so does the relayed approval through rdy_answer: the version is no item
    let mut st = agent_state(r);
    let published = st.engine.read().seq();
    let e = rdy_answer(
        &mut st,
        serde_json::from_value(json!({
            "ref": second, "option": "approve", "card_hash": "0".repeat(64),
            "evidence": "Yes, land it.", "question": "Shall I land it?", "session_ref": "chat-7",
        }))
        .unwrap(),
    )
    .unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_NOT_READY, "{e}");
    assert_eq!(e.issues, nr.issues, "{e}");
    assert_eq!(st.engine.read().seq(), published, "nothing is written");
    let canon = st.engine.read().materialized(&Scope::Canon).unwrap();
    assert!(
        canon.get(&d).is_none(),
        "the rejected decision is not in canon"
    );
    drop(canon);
    drop(st);
    // the author revises the decision: its new version is no longer rejected, and it lands
    let mut r = Rdy::open(dir.path()).unwrap();
    put(
        &mut r,
        GUEST,
        json!([{"op": "update", "id": d.to_string(),
                "fields": {"statement": "Guests see a small banner"}}]),
    );
    let third = propose(&mut r, GUEST);
    confirm_exceptions(&mut r, &approver(), &third);
    r.land_with(&approver(), &third, LandOptions::default())
        .expect("the revised decision lands");
    let canon = r.read().materialized(&Scope::Canon).unwrap();
    assert_eq!(canon.get(&d).unwrap().field_str("status"), Some("accepted"));
}
