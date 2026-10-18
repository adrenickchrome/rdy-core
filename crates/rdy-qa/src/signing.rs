//! The QA signer: two fixed in-memory Ed25519 keys behind the engine's `SigningAgent`, and the
//! fixed public vectors of the signature perf ops.
//!
//! This is a test harness. No production path imports it, no key is ever read from or written to
//! disk, and the seeds below are public test values. A record is signed through the same
//! wrapper a real agent goes through (`sign_sshsig`), so the SSHSIG blob and the statement are
//! the ones a real signing act stores (D-1Z951, D-17AA8). The fixed vectors also hold one `sk`
//! key whose flags and counter are set here; the authenticator is simulated in memory, and no
//! physical token is touched (FIDO flag vectors stay in the crypto tests).
use rdy_engine::allowed_signers::AllowedSigners;
use rdy_engine::signature_check::{CheckContext, SignatureSubject};
use rdy_engine::signing_crypto::{
    AgentIdentity, KeyInfo, SigningAgent, blob_fingerprint, resolve_key, sign_sshsig,
};
use rdy_engine::{NoAgentCause, RdyError, no_agent};
use rdy_model::signature::{Signature, StatementSource, parse_statement, statement};
use rdy_model::{
    ActorRun, Assurance, Hash, LandingRecord, Provenance, RepoId, Role, signature::StatementKind,
};
use ssh_key::private::Ed25519Keypair;
use ssh_key::public::{KeyData, SkEd25519};
use ssh_key::sha2::{Digest, Sha256};
use ssh_key::{Algorithm, PrivateKey, PublicKey, SigningKey};

/// The seed of the person's key.
const PERSON_SEED: [u8; 32] = [0x70; 32];
/// The seed of the agent's key.
const AGENT_SEED: [u8; 32] = [0xa6; 32];
/// The seed of the simulated security key of the perf vectors.
const SK_SEED: [u8; 32] = [0x5b; 32];
/// The flags of the simulated authenticator: user presence.
const SK_FLAGS: u8 = 0x01;
/// The counter of the simulated authenticator.
const SK_COUNTER: u32 = 7;
/// The application string of an `sk-ssh-ed25519@openssh.com` key.
const SK_APPLICATION: &str = "ssh:";
/// The wire name of the sk key type.
const SK_ALGORITHM: &str = "sk-ssh-ed25519@openssh.com";

/// A fixed test key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestKey {
    /// The person's key: signs approvals and tags.
    Person,
    /// The agent's key: signs relays.
    Agent,
}

/// What a held key signs with.
#[derive(Clone, Copy)]
enum Material {
    /// A plain `ssh-ed25519` key.
    Plain([u8; 32]),
    /// A simulated `sk-ssh-ed25519@openssh.com` key, with the flags and counter its
    /// authenticator reports.
    Sk {
        seed: [u8; 32],
        flags: u8,
        counter: u32,
    },
}

impl Material {
    fn seed(&self) -> &[u8; 32] {
        match self {
            Material::Plain(seed) | Material::Sk { seed, .. } => seed,
        }
    }

    fn public_key(&self) -> PublicKey {
        let public = Ed25519Keypair::from_seed(self.seed()).public;
        PublicKey::from(match self {
            Material::Plain(_) => KeyData::Ed25519(public),
            Material::Sk { .. } => KeyData::SkEd25519(SkEd25519::new(public, SK_APPLICATION)),
        })
    }

    fn blob(&self) -> Vec<u8> {
        self.public_key()
            .to_bytes()
            .expect("a test public key encodes")
    }

    /// The SSH signature blob (algorithm, then signature) of `payload`, as an agent's sign
    /// response carries it.
    fn sign(&self, payload: &[u8]) -> Vec<u8> {
        let key = PrivateKey::from(Ed25519Keypair::from_seed(self.seed()));
        let mut out = Vec::new();
        match self {
            Material::Plain(_) => {
                put_string(&mut out, b"ssh-ed25519");
                put_string(&mut out, &raw_sign(&key, payload));
            }
            Material::Sk { flags, counter, .. } => {
                // the authenticator signs the application hash, the flags, the counter and the
                // payload hash; its reply is the signature, then the flags and the counter
                let mut message = Sha256::digest(SK_APPLICATION.as_bytes()).to_vec();
                message.push(*flags);
                message.extend(counter.to_be_bytes());
                message.extend(Sha256::digest(payload));
                put_string(&mut out, SK_ALGORITHM.as_bytes());
                put_string(&mut out, &raw_sign(&key, &message));
                out.push(*flags);
                out.extend(counter.to_be_bytes());
            }
        }
        out
    }
}

/// The 64 raw Ed25519 signature bytes of `message`.
fn raw_sign<S: SigningKey>(key: &S, message: &[u8]) -> Vec<u8> {
    let signature = key.sign(message);
    debug_assert_eq!(signature.algorithm(), Algorithm::Ed25519);
    signature.as_bytes().to_vec()
}

/// An SSH `string`: a big-endian length, then the bytes.
fn put_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend((bytes.len() as u32).to_be_bytes());
    out.extend(bytes);
}

/// The test harness's agent: it holds fixed keys in memory and signs what it is asked.
pub struct QaAgent {
    held: Vec<Material>,
}

impl QaAgent {
    /// An agent that holds the person's and the agent's keys.
    pub fn new() -> QaAgent {
        QaAgent {
            held: vec![
                Material::Plain(PERSON_SEED),
                Material::Plain(AGENT_SEED),
                Material::Sk {
                    seed: SK_SEED,
                    flags: SK_FLAGS,
                    counter: SK_COUNTER,
                },
            ],
        }
    }

    /// The public-key blob of `key`.
    pub fn blob(key: TestKey) -> Vec<u8> {
        Material::Plain(seed_of(key)).blob()
    }

    /// The SHA256 fingerprint of `key`.
    pub fn fingerprint(key: TestKey) -> String {
        blob_fingerprint(&QaAgent::blob(key))
    }
}

impl Default for QaAgent {
    fn default() -> Self {
        QaAgent::new()
    }
}

fn seed_of(key: TestKey) -> [u8; 32] {
    match key {
        TestKey::Person => PERSON_SEED,
        TestKey::Agent => AGENT_SEED,
    }
}

impl SigningAgent for QaAgent {
    fn identities(&self) -> Result<Vec<AgentIdentity>, RdyError> {
        Ok(self
            .held
            .iter()
            .map(|m| AgentIdentity {
                public_key: m.blob(),
            })
            .collect())
    }

    fn sign(&self, public_key: &[u8], payload: &[u8]) -> Result<Vec<u8>, RdyError> {
        self.held
            .iter()
            .find(|m| m.blob() == public_key)
            .map(|m| m.sign(payload))
            .ok_or_else(|| no_agent(NoAgentCause::NotHeld))
    }
}

/// The key `fingerprint` names, resolved through `agent`.
fn key_info(agent: &QaAgent, fingerprint: &str) -> KeyInfo {
    resolve_key("signing.key", fingerprint, agent).expect("the test agent holds its keys")
}

/// A signature of `subject` for the statement of `source` in `repo`, signed by `key` through
/// the engine's SSHSIG wrapper. `signed_by` is who asked, as the record says.
pub fn sign_record(
    subject: Hash,
    source: StatementSource<'_>,
    repo: RepoId,
    signed_by: Provenance,
    key: TestKey,
) -> Signature {
    sign_with(
        &QaAgent::new(),
        subject,
        source,
        repo,
        signed_by,
        &QaAgent::fingerprint(key),
    )
}

fn sign_with(
    agent: &QaAgent,
    subject: Hash,
    source: StatementSource<'_>,
    repo: RepoId,
    signed_by: Provenance,
    fingerprint: &str,
) -> Signature {
    let info = key_info(agent, fingerprint);
    let statement =
        statement(source, Some(repo), &info.fingerprint).expect("the record can be signed");
    let kind: StatementKind = parse_statement(&statement)
        .expect("a built statement parses")
        .kind();
    let namespace = kind.namespace();
    let sig = sign_sshsig(agent, &info, namespace, &statement).expect("the test agent signs");
    Signature {
        subject,
        namespace: namespace.to_string(),
        statement,
        sig,
        signed_by,
    }
}

/// A fixed public vector for the signature perf ops: a signature, the context it is checked in,
/// and the parsed trust input that trusts its key.
pub struct Vector {
    pub record: Signature,
    pub context: CheckContext,
    pub trust: AllowedSigners,
}

/// The landing every vector signs: a person's local approval, with fixed hashes.
fn vector_landing() -> LandingRecord {
    LandingRecord {
        proposal: Hash([1; 32]),
        prev: None,
        state: Hash([2; 32]),
        approved_by: Provenance {
            actor: "human:qa".into(),
            run: None,
            role: Role::Approver,
            at_ms: 1,
        },
        changes: vec![],
        chosen: None,
        checks: vec![],
        assurance: Assurance::Local,
        relayed_by: None::<ActorRun>,
        relay_evidence: None,
        approved_version: Some(Hash([3; 32])),
        rebased_from: vec![],
        evidence: None,
    }
}

/// The fixed vector of a plain Ed25519 key (`sk` false) or of the simulated security key whose
/// authenticator reports user presence (`sk` true). Both sign the same approval statement.
pub fn vector(sk: bool) -> Vector {
    let agent = QaAgent::new();
    let material = if sk {
        agent.held[2]
    } else {
        Material::Plain(PERSON_SEED)
    };
    let landing = vector_landing();
    let hash = landing.id();
    let repo = RepoId([0x14; 16]);
    let record = sign_with(
        &agent,
        hash,
        StatementSource::Approval(&landing),
        repo,
        Provenance {
            actor: "human:qa".into(),
            run: None,
            role: Role::Approver,
            at_ms: 1,
        },
        &blob_fingerprint(&material.blob()),
    );
    let line = material
        .public_key()
        .to_openssh()
        .expect("a test public key spells");
    let trust = AllowedSigners::parse(format!("human:qa {line}\n").as_bytes())
        .expect("the trust line parses");
    Vector {
        record,
        context: CheckContext {
            subject: SignatureSubject::Landing {
                hash,
                record: landing,
            },
            current_repo: repo,
            source_repos: vec![],
        },
        trust,
    }
}
