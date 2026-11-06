//! rtc7 (FW-MH.rules, D-WDKWQ, D-CC90E): a rule that a person asked for in chat lands with a text
//! relay and no card hash. The adapter adds no API for it: the hub reads such a landing through
//! the take and snapshot calls that already exist, so the instruction, its decision, the relay
//! and the absent hash must survive a reopen and a take into a holder. The hub never calls MCP
//! and never inspects a user's home; the rule here lands through the engine, with a host that
//! names the person and nothing else.
use rdy_adapter::history::{BlockOp, LandingFacts};
use rdy_adapter::take::UploadTarget;
use rdy_adapter::{Hash, RdyError, StoreHandle};
use rdy_engine::engine::Local;
use rdy_engine::instructions::{Caller, Contribution, LoadRequest, PersonHost};
use rdy_engine::rule_types::parse_request;
use rdy_engine::rules::{RuleOrigin, route_rule};
use rdy_engine::signing_crypto::{AgentIdentity, SigningAgent};
use rdy_engine::{Rdy, Session, SessionKind, stream_export};
use rdy_model::{EvidenceKind, Role};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

const PERSON: &str = "human:qa";
const AGENT: &str = "agent:a";
const EVIDENCE: &str = "Yes, keep that rule.";
const QUESTION: &str = "Shall I save this rule?";

/// A host that names the person. It reads no home and no other project.
struct NamedPerson;

impl PersonHost for NamedPerson {
    fn person_actor(&self, _project: &Path) -> Result<String, RdyError> {
        Ok(PERSON.to_string())
    }

    fn fetch(&self, _: &Path, _: &LoadRequest, _: &Caller) -> Result<Contribution, RdyError> {
        panic!("a project rule reads no other project")
    }
}

/// An agent that holds no key. The project signs never, so nothing asks it.
struct NoKey;

impl SigningAgent for NoKey {
    fn identities(&self) -> Result<Vec<AgentIdentity>, RdyError> {
        Ok(vec![])
    }

    fn sign(&self, _: &[u8], _: &[u8]) -> Result<Vec<u8>, RdyError> {
        panic!("a project that signs never asked the agent")
    }
}

fn rule(local: &Local, v: Value) -> Value {
    let s = Session::agent(AGENT.into(), Some("r1".into()), Role::Writer).unwrap();
    let q = parse_request(&v).unwrap_or_else(|e| panic!("{v}: {e}"));
    let host = NamedPerson;
    route_rule(
        Some(local),
        Some(&host),
        &RuleOrigin {
            session: &s,
            relayed_by: None,
        },
        &q,
        Arc::new(NoKey),
    )
    .unwrap_or_else(|e| panic!("{v}: {e}"))
}

fn human() -> Session {
    Session {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
        kind: SessionKind::Human,
    }
}

/// A project at `dir` with a rule added and then edited, each by chat. Returns the rule's id, the
/// decision of the add and the replies of both calls.
fn source(dir: &Path) -> (String, String, Value, Value) {
    Rdy::init(dir, "rdy-standard").unwrap().close().unwrap();
    std::fs::write(
        dir.join(".rdy/local.yaml"),
        "signing:\n  key: null\n  agent_key: null\n  sign: never\n  allowed_signers: null\n",
    )
    .unwrap();
    let local = Local::open(dir).unwrap();
    let added = rule(
        &local,
        json!({"scope": "project", "op": "add", "text": "Say what you changed.",
               "why": "They asked.", "evidence": EVIDENCE, "question": QUESTION,
               "session_ref": "chat-1"}),
    );
    let id = added["rule"]["id"].as_str().expect("a rule id").to_string();
    let decision = added["decision"].as_str().expect("a decision").to_string();
    let edited = rule(
        &local,
        json!({"scope": "project", "op": "edit", "id": id, "text": "Say what you changed and why.",
               "why": "Wider.", "evidence": "Yes, wider.", "session_ref": "chat-2"}),
    );
    local.close().unwrap();
    (id, decision, added, edited)
}

fn landed(reply: &Value) -> Hash {
    reply["landing"]["landing"]
        .as_str()
        .unwrap_or_else(|| panic!("a landing in {reply}"))
        .parse()
        .unwrap()
}

/// What the hub keeps of a landing: the relay, the evidence and the blocks, as recorded.
fn kept(f: &LandingFacts) -> Value {
    json!({
        "hash": f.hash.to_string(),
        "relayed_by": f.record.relayed_by.as_ref().map(|r| (r.actor.clone(), r.run.clone())),
        "quoted": f.record.relay_evidence.as_ref().map(|r| r.quoted.clone()),
        "session_ref": f.record.relay_evidence.as_ref().map(|r| r.session_ref.clone()),
        "question": f.record.relay_evidence.as_ref().and_then(|r| r.question.clone()),
        "evidence": f.record.evidence.as_ref().map(|e| (e.kind.as_str(), e.card_hash.is_some())),
        "blocks": f.blocks.iter()
            .map(|b| (b.id.to_string(), b.op.as_str(), b.version.map(|v| v.to_string())))
            .collect::<Vec<_>>(),
    })
}

fn check_text_landing(f: &LandingFacts, rule_id: &str, decision: &str, evidence: &str) {
    assert_eq!(
        f.record.approved_by.actor, PERSON,
        "the person approved: {:?}",
        f.record
    );
    let relay = f.record.relayed_by.as_ref().expect("a relay");
    assert_eq!(
        (relay.actor.as_str(), relay.run.as_deref()),
        (AGENT, Some("r1"))
    );
    let said = f.record.relay_evidence.as_ref().expect("relay evidence");
    assert_eq!(said.quoted, evidence);
    let kind = f.record.evidence.as_ref().expect("approval evidence");
    assert_eq!(kind.kind, EvidenceKind::Text);
    assert!(
        kind.card_hash.is_none(),
        "a text relay shows no card: {kind:?}"
    );
    let ids: Vec<String> = f.blocks.iter().map(|b| b.id.to_string()).collect();
    assert!(ids.iter().any(|i| i == rule_id), "{rule_id} in {ids:?}");
    assert!(ids.iter().any(|i| i == decision), "{decision} in {ids:?}");
}

#[test]
fn rtc7_adapter_reads_and_takes_text_rule_landings() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let (id, decision, added, edited) = source(&src);
    let (first, second) = (landed(&added), landed(&edited));

    // the snapshot of the project reads both landings, newest first, as recorded
    let handle = StoreHandle::open(&src).unwrap();
    let chain = handle.snapshot().unwrap().canon_chain().unwrap();
    assert_eq!(
        chain.iter().map(|f| f.hash).collect::<Vec<_>>(),
        [second, first]
    );
    check_text_landing(&chain[1], &id, &decision, EVIDENCE);
    let q = chain[1].record.relay_evidence.as_ref().unwrap();
    assert_eq!(
        (q.session_ref.as_str(), q.question.as_deref()),
        ("chat-1", Some(QUESTION))
    );
    let creates: Vec<String> = chain[1]
        .blocks
        .iter()
        .filter(|b| b.op == BlockOp::Create)
        .map(|b| b.id.to_string())
        .collect();
    assert!(
        creates.contains(&id) && creates.contains(&decision),
        "{creates:?}"
    );
    // the edit is its own text landing, with its own words and no question
    let edit_ids: Vec<String> = chain[0].blocks.iter().map(|b| b.id.to_string()).collect();
    assert!(edit_ids.contains(&id), "{edit_ids:?}");
    assert_eq!(
        chain[0].record.relay_evidence.as_ref().unwrap().quoted,
        "Yes, wider."
    );
    assert!(
        chain[0]
            .record
            .evidence
            .as_ref()
            .unwrap()
            .card_hash
            .is_none()
    );
    let want: Vec<Value> = chain.iter().map(kept).collect();
    handle.close().unwrap();

    // after a reopen the same landings read the same
    let reopened = StoreHandle::open(&src).unwrap();
    let again: Vec<Value> = reopened
        .snapshot()
        .unwrap()
        .canon_chain()
        .unwrap()
        .iter()
        .map(kept)
        .collect();
    assert_eq!(again, want);
    reopened.close().unwrap();

    // a take of the tagged history into a holder keeps them, and so does a reopen of the holder
    let mut r = Rdy::open(&src).unwrap();
    r.tag(&human(), "v1.0.0", None, None).unwrap();
    let tag = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == "v1.0.0")
        .unwrap()
        .1;
    let landing = r.store().read_tag(&tag).unwrap().landing;
    let repo = r.store().repo_id().unwrap().unwrap();
    let mut bytes = Vec::new();
    stream_export::write(&r.read(), tag, &BTreeSet::new(), &mut bytes).unwrap();
    r.close().unwrap();
    let file = dir.path().join("batch.rdysync");
    std::fs::write(&file, &bytes).unwrap();
    let holder_dir = dir.path().join("holder");
    let holder = StoreHandle::create_holder(&holder_dir, repo).unwrap();
    holder
        .take(
            &[File::open(&file).unwrap()],
            UploadTarget {
                tag: "v1.0.0".into(),
                landing,
            },
        )
        .unwrap();
    holder.close().unwrap();
    let holder = StoreHandle::open_holder(&holder_dir).unwrap();
    let taken = holder.snapshot().unwrap().tag_history("v1.0.0").unwrap();
    let got: Vec<Value> = taken.iter().map(kept).collect();
    assert_eq!(got, want, "the take keeps every recorded fact");
    check_text_landing(&taken[1], &id, &decision, EVIDENCE);
    holder.close().unwrap();
}
