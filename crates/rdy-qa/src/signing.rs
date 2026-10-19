//! The QA signer: two fixed in-memory Ed25519 keys behind the engine's `SigningAgent`, and the
//! frozen public vectors of the signature perf ops.
//!
//! This is a test harness. No production path imports it, no key is ever read from or written to
//! disk, and the seeds below are public test values. A record is signed through the same
//! wrapper a real agent goes through (`sign_sshsig`), so the SSHSIG blob and the statement are
//! the ones a real signing act stores (D-1Z951, D-17AA8). That signer builds the fixture's
//! records only. The perf vectors do not use it: they are fixed bytes (statement, SSHSIG blob,
//! trust key), so the benchmark input cannot move with a change of the builder or the wrapper.
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
use ssh_key::public::KeyData;
use ssh_key::{Algorithm, PrivateKey, PublicKey, SigningKey};

/// The seed of the person's key.
const PERSON_SEED: [u8; 32] = [0x70; 32];
/// The seed of the agent's key.
const AGENT_SEED: [u8; 32] = [0xa6; 32];

/// A fixed test key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestKey {
    /// The person's key: signs approvals and tags.
    Person,
    /// The agent's key: signs relays.
    Agent,
}

/// A held plain `ssh-ed25519` key, by its seed.
#[derive(Clone, Copy)]
struct Material([u8; 32]);

impl Material {
    fn public_key(&self) -> PublicKey {
        PublicKey::from(KeyData::Ed25519(Ed25519Keypair::from_seed(&self.0).public))
    }

    fn blob(&self) -> Vec<u8> {
        self.public_key()
            .to_bytes()
            .expect("a test public key encodes")
    }

    /// The SSH signature blob (algorithm, then signature) of `payload`, as an agent's sign
    /// response carries it.
    fn sign(&self, payload: &[u8]) -> Vec<u8> {
        let key = PrivateKey::from(Ed25519Keypair::from_seed(&self.0));
        let mut out = Vec::new();
        put_string(&mut out, b"ssh-ed25519");
        put_string(&mut out, &raw_sign(&key, payload));
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
            held: vec![Material(PERSON_SEED), Material(AGENT_SEED)],
        }
    }

    /// The public-key blob of `key`.
    pub fn blob(key: TestKey) -> Vec<u8> {
        Material(seed_of(key)).blob()
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

/// The id of [`vector_landing`], pinned. The vectors below sign statements that name it, so a
/// change of the landing's encoding fails the test that pins it instead of moving the benchmark.
const VECTOR_SUBJECT: [u8; 32] = [
    0xb6, 0x22, 0xdd, 0x3d, 0x92, 0x05, 0x4b, 0x69, 0x03, 0x82, 0xca, 0x3d, 0x90, 0x77, 0x0c, 0xdd,
    0x17, 0xaf, 0xb6, 0x38, 0x2c, 0xc7, 0xe0, 0xd9, 0xac, 0x57, 0x85, 0x5f, 0x72, 0xeb, 0x59, 0xf4,
];

/// The repo the vectors are signed for.
const VECTOR_REPO: RepoId = RepoId([0x14; 16]);

/// The frozen plain-key vector. The statement is written out by hand; the SSHSIG blob was made
/// once by OpenSSH's `ssh-keygen -Y sign -n rdy-approval-v1` over exactly these bytes with a
/// throwaway Ed25519 key, so neither depends on rdy's statement builder or its SSHSIG wrapper.
const PLAIN_STATEMENT: &str = "rdy-statement: 1\nkind: approval\nrepo: 14141414141414141414141414141414\nproposal_version: 0303030303030303030303030303030303030303030303030303030303030303\nbase: none\nactor: human:qa\nassurance: local\nkey: SHA256:8nEdi2dFYm+E9JRNIKxJNA5RGKdcAaVkhr4NVgAnE/4\n";
const PLAIN_SIG: &str = "53534853494700000001000000330000000b7373682d6564323535313900000020a81bd5afaf10098a7606e43cd55ef8f253e2ae1088d77937c74e0df78ae7b8820000000f7264792d617070726f76616c2d76310000000000000006736861353132000000530000000b7373682d6564323535313900000040e00f585fdac44777b16afb623767f267a85d3ac92456d989759eda0401fe044fea662b05faaf8722814f5d49f8197e4ef0c93079f48f1d6bab7d27c5c599cc01";
const PLAIN_KEY_LINE: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKgb1a+vEAmKdgbkPNVe+PJT4q4QiNd5N8dODfeK57iC";

/// The frozen security-key vector: the same statement, for a simulated
/// `sk-ssh-ed25519@openssh.com` key whose authenticator reported user presence (flags 0x01) and
/// counter 7. OpenSSH cannot sign without a token, so the blob was produced by the standalone
/// script `fixtures/signing/sk_vector_sim.py` (Python standard library only, an RFC 8032
/// Ed25519 reference, the statement written out by hand, the SSHSIG and U2F encodings built
/// from OpenSSH's PROTOCOL.sshsig and PROTOCOL.u2f). It shares no code with rdy's statement
/// builder or SSHSIG wrapper. Ed25519 is deterministic, so the script reproduces these bytes
/// exactly, and `ssh-keygen -Y verify` accepts the blob ("Good ... ED25519-SK key"). The
/// script's SHA-256 is pinned by a test below. No token is touched when the vector is used.
const SK_STATEMENT: &str = "rdy-statement: 1\nkind: approval\nrepo: 14141414141414141414141414141414\nproposal_version: 0303030303030303030303030303030303030303030303030303030303030303\nbase: none\nactor: human:qa\nassurance: local\nkey: SHA256:2V6EF6XZaGa6R+yy2uTYpfxOTHPP4+nkupDfZcK0hx8\n";
const SK_SIG: &str = "535348534947000000010000004a0000001a736b2d7373682d65643235353139406f70656e7373682e636f6d000000206b734a8eff246fe734b38d4046c148eee5f04fe87b3a0a423955a77956de066b000000047373683a0000000f7264792d617070726f76616c2d76310000000000000006736861353132000000670000001a736b2d7373682d65643235353139406f70656e7373682e636f6d000000404b49af80beaf8a68f32f81972926c44110ecd12739ef5e76268d3966b639e494d74d2ffa2c741d78ad07aa2674279c7b342b480db8b63fa7d25fa94f5527b80c0100000007";
const SK_KEY_LINE: &str = "sk-ssh-ed25519@openssh.com AAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2guY29tAAAAIGtzSo7/JG/nNLONQEbBSO7l8E/oezoKQjlVp3lW3gZrAAAABHNzaDo=";

/// Bytes from lowercase hex. Only the constants above go through it.
fn unhex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2), "odd hex");
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
        .collect()
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

/// The frozen vector of a plain Ed25519 key (`sk` false) or of a simulated security key whose
/// authenticator reports user presence (`sk` true). Both are fixed public bytes: nothing is
/// signed here, and the parsed trust input is made once, for the caller to hold outside its
/// measured loop.
pub fn vector(sk: bool) -> Vector {
    let (statement, sig, key_line) = if sk {
        (SK_STATEMENT, SK_SIG, SK_KEY_LINE)
    } else {
        (PLAIN_STATEMENT, PLAIN_SIG, PLAIN_KEY_LINE)
    };
    let hash = Hash(VECTOR_SUBJECT);
    let trust = AllowedSigners::parse(format!("human:qa {key_line}\n").as_bytes())
        .expect("the trust line parses");
    Vector {
        record: Signature {
            subject: hash,
            namespace: "rdy-approval-v1".to_string(),
            statement: statement.as_bytes().to_vec(),
            sig: unhex(sig),
            signed_by: Provenance {
                actor: "human:qa".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1,
            },
        },
        context: CheckContext {
            subject: SignatureSubject::Landing {
                hash,
                record: vector_landing(),
            },
            current_repo: VECTOR_REPO,
            source_repos: vec![],
            holder: false,
        },
        trust,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_engine::signature_check::check_signature;
    use ssh_key::sha2::{Digest, Sha256};

    fn sha256_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Task 10 fix 1: the perf vectors are fixed public bytes, not made at run time by the
    /// statement builder and the SSHSIG wrapper under test. The checksums below are of the
    /// bytes themselves, so a coordinated change of the producer and the checker cannot move
    /// the benchmark input without failing here.
    #[test]
    fn sig10_the_perf_vectors_are_frozen_public_bytes() {
        let want = [
            (
                false,
                "SHA256:8nEdi2dFYm+E9JRNIKxJNA5RGKdcAaVkhr4NVgAnE/4",
                "402a5cec3fcd631d36547912142918487891c121e91245fa01d2c6c1064305b4",
                "029e751d4494054335282221f7b458cfc4c34d18e64cb8edf91c8026350daf83",
            ),
            (
                true,
                "SHA256:2V6EF6XZaGa6R+yy2uTYpfxOTHPP4+nkupDfZcK0hx8",
                "93c15d64d7b95a131f96bfa67b1437e3e7935f2ded9083c2be516cd01afc282b",
                "f8303a249490514885040c449e8145a49c3f19bdccfb3c4aaca1fdb000dffc1a",
            ),
        ];
        for (sk, fingerprint, statement_sum, sig_sum) in want {
            let v = vector(sk);
            assert_eq!(sha256_hex(&v.record.statement), statement_sum, "sk {sk}");
            assert_eq!(sha256_hex(&v.record.sig), sig_sum, "sk {sk}");
            assert_eq!(v.record.namespace, "rdy-approval-v1");
            assert_eq!(v.record.subject, Hash(VECTOR_SUBJECT));
            let c = check_signature(&v.record, &v.context, Some(&v.trust));
            assert_eq!(c.code, None, "{c:?}");
            assert_eq!(c.key.as_deref(), Some(fingerprint), "sk {sk}");
        }
        // the subject is the landing the context holds: a change of the landing's encoding
        // fails here, not silently in the benchmark
        let v = vector(false);
        match &v.context.subject {
            SignatureSubject::Landing { hash, record } => {
                assert_eq!(*hash, Hash(VECTOR_SUBJECT));
                assert_eq!(record.id(), *hash);
            }
            other => panic!("not a landing: {other:?}"),
        }
        // the plain vector is made by a key rdy's test agent does not hold
        let c = check_signature(&v.record, &v.context, Some(&v.trust));
        assert_ne!(c.key, Some(QaAgent::fingerprint(TestKey::Person)));
        assert_ne!(c.key, Some(QaAgent::fingerprint(TestKey::Agent)));
    }

    /// Runs the simulator with `program`. `None` when the program is not installed; any other
    /// launch error fails the test.
    fn run_simulator(program: &str, path: &str) -> Option<std::process::Output> {
        match std::process::Command::new(program).arg(path).output() {
            Ok(out) => Some(out),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => panic!("cannot launch {program}: {e}"),
        }
    }

    /// Task 10 fix round 3: a missing python3 is a counted skip, never a silent pass.
    #[test]
    fn sig10_a_missing_python_is_reported_not_passed_silently() {
        assert!(run_simulator("rdy-no-such-python-for-qa", "x.py").is_none());
    }

    const SK_SCRIPT: &str = include_str!("../fixtures/signing/sk_vector_sim.py");

    /// Task 10 fix round 2: the security-key vector comes from a standalone producer that does
    /// not use rdy. The script is pinned by checksum, it imports nothing from rdy, and (when
    /// python3 is present) running it reproduces the pinned statement, blob and trust line.
    #[test]
    fn sig10_the_sk_vector_comes_from_the_standalone_simulator() {
        assert_eq!(
            sha256_hex(SK_SCRIPT.as_bytes()),
            "e739338e76be2dab55617e995d79a7eec423254b87fbec8054f1a7f64480bc12",
            "the simulator script changed: the vector's provenance is pinned"
        );
        for line in SK_SCRIPT.lines() {
            let t = line.trim_start();
            if t.starts_with("import ") || t.starts_with("from ") {
                assert!(
                    matches!(t, "import base64" | "import hashlib" | "import struct"),
                    "the simulator imports only the standard library: {t}"
                );
            }
        }
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/signing/sk_vector_sim.py"
        );
        let Some(out) = run_simulator("python3", path) else {
            crate::skip::skip(
                "signing::tests::sig10_the_sk_vector_comes_from_the_standalone_simulator",
                crate::skip::Why::Prerequisite,
                "python3 is not installed: the simulator's output was not reproduced",
            );
            return;
        };
        assert!(out.status.success(), "{out:?}");
        let text = String::from_utf8(out.stdout).expect("utf8");
        let field = |name: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(name))
                .expect("field")
                .trim()
                .to_string()
        };
        assert_eq!(field("key_line:"), SK_KEY_LINE);
        assert_eq!(field("sig_hex:"), SK_SIG);
        assert_eq!(
            field("statement:"),
            SK_STATEMENT.replace('\n', "\\n"),
            "statement"
        );
    }
}
