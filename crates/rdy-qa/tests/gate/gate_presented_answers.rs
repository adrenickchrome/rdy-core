//! ui1 (D-6M0AF, D-WPE4R, D-QVH4N, D-1Z951, D-TF78H, D-JQD5X, D-67ZN7; TASK-982 §1, O2, O9,
//! O11, C7, C8): a card the person answers in the local approver UI, or one an MCP client
//! elicited from them, goes through the answer engine's own binding and dispatch as the person
//! (`local`), with evidence `card` or `elicited` and no relay. Role, freshness and every hold
//! keep their refusals. The UI never asks an agent to sign; an elicited approval signs with the
//! person's key through the caller's agent. A UI Later stores nothing; an elicited Later is a
//! chat-session snooze keyed by the caller's run.
use ed25519_dalek::Signer as _;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::answer::{
    AnswerRequest, Presentation, PresentedAnswer, presented_item, presented_question_brief,
    run_presented_answer,
};
use rdy_engine::check::api::{ReviewRequest, run_review};
use rdy_engine::engine::{Engine, Local, with_writer_result};
use rdy_engine::pending::{
    CARD_MAX_TOKENS, ExceptionKind, HOLD_LABEL, Item, ItemKind, KEEP_LABEL, Pending, PendingQuery,
    PlanCardBinding,
};
use rdy_engine::semantic::api::run_review_submit;
use rdy_engine::semantic::submit::{CitationRequest, ReviewSubmitRequest};
use rdy_engine::signature_check::{SIGNED_VALID, STATEMENT_MATCH};
use rdy_engine::signing_act::{SigningAct, run_sign, run_signed_act};
use rdy_engine::signing_crypto::{AgentIdentity, SigningAgent, UnavailableAgent, blob_fingerprint};
use rdy_engine::tensions::{TensionKind, TensionSeverity, tension_label};
use rdy_engine::{
    DecisionInput, E_BAD_INPUT, E_BRIEF_REQUIRED, E_BRIEF_STALE, E_DEFERRAL_OPEN, E_EXCEPTION_OPEN,
    E_ITEM_MOVED, E_NO_AGENT, E_PERMISSION, E_PROPOSAL_MOVED, E_PROPOSAL_NOT_READY,
    E_SIGNING_KEY_MISSING, E_SIGNING_KEY_TYPE, LandOptions, NoAgentCause, ProposalInfo, Rdy,
    RdyError, Scope, Session, W_SIGNING_FAILED, WriteRequest, no_agent,
};
use rdy_model::plan::PlanKind;
use rdy_model::signature::{Signature, parse_statement};
use rdy_model::{
    Answer, AnswerAct, ApprovalEvidence, Assurance, BlockId, EvidenceKind, Hash, Justification,
    LandingRecord, Role,
};
use rdy_qa::edit_loop::{brief_of, spec_doc, write_cited};
use rdy_qa::fixtures::{approver, land, parse_test_ops, seed_doc, write, writer};
use rdy_qa::intake::{
    BATCH_SUMMARY, BATCH_TITLE, BATCH_VIEW, PERSON, TEST_SUMMARY, batch_project, decide_in,
    decision, propose_reported,
};
use rdy_qa::packs::QA_PACK_YAML;
use rdy_qa::tensions::{ag, created, j_typo, of_kind, open, put, put_j, upd};
use rdy_store::kinds;
use serde_json::{Value, json};
use ssh_key::sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// A person who wrote no side of the tension fixtures.
const FOUNDER: &str = "human:founder";
/// The UI's fix for an approval that `signing.sign: always` would sign.
fn ui_fix(pref: &str) -> String {
    format!("run rdy approve {pref} in a terminal to sign this approval")
}

// ---------------------------------------------------------------- sources and callers

const SOURCES: [Presentation; 2] = [Presentation::Ui, Presentation::Elicited];

/// The MCP agent session that elicits an answer: `agent:relay`, run `r1`.
fn chat() -> Session {
    Session::agent("agent:relay".into(), Some("r1".into()), Role::Writer).unwrap()
}

/// The session that presents the card: the person's own session for the UI, the agent's for an
/// elicited answer.
fn caller(source: Presentation) -> Session {
    match source {
        Presentation::Ui => approver(),
        Presentation::Elicited => chat(),
    }
}

fn kind_of(source: Presentation) -> EvidenceKind {
    match source {
        Presentation::Ui => EvidenceKind::Card,
        Presentation::Elicited => EvidenceKind::Elicited,
    }
}

/// One presented answer as `person`, through `caller(source)`.
#[allow(clippy::too_many_arguments)]
fn present_as(
    e: &Local,
    agent: &dyn SigningAgent,
    by: &Session,
    source: Presentation,
    person: &str,
    r#ref: &str,
    card_hash: &str,
    option: &str,
    note: Option<&str>,
    brief: Option<&str>,
) -> Result<Value, RdyError> {
    run_presented_answer(
        e,
        agent,
        by,
        person,
        source,
        PresentedAnswer {
            reference: r#ref.into(),
            card_hash: card_hash.into(),
            option: option.into(),
            note: note.map(str::to_string),
            brief: brief.map(str::to_string),
        },
    )
}

/// [`present_as`] through the source's own caller, with no agent key held.
#[allow(clippy::too_many_arguments)]
fn present(
    e: &Local,
    source: Presentation,
    person: &str,
    r#ref: &str,
    card_hash: &str,
    option: &str,
    note: Option<&str>,
    brief: Option<&str>,
) -> Result<Value, RdyError> {
    present_as(
        e,
        &FakeAgent::new(&[]),
        &caller(source),
        source,
        person,
        r#ref,
        card_hash,
        option,
        note,
        brief,
    )
}

// ---------------------------------------------------------------- engine helpers

fn on<T: Send + 'static>(
    e: &dyn Engine,
    f: impl FnOnce(&mut Rdy) -> Result<T, RdyError> + Send + 'static,
) -> Result<T, RdyError> {
    with_writer_result(e, f)
}

fn queue_of(e: &dyn Engine, person: &str, session: Option<&str>) -> Pending {
    e.read()
        .pending(&PendingQuery {
            person: person.into(),
            session_ref: session.map(str::to_string),
            max_tokens: CARD_MAX_TOKENS,
        })
        .expect("pending")
}

fn find(e: &dyn Engine, person: &str, r#ref: &str) -> Option<Item> {
    queue_of(e, person, None)
        .items
        .into_iter()
        .find(|i| i.r#ref == r#ref)
}

fn card(e: &dyn Engine, person: &str, r#ref: &str) -> Item {
    find(e, person, r#ref).unwrap_or_else(|| {
        let q = queue_of(e, person, None);
        let refs: Vec<&str> = q.items.iter().map(|i| i.r#ref.as_str()).collect();
        panic!("{ref} is not in {refs:?}; not ready: {:#?}", q.not_ready)
    })
}

fn labels(i: &Item) -> Vec<String> {
    i.options.iter().map(|o| o.label.clone()).collect()
}

fn label_of(i: &Item, option: &str) -> String {
    i.options
        .iter()
        .find(|o| o.id == option)
        .unwrap_or_else(|| panic!("{option} on {}", i.r#ref))
        .label
        .clone()
}

fn hash(text: &str) -> Hash {
    text.parse().expect("hash")
}

fn answer_of(e: &dyn Engine, item: &str) -> Option<Answer> {
    e.read()
        .reader()
        .answer_of(item)
        .expect("answer_of")
        .map(|(_, a)| a)
}

/// Every answer and snooze the published state holds.
fn every_answer(e: &dyn Engine) -> Vec<(String, Answer)> {
    let st = e.read();
    ["answer/", "pending/"]
        .iter()
        .flat_map(|p| st.reader().answers_with_prefix(p).expect("answers"))
        .map(|(name, _, a)| (name, a))
        .collect()
}

/// What a refusal must leave as it was: the answers, the snoozes and canon.
fn untouched(e: &dyn Engine) -> (Vec<(String, Answer)>, Option<Hash>) {
    (every_answer(e), e.read().canon_head())
}

fn refused(r: &Result<Value, RdyError>, code: &str) -> RdyError {
    match r {
        Err(e) => {
            assert_eq!(e.code, code, "{e:#?}");
            e.clone()
        }
        Ok(v) => panic!("expected {code}, got {v:#}"),
    }
}

/// The reply of every successful act names it as the person's own: `local`, no relay.
fn local_reply(v: &Value, act: &str, option: &str) {
    assert_eq!(v["act"], act, "{v:#}");
    assert_eq!(v["option"], option, "{v:#}");
    assert_eq!(v["assurance"], "local", "{v:#}");
    assert!(v["relayed_by"].is_null(), "{v:#}");
}

/// The stored ANSWER of `key`: the person's provenance, the card's labels, the chosen option and
/// label, the hash shown, `kind` and no relay.
#[allow(clippy::too_many_arguments)]
fn check_answer(
    e: &dyn Engine,
    key: &str,
    act: AnswerAct,
    shown: &Item,
    option: &str,
    label: Option<&str>,
    person: &str,
    kind: EvidenceKind,
) -> Answer {
    let a = answer_of(e, key).unwrap_or_else(|| panic!("no answer to {key}"));
    assert_eq!(a.act, act, "{a:#?}");
    assert_eq!(a.option.as_deref(), Some(option), "{a:#?}");
    assert_eq!(a.label.as_deref(), label, "{a:#?}");
    assert_eq!(a.options, labels(shown), "{a:#?}");
    assert_eq!(a.card_hash, Some(hash(&shown.card_hash)), "{a:#?}");
    assert_eq!(
        (
            a.person.actor.as_str(),
            a.person.run.as_deref(),
            a.person.role
        ),
        (person, Some("cli"), Role::Approver),
        "{a:#?}"
    );
    assert!(
        a.relayed_by.is_none() && a.relay_evidence.is_none(),
        "{a:#?}"
    );
    assert_eq!(a.evidence_kind, Some(kind), "{a:#?}");
    a
}

/// The landing a reply names.
fn landing_of(e: &dyn Engine, v: &Value) -> (Hash, LandingRecord) {
    let h = hash(
        v["landing"]["landing"]
            .as_str()
            .unwrap_or_else(|| panic!("no landing in {v:#}")),
    );
    (h, e.read().reader().landing(&h).expect("the landing"))
}

/// The stored landing: assurance local, the person as approver, no relay, and the evidence of
/// the card shown with the chosen label.
fn check_landing(
    e: &dyn Engine,
    v: &Value,
    shown: &Item,
    choice: &str,
    person: &str,
    kind: EvidenceKind,
) -> Hash {
    let (h, rec) = landing_of(e, v);
    assert_eq!(rec.assurance, Assurance::Local, "{rec:#?}");
    assert_eq!(rec.approved_by.actor, person, "{rec:#?}");
    assert!(rec.relayed_by.is_none() && rec.relay_evidence.is_none());
    assert_eq!(
        rec.evidence,
        Some(ApprovalEvidence {
            kind,
            options: labels(shown),
            choice: Some(choice.into()),
            card_hash: Some(hash(&shown.card_hash)),
        })
    );
    h
}

fn fsck(e: &dyn Engine) {
    on(e, |r| r.fsck().map(|_| ())).expect("fsck");
}

// ---------------------------------------------------------------- the fake agent

#[derive(Clone, Copy, Debug, PartialEq)]
enum KeyKind {
    Ed,
    Sk,
    /// A key of a type rdy does not know: rdy does not sign with it.
    Unknown,
}

#[derive(Clone, Copy, Debug)]
struct SoftKey {
    seed: [u8; 32],
    kind: KeyKind,
}

const PERSON_SEED: [u8; 32] = [7; 32];
const AGENT_SEED: [u8; 32] = [9; 32];
const SK_APP: &[u8] = b"ssh:";

fn ssh_string(bytes: &[u8]) -> Vec<u8> {
    let mut out = (bytes.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(bytes);
    out
}

impl SoftKey {
    fn dalek(&self) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&self.seed)
    }

    fn blob(&self) -> Vec<u8> {
        let public = self.dalek().verifying_key().to_bytes();
        match self.kind {
            KeyKind::Ed => [ssh_string(b"ssh-ed25519"), ssh_string(&public)].concat(),
            KeyKind::Sk => [
                ssh_string(b"sk-ssh-ed25519@openssh.com"),
                ssh_string(&public),
                ssh_string(SK_APP),
            ]
            .concat(),
            KeyKind::Unknown => {
                [ssh_string(b"ssh-unknown@example.com"), ssh_string(&public)].concat()
            }
        }
    }

    fn fingerprint(&self) -> String {
        blob_fingerprint(&self.blob())
    }

    fn sign(&self, payload: &[u8]) -> Vec<u8> {
        match self.kind {
            KeyKind::Ed | KeyKind::Unknown => {
                let raw = self.dalek().sign(payload).to_bytes();
                [ssh_string(b"ssh-ed25519"), ssh_string(&raw)].concat()
            }
            KeyKind::Sk => {
                let flags = 1u8;
                let counter = 42u32;
                let mut m = Sha256::digest(SK_APP).to_vec();
                m.push(flags);
                m.extend(counter.to_be_bytes());
                m.extend(Sha256::digest(payload));
                let raw = self.dalek().sign(&m).to_bytes();
                let mut out =
                    [ssh_string(b"sk-ssh-ed25519@openssh.com"), ssh_string(&raw)].concat();
                out.push(flags);
                out.extend(counter.to_be_bytes());
                out
            }
        }
    }
}

fn person_ed() -> SoftKey {
    SoftKey {
        seed: PERSON_SEED,
        kind: KeyKind::Ed,
    }
}
fn person_sk() -> SoftKey {
    SoftKey {
        seed: PERSON_SEED,
        kind: KeyKind::Sk,
    }
}
fn agent_ed() -> SoftKey {
    SoftKey {
        seed: AGENT_SEED,
        kind: KeyKind::Ed,
    }
}
fn unknown_key() -> SoftKey {
    SoftKey {
        seed: [5; 32],
        kind: KeyKind::Unknown,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Reply {
    Good,
    Cancel,
}

type Hook = Box<dyn FnMut(usize) + Send>;

/// An agent holding `keys`, answering by `script` (then `Good`), counting every call, and
/// running `hook` before its first sign reply.
struct FakeAgent {
    keys: Vec<SoftKey>,
    script: Mutex<Vec<Reply>>,
    calls: AtomicUsize,
    signs: AtomicUsize,
    hook: Mutex<Option<Hook>>,
}

impl FakeAgent {
    fn new(keys: &[SoftKey]) -> FakeAgent {
        FakeAgent {
            keys: keys.to_vec(),
            script: Mutex::new(vec![]),
            calls: AtomicUsize::new(0),
            signs: AtomicUsize::new(0),
            hook: Mutex::new(None),
        }
    }

    fn scripted(keys: &[SoftKey], script: &[Reply]) -> FakeAgent {
        let a = FakeAgent::new(keys);
        let mut s = script.to_vec();
        s.reverse();
        *a.script.lock().unwrap() = s;
        a
    }

    fn hooked(keys: &[SoftKey], hook: impl FnMut(usize) + Send + 'static) -> FakeAgent {
        let a = FakeAgent::new(keys);
        *a.hook.lock().unwrap() = Some(Box::new(hook));
        a
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn signs(&self) -> usize {
        self.signs.load(Ordering::SeqCst)
    }
}

impl SigningAgent for FakeAgent {
    fn identities(&self) -> Result<Vec<AgentIdentity>, RdyError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .keys
            .iter()
            .map(|k| AgentIdentity {
                public_key: k.blob(),
            })
            .collect())
    }

    fn sign(&self, public_key: &[u8], payload: &[u8]) -> Result<Vec<u8>, RdyError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let n = self.signs.fetch_add(1, Ordering::SeqCst);
        let hook = self.hook.lock().unwrap().take();
        if let Some(mut h) = hook {
            h(n);
        }
        let reply = self.script.lock().unwrap().pop().unwrap_or(Reply::Good);
        if reply == Reply::Cancel {
            return Err(no_agent(NoAgentCause::Cancelled));
        }
        let key = self
            .keys
            .iter()
            .find(|k| k.blob() == public_key)
            .expect("the fake agent holds the key it was asked for");
        Ok(key.sign(payload))
    }
}

/// The signing settings and the configured person of `.rdy/local.yaml`.
fn local_config(dir: &Path, actor: &str, sign: &str, key: Option<SoftKey>, agent: Option<SoftKey>) {
    let sel = |k: Option<SoftKey>| match k {
        Some(k) => format!("\"{}\"", k.fingerprint()),
        None => "null".into(),
    };
    std::fs::write(
        dir.join(".rdy/local.yaml"),
        format!(
            "actor: {actor}\nsigning:\n  key: {}\n  agent_key: {}\n  sign: {sign}\n  allowed_signers: null\n",
            sel(key),
            sel(agent)
        ),
    )
    .expect("local config");
}

/// The stored signatures of `subject`.
fn signatures(e: &dyn Engine, subject: &Hash) -> Vec<Signature> {
    let st = e.read();
    st.reader()
        .signatures_of(subject)
        .expect("signatures_of")
        .iter()
        .map(|h| st.reader().signature(h).expect("signature"))
        .collect()
}

/// `(kind, key)` of each stored signature of `subject`, sorted.
fn signature_kinds(e: &dyn Engine, subject: &Hash) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = signatures(e, subject)
        .iter()
        .map(|g| {
            let p = parse_statement(&g.statement).expect("statement");
            (
                p.kind().as_str().to_string(),
                p.field("key").expect("key").to_string(),
            )
        })
        .collect();
    out.sort();
    out
}

fn signing_warnings(v: &Value) -> Vec<String> {
    v["landing"]["warnings"]
        .as_array()
        .map(|w| {
            w.iter()
                .filter(|w| w["code"] == W_SIGNING_FAILED)
                .map(|w| w["message"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------- fixtures

/// A project whose canon holds a seed doc: its directory, engine and section.
fn simple() -> (tempfile::TempDir, Local, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut r = Rdy::init(dir.path(), "rdy-standard").expect("init");
    let (_, section, _) = seed_doc(&mut r, "seed");
    let p = r
        .propose(&writer(), "seed", "Seed", Some("Seeds."))
        .expect("propose seed");
    r.land(&approver(), &p.id).expect("land seed");
    local_config(dir.path(), PERSON, "never", None, None);
    (dir, Local::new(r), section)
}

/// A claim in view `view` under `section`, proposed with a summary, its open plan items
/// answered first.
fn proposal(e: &dyn Engine, section: &str, view: &str) -> ProposalInfo {
    let (section, view) = (section.to_string(), view.to_string());
    on(e, move |r| {
        write(
            r,
            &view,
            json!([{"op": "create", "type": "claim", "parent": section, "body": format!("A claim of {view}.")}]),
        )?;
        // M3e: a version's section neighbors are plan items; answer them before the propose
        rdy_qa::intake::answer_open_items(r, &writer(), &view);
        r.propose(&writer(), &view, &view, Some("Adds one claim."))
    })
    .expect("proposal")
}

/// The `confirm` items beside batch `batch`, in queue order.
fn exceptions_of(e: &dyn Engine, batch: &str) -> Vec<Item> {
    queue_of(e, PERSON, None)
        .items
        .into_iter()
        .filter(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(batch))
        .collect()
}

/// `batch_project` with a decision that depends on claim A and another view that edits A: open
/// blocking tensions on the batch. Their labels, sorted.
fn blocking_world() -> (tempfile::TempDir, Local, Vec<String>) {
    let (dir, mut r, b) = batch_project();
    decide_in(
        &mut r,
        BATCH_VIEW,
        DecisionInput {
            depends_on: vec![b.spec.a.to_string()],
            ..decision(&b.spec, "Guests rely on A", "Guests rely on A")
        },
    );
    put(&mut r, "other", upd(&b.spec.a, "A as other has it."));
    let (r, _) = propose_reported(dir.path(), r, BATCH_VIEW, BATCH_TITLE, Some(BATCH_SUMMARY));
    let mut labels: Vec<String> = open(&r)
        .iter()
        .filter(|t| t.severity == TensionSeverity::Blocking)
        .map(|t| tension_label(&t.id))
        .collect();
    labels.sort();
    assert!(!labels.is_empty());
    (dir, Local::new(r), labels)
}

// -- the re-affirmation card

/// Canon: a section with claim a and two accepted decisions on it; a then changes, so both are
/// suspect and a's re-affirmation card stands. The card's ref.
fn reaffirm_world() -> (tempfile::TempDir, Local, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
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
    for statement in ["One rule", "Two rule"] {
        let view = format!("d-{}", statement.replace(' ', "-"));
        r.decide(
            &writer(),
            &view,
            DecisionInput {
                parent: s.clone(),
                statement: statement.into(),
                depends_on: vec![a.clone()],
                reversibility: Some("two_way".into()),
                ..Default::default()
            },
        )
        .unwrap();
        land(&mut r, &view).unwrap();
    }
    write(
        &mut r,
        "x",
        json!([{"op": "update", "id": a, "body": "A holds 60 days."}]),
    )
    .unwrap();
    land(&mut r, "x").unwrap();
    let e = Local::new(r);
    let item = queue_of(&e, PERSON, None)
        .items
        .into_iter()
        .find(|i| {
            matches!(&i.plan_binding, Some(PlanCardBinding::Reaffirm { cause, .. }) if cause.as_str() == a)
        })
        .expect("a's re-affirmation card");
    (dir, e, item.r#ref)
}

// -- the deferral card

fn pi(cause: &str, subject: &str, kind: PlanKind) -> String {
    rdy_model::plan::item_display(&rdy_model::plan::item_hash(
        &BlockId::parse(cause).unwrap(),
        subject,
        kind,
    ))
}

/// Canon: claims a, b and q, where b and q build on a; view w changes a and defers b's and q's
/// confirm items in its propose. The version, with its deferral card.
fn deferral_world() -> (tempfile::TempDir, Local, ProposalInfo) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}}."},
            {"op": "create", "type": "claim", "id": "$q", "parent": "$s", "body": "Q builds on {{$a}}."}
        ]),
    )
    .unwrap();
    land(&mut r, "seed").unwrap();
    let get = |k: &str| w.created[k].clone();
    let (a, b, q) = (get("$a"), get("$b"), get("$q"));
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": a, "body": "A holds 60 days."}]),
    )
    .unwrap();
    let defer = |item: String, reason: &str| rdy_engine::plan::PlanAnswerInput {
        item,
        act: rdy_model::plan::PlanAct::Defer,
        reason: Some(reason.into()),
    };
    let answers = [
        defer(pi(&a, &b, PlanKind::Confirm), "b waits for the review"),
        defer(pi(&a, &q, PlanKind::Confirm), "q waits for the rewrite"),
    ];
    let p = r
        .propose_plan(
            &writer(),
            rdy_engine::plan::ProposePlanRequest {
                summary: Some(TEST_SUMMARY),
                plan_answers: &answers,
                ..rdy_engine::plan::ProposePlanRequest::new("w", "w")
            },
        )
        .unwrap();
    (dir, Local::new(r), p)
}

// -- verdicts

fn reviewer() -> Session {
    Session::agent("agent:r".into(), Some("r".into()), Role::Reviewer).unwrap()
}

/// An independent `meaning_preservation` request on version `pref`, answered `verdict` by
/// agent:r citing `cite`: the review's hash.
fn meaning_verdict(e: &dyn Engine, pref: &str, verdict: &str, cite: &BlockId) -> String {
    let q = run_review(
        e,
        &ag("a1"),
        PERSON,
        true,
        None,
        ReviewRequest::from_value(
            json!({"op": "request", "check": "meaning_preservation", "subject": pref}),
        )
        .unwrap(),
    )
    .expect("request");
    let req = ReviewSubmitRequest {
        request: q["request"].as_str().unwrap().into(),
        case_hash: q["case_hash"].as_str().unwrap().into(),
        verdict: verdict.into(),
        citations: vec![CitationRequest {
            block: cite.to_string(),
            note: "the meaning moved".into(),
        }],
        severity: None,
        reason: Some("it reads otherwise".into()),
    };
    let v = run_review_submit(e, &reviewer(), req).expect("submit");
    v["review"].as_str().unwrap().to_string()
}

/// A draft doc `notes` with claim x, then agent:a1's typo edit of x, proposed, with a `fail`
/// meaning verdict on it: the version, its review and x.
fn verdict_world() -> (tempfile::TempDir, Local, ProposalInfo, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let made = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "X holds."}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let x = created(&made, "$x");
    put_j(&mut r, "a1", upd(&x, "X holds, now."), j_typo());
    let p = r
        .propose(&ag("a1"), "a1", "a1", Some(TEST_SUMMARY))
        .unwrap();
    let e = Local::new(r);
    let review = meaning_verdict(&e, &p.r#ref, "fail", &x);
    (dir, e, p, review)
}

// -- the policy

fn write_as_j(
    r: &mut Rdy,
    s: &Session,
    view: &str,
    ops: Value,
    j: Option<Justification>,
) -> rdy_engine::WriteResult {
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

/// An `rdy-qa` project under a policy whose role `approvers` holds only [`PERSON`] and is
/// required for every version that writes a decision; then agent:a1's version that writes one.
fn policed_world() -> (tempfile::TempDir, Local, ProposalInfo) {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").expect("init"));
    std::fs::write(dir.path().join(".rdy/packs/rdy-qa.yaml"), QA_PACK_YAML).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: rdy-qa\n").unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let seed = write_as_j(
        &mut r,
        &writer(),
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Ideas"}}
        ]),
        None,
    );
    land(&mut r, "seed").unwrap();
    let s = created(&seed, "$s");
    let rules = write_as_j(
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
    write_as_j(
        &mut r,
        &writer(),
        "pol",
        json!([{"op": "create", "type": "authority_policy", "id": "$p", "parent": rules.created["$d"], "fields": {
            "key": "authority-policy",
            "agent_may_accept": [],
            "roles": {"approvers": [PERSON]},
            "require": [{"when": "decision", "role": "approvers"}],
            "owners": []
        }}]),
        Some(Justification::Implements(vec![d])),
    );
    land(&mut r, "pol").unwrap();
    put_j(
        &mut r,
        "a1",
        json!([{"op": "create", "type": "decision", "id": "$dec", "parent": s.to_string(),
                "fields": {"statement": "Ideas stay short", "status": "proposed", "needs_content": false,
                           "reversibility": "two_way", "authority_basis": "precedent"}}]),
        j_typo(),
    );
    let p = r
        .propose(&ag("a1"), "a1", "a1", Some(TEST_SUMMARY))
        .unwrap();
    (dir, Local::new(r), p)
}

// -- questions

#[derive(Clone, Copy, Debug, PartialEq)]
enum Dest {
    /// `decisions.home` names the spec doc.
    Home,
    /// The question's parent: the spec's section.
    Parent,
    /// The project root.
    Root,
}

/// The spec doc, stable in canon, and an open question with authored options: under the
/// section, or at the root for [`Dest::Root`]; [`Dest::Home`] sets `decisions.home` to the doc.
/// The question's item ref and the decision's destination.
fn question_world(dest: Dest) -> (tempfile::TempDir, Local, String, Option<BlockId>) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let spec = spec_doc(&mut r);
    let t = brief_of(&r, &Scope::Canon, &[&spec.doc]).base.token;
    write_cited(
        &mut r,
        "st",
        json!([{"op": "update", "id": spec.doc.to_string(), "fields": {"lifecycle": "stable"}}]),
        Some(&t),
    )
    .expect("make the doc stable");
    land(&mut r, "st").expect("land the stable doc");
    let fields = json!({
        "prompt": "Who approves refunds?",
        "title": "Who approves refunds?",
        "why": "Support waits on this",
        "options": ["Finance", "Support lead", "Either"],
        "recommended": "Finance",
        "recommend_reason": "Finance owns the ledger"
    });
    let w = match dest {
        Dest::Root => write(
            &mut r,
            "qs",
            json!([{"op": "create", "type": "question", "id": "$q", "fields": fields}]),
        )
        .unwrap(),
        _ => {
            let t = brief_of(&r, &Scope::Canon, &[&spec.section]).base.token;
            write_cited(
                &mut r,
                "qs",
                json!([{"op": "create", "type": "question", "id": "$q",
                        "parent": spec.section.to_string(), "fields": fields}]),
                Some(&t),
            )
            .unwrap()
        }
    };
    land(&mut r, "qs").unwrap();
    if dest == Dest::Home {
        let cfg = dir.path().join(".rdy.yaml");
        let mut text = std::fs::read_to_string(&cfg).unwrap_or_default();
        text.push_str(&format!("decisions:\n  home: {}\n", spec.doc));
        std::fs::write(&cfg, text).unwrap();
    }
    r.close().unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    let q = created(&w, "$q");
    let v = rdy_qa::fixtures::version_of(&r, &Scope::Canon, &q.to_string());
    let destination = match dest {
        Dest::Home => Some(spec.doc.clone()),
        Dest::Parent => Some(spec.section.clone()),
        Dest::Root => None,
    };
    (dir, Local::new(r), format!("{q}@{}", &v[..12]), destination)
}

/// The decision a question's ANSWER names, as view `view` holds it.
fn decision_of(e: &dyn Engine, view: &str, change: &Hash) -> rdy_model::Block {
    let st = e.read();
    let c = st.reader().change(change).expect("the change");
    let id = c.ops[0].block_id().clone();
    st.block(&Scope::View(view.into()), &id)
        .expect("the decision in the view")
}

/// The person's answer view, or canon when it does not exist yet.
fn answer_scope(e: &dyn Engine) -> Scope {
    let view = Session::human(PERSON.into(), Role::Approver)
        .unwrap()
        .default_view();
    if e.read().has_view(&view) {
        Scope::View(view)
    } else {
        Scope::Canon
    }
}

// ---------------------------------------------------------------- tests

/// Every act of the queue, presented in the UI and elicited: the same dispatch, the person's
/// records with `card` or `elicited` evidence, and the normal reply.
#[test]
fn ui1_every_presented_act_uses_the_existing_dispatch() {
    for source in SOURCES {
        let kind = kind_of(source);
        let s = format!("{source:?}");

        // approve, and reject a proposal
        let (_d, e, section) = simple();
        let p = proposal(&e, &section, "a");
        let shown = card(&e, PERSON, &p.r#ref);
        assert_eq!(shown.kind, ItemKind::Approve);
        // an approval keeps no note: the existing rule
        let r = present(
            &e,
            source,
            PERSON,
            &p.r#ref,
            &shown.card_hash,
            "approve",
            Some("ok"),
            None,
        );
        assert_eq!(
            refused(&r, E_BAD_INPUT).message,
            "an approval keeps no note"
        );
        let v = present(
            &e,
            source,
            PERSON,
            &p.r#ref,
            &shown.card_hash,
            "approve",
            None,
            None,
        )
        .unwrap_or_else(|err| panic!("{s} approve: {err:#?}"));
        local_reply(&v, "approve", "approve");
        assert!(v["answer"].is_null());
        assert_eq!(v["signatures"], json!([]), "{v:#}");
        let label = label_of(&shown, "approve");
        check_landing(&e, &v, &shown, &label, PERSON, kind);
        let p = proposal(&e, &section, "b");
        let shown = card(&e, PERSON, &p.r#ref);
        let v = present(
            &e,
            source,
            PERSON,
            &p.r#ref,
            &shown.card_hash,
            "reject",
            None,
            None,
        )
        .unwrap();
        local_reply(&v, "reject_proposal", "reject");
        let label = label_of(&shown, "reject");
        check_answer(
            &e,
            &p.r#ref,
            AnswerAct::RejectProposal,
            &shown,
            "reject",
            Some(&label),
            PERSON,
            kind,
        );
        fsck(&e);

        // confirm, and reject, a decision beside a batch
        let (_d, r, b) = batch_project();
        let e = Local::new(r);
        let ex = exceptions_of(&e, &b.r#ref);
        let plain: Vec<&Item> = ex
            .iter()
            .filter(|i| i.exception != Some(ExceptionKind::ReaffirmOneWay))
            .collect();
        assert!(plain.len() >= 2, "{ex:#?}");
        for (i, option, act) in [
            (plain[0], "confirm", AnswerAct::Confirm),
            (plain[1], "reject", AnswerAct::RejectDecision),
        ] {
            let v = present(
                &e,
                source,
                PERSON,
                &i.r#ref,
                &i.card_hash,
                option,
                None,
                None,
            )
            .unwrap_or_else(|err| panic!("{s} {option}: {err:#?}"));
            local_reply(&v, act.as_str(), option);
            let label = label_of(i, option);
            check_answer(&e, &i.r#ref, act, i, option, Some(&label), PERSON, kind);
        }

        // the advice: accepting it keeps its plain record (F3); dismissing tensions writes the
        // person's reviews and one ANSWER
        let (_d, r, b) = batch_project();
        let e = Local::new(r);
        let advice = format!("advice:{}", b.r#ref);
        let shown = card(&e, FOUNDER, &advice);
        let v = present(
            &e,
            source,
            FOUNDER,
            &advice,
            &shown.card_hash,
            "accept",
            None,
            None,
        )
        .unwrap_or_else(|err| panic!("{s} accept: {err:#?}"));
        local_reply(&v, "accept", "accept");
        assert!(v["answer"].is_null(), "no ANSWER for an acceptance: {v:#}");
        assert!(answer_of(&e, &advice).is_none());
        let accepted: Vec<_> = {
            let st = e.read();
            let all = on(&e, |r| Ok(of_kind(r, TensionKind::Semantic))).unwrap();
            all.iter()
                .filter_map(|t| st.tension_info(t).judged.and_then(|j| j.accepted))
                .collect()
        };
        assert!(!accepted.is_empty());
        for a in &accepted {
            assert_eq!(
                (
                    a.actor.as_str(),
                    a.relayed_by.clone(),
                    a.evidence.as_deref()
                ),
                (FOUNDER, None, None),
                "the person's plain acceptance"
            );
        }
        fsck(&e);

        let (_d, e, tensions) = blocking_world();
        let shown = card(&e, FOUNDER, &tensions[0]);
        let r = present(
            &e,
            source,
            FOUNDER,
            &tensions[0],
            &shown.card_hash,
            "dismiss",
            None,
            None,
        );
        assert_eq!(
            refused(&r, E_BAD_INPUT).message,
            "a dismissal needs a note: the person's reason"
        );
        let v = present(
            &e,
            source,
            FOUNDER,
            &tensions[0],
            &shown.card_hash,
            "dismiss",
            Some("A is stable"),
            None,
        )
        .unwrap_or_else(|err| panic!("{s} dismiss: {err:#?}"));
        local_reply(&v, "dismiss", "dismiss");
        let label = label_of(&shown, "dismiss");
        let a = check_answer(
            &e,
            &tensions[0],
            AnswerAct::Dismiss,
            &shown,
            "dismiss",
            Some(&label),
            FOUNDER,
            kind,
        );
        assert_eq!(a.note.as_deref(), Some("A is stable"));
        assert_eq!(a.targets.len(), 1, "the person's review");
        let review = e.read().reader().review(&a.targets[0]).unwrap();
        assert_eq!(review.provenance.actor, FOUNDER);
        fsck(&e);

        // both deferral choices: a refusal, then a later acceptance (the latest answer wins)
        let (_d, e, p) = deferral_world();
        let defer = format!("defer:{}", p.r#ref);
        for (option, act) in [
            ("o2", AnswerAct::RefuseDeferrals),
            ("o1", AnswerAct::AcceptDeferrals),
        ] {
            let shown = presented_item(&e.read(), PERSON, &defer, None).unwrap();
            assert_eq!(shown.kind, ItemKind::Defer);
            let v = present(
                &e,
                source,
                PERSON,
                &defer,
                &shown.card_hash,
                option,
                None,
                None,
            )
            .unwrap_or_else(|err| panic!("{s} {option}: {err:#?}"));
            local_reply(&v, act.as_str(), option);
            let label = label_of(&shown, option);
            check_answer(&e, &defer, act, &shown, option, Some(&label), PERSON, kind);
        }
        fsck(&e);

        // a re-affirmation card: keep them suspect, and they still hold
        let (_d, e, cause) = reaffirm_world();
        let shown = card(&e, PERSON, &cause);
        let r = present(
            &e,
            source,
            PERSON,
            &cause,
            &shown.card_hash,
            "o2",
            None,
            None,
        );
        assert_eq!(
            refused(&r, E_BAD_INPUT).message,
            "Keep them suspect needs a note"
        );
        let v = present(
            &e,
            source,
            PERSON,
            &cause,
            &shown.card_hash,
            "o2",
            Some("checked later"),
            None,
        )
        .unwrap_or_else(|err| panic!("{s} keep: {err:#?}"));
        local_reply(&v, "keep_suspect", "o2");
        let a = check_answer(
            &e,
            &cause,
            AnswerAct::KeepSuspect,
            &shown,
            "o2",
            Some(KEEP_LABEL),
            PERSON,
            kind,
        );
        assert_eq!(a.note.as_deref(), Some("checked later"));
        fsck(&e);
        let (_d, e, cause) = reaffirm_world();
        let shown = card(&e, PERSON, &cause);
        let v = present(
            &e,
            source,
            PERSON,
            &cause,
            &shown.card_hash,
            "o1",
            None,
            None,
        )
        .unwrap_or_else(|err| panic!("{s} hold: {err:#?}"));
        local_reply(&v, "reaffirm", "o1");
        check_landing(&e, &v, &shown, HOLD_LABEL, PERSON, kind);
        fsck(&e);

        // M3d: a verdict accepted as blocking
        let (_d, e, p, review) = verdict_world();
        let shown = card(&e, PERSON, &p.r#ref);
        let id = format!("accept_verdict:{review}:{}", p.id);
        assert!(
            shown.options.iter().any(|o| o.id == id),
            "{:#?}",
            shown.options
        );
        // an unknown option never dispatches
        let other = format!("accept_verdict:{review}:{}", "0".repeat(64));
        let before = untouched(&e);
        let r = present(
            &e,
            source,
            PERSON,
            &p.r#ref,
            &shown.card_hash,
            &other,
            None,
            None,
        );
        assert_eq!(
            refused(&r, E_BAD_INPUT).message,
            format!("{other} is not an option of {}", p.r#ref)
        );
        assert_eq!(untouched(&e), before);
        let v = present(
            &e,
            source,
            PERSON,
            &p.r#ref,
            &shown.card_hash,
            &id,
            None,
            None,
        )
        .unwrap_or_else(|err| panic!("{s} accept_verdict: {err:#?}"));
        local_reply(&v, "accept_verdict", &id);
        let label = label_of(&shown, &id);
        check_answer(
            &e,
            &format!("verdict:{review}"),
            AnswerAct::AcceptVerdict,
            &shown,
            &id,
            Some(&label),
            PERSON,
            kind,
        );
        fsck(&e);

        // an authored question, at each destination
        for dest in [Dest::Home, Dest::Parent, Dest::Root] {
            question_case(source, dest);
        }
    }
}

/// One authored question at `dest`, presented by `source` (see the test above).
fn question_case(source: Presentation, dest: Dest) {
    let kind = kind_of(source);
    let what = format!("{source:?} {dest:?}");
    let (_d, e, it, destination) = question_world(dest);
    let person = Session::human(PERSON.into(), Role::Approver).unwrap();
    let shown = card(&e, PERSON, &it);
    assert_eq!(shown.kind, ItemKind::Answer);
    let briefing = presented_question_brief(&e.read(), &e.briefs(), &person, &shown).unwrap();
    // the helper leaves the card and its hash as they were
    let again = card(&e, PERSON, &it);
    assert_eq!(
        (&again.card_hash, &again.card),
        (&shown.card_hash, &shown.card)
    );
    let Some(dest_id) = destination else {
        assert!(briefing.is_none(), "{what}: no briefing for the root");
        let v = present(&e, source, PERSON, &it, &shown.card_hash, "o1", None, None)
            .unwrap_or_else(|err| panic!("{what}: {err:#?}"));
        local_reply(&v, "answer_question", "o1");
        let a = check_answer(
            &e,
            &it,
            AnswerAct::AnswerQuestion,
            &shown,
            "o1",
            Some("Finance"),
            PERSON,
            kind,
        );
        let d = decision_of(&e, &person.default_view(), &a.targets[0]);
        assert_eq!(d.parent, None, "{what}");
        fsck(&e);
        return;
    };
    let briefing = briefing.unwrap_or_else(|| panic!("{what}: a briefing"));
    // the briefing of the resolved destination, as rdy_brief gives it
    let want = e
        .read()
        .brief_ids(&answer_scope(&e), &BTreeSet::from([dest_id.clone()]), None)
        .unwrap();
    assert_eq!(briefing.base.token, want.base.token, "{what}");
    // no briefing: refused, nothing written
    let before = untouched(&e);
    let r = present(&e, source, PERSON, &it, &shown.card_hash, "o1", None, None);
    refused(&r, E_BRIEF_REQUIRED);
    assert_eq!(untouched(&e), before, "{what}");
    assert!(!e.read().has_view(&person.default_view()) || answer_of(&e, &it).is_none());
    // a stale briefing: the destination changed since; refused, nothing written
    let stale = briefing.base.token.clone();
    let id = dest_id.clone();
    on(&e, move |r| {
        let t = brief_of(r, &Scope::Canon, &[&id]).base.token;
        write_cited(
            r,
            "moved",
            json!([{"op": "update", "id": id.to_string(), "fields": {"title": "Moved"}}]),
            Some(&t),
        )?;
        land(r, "moved").map(|_| ())
    })
    .unwrap();
    let shown = card(&e, PERSON, &it);
    let before = untouched(&e);
    let r = present(
        &e,
        source,
        PERSON,
        &it,
        &shown.card_hash,
        "o1",
        None,
        Some(&stale),
    );
    refused(&r, E_BRIEF_STALE);
    assert_eq!(untouched(&e), before, "{what}");
    // the current briefing, registered by the helper: the decision is written there
    let briefing = presented_question_brief(&e.read(), &e.briefs(), &person, &shown)
        .unwrap()
        .expect("a briefing");
    let v = present(
        &e,
        source,
        PERSON,
        &it,
        &shown.card_hash,
        "o1",
        None,
        Some(&briefing.base.token),
    )
    .unwrap_or_else(|err| panic!("{what}: {err:#?}"));
    local_reply(&v, "answer_question", "o1");
    let a = check_answer(
        &e,
        &it,
        AnswerAct::AnswerQuestion,
        &shown,
        "o1",
        Some("Finance"),
        PERSON,
        kind,
    );
    let d = decision_of(&e, &person.default_view(), &a.targets[0]);
    assert_eq!(d.parent, Some(dest_id), "{what}");
    fsck(&e);
}

/// The relayed answer of the same request: the refusal the presented one must keep.
fn relayed_err(e: &dyn Engine, person: &str, r#ref: &str, option: &str, h: &str) -> RdyError {
    let (person, req) = (
        person.to_string(),
        AnswerRequest {
            r#ref: r#ref.into(),
            option: option.into(),
            card_hash: Some(h.into()),
            note: None,
            relay: RelayInput {
                relayed_by: None,
                evidence: Some("yes".into()),
                question: Some("Shall I?".into()),
                session_ref: Some("chat-7".into()),
                kind: None,
            },
            brief: None,
        },
    );
    on(e, move |r| r.answer(&chat(), &person, req)).expect_err("the relayed answer is refused")
}

/// A presented answer refused as the relayed answer of the same request is refused: code,
/// message, fix, issues and the corrected payload; no answer, snooze or landing is written.
fn same_refusal(
    e: &dyn Engine,
    local: &Local,
    source: Presentation,
    person: &str,
    r#ref: &str,
    option: &str,
    h: &str,
) -> RdyError {
    let before = untouched(e);
    let got = present(local, source, person, r#ref, h, option, None, None)
        .expect_err("the presented answer is refused");
    assert_eq!(
        untouched(e),
        before,
        "{source:?} {ref} {option}: nothing written"
    );
    let want = relayed_err(e, person, r#ref, option, h);
    assert_eq!(
        (
            &got.code,
            &got.message,
            &got.fix,
            &got.issues,
            &got.corrected
        ),
        (
            &want.code,
            &want.message,
            &want.fix,
            &want.issues,
            &want.corrected
        ),
        "{source:?} {ref} {option}"
    );
    assert_eq!(untouched(e), before);
    got
}

#[test]
fn ui1_presentation_cannot_bypass_role_freshness_or_holds() {
    for source in SOURCES {
        // a person outside the policy role
        let (_d, e, p) = policed_world();
        let shown = card(&e, PERSON, &p.r#ref);
        for option in ["approve", "reject"] {
            let err = same_refusal(
                &e,
                &e,
                source,
                "human:other",
                &p.r#ref,
                option,
                &shown.card_hash,
            );
            assert_eq!(err.code, E_PERMISSION, "{err:#?}");
        }

        // a moved proposal
        let (_d, e, section) = simple();
        let old = proposal(&e, &section, "m");
        let shown = card(&e, PERSON, &old.r#ref);
        let newer = proposal(&e, &section, "m");
        assert_ne!(newer.r#ref, old.r#ref);
        let err = same_refusal(
            &e,
            &e,
            source,
            PERSON,
            &old.r#ref,
            "approve",
            &shown.card_hash,
        );
        assert_eq!(err.code, E_PROPOSAL_MOVED, "{err:#?}");
        // a card hash that is not the card shown: the fresh card comes back
        let wrong = Hash::of(b"another card").to_string();
        let err = same_refusal(&e, &e, source, PERSON, &newer.r#ref, "approve", &wrong);
        assert_eq!(err.code, E_PROPOSAL_MOVED, "{err:#?}");
        assert!(err.corrected.is_some());

        // a moved item: a confirm item with another card's hash
        let (_d, r, b) = batch_project();
        let e = Local::new(r);
        let ex = exceptions_of(&e, &b.r#ref);
        let err = same_refusal(&e, &e, source, PERSON, &ex[0].r#ref, "confirm", &wrong);
        assert_eq!(err.code, E_ITEM_MOVED, "{err:#?}");
        assert!(err.corrected.is_some());
        // an open exception holds the batch
        let shown = card(&e, PERSON, &b.r#ref);
        let err = same_refusal(
            &e,
            &e,
            source,
            PERSON,
            &b.r#ref,
            "approve",
            &shown.card_hash,
        );
        assert_eq!(err.code, E_EXCEPTION_OPEN, "{err:#?}");
        // an accepted tension hold: the person accepted the advice
        let advice = format!("advice:{}", b.r#ref);
        let a = card(&e, FOUNDER, &advice);
        present(
            &e,
            source,
            FOUNDER,
            &advice,
            &a.card_hash,
            "accept",
            None,
            None,
        )
        .unwrap();
        let err = same_refusal(
            &e,
            &e,
            source,
            PERSON,
            &b.r#ref,
            "approve",
            &shown.card_hash,
        );
        assert_eq!(err.code, E_PROPOSAL_NOT_READY, "{err:#?}");
        assert_eq!(
            err.issues.iter().map(|i| i.code).collect::<Vec<_>>(),
            vec!["E_BLOCKING_TENSION"],
            "the accepted advice holds the batch"
        );

        // an open deferral holds the version
        let (_d, e, p) = deferral_world();
        let shown = card(&e, PERSON, &p.r#ref);
        let err = same_refusal(
            &e,
            &e,
            source,
            PERSON,
            &p.r#ref,
            "approve",
            &shown.card_hash,
        );
        assert_eq!(err.code, E_DEFERRAL_OPEN, "{err:#?}");

        // an accepted check hold: the person accepted the verdict as blocking
        let (_d, e, p, review) = verdict_world();
        let shown = card(&e, PERSON, &p.r#ref);
        let id = format!("accept_verdict:{review}:{}", p.id);
        present(
            &e,
            source,
            PERSON,
            &p.r#ref,
            &shown.card_hash,
            &id,
            None,
            None,
        )
        .unwrap();
        let err = same_refusal(
            &e,
            &e,
            source,
            PERSON,
            &p.r#ref,
            "approve",
            &shown.card_hash,
        );
        assert_eq!(err.code, E_PROPOSAL_NOT_READY, "{err:#?}");
    }
}

#[test]
fn ui1_relay_cannot_claim_elicited() {
    let (_d, e, section) = simple();
    let p = proposal(&e, &section, "a");
    let shown = card(&e, PERSON, &p.r#ref);
    let before = untouched(&e);
    let (r#ref, h) = (p.r#ref.clone(), shown.card_hash.clone());
    let err = on(&e, move |r| {
        r.answer(
            &chat(),
            PERSON,
            AnswerRequest {
                r#ref,
                option: "reject".into(),
                card_hash: Some(h),
                note: None,
                relay: RelayInput {
                    relayed_by: None,
                    evidence: Some("yes".into()),
                    question: Some("Shall I?".into()),
                    session_ref: Some("chat-7".into()),
                    kind: Some(EvidenceKind::Elicited),
                },
                brief: None,
            },
        )
    })
    .unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str(), err.fix.as_str()),
        (
            E_BAD_INPUT,
            "a relayed answer's evidence is card or text",
            "pass kind card for an answer on the card, or text for the person's own words"
        )
    );
    assert_eq!(untouched(&e), before);
}

#[test]
fn ui1_ui_signing_is_unsigned_or_refused() {
    // the closed causes: eight, the UI's last
    assert_eq!(NoAgentCause::ALL.len(), 8);
    assert_eq!(NoAgentCause::ALL[7], NoAgentCause::UiUnavailable);
    for c in NoAgentCause::ALL {
        assert_eq!(NoAgentCause::parse(c.as_str()), Some(c), "{c:?}");
    }
    assert_eq!(
        NoAgentCause::UiUnavailable.as_str(),
        "the local UI has no agent socket"
    );
    let (d, e, section) = simple();
    for mode in ["never", "auto"] {
        local_config(d.path(), PERSON, mode, Some(person_ed()), Some(agent_ed()));
        let agent = FakeAgent::new(&[person_ed(), agent_ed()]);
        let p = proposal(&e, &section, &format!("u-{mode}"));
        let shown = card(&e, PERSON, &p.r#ref);
        let v = present_as(
            &e,
            &agent,
            &approver(),
            Presentation::Ui,
            PERSON,
            &p.r#ref,
            &shown.card_hash,
            "approve",
            None,
            None,
        )
        .unwrap();
        assert_eq!(v["signatures"], json!([]), "{mode}: {v:#}");
        assert!(
            signing_warnings(&v).is_empty(),
            "{mode}: no signing warning: {v:#}"
        );
        let (l, _) = landing_of(&e, &v);
        assert!(signatures(&e, &l).is_empty(), "{mode}");
        assert_eq!(agent.calls(), 0, "{mode}: the UI never asks an agent");
    }
    local_config(
        d.path(),
        PERSON,
        "always",
        Some(person_ed()),
        Some(agent_ed()),
    );
    let agent = FakeAgent::new(&[person_ed(), agent_ed()]);
    let p = proposal(&e, &section, "u-always");
    let shown = card(&e, PERSON, &p.r#ref);
    let before = untouched(&e);
    let end = e.read().seq();
    let err = present_as(
        &e,
        &agent,
        &approver(),
        Presentation::Ui,
        PERSON,
        &p.r#ref,
        &shown.card_hash,
        "approve",
        None,
        None,
    )
    .unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str(), err.fix.clone()),
        (
            E_NO_AGENT,
            "the SSH agent could not sign: the local UI has no agent socket",
            ui_fix(&p.r#ref)
        )
    );
    assert_eq!(untouched(&e), before);
    assert_eq!(e.read().seq(), end, "nothing written");
    assert_eq!(agent.calls(), 0);
    // an answer that lands nothing still works under always
    let v = present_as(
        &e,
        &agent,
        &approver(),
        Presentation::Ui,
        PERSON,
        &p.r#ref,
        &shown.card_hash,
        "reject",
        None,
        None,
    )
    .unwrap();
    local_reply(&v, "reject_proposal", "reject");
    assert_eq!(agent.calls(), 0);
}

/// One elicited approval of a new version under the project's settings: the reply or the
/// refusal, checking that a refusal writes nothing.
fn elicited_approval(
    e: &Local,
    section: &str,
    agent: &dyn SigningAgent,
    n: &str,
) -> Result<Value, RdyError> {
    let p = proposal(e, section, n);
    let shown = card(e, PERSON, &p.r#ref);
    let end = e.read().seq();
    let r = present_as(
        e,
        agent,
        &chat(),
        Presentation::Elicited,
        PERSON,
        &p.r#ref,
        &shown.card_hash,
        "approve",
        None,
        None,
    );
    if r.is_err() {
        assert_eq!(e.read().seq(), end, "a refusal writes nothing");
    }
    r
}

#[test]
fn ui1_elicited_signing_uses_only_the_person_key() {
    let (d, e, section) = simple();
    let e = Arc::new(e);
    let n = AtomicUsize::new(0);
    let next = || format!("e{}", n.fetch_add(1, Ordering::SeqCst));
    let only_person = |v: &Value, key: &SoftKey| {
        let (l, _) = landing_of(&*e, v);
        assert_eq!(
            signature_kinds(&*e, &l),
            vec![("approval".to_string(), key.fingerprint())],
            "the person's key alone; no relay signature: {v:#}"
        );
        for g in signatures(&*e, &l) {
            assert_eq!(g.signed_by.actor, "agent:relay", "the caller asked");
        }
        let checks = v["signatures"].as_array().unwrap();
        assert_eq!(checks.len(), 1, "{v:#}");
        assert_eq!(checks[0]["signed"], SIGNED_VALID);
        assert_eq!(checks[0]["statement"], STATEMENT_MATCH);
        assert_eq!(checks[0]["kind"], "approval");
        checks[0].clone()
    };
    // never: no agent call
    local_config(
        d.path(),
        PERSON,
        "never",
        Some(person_ed()),
        Some(agent_ed()),
    );
    let agent = FakeAgent::new(&[person_ed(), agent_ed()]);
    let v = elicited_approval(&e, &section, &agent, &next()).unwrap();
    assert_eq!(v["signatures"], json!([]));
    assert_eq!(agent.calls(), 0);
    // auto and always: the person's key, through the caller's agent
    for mode in ["auto", "always"] {
        local_config(d.path(), PERSON, mode, Some(person_ed()), Some(agent_ed()));
        let agent = FakeAgent::new(&[person_ed(), agent_ed()]);
        let v = elicited_approval(&e, &section, &agent, &next()).unwrap();
        only_person(&v, &person_ed());
        assert_eq!(agent.signs(), 1, "{mode}");
    }
    // no person key: auto is unsigned with no warning, always is refused; the agent key is
    // never used
    local_config(d.path(), PERSON, "auto", None, Some(agent_ed()));
    let agent = FakeAgent::new(&[person_ed(), agent_ed()]);
    let v = elicited_approval(&e, &section, &agent, &next()).unwrap();
    assert_eq!(v["signatures"], json!([]));
    assert!(signing_warnings(&v).is_empty(), "{v:#}");
    assert_eq!(agent.calls(), 0);
    local_config(d.path(), PERSON, "always", None, Some(agent_ed()));
    let err = elicited_approval(&e, &section, &agent, &next()).unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str()),
        (E_SIGNING_KEY_MISSING, "signing.key is not set")
    );
    // a missing agent: auto saves unsigned with the warning, always refuses
    local_config(d.path(), PERSON, "auto", Some(person_ed()), None);
    let v = elicited_approval(&e, &section, &UnavailableAgent, &next()).unwrap();
    assert_eq!(
        signing_warnings(&v),
        vec!["the act was saved without signatures: E_NO_AGENT: the SSH agent could not sign: SSH_AUTH_SOCK is not set".to_string()]
    );
    local_config(d.path(), PERSON, "always", Some(person_ed()), None);
    let err = elicited_approval(&e, &section, &UnavailableAgent, &next()).unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str()),
        (
            E_NO_AGENT,
            "the SSH agent could not sign: SSH_AUTH_SOCK is not set"
        )
    );
    // a key type rdy does not sign with
    local_config(d.path(), PERSON, "always", Some(unknown_key()), None);
    let agent = FakeAgent::new(&[unknown_key()]);
    let err = elicited_approval(&e, &section, &agent, &next()).unwrap_err();
    assert_eq!(err.code, E_SIGNING_KEY_TYPE, "{err:#?}");
    // a FIDO key reports the person's presence
    local_config(d.path(), PERSON, "always", Some(person_sk()), None);
    let agent = FakeAgent::new(&[person_sk()]);
    let v = elicited_approval(&e, &section, &agent, &next()).unwrap();
    let check = only_person(&v, &person_sk());
    assert_eq!(check["presence"], "present", "{check:#}");
    // a cancelled request: always refuses, auto saves unsigned
    local_config(d.path(), PERSON, "always", Some(person_ed()), None);
    let agent = FakeAgent::scripted(&[person_ed()], &[Reply::Cancel]);
    let err = elicited_approval(&e, &section, &agent, &next()).unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str()),
        (
            E_NO_AGENT,
            "the SSH agent could not sign: request cancelled"
        )
    );
    local_config(d.path(), PERSON, "auto", Some(person_ed()), None);
    let agent = FakeAgent::scripted(&[person_ed()], &[Reply::Cancel]);
    let v = elicited_approval(&e, &section, &agent, &next()).unwrap();
    assert_eq!(
        signing_warnings(&v),
        vec!["the act was saved without signatures: E_NO_AGENT: the SSH agent could not sign: request cancelled".to_string()]
    );
    // the project moves while the agent signs: the first signature is discarded and the act
    // prepared and signed again
    local_config(d.path(), PERSON, "always", Some(person_ed()), None);
    let mover = Arc::clone(&e);
    let s2 = section.clone();
    let agent = FakeAgent::hooked(&[person_ed()], move |_| {
        let s2 = s2.clone();
        on(&*mover, move |r| {
            write(
                r,
                "elsewhere",
                json!([{"op": "create", "type": "claim", "parent": s2, "body": "moves the log"}]),
            )
            .map(|_| ())
        })
        .unwrap();
    });
    let v = elicited_approval(&e, &section, &agent, &next()).unwrap();
    only_person(&v, &person_ed());
    assert_eq!(agent.signs(), 2, "signed again after the move");
}

#[test]
fn ui1_a_ui_landing_can_be_signed_later_on_both_surfaces() {
    let (d, e, section) = simple();
    let keys = [person_ed(), agent_ed()];
    let approve = |source: Presentation, n: &str| -> Hash {
        let p = proposal(&e, &section, n);
        let shown = card(&e, PERSON, &p.r#ref);
        let v = present(
            &e,
            source,
            PERSON,
            &p.r#ref,
            &shown.card_hash,
            "approve",
            None,
            None,
        )
        .unwrap();
        landing_of(&e, &v).0
    };
    let ui = approve(Presentation::Ui, "s1");
    let elicited = approve(Presentation::Elicited, "s2");
    local_config(
        d.path(),
        PERSON,
        "never",
        Some(person_ed()),
        Some(agent_ed()),
    );
    for (what, l) in [("ui", ui), ("elicited", elicited)] {
        // as the person
        let agent = FakeAgent::new(&keys);
        let res = run_sign(&e, &agent, &approver(), &l.to_string(), None).unwrap();
        assert_eq!(res.check.kind.as_deref(), Some("approval"), "{what}");
        assert!(!res.already_signed);
        // a relay kind from an agent stays refused
        let err = run_sign(
            &e,
            &agent,
            &chat(),
            &l.to_string(),
            Some(rdy_model::signature::StatementKind::Relay),
        )
        .unwrap_err();
        assert_eq!(err.code, E_PERMISSION, "{what}: {err:#?}");
    }
    // as the MCP agent: the person's approval key, kind approval by default, no relay signature
    let l = approve(Presentation::Elicited, "s3");
    let agent = FakeAgent::new(&keys);
    let res = run_sign(&e, &agent, &chat(), &l.to_string(), None).unwrap();
    assert_eq!(res.check.kind.as_deref(), Some("approval"));
    assert_eq!(
        signature_kinds(&e, &l),
        vec![("approval".to_string(), person_ed().fingerprint())]
    );
    let l = approve(Presentation::Ui, "s4");
    let res = run_sign(&e, &agent, &chat(), &l.to_string(), None).unwrap();
    assert_eq!(res.check.kind.as_deref(), Some("approval"));
    assert_eq!(signature_kinds(&e, &l).len(), 1);

    // another configured person: refused for the agent, and for another person's session
    let l = approve(Presentation::Ui, "s5");
    local_config(
        d.path(),
        "human:other",
        "never",
        Some(person_ed()),
        Some(agent_ed()),
    );
    let err = run_sign(&e, &agent, &chat(), &l.to_string(), None).unwrap_err();
    assert_eq!(err.code, E_PERMISSION, "{err:#?}");
    let other = Session::human("human:other".into(), Role::Approver).unwrap();
    let err = run_sign(&e, &agent, &other, &l.to_string(), None).unwrap_err();
    assert_eq!(err.code, E_PERMISSION, "{err:#?}");
    local_config(
        d.path(),
        PERSON,
        "never",
        Some(person_ed()),
        Some(agent_ed()),
    );
    // a local CLI landing with no evidence
    let p = proposal(&e, &section, "s6");
    let id = p.id.clone();
    let cli = on(&e, move |r| {
        r.land_with(&approver(), &id, LandOptions::default())
    })
    .unwrap();
    let err = run_sign(&e, &agent, &chat(), &cli.landing, None).unwrap_err();
    assert_eq!(err.code, E_PERMISSION, "{err:#?}");
    // a tag
    let tag = run_signed_act(
        &e,
        &agent,
        &approver(),
        PERSON,
        SigningAct::Tag {
            name: "v1.0.0".into(),
            landing: None,
            message: None,
            relay: RelayInput::default(),
            implicit: false,
        },
    )
    .unwrap();
    assert!(tag.is_object() || tag.is_string(), "{tag:#}");
    let err = run_sign(&e, &agent, &chat(), "tag:v1.0.0", None).unwrap_err();
    assert_eq!(err.code, E_PERMISSION, "{err:#?}");
    // a policy approval
    let policy = on(&e, move |r| {
        let mut rec: LandingRecord = r.store().landing(&ui).unwrap();
        rec.read_identity = None;
        rec.assurance = Assurance::Policy;
        rec.changes.clear();
        let (kind, bytes) = rdy_store::codec::item(kinds::LANDING, &rec).unwrap();
        let h = Hash::of(&bytes);
        r.store_mut().write(vec![(kind, bytes)]).unwrap();
        Ok(h)
    })
    .unwrap();
    let err = run_sign(&e, &agent, &chat(), &policy.to_string(), None).unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str()),
        (E_PERMISSION, "policy approvals cannot be signed")
    );
}

#[test]
fn ui1_later_follows_the_surface_rule() {
    let (_d, e, section) = simple();
    let p = proposal(&e, &section, "l");
    let shown = card(&e, PERSON, &p.r#ref);
    // the UI: only after a fresh binding, and nothing is stored
    let end = e.read().seq();
    let v = present(
        &e,
        Presentation::Ui,
        PERSON,
        &p.r#ref,
        &shown.card_hash,
        "later",
        None,
        None,
    )
    .unwrap();
    assert_eq!(v, json!({"state": "later"}));
    assert_eq!(e.read().seq(), end, "a UI Later stores nothing");
    let wrong = Hash::of(b"another card").to_string();
    let r = present(
        &e,
        Presentation::Ui,
        PERSON,
        &p.r#ref,
        &wrong,
        "later",
        None,
        None,
    );
    refused(&r, E_PROPOSAL_MOVED);
    // elicited, from a session with no run: refused
    let no_run = Session::agent("agent:relay".into(), None, Role::Writer).unwrap();
    let before = untouched(&e);
    let err = present_as(
        &e,
        &FakeAgent::new(&[]),
        &no_run,
        Presentation::Elicited,
        PERSON,
        &p.r#ref,
        &shown.card_hash,
        "later",
        None,
        None,
    )
    .unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str(), err.fix.as_str()),
        (
            E_BAD_INPUT,
            "Later needs a run to name the chat session",
            "ask again from a session with a run, or show the card in chat and relay the answer"
        )
    );
    assert_eq!(untouched(&e), before);
    // elicited, from run r1: the snooze names the run, with elicited evidence and no relay
    let v = present(
        &e,
        Presentation::Elicited,
        PERSON,
        &p.r#ref,
        &shown.card_hash,
        "later",
        None,
        None,
    )
    .unwrap();
    local_reply(&v, "later", "later");
    let (_, a) = e
        .read()
        .reader()
        .snooze_of(PERSON, &p.r#ref)
        .unwrap()
        .expect("a snooze");
    assert_eq!(
        (a.act, a.person.actor.as_str(), a.person.run.as_deref()),
        (AnswerAct::Later, PERSON, Some("r1"))
    );
    assert_eq!(a.evidence_kind, Some(EvidenceKind::Elicited));
    assert_eq!(a.card_hash, Some(hash(&shown.card_hash)));
    assert!(a.relayed_by.is_none() && a.relay_evidence.is_none());
    fsck(&e);
    // the queue read with that run hides it; another run, or none, shows it
    let refs = |session: Option<&str>| -> Vec<String> {
        queue_of(&e, PERSON, session)
            .items
            .into_iter()
            .map(|i| i.r#ref)
            .collect()
    };
    assert!(!refs(Some("r1")).contains(&p.r#ref));
    assert!(refs(Some("r2")).contains(&p.r#ref));
    assert!(refs(None).contains(&p.r#ref));
    // a later version shows again in the same run
    let newer = proposal(&e, &section, "l");
    assert!(refs(Some("r1")).contains(&newer.r#ref));
    // the CLI keeps its refusal
    let (r#ref, h) = (
        newer.r#ref.clone(),
        card(&e, PERSON, &newer.r#ref).card_hash,
    );
    let err = on(&e, move |r| {
        r.answer(
            &approver(),
            PERSON,
            AnswerRequest {
                r#ref,
                option: "later".into(),
                card_hash: Some(h),
                ..AnswerRequest::default()
            },
        )
    })
    .unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str(), err.fix.as_str()),
        (
            E_BAD_INPUT,
            "Later needs a chat session; the CLI has no Later",
            "answer with another option, or let the agent relay Later from chat"
        )
    );
}

#[test]
fn ui1_lookup_reaches_a_card_beyond_the_pending_cap() {
    let (_d, e, section) = simple();
    let n = rdy_engine::pending::queue::PENDING_ITEMS_MAX + 2;
    let refs: Vec<String> = (0..n)
        .map(|i| proposal(&e, &section, &format!("cap{i}")).r#ref)
        .collect();
    let shown: Vec<String> = queue_of(&e, PERSON, None)
        .items
        .into_iter()
        .map(|i| i.r#ref)
        .collect();
    assert!(shown.len() <= rdy_engine::pending::queue::PENDING_ITEMS_MAX);
    let beyond = refs
        .iter()
        .find(|r| !shown.contains(r))
        .expect("a card past the cap");
    let item = presented_item(&e.read(), PERSON, beyond, None).unwrap();
    assert_eq!(&item.r#ref, beyond);
    assert_eq!(item.kind, ItemKind::Approve);
    // the same full hash binds it again, and an answer to it is bound as shown
    let again = presented_item(&e.read(), PERSON, beyond, Some(&item.card_hash)).unwrap();
    assert_eq!(
        (&again.card_hash, &again.card),
        (&item.card_hash, &item.card)
    );
    let v = present(
        &e,
        Presentation::Ui,
        PERSON,
        beyond,
        &item.card_hash,
        "reject",
        None,
        None,
    )
    .unwrap();
    local_reply(&v, "reject_proposal", "reject");
    // a hash that is not the card's: the moved refusal with the fresh card
    let wrong = Hash::of(b"another card").to_string();
    // a shown card, never `beyond`: cards raised in one second order by ref, so refs[0] can be past the cap
    let err = presented_item(&e.read(), PERSON, &shown[0], Some(&wrong)).unwrap_err();
    assert_eq!(err.code, E_PROPOSAL_MOVED, "{err:#?}");
    assert!(err.corrected.is_some());
    // an answered item is gone
    let err = presented_item(&e.read(), PERSON, beyond, None).unwrap_err();
    assert_eq!(err.code, E_ITEM_MOVED, "{err:#?}");
    // never by title, only by ref
    let err = presented_item(&e.read(), PERSON, "cap0", None).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err:#?}");

    // its role holds
    let (_d, e, p) = policed_world();
    let item = presented_item(&e.read(), PERSON, &p.r#ref, None).unwrap();
    assert_eq!(item.r#ref, p.r#ref);
    let err = presented_item(&e.read(), "human:other", &p.r#ref, None).unwrap_err();
    assert_eq!(
        (err.code, err.message.clone()),
        (
            E_PERMISSION,
            format!(
                "human:other is not in the policy role approvers for {}",
                p.r#ref
            )
        )
    );
}
