//! cp12 (D-AAQ9Z, D-GV465, D-DKJS5, D-MPJM0, D-JV6Z8; concepts/decision-intake p-T69HT,
//! reference/mcp-tools p-VVBVR, p-D3P76, reference/errors p-7JZCN, concepts/briefings p-AEYTC;
//! O10, O19): a version whose bound plan defers non-code items stands beside its batch with a
//! deferral card, and every approval path refuses it with `E_DEFERRAL_OPEN` until the card has
//! an answer. A refusal makes the version not ready. A version that re-affirms a `one_way`
//! decision stands beside its batch with a `reaffirm_one_way` exception. The seal names the
//! acceptance of the approved version. Code deferrals and versions proposed before store 17 never
//! meet the hold.
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::engine::Local;
use rdy_engine::pending::{
    APPROVE_REASON, APPROVE_REASON_BESIDE, CARD_DEFAULT_TOKENS, CARD_MAX_TOKENS, ExceptionKind,
    Item, ItemKind, Pending, PendingQuery, PlanCardBinding, card_hash, render,
};
use rdy_engine::plan::{PlanAnswerInput, ProposePlanRequest};
use rdy_engine::{
    DecisionInput, E_BAD_INPUT, E_DEFERRAL_OPEN, E_EXCEPTION_OPEN, E_ITEM_MOVED,
    E_PROPOSAL_NOT_READY, E_VALIDATION, LandOptions, ProposalInfo, Rdy, RdyError, Session,
};
use rdy_mcp::tools::{McpState, rdy_answer, rdy_pending, rdy_session};
use rdy_model::plan::{PlanAct, PlanKind, PlanOutcome, item_display, item_hash};
use rdy_model::{Answer, AnswerAct, BlockId, Hash};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{approver, project, write, write_as, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY, confirm_exceptions};
use rdy_qa::tensions::ag;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

// ---- the project ----

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn h(s: &str) -> Hash {
    s.parse().unwrap()
}

/// Canon: doc `spec`, section `Rules`, claims a ("A holds 30 days."), c ("C stands."), b, which
/// references a and c, q, which references a, p, which states a's literal, and z, which nothing
/// references.
struct Spec {
    s: String,
    a: String,
    b: String,
    c: String,
    q: String,
    p: String,
    z: String,
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
            {"op": "create", "type": "claim", "id": "$q", "parent": "$s", "body": "Q builds on {{$a}}."},
            {"op": "create", "type": "claim", "id": "$p", "parent": "$s", "body": "Wait 30 days per {{$a}}."},
            {"op": "create", "type": "claim", "id": "$z", "parent": "$s", "body": "Z stands alone."}
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
        q: get("$q"),
        p: get("$p"),
        z: get("$z"),
    }
}

fn edit_as(r: &mut Rdy, s: &Session, view: &str, target: &str, body: &str) {
    write_as(
        r,
        s,
        view,
        json!([{"op": "update", "id": target, "body": body}]),
    )
    .unwrap_or_else(|e| panic!("edit {target} in {view}: {e}"));
}

fn edit(r: &mut Rdy, view: &str, target: &str, body: &str) {
    edit_as(r, &writer(), view, target, body);
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

/// Proposes `view` as `s` with a summary and `answers`.
fn propose_as(r: &mut Rdy, s: &Session, view: &str, answers: &[PlanAnswerInput]) -> ProposalInfo {
    r.propose_plan(
        s,
        ProposePlanRequest {
            summary: Some(TEST_SUMMARY),
            plan_answers: answers,
            ..ProposePlanRequest::new(view, view)
        },
    )
    .unwrap_or_else(|e| panic!("propose {view}: {e}"))
}

fn propose(r: &mut Rdy, view: &str, answers: &[PlanAnswerInput]) -> ProposalInfo {
    propose_as(r, &writer(), view, answers)
}

fn defer_ref(pref: &str) -> String {
    format!("defer:{pref}")
}

/// The queue as [`PERSON`] sees it: every card whole, or cut to `max`.
fn queue_at(r: &Rdy, max: usize) -> Pending {
    r.read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: max,
        })
        .expect("pending")
}

fn queue(r: &Rdy) -> Pending {
    queue_at(r, CARD_MAX_TOKENS)
}

fn item_of(r: &Rdy, r#ref: &str) -> Option<Item> {
    queue(r).items.into_iter().find(|i| i.r#ref == r#ref)
}

fn card_json(i: &Item) -> Value {
    json!({"ref": i.r#ref, "card_hash": i.card_hash, "card": i.card})
}

fn log_end(r: &Rdy) -> Location {
    r.store().log_end()
}

fn fact(i: &Item, name: &str) -> String {
    i.facts
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no fact {name}: {:#?}", i.facts))
        .value
        .clone()
}

/// The local answer of `option` to `item` by the person.
fn answer_local(r: &mut Rdy, item: &str, option: &str) -> Result<Value, RdyError> {
    r.answer(
        &approver(),
        PERSON,
        AnswerRequest {
            r#ref: item.into(),
            option: option.into(),
            ..AnswerRequest::default()
        },
    )
    .map(|i| serde_json::to_value(i).unwrap())
}

fn land_err(r: &mut Rdy, pref: &str) -> RdyError {
    let before = log_end(r);
    let e = r
        .land_with(&approver(), pref, LandOptions::default())
        .unwrap_err();
    assert_eq!(log_end(r), before, "a refused landing writes nothing");
    e
}

/// Checks `e` is the refusal of `pref`, which is not ready, with the queue's issues `issues`
/// (D-ME2HD).
fn not_ready(e: &RdyError, pref: &str, issues: &[rdy_schema::Issue]) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_PROPOSAL_NOT_READY,
            format!(
                "{pref} is not ready: {} issue{}",
                issues.len(),
                if issues.len() == 1 { "" } else { "s" }
            )
            .as_str(),
            "fix each issue and propose again: rdy_propose / rdy propose"
        ),
        "{e}"
    );
    assert_eq!(e.issues, issues, "{e}");
    assert!(e.corrected.is_none(), "{e}");
}

/// Checks `e` is the deferral hold of `pref`, with `n` deferrals and the card `card`.
fn deferral_open(e: &RdyError, pref: &str, n: usize, card: &Item) {
    assert_eq!(e.code, E_DEFERRAL_OPEN, "{e}");
    assert_eq!(e.message, format!("{n} deferrals of {pref} have no answer"));
    assert_eq!(
        e.fix,
        format!("answer the deferral card first: defer:{pref}")
    );
    assert_eq!(e.corrected, Some(json!({"deferral": card_json(card)})));
}

/// The latest ANSWER to `item` and its hash.
fn answer_record(r: &Rdy, item: &str) -> (Hash, Answer) {
    r.read()
        .reader()
        .answer_of(item)
        .unwrap()
        .expect("an answer")
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
            "actor: human:qa\ncode_paths:\n  app: {}\n",
            serde_json::to_string(&tmp.path().display().to_string()).unwrap()
        ),
    )
    .unwrap();
    tmp
}

/// An accepted decision under `section` with `statement`, depending on `depends_on`, of
/// reversibility `reversibility`, made in view `view` and landed.
fn decision(
    r: &mut Rdy,
    view: &str,
    section: &str,
    statement: &str,
    depends_on: &[&str],
    reversibility: &str,
) -> String {
    let d = r
        .decide(
            &writer(),
            view,
            DecisionInput {
                parent: section.into(),
                statement: statement.into(),
                depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
                reversibility: Some(reversibility.into()),
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("decide {statement}: {e}"));
    let out = d.created["$decision"].clone();
    rdy_qa::fixtures::land(r, view).unwrap();
    out
}

/// View `w` changes a (b and q follow up with a confirm, p with an update) and defers b's and
/// q's items with their reasons in the propose: the version, with two deferrals.
fn deferred(r: &mut Rdy, s: &Spec) -> ProposalInfo {
    edit(r, "w", &s.a, "A holds 60 days.");
    propose(
        r,
        "w",
        &[
            defer(&pi(&s.a, &s.b, PlanKind::Confirm), "b waits for the review"),
            defer(
                &pi(&s.a, &s.q, PlanKind::Confirm),
                "q waits for the rewrite",
            ),
        ],
    )
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

/// The card `r#ref` in the MCP queue.
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

/// A relayed MCP answer of `option` to the card `card`.
fn mcp_answer(st: &mut McpState, card: &Value, option: &str) -> Result<Value, RdyError> {
    rdy_answer(
        st,
        serde_json::from_value(json!({
            "ref": card["ref"], "option": option, "card_hash": card["card_hash"],
            "evidence": "Yes.", "question": "Shall I?", "session_ref": "chat-7",
        }))
        .unwrap(),
    )
}

/// Runs `f` on the served project with the daemon stopped.
fn with_rdy<T>(s: &Served, f: impl FnOnce(&mut Rdy) -> T) -> T {
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let out = f(&mut r);
    r.close().unwrap();
    out
}

// ---- tests ----

/// p-T69HT, p-VVBVR: the deferral card lists the valid deferred non-code items of the version's
/// bound plan, in full item hash order, each with its author and reason; a computed close wins
/// over a deferral, a lapsed answer and a code item are never listed, and no option is
/// recommended. The card binds every listed item.
#[test]
fn cp12_deferral_card_lists_only_valid_noncode_deferrals() {
    let (d, mut r) = project();
    let repo = linked_repo(d.path());
    let s = spec(&mut r);
    commit(
        repo.path(),
        "f.rs",
        &format!("start\n\nImplements: {}", s.a),
    );
    let agent = ag("w");
    edit_as(&mut r, &agent, "w", &s.a, "A holds 60 days.");
    // the agent defers q and the code item; p's update is deferred and then closed by the
    // version itself; c is edited after its deferral, which lapses
    edit_as(&mut r, &agent, "w", &s.c, "C stands firm.");
    let bq = pi(&s.a, &s.q, PlanKind::Confirm);
    let bc = pi(&s.c, &s.b, PlanKind::Confirm);
    r.answer_plan(
        &agent,
        "w",
        &[
            defer(&bq, "q waits for the rewrite"),
            defer(
                &pi(&s.a, "app:f.rs", PlanKind::UpdateImplementation),
                "code later",
            ),
            defer(&pi(&s.a, &s.p, PlanKind::Update), "p later"),
            defer(&bc, "b on c later"),
        ],
    )
    .unwrap();
    edit_as(
        &mut r,
        &agent,
        "w",
        &s.p,
        &format!("Wait 60 days per {{{{{}}}}}.", s.a),
    );
    edit_as(&mut r, &agent, "w", &s.c, "C stands firmer.");
    // the agent defers b's item on a in its propose
    let ba = pi(&s.a, &s.b, PlanKind::Confirm);
    let p = propose_as(&mut r, &agent, "w", &[defer(&ba, "b waits for the review")]);
    let card = item_of(&r, &defer_ref(&p.r#ref)).expect("the deferral card");
    let v = serde_json::to_value(&card).unwrap();
    assert_eq!(v["kind"], "defer");
    assert_eq!(card.title, format!("Deferrals in {}", p.r#ref));
    assert_eq!(card.why, "these items stay open after this proposal lands");
    assert_eq!(card.batch.as_deref(), Some(p.r#ref.as_str()));
    assert_eq!((card.summary.as_deref(), card.exception), (None, None));
    assert!(card.members.is_empty() && !card.blocking);
    assert_eq!(card.raised_by, "agent:w");
    // full item hash order
    let mut want = [
        (
            item_hash(&id(&s.a), &s.b, PlanKind::Confirm),
            format!("{} (after {})", s.b, s.a),
            "agent:w: b waits for the review".to_string(),
        ),
        (
            item_hash(&id(&s.a), &s.q, PlanKind::Confirm),
            format!("{} (after {})", s.q, s.a),
            "agent:w: q waits for the rewrite".to_string(),
        ),
    ];
    want.sort_by_key(|(h, _, _)| *h);
    let got: Vec<(String, String)> = card
        .facts
        .iter()
        .map(|f| (f.name.to_string(), f.value.clone()))
        .collect();
    let names: Vec<(String, String)> = want
        .iter()
        .map(|(_, n, v)| (n.clone(), v.clone()))
        .collect();
    assert_eq!(got, names, "{:#?}", card.facts);
    let opts: Vec<(&str, &str, bool)> = card
        .options
        .iter()
        .map(|o| (o.id.as_str(), o.label.as_str(), o.recommended.is_some()))
        .collect();
    assert_eq!(
        opts,
        vec![
            ("o1", "Accept the deferrals", false),
            ("o2", "Refuse: do them in this proposal", false),
            ("later", "Later", false),
        ]
    );
    // the binding: the version, its bound plan, and each item in full
    let bound = r
        .read()
        .reader()
        .plan_of_proposal(&h(&p.id), log_end(&r))
        .unwrap()
        .unwrap();
    let Some(PlanCardBinding::Deferrals {
        version,
        plan,
        items,
    }) = &card.plan_binding
    else {
        panic!("a deferral binding: {:#?}", card.plan_binding)
    };
    assert_eq!((*version, *plan), (h(&p.id), bound));
    let ids: Vec<Hash> = items.iter().map(|i| i.item).collect();
    assert_eq!(ids, want.iter().map(|(h, _, _)| *h).collect::<Vec<_>>());
    assert!(items.iter().all(|i| i.answer.act == PlanAct::Defer
        && i.kind == PlanKind::Confirm
        && i.cause == id(&s.a)
        && i.subject_hash.is_some()));
    assert_eq!(
        v["plan_binding"]["deferrals"]["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(card.card_hash, card_hash(&card).to_string());
    // the batch card counts every item, code items too
    let batch = item_of(&r, &p.r#ref).unwrap();
    let plan_fact = fact(&batch, "Change plan");
    assert!(plan_fact.ends_with(", 3 deferred"), "{plan_fact}");
}

/// D-AAQ9Z: every approval path refuses the version with `E_DEFERRAL_OPEN` until its card has an
/// answer: the engine's landing, a relayed MCP approval, the person's own `rdy approve`, a
/// relayed `rdy approve` and `rdy answer`. An accepted card, local through the CLI or relayed
/// through MCP, lets the version land.
#[test]
fn cp12_every_approval_path_waits_for_deferral_answer() {
    // in process: the engine's landing, then rdy_answer
    let (d, mut r) = project();
    std::fs::write(d.path().join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
    let s = spec(&mut r);
    let p = deferred(&mut r, &s);
    let dr = defer_ref(&p.r#ref);
    let card = item_of(&r, &dr).expect("the deferral card");
    let shown = queue_at(&r, CARD_DEFAULT_TOKENS)
        .items
        .into_iter()
        .find(|i| i.r#ref == dr)
        .unwrap();
    let e = land_err(&mut r, &p.r#ref);
    deferral_open(&e, &p.r#ref, 2, &shown);
    let mut st = agent_state(r);
    let published = st.engine.read().seq();
    let batch = mcp_card(&mut st, &p.r#ref);
    let e = mcp_answer(&mut st, &batch, "approve").unwrap_err();
    assert_eq!(e.code, E_DEFERRAL_OPEN, "{e}");
    assert_eq!(
        e.corrected,
        Some(json!({"deferral": card_json(&shown)})),
        "{e}"
    );
    assert_eq!(
        st.engine.read().seq(),
        published,
        "a refused answer writes nothing"
    );
    // the relayed acceptance records the act, the full card hash and the label
    let defer_card = mcp_card(&mut st, &dr);
    assert_eq!(defer_card["card_hash"], card.card_hash.as_str());
    let v = mcp_answer(&mut st, &defer_card, "o1").unwrap();
    assert_eq!(v["act"], "accept_deferrals", "{v}");
    assert_eq!(v["assurance"], "relayed", "{v}");
    let (ah, a) = st.engine.read().reader().answer_of(&dr).unwrap().unwrap();
    assert_eq!(v["answer"], ah.to_string());
    assert_eq!(a.act, AnswerAct::AcceptDeferrals);
    assert_eq!(a.card_hash, Some(h(&card.card_hash)));
    assert_eq!(a.label.as_deref(), Some("Accept the deferrals"));
    assert!(a.targets.is_empty());
    assert!(a.relayed_by.is_some());
    // the queue omits an accepted card, and the relayed approval lands
    let batch = mcp_card(&mut st, &p.r#ref);
    let q = rdy_pending(&mut st, serde_json::from_value(json!({})).unwrap()).unwrap();
    assert!(
        q["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["ref"] != dr.as_str()),
        "{q:#}"
    );
    let v = mcp_answer(&mut st, &batch, "approve").expect("an accepted card lets it land");
    assert_eq!(v["act"], "approve", "{v}");

    // through the binary
    let srv = Served::new();
    let (pref, dcard) = with_rdy(&srv, |r| {
        let s = spec(r);
        let p = deferred(r, &s);
        let dr = defer_ref(&p.r#ref);
        let c = queue_at(r, CARD_DEFAULT_TOKENS)
            .items
            .into_iter()
            .find(|i| i.r#ref == dr)
            .expect("the deferral card");
        (p.r#ref, c)
    });
    let before = with_rdy(&srv, |r| log_end(r));
    let check = |(ok, e): (bool, Value)| {
        assert!(!ok, "{e}");
        assert_eq!(e["code"], "E_DEFERRAL_OPEN", "{e}");
        assert_eq!(
            e["message"],
            format!("2 deferrals of {pref} have no answer"),
            "{e}"
        );
        assert_eq!(
            e["fix"],
            format!("answer the deferral card first: defer:{pref}"),
            "{e}"
        );
        assert_eq!(
            e["corrected"]["deferral"]["ref"],
            dcard.r#ref.as_str(),
            "{e}"
        );
        assert_eq!(
            e["corrected"]["deferral"]["card_hash"],
            dcard.card_hash.as_str(),
            "{e}"
        );
    };
    check(srv.json(&["--json", "approve", &pref]));
    check(srv.json(&["--json", "answer", &pref, "approve"]));
    check(srv.json_as(
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
    ));
    assert_eq!(with_rdy(&srv, |r| log_end(r)), before, "nothing is written");
    let (ok, v) = srv.json(&["--json", "answer", &dcard.r#ref, "o1"]);
    assert!(ok, "{v}");
    assert_eq!(
        (v["act"].as_str(), v["assurance"].as_str()),
        (Some("accept_deferrals"), Some("local")),
        "{v}"
    );
    let (ok, v) = srv.json(&["--json", "approve", &pref]);
    assert!(ok, "{v}");
    with_rdy(&srv, |r| r.fsck().expect("fsck"));
}

/// The exception hold comes first; once each exception is confirmed, the deferral hold, with its
/// exact text and the card in `corrected.deferral`.
#[test]
fn cp12_exception_hold_precedes_deferral_hold() {
    let (_d, mut r) = project();
    let s = spec(&mut r);
    edit(&mut r, "w", &s.a, "A holds 60 days.");
    // a decision no content applies: an exception beside the batch
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
    let p = propose(
        &mut r,
        "w",
        &[defer(
            &pi(&s.a, &s.b, PlanKind::Confirm),
            "b waits for the review",
        )],
    );
    let e = land_err(&mut r, &p.r#ref);
    assert_eq!(e.code, E_EXCEPTION_OPEN, "{e}");
    assert_eq!(
        e.message,
        format!("1 exceptions of {} have no answer", p.r#ref)
    );
    assert!(
        e.corrected.as_ref().unwrap().get("deferral").is_none(),
        "{e}"
    );
    confirm_exceptions(&mut r, &approver(), &p.r#ref);
    let dr = defer_ref(&p.r#ref);
    let card = queue_at(&r, CARD_DEFAULT_TOKENS)
        .items
        .into_iter()
        .find(|i| i.r#ref == dr)
        .expect("the deferral card");
    let e = land_err(&mut r, &p.r#ref);
    deferral_open(&e, &p.r#ref, 1, &card);
}

/// A refused card makes the version not ready, with the issue the landing gives too; an answer
/// binds one version: a new version needs a new answer, and a stale ref is `E_ITEM_MOVED`.
#[test]
fn cp12_refusal_is_not_ready_and_new_version_needs_new_answer() {
    let (_d, mut r) = project();
    let s = spec(&mut r);
    let p1 = deferred(&mut r, &s);
    let d1 = defer_ref(&p1.r#ref);
    let v = answer_local(&mut r, &d1, "o1").unwrap();
    assert_eq!(v["act"], "accept_deferrals");
    // a new version: the old acceptance does not carry over
    edit(&mut r, "w", &s.z, "Z stands still.");
    let p2 = propose(&mut r, "w", &[]);
    assert_ne!(p1.r#ref, p2.r#ref);
    let d2 = defer_ref(&p2.r#ref);
    let card2 = queue_at(&r, CARD_DEFAULT_TOKENS)
        .items
        .into_iter()
        .find(|i| i.r#ref == d2)
        .expect("the new version's card");
    let e = land_err(&mut r, &p2.r#ref);
    deferral_open(&e, &p2.r#ref, 2, &card2);
    let before = log_end(&r);
    let e = answer_local(&mut r, &d1, "o1").unwrap_err();
    assert_eq!(e.code, E_ITEM_MOVED, "{e}");
    assert_eq!(e.message, format!("{d1} changed since it was shown"));
    assert_eq!(e.corrected.as_ref().unwrap()["ref"], d2.as_str());
    assert_eq!(log_end(&r), before);
    // a refusal: not ready, and the landing refuses with the same issue
    let v = answer_local(&mut r, &d2, "o2").unwrap();
    assert_eq!(v["act"], "refuse_deferrals");
    let q = queue(&r);
    assert!(
        q.items.iter().all(|i| !i.r#ref.ends_with(&p2.r#ref)),
        "{:#?}",
        q.items
    );
    let nr = q
        .not_ready
        .iter()
        .find(|n| n.r#ref == p2.r#ref)
        .expect("not ready");
    assert_eq!(nr.issues.len(), 1, "{:#?}", nr.issues);
    assert_eq!(nr.issues[0].code, E_VALIDATION);
    assert_eq!(
        nr.issues[0].message,
        format!("the deferrals of {} were refused", p2.r#ref)
    );
    assert_eq!(nr.issues[0].fix, "close the items and propose again");
    let e = land_err(&mut r, &p2.r#ref);
    not_ready(&e, &p2.r#ref, &nr.issues);
    // the next version is unanswered again
    edit(&mut r, "w", &s.z, "Z stands quite still.");
    let p3 = propose(&mut r, "w", &[]);
    let e = land_err(&mut r, &p3.r#ref);
    assert_eq!(e.code, E_DEFERRAL_OPEN, "{e}");
}

/// A code item's deferral needs no acceptance: a version whose only deferral is a code item
/// has no deferral card and lands, its sealed code item deferred with no answer pointer.
#[test]
fn cp12_code_deferrals_never_hold_a_landing() {
    let (d, mut r) = project();
    let repo = linked_repo(d.path());
    let s = spec(&mut r);
    commit(
        repo.path(),
        "f.rs",
        &format!("start\n\nImplements: {}", s.z),
    );
    edit(&mut r, "w", &s.z, "Z stands still.");
    let code = pi(&s.z, "app:f.rs", PlanKind::UpdateImplementation);
    let p = propose(&mut r, "w", &[defer(&code, "the code follows next week")]);
    let q = queue(&r);
    assert!(q.items.iter().any(|i| i.r#ref == p.r#ref), "ready: {q:#?}");
    assert!(
        q.items.iter().all(|i| i.kind != ItemKind::Defer),
        "{:#?}",
        q.items
    );
    let batch = item_of(&r, &p.r#ref).unwrap();
    assert_eq!(fact(&batch, "Change plan"), "0 open, 0 closed, 1 deferred");
    let l = h(&r.land(&approver(), &p.id).expect("lands"));
    let st = r.read();
    let sh = st
        .reader()
        .plan_of_landing(&l, log_end(&r))
        .unwrap()
        .unwrap();
    let sealed = st.reader().plan(&sh).unwrap();
    let items = &sealed.seal.as_ref().unwrap().items;
    assert_eq!(items.len(), 1, "{items:#?}");
    assert_eq!(
        (items[0].kind, items[0].outcome, items[0].answer),
        (PlanKind::UpdateImplementation, PlanOutcome::Deferred, None)
    );
}

/// D-GV465: a version that re-affirms a `one_way` decision stands beside its batch with a
/// `reaffirm_one_way` exception, recommended with the reason this version wrote; a rejection
/// makes the version not ready without touching an unrelated new decision, and an unanswered
/// item is `E_EXCEPTION_OPEN`. A new re-affirmation of the decision binds new answers.
#[test]
fn cp12_one_way_reaffirm_has_its_own_exception() {
    let (_d, mut r) = project();
    let s = spec(&mut r);
    let one = decision(&mut r, "d1", &s.s, "Never drop A", &[&s.a, &s.c], "one_way");
    edit(&mut r, "x", &s.a, "A holds 60 days.");
    edit(&mut r, "x", &s.c, "C stands firm.");
    rdy_qa::fixtures::land(&mut r, "x").unwrap();
    // the version: two re-affirmations with their own reasons, and a new decision that content
    // applies
    write(
        &mut r,
        "w",
        json!([{"op": "reaffirm", "id": one, "targets": [s.a], "reason": "A still fits."}]),
    )
    .unwrap();
    write(
        &mut r,
        "w",
        json!([{"op": "reaffirm", "id": one, "targets": [s.c], "reason": "C still fits."}]),
    )
    .unwrap();
    let new = r
        .decide(
            &writer(),
            "w",
            DecisionInput {
                parent: s.s.clone(),
                statement: "Keep Z".into(),
                affects: vec![s.z.clone()],
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let p = propose(&mut r, "w", &[]);
    let exceptions = |r: &Rdy, pref: &str| -> Vec<Item> {
        queue(r)
            .items
            .into_iter()
            .filter(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(pref))
            .collect()
    };
    let ex = exceptions(&r, &p.r#ref);
    assert_eq!(ex.len(), 1, "{ex:#?}");
    let ex = ex[0].clone();
    let in_view = |r: &Rdy| {
        r.read()
            .materialized(&rdy_engine::Scope::View("w".into()))
            .unwrap()
            .get(&id(&one))
            .unwrap()
            .version_hash()
    };
    assert_eq!(
        ex.r#ref,
        format!("{one}@{}", &in_view(&r).to_string()[..12])
    );
    assert_eq!(ex.exception, Some(ExceptionKind::ReaffirmOneWay));
    assert_eq!(ex.title, "Never drop A");
    let mut causes = [s.a.clone(), s.c.clone()];
    causes.sort();
    assert_eq!(fact(&ex, "Re-affirms"), causes.join(", "));
    assert!(
        ex.card.contains(&format!(
            "Beside {}, on its own: it re-affirms a one_way decision.",
            p.r#ref
        )),
        "{}",
        ex.card
    );
    let opts: Vec<(&str, &str)> = ex
        .options
        .iter()
        .map(|o| (o.id.as_str(), o.label.as_str()))
        .collect();
    assert_eq!(
        opts,
        vec![
            ("confirm", "It still holds"),
            ("reject", "Drop the re-affirmation"),
            ("later", "Later")
        ]
    );
    let mut reasons = [
        (s.a.clone(), "A still fits."),
        (s.c.clone(), "C still fits."),
    ];
    reasons.sort();
    let want = reasons
        .iter()
        .map(|(c, r)| format!("{c}: {r}"))
        .collect::<Vec<_>>()
        .join("; ");
    assert_eq!(
        ex.options[0].recommended.as_ref().map(|r| r.reason()),
        Some(want.as_str())
    );
    assert!(ex.options[1].recommended.is_none());
    // the new decision is a member of the batch, not an exception
    let batch = item_of(&r, &p.r#ref).unwrap();
    assert!(
        batch.members.iter().any(|m| m.starts_with(&new)),
        "{:#?}",
        batch.members
    );
    assert_eq!(fact(&batch, "Exceptions"), "1");
    assert_eq!(
        fact(&batch, "Re-affirms"),
        "none",
        "a one_way decision has its own card"
    );
    // unanswered: E_EXCEPTION_OPEN
    let e = land_err(&mut r, &p.r#ref);
    assert_eq!(e.code, E_EXCEPTION_OPEN, "{e}");
    assert_eq!(
        e.fix,
        format!("answer each exception card first: {}", ex.r#ref)
    );
    // a rejection
    let v = answer_local(&mut r, &ex.r#ref, "reject").unwrap();
    assert_eq!(v["act"], "reject_reaffirm", "{v}");
    assert_eq!(
        answer_record(&r, &ex.r#ref).1.act,
        AnswerAct::RejectReaffirm
    );
    let q = queue(&r);
    let nr = q
        .not_ready
        .iter()
        .find(|n| n.r#ref == p.r#ref)
        .expect("not ready");
    assert_eq!(
        nr.issues.len(),
        1,
        "only the re-affirmation: {:#?}",
        nr.issues
    );
    assert_eq!(
        (
            nr.issues[0].code,
            nr.issues[0].message.as_str(),
            nr.issues[0].fix.as_str()
        ),
        (
            E_VALIDATION,
            format!("{one}'s re-affirmation was rejected").as_str(),
            "drop the re-affirmation and propose again"
        )
    );
    assert_eq!(nr.issues[0].block, Some(id(&one)));
    let e = land_err(&mut r, &p.r#ref);
    not_ready(&e, &p.r#ref, &nr.issues);
    // a new re-affirmation of the decision, in another view: its own version binds new answers
    write(
        &mut r,
        "w2",
        json!([{"op": "reaffirm", "id": one, "reason": "It still holds."}]),
    )
    .unwrap();
    let p2 = propose(&mut r, "w2", &[]);
    let ex2 = exceptions(&r, &p2.r#ref);
    assert_eq!(ex2.len(), 1, "{ex2:#?}");
    assert_ne!(ex2[0].r#ref, ex.r#ref);
    assert_eq!(fact(&ex2[0], "Re-affirms"), causes.join(", "));
    assert_eq!(
        ex2[0].options[0].recommended.as_ref().map(|r| r.reason()),
        Some("It still holds."),
        "one reason for every cause"
    );
    let e = land_err(&mut r, &p2.r#ref);
    assert_eq!(e.code, E_EXCEPTION_OPEN, "{e}");
    confirm_exceptions(&mut r, &approver(), &p2.r#ref);
    r.land_with(&approver(), &p2.r#ref, LandOptions::default())
        .expect("the confirmed re-affirmation lands");
}

/// O10: the deferral card's hash binds every item, also those its text leaves out. A card
/// whose omitted member moves is another card, and an answer bound to it is `E_ITEM_MOVED`.
#[test]
fn cp12_card_binds_deferred_items_beyond_text_budget() {
    let (_d, mut r) = project();
    let s = spec(&mut r);
    // fifty claims build on a; each defers with a long reason
    let ops: Vec<Value> = (0..50)
        .map(|n| {
            json!({"op": "create", "type": "claim", "id": format!("$r{n}"), "parent": s.s,
                   "body": format!("R{n} builds on {{{{{}}}}}.", s.a)})
        })
        .collect();
    let made = write(&mut r, "seed2", Value::Array(ops)).unwrap();
    rdy_qa::fixtures::land(&mut r, "seed2").unwrap();
    edit(&mut r, "w", &s.a, "A holds 60 days.");
    let answers: Vec<PlanAnswerInput> = made
        .created
        .values()
        .enumerate()
        .map(|(n, c)| {
            defer(
                &pi(&s.a, c, PlanKind::Confirm),
                &format!("{n} {}", "waits ".repeat(330)),
            )
        })
        .collect();
    assert_eq!(answers.len(), 50);
    let p = propose(&mut r, "w", &answers);
    let dr = defer_ref(&p.r#ref);
    let card = item_of(&r, &dr).expect("the deferral card");
    assert!(card.card.contains("facts cut;"), "the text is cut");
    let Some(PlanCardBinding::Deferrals { items, .. }) = &card.plan_binding else {
        panic!("a binding")
    };
    assert_eq!(items.len(), 50, "the binding holds every item");
    // the last item is not in the text: moving it keeps the text and changes the hash
    let last = items.last().unwrap().clone();
    assert!(!card.card.contains(&format!("{} (after", last.subject)));
    let mut moved = card.clone();
    if let Some(PlanCardBinding::Deferrals { items, .. }) = &mut moved.plan_binding {
        let l = items.last_mut().unwrap();
        l.answer.reason = Some("moved".into());
    }
    assert_eq!(
        render(&moved, CARD_MAX_TOKENS),
        render(&card, CARD_MAX_TOKENS)
    );
    let moved_hash = card_hash(&moved);
    assert_ne!(moved_hash.to_string(), card.card_hash);
    let before = log_end(&r);
    let e = r
        .answer(
            &approver(),
            PERSON,
            AnswerRequest {
                r#ref: dr.clone(),
                option: "o1".into(),
                card_hash: Some(moved_hash.to_string()),
                ..AnswerRequest::default()
            },
        )
        .unwrap_err();
    assert_eq!(e.code, E_ITEM_MOVED, "{e}");
    assert_eq!(e.corrected, Some(card_json(&card)));
    assert_eq!(log_end(&r), before);
    // the shown hash binds
    let v = r
        .answer(
            &approver(),
            PERSON,
            AnswerRequest {
                r#ref: dr.clone(),
                option: "o1".into(),
                card_hash: Some(card.card_hash.clone()),
                ..AnswerRequest::default()
            },
        )
        .unwrap();
    assert_eq!(v.act, "accept_deferrals");
}

/// The batch card's facts, in order, with Re-affirms after Supersedes and Change plan after
/// Consequences; the text shows the first 20 re-affirmed decisions. The approve reason is one of
/// three closed strings: no companion hold, exceptions only, a deferral card (which wins).
#[test]
fn cp12_batch_facts_and_recommendation_reasons_are_exact() {
    let (_d, mut r) = project();
    let s = spec(&mut r);
    // a plain version: nothing beside it
    edit(&mut r, "plain", &s.z, "Z stands still.");
    let plain = propose(&mut r, "plain", &[]);
    let i = item_of(&r, &plain.r#ref).unwrap();
    let names: Vec<String> = i.facts.iter().map(|f| f.name.to_string()).collect();
    assert_eq!(
        names,
        [
            "Changes",
            "Justification",
            "Decisions",
            "Supersedes",
            "Re-affirms",
            "Reversibility",
            "Consequences",
            "Change plan",
            "Tensions",
            "Coverage",
            "Confidence",
            "Base",
            "Exceptions",
            "Policy",
        ]
    );
    assert_eq!(fact(&i, "Re-affirms"), "none");
    assert_eq!(fact(&i, "Change plan"), "0 open, 0 closed, 0 deferred");
    assert_eq!(
        i.options[0].recommended.as_ref().unwrap().reason(),
        APPROVE_REASON
    );
    assert_eq!(APPROVE_REASON, "its checks pass and nothing blocks it");
    // twenty-one decisions on c, re-affirmed in one write after c changes
    let mut ds: Vec<String> = (0..21)
        .map(|n| {
            decision(
                &mut r,
                &format!("d{n}"),
                &s.s,
                &format!("Build on C {n}"),
                &[&s.c],
                "two_way",
            )
        })
        .collect();
    ds.sort();
    edit(&mut r, "x", &s.c, "C stands firm.");
    rdy_qa::fixtures::land(&mut r, "x").unwrap();
    write(
        &mut r,
        "ra",
        json!([{"op": "reaffirm", "because": s.c, "reason": "C still fits."}]),
    )
    .unwrap();
    // an unapplied decision: an exception beside the batch
    r.decide(
        &writer(),
        "ra",
        DecisionInput {
            parent: s.s.clone(),
            statement: "Hold C".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let ra = propose(&mut r, "ra", &[]);
    let i = item_of(&r, &ra.r#ref).unwrap();
    let each: Vec<String> = ds.iter().map(|d| format!("{d} (after {})", s.c)).collect();
    assert_eq!(
        fact(&i, "Re-affirms"),
        each.join("; "),
        "JSON carries every member"
    );
    let shown = format!("{}; and 1 more", each[..20].join("; "));
    let line = i
        .card
        .lines()
        .find(|l| l.trim_start().starts_with("Re-affirms"))
        .expect("the Re-affirms line");
    assert_eq!(
        line.trim_start().trim_start_matches("Re-affirms").trim(),
        shown
    );
    let reasons = |i: &Item| {
        i.options[0]
            .recommended
            .as_ref()
            .unwrap()
            .reason()
            .to_string()
    };
    assert_eq!(reasons(&i), APPROVE_REASON_BESIDE);
    assert_eq!(
        APPROVE_REASON_BESIDE,
        "its checks pass, and it lands after you confirm each decision beside it"
    );
    // a deferral card, with and without an exception: the deferral reason
    let deferral = "its checks pass, and it lands after you answer each card beside it";
    let p = deferred(&mut r, &s);
    let i = item_of(&r, &p.r#ref).unwrap();
    assert_eq!(reasons(&i), deferral);
    assert_eq!(fact(&i, "Exceptions"), "none");
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
    let p2 = propose(&mut r, "w", &[]);
    let i = item_of(&r, &p2.r#ref).unwrap();
    assert_eq!(reasons(&i), deferral);
    assert_eq!(fact(&i, "Exceptions"), "1");
    let counts = fact(&i, "Change plan");
    assert!(counts.ends_with(", 2 deferred"), "{counts}");
}

/// The seal of a landing points each deferred non-code item to this version's AcceptDeferrals
/// ANSWER, also when the approval writes a final acceptance version; an earlier version's
/// acceptance and a KeepSuspect answer never supply it, and a code item has none.
#[test]
fn cp12_seal_points_to_this_versions_acceptance() {
    let (d, mut r) = project();
    let repo = linked_repo(d.path());
    let s = spec(&mut r);
    commit(
        repo.path(),
        "f.rs",
        &format!("start\n\nImplements: {}", s.a),
    );
    let p1 = deferred(&mut r, &s);
    answer_local(&mut r, &defer_ref(&p1.r#ref), "o1").unwrap();
    // a new version with a decision (the approval writes a final version) and a code deferral
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
    let code = pi(&s.a, "app:f.rs", PlanKind::UpdateImplementation);
    let p2 = propose(&mut r, "w", &[defer(&code, "the code follows")]);
    confirm_exceptions(&mut r, &approver(), &p2.r#ref);
    // a KeepSuspect answer of a re-affirmation card is no acceptance of this card
    let st = r.read();
    let canon = st.materialized(&rdy_engine::Scope::Canon).unwrap();
    let a_version = canon.get(&id(&s.a)).unwrap().version_hash();
    let b_version = canon.get(&id(&s.b)).unwrap().version_hash();
    drop(canon);
    drop(st);
    let keep = Answer {
        item: format!("reaffirm:{}@{}", s.a, &a_version.to_string()[..12]),
        act: AnswerAct::KeepSuspect,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets: vec![b_version],
        person: writer().provenance(),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: Some("they still hold".into()),
    };
    r.store_mut().put_answer(&keep).unwrap();
    r.invalidate();
    let e = land_err(&mut r, &p2.r#ref);
    assert_eq!(e.code, E_DEFERRAL_OPEN, "{e}");
    let accept = answer_local(&mut r, &defer_ref(&p2.r#ref), "o1").unwrap();
    let accepted = h(accept["answer"].as_str().unwrap());
    let l = h(&r
        .land_with(&approver(), &p2.r#ref, LandOptions::default())
        .expect("lands")
        .landing);
    let st = r.read();
    let landing = st.reader().landing(&l).unwrap();
    assert_ne!(landing.proposal, h(&p2.id), "a final acceptance version");
    let sh = st
        .reader()
        .plan_of_landing(&l, log_end(&r))
        .unwrap()
        .unwrap();
    let sealed = st.reader().plan(&sh).unwrap();
    let x = sealed.seal.as_ref().unwrap();
    assert_eq!(x.version, landing.proposal);
    let mut deferred_docs = 0;
    for i in &x.items {
        match (i.kind, i.outcome) {
            (PlanKind::UpdateImplementation, o) => {
                assert_eq!(o, PlanOutcome::Deferred, "{i:#?}");
                assert_eq!(i.answer, None, "a code item has no pointer");
            }
            (_, PlanOutcome::Deferred) => {
                deferred_docs += 1;
                assert_eq!(i.answer, Some(accepted), "{i:#?}");
            }
            _ => assert_eq!(i.answer, None, "{i:#?}"),
        }
    }
    assert_eq!(deferred_docs, 2);
    drop(st);
    r.fsck().unwrap();
}

/// O19, p-AEYTC, O10: a version proposed before store 17 has no bound plan and no stored
/// assessment, so it is not ready: the queue lists it with the O10 issue only, no approve card,
/// no deferral card and no deferral hold. Proposed again, the version binds its view's chain, so
/// its deferral card stands and its approval waits for the person's answer to it.
#[test]
fn cp12_unbound_proposal_is_not_ready_under_o10() {
    let (_d, mut r) = project();
    let s = spec(&mut r);
    edit(&mut r, "w", &s.a, "A holds 60 days.");
    r.answer_plan(
        &writer(),
        "w",
        &[defer(&pi(&s.a, &s.b, PlanKind::Confirm), "b waits")],
    )
    .unwrap();
    let by = writer().provenance();
    let changes = r.store().included("w").unwrap().as_ref().clone();
    let v = r
        .store_mut()
        .propose_changes("w", "w", Some(TEST_SUMMARY), by, changes, None)
        .unwrap();
    r.invalidate();
    let pref = format!("P-{}@1", v.short());
    let q = queue(&r);
    let n = q
        .not_ready
        .iter()
        .find(|n| n.r#ref == pref)
        .unwrap_or_else(|| panic!("{pref} is not ready: {:#?}", q.not_ready));
    assert_eq!(n.issues.len(), 1, "the O10 issue only: {:#?}", n.issues);
    assert_eq!(n.issues[0].code, E_VALIDATION);
    assert_eq!(
        n.issues[0].message,
        format!("{pref} has no stored assessment")
    );
    assert_eq!(
        n.issues[0].fix,
        "propose it again: rdy_propose / rdy propose"
    );
    assert!(q.items.iter().all(|i| i.r#ref != pref), "no approve card");
    assert!(
        q.items.iter().all(|i| i.kind != ItemKind::Defer),
        "no deferral card"
    );
    // O10: its approval refuses with the same issue, before any hold
    let e = r
        .land_with(&approver(), &pref, LandOptions::default())
        .expect_err("not ready under O10");
    assert_eq!(e.code, E_PROPOSAL_NOT_READY, "{e:#?}");
    assert_eq!(e.issues, n.issues, "the O10 issue only");
    // proposed again: an assessed version, ready, with the chain's deferral card beside it
    let p = propose(
        &mut r,
        "w",
        &[defer(&pi(&s.a, &s.b, PlanKind::Confirm), "b waits")],
    );
    let q = queue(&r);
    assert!(
        q.not_ready.iter().all(|n| n.r#ref != p.r#ref),
        "{:#?}",
        q.not_ready
    );
    assert!(item_of(&r, &p.r#ref).is_some(), "the new version is ready");
    assert!(
        q.items.iter().any(|i| i.kind == ItemKind::Defer),
        "the deferral card of the new version"
    );
    let e = r
        .land_with(&approver(), &p.id, LandOptions::default())
        .expect_err("the deferral hold");
    assert_eq!(e.code, E_DEFERRAL_OPEN, "{e}");
}

/// A list of recommended answers that names a deferral card is refused before any answer: the
/// card has no recommended option. Nothing is written, not even the exception before it.
#[test]
fn cp12_all_recommended_refuses_before_any_answer() {
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
    let p = propose(
        &mut r,
        "w",
        &[defer(&pi(&s.a, &s.b, PlanKind::Confirm), "b waits")],
    );
    let ex: Vec<String> = queue(&r)
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Confirm)
        .map(|i| i.r#ref.clone())
        .collect();
    assert_eq!(ex.len(), 1);
    let dr = defer_ref(&p.r#ref);
    let before = log_end(&r);
    let refs = vec![ex[0].clone(), dr.clone(), p.r#ref.clone()];
    let e = r
        .answer_all_recommended(&approver(), PERSON, &refs, &[], RelayInput::default())
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert_eq!(e.message, format!("{dr} has no recommended option"));
    assert_eq!(e.fix, "answer it on its own");
    assert_eq!(log_end(&r), before, "zero new frames");
    assert!(r.read().reader().answer_of(&ex[0]).unwrap().is_none());
}

/// Fix round 1 (finding 1), D-GV465: a re-affirmation is read from the complete block delta. A
/// second change of the same cause, re-affirmed with the same reason, writes the same
/// `reaffirmed` entry but moves the cause's pin: it is a new re-affirmation of the `one_way`
/// decision, so it stands beside its batch and the landing waits for its confirm.
#[test]
fn cp12_repeated_reaffirm_reason_still_needs_confirmation() {
    let (_d, mut r) = project();
    let s = spec(&mut r);
    let one = decision(&mut r, "d1", &s.s, "Never drop A", &[&s.a], "one_way");
    let reaffirm = |r: &mut Rdy, view: &str| -> ProposalInfo {
        write(
            r,
            view,
            json!([{"op": "reaffirm", "id": one, "targets": [s.a], "reason": "A still fits."}]),
        )
        .unwrap_or_else(|e| panic!("reaffirm in {view}: {e}"));
        propose(r, view, &[])
    };
    let exceptions = |r: &Rdy, pref: &str| -> Vec<Item> {
        queue(r)
            .items
            .into_iter()
            .filter(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(pref))
            .collect()
    };
    // the first change of a, re-affirmed, confirmed and landed
    edit(&mut r, "x1", &s.a, "A holds 60 days.");
    rdy_qa::fixtures::land(&mut r, "x1").unwrap();
    let p1 = reaffirm(&mut r, "w1");
    let ex1 = exceptions(&r, &p1.r#ref);
    assert_eq!(ex1.len(), 1, "{ex1:#?}");
    assert_eq!(ex1[0].exception, Some(ExceptionKind::ReaffirmOneWay));
    confirm_exceptions(&mut r, &approver(), &p1.r#ref);
    r.land_with(&approver(), &p1.r#ref, LandOptions::default())
        .expect("the first re-affirmation lands");
    // the second change of a, re-affirmed with the same reason
    edit(&mut r, "x2", &s.a, "A holds 90 days.");
    rdy_qa::fixtures::land(&mut r, "x2").unwrap();
    let p2 = reaffirm(&mut r, "w2");
    let ex2 = exceptions(&r, &p2.r#ref);
    assert_eq!(ex2.len(), 1, "a new re-affirmation: {ex2:#?}");
    assert_eq!(ex2[0].exception, Some(ExceptionKind::ReaffirmOneWay));
    assert_ne!(ex2[0].r#ref, ex1[0].r#ref);
    assert_eq!(fact(&ex2[0], "Re-affirms"), s.a);
    assert_eq!(
        ex2[0].options[0].recommended.as_ref().map(|r| r.reason()),
        Some("A still fits.")
    );
    let batch = item_of(&r, &p2.r#ref).unwrap();
    assert_eq!(fact(&batch, "Exceptions"), "1");
    let e = land_err(&mut r, &p2.r#ref);
    assert_eq!(e.code, E_EXCEPTION_OPEN, "{e}");
    assert_eq!(
        e.fix,
        format!("answer each exception card first: {}", ex2[0].r#ref)
    );
    confirm_exceptions(&mut r, &approver(), &p2.r#ref);
    r.land_with(&approver(), &p2.r#ref, LandOptions::default())
        .expect("the confirmed re-affirmation lands");
}

/// Fix round 1 (finding 2), D-AAQ9Z: a refused deferral card still takes a later answer. The
/// refusal makes the version not ready, but an explicit `defer:P-x@n` of the current version
/// binds its frozen card outside the ready queue, with the card's full hash; the latest valid
/// answer wins, so an acceptance replaces the refusal and the version lands. A refusal can
/// replace an acceptance the same way.
#[test]
fn cp12_refused_deferral_takes_a_later_acceptance() {
    let (_d, mut r) = project();
    let s = spec(&mut r);
    let p = deferred(&mut r, &s);
    let dr = defer_ref(&p.r#ref);
    let card = item_of(&r, &dr).expect("the deferral card");
    let answer = |r: &mut Rdy, option: &str| {
        r.answer(
            &approver(),
            PERSON,
            AnswerRequest {
                r#ref: dr.clone(),
                option: option.into(),
                card_hash: Some(card.card_hash.clone()),
                ..AnswerRequest::default()
            },
        )
    };
    let v = answer(&mut r, "o2").unwrap();
    assert_eq!(v.act, "refuse_deferrals");
    let q = queue(&r);
    assert!(q.not_ready.iter().any(|n| n.r#ref == p.r#ref), "{q:#?}");
    // the same unchanged card, accepted: the acceptance replaces the refusal
    let v = answer(&mut r, "o1").unwrap();
    assert_eq!(v.act, "accept_deferrals");
    let (h1, a) = answer_record(&r, &dr);
    assert_eq!(v.answer, Some(h1.to_string()));
    assert_eq!(a.act, AnswerAct::AcceptDeferrals);
    assert_eq!(a.card_hash, Some(h(&card.card_hash)));
    let q = queue(&r);
    assert!(q.not_ready.iter().all(|n| n.r#ref != p.r#ref), "{q:#?}");
    assert!(q.items.iter().any(|i| i.r#ref == p.r#ref), "ready again");
    assert!(q.items.iter().all(|i| i.r#ref != dr), "accepted: no card");
    // an accepted card still binds an explicit answer: a refusal replaces the acceptance
    let v = answer(&mut r, "o2").unwrap();
    assert_eq!(v.act, "refuse_deferrals");
    let nr = queue(&r)
        .not_ready
        .into_iter()
        .find(|n| n.r#ref == p.r#ref)
        .expect("refused again");
    let e = land_err(&mut r, &p.r#ref);
    not_ready(&e, &p.r#ref, &nr.issues);
    // a card hash that is not the card's is still E_ITEM_MOVED, with the fresh card
    let before = log_end(&r);
    let e = r
        .answer(
            &approver(),
            PERSON,
            AnswerRequest {
                r#ref: dr.clone(),
                option: "o1".into(),
                card_hash: Some(h1.to_string()),
                ..AnswerRequest::default()
            },
        )
        .unwrap_err();
    assert_eq!(e.code, E_ITEM_MOVED, "{e}");
    assert_eq!(e.corrected, Some(card_json(&card)));
    assert_eq!(log_end(&r), before);
    // accepted once more, it lands, and the seal names that acceptance
    let v = answer(&mut r, "o1").unwrap();
    let accepted = h(v.answer.as_deref().unwrap());
    let l = h(&r
        .land_with(&approver(), &p.r#ref, LandOptions::default())
        .expect("lands")
        .landing);
    let st = r.read();
    let sh = st
        .reader()
        .plan_of_landing(&l, log_end(&r))
        .unwrap()
        .unwrap();
    let sealed = st.reader().plan(&sh).unwrap();
    let deferred: Vec<_> = sealed
        .seal
        .as_ref()
        .unwrap()
        .items
        .iter()
        .filter(|i| i.outcome == PlanOutcome::Deferred)
        .collect();
    assert_eq!(deferred.len(), 2);
    assert!(deferred.iter().all(|i| i.answer == Some(accepted)));
    drop(st);
    // a landed version's card no longer binds
    let before = log_end(&r);
    let e = answer(&mut r, "o1").unwrap_err();
    assert_eq!(e.code, E_ITEM_MOVED, "{e}");
    assert_eq!(log_end(&r), before);
}

/// Fix round 1 (finding 3), p-T69HT: the deferral reason holds only while the deferral card has
/// no valid acceptance. Once it is accepted, the approve reason is the one of the holds that
/// remain: none, or the exceptions.
#[test]
fn cp12_approve_reason_after_deferral_acceptance() {
    let (_d, mut r) = project();
    let s = spec(&mut r);
    let reason = |r: &Rdy, pref: &str| {
        item_of(r, pref).unwrap().options[0]
            .recommended
            .as_ref()
            .unwrap()
            .reason()
            .to_string()
    };
    let deferral = "its checks pass, and it lands after you answer each card beside it";
    // no exception: nothing blocks it once the card is accepted
    let p = deferred(&mut r, &s);
    assert_eq!(reason(&r, &p.r#ref), deferral);
    let shown = item_of(&r, &p.r#ref).unwrap();
    answer_local(&mut r, &defer_ref(&p.r#ref), "o1").unwrap();
    assert_eq!(reason(&r, &p.r#ref), APPROVE_REASON);
    // the acceptance moved the approve card: an approval bound to the card shown before it is
    // E_PROPOSAL_MOVED, with the fresh card, and writes nothing
    let fresh = item_of(&r, &p.r#ref).unwrap();
    assert_ne!(fresh.card_hash, shown.card_hash);
    let before = log_end(&r);
    let e = r
        .answer(
            &approver(),
            PERSON,
            AnswerRequest {
                r#ref: p.r#ref.clone(),
                option: "approve".into(),
                card_hash: Some(shown.card_hash.clone()),
                ..AnswerRequest::default()
            },
        )
        .unwrap_err();
    assert_eq!(e.code, rdy_engine::E_PROPOSAL_MOVED, "{e}");
    assert_eq!(e.corrected, Some(card_json(&fresh)));
    assert_eq!(log_end(&r), before);
    // with an exception: the exception reason once the card is accepted
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
    let p2 = propose(&mut r, "w", &[]);
    assert_eq!(reason(&r, &p2.r#ref), deferral);
    answer_local(&mut r, &defer_ref(&p2.r#ref), "o1").unwrap();
    assert_eq!(reason(&r, &p2.r#ref), APPROVE_REASON_BESIDE);
    assert_eq!(fact(&item_of(&r, &p2.r#ref).unwrap(), "Exceptions"), "1");
}
