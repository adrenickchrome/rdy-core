//! rtc3 (FW-MH.rules, D-ZSRBN): a rule request reaches canon through the existing gate. The
//! person's words are the approval: the landing and any confirmation of the rule's own decision
//! record a text relay with no card, and every other relay keeps the card hash. Under
//! `rules_from_chat: card`, the call stops at the normal cards and the answer form answers them.
//! Readiness, freshness, policy roles, signing and the decision's other dependents all hold, at
//! every preparation of the approval. A repeat never lands twice.
use ed25519_dalek::Signer as _;
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::check::api::{ReviewRequest, run_review};
use rdy_engine::engine::{Engine, Local, with_writer_result};
use rdy_engine::pending::{CARD_MAX_TOKENS, PendingQuery};
use rdy_engine::rule_types::{Outcome, Reply, RuleRequest, parse_request};
use rdy_engine::rules::approval::{RuleCall, RuleResult, run_rule};
use rdy_engine::rules::{
    Destination, Prepared, PreparedState, RuleCaller, RuleFault, RuleMutation,
    with_rule_fault_for_tests,
};
use rdy_engine::semantic::api::run_review_submit;
use rdy_engine::semantic::submit::{CitationRequest, ReviewSubmitRequest};
use rdy_engine::signing_act::{SigningAct, run_signed_act};
use rdy_engine::signing_crypto::{AgentIdentity, SigningAgent, blob_fingerprint};
use rdy_engine::{
    E_BAD_INPUT, E_EXCEPTION_OPEN, E_INTERNAL, E_ITEM_MOVED, E_NO_AGENT, E_PERMISSION,
    E_PROPOSAL_MOVED, E_PROPOSAL_NOT_READY, E_SIGNING_KEY_MISSING, E_STALE_BASE, E_VALIDATION,
    E_VIEW_CLOSED, NoAgentCause, Rdy, RdyError, Scope, Session, W_SIGNING_FAILED, WriteRequest,
    no_agent,
};
use rdy_mcp::tools::{McpState, rdy_answer, rdy_session};
use rdy_model::{Answer, AnswerAct, Assurance, BlockId, EvidenceKind, Hash, Justification, Role};
use rdy_qa::fixtures::{approver, land, parse_test_ops, write, writer};
use rdy_qa::intake::{PERSON, verdict_on};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const ME: &str = "agent:a";
const BOSS: &str = "human:boss";
const RELAY_FAILED: &str = "rules_from_chat: relay failed";

// ---- a fixed in-memory key and a fake agent ----

const SEED: [u8; 32] = [9; 32];

fn ssh_string(bytes: &[u8]) -> Vec<u8> {
    let mut out = (bytes.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(bytes);
    out
}

fn key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&SEED)
}

fn blob() -> Vec<u8> {
    [
        ssh_string(b"ssh-ed25519"),
        ssh_string(&key().verifying_key().to_bytes()),
    ]
    .concat()
}

type Hook = Box<dyn FnMut(usize) + Send>;

/// An agent holding the one key. It refuses the first `refuse` sign requests, and runs `hook`
/// before it answers the first sign request (or every one, with `every`).
#[derive(Default)]
struct FakeAgent {
    signs: AtomicUsize,
    refuse: AtomicUsize,
    hook: Mutex<Option<Hook>>,
    every: bool,
}

impl FakeAgent {
    fn hooked(every: bool, hook: impl FnMut(usize) + Send + 'static) -> FakeAgent {
        FakeAgent {
            hook: Mutex::new(Some(Box::new(hook))),
            every,
            ..FakeAgent::default()
        }
    }

    fn refusing() -> FakeAgent {
        FakeAgent {
            refuse: AtomicUsize::new(usize::MAX),
            ..FakeAgent::default()
        }
    }

    fn signs(&self) -> usize {
        self.signs.load(Ordering::SeqCst)
    }
}

impl SigningAgent for FakeAgent {
    fn identities(&self) -> Result<Vec<AgentIdentity>, RdyError> {
        Ok(vec![AgentIdentity { public_key: blob() }])
    }

    fn sign(&self, _public_key: &[u8], payload: &[u8]) -> Result<Vec<u8>, RdyError> {
        let n = self.signs.fetch_add(1, Ordering::SeqCst);
        let hook = self.hook.lock().unwrap().take();
        if let Some(mut h) = hook {
            h(n);
            if self.every {
                *self.hook.lock().unwrap() = Some(h);
            }
        }
        if self.refuse.load(Ordering::SeqCst) > 0 {
            return Err(no_agent(NoAgentCause::Refused));
        }
        let raw = key().sign(payload).to_bytes();
        Ok([ssh_string(b"ssh-ed25519"), ssh_string(&raw)].concat())
    }
}

// ---- the project ----

struct Proj {
    dir: tempfile::TempDir,
    e: Arc<Local>,
}

fn project() -> Proj {
    let dir = tempfile::tempdir().expect("tempdir");
    let r = Rdy::init(dir.path(), "rdy-standard").expect("init");
    let p = Proj {
        dir,
        e: Arc::new(Local::new(r)),
    };
    p.config("never", false);
    p
}

fn on<T: Send + 'static>(e: &Local, f: impl FnOnce(&mut Rdy) -> T + Send + 'static) -> T {
    with_writer_result(e, move |r| Ok(f(r))).expect("write")
}

impl Proj {
    /// The signing settings: `sign`, and the machine key when `agent_key`.
    fn config(&self, sign: &str, agent_key: bool) {
        write_config(self.dir.path(), sign, agent_key);
    }

    fn landings(&self) -> usize {
        self.e.read().log_rows().unwrap().len()
    }

    fn head(&self) -> Option<Hash> {
        self.e.read().canon_head()
    }

    fn view_closed(&self, view: &str) -> bool {
        self.e.read().view_head(view).is_some_and(|h| h.closed)
    }

    fn close_reason(&self, view: &str) -> Option<String> {
        let st = self.e.read();
        let head = st.view_head(view)?;
        if !head.closed {
            return None;
        }
        let mut cur = Some(head.head);
        while let Some(h) = cur {
            let op = st.reader().view_op(&h).unwrap();
            if let rdy_store::view::ViewOpKind::Close { reason } = op.op {
                return Some(reason);
            }
            cur = op.prev;
        }
        None
    }

    /// The live status of decision `id` in canon is `Live`, and `rule` still rests on it.
    fn still_rests_on(&self, rule: &str, id: &str) -> bool {
        let st = self.e.read();
        let m = st.materialized(&Scope::Canon).unwrap();
        let g = st.graph(&Scope::Canon).unwrap();
        let decision = BlockId::parse(id).unwrap();
        matches!(g.status(&decision), Some(rdy_graph::DerivedStatus::Live))
            && m.get(&BlockId::parse(rule).unwrap()).is_some_and(|b| {
                b.links
                    .get("decided_by")
                    .is_some_and(|l| matches!(l.as_slice(), [one] if one.target == decision))
            })
    }

    /// The latest answer to the item that names decision `id`: its ref and its record.
    fn answer_on_decision(&self, id: &str) -> Option<(String, Answer)> {
        let found = self
            .e
            .read()
            .reader()
            .answers_with_prefix(&format!("answer/{id}@"))
            .unwrap();
        found
            .into_iter()
            .next()
            .map(|(name, _, a)| (name["answer/".len()..].to_string(), a))
    }

    fn answers(&self) -> usize {
        self.e
            .read()
            .reader()
            .answers_with_prefix("answer/")
            .unwrap()
            .len()
    }

    fn pending_refs(&self) -> Vec<String> {
        self.e
            .read()
            .pending(&PendingQuery {
                person: PERSON.into(),
                session_ref: None,
                max_tokens: CARD_MAX_TOKENS,
            })
            .unwrap()
            .items
            .into_iter()
            .map(|i| i.r#ref)
            .collect()
    }

    /// Closes the engine and opens the store again.
    fn reopen(self) -> Proj {
        let Proj { dir, e } = self;
        Arc::into_inner(e)
            .expect("one owner of the engine")
            .close()
            .expect("close");
        let r = Rdy::open(dir.path()).expect("reopen");
        Proj {
            dir,
            e: Arc::new(Local::new(r)),
        }
    }
}

fn write_config(dir: &Path, sign: &str, agent_key: bool) {
    let agent_key = if agent_key {
        format!("\"{}\"", blob_fingerprint(&blob()))
    } else {
        "null".to_string()
    };
    std::fs::write(
        dir.join(".rdy/local.yaml"),
        format!(
            "signing:\n  key: null\n  agent_key: {agent_key}\n  sign: {sign}\n  allowed_signers: null\n"
        ),
    )
    .expect("local config");
}

// ---- requests ----

fn me() -> Session {
    Session::agent(ME.into(), Some("r1".into()), Role::Writer).unwrap()
}

fn req(v: Value) -> RuleRequest {
    parse_request(&v).unwrap_or_else(|e| panic!("{v}: {e}"))
}

fn call_as(session: Session, d: Destination) -> RuleCall {
    RuleCall {
        session,
        person: PERSON.into(),
        surface: "mcp".into(),
        destination: d,
        relayed_by: None,
    }
}

fn call() -> RuleCall {
    call_as(me(), Destination::Project)
}

fn run_with(p: &Proj, agent: &FakeAgent, v: Value) -> Result<RuleResult, RdyError> {
    run_rule(&*p.e, agent, &call(), &req(v))
}

fn run(p: &Proj, v: Value) -> Result<RuleResult, RdyError> {
    run_with(p, &FakeAgent::default(), v)
}

fn add(text: &str) -> Value {
    json!({"scope": "project", "op": "add", "text": text, "why": "They asked.",
           "evidence": "yes, keep it", "question": "Save this?", "session_ref": "chat-1"})
}

fn edit(id: &str, text: &str) -> Value {
    json!({"scope": "project", "op": "edit", "id": id, "text": text, "why": "Wider.",
           "evidence": "change it", "session_ref": "chat-1"})
}

fn remove(id: &str) -> Value {
    json!({"scope": "project", "op": "remove", "id": id, "why": "No longer wanted.",
           "evidence": "drop it", "session_ref": "chat-1"})
}

fn answer(item: &str, option: &str, hash: &str) -> Value {
    json!({"scope": "project", "answer": {"ref": item, "option": option, "card_hash": hash,
           "evidence": "Yes, as the card says.", "question": "This card?"}})
}

/// The landing a landed reply holds.
fn landing_of(r: &Reply) -> &Value {
    match &r.outcome {
        Outcome::Landed { landing } => landing,
        other => panic!("not landed: {other:#?}"),
    }
}

/// `(view, proposal_ref, primary ref, card_hash, card, attached)` of a card stop.
type CardStop = (
    String,
    String,
    String,
    String,
    Value,
    Vec<rdy_engine::rule_types::CardEntry>,
);

fn card_of(r: &Reply) -> CardStop {
    match &r.outcome {
        Outcome::Card {
            view,
            proposal_ref,
            item_ref,
            card_hash,
            card,
            cards,
        } => (
            view.clone(),
            proposal_ref.clone(),
            item_ref.clone(),
            card_hash.clone(),
            card.clone(),
            cards.clone(),
        ),
        other => panic!("not a card stop: {other:#?}"),
    }
}

/// `(view, item ref, answer JSON, remaining cards)` of an answered reply.
fn answered_of(
    r: &Reply,
) -> (
    String,
    String,
    Value,
    Vec<rdy_engine::rule_types::CardEntry>,
) {
    match &r.outcome {
        Outcome::Answered {
            view,
            item_ref,
            answer,
            remaining_cards,
            ..
        } => (
            view.clone(),
            item_ref.clone(),
            answer.clone(),
            remaining_cards.clone(),
        ),
        other => panic!("not answered: {other:#?}"),
    }
}

fn landed_rule(p: &Proj, text: &str) -> Reply {
    let r = run(p, add(text)).unwrap_or_else(|e| panic!("add {text}: {e} {:?}", e.issues));
    landing_of(&r.reply);
    r.reply
}

/// The request through its draft and proposal only: the state a later approval finds.
fn prepare_only(p: &Proj, v: Value) -> Prepared {
    let q = req(v);
    let s = me();
    on(&p.e, move |r| {
        r.rule_prepare(
            &RuleCaller {
                session: &s,
                person: PERSON,
                surface: "mcp",
                destination: Destination::Project,
            },
            RuleMutation::of(&q).unwrap(),
        )
    })
    .unwrap_or_else(|e| panic!("prepare: {e}"))
}

fn proposed_ref(p: &Prepared) -> String {
    match &p.state {
        PreparedState::Proposed { reference, .. } => reference.clone(),
        other => panic!("not proposed: {other:?}"),
    }
}

/// Lands, as the person, a doc with the policy `fields` and the decision that adopts it.
fn land_policy(e: &Local, fields: Value) {
    on(e, move |r| {
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
                justification: Some(Justification::Implements(vec![
                    BlockId::parse(&w.created["$dec"]).unwrap(),
                ])),
                message: String::new(),
            },
        )
        .unwrap_or_else(|e| panic!("write the policy: {e}"));
        land(r, "pol").unwrap_or_else(|e| panic!("land the policy: {e}"));
    });
}

fn policy(extra: Value) -> Value {
    let mut base = json!({
        "key": "authority-policy",
        "agent_may_accept": [],
        "roles": {},
        "require": [],
        "owners": []
    });
    for (k, v) in extra.as_object().unwrap() {
        base[k] = v.clone();
    }
    base
}

fn card_policy() -> Value {
    policy(json!({"rules_from_chat": "card"}))
}

/// The policy that needs `BOSS` for every item.
fn boss_policy(extra: Value) -> Value {
    let mut p = policy(json!({
        "roles": {"approvers": [BOSS]},
        "require": [{"when": "any", "role": "approvers"}],
    }));
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    p
}

/// A new instruction that rests on `decision`, under doc `doc`, in the open view `view`.
fn dependent_in_view(e: &Local, view: &str, doc: &str, decision: &str) -> String {
    let (view, doc, decision) = (view.to_string(), doc.to_string(), decision.to_string());
    on(e, move |r| {
        write(
            r,
            &view,
            json!([{"op": "create", "type": "instruction", "id": "$i", "parent": doc,
                    "fields": {"applies_to": ["all"]}, "body": format!("Also rests, {view}."),
                    "links": {"decided_by": [{"target": decision}]}}]),
        )
        .unwrap_or_else(|e| panic!("write the dependent: {e}"))
        .created["$i"]
            .clone()
    })
}

fn dependent_in_canon(e: &Local, view: &str, doc: &str, decision: &str) -> String {
    let id = dependent_in_view(e, view, doc, decision);
    let view = view.to_string();
    on(e, move |r| {
        land(r, &view).unwrap_or_else(|e| panic!("land the dependent: {e}"))
    });
    id
}

/// The shared-decision refusal, naming `blocks`.
fn assert_shared(e: &RdyError, decision: &str, blocks: &[&str]) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_BAD_INPUT,
            format!("decision {decision} also governs other blocks").as_str(),
            "brief the returned blocks and use the normal decision workflow",
        ),
        "{e}"
    );
    let c = e.corrected.as_ref().expect("corrected");
    assert_eq!(c["state"], "normal_path");
    assert_eq!(c["blocks"], json!(blocks));
}

/// An independent reviewer's concern on the proposed decision `decision`: the review's hash.
fn concern_on(e: &dyn Engine, decision: &str) -> String {
    let q = run_review(
        e,
        &me(),
        PERSON,
        true,
        None,
        ReviewRequest::from_value(
            json!({"op": "request", "check": "decision_review", "subject": decision}),
        )
        .unwrap(),
    )
    .unwrap_or_else(|e| panic!("request: {e:?}"));
    let reviewer = Session::agent("agent:r".into(), Some("r".into()), Role::Reviewer).unwrap();
    let v = run_review_submit(
        e,
        &reviewer,
        ReviewSubmitRequest {
            request: q["request"].as_str().unwrap().to_string(),
            case_hash: q["case_hash"].as_str().unwrap().to_string(),
            verdict: "concern".into(),
            citations: vec![CitationRequest {
                block: decision.to_string(),
                note: "the meaning moved".into(),
            }],
            severity: None,
            reason: Some("it reads otherwise".into()),
        },
    )
    .unwrap_or_else(|e| panic!("submit: {e:?}"));
    v["review"].as_str().unwrap().to_string()
}

fn relay_words(evidence: &str) -> RelayInput {
    RelayInput {
        relayed_by: None,
        evidence: Some(evidence.into()),
        question: Some("Shall I?".into()),
        session_ref: Some("chat-7".into()),
        kind: None,
    }
}

// ---- tests ----

#[test]
fn rtc3_text_relay_has_exact_words_and_no_unseen_card() {
    let p = project();
    let (evidence, question) = ("  yes, keep it\n  exactly  ", "  Save this?  ");
    let text = "Run the tests\n\n  before you push.  ";
    let added = run(
        &p,
        json!({"scope": "project", "op": "add", "text": text, "why": "They asked.",
               "evidence": evidence, "question": question, "session_ref": " chat-1 "}),
    )
    .unwrap();
    let v = added.to_json();
    assert_eq!(v["scope"], "project");
    assert_eq!(v["rule"]["text"], text);
    assert_eq!(v["rule"]["retired"], false);
    assert!(v.get("state").is_none(), "a landing has no stop state: {v}");
    let landing = v["landing"].clone();
    assert_eq!(landing["assurance"], "relayed");
    assert_eq!(landing["relayed_by"], json!({"actor": ME, "run": "r1"}));
    assert_eq!(landing["signatures"], json!([]));
    let hash: Hash = landing["landing"].as_str().unwrap().parse().unwrap();

    // the landing record: the person approves, the agent relays, the exact words, no card
    let rec = p.e.read().reader().landing(&hash).unwrap();
    assert_eq!(rec.assurance, Assurance::Relayed);
    assert_eq!(rec.approved_by.actor, PERSON);
    let by = rec.relayed_by.as_ref().unwrap();
    assert_eq!((by.actor.as_str(), by.run.as_deref()), (ME, Some("r1")));
    let quote = rec.relay_evidence.as_ref().unwrap();
    assert_eq!(quote.quoted, evidence);
    assert_eq!(quote.question.as_deref(), Some(question));
    assert_eq!(quote.session_ref, " chat-1 ");
    let ev = rec.evidence.as_ref().unwrap();
    assert_eq!(ev.kind, EvidenceKind::Text);
    assert!(ev.options.is_empty(), "{ev:?}");
    assert_eq!((&ev.choice, &ev.card_hash), (&None, &None));
    let id = added.reply.rule.id.clone();
    let in_canon =
        p.e.read()
            .block(&Scope::Canon, &BlockId::parse(&id).unwrap())
            .unwrap();
    assert_eq!(in_canon.body, text, "the rule keeps the exact bytes");

    // an add confirms nothing
    assert_eq!(p.answers(), 0);

    // an edit's confirmation of its own supersede: the same words, no card
    let old = added.reply.decision.clone();
    let edited = run(&p, edit(&id, "Run the tests first.")).unwrap();
    landing_of(&edited.reply);
    let new = edited.reply.decision.clone();
    assert_ne!(new, old);
    let (item, a) = p.answer_on_decision(&new).expect("a confirmation");
    assert_eq!(a.item, item);
    assert_eq!(a.act, AnswerAct::Confirm);
    assert_eq!(a.option, None);
    assert_eq!(a.label, None);
    assert!(a.options.is_empty());
    assert_eq!(a.card_hash, None);
    assert_eq!(a.evidence_kind, Some(EvidenceKind::Text));
    assert_eq!(a.person.actor, PERSON);
    let by = a.relayed_by.as_ref().unwrap();
    assert_eq!((by.actor.as_str(), by.run.as_deref()), (ME, Some("r1")));
    let quote = a.relay_evidence.as_ref().unwrap();
    assert_eq!(quote.quoted, "change it");
    assert_eq!(quote.session_ref, "chat-1");
    assert_eq!(p.answers(), 1, "only its own decision");

    // a remove keeps the id and the prior words, retired, and revokes its reason
    let removed = run(&p, remove(&id)).unwrap();
    landing_of(&removed.reply);
    assert!(removed.reply.rule.retired);
    assert_eq!(removed.reply.rule.text, "Run the tests first.");
    assert_eq!(removed.reply.rule.id, id);
    let (_, a) = p
        .answer_on_decision(&removed.reply.decision)
        .expect("a confirmation");
    assert_eq!(a.evidence_kind, Some(EvidenceKind::Text));
    assert_eq!(a.relay_evidence.as_ref().unwrap().quoted, "drop it");
    let view = removed.reply.clone();
    assert!(
        p.e.read()
            .block(&Scope::Canon, &BlockId::parse(&view.rule.id).unwrap())
            .is_err(),
        "the rule retired"
    );
    on(&p.e, |r| r.fsck().expect("fsck"));
}

/// A guard (pre-flight correction 11): every other relay keeps the card hash.
#[test]
fn rtc3_other_relays_still_require_card_hash() {
    let p = project();
    land_policy(&p.e, card_policy());
    let stop = run(&p, add("Keep the hash.")).unwrap();
    let (_, r#ref, _, hash, _, _) = card_of(&stop.reply);
    let refuse = |e: RdyError| {
        assert_eq!(
            (e.code, e.message.as_str()),
            (
                E_BAD_INPUT,
                "a relayed answer names the card the person was shown"
            ),
            "{e}"
        );
    };
    let landings = p.landings();

    // the public engine answer, with text evidence too
    for kind in [None, Some(EvidenceKind::Card), Some(EvidenceKind::Text)] {
        let (item, relay) = (
            r#ref.clone(),
            RelayInput {
                kind,
                ..relay_words("Yes.")
            },
        );
        let e = on(&p.e, move |r| {
            r.answer(
                &me(),
                PERSON,
                AnswerRequest {
                    r#ref: item,
                    option: "approve".into(),
                    relay,
                    ..AnswerRequest::default()
                },
            )
        })
        .unwrap_err();
        refuse(e);
    }
    // the signed answer
    let e = run_signed_act(
        &*p.e,
        &FakeAgent::default(),
        &me(),
        PERSON,
        SigningAct::Answer(AnswerRequest {
            r#ref: r#ref.clone(),
            option: "approve".into(),
            relay: RelayInput {
                kind: Some(EvidenceKind::Text),
                ..relay_words("Yes.")
            },
            ..AnswerRequest::default()
        }),
    )
    .unwrap_err();
    refuse(e);
    // recommended answers
    let e = run_signed_act(
        &*p.e,
        &FakeAgent::default(),
        &me(),
        PERSON,
        SigningAct::Recommended {
            refs: vec![r#ref.clone()],
            card_hashes: vec![],
            relay: relay_words("Yes."),
        },
    )
    .unwrap_err();
    refuse(e);
    assert_eq!(p.landings(), landings, "no path landed");

    // the tool: a hash is required, and its absence reaches the same refusal
    let mut st = McpState::new(Arc::clone(&p.e) as Arc<dyn Engine>);
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": ME, "run": "r1", "role": "writer"})).unwrap(),
    )
    .unwrap();
    let e = rdy_answer(
        &mut st,
        serde_json::from_value(json!({
            "ref": r#ref, "option": "approve", "evidence": "Yes.", "question": "Shall I?",
        }))
        .unwrap(),
    )
    .unwrap_err();
    refuse(e);
    assert_eq!(p.landings(), landings);

    // the rule tool's answer form needs one too
    let e = parse_request(&json!({"scope": "project", "answer": {"ref": r#ref, "option": "approve", "evidence": "Yes."}}))
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");

    // the card's own hash still works
    let done = run(&p, answer(&r#ref, "approve", &hash)).unwrap();
    assert_eq!(landing_of(&done.reply)["assurance"], "relayed");
}

#[test]
fn rtc3_edit_and_remove_confirm_only_their_rule_decision() {
    let p = project();
    let first = landed_rule(&p, "First rule stands.");
    let other = landed_rule(&p, "Other rule stands.");

    // an unrelated exception, in another batch, stays unanswered
    let (other_decision, section_doc) = (other.decision.clone(), other.rule.ruleset.clone());
    let other_batch = on(&p.e, move |r| {
        let w = r
            .supersede(
                &writer(),
                "unrelated",
                &other_decision,
                None,
                rdy_engine::DecisionInput {
                    statement: "Replaces the other reason.".into(),
                    ..Default::default()
                },
            )
            .unwrap_or_else(|e| panic!("supersede: {e}"));
        write(
            r,
            "unrelated",
            json!([{"op": "create", "type": "instruction", "parent": section_doc,
                    "fields": {"applies_to": ["all"]}, "body": "Applies the replacement.",
                    "links": {"decided_by": [{"target": w.created["$decision"]}]}}]),
        )
        .unwrap_or_else(|e| panic!("write: {e}"));
        let p = r
            .propose(&writer(), "unrelated", "Unrelated", Some("Unrelated."))
            .unwrap_or_else(|e| panic!("propose: {e}"));
        (p.r#ref, w.created["$decision"].clone())
    });
    let (batch, unrelated) = other_batch;
    assert!(
        p.pending_refs().iter().any(|r| r.starts_with(&unrelated)),
        "{:?}",
        p.pending_refs()
    );

    // the edit's own supersede: confirmed, and it lands
    let edited = run(&p, edit(&first.rule.id, "First rule, wider.")).unwrap();
    landing_of(&edited.reply);
    assert!(p.answer_on_decision(&edited.reply.decision).is_some());
    assert!(
        p.answer_on_decision(&unrelated).is_none(),
        "the other batch's exception has no answer"
    );
    assert!(
        p.pending_refs().iter().any(|r| r.starts_with(&unrelated)),
        "and stays in the queue"
    );
    assert!(p.pending_refs().contains(&batch));

    // a tension beside the rule's own supersede is not this shortcut's to confirm
    let tense = landed_rule(&p, "Tense rule stands.");
    let bystander = landed_rule(&p, "Bystander rule stands.");
    let v = edit(&tense.rule.id, "Tense rule, wider.");
    let prepared = prepare_only(&p, v.clone());
    let (view, decision, against) = (
        prepared.view.clone(),
        BlockId::parse(&prepared.decision).unwrap(),
        BlockId::parse(&bystander.decision).unwrap(),
    );
    let scope = Scope::View(view.clone());
    on(&p.e, move |r| {
        verdict_on(r, &scope, &decision, &against, "agent:r", false);
    });
    let before = p.landings();
    let e = run(&p, v.clone()).unwrap_err();
    assert_eq!(e.code, E_EXCEPTION_OPEN, "{e}");
    let c = e.corrected.as_ref().expect("corrected");
    assert_eq!(c["ref"].as_str(), Some(proposed_ref(&prepared).as_str()));
    assert_eq!(c["view"].as_str(), Some(view.as_str()));
    let shown = c["exceptions"].as_array().expect("the cards");
    assert_eq!(shown.len(), 1, "{c}");
    assert!(
        shown[0]["ref"]
            .as_str()
            .unwrap()
            .starts_with(&prepared.decision)
    );
    assert_eq!(p.landings(), before);
    assert!(
        p.answer_on_decision(&prepared.decision).is_none(),
        "nothing was confirmed"
    );
    assert!(!p.view_closed(&view), "the whole proposal stays open");

    // a remove's own revoke: confirmed, and it lands
    let removed = run(&p, remove(&bystander.rule.id)).unwrap();
    landing_of(&removed.reply);
    assert!(p.answer_on_decision(&removed.reply.decision).is_some());
}

#[test]
fn rtc3_policy_card_returns_and_answers_exact_card() {
    let p = project();
    // two rules that exist before the policy: an edit and a remove need them
    let to_edit = landed_rule(&p, "Rule to edit.");
    let to_remove = landed_rule(&p, "Rule to remove.");
    land_policy(&p.e, card_policy());
    let before = p.landings();

    // add: the card, the whole of it; nothing lands; a repeat is the same stop
    let stop = run(&p, add("A rule on a card.")).unwrap();
    let v = stop.to_json();
    assert_eq!(v["state"], "card");
    assert_eq!(v["rule"]["text"], "A rule on a card.");
    assert!(v.get("landing").is_none(), "{v}");
    let (view, proposal, r#ref, hash, card, cards) = card_of(&stop.reply);
    assert_eq!(r#ref, proposal);
    assert_eq!(card["kind"], "approve");
    assert_eq!(card["card_hash"].as_str(), Some(hash.as_str()));
    assert!(card["card"].as_str().unwrap().contains("Approve"), "{card}");
    assert_eq!(card["options"][0]["id"], "approve");
    assert!(cards.is_empty(), "{cards:#?}");
    assert_eq!(p.landings(), before);
    assert!(!p.view_closed(&view));
    let again = run(&p, add("A rule on a card.")).unwrap();
    assert_eq!(card_of(&again.reply).1, proposal);
    assert_eq!(card_of(&again.reply).3, hash);

    // a moved hash: the normal refusal, with the fresh card; the view stays
    let wrong = "0".repeat(64);
    let e = run(&p, answer(&r#ref, "approve", &wrong)).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e}");
    assert_eq!(
        e.corrected.as_ref().unwrap()["card_hash"].as_str(),
        Some(hash.as_str())
    );
    assert_eq!(p.landings(), before);
    assert!(!p.view_closed(&view));

    // an option the card does not have
    let e = run(&p, answer(&r#ref, "o9", &hash)).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");

    // the wrong destination: another project has no such proposal
    let elsewhere = project();
    let e = run(&elsewhere, answer(&r#ref, "approve", &hash)).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_BAD_INPUT,
            format!("item {ref} does not belong to a rules-from-chat proposal").as_str(),
            "answer it through rdy_answer in its project",
        ),
        "{e}"
    );
    assert_eq!(elsewhere.landings(), 0);

    // a proposal of another flow
    let unrelated = on(&p.e, |r| {
        let w = write(
            r,
            "plain",
            json!([{"op": "create", "type": "doc", "fields": {"name": "plain", "title": "Plain", "doc_type": "page"}}]),
        );
        w.unwrap_or_else(|e| panic!("{e}"));
        r.propose(&writer(), "plain", "Plain", Some("Plain."))
            .unwrap_or_else(|e| panic!("{e}"))
            .r#ref
    });
    let e = run(&p, answer(&unrelated, "approve", &hash)).unwrap_err();
    assert_eq!(
        e.message,
        format!("item {unrelated} does not belong to a rules-from-chat proposal"),
        "{e}"
    );

    // later: a snooze, the card stays
    let later = run(&p, answer(&r#ref, "later", &hash)).unwrap();
    let (_, item, a, remaining) = answered_of(&later.reply);
    assert_eq!(
        (item.as_str(), a["act"].as_str()),
        (r#ref.as_str(), Some("later"))
    );
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].item_ref, r#ref);
    let later_json = later.to_json();
    assert_eq!(later_json["state"], "answered");
    assert_eq!(later_json["proposal_ref"].as_str(), Some(proposal.as_str()));

    // approve: the card's evidence, in the destination
    let done = run(&p, answer(&r#ref, "approve", &hash)).unwrap();
    let landing = landing_of(&done.reply).clone();
    assert_eq!(landing["assurance"], "relayed");
    let hash_of: Hash = landing["landing"].as_str().unwrap().parse().unwrap();
    let rec = p.e.read().reader().landing(&hash_of).unwrap();
    let ev = rec.evidence.as_ref().unwrap();
    assert_eq!(ev.kind, EvidenceKind::Card);
    assert_eq!(ev.card_hash.map(|h| h.to_string()), Some(hash.clone()));
    assert_eq!(ev.choice.as_deref(), card["options"][0]["label"].as_str());
    let quote = rec.relay_evidence.as_ref().unwrap();
    assert_eq!(quote.quoted, "Yes, as the card says.");
    assert_eq!(quote.question.as_deref(), Some("This card?"));
    assert_eq!(
        quote.session_ref, "chat-1",
        "the original request's session"
    );
    assert!(p.view_closed(&view), "a landed view closes");

    // edit: the approve card and its attached confirm card, answered one by one
    let stop = run(&p, edit(&to_edit.rule.id, "Rule edited.")).unwrap();
    let (view, proposal, r#ref, hash, _, cards) = card_of(&stop.reply);
    assert_eq!(cards.len(), 1, "{cards:#?}");
    assert!(cards[0].item_ref.starts_with(&stop.reply.decision));
    assert_eq!(cards[0].card["kind"], "confirm");
    let (confirm_ref, confirm_hash) = (cards[0].item_ref.clone(), cards[0].card_hash.clone());
    // approving first is the gate's refusal, and the proposal stays open
    let e = run(&p, answer(&r#ref, "approve", &hash)).unwrap_err();
    assert_eq!(e.code, E_EXCEPTION_OPEN, "{e}");
    assert!(!p.view_closed(&view));
    // a moved confirm hash
    let e = run(&p, answer(&confirm_ref, "confirm", &wrong)).unwrap_err();
    assert_eq!(e.code, E_ITEM_MOVED, "{e}");
    let confirmed = run(&p, answer(&confirm_ref, "confirm", &confirm_hash)).unwrap();
    let (_, item, a, remaining) = answered_of(&confirmed.reply);
    assert_eq!(item, confirm_ref);
    assert_eq!(a["act"], "confirm");
    assert_eq!(a["assurance"], "relayed");
    assert_eq!(remaining.len(), 1, "{remaining:#?}");
    assert_eq!(remaining[0].item_ref, r#ref, "only this proposal's");
    let record = p.answer_on_decision(&confirmed.reply.decision).unwrap().1;
    assert_eq!(record.evidence_kind, Some(EvidenceKind::Card));
    assert_eq!(
        record.card_hash.map(|h| h.to_string()),
        Some(confirm_hash.clone())
    );
    assert_eq!(record.option.as_deref(), Some("confirm"));
    assert!(!record.options.is_empty());
    assert_eq!(
        p.landings(),
        before + 1,
        "a confirmation alone lands nothing"
    );
    let done = run(&p, answer(&r#ref, "approve", &remaining[0].card_hash)).unwrap();
    landing_of(&done.reply);
    assert_eq!(done.reply.rule.text, "Rule edited.");
    assert_eq!(p.landings(), before + 2);
    assert_eq!(done.reply.decision, confirmed.reply.decision);
    assert!(p.pending_refs().iter().all(|r| !r.contains(&proposal)));

    // remove: the same, and it keeps the prior words
    let stop = run(&p, remove(&to_remove.rule.id)).unwrap();
    let (_, _, r#ref, _, _, cards) = card_of(&stop.reply);
    assert_eq!(cards.len(), 1);
    let confirmed = run(
        &p,
        answer(&cards[0].item_ref, "confirm", &cards[0].card_hash),
    )
    .unwrap();
    let (_, _, _, remaining) = answered_of(&confirmed.reply);
    let done = run(&p, answer(&r#ref, "approve", &remaining[0].card_hash)).unwrap();
    landing_of(&done.reply);
    assert!(done.reply.rule.retired);
    assert_eq!(done.reply.rule.text, "Rule to remove.");

    // reject: an ordinary answer; nothing lands; the proposal has left the queue
    let stop = run(&p, add("A rule the person refuses.")).unwrap();
    let (view, _, r#ref, hash, _, _) = card_of(&stop.reply);
    let landings = p.landings();
    let rejected = run(&p, answer(&r#ref, "reject", &hash)).unwrap();
    let (_, _, a, remaining) = answered_of(&rejected.reply);
    assert_eq!(a["act"], "reject_proposal");
    assert!(remaining.is_empty(), "{remaining:#?}");
    assert_eq!(p.landings(), landings);
    assert!(!p.pending_refs().contains(&r#ref));
    // a repeat of the mutation, and another answer, do not land it
    let e = run(&p, add("A rule the person refuses.")).unwrap_err();
    assert_eq!(e.code, E_ITEM_MOVED, "{e}");
    let e = run(&p, answer(&r#ref, "approve", &hash)).unwrap_err();
    assert_eq!(e.code, E_ITEM_MOVED, "{e}");
    assert_eq!(p.landings(), landings);
    assert!(!p.view_closed(&view));

    // a check verdict, through the cross-project form: the destination is the person's meta
    // project, and this form names it by scope
    let verdicts = project();
    land_policy(&verdicts.e, card_policy());
    let meta = call_as(me(), Destination::Meta);
    let person_add = json!({"scope": "person", "op": "add", "text": "A rule with a concern.",
        "why": "They asked.", "evidence": "keep it", "session_ref": "chat-2"});
    let stop = run_rule(
        &*verdicts.e,
        &FakeAgent::default(),
        &meta,
        &req(person_add.clone()),
    )
    .unwrap();
    assert_eq!(stop.reply.scope, rdy_engine::rule_types::Scope::Person);
    let (_, _, r#ref, _, _, _) = card_of(&stop.reply);
    let review = concern_on(&*verdicts.e, &stop.reply.decision);
    let stop = run_rule(&*verdicts.e, &FakeAgent::default(), &meta, &req(person_add)).unwrap();
    let (_, _, _, hash, card, _) = card_of(&stop.reply);
    let option = card["options"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|o| {
            o["id"]
                .as_str()
                .filter(|i| i.starts_with("accept_verdict:"))
        })
        .expect("the acceptance option")
        .to_string();
    assert_eq!(
        option.len(),
        144,
        "the returned option is the whole grammar"
    );
    assert!(option.contains(&review));
    let landings = verdicts.landings();
    let v = json!({"scope": "person", "answer": {"ref": r#ref, "option": option, "card_hash": hash,
                   "evidence": "Yes, that blocks it."}});
    let accepted = run_rule(&*verdicts.e, &FakeAgent::default(), &meta, &req(v)).unwrap();
    let (_, item, a, remaining) = answered_of(&accepted.reply);
    assert_eq!(item, r#ref);
    assert_eq!(a["act"], "accept_verdict");
    assert!(
        remaining.iter().all(|c| c.item_ref != r#ref),
        "a version with an accepted verdict has no approve card: {remaining:#?}"
    );
    let st = verdicts.e.read();
    let record = st
        .reader()
        .answer_of(&format!("verdict:{review}"))
        .unwrap()
        .expect("the destination's answer record")
        .1;
    assert_eq!(record.act, AnswerAct::AcceptVerdict);
    assert_eq!(record.option.as_deref(), Some(option.as_str()));
    assert_eq!(record.card_hash.map(|h| h.to_string()), Some(hash));
    assert_eq!(
        record.relay_evidence.unwrap().quoted,
        "Yes, that blocks it."
    );
    drop(st);
    assert_eq!(
        verdicts.landings(),
        landings,
        "an acceptance alone lands nothing"
    );
}

#[test]
fn rtc3_required_policy_role_refuses_without_landing() {
    let p = project();
    let to_edit = landed_rule(&p, "Rule under a role, to edit.");
    let to_remove = landed_rule(&p, "Rule under a role, to remove.");
    land_policy(&p.e, boss_policy(json!({})));
    let before = p.landings();
    let head = p.head();
    let refused = |e: &RdyError, r: &str| {
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_PERMISSION,
                format!("{PERSON} is not in the policy role approvers for {r}").as_str(),
                "ask a person in the policy role approvers, or relay their answer",
            ),
            "{e}"
        );
    };

    // a draft cannot change the policy that judges it: another open view's change is not canon
    on(&p.e, |r| {
        let st = r.read();
        let m = st.materialized(&Scope::Canon).unwrap();
        let id = m
            .iter()
            .find(|b| b.block_type == "authority_policy")
            .unwrap()
            .id
            .to_string();
        drop(st);
        write(
            r,
            "loosen",
            json!([{"op": "update", "id": id,
                    "fields": {"roles": {}, "require": []}}]),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    });

    for (v, name) in [
        (add("A new rule under a role."), "add"),
        (edit(&to_edit.rule.id, "Edited under a role."), "edit"),
        (remove(&to_remove.rule.id), "remove"),
    ] {
        let prepared = prepare_only(&p, v.clone());
        let r#ref = proposed_ref(&prepared);
        let e = run(&p, v.clone()).unwrap_err();
        refused(&e, &r#ref);
        assert_eq!(p.landings(), before, "{name} landed nothing");
        assert_eq!(p.head(), head, "{name} left canon alone");
        assert_eq!(
            p.close_reason(&prepared.view).as_deref(),
            Some(RELAY_FAILED),
            "{name}: an automatic flow closes its view"
        );
        // the retry finds the closed view
        let e = run(&p, v).unwrap_err();
        assert_eq!(e.code, E_VIEW_CLOSED, "{name}: {e}");
    }

    // the answer form: the card, then the role; the proposal stays open
    let p = project();
    let to_edit = landed_rule(&p, "Rule under a role, to edit.");
    land_policy(&p.e, boss_policy(json!({"rules_from_chat": "card"})));
    let before = p.landings();
    let stop = run(&p, edit(&to_edit.rule.id, "Edited on a card.")).unwrap();
    let (view, _, r#ref, hash, _, cards) = card_of(&stop.reply);
    let confirm = cards.first().expect("the confirm card").clone();
    let e = run(&p, answer(&confirm.item_ref, "confirm", &confirm.card_hash)).unwrap_err();
    refused(&e, &confirm.item_ref);
    let e = run(&p, answer(&r#ref, "approve", &hash)).unwrap_err();
    refused(&e, &r#ref);
    assert_eq!(p.landings(), before);
    assert!(
        !p.view_closed(&view),
        "an answer refusal keeps the proposal"
    );
    assert_eq!(p.answers(), 0);
    // a person in the role is not this call's person, so Later still works: it is never refused
    let later = run(&p, answer(&r#ref, "later", &hash)).unwrap();
    assert_eq!(answered_of(&later.reply).2["act"], "later");

    // a reader cannot start an answer or a mutation; a human cannot relay
    let reader_session = Session::agent(ME.into(), Some("r1".into()), Role::Reader).unwrap();
    let as_reader = call_as(reader_session, Destination::Project);
    let e = run_rule(
        &*p.e,
        &FakeAgent::default(),
        &as_reader,
        &req(answer(&r#ref, "approve", &hash)),
    )
    .unwrap_err();
    assert_eq!(e.code, E_PERMISSION, "{e}");
    let e = run_rule(
        &*p.e,
        &FakeAgent::default(),
        &as_reader,
        &req(add("Not from a reader.")),
    )
    .unwrap_err();
    assert_eq!(e.code, E_PERMISSION, "{e}");
    let human = call_as(approver(), Destination::Project);
    let e = run_rule(
        &*p.e,
        &FakeAgent::default(),
        &human,
        &req(add("Not from a human relay.")),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            E_PERMISSION,
            "a person answers as themselves; only the relaying agent records a relay"
        ),
        "{e}"
    );
}

#[test]
fn rtc3_not_ready_returns_ref_and_ordered_issues() {
    let p = project();
    landed_rule(&p, "Say exactly this once.");
    let before = p.landings();
    // the same words again: a real duplicate-definition tension holds the version back
    let v = json!({"scope": "project", "op": "add", "text": "Say exactly this once.",
                   "why": "Again.", "evidence": "again", "session_ref": "chat-2"});
    let stop = run(&p, v.clone()).unwrap();
    let json = stop.to_json();
    assert_eq!(json["state"], "not_ready");
    assert!(json.get("card").is_none(), "no fabricated card: {json}");
    assert!(json.get("landing").is_none(), "{json}");
    let Outcome::NotReady {
        view,
        item_ref,
        issues,
    } = &stop.reply.outcome
    else {
        panic!("{:#?}", stop.reply.outcome);
    };
    assert_eq!(p.landings(), before);
    assert!(!p.view_closed(view));
    let q =
        p.e.read()
            .pending(&PendingQuery {
                person: PERSON.into(),
                session_ref: None,
                max_tokens: CARD_MAX_TOKENS,
            })
            .unwrap();
    let want = &q
        .not_ready
        .iter()
        .find(|n| &n.r#ref == item_ref)
        .expect("the queue lists it")
        .issues;
    assert!(!want.is_empty());
    assert_eq!(
        issues,
        &want
            .iter()
            .map(|i| serde_json::to_value(i).unwrap())
            .collect::<Vec<_>>(),
        "the queue's issues, in its order"
    );
    assert!(
        !q.items.iter().any(|i| &i.r#ref == item_ref),
        "the queue shows no card for it"
    );

    // a repeat returns the same proposal
    let again = run(&p, v.clone()).unwrap();
    let Outcome::NotReady {
        item_ref: again_ref,
        ..
    } = &again.reply.outcome
    else {
        panic!("{:#?}", again.reply.outcome);
    };
    assert_eq!(again_ref, item_ref);
    assert_eq!(p.landings(), before);

    // the card policy gives the same stop, with no card
    land_policy(&p.e, card_policy());
    let carded = run(&p, v).unwrap();
    assert!(
        matches!(carded.reply.outcome, Outcome::NotReady { .. }),
        "{:#?}",
        carded.reply.outcome
    );

    // the answer form keeps E_PROPOSAL_NOT_READY, its issues, and adds the destination and ref
    let e = run(&p, answer(item_ref, "approve", &"0".repeat(64))).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_NOT_READY, "{e}");
    assert!(!e.issues.is_empty());
    let c = e.corrected.as_ref().expect("corrected");
    assert_eq!(c["ref"].as_str(), Some(item_ref.as_str()));
    assert_eq!(c["view"].as_str(), Some(view.as_str()));
    assert_eq!(
        c["project"]["name"].as_str(),
        Some(first_project_name(&p).as_str())
    );
    assert!(!p.view_closed(view));
}

fn first_project_name(p: &Proj) -> String {
    p.e.read().project_name()
}

#[test]
fn rtc3_late_dependent_stops_every_approval_preparation() {
    // text approval, after the draft: a dependent in another open view, then in canon
    for op in ["edit", "remove"] {
        let p = project();
        let rule = landed_rule(&p, "A rule with a late dependent.");
        let (id, old, doc) = (
            rule.rule.id.clone(),
            rule.decision.clone(),
            rule.rule.ruleset.clone(),
        );
        let v = match op {
            "edit" => edit(&id, "A rule, edited late."),
            _ => remove(&id),
        };
        let prepared = prepare_only(&p, v.clone());
        let blocker = dependent_in_view(&p.e, "late", &doc, &old);
        let before = p.landings();
        let e = run(&p, v).unwrap_err();
        assert_shared(&e, &old, &[&blocker]);
        assert_eq!(p.landings(), before, "{op}");
        assert!(
            p.still_rests_on(&id, &old),
            "{op}: the old decision stays live"
        );
        assert_eq!(
            p.close_reason(&prepared.view).as_deref(),
            Some(RELAY_FAILED),
            "{op}"
        );
    }

    // a signing retry: the dependent lands in canon while the signer waits
    for op in ["edit", "remove"] {
        let p = project();
        let rule = landed_rule(&p, "A rule that moves under a signer.");
        let (id, old, doc) = (
            rule.rule.id.clone(),
            rule.decision.clone(),
            rule.rule.ruleset.clone(),
        );
        p.config("auto", true);
        let (e2, d2, o2) = (Arc::clone(&p.e), doc.clone(), old.clone());
        let made = Arc::new(Mutex::new(None::<String>));
        let m2 = Arc::clone(&made);
        let agent = FakeAgent::hooked(false, move |_| {
            *m2.lock().unwrap() = Some(dependent_in_canon(&e2, "signing", &d2, &o2));
        });
        let before = p.landings();
        let v = match op {
            "edit" => edit(&id, "A rule, edited under a signer."),
            _ => remove(&id),
        };
        let e = run_with(&p, &agent, v).unwrap_err();
        let blocker = made.lock().unwrap().clone().expect("the signer ran");
        assert_shared(&e, &old, &[&blocker]);
        assert_eq!(p.landings(), before + 1, "{op}: only the dependent landed");
        assert_eq!(agent.signs(), 1, "{op}: the first signature was discarded");
        assert!(p.still_rests_on(&id, &old), "{op}");
    }

    // a returned card answer: a refusal, with the proposal open; the view's own close clears it
    for op in ["edit", "remove"] {
        let p = project();
        let rule = landed_rule(&p, "A rule that waits on a card.");
        let (id, old, doc) = (
            rule.rule.id.clone(),
            rule.decision.clone(),
            rule.rule.ruleset.clone(),
        );
        land_policy(&p.e, card_policy());
        let v = match op {
            "edit" => edit(&id, "A rule, edited on a card."),
            _ => remove(&id),
        };
        let stop = run(&p, v).unwrap();
        let (view, _, r#ref, _, _, cards) = card_of(&stop.reply);
        let confirmed = run(
            &p,
            answer(&cards[0].item_ref, "confirm", &cards[0].card_hash),
        )
        .unwrap();
        let hash = answered_of(&confirmed.reply).3[0].card_hash.clone();
        let blocker = dependent_in_view(&p.e, "late-card", &doc, &old);
        let before = p.landings();
        let e = run(&p, answer(&r#ref, "approve", &hash)).unwrap_err();
        assert_shared(&e, &old, &[&blocker]);
        assert_eq!(p.landings(), before, "{op}");
        assert!(p.still_rests_on(&id, &old), "{op}");
        assert!(
            !p.view_closed(&view),
            "{op}: the presented proposal stays open"
        );
    }
}

#[test]
fn rtc3_late_policy_change_rechecks_card_and_role() {
    // land to card, under the signing barrier: the fresh card, and nothing lands
    let p = project();
    p.config("auto", true);
    let e2 = Arc::clone(&p.e);
    let agent = FakeAgent::hooked(false, move |_| land_policy(&e2, card_policy()));
    let before = p.landings();
    let stop = run_with(&p, &agent, add("A rule while the policy moves.")).unwrap();
    let (view, _, r#ref, hash, _, _) = card_of(&stop.reply);
    assert_eq!(p.landings(), before + 1, "only the policy landed");
    assert_eq!(agent.signs(), 1);
    assert!(!p.view_closed(&view));
    // the card is fresh: an approval of it lands, with no moved refusal
    let done = run_with(&p, &FakeAgent::default(), answer(&r#ref, "approve", &hash)).unwrap();
    assert_eq!(landing_of(&done.reply)["assurance"], "relayed");

    // a changed role: the normal freshness refusal first, which keeps the proposal, then the
    // normal role check, which closes it
    let p = project();
    p.config("auto", true);
    let e2 = Arc::clone(&p.e);
    let agent = FakeAgent::hooked(false, move |_| land_policy(&e2, boss_policy(json!({}))));
    let before = p.landings();
    let v = add("A rule while the role moves.");
    let e = run_with(&p, &agent, v.clone()).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e}");
    let c = e.corrected.as_ref().expect("corrected");
    assert!(c.get("card").is_some(), "the fresh card: {c}");
    let r#ref = c["ref"].as_str().unwrap().to_string();
    let view = c["view"].as_str().unwrap().to_string();
    assert_eq!(p.landings(), before + 1);
    assert!(!p.view_closed(&view));
    let e = run_with(&p, &FakeAgent::default(), v).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            E_PERMISSION,
            format!("{PERSON} is not in the policy role approvers for {ref}").as_str()
        ),
        "{e}"
    );
    assert_eq!(p.landings(), before + 1);
    assert_eq!(p.close_reason(&view).as_deref(), Some(RELAY_FAILED));

    // a policy that keeps land: freshness alone; a retry lands
    let p = project();
    p.config("auto", true);
    let e2 = Arc::clone(&p.e);
    let agent = FakeAgent::hooked(false, move |_| land_policy(&e2, policy(json!({}))));
    let v = add("A rule while the policy is adopted.");
    let e = run_with(&p, &agent, v.clone()).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e}");
    let done = run(&p, v).unwrap();
    landing_of(&done.reply);
}

#[test]
fn rtc3_retry_after_lost_reply_never_lands_twice() {
    let p = project();
    // the mutation: the first reply is lost, and the repeat reads the receipt
    let v = add("A rule whose reply was lost.");
    let first = run(&p, v.clone()).unwrap();
    let log = p.landings();
    let head = p.head();
    let answers = p.answers();
    let again = run(&p, v.clone()).unwrap();
    let (a, b) = (landing_of(&first.reply), landing_of(&again.reply));
    for key in [
        "landing",
        "approved",
        "version",
        "title",
        "summary",
        "changes",
        "assurance",
        "relayed_by",
    ] {
        assert_eq!(a[key], b[key], "{key}");
    }
    assert_eq!(first.reply.rule, again.reply.rule);
    assert_eq!(first.reply.decision, again.reply.decision);
    assert_eq!(p.landings(), log, "one canon landing");
    assert_eq!(p.head(), head);
    assert_eq!(p.answers(), answers);

    // an edit, and a remove after its rule retired
    let edited = run(&p, edit(&first.reply.rule.id, "A rule, edited once.")).unwrap();
    let log = p.landings();
    let answers = p.answers();
    let edited_again = run(&p, edit(&first.reply.rule.id, "A rule, edited once.")).unwrap();
    assert_eq!(
        landing_of(&edited.reply)["landing"],
        landing_of(&edited_again.reply)["landing"]
    );
    assert_eq!((p.landings(), p.answers()), (log, answers));
    let removed = run(&p, remove(&first.reply.rule.id)).unwrap();
    let log = p.landings();
    let removed_again = run(&p, remove(&first.reply.rule.id)).unwrap();
    assert_eq!(
        landing_of(&removed.reply)["landing"],
        landing_of(&removed_again.reply)["landing"]
    );
    assert_eq!(p.landings(), log);

    // the answer form: an exact repeat returns the prior result, with no new record
    land_policy(&p.e, card_policy());
    let rule = run(&p, add("A rule on a card, whose reply was lost.")).unwrap();
    let (_, _, r#ref, hash, _, _) = card_of(&rule.reply);
    let log = p.landings();
    let done = run(&p, answer(&r#ref, "approve", &hash)).unwrap();
    assert_eq!(p.landings(), log + 1);
    let head = p.head();
    let answers = p.answers();
    let done_again = run(&p, answer(&r#ref, "approve", &hash)).unwrap();
    assert_eq!(
        landing_of(&done.reply)["landing"],
        landing_of(&done_again.reply)["landing"]
    );
    assert_eq!(done.reply.rule, done_again.reply.rule);
    assert_eq!(
        (p.landings(), p.head(), p.answers()),
        (log + 1, head, answers)
    );
    // the mutation finds the same landing
    let m = run(&p, add("A rule on a card, whose reply was lost.")).unwrap();
    assert_eq!(
        landing_of(&m.reply)["landing"],
        landing_of(&done.reply)["landing"]
    );
    // a different answer does not reuse that result
    let other = json!({"scope": "project", "answer": {"ref": r#ref, "option": "approve", "card_hash": hash,
                       "evidence": "Other words."}});
    let e = run(&p, other).unwrap_err();
    assert_eq!(
        e.code, E_PROPOSAL_MOVED,
        "the landing wrote a final version: {e}"
    );
    assert_eq!(p.landings(), log + 1);

    // a confirmation, repeated: its record, not a second one
    let target = run(&p, add("A rule to edit on a card.")).unwrap();
    let (_, _, approve_ref, approve_hash, _, _) = card_of(&target.reply);
    let landed = run(&p, answer(&approve_ref, "approve", &approve_hash)).unwrap();
    landing_of(&landed.reply);
    let stop = run(&p, edit(&target.reply.rule.id, "A rule, edited on a card.")).unwrap();
    let (_, _, _, _, _, cards) = card_of(&stop.reply);
    let c = run(
        &p,
        answer(&cards[0].item_ref, "confirm", &cards[0].card_hash),
    )
    .unwrap();
    let answers = p.answers();
    let c2 = run(
        &p,
        answer(&cards[0].item_ref, "confirm", &cards[0].card_hash),
    )
    .unwrap();
    assert_eq!(
        answered_of(&c.reply).2["answer"],
        answered_of(&c2.reply).2["answer"]
    );
    assert_eq!(p.answers(), answers, "no second record");
    // after its proposal landed too
    let (_, _, _, remaining) = answered_of(&c2.reply);
    let landed = run(
        &p,
        answer(&remaining[0].item_ref, "approve", &remaining[0].card_hash),
    )
    .unwrap();
    landing_of(&landed.reply);
    let c3 = run(
        &p,
        answer(&cards[0].item_ref, "confirm", &cards[0].card_hash),
    )
    .unwrap();
    let (_, _, a, remaining) = answered_of(&c3.reply);
    assert_eq!(a["answer"], answered_of(&c.reply).2["answer"]);
    assert!(remaining.is_empty(), "{remaining:#?}");
    assert_eq!(c3.reply.rule.text, "A rule, edited on a card.");
    assert_eq!(p.answers(), answers);
}

#[test]
fn rtc3_signing_and_cleanup_preserve_committed_result() {
    // always, with no key: nothing lands, and the automatic flow closes its view
    let p = project();
    p.config("always", false);
    let v = add("A rule that must be signed.");
    let e = run(&p, v.clone()).unwrap_err();
    assert_eq!(e.code, E_SIGNING_KEY_MISSING, "{e}");
    assert_eq!(p.landings(), 0);
    let e = run(&p, v).unwrap_err();
    assert_eq!(e.code, E_VIEW_CLOSED, "{e}");

    // always, with an agent that refuses: the same
    p.config("always", true);
    let refusing = FakeAgent::refusing();
    let v = add("A rule the agent will not sign.");
    let e = run_with(&p, &refusing, v.clone()).unwrap_err();
    assert_eq!(e.code, E_NO_AGENT, "{e}");
    assert_eq!(refusing.signs(), 1);
    assert_eq!(p.landings(), 0);

    // an always failure whose cleanup cannot write keeps the refusal and says so
    let v = add("A rule whose cleanup fails.");
    let q = req(v.clone());
    let e = with_rule_fault_for_tests(RuleFault::Close, || run_rule(&*p.e, &refusing, &call(), &q))
        .unwrap_err();
    assert_eq!(e.code, E_NO_AGENT, "{e}");
    let c = e.corrected.as_ref().expect("corrected");
    assert_eq!(c["cleanup"], "pending");
    let view = c["view"].as_str().unwrap().to_string();
    assert!(!p.view_closed(&view), "the whole change stays recoverable");

    // auto: the first failure lands unsigned with one warning
    p.config("auto", true);
    let refusing = FakeAgent::refusing();
    let done = run_with(&p, &refusing, add("A rule signed if it can be.")).unwrap();
    let landing = landing_of(&done.reply).clone();
    assert_eq!(landing["signatures"], json!([]));
    let codes: Vec<&str> = landing["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["code"].as_str())
        .collect();
    assert!(codes.contains(&W_SIGNING_FAILED), "{codes:?}");
    // the landing is signed when the agent answers
    let agent = FakeAgent::default();
    let signed = run_with(&p, &agent, add("A rule the agent signs.")).unwrap();
    let landing = landing_of(&signed.reply).clone();
    assert_eq!(landing["signatures"].as_array().unwrap().len(), 1);
    assert_eq!(agent.signs(), 1);

    // the reply was lost: the repeat signs nothing again, and still names the signature
    let repeat = run_with(&p, &agent, add("A rule the agent signs.")).unwrap();
    assert_eq!(agent.signs(), 1, "no second signature");
    assert_eq!(landing_of(&repeat.reply)["landing"], landing["landing"]);
    assert_eq!(
        landing_of(&repeat.reply)["signatures"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // a landing whose cleanup fails stands: a warning with the underlying code, and the view
    // stays open until a repeat closes it
    p.config("never", false);
    let v = add("A rule whose close fails after it lands.");
    let q = req(v.clone());
    let before = p.landings();
    let done = with_rule_fault_for_tests(RuleFault::Close, || {
        run_rule(&*p.e, &FakeAgent::default(), &call(), &q)
    })
    .unwrap();
    let landing = landing_of(&done.reply).clone();
    assert_eq!(p.landings(), before + 1, "the landing stands");
    let warning = landing["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == E_INTERNAL)
        .expect("the cleanup warning")
        .clone();
    assert!(
        warning["fix"].as_str().unwrap().contains("rdy view close"),
        "{warning}"
    );
    let view = rule_view_of(&p, &done.reply);
    assert!(!p.view_closed(&view));

    // reopen: the repeat finishes the cleanup and returns the known result, landing nothing
    let p = p.reopen();
    p.config("never", false);
    let again = run(&p, v).unwrap();
    assert_eq!(landing_of(&again.reply)["landing"], landing["landing"]);
    assert_eq!(p.landings(), before + 1);
    assert!(p.view_closed(&view));
    assert!(
        landing_of(&again.reply)["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|w| w["code"] != E_INTERNAL),
        "the cleanup is done"
    );
}

/// The view of the rule request whose reply is `r`: the only open or landed view of its name.
fn rule_view_of(p: &Proj, r: &Reply) -> String {
    let st = p.e.read();
    let version: Hash = landing_of(r)["version"].as_str().unwrap().parse().unwrap();
    st.reader().proposal(&version).unwrap().view
}

/// Pre-flight correction 3: a landing refusal keeps the view and the proposal, and returns the
/// refusal and the ref. A retry lands.
#[test]
fn rtc3_landing_refusal_keeps_view_and_retry_lands() {
    // the log moves through all three signing attempts
    let p = project();
    p.config("auto", true);
    let e2 = Arc::clone(&p.e);
    let agent = FakeAgent::hooked(true, move |n| {
        let view = format!("mover{n}");
        on(&e2, move |r| {
            write(
                r,
                &view,
                json!([{"op": "create", "type": "doc", "fields": {"name": view, "title": "Mover", "doc_type": "page"}}]),
            )
            .unwrap_or_else(|e| panic!("{e}"));
        });
    });
    let v = add("A rule that loses three signing races.");
    let e = run_with(&p, &agent, v.clone()).unwrap_err();
    assert_eq!(e.code, E_STALE_BASE, "{e}");
    assert_eq!(agent.signs(), 3);
    let c = e.corrected.as_ref().expect("corrected");
    let view = c["view"].as_str().unwrap().to_string();
    assert!(c["ref"].as_str().unwrap().starts_with("P-"), "{c}");
    assert!(!p.view_closed(&view), "the view and the proposal stay");
    assert_eq!(p.landings(), 0);
    let done = run_with(&p, &FakeAgent::default(), v).unwrap();
    landing_of(&done.reply);

    // a competing doc takes the default path: the refusal keeps the proposal
    let p = project();
    let v = add("A rule that meets another my-rules.");
    let prepared = prepare_only(&p, v.clone());
    on(&p.e, |r| {
        write(
            r,
            "squatter",
            json!([{"op": "create", "type": "doc", "fields": {"name": "project-rules", "key": "project-rules", "title": "Squatter", "doc_type": "page", "lifecycle": "draft"}}]),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        land(r, "squatter").unwrap_or_else(|e| panic!("{e}"));
    });
    let e = run(&p, v.clone()).unwrap_err();
    assert_eq!(e.code, E_VALIDATION, "{e}");
    let c = e.corrected.as_ref().expect("corrected");
    assert_eq!(c["ref"].as_str(), Some(proposed_ref(&prepared).as_str()));
    assert_eq!(c["view"].as_str(), Some(prepared.view.as_str()));
    assert!(!p.view_closed(&prepared.view));
    assert_eq!(p.landings(), 1, "only the squatter landed");
}

/// A competing retry that landed wins over a refusal of the same request: nothing withdraws it.
#[test]
fn rtc3_competing_retry_wins_over_a_failed_relay() {
    let p = project();
    p.config("always", true);
    let v = add("A rule two calls ask for.");
    let (e2, v2) = (Arc::clone(&p.e), v.clone());
    let agent = FakeAgent::hooked(false, move |_| {
        let good = FakeAgent::default();
        let r = run_rule(&*e2, &good, &call(), &req(v2.clone())).expect("the competing call");
        landing_of(&r.reply);
    });
    agent.refuse.store(usize::MAX, Ordering::SeqCst);
    let r = run_rule(&*p.e, &agent, &call(), &req(v)).unwrap();
    landing_of(&r.reply);
    assert_eq!(p.landings(), 1, "one canon landing");
    let view = rule_view_of(&p, &r.reply);
    assert_eq!(
        p.close_reason(&view).as_deref(),
        Some("rules_from_chat: landed"),
        "nothing withdrew it"
    );
}

/// The form's other item refs resolve to the proposal they name. An advised tension holds a rule's
/// version back as an open plan item, so it has no card to answer: the normal refusal of a moved
/// item, not the refusal of another flow's. Another flow's proposal gets that one.
#[test]
fn rtc3_advice_and_deferral_refs_resolve_to_their_proposal() {
    let p = project();
    let other = landed_rule(&p, "A rule that stands beside.");
    let v = add("A rule with advice beside it.");
    let prepared = prepare_only(&p, v.clone());
    let scope = Scope::View(prepared.view.clone());
    let (x, y) = (
        BlockId::parse(&prepared.decision).unwrap(),
        BlockId::parse(&other.decision).unwrap(),
    );
    on(&p.e, move |r| {
        verdict_on(r, &scope, &x, &y, "agent:r", true)
    });
    let r#ref = proposed_ref(&prepared);
    let hash = "0".repeat(64);
    for prefix in ["advice:", "defer:"] {
        let e = run(&p, answer(&format!("{prefix}{ref}"), "later", &hash)).unwrap_err();
        assert_eq!(e.code, E_ITEM_MOVED, "{prefix}: {e}");
    }
    let plain = on(&p.e, |r| {
        write(
            r,
            "plain",
            json!([{"op": "create", "type": "doc", "fields": {"name": "plain", "title": "Plain", "doc_type": "page"}}]),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        r.propose(&writer(), "plain", "Plain", Some("Plain."))
            .unwrap_or_else(|e| panic!("{e}"))
            .r#ref
    });
    for prefix in ["advice:", "defer:"] {
        let item = format!("{prefix}{plain}");
        let e = run(&p, answer(&item, "later", &hash)).unwrap_err();
        assert_eq!(
            e.message,
            format!("item {item} does not belong to a rules-from-chat proposal"),
            "{e}"
        );
    }
}

/// Pre-flight correction 10: only card evidence binds verdicts, so a text relay stops at the card
/// while a concern is open. Nothing lands over it.
#[test]
fn rtc3_open_concern_stops_text_relay_at_the_card() {
    let p = project();
    let v = add("A rule with an open concern.");
    let prepared = prepare_only(&p, v.clone());
    let r#ref = proposed_ref(&prepared);
    concern_on(&*p.e, &prepared.decision);
    let stop = run(&p, v.clone()).unwrap();
    let (view, proposal, item, _, card, _) = card_of(&stop.reply);
    assert_eq!(proposal, r#ref);
    assert_eq!(item, r#ref);
    assert!(
        card["options"].as_array().unwrap().iter().any(|o| o["id"]
            .as_str()
            .is_some_and(|i| i.starts_with("accept_verdict:"))),
        "{card}"
    );
    assert!(
        card["options"]
            .as_array()
            .unwrap()
            .iter()
            .all(|o| o.get("recommended").is_none()),
        "an open concern recommends nothing: {card}"
    );
    assert_eq!(p.landings(), 0);
    assert!(!p.view_closed(&view));
    // a retried text relay stops at the same card
    let again = run(&p, v).unwrap();
    assert_eq!(card_of(&again.reply).1, r#ref);
    assert_eq!(p.landings(), 0);
    assert_eq!(p.answers(), 0, "the shortcut accepts no verdict");
}

/// A policy that does not read is no absent policy: the request stops before any draft.
#[test]
fn rtc3_invalid_policy_stops_before_any_draft() {
    let p = project();
    land_policy(&p.e, policy(json!({})));
    let (block, base) = {
        let st = p.e.read();
        let m = st.materialized(&Scope::Canon).unwrap();
        let b = m
            .iter()
            .find(|b| b.block_type == "authority_policy")
            .unwrap()
            .clone();
        let base = m.version_of(&b.id).unwrap();
        (b, base)
    };
    // canon holds a block the pack refuses, as a pack change leaves one
    on(&p.e, move |r| {
        let mut bad = block.clone();
        bad.fields.insert(
            "rules_from_chat".into(),
            rdy_model::FieldValue::Str("sometimes".into()),
        );
        r.store_mut()
            .commit_to_view(
                "bad-policy",
                rdy_model::Change {
                    view: "bad-policy".into(),
                    ops: vec![rdy_model::Op::Update { block: bad, base }],
                    deps: vec![],
                    provenance: writer().provenance(),
                    justification: None,
                    message: "raw".into(),
                    links: Default::default(),
                    read_identity: None,
                },
            )
            .expect("raw commit");
        r.invalidate();
        rdy_qa::fixtures::land_with_a_tie(r, "bad-policy");
    });
    let before = p.e.read().view_names();
    let e = run(&p, add("A rule under a broken policy.")).unwrap_err();
    assert_eq!(e.code, E_VALIDATION, "{e}");
    assert!(
        e.issues.iter().any(|i| i.code == "E_FIELD_ENUM"),
        "{:?}",
        e.issues
    );
    assert_eq!(p.e.read().view_names(), before, "no view was created");
}
