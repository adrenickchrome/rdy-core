//! ck7 (D-JQD5X; O13, O14, O16; D-ME2HD, D-HAG73, D-QGEE3; p-8WADR): verdict facts, card
//! freshness and version-bound acceptance. The stored assessment keeps its propose-time card; the
//! card shown is that card plus one line for each Check review whose request's case matches the
//! version now, and an approval binds the hash of the card shown. A person accepts a `concern` or
//! `fail` on the card as blocking for that version alone: an `accept_verdict` ANSWER, and the
//! fifth later refusal for every lander while the case holds or cannot be read.
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::answer::{AnswerInfo, AnswerRequest};
use rdy_engine::check::api::{ReviewRequest, run_review};
use rdy_engine::check::verdicts::{
    ACCEPT_CONSEQUENCE, MatchingVerdict, accepted_verdict_issues, matching_verdicts,
};
use rdy_engine::engine::{Local, with_writer_result};
use rdy_engine::pending::{
    CARD_MAX_TOKENS, Item, ItemKind, Pending, PendingQuery, card_hash, render,
};
use rdy_engine::readiness::{Candidate, readiness};
use rdy_engine::semantic::api::run_review_submit;
use rdy_engine::semantic::submit::{CitationRequest, ReviewSubmitRequest};
use rdy_engine::tensions::api::AcceptRequest;
use rdy_engine::{
    E_BAD_INPUT, E_BLOCKING_TENSION, E_LIMIT, E_PERMISSION, E_PROPOSAL_MOVED, E_PROPOSAL_NOT_READY,
    E_VALIDATION, Engine, LandOptions, ProposalInfo, ProposalRef, Rdy, RdyError, Relay, Session,
    WriteRequest, WriteResult,
};
use rdy_mcp::tools::{McpState, rdy_answer, rdy_session};
use rdy_model::assessment::{Assessment, PolicyVerdict};
use rdy_model::{
    ActorRun, Answer, AnswerAct, ApprovalEvidence, Assurance, BlockId, Citation, EvidenceKind,
    Hash, Justification, Proposal, Provenance, RelayEvidence, Review, ReviewSubject, Role, Verdict,
};
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::{approver, land, parse_test_ops, project, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY, batch_project};
use rdy_qa::packs::QA_PACK_YAML;
use rdy_qa::tensions::{ag, cite, created, j_implements, j_typo, put, put_j, term, upd};
use rdy_schema::{Issue, Severity};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

/// The reason of every concern and fail these tests write.
pub(crate) const REASON: &str = "it reads otherwise";
/// The citation note of every concern and fail these tests write.
const NOTE: &str = "the meaning moved";
/// The fix of an accepted verdict's issue.
const HELD_FIX: &str = "change the cited blocks, then propose again: rdy_write / rdy edit";

// ---------------------------------------------------------------- engine helpers

/// Runs `f` on the writer and returns its result.
pub(crate) fn on<T: Send + 'static>(
    e: &dyn Engine,
    f: impl FnOnce(&mut Rdy) -> Result<T, RdyError> + Send + 'static,
) -> Result<T, RdyError> {
    with_writer_result(e, f)
}

fn review_op(e: &dyn Engine, s: &Session, v: Value) -> Value {
    run_review(
        e,
        s,
        PERSON,
        true,
        None,
        ReviewRequest::from_value(v.clone()).unwrap(),
    )
    .unwrap_or_else(|err| panic!("{v}: {err:?}"))
}

fn reviewer() -> Session {
    Session::agent("agent:r".into(), Some("r".into()), Role::Reviewer).unwrap()
}

/// A request of `check` on `subject` by `author`: author-run with `author_run`, else
/// independent. Its label and case hash.
fn ask(
    e: &dyn Engine,
    author: &Session,
    check: &str,
    subject: Value,
    author_run: bool,
) -> (String, String) {
    let op = if author_run { "run" } else { "request" };
    let q = review_op(
        e,
        author,
        json!({"op": op, "check": check, "subject": subject}),
    );
    (
        q["request"].as_str().expect("a request").to_string(),
        q["case_hash"].as_str().unwrap().to_string(),
    )
}

/// `s`'s verdict `verdict` on request `rq`: a concern or a fail cites `cite` with [`NOTE`] and
/// gives [`REASON`]; a pass cites nothing and gives no reason. The review's hash.
fn answer_request(
    e: &dyn Engine,
    s: &Session,
    (rq, case): &(String, String),
    verdict: &str,
    cite: &BlockId,
) -> String {
    let pass = verdict == "pass";
    let req = ReviewSubmitRequest {
        request: rq.clone(),
        case_hash: case.clone(),
        verdict: verdict.into(),
        citations: if pass {
            vec![]
        } else {
            vec![CitationRequest {
                block: cite.to_string(),
                note: NOTE.into(),
            }]
        },
        severity: None,
        reason: (!pass).then(|| REASON.to_string()),
    };
    let v = run_review_submit(e, s, req).unwrap_or_else(|err| panic!("submit {rq}: {err:?}"));
    v["review"].as_str().unwrap().to_string()
}

/// An independent `meaning_preservation` request on version `pref`, asked by `author` and
/// answered `verdict` by agent:r (citing `cite` for a concern or a fail): the review's hash.
pub(crate) fn meaning_verdict(
    e: &dyn Engine,
    author: &Session,
    pref: &str,
    verdict: &str,
    cite: &BlockId,
) -> String {
    let q = ask(e, author, "meaning_preservation", json!(pref), false);
    answer_request(e, &reviewer(), &q, verdict, cite)
}

fn queue(e: &dyn Engine, max_tokens: usize) -> Result<Pending, RdyError> {
    e.read().pending(&PendingQuery {
        person: PERSON.into(),
        session_ref: None,
        max_tokens,
    })
}

fn approve_card(e: &dyn Engine, pref: &str) -> Item {
    let q = queue(e, CARD_MAX_TOKENS).unwrap();
    let not_ready = q.not_ready.clone();
    q.items
        .into_iter()
        .find(|i| i.r#ref == pref && i.kind == ItemKind::Approve)
        .unwrap_or_else(|| panic!("an approve item for {pref}: {not_ready:#?}"))
}

fn has_card(e: &dyn Engine, pref: &str) -> bool {
    queue(e, CARD_MAX_TOKENS)
        .unwrap()
        .items
        .iter()
        .any(|i| i.r#ref == pref && i.kind == ItemKind::Approve)
}

fn not_ready(e: &dyn Engine, pref: &str) -> Option<Vec<Issue>> {
    queue(e, CARD_MAX_TOKENS)
        .unwrap()
        .not_ready
        .into_iter()
        .find(|n| n.r#ref == pref)
        .map(|n| n.issues)
}

fn lines(i: &Item) -> Vec<String> {
    i.facts
        .iter()
        .map(|f| format!("{}: {}", f.name, f.value))
        .collect()
}

fn ids(i: &Item) -> Vec<String> {
    i.options.iter().map(|o| o.id.clone()).collect()
}

/// A proposal version as these tests name it: its `P-x@n` and its full hash.
#[derive(Clone, Debug)]
struct Ver {
    r#ref: String,
    id: String,
}

impl From<&ProposalInfo> for Ver {
    fn from(p: &ProposalInfo) -> Ver {
        Ver {
            r#ref: p.r#ref.clone(),
            id: p.id.clone(),
        }
    }
}

/// The version `v` names and its record.
fn version(e: &dyn Engine, v: &Ver) -> (ProposalRef, Proposal) {
    let st = e.read();
    let version: Hash = v.id.parse().unwrap();
    let rec = st.reader().proposal(&version).unwrap();
    let n = v.r#ref.rsplit_once('@').unwrap().1.parse().unwrap();
    let pref = ProposalRef {
        version,
        stable: rec.stable.unwrap_or(version),
        n,
    };
    assert_eq!(pref.to_string(), v.r#ref);
    (pref, rec)
}

fn held_of(e: &dyn Engine, v: impl Into<Ver>) -> Vec<Issue> {
    let (pref, rec) = version(e, &v.into());
    accepted_verdict_issues(&e.read(), &pref, &rec).unwrap()
}

fn shown_of(e: &dyn Engine, v: impl Into<Ver>) -> Vec<MatchingVerdict> {
    let (pref, rec) = version(e, &v.into());
    matching_verdicts(&e.read(), &pref, &rec).unwrap()
}

/// The issue of review `review` of `check`, accepted as blocking for `pref`.
fn held(check: &str, review: &str, pref: &str) -> Issue {
    Issue {
        severity: Severity::Error,
        code: E_VALIDATION,
        block: None,
        message: format!("{check} verdict {review} is accepted as blocking for {pref}"),
        fix: HELD_FIX.into(),
    }
}

/// The id of the acceptance option of `review` on version `v`.
fn accept_id(review: &str, v: impl Into<Ver>) -> String {
    format!("accept_verdict:{review}:{}", v.into().id)
}

/// The stored assessment of `p` and its hash.
fn stored(e: &dyn Engine, p: &ProposalInfo) -> (Hash, Assessment) {
    let st = e.read();
    let v: Hash = p.id.parse().unwrap();
    let h = st
        .reader()
        .assessment_of_proposal(&v, st.seq())
        .unwrap()
        .unwrap_or_else(|| panic!("{} stores an assessment", p.r#ref));
    (h, st.reader().assessment(&h).unwrap())
}

fn relay_agent() -> Session {
    Session::agent("agent:relay".into(), Some("r1".into()), Role::Writer).unwrap()
}

fn relay_input(words: &str) -> RelayInput {
    RelayInput {
        relayed_by: None,
        evidence: Some(words.into()),
        question: Some("Shall I?".into()),
        session_ref: Some("chat-7".into()),
        kind: None,
    }
}

/// `person`'s answer `option` to item `r`, relayed by agent:relay with the card hash shown.
fn relayed(
    e: &dyn Engine,
    person: &str,
    r: &str,
    option: &str,
    card_hash: &str,
) -> Result<AnswerInfo, RdyError> {
    let (person, req) = (
        person.to_string(),
        AnswerRequest {
            r#ref: r.into(),
            option: option.into(),
            card_hash: Some(card_hash.into()),
            note: None,
            relay: relay_input("Yes, do it."),
            brief: None,
        },
    );
    on(e, move |rdy| rdy.answer(&relay_agent(), &person, req))
}

/// The person's own answer `option` to item `r`, with no card hash.
fn local(e: &dyn Engine, r: &str, option: &str) -> Result<AnswerInfo, RdyError> {
    let req = AnswerRequest {
        r#ref: r.into(),
        option: option.into(),
        ..Default::default()
    };
    on(e, move |rdy| rdy.answer(&approver(), PERSON, req))
}

fn not_ready_issues(r: Result<impl std::fmt::Debug, RdyError>) -> Vec<Issue> {
    let e = r.expect_err("not ready");
    assert_eq!(e.code, E_PROPOSAL_NOT_READY, "{e:#?}");
    e.issues
}

fn propose_in(e: &dyn Engine, view: &str) -> ProposalInfo {
    let view = view.to_string();
    let p = on(e, move |r| {
        r.propose(&ag(&view), &view, &view, Some(TEST_SUMMARY))
    })
    .unwrap();
    assert!(p.ready, "{:#?}", p.issues);
    p
}

fn write_in(e: &dyn Engine, view: &str, ops: Value, j: Option<Justification>) -> WriteResult {
    let view = view.to_string();
    on(e, move |r| Ok(put_j(r, &view, ops, j))).unwrap()
}

// ---------------------------------------------------------------- git helpers

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

fn point_repo(project: &Path, checkout: &Path) {
    std::fs::create_dir_all(project.join(".rdy")).unwrap();
    std::fs::write(
        project.join(".rdy/local.yaml"),
        format!("code_paths:\n  app: {}\n", checkout.display()),
    )
    .unwrap();
}

fn link_repo(project: &Path, checkout: &Path) {
    let mut yaml = std::fs::read_to_string(project.join(".rdy.yaml")).unwrap();
    yaml.push_str("code_repos: [app]\n");
    std::fs::write(project.join(".rdy.yaml"), yaml).unwrap();
    point_repo(project, checkout);
}

/// A git checkout with two commits of `cache.rs` that implement `d`: (dir guard, path, from, to).
fn checkout_for(d: &BlockId) -> (tempfile::TempDir, PathBuf, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let repo = std::fs::canonicalize(dir.path()).unwrap();
    git(&repo, &["init", "-q", "--initial-branch=main"]);
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
    (dir, repo, from, to)
}

/// The path of git object `hash` in `repo`.
fn object_path(repo: &Path, hash: &str) -> PathBuf {
    repo.join(".git/objects").join(&hash[..2]).join(&hash[2..])
}

// ---------------------------------------------------------------- worlds

/// A standard project: a draft doc `notes` with a section `s` and claims `x`, `z` and `w`.
struct Notes {
    dir: tempfile::TempDir,
    e: Local,
    x: BlockId,
    z: BlockId,
    w: BlockId,
}

fn notes() -> Notes {
    let (dir, mut r) = project();
    let made = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "X holds."},
            {"op": "create", "type": "claim", "id": "$z", "parent": "$s", "body": "Z holds."},
            {"op": "create", "type": "claim", "id": "$w", "parent": "$s", "body": "W holds."}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    Notes {
        x: created(&made, "$x"),
        z: created(&made, "$z"),
        w: created(&made, "$w"),
        dir,
        e: Local::new(r),
    }
}

/// `notes`, then agent:a1's typo edit of `x` in view `a1`, proposed: the version.
fn notes_version() -> (Notes, ProposalInfo) {
    let n = notes();
    write_in(&n.e, "a1", upd(&n.x, "X holds, now."), j_typo());
    let p = propose_in(&n.e, "a1");
    (n, p)
}

/// The policy of two rules (as ap11's): agents `agent:a*` may land draft edits with
/// `implements` or `editorial` changes and no decision (`drafts`), and `two_way` decisions in
/// area `payments` with basis `precedent` (`decides`). A version that writes a decision
/// otherwise needs a person from `approvers`.
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

/// An `rdy-qa` project under the policy: a draft doc `notes` (section `s`, claims `x`, `z`), a
/// draft doc `wallets` in area `payments` (section `ws`), and the policy, landed.
struct Policed {
    dir: tempfile::TempDir,
    e: Local,
    s: BlockId,
    x: BlockId,
    z: BlockId,
    ws: BlockId,
}

fn write_as(
    r: &mut Rdy,
    s: &Session,
    view: &str,
    ops: Value,
    j: Option<Justification>,
) -> WriteResult {
    r.write(
        s,
        WriteRequest {
            view: view.into(),
            ops: parse_test_ops(ops).unwrap(),
            justification: j,
            message: String::new(),
        },
    )
    .unwrap_or_else(|e| panic!("write in {view}: {e}"))
}

fn policed() -> Policed {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").expect("init"));
    std::fs::write(dir.path().join(".rdy/packs/rdy-qa.yaml"), QA_PACK_YAML).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: rdy-qa\n").unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let w = write_as(
        &mut r,
        &writer(),
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "X holds."},
            {"op": "create", "type": "claim", "id": "$z", "parent": "$s", "body": "Z holds."},
            {"op": "create", "type": "doc", "id": "$w", "fields": {"name": "wallets", "title": "Wallets", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$ws", "parent": "$w", "fields": {"title": "Fees"}}
        ]),
        None,
    );
    land(&mut r, "seed").unwrap();
    let rules = write_as(
        &mut r,
        &writer(),
        "pol",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "overview"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$d", "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}}
        ]),
        None,
    );
    let d = created(&rules, "$dec");
    write_as(
        &mut r,
        &writer(),
        "pol",
        json!([{"op": "create", "type": "authority_policy", "id": "$p", "parent": rules.created["$d"], "fields": policy_fields()}]),
        Some(Justification::Implements(vec![d])),
    );
    land(&mut r, "pol").unwrap();
    Policed {
        s: created(&w, "$s"),
        x: created(&w, "$x"),
        z: created(&w, "$z"),
        ws: created(&w, "$ws"),
        dir,
        e: Local::new(r),
    }
}

/// Agent `agent:<view>`'s typo edit of `id` in view `view`.
fn typo(e: &dyn Engine, view: &str, id: &BlockId, body: &str) {
    write_in(e, view, upd(id, body), j_typo());
}

/// The person approves `p` on its card, as it stands now.
fn approve_now(e: &dyn Engine, p: &ProposalInfo) -> AnswerInfo {
    let card = approve_card(e, &p.r#ref);
    relayed(e, PERSON, &p.r#ref, "approve", &card.card_hash)
        .unwrap_or_else(|err| panic!("approve {}: {err:#?}", p.r#ref))
}

// ---------------------------------------------------------------- tests

/// O13: all seven checks other than contradiction, with both modes, show after every stored
/// fact (the last of which is Policy), in catalogue order, as `<check name>: <verdict>
/// (<actor>, <mode>)[: <reason>]`. The card's metadata holds every field, each concern and fail
/// gets an acceptance option before Reject and Later, and the stored assessment is the one
/// propose wrote.
#[test]
fn ck7_shown_card_appends_verdicts_after_policy() {
    let (dir, mut r) = project();
    let s = spec_doc(&mut r);
    // canon: prose that the decision D governs, which prose drift reads
    put_j(
        &mut r,
        "prose",
        json!([{"op": "create", "type": "prose", "id": "$p", "parent": s.section.to_string(),
                "body": "Readers keep A in mind.",
                "links": {"decided_by": [{"target": s.decision.to_string()}]}}]),
        j_typo(),
    );
    land(&mut r, "prose").unwrap();
    // view a: an editorial edit of C, a claim that implements D, a proposed decision D2 that
    // refines D and has confirmation text, a term and a claim that names it
    put_j(&mut r, "a", upd(&s.c, "C stands alone!"), j_typo());
    let x = created(
        &put_j(
            &mut r,
            "a",
            json!([{"op": "create", "type": "claim", "id": "$x", "parent": s.section.to_string(),
                    "body": "X follows the decision."}]),
            j_implements(&s.decision),
        ),
        "$x",
    );
    let d2 = rdy_qa::tensions::try_decide_in(&mut r, "a", &s.section, "Cache the reads").unwrap();
    put_j(
        &mut r,
        "a",
        json!([{"op": "update", "id": d2.to_string(),
                "fields": {"confirmation": "A test reads the cache."},
                "links": {"refines": [{"target": s.decision.to_string()}]}}]),
        None,
    );
    let t = created(&put(&mut r, "a", term(&s.section, "Cache")), "$t");
    put(&mut r, "a", cite(&s.section, &t));
    let (_checkout, repo, from, to) = checkout_for(&d2);
    link_repo(dir.path(), &repo);
    let p = r.propose(&ag("a"), "a", "a", Some(TEST_SUMMARY)).unwrap();
    assert!(p.ready, "{:#?}", p.issues);
    let e = Local::new(r);
    let (before_hash, before) = stored(&e, &p);
    let none = approve_card(&e, &p.r#ref);
    assert_eq!(
        lines(&none),
        before.card.facts,
        "no verdict: the stored facts"
    );
    assert!(none.check_verdicts.is_empty());
    assert!(
        serde_json::to_value(&none)
            .unwrap()
            .get("check_verdicts")
            .is_none(),
        "no metadata key with no verdict"
    );
    assert!(
        before.card.facts.last().unwrap().starts_with("Policy: "),
        "{:#?}",
        before.card.facts
    );
    let a = ag("a");
    let range = format!("app@{}..{}", &from[..12], &to[..12]);
    // (check, subject, author-run, verdict, cited block)
    let rows: Vec<(&str, Value, bool, &str, BlockId)> = vec![
        (
            "meaning_preservation",
            json!(p.r#ref),
            true,
            "pass",
            s.c.clone(),
        ),
        (
            "implementation_fidelity",
            json!(p.r#ref),
            false,
            "concern",
            x.clone(),
        ),
        (
            "decision_review",
            json!(d2.to_string()),
            true,
            "fail",
            d2.clone(),
        ),
        (
            "entity_divergence",
            json!(t.to_string()),
            false,
            "pass",
            t.clone(),
        ),
        ("prose_drift", json!(p.r#ref), false, "fail", d2.clone()),
        (
            "diff_contradicts",
            json!({"decision": d2.to_string(), "code": range}),
            true,
            "concern",
            d2.clone(),
        ),
        (
            "confirmation",
            json!(d2.to_string()),
            false,
            "fail",
            d2.clone(),
        ),
    ];
    let mut reviews = vec![];
    for (check, subject, author_run, verdict, cited) in &rows {
        let q = ask(&e, &a, check, subject.clone(), *author_run);
        let by = if *author_run { a.clone() } else { reviewer() };
        reviews.push((q.0.clone(), answer_request(&e, &by, &q, verdict, cited)));
    }
    let card = approve_card(&e, &p.r#ref);
    let n = before.card.facts.len();
    assert_eq!(
        lines(&card)[..n],
        before.card.facts[..],
        "the stored prefix"
    );
    assert_eq!(
        lines(&card)[n..],
        [
            "meaning preservation: pass (agent:a, author-run)".to_string(),
            format!("implementation fidelity: concern (agent:r, independent): {REASON}"),
            format!("decision review: fail (agent:a, author-run): {REASON}"),
            "entity divergence: pass (agent:r, independent)".to_string(),
            format!("prose drift: fail (agent:r, independent): {REASON}"),
            format!("diff contradicts: concern (agent:a, author-run): {REASON}"),
            format!("confirmation: fail (agent:r, independent): {REASON}"),
        ]
    );
    // the metadata: every field, in the facts' order
    assert_eq!(card.check_verdicts.len(), 7);
    for (m, ((check, _, author_run, verdict, cited), (rq, review))) in
        card.check_verdicts.iter().zip(rows.iter().zip(&reviews))
    {
        assert_eq!(m.check, *check);
        assert_eq!(format!("rq-{}", m.request), *rq);
        assert_eq!(m.review.to_string(), *review);
        assert_eq!(
            m.mode,
            if *author_run {
                "author-run"
            } else {
                "independent"
            }
        );
        let (actor, run) = if *author_run {
            ("agent:a", "a")
        } else {
            ("agent:r", "r")
        };
        assert_eq!((m.actor.as_str(), m.run.as_deref()), (actor, Some(run)));
        assert_eq!(m.verdict, *verdict);
        let request = e.read().reader().check_request(&m.request).unwrap();
        assert_eq!(m.case_hash, request.case_hash);
        if *verdict == "pass" {
            assert!(m.reason.is_empty() && m.citations.is_empty(), "{m:#?}");
        } else {
            assert_eq!(m.reason, REASON);
            assert_eq!(m.citations.len(), 1);
            assert_eq!(
                (m.citations[0].block.as_str(), m.citations[0].note.as_str()),
                (cited.as_str(), NOTE)
            );
        }
    }
    let row = &serde_json::to_value(&card).unwrap()["check_verdicts"][0];
    assert_eq!(
        row.as_object().unwrap().keys().cloned().collect::<Vec<_>>(),
        [
            "actor",
            "case_hash",
            "check",
            "citations",
            "mode",
            "reason",
            "request",
            "review",
            "run",
            "verdict"
        ]
    );
    // one acceptance option for each concern and fail, before Reject and Later
    let accepted: Vec<&MatchingVerdict> = card
        .check_verdicts
        .iter()
        .filter(|m| m.verdict != "pass")
        .collect();
    let mut want = vec!["approve".to_string()];
    want.extend(
        accepted
            .iter()
            .map(|m| accept_id(&m.review.to_string(), &p)),
    );
    want.extend(["reject".to_string(), "later".to_string()]);
    assert_eq!(ids(&card), want);
    let labels: Vec<&str> = card.options[1..6]
        .iter()
        .map(|o| o.label.as_str())
        .collect();
    assert_eq!(
        labels,
        [
            "Accept implementation fidelity concern as blocking",
            "Accept decision review fail as blocking",
            "Accept prose drift fail as blocking",
            "Accept diff contradicts concern as blocking",
            "Accept confirmation fail as blocking"
        ]
    );
    for o in &card.options[1..6] {
        assert_eq!(o.consequences, [ACCEPT_CONSEQUENCE.to_string()]);
        assert!(o.recommended.is_none(), "{o:#?}");
    }
    assert_eq!(
        card.options
            .iter()
            .filter(|o| o.recommended.is_some())
            .count(),
        none.options
            .iter()
            .filter(|o| o.recommended.is_some())
            .count(),
        "no new option is recommended"
    );
    // the stored assessment did not change
    let (after_hash, after) = stored(&e, &p);
    assert_eq!(after_hash, before_hash);
    assert_eq!(after, before);
    assert_ne!(card.card_hash, none.card_hash);
}

/// O13: only a case that matches the version's selected changes shows: a change outside a
/// subset, another view's version, a case whose omitted context changed and code evidence that
/// cannot be read show nothing; a new version with the same case shows the verdict again.
#[test]
fn ck7_only_matching_selected_cases_show() {
    let n = notes();
    let a1 = ag("a1");
    let c1 = write_in(&n.e, "a1", upd(&n.x, "X holds, now."), j_typo())
        .change
        .clone();
    let c2 = write_in(&n.e, "a1", upd(&n.z, "Z holds, now."), j_typo())
        .change
        .clone();
    // the subset: c1 only
    let subset = {
        let c1 = c1.clone();
        on(&n.e, move |r| {
            r.propose_subset(&ag("a1"), "a1", "a1", Some(&[c1]), Some(TEST_SUMMARY))
        })
        .unwrap()
    };
    assert!(subset.ready, "{:#?}", subset.issues);
    let on_c1 = {
        let q = ask(&n.e, &a1, "meaning_preservation", json!(c1), false);
        answer_request(&n.e, &reviewer(), &q, "fail", &n.x)
    };
    let on_c2 = {
        let q = ask(&n.e, &a1, "meaning_preservation", json!(c2), false);
        answer_request(&n.e, &reviewer(), &q, "fail", &n.z)
    };
    let on_version = meaning_verdict(&n.e, &a1, &subset.r#ref, "fail", &n.x);
    let reviews = |i: &Item| -> Vec<String> {
        let mut v: Vec<String> = i
            .check_verdicts
            .iter()
            .map(|m| m.review.to_string())
            .collect();
        v.sort();
        v
    };
    let mut want = vec![on_c1.clone(), on_version.clone()];
    want.sort();
    assert!(!want.contains(&on_c2));
    assert_eq!(
        reviews(&approve_card(&n.e, &subset.r#ref)),
        want,
        "the change outside the subset shows nothing"
    );
    // another view's version that edits the same block
    write_in(&n.e, "a2", upd(&n.x, "X holds, again."), j_typo());
    let other = propose_in(&n.e, "a2");
    assert!(
        approve_card(&n.e, &other.r#ref).check_verdicts.is_empty(),
        "another view shows nothing"
    );
    // a new version with the same case: an unjustified edit is no editorial row
    let c3 = write_in(&n.e, "a1", upd(&n.w, "W holds, now."), None)
        .change
        .clone();
    let again = {
        let (c1, c3) = (c1.clone(), c3.clone());
        on(&n.e, move |r| {
            r.propose_subset(&ag("a1"), "a1", "a1", Some(&[c1, c3]), Some(TEST_SUMMARY))
        })
        .unwrap()
    };
    assert_ne!(again.id, subset.id);
    assert_eq!(
        reviews(&approve_card(&n.e, &again.r#ref)),
        want,
        "the same case on a new version shows again"
    );

    // omitted context: a case past its row cap; a new version that changes only an omitted row
    let (_dir, mut r) = project();
    let s = spec_doc(&mut r);
    let creates: Vec<Value> = (0..70)
        .map(|i| {
            json!({"op": "create", "type": "claim", "id": format!("$k{i}"),
                   "parent": s.section.to_string(), "body": format!("K{i} holds.")})
        })
        .collect();
    let made = put_j(&mut r, "seed", json!(creates), j_typo());
    land(&mut r, "seed").unwrap();
    let ks: Vec<BlockId> = (0..70).map(|i| created(&made, &format!("$k{i}"))).collect();
    let edits: Vec<Value> = ks
        .iter()
        .enumerate()
        .map(|(i, k)| json!({"op": "update", "id": k.to_string(), "body": format!("K{i} holds!")}))
        .collect();
    put_j(&mut r, "o", json!(edits), j_typo());
    let e = Local::new(r);
    let first = propose_in(&e, "o");
    let q = review_op(
        &e,
        &ag("o"),
        json!({"op": "request", "check": "meaning_preservation", "subject": first.r#ref}),
    );
    assert!(
        q["case"]["omitted"]["blocks"].as_u64().unwrap() > 0,
        "{}",
        q["case"]["omitted"]
    );
    let review = answer_request(
        &e,
        &reviewer(),
        &(
            q["request"].as_str().unwrap().to_string(),
            q["case_hash"].as_str().unwrap().to_string(),
        ),
        "fail",
        &ks[0],
    );
    assert_eq!(
        reviews(&approve_card(&e, &first.r#ref)),
        vec![review.clone()]
    );
    typo(&e, "o", &ks[69], "K69 holds!!");
    let second = propose_in(&e, "o");
    assert!(
        approve_card(&e, &second.r#ref).check_verdicts.is_empty(),
        "a changed omitted row changes the case"
    );

    // code evidence that cannot be read shows no line; read again, the line returns
    let (dir, mut r) = project();
    let s = spec_doc(&mut r);
    let d = rdy_qa::tensions::try_decide_in(&mut r, "a1", &s.section, "Cache the reads").unwrap();
    let (_checkout, repo, from, to) = checkout_for(&d);
    link_repo(dir.path(), &repo);
    let e = Local::new(r);
    let range = format!("app@{}..{}", &from[..12], &to[..12]);
    let q = ask(
        &e,
        &a1,
        "diff_contradicts",
        json!({"decision": d.to_string(), "code": range}),
        true,
    );
    let review = answer_request(&e, &a1, &q, "fail", &d);
    let p = propose_in(&e, "a1");
    let card = approve_card(&e, &p.r#ref);
    assert_eq!(reviews(&card), vec![review.clone()]);
    // (a missing checkout also makes the version's change plan unreadable, so the queue lists
    // it as not ready; the verdict rows are read directly)
    let moved = repo.with_extension("moved");
    std::fs::rename(&repo, &moved).unwrap();
    assert!(shown_of(&e, &p).is_empty(), "a missing checkout");
    std::fs::rename(&moved, &repo).unwrap();
    let blob = git(&repo, &["rev-parse", &format!("{to}:cache.rs")]);
    let blob_object = object_path(&repo, &blob);
    let saved = std::fs::read(&blob_object).unwrap();
    std::fs::remove_file(&blob_object).unwrap();
    std::fs::remove_file(repo.join("cache.rs")).unwrap();
    assert!(shown_of(&e, &p).is_empty(), "a failed git read");
    std::fs::write(&blob_object, saved).unwrap();
    git(&repo, &["checkout", "--", "cache.rs"]);
    assert_eq!(approve_card(&e, &p.r#ref).card_hash, card.card_hash);

    // the same block written again: the case reads the version's selected changes, never a
    // view edit of that block nobody proposed or a later overwrite that the subset excludes
    let m = notes();
    let c1 = write_in(&m.e, "a1", upd(&m.x, "X holds, now."), j_typo())
        .change
        .clone();
    let on_c1 = {
        let q = ask(&m.e, &a1, "meaning_preservation", json!(c1), false);
        answer_request(&m.e, &reviewer(), &q, "fail", &m.x)
    };
    let whole = propose_in(&m.e, "a1");
    let on_whole = meaning_verdict(&m.e, &a1, &whole.r#ref, "fail", &m.x);
    let mut both = vec![on_c1.clone(), on_whole.clone()];
    both.sort();
    assert_eq!(reviews(&approve_card(&m.e, &whole.r#ref)), both);
    // an overwrite of x in the view, not proposed
    write_in(&m.e, "a1", upd(&m.x, "X holds, later."), j_typo());
    let mut shown: Vec<String> = shown_of(&m.e, &whole)
        .iter()
        .map(|v| v.review.to_string())
        .collect();
    shown.sort();
    assert_eq!(shown, both, "an unproposed overwrite of the same block");
    // a subset of the reviewed change, chosen after an overwrite it excludes
    let k = notes();
    let c1 = write_in(&k.e, "a1", upd(&k.x, "X holds, now."), j_typo())
        .change
        .clone();
    let on_c1 = {
        let q = ask(&k.e, &a1, "meaning_preservation", json!(c1), false);
        answer_request(&k.e, &reviewer(), &q, "fail", &k.x)
    };
    let c2 = write_in(&k.e, "a1", upd(&k.x, "X holds, later."), j_typo())
        .change
        .clone();
    let only_c1 = {
        let c1 = c1.clone();
        on(&k.e, move |r| {
            r.propose_subset(&ag("a1"), "a1", "a1", Some(&[c1]), Some(TEST_SUMMARY))
        })
        .unwrap()
    };
    assert!(only_c1.ready, "{:#?}", only_c1.issues);
    assert_eq!(
        reviews(&approve_card(&k.e, &only_c1.r#ref)),
        vec![on_c1.clone()],
        "the excluded overwrite of the same block"
    );
    // the overwrite proposed too: the net text is another case
    let with_c2 = {
        let (c1, c2) = (c1.clone(), c2.clone());
        on(&k.e, move |r| {
            r.propose_subset(&ag("a1"), "a1", "a1", Some(&[c1, c2]), Some(TEST_SUMMARY))
        })
        .unwrap()
    };
    assert!(
        approve_card(&k.e, &with_c2.r#ref).check_verdicts.is_empty(),
        "the proposed overwrite changes the case"
    );
    drop((n.dir, m.dir, k.dir));
}

/// O13: a verdict moves the card's hash, so every person's approval bound to the card shown
/// before it is `E_PROPOSAL_MOVED` with the fresh card, which shows the line: an MCP answer, a
/// list of recommended answers and a direct approval with the card as evidence. The fresh card's
/// hash lands.
#[test]
fn ck7_verdict_moves_every_person_approval_card() {
    let (n, p) = notes_version();
    let Notes {
        dir: _dir, e, x, ..
    } = n;
    let e = Arc::new(e);
    let shown = approve_card(&*e, &p.r#ref).card_hash;
    meaning_verdict(&*e, &ag("a1"), &p.r#ref, "fail", &x);
    let fresh = approve_card(&*e, &p.r#ref);
    assert_ne!(fresh.card_hash, shown);
    let line = format!("meaning preservation: fail (agent:r, independent): {REASON}");
    assert_eq!(lines(&fresh).last(), Some(&line));
    let head = e.read().canon_head();
    let shows_line = |err: &RdyError, card: &Value| {
        assert_eq!(err.code, E_PROPOSAL_MOVED, "{err:#?}");
        assert_eq!(card["card_hash"], json!(fresh.card_hash), "{err:#?}");
        assert!(
            card["card"]
                .as_str()
                .unwrap()
                .contains("meaning preservation"),
            "{err:#?}"
        );
    };
    // the MCP answer
    let mut st = McpState::new(e.clone());
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:relay", "run": "r1", "role": "writer"}))
            .unwrap(),
    )
    .unwrap();
    let mcp = |st: &mut McpState, hash: &str| {
        rdy_answer(
            st,
            serde_json::from_value(json!({
                "ref": p.r#ref, "option": "approve", "card_hash": hash,
                "evidence": "Yes, land it.", "question": "Shall I land it?", "session_ref": "chat-7",
            }))
            .unwrap(),
        )
    };
    let err = mcp(&mut st, &shown).unwrap_err();
    shows_line(&err, err.corrected.as_ref().unwrap());
    // a list of recommended answers
    let (r1, h1) = (p.r#ref.clone(), shown.clone());
    let err = on(&*e, move |r| {
        r.answer_all_recommended(
            &relay_agent(),
            PERSON,
            &[r1],
            &[h1],
            relay_input("Yes, all of them."),
        )
    })
    .unwrap_err();
    shows_line(&err, err.corrected.as_ref().unwrap());
    // a direct approval with the card as evidence
    let (r1, h1) = (p.r#ref.clone(), shown.clone());
    let err = on(&*e, move |r| {
        r.land_with(
            &approver(),
            &r1,
            LandOptions {
                strict: false,
                relay: Some(Relay {
                    by: ActorRun {
                        actor: "agent:relay".into(),
                        run: Some("r1".into()),
                    },
                    evidence: RelayEvidence {
                        quoted: "Yes, land it.".into(),
                        session_ref: "chat-7".into(),
                        question: Some("Shall I land it?".into()),
                    },
                }),
                evidence: Some(ApprovalEvidence {
                    kind: EvidenceKind::Card,
                    options: vec![],
                    choice: Some("approve".into()),
                    card_hash: Some(h1.parse().unwrap()),
                }),
            },
        )
    })
    .unwrap_err();
    shows_line(&err, err.corrected.as_ref().unwrap());
    assert_eq!(e.read().canon_head(), head, "nothing landed");
    // the fresh card lands
    let v = mcp(&mut st, &fresh.card_hash).expect("the fresh card's approval lands");
    assert!(v["landing"].is_object(), "{v:#}");
    assert_ne!(e.read().canon_head(), head);
}

/// O13, O16: a verdict nobody accepted changes neither the stored readiness nor the stored
/// policy verdict: a person approves a version with a fail, and the policy lands another.
#[test]
fn ck7_unaccepted_fail_never_changes_readiness_or_policy() {
    let w = policed();
    typo(&w.e, "a1", &w.x, "X holds, now.");
    let p = propose_in(&w.e, "a1");
    let (h, before) = stored(&w.e, &p);
    assert_eq!(
        before.policy.as_ref().map(|a| a.verdict.clone()),
        Some(PolicyVerdict::Accepts {
            rule: "drafts".into()
        })
    );
    meaning_verdict(&w.e, &ag("a1"), &p.r#ref, "fail", &w.x);
    assert_eq!(stored(&w.e, &p), (h, before.clone()), "the stored record");
    let (pref, rec) = version(&w.e, &Ver::from(&p));
    {
        let st = w.e.read();
        let c = Candidate::stored(&st, pref, rec).unwrap();
        assert_eq!(readiness(&st, &c).unwrap().issues, vec![], "still ready");
    }
    assert!(held_of(&w.e, &p).is_empty());
    let done = approve_now(&w.e, &p);
    assert_eq!(done.act, "approve");
    // the policy lands a second version with a fail
    typo(&w.e, "a2", &w.z, "Z holds, now.");
    let q = propose_in(&w.e, "a2");
    meaning_verdict(&w.e, &ag("a2"), &q.r#ref, "fail", &w.z);
    let l = on(&w.e, move |r| r.land_by_policy(&ag("a2"), &q.r#ref)).expect("the policy lands it");
    assert_eq!(l.assurance, "policy");
    let st = w.e.read();
    let rec = st.reader().landing(&st.canon_head().unwrap()).unwrap();
    assert_eq!(rec.assurance, Assurance::Policy);
}

/// O14: the acceptance is one ANSWER `accept_verdict` of item `verdict:<review>`, with the full
/// option, the label, the card's labels and hash, no targets, the person and the relay. A stale
/// card, an option of another version and a person outside the policy role write nothing.
#[test]
fn ck7_acceptance_records_the_person_case_and_version() {
    let w = policed();
    typo(&w.e, "a1", &w.x, "X holds, now.");
    // a decision outside area payments: the version needs a person from approvers
    write_in(
        &w.e,
        "a1",
        json!([{"op": "create", "type": "decision", "id": "$dec", "parent": w.s.to_string(),
                "fields": {"statement": "Ideas stay short", "status": "proposed", "needs_content": false,
                           "reversibility": "two_way", "authority_basis": "precedent"}}]),
        j_typo(),
    );
    let p = propose_in(&w.e, "a1");
    let before = approve_card(&w.e, &p.r#ref).card_hash;
    let review = meaning_verdict(&w.e, &ag("a1"), &p.r#ref, "fail", &w.x);
    let card = approve_card(&w.e, &p.r#ref);
    let id = accept_id(&review, &p);
    assert!(ids(&card).contains(&id), "{:#?}", ids(&card));
    let item = format!("verdict:{review}");
    let unchanged = |e: &Local, seq| {
        assert_eq!(e.read().seq(), seq, "nothing was written");
        assert!(e.read().reader().answer_of(&item).unwrap().is_none());
    };
    // a stale card
    let seq = w.e.read().seq();
    let err = relayed(&w.e, PERSON, &p.r#ref, &id, &before).unwrap_err();
    assert_eq!(err.code, E_PROPOSAL_MOVED, "{err:#?}");
    unchanged(&w.e, seq);
    // an option of another version
    let other = format!("accept_verdict:{review}:{}", "0".repeat(64));
    let err = relayed(&w.e, PERSON, &p.r#ref, &other, &card.card_hash).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err:#?}");
    assert_eq!(
        err.message,
        format!("{other} is not an option of {}", p.r#ref)
    );
    unchanged(&w.e, seq);
    // a person outside the policy role
    let err = relayed(&w.e, "human:other", &p.r#ref, &id, &card.card_hash).unwrap_err();
    assert_eq!(err.code, E_PERMISSION, "{err:#?}");
    assert_eq!(
        err.message,
        format!(
            "human:other is not in the policy role approvers for {}",
            p.r#ref
        )
    );
    unchanged(&w.e, seq);
    // the person accepts, relayed
    let label = "Accept meaning preservation fail as blocking";
    let done = relayed(&w.e, PERSON, &p.r#ref, &id, &card.card_hash).unwrap();
    assert_eq!(
        (done.act.as_str(), done.option.as_str(), done.label.as_str()),
        ("accept_verdict", id.as_str(), label)
    );
    assert_eq!(done.assurance, "relayed");
    let (h, a) =
        w.e.read()
            .reader()
            .answer_of(&item)
            .unwrap()
            .expect("the answer");
    assert_eq!(done.answer, Some(h.to_string()));
    assert_eq!(
        Answer {
            person: Provenance {
                at_ms: 0,
                ..a.person.clone()
            },
            ..a.clone()
        },
        Answer {
            item: item.clone(),
            act: AnswerAct::AcceptVerdict,
            option: Some(id.clone()),
            label: Some(label.into()),
            options: card.options.iter().map(|o| o.label.clone()).collect(),
            card_hash: Some(card.card_hash.parse().unwrap()),
            targets: vec![],
            person: Provenance {
                actor: PERSON.into(),
                run: Some("cli".into()),
                role: Role::Approver,
                at_ms: 0,
            },
            relayed_by: Some(ActorRun {
                actor: "agent:relay".into(),
                run: Some("r1".into()),
            }),
            relay_evidence: Some(RelayEvidence {
                quoted: "Yes, do it.".into(),
                session_ref: "chat-7".into(),
                question: Some("Shall I?".into()),
            }),
            evidence_kind: Some(EvidenceKind::Card),
            note: None,
        }
    );
    // the next queue read places the version under not_ready
    assert!(!has_card(&w.e, &p.r#ref));
    assert_eq!(
        not_ready(&w.e, &p.r#ref),
        Some(vec![held("meaning_preservation", &review, &p.r#ref)])
    );
}

/// O14, D-HAG73: an accepted verdict refuses every lander with the same issue: a direct
/// approval, an answer's approval, a list of recommended answers and the policy; the queue lists
/// the version as not ready.
#[test]
fn ck7_acceptance_blocks_every_lander_and_pending() {
    let w = policed();
    typo(&w.e, "a1", &w.x, "X holds, now.");
    let p = propose_in(&w.e, "a1");
    let review = meaning_verdict(&w.e, &ag("a1"), &p.r#ref, "fail", &w.x);
    let done = local(&w.e, &p.r#ref, &accept_id(&review, &p)).unwrap();
    assert_eq!(done.act, "accept_verdict");
    let want = vec![held("meaning_preservation", &review, &p.r#ref)];
    // the author writes the held block again and proposes nothing: the version, and so its
    // case and its hold, stay as they were
    typo(&w.e, "a1", &w.x, "X holds, fixed.");
    assert_eq!(
        held_of(&w.e, &p),
        want,
        "an unproposed edit of the held block"
    );
    let head = w.e.read().canon_head();
    let r1 = p.r#ref.clone();
    assert_eq!(
        not_ready_issues(on(&w.e, move |r| r.land_report(&approver(), &r1))),
        want,
        "a direct approval"
    );
    assert_eq!(
        not_ready_issues(local(&w.e, &p.r#ref, "approve")),
        want,
        "an answer's approval"
    );
    let r1 = p.r#ref.clone();
    assert_eq!(
        not_ready_issues(on(&w.e, move |r| r.answer_all_recommended(
            &approver(),
            PERSON,
            &[r1],
            &[],
            RelayInput::default()
        ))),
        want,
        "a list of recommended answers"
    );
    let r1 = p.r#ref.clone();
    assert_eq!(
        not_ready_issues(on(&w.e, move |r| r.land_by_policy(&ag("a1"), &r1))),
        want,
        "the policy"
    );
    assert_eq!(w.e.read().canon_head(), head, "nothing landed");
    assert_eq!(
        w.e.read()
            .materialized(&rdy_engine::Scope::Canon)
            .unwrap()
            .get(&w.x)
            .unwrap()
            .body,
        "X holds.",
        "the blocked text is not canon"
    );
    assert!(!has_card(&w.e, &p.r#ref));
    assert_eq!(not_ready(&w.e, &p.r#ref), Some(want));
}

/// O14: one review accepted for two versions holds both: every AcceptVerdict answer counts, not
/// only the one the latest-answer ref names, also after a reindex and a reopen. A third version
/// inherits no hold.
#[test]
fn ck7_one_review_can_hold_two_versions_without_latest_ref_loss() {
    let (n, first) = notes_version();
    let review = meaning_verdict(&n.e, &ag("a1"), &first.r#ref, "fail", &n.x);
    local(&n.e, &first.r#ref, &accept_id(&review, &first)).unwrap();
    // a new version with the same case: an unjustified edit is no editorial row
    write_in(&n.e, "a1", upd(&n.z, "Z holds, now."), None);
    let second = propose_in(&n.e, "a1");
    let card = approve_card(&n.e, &second.r#ref);
    assert!(
        ids(&card).contains(&accept_id(&review, &second)),
        "{card:#?}"
    );
    local(&n.e, &second.r#ref, &accept_id(&review, &second)).unwrap();
    let item = format!("verdict:{review}");
    let latest = n.e.read().reader().answer_of(&item).unwrap().unwrap().1;
    assert_eq!(latest.option, Some(accept_id(&review, &second)));
    let both = |e: &Local| {
        assert_eq!(
            held_of(e, &first),
            vec![held("meaning_preservation", &review, &first.r#ref)]
        );
        assert_eq!(
            held_of(e, &second),
            vec![held("meaning_preservation", &review, &second.r#ref)]
        );
    };
    both(&n.e);
    on(&n.e, |r| r.reindex()).unwrap();
    both(&n.e);
    let Notes { dir, e, z, .. } = n;
    e.close().unwrap();
    let e = Local::open(dir.path()).unwrap();
    both(&e);
    // a third version: the verdict shows with its option, and nothing holds it
    write_in(&e, "a1", upd(&z, "Z holds, again."), None);
    let third = propose_in(&e, "a1");
    assert!(held_of(&e, &third).is_empty());
    let card = approve_card(&e, &third.r#ref);
    assert!(
        ids(&card).contains(&accept_id(&review, &third)),
        "{card:#?}"
    );
    assert_eq!(not_ready(&e, &third.r#ref), None);
}

/// O14, p-8WADR: a changed case ends the check's hold, and a new version holds nothing of it;
/// an accepted tension advice keeps its own text-bound rule, which a new version cannot pass.
#[test]
fn ck7_changed_case_ends_only_the_check_hold() {
    let (dir, r, b) = batch_project();
    let e = Local::new(r);
    let batch = ag(rdy_qa::intake::BATCH_VIEW);
    let first = Ver {
        r#ref: b.r#ref.clone(),
        id: b.report["id"].as_str().unwrap().to_string(),
    };
    let q = ask(
        &e,
        &batch,
        "decision_review",
        json!(b.member.to_string()),
        true,
    );
    let review = answer_request(&e, &batch, &q, "fail", &b.member);
    local(&e, &first.r#ref, &accept_id(&review, first.clone())).unwrap();
    // the person also accepts the reviewer's advice on the version's tension
    let (proposal, digest) = (
        b.r#ref.clone(),
        b.report["advised_digest"].as_str().unwrap().to_string(),
    );
    let accepted = on(&e, move |r| {
        r.accept_advice(
            &writer(),
            PERSON,
            AcceptRequest {
                proposal,
                advised_digest: digest,
                ..Default::default()
            },
        )
    })
    .unwrap()
    .accepted;
    assert!(!accepted.is_empty());
    let r1 = first.r#ref.clone();
    let issues = not_ready_issues(on(&e, move |r| r.land_report(&approver(), &r1)));
    let (tension, check) = issues.split_at(issues.len() - 1);
    assert!(
        !tension.is_empty() && tension.iter().all(|i| i.code == E_BLOCKING_TENSION),
        "{issues:#?}"
    );
    assert_eq!(check, [held("decision_review", &review, &first.r#ref)]);
    // the author changes the reviewed decision and proposes again
    write_in(
        &e,
        rdy_qa::intake::BATCH_VIEW,
        json!([{"op": "update", "id": b.member.to_string(),
                "body": "Guest readers need no account, ever."}]),
        None,
    );
    // the view edit alone is no new version: the first version's case and hold stay
    assert_eq!(
        held_of(&e, first.clone()),
        vec![held("decision_review", &review, &first.r#ref)],
        "an unproposed edit keeps the hold"
    );
    let r1 = first.r#ref.clone();
    let issues = not_ready_issues(on(&e, move |r| r.land_report(&approver(), &r1)));
    assert_eq!(
        issues.last(),
        Some(&held("decision_review", &review, &first.r#ref)),
        "{issues:#?}"
    );
    let view = rdy_qa::intake::BATCH_VIEW.to_string();
    let second = on(&e, move |r| {
        r.propose(&ag(&view), &view, "again", Some(TEST_SUMMARY))
    })
    .unwrap();
    assert!(
        held_of(&e, &second).is_empty(),
        "no hold on the new version"
    );
    assert!(!second.ready, "the accepted advice still blocks");
    assert!(
        second.issues.iter().any(|i| i.code == E_BLOCKING_TENSION),
        "{:#?}",
        second.issues
    );
    assert!(
        !second
            .issues
            .iter()
            .any(|i| i.message.contains("is accepted as blocking")),
        "{:#?}",
        second.issues
    );
    drop(dir);
}

/// O14, O16, TASK-943 C2: code evidence that cannot be read keeps an accepted hold for the
/// approver and the policy alike: a missing checkout, a reopen without it, a recorded commit
/// that is gone and a git read that fails. Identical evidence restored keeps the hold; changed
/// evidence ends it. An unavailable verdict nobody accepted is no issue, a verdict line that
/// cannot be shown erases no hold, and a new version inherits none.
#[test]
fn ck7_unavailable_code_preserves_accepted_holds_for_both_landers() {
    let w = policed();
    typo(&w.e, "a1", &w.x, "X holds, now.");
    let made = write_in(
        &w.e,
        "a1",
        json!([{"op": "create", "type": "decision", "id": "$dec", "parent": w.ws.to_string(),
                "fields": {"statement": "Fees round up", "status": "proposed", "needs_content": false,
                           "reversibility": "two_way", "authority_basis": "precedent",
                           "confirmation": "The fee test passes."}}]),
        j_typo(),
    );
    let dec = created(&made, "$dec");
    let (_checkout, repo, from, to) = checkout_for(&dec);
    link_repo(w.dir.path(), &repo);
    let a1 = ag("a1");
    let range = format!("app@{}..{}", &from[..12], &to[..12]);
    let qd = ask(
        &w.e,
        &a1,
        "diff_contradicts",
        json!({"decision": dec.to_string(), "code": range}),
        true,
    );
    let diff = answer_request(&w.e, &a1, &qd, "fail", &dec);
    let qc = ask(&w.e, &a1, "confirmation", json!(dec.to_string()), true);
    let confirmation = answer_request(&w.e, &a1, &qc, "fail", &dec);
    let p = propose_in(&w.e, "a1");
    let moved = repo.with_extension("moved");
    // unaccepted and unavailable: no line and no accepted-verdict issue
    std::fs::rename(&repo, &moved).unwrap();
    assert!(shown_of(&w.e, &p).is_empty());
    assert!(held_of(&w.e, &p).is_empty());
    std::fs::rename(&moved, &repo).unwrap();
    // the person accepts the diff fail on the card; the confirmation fail is accepted as the
    // card's answer records it (the card left the queue with the first acceptance)
    let card = approve_card(&w.e, &p.r#ref);
    assert_eq!(card.check_verdicts.len(), 2);
    local(&w.e, &p.r#ref, &accept_id(&diff, &p)).unwrap();
    let a = Answer {
        item: format!("verdict:{confirmation}"),
        act: AnswerAct::AcceptVerdict,
        option: Some(accept_id(&confirmation, &p)),
        label: Some("Accept confirmation fail as blocking".into()),
        options: card.options.iter().map(|o| o.label.clone()).collect(),
        card_hash: Some(card.card_hash.parse().unwrap()),
        targets: vec![],
        person: approver().provenance(),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: None,
    };
    on(&w.e, move |r| {
        r.store_mut().put_answer(&a).map_err(RdyError::from)?;
        r.invalidate();
        Ok(())
    })
    .unwrap();
    let both = vec![
        held("diff_contradicts", &diff, &p.r#ref),
        held("confirmation", &confirmation, &p.r#ref),
    ];
    let landers = |e: &Local| -> (Vec<Issue>, Vec<Issue>) {
        let (r1, r2) = (p.r#ref.clone(), p.r#ref.clone());
        (
            not_ready_issues(on(e, move |r| r.land_report(&approver(), &r1))),
            not_ready_issues(on(e, move |r| r.land_by_policy(&ag("a1"), &r2))),
        )
    };
    assert_eq!(landers(&w.e), (both.clone(), both.clone()), "accepted");
    // 1. a missing checkout: no verdict line, and the holds stay
    std::fs::rename(&repo, &moved).unwrap();
    assert!(shown_of(&w.e, &p).is_empty(), "no line can be shown");
    assert_eq!(
        landers(&w.e),
        (both.clone(), both.clone()),
        "missing checkout"
    );
    // 2. a reopen without it
    let Policed { dir, e, z, .. } = w;
    e.close().unwrap();
    let e = Local::open(dir.path()).unwrap();
    assert_eq!(landers(&e), (both.clone(), both.clone()), "reopened");
    std::fs::rename(&moved, &repo).unwrap();
    // 3. a recorded commit that is gone
    let commit_object = object_path(&repo, &from);
    let saved = std::fs::read(&commit_object).unwrap();
    std::fs::remove_file(&commit_object).unwrap();
    assert_eq!(
        landers(&e),
        (both.clone(), both.clone()),
        "a missing commit"
    );
    std::fs::write(&commit_object, &saved).unwrap();
    // 4. a git read that fails: the new blob is gone, and its worktree file too
    let blob = git(&repo, &["rev-parse", &format!("{to}:cache.rs")]);
    let blob_object = object_path(&repo, &blob);
    let saved = std::fs::read(&blob_object).unwrap();
    std::fs::remove_file(&blob_object).unwrap();
    std::fs::remove_file(repo.join("cache.rs")).unwrap();
    assert_eq!(
        landers(&e),
        (both.clone(), both.clone()),
        "a failed git read"
    );
    // 5. identical evidence restored keeps the holds
    std::fs::write(&blob_object, saved).unwrap();
    git(&repo, &["checkout", "--", "cache.rs"]);
    assert_eq!(landers(&e), (both.clone(), both.clone()), "restored");
    // 6. changed evidence: a new head changes the confirmation case only
    commit(
        &repo,
        "cache.rs",
        "fn a() { cache(); log(); }\n",
        &format!("log\n\nImplements: {dec}"),
    );
    let diff_only = vec![held("diff_contradicts", &diff, &p.r#ref)];
    assert_eq!(landers(&e), (diff_only.clone(), diff_only), "changed");
    // a new version inherits no hold, and the policy lands it
    typo(&e, "a1", &z, "Z holds, now.");
    let again = propose_in(&e, "a1");
    assert!(held_of(&e, &again).is_empty());
    let card = approve_card(&e, &again.r#ref);
    assert_eq!(
        card.check_verdicts
            .iter()
            .map(|m| m.review.to_string())
            .collect::<Vec<_>>(),
        vec![diff.clone()],
        "the diff fail shows on the new version"
    );
    let l = on(&e, move |r| r.land_by_policy(&ag("a1"), &again.r#ref)).expect("it lands");
    assert_eq!(l.assurance, "policy");
}

/// A Pass, Concern or Fail review of request `request` with case `case` by agent:r, run `run`.
fn raw_review(
    request: Hash,
    case: Hash,
    verdict: Verdict,
    reason: String,
    cite: Option<&BlockId>,
    run: String,
    at_ms: i64,
) -> Review {
    Review {
        subject: ReviewSubject::Check { request },
        case_hash: Some(case),
        verdict,
        reason,
        citations: cite
            .map(|b| {
                vec![Citation {
                    block: b.clone(),
                    note: "n".into(),
                }]
            })
            .unwrap_or_default(),
        provenance: Provenance {
            actor: "agent:r".into(),
            run: Some(run),
            role: Role::Reviewer,
            at_ms,
        },
    }
}

/// Writes `reviews` straight to the log, in batches (imported history may hold many reviews of
/// one request).
fn write_reviews(e: &dyn Engine, reviews: Vec<Review>) {
    on(e, move |r| {
        for chunk in reviews.chunks(10_000) {
            let items: Vec<_> = chunk
                .iter()
                .map(|v| rdy_store::codec::item_new(rdy_store::kinds::REVIEW, v))
                .collect();
            r.store_mut().write(items).map_err(RdyError::from)?;
        }
        r.invalidate();
        Ok(())
    })
    .unwrap();
}

/// A version of `notes` and one independent meaning request on it: (world, version, request,
/// case).
fn limit_world() -> (Notes, ProposalInfo, Hash, Hash) {
    let (n, p) = notes_version();
    let (rq, case) = ask(
        &n.e,
        &ag("a1"),
        "meaning_preservation",
        json!(p.r#ref),
        false,
    );
    (
        n,
        p,
        rq["rq-".len()..].parse().unwrap(),
        case.parse().unwrap(),
    )
}

/// A reason of exactly `len` UTF-8 bytes and at most 2,000 characters (`len` <= 8,000).
fn reason_of(len: usize) -> String {
    let mut s = "\u{1F600}".repeat(len / 4);
    s.push_str(match len % 4 {
        1 => "a",
        2 => "\u{e9}",
        3 => "\u{20ac}",
        _ => "",
    });
    assert_eq!(s.len(), len);
    s
}

/// Fills `n`'s version `p` with fail reviews of request `rq` until their verdict text is exactly
/// `target` bytes.
fn fill_text(n: &Notes, p: &ProposalInfo, rq: Hash, case: Hash, target: usize) {
    let review = |i: usize, len: usize| {
        raw_review(
            rq,
            case,
            Verdict::Fail,
            reason_of(len),
            Some(&n.x),
            format!("r{i:06}"),
            i as i64,
        )
    };
    // one probe row gives the fixed part of each row
    write_reviews(&n.e, vec![review(0, 8000)]);
    let row = shown_of(&n.e, p)[0].text_bytes();
    let fixed = row - 8000;
    let mut left = target - row;
    let mut lens = vec![];
    while left > 0 {
        let len = if left > row + fixed {
            // a full row, leaving room for at least one more
            8000.min(left - fixed - (fixed + 1))
        } else {
            left - fixed
        };
        assert!((1..=8000).contains(&len), "{left} {row} {fixed}");
        lens.push(len);
        left -= len + fixed;
    }
    write_reviews(
        &n.e,
        lens.iter()
            .enumerate()
            .map(|(i, len)| review(i + 1, *len))
            .collect(),
    );
}

/// The card limits (fixed safety limits): 100,000 verdict rows and 16 MiB of verdict text show
/// whole; one row or one byte past is `E_LIMIT`, never a shorter card, and no approval binds.
/// The hash covers every row, also those the rendered card cuts.
#[test]
fn ck7_card_limits_refuse_without_truncating_authorization() {
    let (n, p, rq, case) = limit_world();
    let passes: Vec<Review> = (0..100_000)
        .map(|i| {
            raw_review(
                rq,
                case,
                Verdict::Pass,
                String::new(),
                None,
                format!("r{i:06}"),
                i as i64,
            )
        })
        .collect();
    write_reviews(&n.e, passes);
    let card = approve_card(&n.e, &p.r#ref);
    assert_eq!(card.check_verdicts.len(), 100_000);
    assert!(card.card.contains("facts cut"), "the rendered card is cut");
    assert_eq!(card.card_hash, card_hash(&card).to_string());
    let small = queue(&n.e, 2_000)
        .unwrap()
        .items
        .into_iter()
        .find(|i| i.r#ref == p.r#ref)
        .unwrap();
    assert_eq!(small.card_hash, card.card_hash, "the budget moves no hash");
    // a row the rendered card leaves out still moves the hash
    let mut other = card.clone();
    let last = other.facts.len() - 1;
    other.facts[last].value = "fail (agent:r, independent): hidden".into();
    other.check_verdicts[99_999].verdict = "fail".into();
    other.check_verdicts[99_999].reason = "hidden".into();
    assert_eq!(
        render(&other, CARD_MAX_TOKENS),
        render(&card, CARD_MAX_TOKENS)
    );
    assert_ne!(card_hash(&other), card_hash(&card));
    // one row past
    write_reviews(
        &n.e,
        vec![raw_review(
            rq,
            case,
            Verdict::Pass,
            String::new(),
            None,
            "r100000".into(),
            100_000,
        )],
    );
    let limit = |err: RdyError, pref: &str, what: &str| {
        assert_eq!(
            (err.code, err.message.as_str(), err.fix.as_str()),
            (
                E_LIMIT,
                format!("the approval card of {pref} matches more than {what}").as_str(),
                "propose a smaller version: rdy_propose / rdy propose"
            ),
            "{err:#?}"
        );
    };
    limit(
        queue(&n.e, CARD_MAX_TOKENS).unwrap_err(),
        &p.r#ref,
        "100000 check verdicts",
    );
    let head = n.e.read().canon_head();
    limit(
        relayed(&n.e, PERSON, &p.r#ref, "approve", &card.card_hash).unwrap_err(),
        &p.r#ref,
        "100000 check verdicts",
    );
    assert_eq!(n.e.read().canon_head(), head, "nothing landed");

    // 16 MiB of text shows whole; one byte past refuses
    const TEXT: usize = 16 * 1024 * 1024;
    let (n, p, rq, case) = limit_world();
    fill_text(&n, &p, rq, case, TEXT);
    let card = approve_card(&n.e, &p.r#ref);
    assert_eq!(
        card.check_verdicts
            .iter()
            .map(MatchingVerdict::text_bytes)
            .sum::<usize>(),
        TEXT
    );
    let (n, p, rq, case) = limit_world();
    fill_text(&n, &p, rq, case, TEXT + 1);
    limit(
        queue(&n.e, CARD_MAX_TOKENS).unwrap_err(),
        &p.r#ref,
        "16777216 bytes of check verdict text",
    );
}
