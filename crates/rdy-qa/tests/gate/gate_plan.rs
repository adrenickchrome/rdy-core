//! cp10 (D-4JMPH, system/store p-8D6ES, concepts/briefings p-1FHVQ, p-AEYTC, O5, O6, O16, O19):
//! a landing seals its version's change plan in its own batch, after the landing and before its
//! signatures: every item with its outcome, the decisions it names, the cumulative answers and
//! the recorded HEADs. An acceptance version binds a plan of its own; a landing with no item and
//! no re-affirmation writes no seal; a version proposed before store 17 lands under the store-16
//! rules with a seal that names no earlier chain version. A failure at any frame, or a writer
//! that moves during signing, leaves no half.
use rdy_engine::engine::{Local, with_writer_result};
use rdy_engine::plan::{PlanAnswerInput, PlanQuery, ProposePlanRequest};
use rdy_engine::signing_act::{SigningAct, run_signed_act};
use rdy_engine::signing_crypto::{AgentIdentity, SigningAgent};
use rdy_engine::{DecisionInput, E_STALE_BASE, LandOptions, ProposalInfo, Rdy, RdyError};
use rdy_model::plan::{Plan, PlanAct, PlanHead, PlanKind, PlanOutcome, item_display, item_hash};
use rdy_model::{BlockId, Hash};
use rdy_qa::fixtures::{approver, project, write, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY, confirm_exceptions};
use rdy_qa::signing::{QaAgent, TestKey};
use rdy_store::kinds;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

// ---- the project ----

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn h(s: &str) -> Hash {
    s.parse().unwrap()
}

/// Canon: doc `spec`, section `Rules`, claims a ("A holds 30 days."), c ("C stands."), b, which
/// references a and c, and p, which states a's literal.
struct Spec {
    s: String,
    a: String,
    b: String,
    c: String,
    p: String,
}

fn spec(r: &mut Rdy) -> Spec {
    let w = write(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C stands."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}} and {{$c}}."},
            {"op": "create", "type": "claim", "id": "$p", "parent": "$s", "body": "Wait 30 days per {{$a}}."}
        ]),
    )
    .unwrap();
    rdy_qa::fixtures::land(r, "seed").unwrap();
    let get = |k: &str| w.created[k].clone();
    Spec {
        s: get("$s"),
        a: get("$a"),
        b: get("$b"),
        c: get("$c"),
        p: get("$p"),
    }
}

fn edit(r: &mut Rdy, view: &str, target: &str, body: &str) -> String {
    write(
        r,
        view,
        json!([{"op": "update", "id": target, "body": body}]),
    )
    .unwrap_or_else(|e| panic!("edit {target} in {view}: {e}"))
    .change
}

fn claim(r: &mut Rdy, view: &str, section: &str, body: &str) -> String {
    write(
        r,
        view,
        json!([{"op": "create", "type": "claim", "id": "$x", "parent": section, "body": body}]),
    )
    .unwrap_or_else(|e| panic!("claim in {view}: {e}"))
    .created["$x"]
        .clone()
}

/// Proposes `view` with a summary (a version that lands a decision is approved through a card).
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

/// Confirms the exceptions beside `pref` and lands version `version`; returns the landing.
fn land_version(r: &mut Rdy, pref: &str, version: &str) -> Hash {
    confirm_exceptions(r, &approver(), pref);
    land_now(r, version)
}

/// Lands version `version` as the approver, its exceptions already confirmed.
fn land_now(r: &mut Rdy, version: &str) -> Hash {
    h(&r.land(&approver(), version)
        .unwrap_or_else(|e| panic!("land {version}: {e}")))
}

fn land(r: &mut Rdy, p: &ProposalInfo) -> Hash {
    land_version(r, &p.r#ref, &p.id)
}

/// The person accepts the deferral card of `pref` (D-AAQ9Z, Task 12); returns the ANSWER.
fn accept_deferrals(r: &mut Rdy, pref: &str) -> Hash {
    let info = r
        .answer(
            &approver(),
            PERSON,
            rdy_engine::answer::AnswerRequest {
                r#ref: format!("defer:{pref}"),
                option: "o1".into(),
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("accept the deferrals of {pref}: {e}"));
    assert_eq!(info.act, "accept_deferrals");
    h(info.answer.as_deref().unwrap())
}

/// A version of `view` as a store-16 engine wrote it: the raw proposal writer, no plan.
fn unbound(r: &mut Rdy, view: &str) -> (Hash, String) {
    let by = writer().provenance();
    let changes = r.store().included(view).unwrap().as_ref().clone();
    let v = r
        .store_mut()
        .propose_changes(view, view, Some(TEST_SUMMARY), by, changes, None)
        .unwrap();
    r.invalidate();
    (v, format!("P-{}@1", v.short()))
}

fn pi(cause: &str, subject: &str, kind: PlanKind) -> String {
    item_display(&item_hash(&id(cause), subject, kind))
}

fn ans(item: &str, act: PlanAct, reason: Option<&str>) -> PlanAnswerInput {
    PlanAnswerInput {
        item: item.into(),
        act,
        reason: reason.map(str::to_string),
    }
}

fn bound(r: &Rdy, version: &Hash) -> Option<(Hash, Plan)> {
    let st = r.read();
    let reader = st.reader();
    let b = reader
        .plan_of_proposal(version, r.store().log_end())
        .unwrap()?;
    Some((b, reader.plan(&b).unwrap()))
}

fn seal(r: &Rdy, landing: &Hash) -> Option<(Hash, Plan)> {
    let st = r.read();
    let reader = st.reader();
    let s = reader
        .plan_of_landing(landing, r.store().log_end())
        .unwrap()?;
    Some((s, reader.plan(&s).unwrap()))
}

fn plan_ref(r: &Rdy, view: &str) -> Option<Hash> {
    r.read()
        .reader()
        .get_ref(&rdy_store::plan::plan_ref(view))
        .unwrap()
        .map(|v| Hash(v.as_slice().try_into().unwrap()))
}

fn plans_in(r: &Rdy) -> usize {
    let mut n = 0;
    r.store()
        .for_each_object_of(kinds::PLAN, |_| {
            n += 1;
            Ok(())
        })
        .unwrap();
    n
}

fn read_plan(r: &Rdy, q: PlanQuery) -> Value {
    let q = PlanQuery {
        max_tokens: Some(100_000),
        ..q
    };
    serde_json::to_value(r.read().change_plan(&q, "work").unwrap()).unwrap()
}

fn proposal_q(pref: &str) -> PlanQuery {
    PlanQuery {
        proposal: Some(pref.into()),
        ..Default::default()
    }
}

fn landing_q(l: &Hash) -> PlanQuery {
    PlanQuery {
        landing: Some(l.to_string()),
        ..Default::default()
    }
}

/// Every ref and the log end.
fn log_state(r: &Rdy) -> (Location, Vec<(String, Vec<u8>)>) {
    (
        r.store().log_end(),
        r.read().reader().refs_with_prefix("").unwrap(),
    )
}

/// The kinds of the frames from `from` to the log end, markers included.
fn frames_from(r: &Rdy, from: Location) -> Vec<u8> {
    let mut out = vec![];
    r.store()
        .reader()
        .for_each_frame_from(from, r.store().log_end(), |_, kind, _, _, _| {
            out.push(kind);
            Ok(())
        })
        .unwrap();
    out
}

fn outcome(p: &Plan, cause: &str, subject: &str, kind: PlanKind) -> PlanOutcome {
    let want = item_hash(&id(cause), subject, kind);
    p.seal
        .as_ref()
        .unwrap()
        .items
        .iter()
        .find(|i| i.id == want)
        .unwrap_or_else(|| panic!("no sealed item {}: {p:#?}", item_display(&want)))
        .outcome
}

// ---- git ----

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
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, path: &str, message: &str) -> String {
    std::fs::write(dir.join(path), format!("{message}\n")).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--no-verify", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

/// A fresh checkout linked to the project at `project` as code repo `app`.
fn linked_repo(project: &Path) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "--initial-branch=main"]);
    let cfg = project.join(".rdy.yaml");
    let mut text = std::fs::read_to_string(&cfg).unwrap_or_default();
    text.push_str("code_repos: [app]\n");
    std::fs::write(&cfg, text).unwrap();
    std::fs::write(
        project.join(".rdy/local.yaml"),
        format!(
            "code_paths:\n  app: {}\n",
            serde_json::to_string(&tmp.path().display().to_string()).unwrap()
        ),
    )
    .unwrap();
    tmp
}

fn heads_at(head: &str) -> Vec<PlanHead> {
    vec![PlanHead {
        repo: "app".into(),
        head: Some(head.into()),
    }]
}

/// An accepted decision with `statement`, made in view `view` and landed.
fn decision(
    r: &mut Rdy,
    view: &str,
    section: &str,
    statement: &str,
    depends_on: &[&str],
) -> String {
    let d = r
        .decide(
            &writer(),
            view,
            DecisionInput {
                parent: section.into(),
                statement: statement.into(),
                depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("decide {statement}: {e}"));
    let out = d.created["$decision"].clone();
    rdy_qa::fixtures::land(r, view).unwrap();
    out
}

// ---- tests ----

/// O19, p-AEYTC: a version proposed before store 17 reads with plan null and its computed items,
/// with no answer and no plan warning, lands under the store-16 rules, and its seal names no
/// earlier chain version (not the view's answer-only chain either), has no answers, the HEADs
/// captured once as both ends, each item's computed close else open and no answer pointer, and
/// becomes `plan/<view>`. A zero-item version writes no plan; an approval-generated final
/// version binds none and the seal names it.
#[test]
fn cp10_pre17_proposal_lands_under_store16_rules() {
    let (d, mut r) = project();
    let repo = linked_repo(d.path());
    let s = spec(&mut r);
    let h1 = commit(
        repo.path(),
        "f.rs",
        &format!("start\n\nImplements: {}", s.a),
    );
    edit(&mut r, "w", &s.a, "A holds 60 days.");
    edit(
        &mut r,
        "w",
        &s.p,
        &format!("Wait 60 days per {{{{{}}}}}.", s.a),
    );
    // an answer-only chain in the view, then the store-16 version
    let confirm = pi(&s.a, &s.b, PlanKind::Confirm);
    let chain = r
        .answer_plan(
            &writer(),
            "w",
            &[ans(&confirm, PlanAct::Defer, Some("later"))],
        )
        .unwrap();
    let (old, pref) = unbound(&mut r, "w");
    let read = read_plan(&r, proposal_q(&pref));
    assert_eq!(read["plan"], Value::Null);
    assert_eq!(read["notes"], json!([]));
    let items = read["items"].as_array().unwrap();
    assert!(!items.is_empty(), "{read:#}");
    assert!(items.iter().all(|i| i.get("answer").is_none()), "{read:#}");
    let row = items
        .iter()
        .find(|i| i["id"] == confirm)
        .expect("b's confirm");
    assert_eq!(
        row["state"], "open",
        "the chain's defer is no answer of the version"
    );

    confirm_exceptions(&mut r, &approver(), &pref);
    let start = r.store().log_end();
    let l = land_now(&mut r, &old.to_string());
    let (sh, sealed) = seal(&r, &l).expect("an item: the landing seals");
    assert_eq!(
        (sealed.stable, sealed.prev),
        (None, None),
        "no earlier chain version"
    );
    assert_ne!(sealed.prev, Some(chain));
    assert!(sealed.answers.is_empty());
    assert_eq!(sealed.heads, heads_at(&h1), "the HEADs captured once");
    assert_eq!(sealed.proposal, Some(old));
    let x = sealed.seal.as_ref().unwrap();
    assert_eq!((x.landing, x.version), (l, old));
    assert!(x.items.iter().all(|i| i.answer.is_none()));
    assert_eq!(
        outcome(&sealed, &s.a, &s.p, PlanKind::Update),
        PlanOutcome::Updated,
        "a computed close"
    );
    assert_eq!(
        outcome(&sealed, &s.a, &s.b, PlanKind::Confirm),
        PlanOutcome::Open,
        "else open"
    );
    assert_eq!(
        outcome(&sealed, &s.a, "app:f.rs", PlanKind::UpdateImplementation),
        PlanOutcome::Open
    );
    assert_eq!(
        plan_ref(&r, "w"),
        Some(sh),
        "the seal ends the view's chain"
    );
    assert!(bound(&r, &old).is_none(), "nothing binds the old version");
    let kinds_of = frames_from(&r, start);
    assert_eq!(
        kinds_of
            .iter()
            .filter(|k| **k == kinds::BATCH_COMMIT)
            .count(),
        1,
        "one batch: {kinds_of:?}"
    );

    // a zero-item version writes no plan
    let z = claim(&mut r, "z", &s.s, "Z stands alone.");
    assert!(!z.is_empty());
    let (zv, zref) = unbound(&mut r, "z");
    let before = plans_in(&r);
    let zl = land_version(&mut r, &zref, &zv.to_string());
    assert!(seal(&r, &zl).is_none());
    assert_eq!(plans_in(&r), before);
    assert_eq!(plan_ref(&r, "z"), None);

    // an approval-generated final version: the seal names it, and nothing binds it
    edit(&mut r, "dv", &s.c, "C stands firm.");
    r.decide(
        &writer(),
        "dv",
        DecisionInput {
            parent: s.s.clone(),
            statement: "Keep C firm".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let (dvv, dvref) = unbound(&mut r, "dv");
    let dl = land_version(&mut r, &dvref, &dvv.to_string());
    let landing = r.read().reader().landing(&dl).unwrap();
    assert_ne!(
        landing.proposal, dvv,
        "the acceptance wrote a final version"
    );
    let (_, dsealed) = seal(&r, &dl).expect("b's confirm on c: a seal");
    assert_eq!(dsealed.proposal, Some(landing.proposal));
    assert_eq!(dsealed.seal.as_ref().unwrap().version, landing.proposal);
    assert_eq!((dsealed.stable, dsealed.prev), (None, None));
    assert!(
        bound(&r, &landing.proposal).is_none(),
        "no synthesized binding"
    );
    assert!(bound(&r, &dvv).is_none());
    r.fsck().unwrap();
}

/// D-4JMPH: the seal records every computed item with its outcome (all ten) and names exactly
/// the decisions the landing creates, supersedes, revokes, refines and re-affirms; the deferred
/// item points to the version's acceptance of its deferral card (Task 12). It carries the chain's cumulative answers and
/// the version's recorded HEADs, after the final version's own bound plan.
#[test]
fn cp10_landing_seals_all_outcomes_and_named_decisions() {
    let (d, mut r) = project();
    let repo = linked_repo(d.path());
    let s = spec(&mut r);
    let q = claim(
        &mut r,
        "seed2",
        &s.s,
        &format!("Also 30 days per {{{{{}}}}}.", s.a),
    );
    let b2 = claim(
        &mut r,
        "seed2",
        &s.s,
        &format!("B2 builds on {{{{{}}}}}.", s.a),
    );
    let b3 = claim(
        &mut r,
        "seed2",
        &s.s,
        &format!("B3 builds on {{{{{}}}}}.", s.a),
    );
    rdy_qa::fixtures::land(&mut r, "seed2").unwrap();
    let d1 = decision(&mut r, "d1", &s.s, "Keep thirty", &[]);
    let d3 = decision(&mut r, "d3", &s.s, "Keep the old form", &[]);
    let d4 = decision(&mut r, "d4", &s.s, "Keep the review", &[]);
    let sdep = decision(&mut r, "d5", &s.s, "Build on A", &[&s.a]);
    let sret = decision(&mut r, "d6", &s.s, "Build on A again", &[&s.a]);
    let rr = write(
        &mut r,
        "seed3",
        json!([{"op": "create", "type": "claim", "id": "$r", "parent": s.s, "body": "Teams follow it.",
            "links": {"decided_by": [{"target": d1}]}}]),
    )
    .unwrap()
    .created["$r"]
        .clone();
    rdy_qa::fixtures::land(&mut r, "seed3").unwrap();
    let h1 = {
        commit(
            repo.path(),
            "f.rs",
            &format!("start\n\nImplements: {}", s.a),
        );
        commit(
            repo.path(),
            "g.rs",
            &format!("start g\n\nImplements: {}", s.a),
        )
    };

    // the version: a changes, p follows (updated), b is repinned, r is relinked to the decision
    // that supersedes d1, sdep is re-affirmed, sret is superseded, d3 revoked and d4 refined
    edit(&mut r, "w", &s.a, "A holds 60 days.");
    edit(
        &mut r,
        "w",
        &s.p,
        &format!("Wait 60 days per {{{{{}}}}}.", s.a),
    );
    write(&mut r, "w", json!([{"op": "repin", "id": s.b}])).unwrap();
    let dnew = r
        .supersede(
            &writer(),
            "w",
            &d1,
            None,
            DecisionInput {
                parent: s.s.clone(),
                statement: "Keep sixty".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": rr, "links": {"decided_by": [{"target": dnew}]}}]),
    )
    .unwrap();
    let dsup = r
        .supersede(
            &writer(),
            "w",
            &sret,
            None,
            DecisionInput {
                parent: s.s.clone(),
                statement: "Build on A at sixty".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let drev = r
        .revoke(&writer(), "w", &d3, "The old form is gone.", &s.s, None)
        .unwrap()
        .created["$decision"]
        .clone();
    let dref = r
        .decide(
            &writer(),
            "w",
            DecisionInput {
                parent: s.s.clone(),
                statement: "Review weekly".into(),
                refines: vec![d4.clone()],
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    write(
        &mut r,
        "w",
        json!([{"op": "reaffirm", "id": sdep, "reason": "It still builds on A."}]),
    )
    .unwrap();
    // the chain's first version fixes the code baseline at h1; a later commit touches f.rs
    let first = r
        .answer_plan(
            &writer(),
            "w",
            &[
                ans(
                    &pi(&s.a, &q, PlanKind::Update),
                    PlanAct::Defer,
                    Some("q follows later"),
                ),
                ans(
                    &pi(&s.a, &b2, PlanKind::Confirm),
                    PlanAct::Confirm,
                    Some("B2 still holds"),
                ),
                ans(
                    &pi(&s.a, "app:g.rs", PlanKind::UpdateImplementation),
                    PlanAct::Done,
                    None,
                ),
            ],
        )
        .unwrap();
    let h2 = commit(
        repo.path(),
        "f.rs",
        &format!("follow\n\nImplements: {}", s.a),
    );
    let p = propose(&mut r, "w", &[]);
    let (bh, bplan) = bound(&r, &h(&p.id)).unwrap();
    assert_eq!(bplan.heads, heads_at(&h2));
    let before = read_plan(&r, proposal_q(&p.r#ref));
    confirm_exceptions(&mut r, &approver(), &p.r#ref);
    let accepted = accept_deferrals(&mut r, &p.r#ref);
    let start = r.store().log_end();
    let l = land_now(&mut r, &p.id);
    let landing = r.read().reader().landing(&l).unwrap();
    let fin = landing.proposal;
    assert_ne!(fin, h(&p.id), "the proposed decisions' acceptance");
    let (fh, fplan) = bound(&r, &fin).expect("the final version binds its plan");
    assert_eq!((fplan.stable, fplan.prev), (Some(first), Some(bh)));
    let (sh, sealed) = seal(&r, &l).unwrap();
    assert_eq!((sealed.stable, sealed.prev), (Some(first), Some(fh)));
    assert_eq!(sealed.heads, heads_at(&h2), "the version's recorded HEADs");
    assert_ne!(Some(h1), sealed.heads[0].head);
    assert_eq!(sealed.answers, bplan.answers, "the cumulative answers");
    assert_eq!(sealed.answers.len(), 3);
    let x = sealed.seal.as_ref().unwrap();
    assert_eq!((x.landing, x.version), (l, fin));
    assert_eq!(plan_ref(&r, "w"), Some(sh));

    // the ten outcomes
    let want = [
        (&s.a, &s.p, PlanKind::Update, PlanOutcome::Updated),
        (&d1, &rr, PlanKind::Rejustify, PlanOutcome::Relinked),
        (&s.a, &s.b, PlanKind::Confirm, PlanOutcome::Repinned),
        (&s.a, &sdep, PlanKind::Reaffirm, PlanOutcome::Reaffirmed),
        (&s.a, &sret, PlanKind::Reaffirm, PlanOutcome::Retired),
        (
            &s.a,
            &"app:f.rs".to_string(),
            PlanKind::UpdateImplementation,
            PlanOutcome::Committed,
        ),
        (&s.a, &b2, PlanKind::Confirm, PlanOutcome::Confirmed),
        (
            &s.a,
            &"app:g.rs".to_string(),
            PlanKind::UpdateImplementation,
            PlanOutcome::Done,
        ),
        (&s.a, &q, PlanKind::Update, PlanOutcome::Deferred),
        (&s.a, &b3, PlanKind::Confirm, PlanOutcome::Open),
    ];
    for (cause, subject, kind, o) in want {
        assert_eq!(
            outcome(&sealed, cause, subject, kind),
            o,
            "{} on {subject}",
            kind.as_str()
        );
    }
    // Task 12: the deferred document item points to the version's acceptance; no other does
    for i in &x.items {
        let want = (i.outcome == PlanOutcome::Deferred).then_some(accepted);
        assert_eq!(i.answer, want, "{i:#?}");
    }
    // the seal holds exactly the version's items, as its read listed them
    let listed: BTreeSet<String> = before["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect();
    let sealed_ids: BTreeSet<String> = x.items.iter().map(|i| item_display(&i.id)).collect();
    assert_eq!(sealed_ids, listed);
    // the five decision roles, and no other governing decision
    let mut roles: Vec<BlockId> = [&dnew, &dsup, &drev, &dref, &d1, &sret, &d3, &d4, &sdep]
        .into_iter()
        .map(|x| id(x))
        .collect();
    roles.sort();
    assert_eq!(x.decisions, roles);
    // one batch: the final version, the landing, its bound plan, then the seal
    let kinds_of = frames_from(&r, start);
    assert_eq!(
        kinds_of
            .iter()
            .filter(|k| **k == kinds::BATCH_COMMIT)
            .count(),
        1,
        "{kinds_of:?}"
    );
    let at = |k: u8| kinds_of.iter().position(|x| *x == k).unwrap();
    let plans: Vec<usize> = kinds_of
        .iter()
        .enumerate()
        .filter(|(_, k)| **k == kinds::PLAN)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(plans.len(), 2, "{kinds_of:?}");
    assert!(at(kinds::PROPOSAL) < at(kinds::LANDING) && at(kinds::LANDING) < plans[0]);
    r.fsck().unwrap();
}

/// D-4JMPH: an approval that writes a final acceptance version binds a plan of its own in the
/// landing's batch, with the approved candidate's assessment and HEADs; the approved version's
/// plan is not changed, and the seal names the landed version. With no acceptance, the
/// approved version is the landed one and gets no second binding.
#[test]
fn cp10_acceptance_version_has_its_own_plan_binding() {
    let (_d, mut r) = project();
    let s = spec(&mut r);
    edit(&mut r, "w", &s.a, "A holds 60 days.");
    r.decide(
        &writer(),
        "w",
        DecisionInput {
            parent: s.s.clone(),
            statement: "Hold sixty".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let p = propose(&mut r, "w", &[]);
    let (bh, bplan) = bound(&r, &h(&p.id)).unwrap();
    let raw_before = r.read().reader().get_raw(&bh).unwrap();
    let read = read_plan(&r, proposal_q(&p.r#ref));
    let l = land(&mut r, &p);
    let landing = r.read().reader().landing(&l).unwrap();
    assert_eq!(landing.approved_version, Some(h(&p.id)));
    let fin = landing.proposal;
    assert_ne!(fin, h(&p.id));
    // the approved version keeps its plan, byte for byte
    assert_eq!(bound(&r, &h(&p.id)).map(|(x, _)| x), Some(bh));
    assert_eq!(r.read().reader().get_raw(&bh).unwrap(), raw_before);
    // the final version's plan: the approved candidate's answers and HEADs, after its plan
    let (fh, fplan) = bound(&r, &fin).unwrap();
    assert_eq!(fplan.proposal, Some(fin));
    assert_eq!((fplan.stable, fplan.prev), (Some(bh), Some(bh)));
    assert_eq!(fplan.heads, bplan.heads);
    assert_eq!(fplan.answers, bplan.answers);
    assert!(fplan.seal.is_none());
    let (_, sealed) = seal(&r, &l).unwrap();
    assert_eq!(sealed.prev, Some(fh));
    let x = sealed.seal.as_ref().unwrap();
    assert_eq!(x.version, fin);
    let listed: Vec<String> = read["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect();
    let sealed_ids: Vec<String> = x.items.iter().map(|i| item_display(&i.id)).collect();
    assert_eq!(sealed_ids, listed, "the approved candidate's items");

    // no acceptance: the approved version lands as is and binds once
    edit(&mut r, "v", &s.c, "C stands firm.");
    let pv = propose(&mut r, "v", &[]);
    let lv = land(&mut r, &pv);
    let lrec = r.read().reader().landing(&lv).unwrap();
    assert_eq!(lrec.proposal, h(&pv.id));
    let (vb, _) = bound(&r, &h(&pv.id)).unwrap();
    let (_, vs) = seal(&r, &lv).unwrap();
    assert_eq!(vs.prev, Some(vb));
    assert_eq!(vs.seal.as_ref().unwrap().version, h(&pv.id));
    r.fsck().unwrap();
}

/// D-4JMPH, p-8D6ES: a landing seals the answers the approved version bound, not answers the
/// view recorded after that propose. An answer-only version after the propose replaces one
/// bound answer and adds another: the approved version's read and its seal still hold the bound
/// ones, on both landing paths (the approved version lands as is, or an acceptance writes a
/// final version whose own binding carries the approved answers). The landing's plans still
/// follow the view's chain, after the answer-only version.
#[test]
fn cp10_landing_seals_the_approved_versions_bound_answers() {
    for acceptance in [false, true] {
        let (_d, mut r) = project();
        let s = spec(&mut r);
        edit(&mut r, "w", &s.a, "A holds 60 days.");
        if acceptance {
            r.decide(
                &writer(),
                "w",
                DecisionInput {
                    parent: s.s.clone(),
                    statement: "Hold sixty".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let confirm = pi(&s.a, &s.b, PlanKind::Confirm);
        let update = pi(&s.a, &s.p, PlanKind::Update);
        let p = propose(&mut r, "w", &[ans(&confirm, PlanAct::Defer, Some("later"))]);
        let (bh, bplan) = bound(&r, &h(&p.id)).unwrap();
        assert_eq!(bplan.answers.len(), 1, "{acceptance}");
        // after the propose: the confirm is replaced and the update deferred, in the view only
        let after = r
            .answer_plan(
                &writer(),
                "w",
                &[
                    ans(&confirm, PlanAct::Confirm, Some("B holds after all")),
                    ans(&update, PlanAct::Defer, Some("p follows later")),
                ],
            )
            .unwrap();
        let read = read_plan(&r, proposal_q(&p.r#ref));
        let state = |item: &str| {
            read["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| i["id"] == item)
                .unwrap_or_else(|| panic!("{item}: {read:#}"))["state"]
                .clone()
        };
        assert_eq!(state(&confirm), "deferred", "{acceptance}: {read:#}");
        assert_eq!(state(&update), "open", "{acceptance}: {read:#}");

        let accepted = accept_deferrals(&mut r, &p.r#ref);
        let l = land(&mut r, &p);
        let landing = r.read().reader().landing(&l).unwrap();
        let fin = landing.proposal;
        assert_eq!(
            fin != h(&p.id),
            acceptance,
            "the acceptance writes a final version"
        );
        let (sh, sealed) = seal(&r, &l).expect("two items: a seal");
        assert_eq!(
            sealed.answers, bplan.answers,
            "{acceptance}: the bound answers"
        );
        assert_eq!(
            outcome(&sealed, &s.a, &s.b, PlanKind::Confirm),
            PlanOutcome::Deferred,
            "{acceptance}: the bound defer"
        );
        let deferred = item_hash(&id(&s.a), &s.b, PlanKind::Confirm);
        let x = sealed.seal.as_ref().unwrap();
        let item = x.items.iter().find(|i| i.id == deferred).unwrap();
        assert_eq!(item.answer, Some(accepted), "{acceptance}: the acceptance");
        assert_eq!(
            outcome(&sealed, &s.a, &s.p, PlanKind::Update),
            PlanOutcome::Open,
            "{acceptance}: the later defer is no answer of the version"
        );
        if acceptance {
            let (fh, fplan) = bound(&r, &fin).expect("the final version binds its plan");
            assert_eq!(fplan.answers, bplan.answers, "the final binding's answers");
            assert_eq!(fplan.heads, bplan.heads);
            assert_eq!((fplan.stable, fplan.prev), (Some(bh), Some(after)));
            assert_eq!((sealed.stable, sealed.prev), (Some(bh), Some(fh)));
        } else {
            assert_eq!((sealed.stable, sealed.prev), (Some(bh), Some(after)));
        }
        assert_eq!(plan_ref(&r, "w"), Some(sh));
        r.fsck().unwrap();
    }
}

/// p-1FHVQ: a landing with no item that re-affirms nothing writes no seal (its propose-time
/// plan stays readable, and the view's next answer starts a chain); a re-affirmation-only
/// landing seals, naming the decision, also with no item left.
#[test]
fn cp10_empty_landing_has_no_seal_but_reaffirm_only_does() {
    let (_d, mut r) = project();
    let s = spec(&mut r);
    claim(&mut r, "z", &s.s, "Z stands alone.");
    let pz = propose(&mut r, "z", &[]);
    let (zb, _) = bound(&r, &h(&pz.id)).unwrap();
    assert_eq!((pz.plan.open, pz.plan.closed, pz.plan.deferred), (0, 0, 0));
    let before = plans_in(&r);
    let zl = land(&mut r, &pz);
    assert!(seal(&r, &zl).is_none(), "no seal");
    assert_eq!(plans_in(&r), before, "no plan of any kind");
    assert_eq!(plan_ref(&r, "z"), Some(zb));
    let still = read_plan(&r, proposal_q(&pz.r#ref));
    assert_eq!(
        still["plan"],
        zb.to_string(),
        "the propose-time plan stays readable"
    );
    let empty = read_plan(&r, landing_q(&zl));
    assert_eq!(empty["plan"], Value::Null);
    // the chain ended with the landing: the view's next answer starts one
    edit(&mut r, "z", &s.a, "A holds 60 days.");
    let next = r
        .answer_plan(
            &writer(),
            "z",
            &[ans(
                &pi(&s.a, &s.b, PlanKind::Confirm),
                PlanAct::Confirm,
                Some("holds"),
            )],
        )
        .unwrap();
    let np = r.read().reader().plan(&next).unwrap();
    assert_eq!((np.stable, np.prev), (None, None));

    // a decision on a, suspect in canon once a changes there
    let sdep = decision(&mut r, "dc", &s.s, "Build on C", &[&s.c]);
    edit(&mut r, "x", &s.c, "C stands firm.");
    let px = propose(&mut r, "x", &[]);
    land(&mut r, &px);
    write(
        &mut r,
        "y",
        json!([{"op": "reaffirm", "id": sdep, "reason": "It still builds on C."}]),
    )
    .unwrap();
    let py = propose(&mut r, "y", &[]);
    let yl = land(&mut r, &py);
    let (_, sealed) = seal(&r, &yl).expect("a re-affirmation seals");
    let x = sealed.seal.as_ref().unwrap();
    assert_eq!(x.decisions, vec![id(&sdep)]);
    assert!(
        x.items.iter().all(|i| i.outcome != PlanOutcome::Open),
        "no open item: {:?}",
        x.items
    );
    r.fsck().unwrap();
}

/// A failed append at any frame of a propose batch or a landing batch leaves no half: every ref
/// and the log end as they were, no proposal without its plan and no landing without its seal,
/// and fsck agrees.
#[test]
fn cp10_append_failure_leaves_no_proposal_or_seal_half() {
    /// A project with view `w` ready to propose (a, and a proposed decision for an acceptance).
    fn ready() -> (tempfile::TempDir, Rdy) {
        let (d, mut r) = project();
        let s = spec(&mut r);
        edit(&mut r, "w", &s.a, "A holds 60 days.");
        r.decide(
            &writer(),
            "w",
            DecisionInput {
                parent: s.s.clone(),
                statement: "Hold sixty".into(),
                ..Default::default()
            },
        )
        .unwrap();
        (d, r)
    }
    /// The data frames one act writes: its batch less the marker and the seal's TRANSITION.
    fn data_frames(r: &Rdy, start: Location) -> usize {
        frames_from(r, start)
            .iter()
            .filter(|k| !matches!(**k, kinds::BATCH_COMMIT | kinds::TRANSITION))
            .count()
    }
    // the propose batch
    let (_d0, mut r0) = ready();
    let start = r0.store().log_end();
    let p0 = propose(&mut r0, "w", &[]);
    let n = data_frames(&r0, start);
    assert!(n >= 4, "a version, its refs and its plan: {n}");
    for k in 0..n {
        let (_d, mut r) = ready();
        let before = log_state(&r);
        let plans = plans_in(&r);
        r.store_mut().fail_append_at_frame_for_tests(Some((k, 3)));
        let e = r
            .propose_plan(
                &writer(),
                ProposePlanRequest {
                    summary: Some(TEST_SUMMARY),
                    ..ProposePlanRequest::new("w", "w")
                },
            )
            .unwrap_err();
        r.store_mut().fail_append_at_frame_for_tests(None);
        assert_eq!(log_state(&r), before, "frame {k}: {e}");
        assert_eq!(plans_in(&r), plans, "frame {k}");
        r.fsck().unwrap();
        // the act still succeeds afterwards, whole
        let p = propose(&mut r, "w", &[]);
        assert!(bound(&r, &h(&p.id)).is_some(), "frame {k}");
    }
    // the landing batch: the acceptance version, the landing, its bound plan and the seal
    confirm_exceptions(&mut r0, &approver(), &p0.r#ref);
    let start = r0.store().log_end();
    let l0 = land_now(&mut r0, &p0.id);
    assert!(seal(&r0, &l0).is_some());
    let n = data_frames(&r0, start);
    for k in 0..n {
        let (_d, mut r) = ready();
        let p = propose(&mut r, "w", &[]);
        confirm_exceptions(&mut r, &approver(), &p.r#ref);
        let before = log_state(&r);
        let plans = plans_in(&r);
        r.store_mut().fail_append_at_frame_for_tests(Some((k, 3)));
        let e = r.land(&approver(), &p.id).unwrap_err();
        r.store_mut().fail_append_at_frame_for_tests(None);
        assert_eq!(log_state(&r), before, "frame {k}: {e}");
        assert_eq!(plans_in(&r), plans, "frame {k}");
        let pid = h(&p.id);
        assert!(
            r.read()
                .landings()
                .unwrap()
                .iter()
                .all(|(_, l)| l.approved_version != Some(pid)),
            "frame {k}: no landing"
        );
        r.fsck().unwrap();
        let l = h(&r.land(&approver(), &p.id).unwrap());
        assert!(seal(&r, &l).is_some(), "frame {k}");
    }
}

/// A seal reads as recorded: later canon and later code change neither its bytes nor its read.
#[test]
fn cp10_sealed_reads_ignore_later_canon_and_code() {
    let (d, mut r) = project();
    let repo = linked_repo(d.path());
    let s = spec(&mut r);
    commit(
        repo.path(),
        "f.rs",
        &format!("start\n\nImplements: {}", s.a),
    );
    edit(&mut r, "w", &s.a, "A holds 60 days.");
    let p = propose(&mut r, "w", &[]);
    let l = land(&mut r, &p);
    let (sh, _) = seal(&r, &l).unwrap();
    let raw = r.read().reader().get_raw(&sh).unwrap();
    let read = read_plan(&r, landing_q(&l));
    // canon moves on a again, and the checkout closes the code item
    edit(&mut r, "v", &s.a, "A holds 90 days.");
    let pv = propose(&mut r, "v", &[]);
    land(&mut r, &pv);
    commit(
        repo.path(),
        "f.rs",
        &format!("follow\n\nImplements: {}", s.a),
    );
    assert_eq!(r.read().reader().get_raw(&sh).unwrap(), raw);
    assert_eq!(
        read_plan(&r, landing_q(&l)),
        read,
        "the seal reads as recorded"
    );
    let code = pi(&s.a, "app:f.rs", PlanKind::UpdateImplementation);
    let row = read["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == code)
        .unwrap()
        .clone();
    assert_eq!(row["outcome"], "open");
}

/// O16: the prepared plan is part of the prepared approval: a writer that moves while the agent
/// signs refuses the landing with no partial frame (no landing, no plan, no signature), and an
/// undisturbed retry writes the landing, its seal and its signature in one batch.
#[test]
fn cp10_signing_freshness_covers_the_plan() {
    struct Moving {
        inner: QaAgent,
        hook: Mutex<Box<dyn FnMut() + Send>>,
    }
    impl SigningAgent for Moving {
        fn identities(&self) -> Result<Vec<AgentIdentity>, RdyError> {
            self.inner.identities()
        }
        fn sign(&self, public_key: &[u8], payload: &[u8]) -> Result<Vec<u8>, RdyError> {
            let mut hook = self.hook.lock().unwrap();
            (hook.as_mut())();
            self.inner.sign(public_key, payload)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let s = spec(&mut r);
    edit(&mut r, "w", &s.a, "A holds 60 days.");
    let p = propose(&mut r, "w", &[]);
    let (bh, _) = bound(&r, &h(&p.id)).unwrap();
    std::fs::write(
        dir.path().join(".rdy/local.yaml"),
        format!(
            "signing:\n  key: \"{}\"\n  agent_key: null\n  sign: always\n  allowed_signers: null\n",
            QaAgent::fingerprint(TestKey::Person)
        ),
    )
    .unwrap();
    let e = Arc::new(Local::new(r));
    let on = |f: Box<dyn FnOnce(&mut Rdy) + Send>| {
        with_writer_result(&*e, move |r| {
            f(r);
            Ok(())
        })
        .unwrap()
    };
    let plans_before = Arc::new(Mutex::new(0usize));
    {
        let pb = Arc::clone(&plans_before);
        on(Box::new(move |r| *pb.lock().unwrap() = plans_in(r)));
    }
    let (e2, section) = (Arc::clone(&e), s.s.clone());
    let n = Arc::new(Mutex::new(0usize));
    let agent = Moving {
        inner: QaAgent::new(),
        hook: Mutex::new(Box::new(move || {
            let mut k = n.lock().unwrap();
            *k += 1;
            let (section, view) = (section.clone(), format!("mover{k}"));
            with_writer_result(&*e2, move |r| {
                claim(r, &view, &section, "moves the log");
                Ok(())
            })
            .unwrap();
        })),
    };
    let err = run_signed_act(
        &*e,
        &agent,
        &approver(),
        PERSON,
        SigningAct::Approve {
            proposal: p.id.clone(),
            options: LandOptions::default(),
        },
    )
    .unwrap_err();
    assert_eq!(err.code, E_STALE_BASE, "{err}");
    assert_eq!(
        err.message,
        "the project changed during all 3 signing attempts"
    );
    let (pid, before) = (h(&p.id), *plans_before.lock().unwrap());
    on(Box::new(move |r| {
        assert!(
            r.read()
                .landings()
                .unwrap()
                .iter()
                .all(|(_, l)| l.approved_version != Some(pid)),
            "no landing"
        );
        assert_eq!(plans_in(r), before, "no plan");
        assert_eq!(plan_ref(r, "w"), Some(bh), "the view's plan stands");
        r.fsck().unwrap();
    }));
    // undisturbed: the landing, its seal and its signature in one batch
    let still = Moving {
        inner: QaAgent::new(),
        hook: Mutex::new(Box::new(|| {})),
    };
    let v = run_signed_act(
        &*e,
        &still,
        &approver(),
        PERSON,
        SigningAct::Approve {
            proposal: p.id.clone(),
            options: LandOptions::default(),
        },
    )
    .unwrap();
    let l = h(v["landing"].as_str().unwrap());
    on(Box::new(move |r| {
        assert!(seal(r, &l).is_some());
        let frames: Vec<u8> = r.store().last_batch().iter().map(|(k, _)| *k).collect();
        for k in [kinds::LANDING, kinds::PLAN, kinds::SIGNATURE] {
            assert!(frames.contains(&k), "{frames:?}");
        }
        let at = |k: u8| frames.iter().rposition(|x| *x == k).unwrap();
        assert!(
            at(kinds::PLAN) < at(kinds::SIGNATURE),
            "the seal before the signature"
        );
        r.fsck().unwrap();
    }));
}
