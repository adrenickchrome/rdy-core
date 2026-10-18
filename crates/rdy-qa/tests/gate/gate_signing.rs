//! sig8 (D-1Z951, D-17AA8, D-QVH4N, D-F0904, D-KQ4CA): rdy signs an approval or a tag before
//! its one commit. The act is prepared under the writer, the caller's agent signs the exact
//! prospective record with the writer free, and the act and its signatures are written in one
//! batch, or nothing is. Canon that moves meanwhile discards the signatures and prepares again;
//! a changed card or redaction report is refused, never re-approved. Later signing checks the
//! person, the kind, the relay and duplicates. Every key here is a fixed in-memory test key.
use ed25519_dalek::Signer as _;
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::engine::{Engine, Local, with_writer_result};
use rdy_engine::pending::{CARD_MAX_TOKENS, PendingQuery};
use rdy_engine::redaction::RedactInput;
use rdy_engine::signature_check::{SIGNED_VALID, STATEMENT_MATCH, signature_checks};
use rdy_engine::signing_act::{SigningAct, run_sign, run_signed_act};
use rdy_engine::signing_crypto::{AgentIdentity, SigningAgent, blob_fingerprint};
use rdy_engine::{
    E_BAD_INPUT, E_NO_AGENT, E_PERMISSION, E_PROPOSAL_MOVED, E_REDACTED_STATE, E_REDACTION_HUMAN,
    E_REDACTION_STALE, E_SIGNING_KEY_MISSING, E_STALE_BASE, LandOptions, NoAgentCause, Rdy,
    RdyError, Relay, Scope, Session, W_SIGNING_FAILED, no_agent,
};
use rdy_model::redaction::CONTENT_REMOVED;
use rdy_model::signature::{Signature, StatementKind, parse_statement};
use rdy_model::{ActorRun, Assurance, Hash, LandingRecord, RelayEvidence, Role};
use rdy_qa::fixtures::{approver, seed_doc, write};
use rdy_qa::intake::PERSON;
use rdy_store::kinds;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use ssh_key::sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

// ---- fixed in-memory test keys ----

const PERSON_SEED: [u8; 32] = [7; 32];
const AGENT_SEED: [u8; 32] = [9; 32];
const OTHER_SEED: [u8; 32] = [11; 32];
const SK_APP: &[u8] = b"ssh:";
const COUNTER: u32 = 42;

#[derive(Clone, Copy, Debug, PartialEq)]
enum KeyKind {
    Ed,
    Sk,
}

#[derive(Clone, Copy, Debug)]
struct SoftKey {
    seed: [u8; 32],
    kind: KeyKind,
}

fn ssh_string(bytes: &[u8]) -> Vec<u8> {
    let mut out = (bytes.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(bytes);
    out
}

impl SoftKey {
    fn dalek(&self) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&self.seed)
    }

    fn public(&self) -> [u8; 32] {
        self.dalek().verifying_key().to_bytes()
    }

    /// The SSH public-key blob.
    fn blob(&self) -> Vec<u8> {
        match self.kind {
            KeyKind::Ed => [ssh_string(b"ssh-ed25519"), ssh_string(&self.public())].concat(),
            KeyKind::Sk => [
                ssh_string(b"sk-ssh-ed25519@openssh.com"),
                ssh_string(&self.public()),
                ssh_string(SK_APP),
            ]
            .concat(),
        }
    }

    fn fingerprint(&self) -> String {
        blob_fingerprint(&self.blob())
    }

    /// The agent's sign reply over `payload`: the SSH signature blob (an sk key's carries the
    /// user-presence flag and a counter).
    fn sign(&self, payload: &[u8]) -> Vec<u8> {
        match self.kind {
            KeyKind::Ed => {
                let raw = self.dalek().sign(payload).to_bytes();
                [ssh_string(b"ssh-ed25519"), ssh_string(&raw)].concat()
            }
            KeyKind::Sk => {
                let flags = 1u8;
                let mut m = Sha256::digest(SK_APP).to_vec();
                m.push(flags);
                m.extend(COUNTER.to_be_bytes());
                m.extend(Sha256::digest(payload));
                let raw = self.dalek().sign(&m).to_bytes();
                let mut out =
                    [ssh_string(b"sk-ssh-ed25519@openssh.com"), ssh_string(&raw)].concat();
                out.push(flags);
                out.extend(COUNTER.to_be_bytes());
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
fn agent_sk() -> SoftKey {
    SoftKey {
        seed: AGENT_SEED,
        kind: KeyKind::Sk,
    }
}

/// The SSHSIG signed data `payload` with its namespace swapped for another rdy namespace.
fn swap_namespace(payload: &[u8]) -> Vec<u8> {
    assert_eq!(&payload[..6], b"SSHSIG");
    let n = u32::from_be_bytes(payload[6..10].try_into().unwrap()) as usize;
    let ns = &payload[10..10 + n];
    let other: &[u8] = if ns == b"rdy-tag-v1" {
        b"rdy-approval-v1"
    } else {
        b"rdy-tag-v1"
    };
    [&payload[..6], &ssh_string(other), &payload[10 + n..]].concat()
}

// ---- the fake agent ----

/// What the fake agent answers to one sign request.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Reply {
    Good,
    Refuse,
    /// Signed by a key the request did not name.
    OtherKey,
    /// Signed over other bytes.
    OtherPayload,
    /// Signed over the same statement in another namespace.
    OtherNamespace,
}

type Hook = Box<dyn FnMut(usize) + Send>;

/// An agent holding `keys`, answering sign requests by `script` (then `Good`), counting every
/// call, and running `hook` (with the sign call's number) before it answers.
struct FakeAgent {
    keys: Vec<SoftKey>,
    script: Mutex<VecDeque<Reply>>,
    identities: AtomicUsize,
    signs: AtomicUsize,
    hook: Mutex<Option<Hook>>,
    keep_hook: bool,
}

impl FakeAgent {
    fn new(keys: &[SoftKey]) -> FakeAgent {
        FakeAgent {
            keys: keys.to_vec(),
            script: Mutex::new(VecDeque::new()),
            identities: AtomicUsize::new(0),
            signs: AtomicUsize::new(0),
            hook: Mutex::new(None),
            keep_hook: false,
        }
    }

    fn scripted(keys: &[SoftKey], script: &[Reply]) -> FakeAgent {
        let a = FakeAgent::new(keys);
        a.script.lock().unwrap().extend(script.iter().copied());
        a
    }

    /// The agent, with `hook` run before the first sign reply (or every one, with `every`).
    fn hooked(keys: &[SoftKey], every: bool, hook: impl FnMut(usize) + Send + 'static) -> Self {
        let mut a = FakeAgent::new(keys);
        a.keep_hook = every;
        *a.hook.lock().unwrap() = Some(Box::new(hook));
        a
    }

    fn calls(&self) -> usize {
        self.identities.load(Ordering::SeqCst) + self.signs.load(Ordering::SeqCst)
    }

    fn signs(&self) -> usize {
        self.signs.load(Ordering::SeqCst)
    }
}

impl SigningAgent for FakeAgent {
    fn identities(&self) -> Result<Vec<AgentIdentity>, RdyError> {
        self.identities.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .keys
            .iter()
            .map(|k| AgentIdentity {
                public_key: k.blob(),
            })
            .collect())
    }

    fn sign(&self, public_key: &[u8], payload: &[u8]) -> Result<Vec<u8>, RdyError> {
        let n = self.signs.fetch_add(1, Ordering::SeqCst);
        let hook = self.hook.lock().unwrap().take();
        if let Some(mut h) = hook {
            h(n);
            if self.keep_hook {
                *self.hook.lock().unwrap() = Some(h);
            }
        }
        let reply = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Reply::Good);
        let key = *self
            .keys
            .iter()
            .find(|k| k.blob() == public_key)
            .expect("the fake agent holds the key it was asked for");
        Ok(match reply {
            Reply::Good => key.sign(payload),
            Reply::Refuse => return Err(no_agent(NoAgentCause::Refused)),
            Reply::OtherKey => SoftKey {
                seed: OTHER_SEED,
                kind: key.kind,
            }
            .sign(payload),
            Reply::OtherPayload => key.sign(&[payload, b"x"].concat()),
            Reply::OtherNamespace => key.sign(&swap_namespace(payload)),
        })
    }
}

// ---- the project ----

/// The relaying agent's session.
fn relayer() -> Session {
    Session::agent("agent:relay".into(), Some("r1".into()), Role::Writer).expect("agent")
}

/// Runs `f` on the engine's writer.
fn on<T: Send + 'static>(e: &Local, f: impl FnOnce(&mut Rdy) -> T + Send + 'static) -> T {
    with_writer_result(e, move |r| Ok(f(r))).expect("write")
}

struct Proj {
    dir: tempfile::TempDir,
    e: Arc<Local>,
    section: String,
    n: AtomicUsize,
}

/// A project whose canon holds a seed doc, with signing set to `never` and no keys.
fn project() -> Proj {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut r = Rdy::init(dir.path(), "rdy-standard").expect("init");
    let (_, section, _) = seed_doc(&mut r, "seed");
    let p = r
        .propose(&rdy_qa::fixtures::writer(), "seed", "Seed", Some("Seeds."))
        .expect("propose seed");
    r.land(&approver(), &p.id).expect("land seed");
    let proj = Proj {
        dir,
        e: Arc::new(Local::new(r)),
        section,
        n: AtomicUsize::new(0),
    };
    proj.config("never", None, None);
    proj
}

/// The signing settings of `.rdy/local.yaml` (null keys are unset).
fn local_config(dir: &Path, sign: &str, key: Option<SoftKey>, agent_key: Option<SoftKey>) {
    let sel = |k: Option<SoftKey>| match k {
        Some(k) => format!("\"{}\"", k.fingerprint()),
        None => "null".into(),
    };
    std::fs::write(
        dir.join(".rdy/local.yaml"),
        format!(
            "signing:\n  key: {}\n  agent_key: {}\n  sign: {sign}\n  allowed_signers: null\n",
            sel(key),
            sel(agent_key)
        ),
    )
    .expect("local config");
}

impl Proj {
    fn config(&self, sign: &str, key: Option<SoftKey>, agent_key: Option<SoftKey>) {
        local_config(self.dir.path(), sign, key, agent_key);
    }

    /// A ready proposal of a new claim in a view of its own: `(version hash, P-x@n)`.
    fn proposal(&self) -> (String, String) {
        let n = self.n.fetch_add(1, Ordering::SeqCst);
        new_proposal(&self.e, &self.section, &format!("v{n}"))
    }

    /// A version name above every earlier one.
    fn version(&self) -> String {
        format!("v1.0.{}", self.n.fetch_add(1, Ordering::SeqCst))
    }

    fn log_end(&self) -> Location {
        on(&self.e, |r| r.store().log_end())
    }

    fn signature_count(&self) -> usize {
        on(&self.e, |r| {
            let mut n = 0;
            r.store()
                .for_each_object_of(kinds::SIGNATURE, |_| {
                    n += 1;
                    Ok(())
                })
                .unwrap();
            n
        })
    }

    fn approve_local(&self, agent: &FakeAgent, id: &str) -> Result<Value, RdyError> {
        run_signed_act(
            &*self.e,
            agent,
            &approver(),
            PERSON,
            SigningAct::Approve {
                proposal: id.into(),
                options: LandOptions::default(),
            },
        )
    }

    fn approve_relayed(&self, agent: &FakeAgent, id: &str) -> Result<Value, RdyError> {
        run_signed_act(
            &*self.e,
            agent,
            &relayer(),
            PERSON,
            SigningAct::Approve {
                proposal: id.into(),
                options: relayed_options(),
            },
        )
    }

    fn approve_by_card(&self, agent: &FakeAgent, r#ref: &str) -> Result<Value, RdyError> {
        let h = card_hash(&self.e, r#ref);
        run_signed_act(
            &*self.e,
            agent,
            &relayer(),
            PERSON,
            SigningAct::Answer(AnswerRequest {
                r#ref: r#ref.into(),
                option: "approve".into(),
                card_hash: Some(h),
                note: None,
                relay: words(),
                brief: None,
            }),
        )
    }

    fn tag_local(&self, agent: &FakeAgent, name: &str) -> Result<Value, RdyError> {
        run_signed_act(
            &*self.e,
            agent,
            &approver(),
            PERSON,
            SigningAct::Tag {
                name: name.into(),
                landing: None,
                message: None,
                relay: RelayInput::default(),
                implicit: false,
            },
        )
    }

    fn tag_relayed(&self, agent: &FakeAgent, name: &str) -> Result<Value, RdyError> {
        run_signed_act(
            &*self.e,
            agent,
            &relayer(),
            PERSON,
            SigningAct::Tag {
                name: name.into(),
                landing: None,
                message: None,
                relay: words(),
                implicit: true,
            },
        )
    }
}

/// A claim in view `view` under `section`, proposed with a summary: `(version hash, P-x@n)`.
fn new_proposal(e: &Local, section: &str, view: &str) -> (String, String) {
    let (section, view) = (section.to_string(), view.to_string());
    on(e, move |r| {
        write(
            r,
            &view,
            json!([{"op": "create", "type": "claim", "parent": section, "body": format!("A claim of {view}.")}]),
        )
        .expect("write claim");
        let p = r
            .propose(
                &rdy_qa::fixtures::writer(),
                &view,
                &view,
                Some("Adds one claim."),
            )
            .expect("propose");
        (p.id, p.r#ref)
    })
}

/// A write to a view of its own that moves the log and lands nothing.
fn move_log(e: &Local, section: &str, view: &str) {
    let (section, view) = (section.to_string(), view.to_string());
    on(e, move |r| {
        write(
            r,
            &view,
            json!([{"op": "create", "type": "claim", "parent": section, "body": "moves the log"}]),
        )
        .expect("move the log");
    });
}

/// Lands a new proposal of view `view` as the person, unsigned: canon moves.
fn move_canon(e: &Local, section: &str, view: &str) -> String {
    let (id, _) = new_proposal(e, section, view);
    on(e, move |r| r.land(&approver(), &id).expect("land"))
}

fn relayed_options() -> LandOptions {
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
        evidence: None,
    }
}

fn words() -> RelayInput {
    RelayInput {
        relayed_by: None,
        evidence: Some("Yes, go ahead.".into()),
        question: Some("Shall I record it?".into()),
        session_ref: Some("chat-7".into()),
        kind: None,
    }
}

/// The card hash the queue shows for `r#ref` now.
fn card_hash(e: &Local, r#ref: &str) -> String {
    e.read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .expect("pending")
        .items
        .into_iter()
        .find(|i| i.r#ref == r#ref)
        .unwrap_or_else(|| panic!("{ref} is not in the queue"))
        .card_hash
}

/// The subject an act's result names: the landing of an approval or an answer, the tag of a
/// tag.
fn subject(v: &Value) -> Hash {
    let text = if v["landing"].is_object() {
        v["landing"]["landing"].as_str()
    } else if v["tag"].is_string() {
        v["tag"].as_str()
    } else {
        v["landing"].as_str()
    };
    text.unwrap_or_else(|| panic!("no subject in {v:#}"))
        .parse()
        .expect("hash")
}

/// The stored signatures of `subject`, decoded.
fn stored(e: &Local, subject: &Hash) -> Vec<Signature> {
    let st = e.read();
    st.reader()
        .signatures_of(subject)
        .expect("signatures_of")
        .iter()
        .map(|h| st.reader().signature(h).expect("signature"))
        .collect()
}

/// The statement kinds and key fingerprints of `sigs`, sorted.
fn kinds_keys(sigs: &[Signature]) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = sigs
        .iter()
        .map(|g| {
            let p = parse_statement(&g.statement).expect("statement");
            (p.kind().as_str(), p.field("key").expect("key").to_string())
        })
        .collect();
    out.sort();
    out
}

/// `(kind, key)` of each check in the result's `signatures`, in result order.
fn result_signatures(v: &Value) -> Vec<(String, String)> {
    v["signatures"]
        .as_array()
        .unwrap_or_else(|| panic!("no signatures in {v:#}"))
        .iter()
        .map(|c| {
            assert_eq!(c["signed"], SIGNED_VALID, "{c:#}");
            assert_eq!(c["statement"], STATEMENT_MATCH, "{c:#}");
            (
                c["kind"].as_str().unwrap().to_string(),
                c["key"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// The `W_SIGNING_FAILED` warnings of a result, as `(code, message)`.
fn signing_warnings(v: &Value) -> Vec<String> {
    let list = if v["landing"].is_object() {
        &v["landing"]["warnings"]
    } else {
        &v["warnings"]
    };
    list.as_array()
        .map(|w| {
            w.iter()
                .filter(|w| w["code"] == W_SIGNING_FAILED)
                .map(|w| {
                    assert_eq!(
                        w["fix"], "fix the signing error, then run rdy sign",
                        "{w:#}"
                    );
                    w["message"].as_str().unwrap().to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn failed(code: &str, message: &str) -> String {
    format!("the act was saved without signatures: {code}: {message}")
}

fn key_missing(setting: &str) -> (String, String) {
    (
        format!("{setting} is not set"),
        format!("set {setting} in the user config or .rdy/local.yaml"),
    )
}

fn assert_err(e: &RdyError, code: &str, message: &str) {
    assert_eq!((e.code, e.message.as_str()), (code, message), "{e}");
}

/// What one act did, as the mode table states it.
#[derive(Clone, Debug, PartialEq)]
enum Outcome {
    /// Saved with no signature and no warning.
    Unsigned,
    /// Saved with no signature and one W_SIGNING_FAILED of this code and message.
    Warned(String, String),
    /// Saved with these (kind, key fingerprint) signatures, in result order.
    Signed(Vec<(String, String)>),
    /// Refused with this code and message; nothing written.
    Refused(&'static str, String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Act {
    Approval,
    Tag,
}

/// Runs one `act` (relayed or local) under the project's config with `agent`, and reports the
/// outcome, checking that a refusal wrote nothing and that the store holds exactly the
/// result's signatures.
fn outcome(p: &Proj, agent: &FakeAgent, act: Act, relayed: bool) -> Outcome {
    let target = match act {
        Act::Approval => p.proposal().0,
        Act::Tag => p.version(),
    };
    let end = p.log_end();
    let before = p.signature_count();
    let r = match (act, relayed) {
        (Act::Approval, false) => p.approve_local(agent, &target),
        (Act::Approval, true) => p.approve_relayed(agent, &target),
        (Act::Tag, false) => p.tag_local(agent, &target),
        (Act::Tag, true) => p.tag_relayed(agent, &target),
    };
    match r {
        Err(e) => {
            assert_eq!(p.log_end(), end, "a refused act writes nothing: {e}");
            Outcome::Refused(e.code, e.message)
        }
        Ok(v) => {
            let subject = subject(&v);
            let sigs = result_signatures(&v);
            let stored = stored(&p.e, &subject);
            assert_eq!(p.signature_count() - before, stored.len(), "{v:#}");
            let mut from_result: Vec<(&str, String)> =
                sigs.iter().map(|(k, f)| (k.as_str(), f.clone())).collect();
            from_result.sort();
            assert_eq!(kinds_keys(&stored), from_result, "{v:#}");
            let warnings = signing_warnings(&v);
            match (sigs.is_empty(), warnings.as_slice()) {
                (true, []) => Outcome::Unsigned,
                (true, [w]) => {
                    let rest = w
                        .strip_prefix("the act was saved without signatures: ")
                        .expect("warning text");
                    let (code, message) = rest.split_once(": ").expect("code: message");
                    Outcome::Warned(code.to_string(), message.to_string())
                }
                (false, []) => Outcome::Signed(sigs),
                other => panic!("signed and warned at once: {other:?} {v:#}"),
            }
        }
    }
}

fn signed(pairs: &[(&str, &SoftKey)]) -> Outcome {
    Outcome::Signed(
        pairs
            .iter()
            .map(|(k, key)| (k.to_string(), key.fingerprint()))
            .collect(),
    )
}

#[test]
fn sig8_every_mode_has_the_closed_act_outcome() {
    let p = project();
    let refused = || "the SSH agent could not sign: request refused".to_string();
    let (key_msg, _) = key_missing("signing.key");
    let (agent_msg, _) = key_missing("signing.agent_key");
    for act in [Act::Approval, Act::Tag] {
        let (pkind, what) = match act {
            Act::Approval => ("approval", "approval"),
            Act::Tag => ("tag", "tag"),
        };
        // 1. a human's local act, person key unset
        for (mode, want) in [
            ("never", Outcome::Unsigned),
            ("auto", Outcome::Unsigned),
            (
                "always",
                Outcome::Refused(E_SIGNING_KEY_MISSING, key_msg.clone()),
            ),
        ] {
            p.config(mode, None, Some(agent_ed()));
            let agent = FakeAgent::new(&[person_ed(), agent_ed()]);
            assert_eq!(outcome(&p, &agent, act, false), want, "{what} local {mode}");
            assert_eq!(agent.calls(), 0, "{what} local {mode}: no agent call");
        }
        // 2. a human's local act, person key set: both key types
        for person in [person_ed(), person_sk()] {
            p.config("never", Some(person), None);
            let agent = FakeAgent::new(&[person]);
            assert_eq!(outcome(&p, &agent, act, false), Outcome::Unsigned);
            assert_eq!(agent.calls(), 0, "never asks no agent");
            for mode in ["auto", "always"] {
                p.config(mode, Some(person), None);
                let agent = FakeAgent::new(&[person]);
                assert_eq!(
                    outcome(&p, &agent, act, false),
                    signed(&[(pkind, &person)]),
                    "{what} local {mode} {person:?}"
                );
            }
            p.config("auto", Some(person), None);
            let agent = FakeAgent::scripted(&[person], &[Reply::Refuse]);
            assert_eq!(
                outcome(&p, &agent, act, false),
                Outcome::Warned(E_NO_AGENT.into(), refused()),
                "{what} local auto failure"
            );
            p.config("always", Some(person), None);
            let agent = FakeAgent::scripted(&[person], &[Reply::Refuse]);
            assert_eq!(
                outcome(&p, &agent, act, false),
                Outcome::Refused(E_NO_AGENT, refused()),
                "{what} local always failure"
            );
        }
        // 3. a relayed act, both keys unset
        for (mode, want) in [
            ("never", Outcome::Unsigned),
            ("auto", Outcome::Unsigned),
            (
                "always",
                Outcome::Refused(E_SIGNING_KEY_MISSING, agent_msg.clone()),
            ),
        ] {
            p.config(mode, None, None);
            let agent = FakeAgent::new(&[person_ed(), agent_ed()]);
            assert_eq!(
                outcome(&p, &agent, act, true),
                want,
                "{what} relayed {mode}"
            );
            assert_eq!(agent.calls(), 0, "{what} relayed {mode}: no agent call");
        }
        // 4. a relayed act, agent key set, person key unset: both key types
        for machine in [agent_ed(), agent_sk()] {
            p.config("never", None, Some(machine));
            let agent = FakeAgent::new(&[machine]);
            assert_eq!(outcome(&p, &agent, act, true), Outcome::Unsigned);
            assert_eq!(agent.calls(), 0);
            for mode in ["auto", "always"] {
                p.config(mode, None, Some(machine));
                let agent = FakeAgent::new(&[machine]);
                assert_eq!(
                    outcome(&p, &agent, act, true),
                    signed(&[("relay", &machine)]),
                    "{what} relayed {mode} {machine:?}"
                );
            }
            p.config("auto", None, Some(machine));
            let agent = FakeAgent::scripted(&[machine], &[Reply::Refuse]);
            assert_eq!(
                outcome(&p, &agent, act, true),
                Outcome::Warned(E_NO_AGENT.into(), refused())
            );
            p.config("always", None, Some(machine));
            let agent = FakeAgent::scripted(&[machine], &[Reply::Refuse]);
            assert_eq!(
                outcome(&p, &agent, act, true),
                Outcome::Refused(E_NO_AGENT, refused())
            );
        }
        // 5. a relayed act, both keys set: relay first, then person; the first failure
        // discards both
        for (person, machine) in [(person_sk(), agent_ed()), (person_ed(), agent_sk())] {
            p.config("never", Some(person), Some(machine));
            let agent = FakeAgent::new(&[person, machine]);
            assert_eq!(outcome(&p, &agent, act, true), Outcome::Unsigned);
            assert_eq!(agent.calls(), 0);
            for mode in ["auto", "always"] {
                p.config(mode, Some(person), Some(machine));
                let agent = FakeAgent::new(&[person, machine]);
                assert_eq!(
                    outcome(&p, &agent, act, true),
                    signed(&[("relay", &machine), (pkind, &person)]),
                    "{what} relayed {mode} both"
                );
                assert_eq!(agent.signs(), 2);
            }
            // the second signature fails: no half pair
            p.config("auto", Some(person), Some(machine));
            let agent = FakeAgent::scripted(&[person, machine], &[Reply::Good, Reply::Refuse]);
            assert_eq!(
                outcome(&p, &agent, act, true),
                Outcome::Warned(E_NO_AGENT.into(), refused()),
                "{what} relayed auto second failure"
            );
            assert_eq!(agent.signs(), 2);
            // the first fails: the person is never asked
            let agent = FakeAgent::scripted(&[person, machine], &[Reply::Refuse]);
            assert_eq!(
                outcome(&p, &agent, act, true),
                Outcome::Warned(E_NO_AGENT.into(), refused())
            );
            assert_eq!(agent.signs(), 1, "discarded on the first failure");
            p.config("always", Some(person), Some(machine));
            let agent = FakeAgent::scripted(&[person, machine], &[Reply::Good, Reply::Refuse]);
            assert_eq!(
                outcome(&p, &agent, act, true),
                Outcome::Refused(E_NO_AGENT, refused()),
                "{what} relayed always second failure"
            );
        }
        // 6. a relayed act, person key set, agent key unset or ignored
        for (agent_key, why) in [(None, "unset"), (Some(person_ed()), "equal, so ignored")] {
            p.config("never", Some(person_ed()), agent_key);
            let agent = FakeAgent::new(&[person_ed()]);
            assert_eq!(outcome(&p, &agent, act, true), Outcome::Unsigned, "{why}");
            p.config("auto", Some(person_ed()), agent_key);
            let agent = FakeAgent::new(&[person_ed()]);
            assert_eq!(
                outcome(&p, &agent, act, true),
                Outcome::Warned(E_SIGNING_KEY_MISSING.into(), agent_msg.clone()),
                "{what} relayed auto, agent key {why}"
            );
            assert_eq!(agent.calls(), 0, "no agent call: {why}");
            p.config("always", Some(person_ed()), agent_key);
            let agent = FakeAgent::new(&[person_ed()]);
            assert_eq!(
                outcome(&p, &agent, act, true),
                Outcome::Refused(E_SIGNING_KEY_MISSING, agent_msg.clone()),
                "{what} relayed always, agent key {why}"
            );
            assert_eq!(agent.calls(), 0);
        }
    }
    // the exact warning text of row 6
    p.config("auto", Some(person_ed()), None);
    let agent = FakeAgent::new(&[person_ed()]);
    let v = p.approve_relayed(&agent, &p.proposal().0).unwrap();
    assert_eq!(
        signing_warnings(&v),
        vec![failed(
            "E_SIGNING_KEY_MISSING",
            "signing.agent_key is not set"
        )]
    );
    assert_eq!(v["signatures"], json!([]), "unsigned: an empty list");

    // 7. an answer that makes no landing or tag is never signed and its result is unchanged
    for mode in ["never", "auto", "always"] {
        p.config(mode, Some(person_ed()), Some(agent_ed()));
        let agent = FakeAgent::new(&[person_ed(), agent_ed()]);
        let (_, r#ref) = p.proposal();
        let h = card_hash(&p.e, &r#ref);
        let v = run_signed_act(
            &*p.e,
            &agent,
            &relayer(),
            PERSON,
            SigningAct::Answer(AnswerRequest {
                r#ref: r#ref.clone(),
                option: "reject".into(),
                card_hash: Some(h),
                note: None,
                relay: words(),
                brief: None,
            }),
        )
        .unwrap();
        assert_eq!(v["act"], "reject_proposal", "{v:#}");
        assert!(v.get("signatures").is_none(), "unchanged result: {v:#}");
        assert_eq!(agent.calls(), 0, "{mode}: no agent call");
    }

    // with no signing settings at all, nothing contacts an agent
    std::fs::write(
        p.dir.path().join(".rdy/local.yaml"),
        "signing:\n  key: null\n  agent_key: null\n  allowed_signers: null\n",
    )
    .unwrap();
    let agent = FakeAgent::new(&[]);
    assert_eq!(outcome(&p, &agent, Act::Approval, true), Outcome::Unsigned);
    assert_eq!(outcome(&p, &agent, Act::Tag, false), Outcome::Unsigned);
    assert_eq!(agent.calls(), 0);
}

#[test]
fn sig8_the_subject_and_signatures_share_one_sealed_batch() {
    let p = project();
    let (person, machine) = (person_sk(), agent_ed());
    let agent = FakeAgent::new(&[person, machine]);
    // the act's subject, its signatures and (for a relayed tag) its ANSWER are in the last
    // sealed batch, and every signature covers the stored subject
    let check = |v: &Value, want: Vec<(&str, String)>| {
        let subject = subject(v);
        let batch: Vec<Hash> = on(&p.e, |r| {
            r.store().last_batch().iter().map(|(_, h)| *h).collect()
        });
        let sigs = stored(&p.e, &subject);
        assert_eq!(kinds_keys(&sigs), want, "{v:#}");
        assert!(batch.contains(&subject), "the subject is in the batch");
        for g in &sigs {
            assert!(batch.contains(&g.id()), "the signature is in the batch");
            assert_eq!(g.subject, subject);
        }
        let checks = signature_checks(&p.e.read(), &subject, None).unwrap();
        assert!(
            checks
                .iter()
                .all(|c| c.signed == SIGNED_VALID && c.statement == STATEMENT_MATCH),
            "{checks:#?}"
        );
        assert_eq!(
            v["signatures"].as_array().unwrap().len(),
            checks.len(),
            "{v:#}"
        );
        (subject, sigs, batch)
    };

    // a local approval
    p.config("auto", Some(person), Some(machine));
    let v = p.approve_local(&agent, &p.proposal().0).unwrap();
    let (l, _, _) = check(&v, vec![("approval", person.fingerprint())]);
    let rec = p.e.read().reader().landing(&l).unwrap();
    assert_eq!(rec.assurance, Assurance::Local);
    let g = &stored(&p.e, &l)[0];
    assert_eq!(g.signed_by.actor, "human:qa", "the asking session");

    // a text relay of an approval
    let v = p.approve_relayed(&agent, &p.proposal().0).unwrap();
    let both = vec![
        ("approval", person.fingerprint()),
        ("relay", machine.fingerprint()),
    ];
    let (l, sigs, _) = check(&v, both.clone());
    let rec = p.e.read().reader().landing(&l).unwrap();
    assert_eq!(rec.assurance, Assurance::Relayed);
    assert_eq!(
        rec.approved_by.actor, PERSON,
        "the stored originator is the person"
    );
    for g in &sigs {
        assert_eq!(
            (g.signed_by.actor.as_str(), g.signed_by.run.as_deref()),
            ("agent:relay", Some("r1")),
            "signed_by is the agent's provenance"
        );
    }
    let order: Vec<String> = result_signatures(&v).into_iter().map(|(k, _)| k).collect();
    assert_eq!(order, ["relay", "approval"], "relay before person");

    // a card relay of an approval (rdy_answer)
    let (_, r#ref) = p.proposal();
    let v = p.approve_by_card(&agent, &r#ref).unwrap();
    assert_eq!(v["act"], "approve");
    let (l, sigs, _) = check(&v, both.clone());
    let rec = p.e.read().reader().landing(&l).unwrap();
    let card = rec.evidence.as_ref().expect("card evidence");
    assert!(card.card_hash.is_some());
    for g in &sigs {
        let st = parse_statement(&g.statement).unwrap();
        assert_eq!(
            st.field("proposal_version"),
            rec.approved_version.map(|h| h.to_string()).as_deref()
        );
    }

    // a local tag
    let name = p.version();
    let v = p.tag_local(&agent, &name).unwrap();
    let (t, sigs, _) = check(&v, vec![("tag", person.fingerprint())]);
    assert_eq!(v["name"], name.as_str());
    assert!(
        v.get("relayed_by").is_none(),
        "a local tag keeps its result: {v:#}"
    );
    let st = parse_statement(&sigs[0].statement).unwrap();
    assert_eq!(st.field("tag"), Some(t.to_string().as_str()));

    // a relayed tag: the ANSWER is in the same batch, and the relay statement names it
    let name = p.version();
    let v = p.tag_relayed(&agent, &name).unwrap();
    let (t, sigs, batch) = check(
        &v,
        vec![
            ("relay", machine.fingerprint()),
            ("tag", person.fingerprint()),
        ],
    );
    let (aid, a) =
        p.e.read()
            .reader()
            .answer_of(&format!("tag:{name}"))
            .unwrap()
            .expect("the relay's answer");
    assert_eq!(v["answer"], aid.to_string());
    assert_eq!(a.targets, vec![t], "the answer targets the tag");
    assert!(batch.contains(&aid), "the answer is in the batch");
    let relay = sigs
        .iter()
        .map(|g| parse_statement(&g.statement).unwrap())
        .find(|s| s.kind() == StatementKind::Relay)
        .unwrap()
        .field("answer")
        .map(str::to_string);
    assert_eq!(relay, Some(aid.to_string()));
    let at = |h: &Hash| batch.iter().position(|x| x == h).unwrap();
    for g in &sigs {
        assert!(at(&aid) < at(&g.id()), "signatures follow the answer");
    }
    on(&p.e, |r| r.fsck().expect("fsck"));
}

#[test]
fn sig8_the_writer_is_free_during_agent_wait() {
    let p = project();
    p.config("always", Some(person_ed()), None);
    let (asked_tx, asked_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let agent = Arc::new(FakeAgent::hooked(&[person_ed()], false, move |_| {
        asked_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    }));
    let (id, _) = p.proposal();
    let (e, a) = (Arc::clone(&p.e), Arc::clone(&agent));
    let act = std::thread::spawn(move || {
        run_signed_act(
            &*e,
            &*a,
            &approver(),
            PERSON,
            SigningAct::Approve {
                proposal: id,
                options: LandOptions::default(),
            },
        )
    });
    asked_rx.recv().unwrap();
    // the agent waits for its release: an independent writer runs and finishes meanwhile
    let (section, e) = (p.section.clone(), Arc::clone(&p.e));
    let other = std::thread::spawn(move || move_log(&e, &section, "independent"));
    other
        .join()
        .expect("the independent write finished while the agent waited");
    assert!(p.e.read().has_view("independent"));
    release_tx.send(()).unwrap();
    let v = act.join().unwrap().expect("the act lands");
    // the write moved the log, so the act was prepared and signed again
    assert_eq!(agent.signs(), 2);
    let l = subject(&v);
    assert_eq!(stored(&p.e, &l).len(), 1);
    assert_eq!(p.signature_count(), 1, "the first signature was discarded");
}

#[test]
fn sig8_moved_canon_discards_the_first_signature() {
    let p = project();
    p.config("auto", Some(person_ed()), Some(agent_ed()));
    // canon moves while the agent signs the first relay statement
    let moved = Arc::new(Mutex::new(None::<String>));
    let (e, section, m) = (Arc::clone(&p.e), p.section.clone(), Arc::clone(&moved));
    let agent = FakeAgent::hooked(&[person_ed(), agent_ed()], false, move |_| {
        *m.lock().unwrap() = Some(move_canon(&e, &section, "mover"));
    });
    let (id, _) = p.proposal();
    let v = p.approve_relayed(&agent, &id).unwrap();
    let mover: Hash = moved.lock().unwrap().clone().unwrap().parse().unwrap();
    let l = subject(&v);
    let rec = p.e.read().reader().landing(&l).unwrap();
    assert_eq!(rec.prev, Some(mover), "prepared again from the new prev");
    assert_eq!(agent.signs(), 4, "a discarded pair, then relay and person");
    // only the final subject and its signatures exist
    let sigs = stored(&p.e, &l);
    assert_eq!(sigs.len(), 2);
    assert_eq!(p.signature_count(), 2);
    let landings_of_id = on(&p.e, move |r| {
        let mut n = 0;
        let want: Hash = id.parse().unwrap();
        r.store()
            .for_each_object_of(kinds::LANDING, |h| {
                let rec = r.store().landing(&h)?;
                if rec.approved_version == Some(want) {
                    n += 1;
                }
                Ok(())
            })
            .unwrap();
        n
    });
    assert_eq!(
        landings_of_id, 1,
        "the first prepared landing was never written"
    );

    // a third movement gives up with the exact error, and writes nothing of the act
    let (e, section) = (Arc::clone(&p.e), p.section.clone());
    let agent = FakeAgent::hooked(&[person_ed(), agent_ed()], true, move |n| {
        move_log(&e, &section, &format!("again{n}"));
    });
    let (id, _) = p.proposal();
    let before = p.signature_count();
    let err = p.approve_relayed(&agent, &id).unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str(), err.fix.as_str()),
        (
            E_STALE_BASE,
            "the project changed during all 3 signing attempts",
            "read the current proposal or tag target, then try again"
        )
    );
    assert_eq!(
        agent.signs(),
        6,
        "a relay and a person signature an attempt"
    );
    assert_eq!(p.signature_count(), before);
    let landed =
        p.e.read()
            .landings()
            .unwrap()
            .iter()
            .any(|(_, rec)| rec.approved_version.map(|h| h.to_string()) == Some(id.clone()));
    assert!(!landed);
}

/// A project whose canon holds a prose block with `SECRET`, and a request to remove its
/// current version, proposed: `(project, removal version, prose id)`.
const SECRET: &str = "The launch code is ZEBRA-7731.";

fn removal_project() -> (Proj, String, String) {
    let p = project();
    let section = p.section.clone();
    let prose = on(&p.e, move |r| {
        let w = write(
            r,
            "secret",
            json!([{"op": "create", "type": "prose", "id": "$p", "parent": section, "body": SECRET}]),
        )
        .unwrap();
        let prop = r
            .propose(
                &rdy_qa::fixtures::writer(),
                "secret",
                "Secret",
                Some("Adds it."),
            )
            .unwrap();
        r.land(&approver(), &prop.id).unwrap();
        w.created["$p"].clone()
    });
    let target = prose.clone();
    let id = on(&p.e, move |r| {
        r.redact_in(
            &rdy_qa::fixtures::writer(),
            "redact",
            RedactInput {
                targets: vec![target],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
        r.propose(
            &rdy_qa::fixtures::writer(),
            "redact",
            "Remove the code",
            Some("Removes the leaked code."),
        )
        .unwrap()
        .id
    });
    (p, id, prose)
}

fn on_disk(dir: &Path, needle: &str) -> bool {
    let segs = dir.join(".rdy/store/segments");
    std::fs::read_dir(segs).unwrap().any(|e| {
        let bytes = std::fs::read(e.unwrap().path()).unwrap();
        bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
    })
}

#[test]
fn sig8_changed_cards_and_redaction_reports_are_never_reapproved() {
    // a new version of the proposal while the agent signs: the retry meets the existing
    // refusal of a moved card, and nothing lands
    let p = project();
    p.config("auto", Some(person_ed()), Some(agent_ed()));
    let (_, r#ref) = p.proposal();
    let view = format!("v{}", p.n.load(Ordering::SeqCst) - 1);
    let (e, section) = (Arc::clone(&p.e), p.section.clone());
    let agent = FakeAgent::hooked(&[person_ed(), agent_ed()], false, move |_| {
        let (section, view) = (section.clone(), view.clone());
        on(&e, move |r| {
            write(
                r,
                &view,
                json!([{"op": "create", "type": "claim", "parent": section, "body": "More."}]),
            )
            .unwrap();
            r.propose(&rdy_qa::fixtures::writer(), &view, &view, Some("Adds two."))
                .unwrap();
        });
    });
    let before = p.signature_count();
    let e = p.approve_by_card(&agent, &r#ref).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e}");
    assert!(e.corrected.is_some(), "the fresh card: {e}");
    assert_eq!(
        agent.signs(),
        2,
        "one attempt (relay and person), never signed again"
    );
    assert_eq!(p.signature_count(), before);

    // a changed redaction report while the agent signs: E_REDACTION_STALE, no removal, no retry
    let (p, id, prose) = removal_project();
    p.config("auto", Some(person_ed()), None);
    let (e, section) = (Arc::clone(&p.e), p.section.clone());
    let agent = FakeAgent::hooked(&[person_ed()], false, move |_| {
        let section = section.clone();
        on(&e, move |r| {
            write(
                r,
                "quote",
                json!([{"op": "create", "type": "prose", "parent": section, "body": SECRET}]),
            )
            .unwrap();
        });
    });
    let end_before = p.log_end();
    let err = p.approve_local(&agent, &id).unwrap_err();
    assert_eq!(err.code, E_REDACTION_STALE, "{err}");
    assert_eq!(agent.signs(), 1, "no retry");
    assert_ne!(p.log_end(), end_before, "only the quote was written");
    assert_eq!(p.signature_count(), 0);
    assert!(on_disk(p.dir.path(), SECRET), "zero removal");
    let st = p.e.read();
    assert_eq!(st.redaction_epoch(), 0);
    let prose = rdy_model::BlockId::parse(&prose).unwrap();
    assert_eq!(st.block(&Scope::Canon, &prose).unwrap().body, SECRET);
}

#[test]
fn sig8_always_failure_and_seal_failure_leave_no_partial_act() {
    let p = project();
    // the agent refuses: nothing is written
    p.config("always", Some(person_ed()), Some(agent_ed()));
    let agent = FakeAgent::scripted(&[person_ed(), agent_ed()], &[Reply::Refuse]);
    assert_eq!(
        outcome(&p, &agent, Act::Approval, false),
        Outcome::Refused(
            E_NO_AGENT,
            "the SSH agent could not sign: request refused".into()
        )
    );
    // the second signature fails: no relay signature alone
    let agent = FakeAgent::scripted(&[person_ed(), agent_ed()], &[Reply::Good, Reply::Refuse]);
    assert!(matches!(
        outcome(&p, &agent, Act::Tag, true),
        Outcome::Refused(E_NO_AGENT, _)
    ));
    assert_eq!(p.signature_count(), 0);

    // an index failure on the commit leaves nothing of the landing or its signatures
    p.config("auto", Some(person_ed()), Some(agent_ed()));
    let agent = FakeAgent::new(&[person_ed(), agent_ed()]);
    let (id, _) = p.proposal();
    let (end, canon) = (p.log_end(), p.e.read().canon_head());
    on(&p.e, |r| r.store_mut().fail_next_index_apply());
    assert!(p.approve_relayed(&agent, &id).is_err());
    assert_eq!(p.log_end(), end);
    assert_eq!(p.e.read().canon_head(), canon);
    assert_eq!(p.signature_count(), 0);
    let v = p.approve_relayed(&agent, &id).expect("it lands afterwards");
    assert_eq!(stored(&p.e, &subject(&v)).len(), 2);

    // a seal failure after the commit's write: the reopen holds nothing of the act
    let (id, _) = p.proposal();
    let canon = p.e.read().canon_head();
    let count = p.signature_count();
    on(&p.e, |r| r.store_mut().fail_index_apply_after(1));
    let agent = FakeAgent::new(&[person_ed(), agent_ed()]);
    assert!(p.approve_relayed(&agent, &id).is_err());
    assert_eq!(agent.signs(), 2, "it failed at the seal, after signing");
    let Proj { dir, e, .. } = p;
    drop(e);
    let r = Rdy::open(dir.path()).expect("reopen");
    assert_eq!(r.read().canon_head(), canon, "no landing");
    let mut n = 0;
    r.store()
        .for_each_object_of(kinds::SIGNATURE, |_| {
            n += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(n, count, "no signature of the failed act");
    r.fsck().expect("fsck");
}

#[test]
fn sig8_later_signing_checks_person_kind_relay_and_duplicates() {
    let p = project();
    p.config("never", Some(person_ed()), Some(agent_sk()));
    let none = FakeAgent::new(&[]);
    let local: Hash = subject(&p.approve_local(&none, &p.proposal().0).unwrap());
    let relayed: Hash = subject(&p.approve_relayed(&none, &p.proposal().0).unwrap());
    let ltag = p.version();
    p.tag_local(&none, &ltag).unwrap();
    let rtag = p.version();
    let rtag_hash = subject(&p.tag_relayed(&none, &rtag).unwrap());
    assert_eq!(none.calls(), 0, "never signs automatically");
    let keys = [person_ed(), agent_sk()];
    let local_t = local.to_string();
    let relayed_t = relayed.to_string();

    // a human signs only their own act; another human is refused
    let other = Session::human("human:other".into(), Role::Approver);
    let agent = FakeAgent::new(&keys);
    let e = run_sign(&*p.e, &agent, &other, &local_t, None).unwrap_err();
    assert_err(
        &e,
        E_PERMISSION,
        &format!("this session cannot sign approval for {local_t}"),
    );
    assert_eq!(
        e.fix,
        "sign as the recorded person, or relay the person's act from an agent session"
    );
    assert_eq!(agent.calls(), 0);
    // a wrong kind
    let e = run_sign(
        &*p.e,
        &agent,
        &approver(),
        &local_t,
        Some(StatementKind::Tag),
    )
    .unwrap_err();
    assert_err(&e, E_BAD_INPUT, &format!("kind tag does not fit {local_t}"));
    assert_eq!(
        e.fix,
        "use approval or relay for a landing, and tag or relay for a tag"
    );
    // the person, by a unique prefix, default kind approval
    let res = run_sign(&*p.e, &agent, &approver(), &local_t[..8], None).unwrap();
    assert_eq!(res.subject, local);
    assert!(!res.already_signed);
    assert_eq!(res.check.kind.as_deref(), Some("approval"));
    assert_eq!(
        res.check.key.as_deref(),
        Some(person_ed().fingerprint().as_str())
    );
    assert_eq!(res.check.signed, SIGNED_VALID);
    assert_eq!(res.check.statement, STATEMENT_MATCH);
    // again: already signed, no agent operation, nothing written
    let agent = FakeAgent::new(&keys);
    let end = p.log_end();
    let res = run_sign(&*p.e, &agent, &approver(), &local_t, None).unwrap();
    assert!(res.already_signed);
    assert_eq!(agent.calls(), 0);
    assert_eq!(p.log_end(), end);
    // the local tag, by its version
    let res = run_sign(&*p.e, &agent, &approver(), &format!("tag:{ltag}"), None).unwrap();
    assert_eq!(res.check.kind.as_deref(), Some("tag"));

    // an agent signs no local act
    let e = run_sign(&*p.e, &agent, &relayer(), &local_t, None).unwrap_err();
    assert_err(
        &e,
        E_PERMISSION,
        &format!("this session cannot sign relay for {local_t}"),
    );
    // an agent's default on a relayed subject is the relay, with the machine key
    let agent = FakeAgent::new(&keys);
    let res = run_sign(&*p.e, &agent, &relayer(), &relayed_t, None).unwrap();
    assert_eq!(res.check.kind.as_deref(), Some("relay"));
    assert_eq!(
        res.check.key.as_deref(),
        Some(agent_sk().fingerprint().as_str())
    );
    assert_eq!(res.check.signed_by.actor, "agent:relay");
    // the agent's explicit person signature, the relay already there: one new signature
    let res = run_sign(
        &*p.e,
        &agent,
        &relayer(),
        &relayed_t,
        Some(StatementKind::Approval),
    )
    .unwrap();
    assert_eq!(
        res.check.key.as_deref(),
        Some(person_ed().fingerprint().as_str())
    );
    assert_eq!(stored(&p.e, &relayed).len(), 2);

    // the relayed tag has no relay signature: the person signature brings the machine key's
    // relay into the same batch
    let agent = FakeAgent::new(&keys);
    let res = run_sign(
        &*p.e,
        &agent,
        &relayer(),
        &format!("tag:{rtag}"),
        Some(StatementKind::Tag),
    )
    .unwrap();
    assert_eq!(res.subject, rtag_hash);
    assert_eq!(agent.signs(), 2);
    let batch: Vec<u8> = on(&p.e, |r| {
        r.store().last_batch().iter().map(|(k, _)| *k).collect()
    });
    assert_eq!(
        batch.iter().filter(|k| **k == kinds::SIGNATURE).count(),
        2,
        "one batch"
    );
    assert_eq!(
        kinds_keys(&stored(&p.e, &rtag_hash)),
        vec![
            ("relay", agent_sk().fingerprint()),
            ("tag", person_ed().fingerprint())
        ]
    );

    // a human signs their own relayed act with no machine key; then an agent asking for the
    // same person signature makes no agent call, though no relay signature exists
    let fresh = subject(&p.approve_relayed(&none, &p.proposal().0).unwrap());
    let fresh_t = fresh.to_string();
    let agent = FakeAgent::new(&[person_ed()]);
    let res = run_sign(&*p.e, &agent, &approver(), &fresh_t, None).unwrap();
    assert_eq!(res.check.kind.as_deref(), Some("approval"));
    assert_eq!(stored(&p.e, &fresh).len(), 1);
    let agent = FakeAgent::new(&keys);
    let res = run_sign(
        &*p.e,
        &agent,
        &relayer(),
        &fresh_t,
        Some(StatementKind::Approval),
    )
    .unwrap();
    assert!(res.already_signed);
    assert_eq!(agent.calls(), 0);
    assert_eq!(stored(&p.e, &fresh).len(), 1);

    // an invalid prior signature does not suppress a new one
    let bare = subject(&p.approve_local(&none, &p.proposal().0).unwrap());
    let bogus = Signature {
        subject: bare,
        namespace: "rdy-approval-v1".into(),
        statement: b"rdy-statement: 1\nkind: approval\n".to_vec(),
        sig: vec![1, 2, 3],
        signed_by: approver().provenance(),
    };
    on(&p.e, move |r| {
        r.store_mut().put_signature(&bogus).unwrap();
    });
    let agent = FakeAgent::new(&keys);
    let res = run_sign(&*p.e, &agent, &approver(), &bare.to_string(), None).unwrap();
    assert!(!res.already_signed);
    assert_eq!(stored(&p.e, &bare).len(), 2);

    // concurrent duplicates: the second request finishes while the first waits on its agent;
    // the first then discards its prepared signature and returns the existing one
    let dup = subject(&p.approve_local(&none, &p.proposal().0).unwrap());
    let dup_t = dup.to_string();
    let e = Arc::clone(&p.e);
    let inner = dup_t.clone();
    let agent = FakeAgent::hooked(&keys, false, move |_| {
        let nested = FakeAgent::new(&[person_ed()]);
        let res = run_sign(&*e, &nested, &approver(), &inner, None).unwrap();
        assert!(!res.already_signed);
    });
    let res = run_sign(&*p.e, &agent, &approver(), &dup_t, None).unwrap();
    assert!(res.already_signed, "the concurrent signature is returned");
    assert_eq!(
        stored(&p.e, &dup).len(),
        1,
        "at most one for the kind and key"
    );

    // a policy approval cannot be signed
    let policy = on(&p.e, move |r| {
        let mut rec: LandingRecord = r.store().landing(&local).unwrap();
        rec.assurance = Assurance::Policy;
        rec.changes.clear();
        let bytes = postcard::to_allocvec(&rec).unwrap();
        let h = Hash::of(&bytes);
        r.store_mut().write(vec![(kinds::LANDING, bytes)]).unwrap();
        h
    });
    let agent = FakeAgent::new(&keys);
    let e = run_sign(&*p.e, &agent, &approver(), &policy.to_string(), None).unwrap_err();
    assert_err(&e, E_PERMISSION, "policy approvals cannot be signed");
    assert_eq!(
        e.fix,
        "wait for the authority policy to define their signer"
    );
    assert_eq!(agent.calls(), 0);

    // targets that resolve to no stored landing, or to more than one
    let e = run_sign(&*p.e, &agent, &approver(), "abcdef0123", None).unwrap_err();
    assert_eq!(
        (e.message.as_str(), e.fix.as_str()),
        (
            "no stored landing matches \"abcdef0123\"",
            "pass a full landing hash or a longer unique prefix"
        )
    );
}

#[test]
fn sig8_bad_agent_signatures_never_reach_the_log() {
    let p = project();
    let invalid = "the SSH agent could not sign: invalid reply".to_string();
    for bad in [Reply::OtherKey, Reply::OtherPayload, Reply::OtherNamespace] {
        for person in [person_ed(), person_sk()] {
            p.config("auto", Some(person), None);
            let agent = FakeAgent::scripted(&[person], &[bad]);
            assert_eq!(
                outcome(&p, &agent, Act::Approval, false),
                Outcome::Warned(E_NO_AGENT.into(), invalid.clone()),
                "{bad:?} {person:?}"
            );
            let agent = FakeAgent::scripted(&[person], &[bad]);
            let v = p.tag_local(&agent, &p.version()).unwrap();
            assert_eq!(
                signing_warnings(&v),
                vec![failed(E_NO_AGENT, &invalid)],
                "the exact warning"
            );
            p.config("always", Some(person), None);
            let agent = FakeAgent::scripted(&[person], &[bad]);
            assert_eq!(
                outcome(&p, &agent, Act::Tag, false),
                Outcome::Refused(E_NO_AGENT, invalid.clone())
            );
        }
    }
    assert_eq!(p.signature_count(), 0, "no bad signature was stored");
    // explicit signing writes nothing either
    p.config("never", Some(person_ed()), None);
    let none = FakeAgent::new(&[]);
    let l = subject(&p.approve_local(&none, &p.proposal().0).unwrap());
    let end = p.log_end();
    let agent = FakeAgent::scripted(&[person_ed()], &[Reply::OtherPayload]);
    let e = run_sign(&*p.e, &agent, &approver(), &l.to_string(), None).unwrap_err();
    assert_err(&e, E_NO_AGENT, &invalid);
    assert_eq!(p.log_end(), end);
    // a good reply's check is the one the result reports
    p.config("auto", Some(person_sk()), Some(agent_ed()));
    let agent = FakeAgent::new(&[person_sk(), agent_ed()]);
    let v = p.approve_relayed(&agent, &p.proposal().0).unwrap();
    let mut checks = signature_checks(&p.e.read(), &subject(&v), None).unwrap();
    let mut reported: Vec<_> = v["signatures"].as_array().unwrap().clone();
    let key = |c: &Value| c["kind"].as_str().unwrap().to_string();
    reported.sort_by_key(key);
    checks.sort_by(|a, b| a.kind.cmp(&b.kind));
    assert_eq!(serde_json::to_value(&checks).unwrap(), json!(reported));
    assert_eq!(reported[0]["presence"], "present", "the sk approval");
}

#[test]
fn sig8_recommended_answers_preserve_order_and_removal_prescan() {
    let p = project();
    p.config("auto", Some(person_ed()), Some(agent_ed()));
    // the person's own answers, bound to the versions only: each landing moves the next card
    let recommended = |refs: &[String], agent: &FakeAgent| {
        run_signed_act(
            &*p.e,
            agent,
            &approver(),
            PERSON,
            SigningAct::Recommended {
                refs: refs.to_vec(),
                card_hashes: vec![],
                relay: RelayInput::default(),
            },
        )
    };
    // two approvals in the given order, each its own signed batch
    let refs = vec![p.proposal().1, p.proposal().1];
    let agent = FakeAgent::new(&[person_ed(), agent_ed()]);
    let v = recommended(&refs, &agent).unwrap();
    let done = v.as_array().expect("a list");
    assert_eq!(
        done.iter()
            .map(|d| d["ref"].as_str().unwrap())
            .collect::<Vec<_>>(),
        refs.iter().map(String::as_str).collect::<Vec<_>>()
    );
    for d in done {
        assert_eq!(d["act"], "approve");
        assert_eq!(
            result_signatures(d),
            vec![("approval".to_string(), person_ed().fingerprint())],
            "{d:#}"
        );
        assert_eq!(stored(&p.e, &subject(d)).len(), 1);
        assert!(
            d["landing"].get("consequences").is_some()
                || d["landing"].get("consequences_error").is_some(),
            "consequences are attached: {d:#}"
        );
    }
    assert_ne!(subject(&done[0]), subject(&done[1]));
    assert!(done[0]["next"].is_null(), "next only on the last answer");

    // a refusal stops the list: what was done is reported, and the rest is not answered
    let first = p.proposal().1;
    let last = p.proposal().1;
    let refs = vec![first.clone(), "P-000000@1".to_string(), last.clone()];
    let agent = FakeAgent::new(&[person_ed(), agent_ed()]);
    let e = recommended(&refs, &agent).unwrap_err();
    let corrected = e.corrected.as_ref().expect("done");
    let reported: Vec<&str> = corrected["done"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["ref"].as_str().unwrap())
        .collect();
    assert_eq!(reported, vec![first.as_str()], "{e}");
    assert!(corrected["done"][0]["signatures"].is_array());
    assert!(
        p.e.read()
            .pending(&PendingQuery {
                person: PERSON.into(),
                session_ref: None,
                max_tokens: CARD_MAX_TOKENS
            })
            .unwrap()
            .items
            .iter()
            .any(|i| i.r#ref == last),
        "the item after the refusal is still waiting"
    );

    // a list that holds a removal writes nothing, wherever the removal stands
    let (p, removal, _) = removal_project();
    p.config("auto", Some(person_ed()), Some(agent_ed()));
    let removal_ref = on(&p.e, move |r| {
        r.resolve_proposal(&removal).unwrap().to_string()
    });
    let ordinary = p.proposal().1;
    let hashes = vec![card_hash(&p.e, &ordinary), "0".repeat(64)];
    let end = p.log_end();
    let agent = FakeAgent::new(&[person_ed(), agent_ed()]);
    let e = run_signed_act(
        &*p.e,
        &agent,
        &relayer(),
        PERSON,
        SigningAct::Recommended {
            refs: vec![ordinary, removal_ref],
            card_hashes: hashes,
            relay: words(),
        },
    )
    .unwrap_err();
    assert_eq!(e.code, E_REDACTION_HUMAN, "{e}");
    assert_eq!(p.log_end(), end, "nothing is answered or landed");
    assert_eq!(agent.calls(), 0);
}

#[test]
fn sig8_signature_publication_preserves_redaction_epochs() {
    let (p, removal, prose) = removal_project();
    p.config("never", Some(person_ed()), None);
    let base = p.e.read().canon_head().unwrap();
    let before = p.e.read();
    before
        .ensure_current_epoch()
        .expect("current before the removal");
    let none = FakeAgent::new(&[]);
    p.approve_local(&none, &removal).expect("the removal lands");
    assert!(!on_disk(p.dir.path(), SECRET), "erased");
    let e = before.ensure_current_epoch().unwrap_err();
    assert_eq!(e.code, E_REDACTED_STATE, "{e}");
    // signing the landing that carried the content publishes a signature, not the content
    let agent = FakeAgent::new(&[person_ed()]);
    let res = run_sign(&*p.e, &agent, &approver(), &base.to_string(), None).unwrap();
    assert!(!res.already_signed);
    assert_eq!(res.check.signed, SIGNED_VALID);
    let e = before.ensure_current_epoch().unwrap_err();
    assert_eq!(e.code, E_REDACTED_STATE, "the old state still refuses");
    let st = p.e.read();
    st.ensure_current_epoch().expect("the new state is current");
    let prose = rdy_model::BlockId::parse(&prose).unwrap();
    assert_eq!(
        st.block(&Scope::Canon, &prose).unwrap().body,
        CONTENT_REMOVED
    );
    assert!(!on_disk(p.dir.path(), SECRET), "still erased");
    on(&p.e, |r| r.fsck().expect("fsck"));
}
