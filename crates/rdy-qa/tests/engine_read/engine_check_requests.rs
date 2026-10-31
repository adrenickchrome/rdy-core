//! ck6: durable review requests (D-JQD5X, D-33BSB, D-M1ZQG): `rdy_review` writes author-run and
//! independent CHECK_REQUESTs, `rdy_review_next` dispatches them beside the derived tension
//! queue, and `rdy_review_submit` answers them. Requests survive a restart; leases do not.
use rdy_engine::check::api::{ReviewRequest, run_review};
use rdy_engine::engine::write_typed;
use rdy_engine::readiness::{Candidate, assess, readiness};
use rdy_engine::semantic::api::{ReviewNextRequest, run_review_next, run_review_submit};
use rdy_engine::semantic::submit::{CheckedSubmit, CitationRequest, ReviewSubmitRequest};
use rdy_engine::{
    E_BAD_INPUT, E_NOT_FOUND, E_PERMISSION, E_REVIEW_INVALID, E_REVIEW_STALE, E_SELF_REVIEW,
    Engine, Local, Rdy, RdyError, Scope, Session, now_ms, pin_clock,
};
use rdy_model::check_request::CheckRequest;
use rdy_model::{BlockId, Hash, Role};
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::*;
use rdy_qa::intake::TEST_SUMMARY;
use rdy_qa::tensions::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

const PERSON: &str = "human:qa";

fn review(e: &dyn Engine, s: &Session, v: Value) -> Result<Value, RdyError> {
    run_review(e, s, PERSON, true, None, ReviewRequest::from_value(v)?)
}

fn ok(e: &dyn Engine, s: &Session, v: Value) -> Value {
    review(e, s, v.clone()).unwrap_or_else(|err| panic!("{v}: {err:?}"))
}

fn reviewer(actor: &str, run: &str) -> Session {
    Session::agent(actor.into(), Some(run.into()), Role::Reviewer).unwrap()
}

fn next(e: &dyn Engine, s: &Session, owner: u64, kind: &str) -> Result<Value, RdyError> {
    run_review_next(e, s, owner, ReviewNextRequest { kind: kind.into() })
}

fn submit_req(
    request: &str,
    case_hash: &str,
    verdict: &str,
    citations: &[(&str, &str)],
    reason: Option<&str>,
) -> ReviewSubmitRequest {
    ReviewSubmitRequest {
        request: request.into(),
        case_hash: case_hash.into(),
        verdict: verdict.into(),
        citations: citations
            .iter()
            .map(|(b, n)| CitationRequest {
                block: (*b).into(),
                note: (*n).into(),
            })
            .collect(),
        severity: None,
        reason: reason.map(str::to_string),
    }
}

fn submit(
    e: &dyn Engine,
    s: &Session,
    request: &str,
    case_hash: &str,
    verdict: &str,
    citations: &[(&str, &str)],
    reason: Option<&str>,
) -> Result<Value, RdyError> {
    run_review_submit(
        e,
        s,
        submit_req(request, case_hash, verdict, citations, reason),
    )
}

fn refusal(r: Result<Value, RdyError>) -> (&'static str, String, String) {
    let e = r.expect_err("a refusal");
    (e.code, e.message, e.fix)
}

fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().expect("an object").keys().cloned().collect();
    k.sort();
    k
}

fn list(e: &dyn Engine, s: &Session, args: Value) -> Value {
    let mut v = json!({"op": "list"});
    for (k, x) in args.as_object().unwrap() {
        v[k] = x.clone();
    }
    ok(e, s, v)
}

fn hash_of(label: &Value) -> Hash {
    label.as_str().unwrap()["rq-".len()..].parse().unwrap()
}

/// Runs `f` on the writer.
fn with<T: serde::Serialize + 'static>(e: &Local, f: impl FnOnce(&mut Rdy) -> T + Send + 'static) {
    write_typed(e, move |r| Ok(f(r))).unwrap();
}

/// A project whose canon holds the spec doc, and agent:a's proposed decision `p` in view `a`.
struct World {
    dir: tempfile::TempDir,
    e: Local,
    s: Spec,
    p: BlockId,
}

fn world() -> World {
    let (dir, mut r) = project();
    let s = spec_doc(&mut r);
    let p = try_decide_in(&mut r, "a", &s.section, "Cache the reads").unwrap();
    World {
        dir,
        e: Local::new(r),
        s,
        p,
    }
}

fn run_on(w: &World, s: &Session, check: &str, subject: Value) -> Value {
    ok(
        &w.e,
        s,
        json!({"op": "run", "check": check, "subject": subject}),
    )
}

fn request_on(w: &World, s: &Session, check: &str, subject: Value) -> Value {
    ok(
        &w.e,
        s,
        json!({"op": "request", "check": check, "subject": subject}),
    )
}

/// The stored request a label names.
fn record(e: &dyn Engine, label: &Value) -> CheckRequest {
    e.read().reader().check_request(&hash_of(label)).unwrap()
}

/// Writes `n` copies of request `r`, each asked at its own time, straight through the store, in
/// batches of 20; returns their hashes.
fn copies(e: &Local, r: &CheckRequest, n: usize, from_ms: i64) -> Vec<Hash> {
    let rows: Vec<CheckRequest> = (0..n)
        .map(|i| {
            let mut c = r.clone();
            c.requested_by.at_ms = from_ms + i as i64;
            c
        })
        .collect();
    rdy_engine::engine::with_writer_result(e, move |w| {
        let mut ids = vec![];
        for chunk in rows.chunks(20) {
            ids.extend(w.store_mut().put_check_requests(chunk)?);
        }
        w.invalidate();
        Ok(ids)
    })
    .unwrap()
}

#[test]
fn ck6_run_request_list_are_durable_and_strict() {
    let w = world();
    let a = ag("a");
    let feature = |e: &Local| {
        e.inspect(|r| Ok(r.store().features().has("check_request")))
            .unwrap()
    };
    // the catalogue, in its fixed order
    let checks = ok(&w.e, &reader(), json!({"op": "checks"}));
    assert_eq!(keys(&checks), ["checks"]);
    let names: Vec<&str> = checks["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "contradiction",
            "meaning_preservation",
            "implementation_fidelity",
            "decision_review",
            "entity_divergence",
            "prose_drift",
            "diff_contradicts",
            "confirmation"
        ]
    );
    assert_eq!(keys(&checks["checks"][3]), ["name", "subject", "verdicts"]);
    assert_eq!(
        checks["checks"][3],
        json!({"name": "decision_review", "subject": "a proposed decision", "verdicts": ["pass", "concern", "fail"]})
    );
    assert_eq!(
        checks["checks"][0]["verdicts"],
        json!(["tension", "no_tension"])
    );

    // each op refuses the keys of the others, with the nearest key named
    for (args, field, fix) in [
        (
            json!({"op": "checks", "check": "decision_review"}),
            "check",
            "remove \"check\"; it takes no fields",
        ),
        (
            json!({"op": "run", "check": "decision_review", "subject": "x", "reason": "why"}),
            "reason",
            "remove \"reason\"; the fields are check, subject",
        ),
        (
            json!({"op": "run", "check": "decision_review", "subject": "x", "evidence": "e"}),
            "evidence",
            "remove \"evidence\"; the fields are check, subject",
        ),
        (
            json!({"op": "run", "check": "decision_review", "subjct": "x"}),
            "subjct",
            "did you mean \"subject\"? the fields are check, subject",
        ),
        (
            json!({"op": "request", "check": "decision_review", "subject": "x", "relayed_by": "agent:a"}),
            "relayed_by",
            "remove \"relayed_by\"; the fields are check, subject, reason, evidence, question, session_ref",
        ),
        (
            json!({"op": "list", "check": "decision_review"}),
            "check",
            "remove \"check\"; the fields are subject, open",
        ),
        (
            json!({"op": "list", "opn": true}),
            "opn",
            "did you mean \"open\"? the fields are subject, open",
        ),
        (
            json!({"op": "list", "subject": {"decision": "x", "code": "app@a..b", "repo": "app"}}),
            "subject.repo",
            "remove \"repo\"; the fields are decision, code",
        ),
    ] {
        let e = ReviewRequest::from_value(args.clone()).unwrap_err();
        assert_eq!(
            (e.code, e.message, e.fix.as_str()),
            (
                E_BAD_INPUT,
                format!("rdy_review: unknown field \"{field}\""),
                fix
            ),
            "{args}"
        );
    }
    for subject in [json!(["one"]), json!(["a", "b", "c"]), json!(5)] {
        let e = ReviewRequest::from_value(json!({"op": "list", "subject": subject})).unwrap_err();
        assert_eq!(
            e.message,
            "rdy_review: subject must be one block, change or P-x@n, two block ids, or {decision, code}"
        );
    }
    // an unknown check, at run and request
    for op in ["run", "request"] {
        assert_eq!(
            refusal(review(
                &w.e,
                &a,
                json!({"op": op, "check": "nope", "subject": w.p.to_string()})
            )),
            (
                E_BAD_INPUT,
                "nope is not a check".into(),
                "list the checks: rdy_review {op: \"checks\"} / rdy review checks".into()
            )
        );
    }
    // a reader lists but writes no request
    assert_eq!(
        refusal(review(
            &w.e,
            &reader(),
            json!({"op": "run", "check": "decision_review", "subject": w.p.to_string()})
        )),
        (
            E_PERMISSION,
            "role Reader cannot write".into(),
            "start the session as a writer".into()
        )
    );
    // the reason's bounds, checked before anything is read
    for (reason, fine) in [
        ("x".repeat(2000), true),
        ("x".repeat(2001), false),
        ("   ".to_string(), false),
        (format!("x{}", " ".repeat(7999)), true),
        (format!("x{}", " ".repeat(8000)), false),
    ] {
        let r = review(
            &w.e,
            &a,
            json!({"op": "request", "check": "nope", "subject": "x", "reason": reason}),
        );
        let e = r.unwrap_err();
        if fine {
            assert_eq!(e.message, "nope is not a check");
        } else {
            assert_eq!(
                (e.code, e.message.as_str(), e.fix.as_str()),
                (
                    E_BAD_INPUT,
                    "review reason must have 1 to 2000 characters",
                    "give a short reason, or omit reason"
                )
            );
        }
    }
    assert!(!feature(&w.e), "no request yet");

    // an author-run request, by the decision's key: the case comes back at once
    let seq = w.e.read().seq();
    let run = run_on(&w, &a, "decision_review", json!(w.p.to_string()));
    assert_ne!(w.e.read().seq(), seq, "the run wrote");
    assert!(feature(&w.e), "first use enables the feature");
    assert_eq!(
        keys(&run),
        ["case", "case_hash", "omitted", "request", "requests"]
    );
    let label = run["request"].as_str().unwrap();
    assert!(label.starts_with("rq-") && label.len() == 3 + 64, "{label}");
    assert_eq!(run["case"]["kind"], "decision_review");
    assert_eq!(run["omitted"], 0);
    assert_eq!(
        run["requests"],
        json!([{"request": run["request"], "case_hash": run["case_hash"], "case": run["case"]}])
    );
    let stored = record(&w.e, &run["request"]);
    assert_eq!(stored.check, "decision_review");
    assert_eq!(stored.subject.blocks, vec![w.p.clone()]);
    assert_eq!(stored.scope, "a");
    assert_eq!(stored.case_hash.to_string(), run["case_hash"]);
    assert_eq!(stored.mode, rdy_model::check_request::CheckMode::Author);
    assert_eq!(
        (
            stored.requested_by.actor.as_str(),
            stored.requested_by.run.as_deref()
        ),
        ("agent:a", Some("a"))
    );
    assert!(stored.relay.is_none() && stored.relayed_by.is_none() && stored.reason.is_none());

    // an independent request on a pair given out of order stores its canonical ids
    let pair = request_on(
        &w,
        &writer(),
        "contradiction",
        json!([w.s.c.to_string(), w.s.a.to_string()]),
    );
    let mut ids = vec![w.s.a.clone(), w.s.c.clone()];
    ids.sort();
    assert_eq!(record(&w.e, &pair["request"]).subject.blocks, ids);
    assert_eq!(
        record(&w.e, &pair["request"]).scope,
        "work",
        "the session's view"
    );

    // a person's request through an agent that wrote nothing of the subject
    let x = ag("x");
    let relayed = ok(
        &w.e,
        &x,
        json!({"op": "request", "check": "decision_review", "subject": w.p.to_string(),
               "reason": "  Check the basis.  ", "evidence": "please review the cache decision",
               "question": "Shall I request a review?"}),
    );
    let rr = record(&w.e, &relayed["request"]);
    assert_eq!(rr.mode, rdy_model::check_request::CheckMode::Independent);
    assert_eq!(rr.requested_by.actor, PERSON);
    assert_eq!(rr.reason.as_deref(), Some("Check the basis."));
    let by = rr.relayed_by.as_ref().unwrap();
    assert_eq!(
        (by.actor.as_str(), by.run.as_deref()),
        ("agent:x", Some("x"))
    );
    let ev = rr.relay.as_ref().unwrap();
    assert_eq!(ev.quoted, "please review the cache decision");
    assert_eq!(ev.session_ref, "mcp");
    assert_eq!(ev.question.as_deref(), Some("Shall I request a review?"));

    // the list: every request in full hash order, with its exact keys
    let all = list(&w.e, &reader(), json!({}));
    assert_eq!(keys(&all), ["omitted", "requests"]);
    assert_eq!(all["omitted"], 0);
    let rows = all["requests"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    let order: Vec<String> = rows
        .iter()
        .map(|r| r["request"].as_str().unwrap().to_string())
        .collect();
    let sorted = {
        let mut s = order.clone();
        s.sort();
        s
    };
    assert_eq!(order, sorted, "full hash order");
    let row = rows
        .iter()
        .find(|r| r["request"] == run["request"])
        .unwrap();
    assert_eq!(
        keys(row),
        [
            "case_hash",
            "check",
            "mode",
            "reason",
            "request",
            "requested_by",
            "scope",
            "status",
            "subject",
            "verdicts"
        ]
    );
    assert_eq!(
        row,
        &json!({
            "request": run["request"],
            "check": "decision_review",
            "subject": {"blocks": [w.p.to_string()], "change": null, "version": null, "code": null},
            "scope": "a",
            "mode": "author",
            "requested_by": {"actor": "agent:a", "run": "a"},
            "reason": null,
            "case_hash": run["case_hash"],
            "status": "open",
            "verdicts": [],
        })
    );
    let row = rows
        .iter()
        .find(|r| r["request"] == relayed["request"])
        .unwrap();
    assert_eq!(row["requested_by"], json!({"actor": PERSON, "run": "cli"}));
    assert_eq!(row["reason"], "Check the basis.");

    // a restart keeps every request
    let dir: PathBuf = w.dir.path().to_path_buf();
    let World { dir: keep, e, .. } = w;
    e.close().unwrap();
    let e = Local::open(&dir).unwrap();
    assert_eq!(list(&e, &reader(), json!({})), all);
    drop(keep);
}

/// agent:a's 21 decisions that refine canon's decision.
fn refiners(w: &World, n: usize) -> Vec<BlockId> {
    let (section, d) = (w.s.section.to_string(), w.s.decision.to_string());
    let ops: Vec<Value> = (0..n)
        .map(|i| {
            json!({"op": "create", "type": "decision", "id": format!("$r{i}"), "parent": section,
                   "fields": {"statement": format!("Refine {i}"), "status": "proposed"},
                   "links": {"refines": [{"target": d}]}})
        })
        .collect();
    let out = rdy_engine::engine::with_writer_result(&w.e, move |r| {
        Ok(put_j(r, "a", Value::Array(ops), None))
    })
    .unwrap();
    (0..n).map(|i| created(&out, &format!("$r{i}"))).collect()
}

#[test]
fn ck6_contradiction_fans_out_and_keeps_pair_semantics() {
    let w = world();
    let a = ag("a");
    let refined = refiners(&w, 21);
    let d = w.s.decision.to_string();
    // a decision's pairs: one request each, at most 20, in one batch; the rest are counted
    let fan = run_on(&w, &a, "contradiction", json!(d));
    let requests = fan["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 20);
    assert_eq!(fan["omitted"], 1);
    assert_eq!(fan["request"], requests[0]["request"]);
    assert_eq!(fan["case_hash"], requests[0]["case_hash"]);
    assert_eq!(fan["case"], requests[0]["case"]);
    let stored: Vec<CheckRequest> = requests
        .iter()
        .map(|q| record(&w.e, &q["request"]))
        .collect();
    let at = stored[0].requested_by.at_ms;
    for (q, r) in requests.iter().zip(&stored) {
        assert_eq!(r.requested_by.at_ms, at, "one batch, one ask");
        assert_eq!(r.subject.blocks.len(), 2);
        assert!(r.subject.blocks.contains(&w.s.decision));
        assert!(r.subject.blocks.iter().any(|b| refined.contains(b)));
        assert_eq!(r.case_hash.to_string(), q["case_hash"]);
        // each case keeps its inner pair label; the outer request is the durable one
        assert_eq!(q["case"]["kind"], "tension");
        assert_ne!(q["case"]["request"], q["request"]);
    }
    let listed = list(&w.e, &reader(), json!({}));
    assert_eq!(listed["requests"].as_array().unwrap().len(), 20);
    // a decision with no pair: nulls, no request, no write
    let lone = try_decide_in_engine(&w, "Lone call");
    let seq = w.e.read().seq();
    let none = run_on(&w, &a, "contradiction", json!(lone.to_string()));
    assert_eq!(
        none,
        json!({"request": null, "case_hash": null, "case": null, "requests": [], "omitted": 0})
    );
    assert_eq!(w.e.read().seq(), seq);

    // a canon-only pair: the person runs it on their own blocks and answers it. The clock is
    // pinned: two runs of one pair in the same millisecond are one stored request (a request is
    // content-addressed and its blocks are sorted), so the two requests below need two stamps.
    let _clock = pin_clock(now_ms());
    let (ca, cc) = (w.s.a.to_string(), w.s.c.to_string());
    let canon = run_on(&w, &writer(), "contradiction", json!([ca, cc]));
    let rq = canon["request"].as_str().unwrap().to_string();
    let h = canon["case_hash"].as_str().unwrap().to_string();
    let t = submit(
        &w.e,
        &writer(),
        &rq,
        &h,
        "tension",
        &[(ca.as_str(), "A and C disagree")],
        None,
    )
    .unwrap();
    assert_eq!(t["request"], rq.as_str());
    assert_eq!(t["verdict"], "tension");
    assert!(t["tension"].is_string(), "{t}");
    // precedence holds: a later no_tension on the settled pair is stale
    let again = run_on(&w, &writer(), "contradiction", json!([cc, ca]));
    assert_eq!(again["case_hash"], canon["case_hash"]);
    assert_ne!(again["request"], canon["request"], "two runs, two requests");
    let (code, msg, fix) = refusal(submit(
        &w.e,
        &writer(),
        again["request"].as_str().unwrap(),
        &h,
        "no_tension",
        &[],
        None,
    ));
    assert_eq!(
        (code, fix.as_str()),
        (E_REVIEW_STALE, "call rdy_review_next again")
    );
    assert!(
        msg.ends_with("found a tension") && msg.contains(" is settled: review "),
        "{msg}"
    );
    // a settled pair answers both requests of its case
    let rows = list(&w.e, &reader(), json!({"subject": [ca, cc]}));
    let rows = rows["requests"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for r in rows {
        assert_eq!(r["status"], "answered");
        assert_eq!(r["verdicts"][0]["verdict"], "tension");
    }
}

/// agent:a decides `statement` in its view through the engine; the decision's id.
fn try_decide_in_engine(w: &World, statement: &'static str) -> BlockId {
    let section = w.s.section.clone();
    rdy_engine::engine::with_writer_result(&w.e, move |r| {
        try_decide_in(r, "a", &section, statement)
    })
    .unwrap()
}

#[test]
fn ck6_queue_keeps_tension_and_dispatches_any() {
    let w = world();
    let a = ag("a");
    // kind is required
    assert!(serde_json::from_value::<ReviewNextRequest>(json!({})).is_err());
    let _clock = pin_clock(1_000);
    // a derived tension request exists: agent:a edits C
    let cw = w.s.c.clone();
    with(&w.e, move |r| put(r, "a", upd(&cw, "C changed.")));
    let r1 = reviewer("agent:r1", "r1");
    let before = next(&w.e, &r1, 1, "tension").unwrap();
    assert!(before["request"].is_string());
    w.e.leases().release_owner(1);
    // author-run requests never enter the queue; two independent ones do
    run_on(&w, &a, "decision_review", json!(w.p.to_string()));
    let q2 = try_decide_in_engine(&w, "Second call");
    let i1 = request_on(&w, &a, "decision_review", json!(w.p.to_string()));
    let i2 = request_on(&w, &a, "decision_review", json!(q2.to_string()));
    let mut durable = [
        i1["request"].as_str().unwrap().to_string(),
        i2["request"].as_str().unwrap().to_string(),
    ];
    durable.sort();
    // kind tension: the derived queue, as before
    let tension = next(&w.e, &r1, 1, "tension").unwrap();
    assert_eq!(tension["request"], before["request"]);
    assert_eq!(tension["case_hash"], before["case_hash"]);
    assert_eq!(tension["case"], before["case"]);
    assert_eq!(tension["kind"], "tension");
    w.e.leases().release_owner(1);
    // a check's kind offers its independent requests in full hash order
    let first = next(&w.e, &r1, 1, "decision_review").unwrap();
    assert_eq!(
        keys(&first),
        ["case", "case_hash", "kind", "lease_expires_at", "request"]
    );
    assert_eq!(first["request"], durable[0].as_str());
    assert_eq!(first["kind"], "decision_review");
    assert!(first["lease_expires_at"].is_string());
    let r2 = reviewer("agent:r2", "r2");
    assert_eq!(
        next(&w.e, &r2, 2, "decision_review").unwrap()["request"],
        durable[1].as_str()
    );
    let none = next(&w.e, &reviewer("agent:r3", "r3"), 3, "decision_review").unwrap();
    assert_eq!(none, json!({"request": null, "leased": 2}));
    assert_eq!(
        next(&w.e, &reviewer("agent:r3", "r3"), 3, "meaning_preservation").unwrap()["request"],
        Value::Null
    );
    // any: the durable requests, then the tension queue
    w.e.leases().release_owner(1);
    w.e.leases().release_owner(2);
    let r4 = reviewer("agent:r4", "r4");
    let got: Vec<Value> = (0..3)
        .map(|_| next(&w.e, &r4, 4, "any").unwrap()["request"].clone())
        .collect();
    assert_eq!(got[0], durable[0].as_str());
    assert_eq!(got[1], durable[1].as_str());
    assert_eq!(got[2], before["request"]);
    // the list caps at 256 rows and counts the rest exactly
    let base = record(&w.e, &i1["request"]);
    let have = list(&w.e, &reader(), json!({}))["requests"]
        .as_array()
        .unwrap()
        .len();
    copies(&w.e, &base, 256 - have, 5_000_000);
    let full = list(&w.e, &reader(), json!({}));
    assert_eq!(full["requests"].as_array().unwrap().len(), 256);
    assert_eq!(full["omitted"], 0);
    copies(&w.e, &base, 1, 9_000_000);
    let over = list(&w.e, &reader(), json!({}));
    assert_eq!(over["requests"].as_array().unwrap().len(), 256);
    assert_eq!(over["omitted"], 1);
}

#[test]
fn ck6_actor_not_run_controls_authorship() {
    let w = world();
    let a = ag("a");
    let p = w.p.to_string();
    let independent = request_on(&w, &a, "decision_review", json!(p));
    let (irq, ih) = (
        independent["request"].as_str().unwrap().to_string(),
        independent["case_hash"].as_str().unwrap().to_string(),
    );
    // the same actor in another run cannot take or judge it independently
    let same = reviewer("agent:a", "b");
    assert_eq!(
        next(&w.e, &same, 7, "decision_review").unwrap()["request"],
        Value::Null
    );
    assert_eq!(
        refusal(submit(&w.e, &same, &irq, &ih, "pass", &[], None)),
        (
            E_SELF_REVIEW,
            format!(
                "actor agent:a wrote block {p} of {irq}; another run of the same actor is not independent"
            ),
            "a reviewer of another actor judges it".into()
        )
    );
    // the author's writer session cannot judge an independent request
    assert_eq!(
        refusal(submit(&w.e, &a, &irq, &ih, "pass", &[], None)).0,
        E_PERMISSION
    );
    // another agent may not ask on work it did not write, unless it relays a person
    let x = ag("x");
    assert_eq!(
        refusal(review(
            &w.e,
            &x,
            json!({"op": "run", "check": "decision_review", "subject": p})
        )),
        (
            E_PERMISSION,
            "actor agent:x wrote no block of this decision_review subject".into(),
            "run checks on your own work, or relay a person's request with their evidence: rdy_review / rdy review request --relayed-by".into()
        )
    );
    let relayed = ok(
        &w.e,
        &x,
        json!({"op": "request", "check": "decision_review", "subject": p, "evidence": "yes, review it"}),
    );
    assert_eq!(record(&w.e, &relayed["request"]).requested_by.actor, PERSON);
    // a relay key without its evidence is the relay table's refusal
    let e = review(
        &w.e,
        &x,
        json!({"op": "request", "check": "decision_review", "subject": p, "question": "q?"}),
    )
    .unwrap_err();
    assert_eq!(
        e.message,
        "an agent answers only by relaying a person's words"
    );
    // a reviewer of another actor judges it
    let r = reviewer("agent:r", "r");
    let claimed = next(&w.e, &r, 8, "decision_review").unwrap();
    assert!(claimed["request"].is_string());
    // an author-run request: only its actor answers, in any run of a write-capable session
    let own = run_on(&w, &a, "decision_review", json!(p));
    let (orq, oh) = (
        own["request"].as_str().unwrap().to_string(),
        own["case_hash"].as_str().unwrap().to_string(),
    );
    assert_eq!(
        refusal(submit(&w.e, &r, &orq, &oh, "pass", &[], None)),
        (
            E_PERMISSION,
            format!("{orq} is an author-run check of agent:a"),
            "claim an independent request: rdy_review_next / rdy review list --open".into()
        )
    );
    let other_run = as_agent("agent:a", "c");
    let done = submit(&w.e, &other_run, &orq, &oh, "pass", &[], None).unwrap();
    assert_eq!(keys(&done), ["request", "review", "verdict"]);
    assert_eq!(done["request"], orq.as_str());
    assert_eq!(done["verdict"], "pass");
}

#[test]
fn ck6_restart_and_session_close_drop_only_leases() {
    let w = world();
    let a = ag("a");
    let q = request_on(&w, &a, "decision_review", json!(w.p.to_string()));
    let ttl = w.e.read().lease_ttl_ms();
    assert!(ttl > 0);
    let (r1, r2) = (reviewer("agent:r1", "r1"), reviewer("agent:r2", "r2"));
    {
        let _c = pin_clock(0);
        assert_eq!(
            next(&w.e, &r1, 1, "decision_review").unwrap()["request"],
            q["request"]
        );
    }
    {
        let _c = pin_clock(ttl - 1);
        assert_eq!(
            next(&w.e, &r2, 2, "decision_review").unwrap()["request"],
            Value::Null,
            "the lease holds until its last millisecond"
        );
    }
    {
        let _c = pin_clock(ttl);
        assert_eq!(
            next(&w.e, &r2, 2, "decision_review").unwrap()["request"],
            q["request"],
            "the lease lapses at the TTL"
        );
    }
    // closing a session frees its leases
    assert_eq!(w.e.leases().release_owner(2), 1);
    // two reviewers claim at once: one wins
    let barrier = std::sync::Barrier::new(2);
    let got: Vec<Value> = std::thread::scope(|sc| {
        let hs: Vec<_> = [(r1.clone(), 11u64), (r2.clone(), 12u64)]
            .into_iter()
            .map(|(s, owner)| {
                let (e, b) = (&w.e, &barrier);
                sc.spawn(move || {
                    b.wait();
                    next(e, &s, owner, "decision_review").unwrap()["request"].clone()
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(
        got.iter().filter(|g| **g == q["request"]).count(),
        1,
        "{got:?}"
    );
    assert_eq!(got.iter().filter(|g| g.is_null()).count(), 1, "{got:?}");
    // a restart drops the leases and keeps the request
    let dir = w.dir.path().to_path_buf();
    let World { dir: keep, e, .. } = w;
    e.close().unwrap();
    let e = Local::open(&dir).unwrap();
    assert!(e.leases().leased(now_ms()).is_empty());
    let rows = list(&e, &reader(), json!({}));
    assert_eq!(rows["requests"][0]["request"], q["request"]);
    assert_eq!(rows["requests"][0]["status"], "open");
    assert_eq!(
        next(&e, &reviewer("agent:r3", "r3"), 3, "decision_review").unwrap()["request"],
        q["request"]
    );
    drop(keep);
}

#[test]
fn ck6_verdict_schema_and_case_freshness_are_atomic() {
    let w = world();
    let a = ag("a");
    let p = w.p.to_string();
    // a related decision gives the case a contextual row
    let (pr, section) = (w.p.clone(), w.s.section.clone());
    let rel = rdy_engine::engine::with_writer_result(&w.e, move |r| {
        let out = put_j(
            r,
            "a",
            json!([{"op": "create", "type": "decision", "id": "$r", "parent": section.to_string(),
                    "fields": {"statement": "Refine the cache", "status": "proposed"},
                    "links": {"refines": [{"target": pr.to_string()}]}}]),
            None,
        );
        Ok(created(&out, "$r"))
    })
    .unwrap();
    let q = request_on(&w, &a, "decision_review", json!(p));
    let (rq, h) = (
        q["request"].as_str().unwrap().to_string(),
        q["case_hash"].as_str().unwrap().to_string(),
    );
    let r = reviewer("agent:r", "r");
    let shown: Vec<String> = q["case"]["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["id"].as_str().unwrap().to_string())
        .collect();
    assert!(shown.contains(&p));
    let sub = |verdict: &str, cites: &[(&str, &str)], reason: Option<&str>| {
        submit(&w.e, &r, &rq, &h, verdict, cites, reason)
    };
    let seq = w.e.read().seq();
    let invalid = |res: Result<Value, RdyError>, message: &str, fix: &str| {
        assert_eq!(
            refusal(res),
            (E_REVIEW_INVALID, message.to_string(), fix.to_string())
        );
        assert_eq!(w.e.read().seq(), seq, "a refusal appends nothing");
    };
    invalid(
        sub("tension", &[], None),
        "verdict \"tension\" is not pass, concern or fail",
        "send the corrected payload: verdict pass, concern or fail",
    );
    let mut sev = submit_req(&rq, &h, "pass", &[], None);
    sev.severity = Some("blocking".into());
    invalid(
        run_review_submit(&w.e, &r, sev),
        "severity is not part of a check verdict",
        "drop severity; a person can accept the verdict as blocking",
    );
    let note = "the basis is thin";
    invalid(
        sub("concern", &[(&p, note)], None),
        "a concern verdict needs a reason of 1 to 2000 characters",
        "say what the check found",
    );
    invalid(
        sub("fail", &[(&p, note)], Some("  ")),
        "a fail verdict needs a reason of 1 to 2000 characters",
        "say what the check found",
    );
    invalid(
        sub("fail", &[(&p, note)], Some(&"x".repeat(2001))),
        "a fail verdict needs a reason of 1 to 2000 characters",
        "say what the check found",
    );
    invalid(
        sub("concern", &[], Some("thin")),
        "a concern verdict needs at least one citation",
        "cite the blocks that support the verdict",
    );
    invalid(
        sub("pass", &[], Some(" ")),
        "a pass reason needs 1 to 2000 characters",
        "give a short reason, or omit reason",
    );
    invalid(
        sub("pass", &[], Some(&format!("x{}", " ".repeat(8000)))),
        "a pass reason needs 1 to 2000 characters",
        "give a short reason, or omit reason",
    );
    let outside = w.s.b.to_string();
    invalid(
        sub("concern", &[(&outside, note)], Some("thin")),
        &format!("citation 1: block {outside} is not shown in this case"),
        "cite a block shown in the case",
    );
    let many: Vec<(&str, &str)> = std::iter::repeat_n((p.as_str(), note), 21).collect();
    invalid(
        sub("concern", &many, Some("thin")),
        "a verdict takes at most 20 citations",
        "keep the 20 citations that matter most",
    );
    let long = "n".repeat(2001);
    invalid(
        sub("concern", &[(&p, note), (&p, &long)], Some("thin")),
        "citation 2: a note needs 1 to 2000 characters",
        "say in a few words what disagrees",
    );
    let wide = format!("n{}", " ".repeat(8000));
    invalid(
        sub("concern", &[(&p, &wide)], Some("thin")),
        "citation 1: a note needs 1 to 2000 characters",
        "say in a few words what disagrees",
    );
    // the request id and the case hash
    assert_eq!(
        refusal(submit(&w.e, &r, "rq-xyz", &h, "pass", &[], None)),
        (
            E_BAD_INPUT,
            "\"rq-xyz\" is not a review request id".into(),
            "pass the request id from rdy_review or rdy_review_next".into()
        )
    );
    assert_eq!(
        refusal(submit(&w.e, &r, &rq, "abc", "pass", &[], None)),
        (
            E_BAD_INPUT,
            "case_hash must be 64 hex digits".into(),
            "pass case_hash from rdy_review_next".into()
        )
    );
    assert_eq!(
        refusal(submit(&w.e, &r, &rq, &"0".repeat(64), "pass", &[], None)),
        (
            E_REVIEW_STALE,
            format!("the case of {rq} changed since you read it"),
            "call rdy_review_next again".into()
        )
    );
    assert_eq!(w.e.read().seq(), seq);
    // the boundaries that pass: 20 citations, 2000-character notes and reasons, 8000 bytes
    let at: Vec<(&str, String)> = (0..20).map(|_| (p.as_str(), "n".repeat(2000))).collect();
    let at: Vec<(&str, &str)> = at.iter().map(|(b, n)| (*b, n.as_str())).collect();
    let wide_ok = format!("x{}", " ".repeat(7999));
    // a stale writer snapshot: resolved on a read, the case changes, the writer refuses
    let c = CheckedSubmit::new(&r, submit_req(&rq, &h, "pass", &[], None)).unwrap();
    let found =
        w.e.read()
            .resolve_submit(&w.e.leases(), &c, now_ms())
            .unwrap();
    let relw = rel.clone();
    with(&w.e, move |r| {
        put(
            r,
            "a",
            json!([{"op": "update", "id": relw.to_string(), "fields": {"statement": "Refine the cache more"}}]),
        )
    });
    let seq = w.e.read().seq();
    let stale = rdy_engine::engine::with_writer_result(&w.e, move |w| w.submit_resolved(c, found))
        .unwrap_err();
    assert_eq!(
        (stale.code, stale.message.as_str(), stale.fix.as_str()),
        (
            E_REVIEW_STALE,
            format!("request {rq} is no longer current: its case changed").as_str(),
            "call rdy_review_next again"
        )
    );
    assert_eq!(w.e.read().seq(), seq, "the stale verdict appended nothing");
    // the request is stale in the list; a fresh one takes the boundary verdict
    let row = list(&w.e, &reader(), json!({"subject": p}));
    let row = row["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["request"] == rq.as_str())
        .unwrap()
        .clone();
    assert_eq!(row["status"], "stale");
    let fresh = request_on(&w, &a, "decision_review", json!(p));
    let (frq, fh) = (
        fresh["request"].as_str().unwrap().to_string(),
        fresh["case_hash"].as_str().unwrap().to_string(),
    );
    let reason = "r".repeat(2000);
    let done = submit(&w.e, &r, &frq, &fh, "fail", &at, Some(&reason)).unwrap();
    assert_eq!(done["verdict"], "fail");
    let one = submit(
        &w.e,
        &reviewer("agent:s", "s"),
        &frq,
        &fh,
        "pass",
        &[],
        Some(&wide_ok),
    );
    let (code, msg, fix) = refusal(one);
    assert_eq!(
        (code, msg, fix.as_str()),
        (
            E_REVIEW_STALE,
            format!(
                "request {frq} is settled: review {} by agent:r answered fail",
                &done["review"].as_str().unwrap()[..12]
            ),
            "call rdy_review_next again"
        )
    );
    // an author-run request whose context changed is run again
    let own = run_on(&w, &a, "decision_review", json!(p));
    let relw = rel.clone();
    with(&w.e, move |r| {
        put(
            r,
            "a",
            json!([{"op": "update", "id": relw.to_string(), "fields": {"statement": "Refine it again"}}]),
        )
    });
    assert_eq!(
        refusal(submit(
            &w.e,
            &a,
            own["request"].as_str().unwrap(),
            own["case_hash"].as_str().unwrap(),
            "pass",
            &[],
            None
        )),
        (
            E_REVIEW_STALE,
            format!(
                "request {} is no longer current: its case changed",
                own["request"].as_str().unwrap()
            ),
            "run the check again: rdy_review {op: \"run\"} / rdy review run".into()
        )
    );
    // the stored verdict: a pass with no reason prints null
    let pass = run_on(&w, &a, "decision_review", json!(p));
    submit(
        &w.e,
        &a,
        pass["request"].as_str().unwrap(),
        pass["case_hash"].as_str().unwrap(),
        "pass",
        &[],
        None,
    )
    .unwrap();
    let rows = list(&w.e, &reader(), json!({"subject": p}));
    let row = rows["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["request"] == pass["request"])
        .unwrap();
    assert_eq!(row["status"], "answered");
    let v = &row["verdicts"][0];
    assert_eq!(
        keys(v),
        [
            "actor",
            "case_hash",
            "citations",
            "reason",
            "review",
            "run",
            "verdict"
        ]
    );
    assert_eq!(v["reason"], Value::Null);
    assert_eq!(v["citations"], json!([]));
    assert_eq!(v["case_hash"], pass["case_hash"]);
}

#[test]
fn ck6_request_prefix_collision_fails_closed() {
    let w = world();
    let a = ag("a");
    let q = request_on(&w, &a, "decision_review", json!(w.p.to_string()));
    let rq = q["request"].as_str().unwrap().to_string();
    let hex = &rq[3..];
    // a derived request (here a lease on a pair) whose key shares the first 12 digits
    let mut fake = hex[..12].to_string();
    fake.push_str(&"f".repeat(52));
    if fake == hex {
        fake.replace_range(63.., "e");
    }
    let key: Hash = fake.parse().unwrap();
    let pair = vec![
        (w.s.a.clone(), Hash([1; 32])),
        (w.s.c.clone(), Hash([2; 32])),
    ];
    assert!(w.e.leases().claim(key, pair, 99, now_ms(), 600_000));
    let r = reviewer("agent:r", "r");
    let short = format!("rq-{}", &hex[..12]);
    assert_eq!(
        refusal(submit(
            &w.e,
            &r,
            &short,
            q["case_hash"].as_str().unwrap(),
            "pass",
            &[],
            None
        )),
        (
            E_NOT_FOUND,
            format!("{short} matches 2 requests"),
            "pass more hex digits".into()
        )
    );
    // more digits resolve it
    let done = submit(
        &w.e,
        &r,
        &rq,
        q["case_hash"].as_str().unwrap(),
        "pass",
        &[],
        None,
    )
    .unwrap();
    assert_eq!(done["request"], rq.as_str());
}

#[test]
fn ck6_list_subject_matching_precedes_the_cap() {
    let w = world();
    let a = ag("a");
    let p = w.p.to_string();
    // an editorial change, and its proposal version
    let aw = w.s.a.clone();
    let change = rdy_engine::engine::with_writer_result(&w.e, move |r| {
        Ok(change_of(&put_j(r, "a", upd(&aw, "A holds!"), j_typo())))
    })
    .unwrap();
    let version = rdy_engine::engine::with_writer_result(&w.e, move |r| {
        r.propose(&ag("a"), "a", "a", Some(TEST_SUMMARY))
    })
    .unwrap();
    let by_change = run_on(&w, &a, "meaning_preservation", json!(change.to_string()));
    let by_version = run_on(&w, &a, "meaning_preservation", json!(version.r#ref));
    let on_p = run_on(&w, &a, "decision_review", json!(p));
    let pair = run_on(
        &w,
        &writer(),
        "contradiction",
        json!([w.s.a.to_string(), w.s.c.to_string()]),
    );
    let only = |args: Value| -> Vec<Value> {
        list(&w.e, &reader(), args)["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["request"].clone())
            .collect()
    };
    // the governing decision is a contextual row of the change's case: no match by it
    let ctx = &by_change["case"]["blocks"];
    assert!(
        ctx.as_array()
            .unwrap()
            .iter()
            .any(|b| b["id"] == w.s.decision.to_string() && b["role"] == "decision"),
        "{ctx}"
    );
    assert!(only(json!({"subject": w.s.decision.to_string()})).is_empty());
    // block membership in the primary blocks
    let a_rows = only(json!({"subject": w.s.a.to_string()}));
    assert!(a_rows.contains(&pair["request"]));
    assert!(a_rows.contains(&by_change["request"]));
    assert!(a_rows.contains(&by_version["request"]));
    assert!(!a_rows.contains(&on_p["request"]));
    let c_rows = only(json!({"subject": w.s.c.to_string()}));
    assert_eq!(c_rows, vec![pair["request"].clone()]);
    // an exact pair, either order
    assert_eq!(
        only(json!({"subject": [w.s.c.to_string(), w.s.a.to_string()]})),
        vec![pair["request"].clone()]
    );
    // a change's and a version's full hash
    assert_eq!(
        only(json!({"subject": change.to_string()})),
        vec![by_change["request"].clone()]
    );
    assert_eq!(
        only(json!({"subject": version.r#ref})),
        vec![by_version["request"].clone()]
    );
    assert_eq!(
        only(json!({"subject": version.id})),
        vec![by_version["request"].clone()]
    );
    // no subject: every request
    assert_eq!(only(json!({})).len(), 4);
    // resolver errors stay exact
    let st = w.e.read();
    let want = st
        .resolve_id(&Scope::View("canon".into()), "no/such/doc")
        .err()
        .or_else(|| st.resolve_id(&Scope::Canon, "no/such/doc").err())
        .unwrap();
    drop(st);
    let got = review(
        &w.e,
        &reader(),
        json!({"op": "list", "subject": "no/such/doc"}),
    )
    .unwrap_err();
    assert_eq!(
        (got.code, got.message, got.fix),
        (want.code, want.message, want.fix)
    );
    // a matching request beyond the first 256 records still appears
    let target_hash = hash_of(&on_p["request"]);
    let filler = record(&w.e, &pair["request"]);
    let mut below = only(json!({}))
        .iter()
        .filter(|r| hash_of(r) < target_hash)
        .count();
    let mut at = 1_000_000;
    while below < 257 {
        let made = copies(&w.e, &filler, 20, at);
        at += 20;
        below += made.iter().filter(|h| **h < target_hash).count();
    }
    let first: Vec<Value> = only(json!({}));
    assert!(!first.contains(&on_p["request"]), "past the first 256 rows");
    let mine = only(json!({"subject": p}));
    assert!(mine.contains(&on_p["request"]), "matched before the cap");
}

// ---------------------------------------------------------------- code evidence

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=qa",
            "-c",
            "user.email=qa",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, path: &str, text: &str, message: &str) -> String {
    std::fs::write(dir.join(path), text).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--no-verify", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

fn link_repo(project: &Path, checkout: &Path) {
    let mut yaml = std::fs::read_to_string(project.join(".rdy.yaml")).unwrap();
    yaml.push_str("code_repos: [app]\n");
    std::fs::write(project.join(".rdy.yaml"), yaml).unwrap();
    point_repo(project, checkout);
}

fn point_repo(project: &Path, checkout: &Path) {
    std::fs::create_dir_all(project.join(".rdy")).unwrap();
    std::fs::write(
        project.join(".rdy/local.yaml"),
        format!("code_paths:\n  app: {}\n", checkout.display()),
    )
    .unwrap();
}

#[test]
fn ck6_unavailable_code_keeps_an_unanswered_request_open() {
    let w = world();
    let checkout = tempfile::tempdir().unwrap();
    let repo = std::fs::canonicalize(checkout.path()).unwrap();
    git(&repo, &["init", "-q", "--initial-branch=main"]);
    let d = w.s.decision.to_string();
    let from = commit(
        &repo,
        "cache.rs",
        "fn a() {}\n",
        &format!("start\n\nImplements: {d}"),
    );
    let to = commit(
        &repo,
        "cache.rs",
        "fn a() { cache(); }\n",
        &format!("cache\n\nImplements: {d}"),
    );
    link_repo(w.dir.path(), &repo);
    let range = format!("app@{}..{}", &from[..12], &to[..12]);
    let subject = json!({"decision": d, "code": range});
    let q = request_on(&w, &writer(), "diff_contradicts", subject.clone());
    let stored = record(&w.e, &q["request"]);
    assert_eq!(
        stored
            .subject
            .code
            .as_ref()
            .map(|c| (c.from.clone(), c.to.clone())),
        Some((from.clone(), to.clone())),
        "the resolved commits are stored"
    );
    // the canonical code tuple filters the list
    let rows = list(&w.e, &reader(), json!({"subject": subject}));
    assert_eq!(rows["requests"][0]["request"], q["request"]);
    let status = |e: &Local| list(e, &reader(), json!({}))["requests"][0]["status"].clone();
    let r = reviewer("agent:r", "r");
    let offered = |owner: u64| {
        let got = next(&w.e, &r, owner, "diff_contradicts").unwrap()["request"].clone();
        w.e.leases().release_owner(owner);
        got
    };
    assert_eq!(status(&w.e), "open");
    assert_eq!(offered(1), q["request"]);
    // 1. a missing checkout
    let moved = repo.with_extension("moved");
    std::fs::rename(&repo, &moved).unwrap();
    assert_eq!(status(&w.e), "open", "unavailable is not stale");
    assert_eq!(offered(2), Value::Null, "the queue skips it");
    let refused = submit(
        &w.e,
        &r,
        q["request"].as_str().unwrap(),
        q["case_hash"].as_str().unwrap(),
        "pass",
        &[],
        None,
    )
    .unwrap_err();
    assert!(
        [E_BAD_INPUT, rdy_engine::E_CONFIG].contains(&refused.code),
        "the code-evidence refusal: {refused:?}"
    );
    std::fs::rename(&moved, &repo).unwrap();
    assert_eq!(offered(3), q["request"], "restored, it is offered again");
    // 2. a recorded commit the checkout does not hold
    let other = tempfile::tempdir().unwrap();
    git(other.path(), &["init", "-q", "--initial-branch=main"]);
    commit(
        other.path(),
        "cache.rs",
        "fn b() {}\n",
        &format!("other\n\nImplements: {d}"),
    );
    point_repo(w.dir.path(), &std::fs::canonicalize(other.path()).unwrap());
    assert_eq!(status(&w.e), "open");
    assert_eq!(offered(4), Value::Null);
    point_repo(w.dir.path(), &repo);
    assert_eq!(offered(5), q["request"]);
    // 3. a git read that fails: the new blob is gone
    let blob = git(&repo, &["rev-parse", &format!("{to}:cache.rs")]);
    let object = repo.join(".git/objects").join(&blob[..2]).join(&blob[2..]);
    let saved = std::fs::read(&object).unwrap();
    std::fs::remove_file(&object).unwrap();
    // git would read a matching worktree file in its place
    std::fs::remove_file(repo.join("cache.rs")).unwrap();
    assert_eq!(status(&w.e), "open");
    assert_eq!(offered(6), Value::Null);
    std::fs::write(&object, saved).unwrap();
    git(&repo, &["checkout", "--", "cache.rs"]);
    assert_eq!(offered(7), q["request"]);
    assert_eq!(status(&w.e), "open");
    // store damage stays an error: a request record that does not decode
    let damaged = rdy_engine::engine::with_writer_result(&w.e, |r| {
        let item = rdy_store::codec::item_new(rdy_store::kinds::CHECK_REQUEST, "not a request");
        let ids = r.store_mut().write(vec![item]).map_err(RdyError::from)?;
        r.invalidate();
        Ok(ids)
    });
    let (code, msg) = match damaged {
        Ok(_) => {
            let e = review(&w.e, &reader(), json!({"op": "list"})).unwrap_err();
            (e.code, e.message)
        }
        Err(e) => panic!("the damaged record was not written: {e:?}"),
    };
    assert_eq!(code, rdy_engine::E_IO, "{msg}");
    assert!(msg.starts_with("cannot decode object"), "{msg}");
}

#[test]
fn ck6_new_verdicts_do_not_change_readiness_policy_or_coverage() {
    let w = world();
    let a = ag("a");
    let cw = w.s.c.clone();
    with(&w.e, move |r| {
        put_j(r, "a", upd(&cw, "C stands alone!"), j_typo())
    });
    let version = rdy_engine::engine::with_writer_result(&w.e, move |r| {
        r.propose(&ag("a"), "a", "a", Some(TEST_SUMMARY))
    })
    .unwrap();
    let facts = |e: &Local| -> Value {
        let st = e.read();
        let v: Hash = version.id.parse().unwrap();
        let pref = e.inspect(|r| r.resolve_proposal(&version.id)).unwrap();
        let c = Candidate::stored(&st, pref, st.reader().proposal(&v).unwrap()).unwrap();
        let ready = readiness(&st, &c).unwrap();
        let assessed = assess(&st, &c, &st.gate_config()).unwrap();
        let coverage = st.coverage_report(&Scope::View("a".into()), None).unwrap();
        json!({
            "issues": format!("{:?}", ready.issues),
            "assessment": format!("{:?}", assessed.assessment),
            "coverage": serde_json::to_value(&coverage).unwrap(),
        })
    };
    let before = facts(&w.e);
    // an unanswered request
    request_on(&w, &a, "meaning_preservation", json!(version.r#ref));
    assert_eq!(facts(&w.e), before, "an unanswered request");
    // pass, concern and fail
    let r = reviewer("agent:r", "r");
    for verdict in ["pass", "concern", "fail"] {
        let q = request_on(&w, &a, "meaning_preservation", json!(version.r#ref));
        let c = w.s.c.to_string();
        let (cites, reason): (Vec<(&str, &str)>, Option<&str>) = if verdict == "pass" {
            (vec![], None)
        } else {
            (
                vec![(c.as_str(), "the meaning moved")],
                Some("it reads otherwise"),
            )
        };
        submit(
            &w.e,
            &r,
            q["request"].as_str().unwrap(),
            q["case_hash"].as_str().unwrap(),
            verdict,
            &cites,
            reason,
        )
        .unwrap();
        assert_eq!(facts(&w.e), before, "after a {verdict}");
    }
}

/// Fix round 1 (review finding 1): a proposal's case reads only the net changes its own selected
/// changes make. A block those changes edit and restore stays out of the case when a later view
/// edit, which is not in the version, changes it again.
#[test]
fn ck6_version_case_keeps_its_own_net_blocks() {
    let w = world();
    let a = ag("a");
    let (aw, cw) = (w.s.a.clone(), w.s.c.clone());
    with(&w.e, move |r| {
        put_j(r, "a", upd(&aw, "A holds!"), j_typo());
        put_j(r, "a", upd(&aw, "A holds."), j_typo());
        put_j(r, "a", upd(&cw, "C stands alone!"), j_typo())
    });
    let version = rdy_engine::engine::with_writer_result(&w.e, move |r| {
        r.propose(&ag("a"), "a", "a", Some(TEST_SUMMARY))
    })
    .unwrap();
    let first = request_on(&w, &a, "meaning_preservation", json!(version.r#ref));
    assert_eq!(
        record(&w.e, &first["request"]).subject.blocks,
        vec![w.s.c.clone()]
    );
    // final review F1: the case shows the version's own text and versions
    let c_in = |scope: Scope| w.e.read().materialized(&scope).unwrap().version_of(&w.s.c);
    let c_sides = |case: &Value| -> (Value, Value) {
        let row = case["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["id"] == w.s.c.to_string())
            .cloned()
            .unwrap_or_else(|| panic!("a row of C: {case}"));
        (row["before"].clone(), row["after"].clone())
    };
    let side = |v: Option<Hash>, text: &str| json!({"version": v.unwrap().to_string(), "text": [{"field": "body", "text": text}]});
    let want = (
        side(c_in(Scope::Canon), "C stands alone."),
        side(c_in(Scope::View("a".into())), "C stands alone!"),
    );
    assert_eq!(c_sides(&first["case"]), want, "{}", first["case"]);
    // later edits of A and C in the view, not in the version
    let (aw, cw) = (w.s.a.clone(), w.s.c.clone());
    with(&w.e, move |r| {
        put_j(r, "a", upd(&aw, "A holds!!"), j_typo());
        put_j(r, "a", upd(&cw, "C stands alone!!"), j_typo())
    });
    let row = |label: &Value| -> Value {
        list(&w.e, &reader(), json!({}))["requests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["request"] == *label)
            .cloned()
            .unwrap()
    };
    assert_eq!(row(&first["request"])["status"], "open");
    let again = request_on(&w, &a, "meaning_preservation", json!(version.r#ref));
    assert_eq!(
        record(&w.e, &again["request"]).subject.blocks,
        vec![w.s.c.clone()]
    );
    assert_eq!(again["case_hash"], first["case_hash"], "{}", again["case"]);
    assert_eq!(
        c_sides(&again["case"]),
        want,
        "the excluded later edit of C"
    );
    let ids: Vec<&Value> = again["case"]["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| &b["id"])
        .collect();
    assert!(
        !ids.contains(&&json!(w.s.a.to_string())),
        "{}",
        again["case"]
    );
}

/// Fix round 1 (review finding 2): the authors of a primary block the subject deletes are read
/// from the deletion and the displayed before version, so another run of the author cannot claim
/// or judge the request.
#[test]
fn ck6_deleted_subject_keeps_its_authors() {
    let (_dir, mut r) = project();
    let s = spec_doc(&mut r);
    let made = put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "id": "$x", "parent": s.section.to_string(),
                "body": "X is short-lived."}]),
    );
    let x = created(&made, "$x");
    land(&mut r, "a").unwrap();
    let gone = change_of(&put_j(
        &mut r,
        "a",
        json!([{"op": "delete", "id": x.to_string()}]),
        j_typo(),
    ));
    let e = Local::new(r);
    let q = ok(
        &e,
        &ag("a"),
        json!({"op": "request", "check": "meaning_preservation", "subject": gone.to_string()}),
    );
    let (rq, ch) = (
        q["request"].as_str().unwrap().to_string(),
        q["case_hash"].as_str().unwrap().to_string(),
    );
    assert_eq!(record(&e, &q["request"]).subject.blocks, vec![x.clone()]);
    // another run of agent:a neither claims nor judges it
    let same = reviewer("agent:a", "b");
    let seq = e.read().seq();
    assert_eq!(
        refusal(submit(&e, &same, &rq, &ch, "pass", &[], None)),
        (
            E_SELF_REVIEW,
            format!(
                "actor agent:a wrote block {x} of {rq}; another run of the same actor is not independent"
            ),
            "a reviewer of another actor judges it".into()
        )
    );
    assert_eq!(e.read().seq(), seq, "a refusal appends nothing");
    assert_eq!(
        next(&e, &same, 7, "meaning_preservation").unwrap()["request"],
        Value::Null
    );
    // a reviewer of another actor claims and judges it
    let other = reviewer("agent:r", "r");
    assert_eq!(
        next(&e, &other, 8, "meaning_preservation").unwrap()["request"],
        rq.as_str()
    );
    submit(&e, &other, &rq, &ch, "pass", &[], None).unwrap();
}
