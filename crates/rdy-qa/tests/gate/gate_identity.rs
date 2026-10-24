//! id6 (D-ME2HD, D-TXFHR): every approval path refuses a version that is not ready with one
//! code, `E_PROPOSAL_NOT_READY`. Its message counts the issues, its fix tells the author to fix
//! each and propose again, and its issues are exactly the queue's readiness issues for that
//! version, in the queue's order, each with its own code and fix. A direct approval, `rdy
//! approve` (through the daemon or not), `rdy answer`, `rdy_answer` and `all_recommended` give
//! the same refusal. The preconditions (permission, a moved version, a withdrawal, the
//! alternatives and a moved base) keep their codes and come first; the two approval holds
//! (`E_EXCEPTION_OPEN`, then `E_DEFERRAL_OPEN`) keep theirs and come after.
use rdy_engine::alt::AltMemberInput;
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::consequence::propose_and_report;
use rdy_engine::engine::{Engine, Local};
use rdy_engine::pending::{CARD_MAX_TOKENS, ExceptionKind, ItemKind, Pending, PendingQuery};
use rdy_engine::plan::{PlanAnswerInput, ProposePlanRequest};
use rdy_engine::signing_act::{SigningAct, run_signed_act};
use rdy_engine::signing_crypto::{AgentIdentity, SigningAgent, blob_fingerprint};
use rdy_engine::{
    DecisionInput, E_ALT_UNCHOSEN, E_BLOCKING_TENSION, E_CONFLICT_UNRESOLVED, E_DEFERRAL_OPEN,
    E_EXCEPTION_OPEN, E_ITEM_MOVED, E_PERMISSION, E_PROPOSAL_MOVED, E_VALIDATION, E_VIEW_CLOSED,
    LandOptions, Rdy, RdyError, Scope, Session,
};
use rdy_mcp::tools::{ERROR_ROWS, McpState, rdy_answer, rdy_session};
use rdy_model::plan::{PlanAct, PlanKind, item_display, item_hash};
use rdy_model::{BlockId, Role};
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::{approver, land, version_of, write, writer};
use rdy_qa::intake::{PERSON, confirm_exceptions, decide_in, decision, pending};
use rdy_qa::tensions::{ag, created, put, upd};
use rdy_schema::Issue;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The code of the refusal (D-ME2HD, n-TZWXS). A literal, so these pins name the contract
/// itself.
const NOT_READY: &str = "E_PROPOSAL_NOT_READY";
/// Its fix (D-ME2HD, n-TZWXS).
const NOT_READY_FIX: &str = "fix each issue and propose again: rdy_propose / rdy propose";
/// The summary every ready version here carries.
const SUMMARY: &str = "Changes one thing.";

/// The message of the refusal of `pref` with `n` issues: one issue reads `1 issue`, more read
/// `<n> issues` (TASK-548).
fn not_ready_message(pref: &str, n: usize) -> String {
    format!(
        "{pref} is not ready: {n} issue{}",
        if n == 1 { "" } else { "s" }
    )
}

/// Checks `e` is the refusal of `pref` whose issues are `issues`, with no corrected payload.
fn assert_not_ready(e: &RdyError, pref: &str, issues: &[Issue]) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            NOT_READY,
            not_ready_message(pref, issues.len()).as_str(),
            NOT_READY_FIX
        ),
        "{e:#?}"
    );
    assert_eq!(e.issues, issues, "{e:#?}");
    assert!(e.corrected.is_none(), "no card: {e:#?}");
}

fn log_end(r: &Rdy) -> Location {
    r.store().log_end()
}

/// The issues the queue lists for `pref` under `not_ready`.
fn queue_issues(q: &Pending, pref: &str) -> Vec<Issue> {
    q.not_ready
        .iter()
        .find(|n| n.r#ref == pref)
        .unwrap_or_else(|| panic!("{pref} is not listed as not ready: {:#?}", q.not_ready))
        .issues
        .clone()
}

fn codes(issues: &[Issue]) -> Vec<&'static str> {
    issues.iter().map(|i| i.code).collect()
}

// ---- the project ----

/// The second doc, `rules`: claims m ("M holds 30 days."), n (cites m) and z (nothing cites it).
struct Rules {
    section: String,
    m: String,
    n: String,
    z: String,
}

fn rules(r: &mut Rdy) -> Rules {
    let w = write(
        r,
        "rules",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
            {"op": "create", "type": "claim", "id": "$m", "parent": "$s", "body": "M holds 30 days."},
            {"op": "create", "type": "claim", "id": "$n", "parent": "$s", "body": "N builds on {{$m}}."},
            {"op": "create", "type": "claim", "id": "$z", "parent": "$s", "body": "Z stands alone."}
        ]),
    )
    .expect("the rules doc");
    land(r, "rules").expect("land the rules doc");
    let get = |k: &str| w.created[k].clone();
    Rules {
        section: get("$s"),
        m: get("$m"),
        n: get("$n"),
        z: get("$z"),
    }
}

/// The six versions that are not ready, one for each kind of readiness issue, by ref.
#[derive(Clone, Debug)]
struct NotReadyVersions {
    /// An edit of claim C that a landing edited too, with a claim that builds on a landed
    /// change of A: a conflict and a blocking tension.
    both: String,
    /// A new claim, proposed with no summary.
    bare: String,
    /// A decision the person rejected.
    rejected: String,
    /// A re-affirmation of a one_way decision the person rejected.
    reaffirm: String,
    /// A version whose deferrals the person refused.
    refused: String,
    /// A folder whose name clashes in the canon a later landing gave: the prospective canon
    /// fails the pack.
    pack: String,
}

impl NotReadyVersions {
    fn all(&self) -> [(&'static str, &str); 6] {
        [
            ("both", &self.both),
            ("bare", &self.bare),
            ("rejected", &self.rejected),
            ("reaffirm", &self.reaffirm),
            ("refused", &self.refused),
            ("pack", &self.pack),
        ]
    }
}

/// The local answer of `option` to `item` by the person.
fn answer_local(r: &mut Rdy, item: &str, option: &str) -> Result<String, RdyError> {
    r.answer(
        &approver(),
        PERSON,
        AnswerRequest {
            r#ref: item.into(),
            option: option.into(),
            ..AnswerRequest::default()
        },
    )
    .map(|i| i.act)
}

/// The confirm items beside batch `pref`.
fn exceptions_of(r: &Rdy, pref: &str) -> Vec<rdy_engine::pending::Item> {
    pending(r)
        .items
        .into_iter()
        .filter(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(pref))
        .collect()
}

/// Proposes `view` as `agent:<view>` with `summary`; the version's `P-x@n`.
fn propose(r: &mut Rdy, view: &str, summary: Option<&str>) -> String {
    r.propose_subset(&ag(view), view, view, None, summary)
        .unwrap_or_else(|e| panic!("propose {view}: {e}"))
        .r#ref
}

/// A new claim under `section` in view `view`, as `agent:<view>`.
fn claim(r: &mut Rdy, view: &str, section: &BlockId, body: &str) {
    put(
        r,
        view,
        json!([{"op": "create", "type": "claim", "parent": section.to_string(), "body": body}]),
    );
}

/// The plan item of `subject`'s follow-up of `kind` for a change of `cause`.
fn plan_item(cause: &str, subject: &str, kind: PlanKind) -> String {
    item_display(&item_hash(
        &BlockId::parse(cause).expect("cause"),
        subject,
        kind,
    ))
}

/// Builds the six versions of [`NotReadyVersions`] in `r`, after the spec and rules docs.
fn not_ready_versions(r: &mut Rdy) -> (Spec, Rules, NotReadyVersions) {
    let spec = spec_doc(r);
    let rules = rules(r);
    // a conflict: two views edit C from canon's version, and one lands
    put(r, "edit-c", upd(&spec.c, "C as edit-c has it."));
    put(r, "conflict", upd(&spec.c, "C as conflict has it."));
    land(r, "edit-c").expect("land edit-c");
    // a blocking tension: a view cites A, and an edit of A lands
    put(
        r,
        "blocking",
        json!([{"op": "create", "type": "claim", "parent": spec.section.to_string(),
                "body": format!("Builds on {{{{{}}}}}.", spec.a)}]),
    );
    put(r, "edit-a", upd(&spec.a, "A changed."));
    land(r, "edit-a").expect("land edit-a");
    // a one_way decision that depends on Z lands, then an edit of Z lands
    let one = r
        .decide(
            &writer(),
            "one-way",
            DecisionInput {
                parent: rules.section.clone(),
                statement: "Never drop Z".into(),
                depends_on: vec![rules.z.clone()],
                reversibility: Some("one_way".into()),
                ..Default::default()
            },
        )
        .expect("decide the one_way decision")
        .created["$decision"]
        .clone();
    land(r, "one-way").expect("land the one_way decision");
    write(
        r,
        "edit-z",
        json!([{"op": "update", "id": rules.z, "body": "Z stands still."}]),
    )
    .expect("edit Z");
    land(r, "edit-z").expect("land edit-z");
    // both: the conflict view takes the blocking view's change too
    let blocking: Vec<String> = r
        .read()
        .included("blocking")
        .iter()
        .map(ToString::to_string)
        .collect();
    r.view_include(&ag("conflict"), "conflict", &blocking)
        .expect("include the blocking change");
    let both = propose(r, "conflict", Some(SUMMARY));
    // bare: no summary
    claim(r, "bare", &spec.section, "A claim of bare.");
    let bare = propose(r, "bare", None);
    // rejected: a decision no content applies, which the person rejects
    let d = decide_in(
        r,
        "rejected",
        DecisionInput {
            affects: vec![],
            ..decision(&spec, "Guests see a banner", "Guests see a banner")
        },
    );
    let rejected = propose(r, "rejected", Some(SUMMARY));
    let ex = exceptions_of(r, &rejected);
    assert_eq!(ex.len(), 1, "{ex:#?}");
    assert!(ex[0].r#ref.starts_with(&format!("{d}@")), "{ex:#?}");
    assert_eq!(
        answer_local(r, &ex[0].r#ref, "reject").unwrap(),
        "reject_decision"
    );
    // reaffirm: a re-affirmation of the one_way decision, which the person rejects
    write(
        r,
        "reaffirm",
        json!([{"op": "reaffirm", "id": one, "targets": [rules.z], "reason": "Z still fits."}]),
    )
    .expect("re-affirm");
    let reaffirm = r
        .propose(&writer(), "reaffirm", "reaffirm", Some(SUMMARY))
        .expect("propose the re-affirmation")
        .r#ref;
    let ex = exceptions_of(r, &reaffirm);
    assert_eq!(ex.len(), 1, "{ex:#?}");
    assert_eq!(ex[0].exception, Some(ExceptionKind::ReaffirmOneWay));
    assert_eq!(
        answer_local(r, &ex[0].r#ref, "reject").unwrap(),
        "reject_reaffirm"
    );
    // refused: an edit of M defers N's confirm, and the person refuses the deferral card
    write(
        r,
        "refused",
        json!([{"op": "update", "id": rules.m, "body": "M holds 60 days."}]),
    )
    .expect("edit M");
    let answers = [PlanAnswerInput {
        item: plan_item(&rules.m, &rules.n, PlanKind::Confirm),
        act: PlanAct::Defer,
        reason: Some("N waits for the review".into()),
    }];
    let refused = r
        .propose_plan(
            &writer(),
            ProposePlanRequest {
                summary: Some(SUMMARY),
                plan_answers: &answers,
                ..ProposePlanRequest::new("refused", "refused")
            },
        )
        .expect("propose the deferral")
        .r#ref;
    assert_eq!(
        answer_local(r, &format!("defer:{refused}"), "o2").unwrap(),
        "refuse_deferrals"
    );
    // pack: two views name a folder alike; one lands after the other proposed
    put(
        r,
        "folder-a",
        json!([{"op": "create", "type": "folder", "fields": {"name": "Wallets"}}]),
    );
    put(
        r,
        "folder-b",
        json!([{"op": "create", "type": "folder", "fields": {"name": "wallets"}}]),
    );
    let pack = propose(r, "folder-b", Some(SUMMARY));
    land(r, "folder-a").expect("land folder-a");
    let made = NotReadyVersions {
        both,
        bare,
        rejected,
        reaffirm,
        refused,
        pack,
    };
    // each is not ready for its own reason, as the queue lists it
    let q = pending(r);
    let want: [(&str, Vec<&str>); 6] = [
        (&made.both, vec![E_CONFLICT_UNRESOLVED, E_BLOCKING_TENSION]),
        (&made.bare, vec![E_VALIDATION]),
        (&made.rejected, vec![E_VALIDATION]),
        (&made.reaffirm, vec![E_VALIDATION]),
        (&made.refused, vec![E_VALIDATION]),
        (&made.pack, vec![E_VALIDATION]),
    ];
    for (pref, want) in want {
        assert_eq!(codes(&queue_issues(&q, pref)), want, "{pref}");
    }
    assert!(
        queue_issues(&q, &made.bare)[0]
            .message
            .ends_with("has no summary"),
        "{:#?}",
        q.not_ready
    );
    (spec, rules, made)
}

/// A fresh project holding [`NotReadyVersions`].
fn project() -> (tempfile::TempDir, Rdy, Spec, Rules, NotReadyVersions) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut r = Rdy::init(dir.path(), "rdy-standard").expect("init");
    let (spec, rules, made) = not_ready_versions(&mut r);
    (dir, r, spec, rules, made)
}

/// Runs `f` on the served project with the daemon stopped (it holds the store while it runs).
fn with_rdy<T>(s: &Served, f: impl FnOnce(&mut Rdy) -> T) -> T {
    s.stop();
    let mut r = Rdy::open(s.dir.path()).expect("open");
    let out = f(&mut r);
    r.close().expect("close");
    out
}

/// An MCP state on `r` with the agent session `agent:relay`, run `r1`.
fn agent_state(r: Rdy) -> McpState {
    let mut st = McpState::new(Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:relay", "run": "r1", "role": "writer"}))
            .expect("session params"),
    )
    .expect("session");
    st
}

/// The relaying agent.
fn relayer() -> Session {
    Session::agent("agent:relay".into(), Some("r1".into()), Role::Writer).expect("agent")
}

/// The person's words, relayed over MCP (no `relayed_by`).
fn words() -> RelayInput {
    RelayInput {
        relayed_by: None,
        evidence: Some("Yes, land it.".into()),
        question: Some("Shall I land it?".into()),
        session_ref: Some("chat-7".into()),
        kind: None,
    }
}

/// The parts of a refusal every path must agree on: code, message, fix and issues.
fn parts(v: &Value) -> Value {
    json!({"code": v["code"], "message": v["message"], "fix": v["fix"], "issues": v["issues"]})
}

/// [`parts`] of the refusal of `pref` with `issues`.
fn expected(pref: &str, issues: &[Issue]) -> Value {
    json!({
        "code": NOT_READY,
        "message": not_ready_message(pref, issues.len()),
        "fix": NOT_READY_FIX,
        "issues": serde_json::to_value(issues).expect("issues"),
    })
}

// ---- tests ----

#[test]
fn id6_every_approval_path_refuses_a_version_that_is_not_ready() {
    let (_d, mut r, _, _, made) = project();
    let q = pending(&r);
    for (name, pref) in made.all() {
        let issues = queue_issues(&q, pref);
        let before = log_end(&r);
        let e = r
            .land_with(&approver(), pref, LandOptions::default())
            .unwrap_err();
        assert_not_ready(&e, pref, &issues);
        assert_eq!(log_end(&r), before, "{name}: a refusal writes nothing");
    }
    // the conflict comes first, then the blocking tension, each with the queue's texts
    let both = queue_issues(&q, &made.both);
    assert!(both[0].message.contains("unresolved conflict"), "{both:#?}");
    assert!(
        both[1].message.contains("blocking tension(s) into canon"),
        "{both:#?}"
    );
    // each stays not ready for the same reasons
    assert_eq!(pending(&r).not_ready, q.not_ready);
}

#[test]
fn id6_cli_and_mcp_refuse_the_same_not_ready_version() {
    let s = Served::new();
    let (made, want) = with_rdy(&s, |r| {
        let (_, _, made) = not_ready_versions(r);
        let q = pending(r);
        let want: Vec<(String, Value)> = made
            .all()
            .iter()
            .map(|(_, pref)| (pref.to_string(), expected(pref, &queue_issues(&q, pref))))
            .collect();
        (made, want)
    });
    let before = with_rdy(&s, |r| log_end(r));
    for (pref, want) in &want {
        // the person's rdy approve, through the daemon
        let (ok, e) = s.json(&["--json", "approve", pref]);
        assert!(!ok, "{e}");
        assert_eq!(parts(&e), *want, "daemon rdy approve {pref}");
        // the person's rdy answer <ref> approve
        let (ok, e) = s.json(&["--json", "answer", pref, "approve"]);
        assert!(!ok, "{e}");
        assert_eq!(parts(&e), *want, "rdy answer {pref}");
        assert!(e.get("corrected").is_none_or(Value::is_null), "{e}");
        // rdy answer --all-recommended, with nothing done before it
        let (ok, e) = s.json(&["--json", "answer", "--all-recommended", pref]);
        assert!(!ok, "{e}");
        assert_eq!(parts(&e), *want, "rdy answer --all-recommended {pref}");
        assert_eq!(e["corrected"], json!({"done": []}), "{e}");
    }
    // rdy approve without the daemon
    s.stop();
    for (pref, want) in &want {
        let (ok, e) = s.json(&["--json", "--no-daemon", "approve", pref]);
        assert!(!ok, "{e}");
        assert_eq!(parts(&e), *want, "rdy --no-daemon approve {pref}");
    }
    assert_eq!(
        with_rdy(&s, |r| log_end(r)),
        before,
        "no CLI path writes anything"
    );
    // rdy_answer, one card and all_recommended, relayed from chat
    let mut st = agent_state(Rdy::open(s.dir.path()).expect("open"));
    // rdy_session may record the session: read the published state after it
    let published = st.engine.read().seq();
    for (pref, want) in &want {
        let e = rdy_answer(
            &mut st,
            serde_json::from_value(json!({
                "ref": pref, "option": "approve", "card_hash": "0".repeat(64),
                "evidence": "Yes, land it.", "question": "Shall I land it?", "session_ref": "chat-7",
            }))
            .unwrap(),
        )
        .unwrap_err();
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(parts(&v), *want, "rdy_answer {pref}");
        assert!(e.corrected.is_none(), "{e:#?}");
        let e = rdy_answer(
            &mut st,
            serde_json::from_value(json!({
                "all_recommended": [{"ref": pref, "card_hash": "0".repeat(64)}],
                "evidence": "Yes, land it.", "question": "Shall I land it?", "session_ref": "chat-7",
            }))
            .unwrap(),
        )
        .unwrap_err();
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(parts(&v), *want, "rdy_answer all_recommended {pref}");
        assert_eq!(e.corrected, Some(json!({"done": []})), "{e:#?}");
    }
    assert_eq!(
        st.engine.read().seq(),
        published,
        "rdy_answer writes nothing"
    );
    assert_eq!(made.all().len(), want.len());
}

#[test]
fn id6_not_ready_answer_is_not_a_moved_card() {
    let (dir, mut r, spec, _, made) = project();
    let q = pending(&r);
    let approve = |r: &mut Rdy, item: &str, option: &str| {
        r.answer(
            &approver(),
            PERSON,
            AnswerRequest {
                r#ref: item.into(),
                option: option.into(),
                ..AnswerRequest::default()
            },
        )
        .unwrap_err()
    };
    let before = log_end(&r);
    for (name, pref) in made.all() {
        let issues = queue_issues(&q, pref);
        // the person's own approval, and a relayed one
        assert_not_ready(&approve(&mut r, pref, "approve"), pref, &issues);
        let e = r
            .answer(
                &relayer(),
                PERSON,
                AnswerRequest {
                    r#ref: pref.into(),
                    option: "approve".into(),
                    card_hash: Some("0".repeat(64)),
                    relay: words(),
                    ..AnswerRequest::default()
                },
            )
            .unwrap_err();
        assert_not_ready(&e, pref, &issues);
        // a unique prefix of the version names it too; the refusal names it as the queue does
        let (stable, n) = pref.trim_start_matches("P-").split_once('@').unwrap();
        for spelling in [
            format!("P-{}@{n}", &stable[..8]),
            format!("P-{}@{n}", stable.to_ascii_uppercase()),
        ] {
            assert_not_ready(&approve(&mut r, &spelling, "approve"), pref, &issues);
        }
        // reject and Later keep the moved card's refusal, with the queue's issues
        for option in ["reject", "later"] {
            let e = approve(&mut r, pref, option);
            assert_eq!(e.code, E_ITEM_MOVED, "{name} {option}: {e:#?}");
            assert_eq!(e.message, format!("{pref} changed since it was shown"));
            assert_eq!(e.issues, issues, "{name} {option}");
        }
    }
    assert_eq!(log_end(&r), before, "no refusal writes anything");
    // a newer version: the older one has moved
    claim(&mut r, "twice", &spec.section, "A claim of twice.");
    let first = propose(&mut r, "twice", None);
    claim(&mut r, "twice", &spec.section, "Another claim of twice.");
    let second = propose(&mut r, "twice", None);
    assert_ne!(first, second);
    let e = approve(&mut r, &first, "approve");
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e:#?}");
    // a withdrawn version
    claim(&mut r, "closed", &spec.section, "A claim of closed.");
    let closed = propose(&mut r, "closed", None);
    r.view_close(&ag("closed"), &["closed".into()], "not needed")
        .expect("close");
    let e = approve(&mut r, &closed, "approve");
    assert_eq!(e.code, E_VIEW_CLOSED, "{e:#?}");
    // a version a reject_proposal answer took out
    claim(&mut r, "rejected-ready", &spec.section, "A ready claim.");
    let gone = propose(&mut r, "rejected-ready", Some(SUMMARY));
    assert_eq!(
        answer_local(&mut r, &gone, "reject").unwrap(),
        "reject_proposal"
    );
    let e = approve(&mut r, &gone, "approve");
    assert_eq!(e.code, E_ITEM_MOVED, "{e:#?}");
    assert!(e.issues.is_empty(), "{e:#?}");
    // an answered question whose answer landed
    let w = write(
        &mut r,
        "qs",
        json!([{"op": "create", "type": "question", "id": "$q", "parent": spec.section.to_string(),
        "fields": {
            "prompt": "Who approves refunds?",
            "title": "Who approves refunds?",
            "why": "Support waits on this",
            "options": ["Finance", "Support lead", "Either"],
            "recommended": "Finance",
            "recommend_reason": "Finance owns the ledger"
        }}]),
    )
    .expect("the question");
    land(&mut r, "qs").expect("land the question");
    r.close().expect("close");
    let mut r = Rdy::open(dir.path()).expect("reopen");
    let qid = created(&w, "$q");
    let v = version_of(&r, &Scope::Canon, &qid.to_string());
    let item = format!("{qid}@{}", &v[..12]);
    let h = pending(&r)
        .items
        .into_iter()
        .find(|i| i.r#ref == item)
        .expect("the question's card")
        .card_hash;
    r.answer(
        &relayer(),
        PERSON,
        AnswerRequest {
            r#ref: item.clone(),
            option: "o1".into(),
            card_hash: Some(h),
            relay: words(),
            ..AnswerRequest::default()
        },
    )
    .expect("answer the question");
    // its answering decision lands, so the question leaves the queue
    land(&mut r, &relayer().default_view()).expect("land the answer");
    for option in ["o1", "approve"] {
        let e = approve(&mut r, &item, option);
        assert_eq!(e.code, E_ITEM_MOVED, "{option}: {e:#?}");
    }
}

#[test]
fn id6_bulk_not_ready_refusal_keeps_done() {
    let (_d, mut r, spec, _, made) = project();
    claim(&mut r, "first", &spec.section, "A claim of first.");
    let first = propose(&mut r, "first", Some(SUMMARY));
    claim(&mut r, "last", &spec.section, "A claim of last.");
    let last = propose(&mut r, "last", Some(SUMMARY));
    let issues = queue_issues(&pending(&r), &made.bare);
    let refs = vec![first.clone(), made.bare.clone(), last.clone()];
    let e = r
        .answer_all_recommended(&approver(), PERSON, &refs, &[], RelayInput::default())
        .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            NOT_READY,
            not_ready_message(&made.bare, issues.len()).as_str(),
            NOT_READY_FIX
        ),
        "{e:#?}"
    );
    assert_eq!(e.issues, issues);
    // exactly the earlier success, and no card
    let corrected = e.corrected.as_ref().expect("what was done");
    assert_eq!(
        corrected.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["done"],
        "{corrected:#}"
    );
    let done = corrected["done"].as_array().expect("done");
    assert_eq!(done.len(), 1, "{corrected:#}");
    assert_eq!(
        (done[0]["ref"].as_str(), done[0]["act"].as_str()),
        (Some(first.as_str()), Some("approve")),
        "{corrected:#}"
    );
    // the earlier one landed; the later one has no answer and did not land
    let st = r.read();
    assert!(st.reader().answer_of(&last).unwrap().is_none());
    drop(st);
    let q = pending(&r);
    assert!(q.items.iter().any(|i| i.r#ref == last), "{q:#?}");
    assert!(q.items.iter().all(|i| i.r#ref != first), "{q:#?}");
}

/// A signing agent that holds no key and counts every ask.
#[derive(Default)]
struct CountingAgent {
    asks: AtomicUsize,
}

impl SigningAgent for CountingAgent {
    fn identities(&self) -> Result<Vec<AgentIdentity>, RdyError> {
        self.asks.fetch_add(1, Ordering::SeqCst);
        Ok(vec![])
    }

    fn sign(&self, _public_key: &[u8], _payload: &[u8]) -> Result<Vec<u8>, RdyError> {
        self.asks.fetch_add(1, Ordering::SeqCst);
        panic!("the agent holds no key to sign with")
    }
}

#[test]
fn id6_holds_and_preconditions_keep_their_codes() {
    let (dir, mut r, spec, rules, made) = project();
    let land_err = |r: &mut Rdy, s: &Session, pref: &str, strict: bool| {
        let before = log_end(r);
        let e = r
            .land_with(
                s,
                pref,
                LandOptions {
                    strict,
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert_eq!(log_end(r), before, "a refusal writes nothing: {e}");
        e
    };
    // permission: an agent session cannot land at all
    let e = land_err(&mut r, &ag("bare"), &made.bare, false);
    assert_eq!(e.code, E_PERMISSION, "{e:#?}");
    // a moved version
    claim(&mut r, "twice", &spec.section, "A claim of twice.");
    let first = propose(&mut r, "twice", None);
    claim(&mut r, "twice", &spec.section, "Another claim of twice.");
    propose(&mut r, "twice", None);
    let e = land_err(&mut r, &approver(), &first, false);
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e:#?}");
    // a withdrawal
    claim(&mut r, "closed", &spec.section, "A claim of closed.");
    let closed = propose(&mut r, "closed", None);
    r.view_close(&ag("closed"), &["closed".into()], "not needed")
        .expect("close");
    let e = land_err(&mut r, &approver(), &closed, false);
    assert_eq!(e.code, E_VIEW_CLOSED, "{e:#?}");
    // the alternatives: a version from before its view's set names no member
    for view in ["alt-a", "alt-b"] {
        write(
            &mut r,
            view,
            json!([{"op": "create", "type": "claim", "parent": spec.section.to_string(),
                    "body": format!("A claim of {view}.")}]),
        )
        .expect("write");
    }
    let early = r
        .propose(&writer(), "alt-b", "alt-b", None)
        .expect("propose alt-b")
        .r#ref;
    let member = |label: &str, view: &str| AltMemberInput {
        label: label.into(),
        view: view.into(),
    };
    r.alt_create(
        &writer(),
        "T",
        None,
        &[member("x", "alt-a"), member("y", "alt-b")],
    )
    .expect("the set");
    let e = land_err(&mut r, &approver(), &early, false);
    assert_eq!(e.code, E_ALT_UNCHOSEN, "{e:#?}");
    // a moved base: strict refuses any landing since; any approval refuses a landing that
    // wrote a block the version writes
    put(&mut r, "stale", upd(&spec.b, "B as stale has it."));
    let stale = propose(&mut r, "stale", None);
    put(&mut r, "edit-b", upd(&spec.b, "B as edit-b has it."));
    land(&mut r, "edit-b").expect("land edit-b");
    let e = land_err(&mut r, &approver(), &made.bare, true);
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e:#?}");
    assert!(e.message.contains("landing(s) since"), "{e:#?}");
    let e = land_err(&mut r, &approver(), &stale, false);
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e:#?}");
    assert!(e.message.contains("after it was proposed"), "{e:#?}");
    // on a ready version: the exception hold, then the deferral hold
    write(
        &mut r,
        "holds",
        json!([{"op": "update", "id": rules.m, "body": "M holds 90 days."}]),
    )
    .expect("edit M");
    r.decide(
        &writer(),
        "holds",
        DecisionInput {
            parent: rules.section.clone(),
            statement: "Hold ninety".into(),
            ..Default::default()
        },
    )
    .expect("an unapplied decision");
    let answers = [PlanAnswerInput {
        item: plan_item(&rules.m, &rules.n, PlanKind::Confirm),
        act: PlanAct::Defer,
        reason: Some("N waits for the review".into()),
    }];
    let held = r
        .propose_plan(
            &writer(),
            ProposePlanRequest {
                summary: Some(SUMMARY),
                plan_answers: &answers,
                ..ProposePlanRequest::new("holds", "holds")
            },
        )
        .expect("propose holds")
        .r#ref;
    let e = land_err(&mut r, &approver(), &held, false);
    assert_eq!(e.code, E_EXCEPTION_OPEN, "{e:#?}");
    confirm_exceptions(&mut r, &approver(), &held);
    let e = land_err(&mut r, &approver(), &held, false);
    assert_eq!(e.code, E_DEFERRAL_OPEN, "{e:#?}");
    // signing that would fail asks no agent when readiness refuses first
    let ssh_string = |b: &[u8]| [&(b.len() as u32).to_be_bytes()[..], b].concat();
    let blob = [ssh_string(b"ssh-ed25519"), ssh_string(&[5u8; 32])].concat();
    std::fs::write(
        dir.path().join(".rdy/local.yaml"),
        format!(
            "signing:\n  key: \"{}\"\n  agent_key: null\n  sign: always\n  allowed_signers: null\n",
            blob_fingerprint(&blob)
        ),
    )
    .expect("local config");
    let issues = queue_issues(&pending(&r), &made.bare);
    r.close().expect("close");
    let e = Local::new(Rdy::open(dir.path()).expect("reopen"));
    let end = e.read().seq();
    let agent = CountingAgent::default();
    let refused = run_signed_act(
        &e,
        &agent,
        &approver(),
        PERSON,
        SigningAct::Approve {
            proposal: made.bare.clone(),
            options: LandOptions::default(),
        },
    )
    .unwrap_err();
    assert_not_ready(&refused, &made.bare, &issues);
    assert_eq!(agent.asks.load(Ordering::SeqCst), 0, "no agent was asked");
    assert_eq!(e.read().seq(), end, "nothing is written");
    // the same signing on a ready version asks the agent, which fails, and writes nothing
    claim_in(&e, &spec.section, "signed");
    let ready = e
        .write(Box::new(|r| {
            let p = r.propose_subset(&ag("signed"), "signed", "signed", None, Some(SUMMARY))?;
            Ok(json!(p.r#ref))
        }))
        .expect("propose signed");
    let end = e.read().seq();
    let failed = run_signed_act(
        &e,
        &agent,
        &approver(),
        PERSON,
        SigningAct::Approve {
            proposal: ready.as_str().unwrap().to_string(),
            options: LandOptions::default(),
        },
    )
    .unwrap_err();
    assert_ne!(failed.code, NOT_READY, "{failed:#?}");
    assert!(agent.asks.load(Ordering::SeqCst) > 0, "{failed:#?}");
    assert_eq!(e.read().seq(), end, "nothing is written: {failed:#?}");
}

/// A new claim under `section` in view `view` through engine `e`, as `agent:<view>`.
fn claim_in(e: &Local, section: &BlockId, view: &str) {
    let (section, view) = (section.clone(), view.to_string());
    e.write(Box::new(move |r| {
        claim(r, &view, &section, &format!("A claim of {view}."));
        Ok(Value::Null)
    }))
    .expect("write the claim");
}

#[test]
fn id6_message_counts_issues_and_catalogue_has_the_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut r = Rdy::init(dir.path(), "rdy-standard").expect("init");
    let (spec, _, made) = not_ready_versions(&mut r);
    // one issue, then two (the conflict and the tension)
    let q = pending(&r);
    for (pref, n, text) in [(&made.bare, 1, "1 issue"), (&made.both, 2, "2 issues")] {
        let issues = queue_issues(&q, pref);
        assert_eq!(issues.len(), n, "{issues:#?}");
        let e = r
            .land_with(&approver(), pref, LandOptions::default())
            .unwrap_err();
        assert_eq!(e.message, format!("{pref} is not ready: {text}"), "{e:#?}");
        assert_eq!(e.issues, issues);
    }
    // then three: a new conflict on C, the blocking change and no summary. A propose's
    // consequences and the queue keep the cause codes.
    put(&mut r, "three", upd(&spec.c, "C as three has it."));
    put(&mut r, "edit-c3", upd(&spec.c, "C as edit-c3 has it."));
    land(&mut r, "edit-c3").expect("land edit-c3");
    let blocking: Vec<String> = r
        .read()
        .included("blocking")
        .iter()
        .map(ToString::to_string)
        .collect();
    r.view_include(&ag("three"), "three", &blocking)
        .expect("include the blocking change");
    let e = Local::new(r);
    let v = propose_and_report(&e, None, |r| {
        r.propose_subset(&ag("three"), "three", "three", None, None)
    })
    .expect("propose three");
    assert!(
        v["consequences_error"]
            .as_str()
            .is_some_and(|s| s.starts_with("E_CONFLICT_UNRESOLVED: ")),
        "{v:#}"
    );
    let three = v["ref"].as_str().expect("the version's ref").to_string();
    let q = e
        .read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .expect("pending");
    let issues = queue_issues(&q, &three);
    assert_eq!(
        codes(&issues),
        vec![E_CONFLICT_UNRESOLVED, E_BLOCKING_TENSION, E_VALIDATION]
    );
    let pref = three.clone();
    let refused = e
        .write(Box::new(move |r| {
            let e = r
                .land_with(&approver(), &pref, LandOptions::default())
                .unwrap_err();
            Ok(serde_json::to_value(&e).unwrap())
        }))
        .expect("land three");
    assert_eq!(parts(&refused), expected(&three, &issues));
    assert_eq!(
        refused["message"],
        format!("{three} is not ready: 3 issues")
    );
    // the catalogue: one new row, and both cause rows stay
    let row = |code: &str| {
        let rows: Vec<_> = ERROR_ROWS.iter().filter(|(c, _, _)| *c == code).collect();
        assert_eq!(rows.len(), 1, "one row for {code}");
        (rows[0].1, rows[0].2)
    };
    assert_eq!(
        row(NOT_READY),
        (
            "the proposal version fails readiness; issues name each failed check",
            NOT_READY_FIX
        )
    );
    assert_eq!(
        row(E_CONFLICT_UNRESOLVED).0,
        "an issue names a conflict the proposal version does not resolve"
    );
    assert_eq!(
        row(E_BLOCKING_TENSION).0,
        "an issue names an undismissed blocking tension that the landing would put into canon"
    );
}
