//! SSHSIG signing through the caller's SSH agent and its offline check (sig3).
//!
//! Interoperability vectors (all public; no private key is committed):
//!
//! - `fixtures/sshsig/sshsig_ed25519` and `id_ed25519.pub`, `sshsig_sk_ed25519` and
//!   `id_sk_ed25519_2.pub`: copied from `tests/examples` of the ssh-key 0.6.7 crate. Message
//!   `testing`, namespace `example`, sha512; the sk signature has flags 0x01 and counter 9.
//!   sha256 of the copies:
//!   - `sshsig_ed25519`      3a93dffb17b883d281b6b22750bd2106cb458544c1b70973c502f82f49991f80
//!   - `id_ed25519.pub`      966ea1bb9cbee91b8e19f8d690ddab8d11e6470d8fc1c80e80f96f5cce9cbddc
//!   - `sshsig_sk_ed25519`   c71d66aaea3f24c7e3bc29c85c63c603a25df982cc2ad4f15dac205cb725872b
//!   - `id_sk_ed25519_2.pub` 4379052f1d6a4ab0dda8ce072d40bf61ea63288efa3ddeda5981a4b50eda9a41
//!
//!   OpenSSH 10.3p1 verifies both independently (an allowed-signers line
//!   `test@example.com <type> <base64>` per key, the message on stdin):
//!   `ssh-keygen -Y verify -f <allowed> -I test@example.com -n example -s <sig> < msg` printed
//!   `Good "example" signature ... with ED25519 key SHA256:UCUiLr7Pjs9wFFJMDByLgc3NrtdU344OgUM45wZPcIQ`
//!   and `... with ED25519-SK key SHA256:6gcI7q24Uf/4jZEyRdzDIZ2x5pW2AxPAQt/lgHoHae0`.
//!
//! - `fixtures/sshsig/rdy_*.sig`: made by the test-only software signer below from fixed
//!   in-memory seeds (ssh-key 0.6.7 encoding, ed25519-dalek 2.2.0 signatures). An sk signature
//!   signs `sha256("ssh:") || flags || counter (u32 BE) || sha256(signed data)`. The test
//!   rebuilds each one and requires the committed bytes; `RDY_SIG3_BLESS=1` rewrites them.
//!   OpenSSH 10.3p1 verifies every one of them, and `rdy_ed25519_reserved.sig` (the OpenSSH
//!   ed25519 vector with reserved bytes `x` put in) too, with
//!   `ssh-keygen -Y verify -f <allowed> -I rdy -n <namespace> -s <sig> < <statement>`.
//!   sha256 of each committed vector is in `RDY_VECTORS` and `RESERVED_SHA256`; OpenSSH printed
//!   `Good "rdy-approval-v1" signature for rdy with ED25519 key
//!   SHA256:8ZCuVRkvejTusxtyIn0cmdknpX24x3ZTBvaclwcO8tg` and `Good "rdy-tag-v1"` (flags 0) and
//!   `Good "rdy-relay-v1"` (flags 1) `... with ED25519-SK key
//!   SHA256:N9QjjOTfxWypv0URSST718IWJrQcnFKJ/L6s71QNB8g`, using the committed public lines
//!   `rdy_ed25519.pub` (sha256 efd2bb9a67434f9b295fef1407159eba1fb997b7e6fd2fa42cea9678652e8652)
//!   and `rdy_sk_ed25519.pub` (4e594308e4f79fb265ae9d329690fbd57775ac91fcdeab82e5d02030b95a1492).
//!   It refused the flags-0 vector with the flags byte set to 1, and with the counter changed:
//!   `Signature verification failed: incorrect signature`.
//!
//! - `id_rsa_3072.pub`, `id_ecdsa_p256.pub`, `id_sk_ecdsa_p256.pub`, `id_ed25519-cert.pub` and
//!   `id_sk_ed25519-cert.pub`: public keys of unsupported types from the same ssh-key examples.

use ed25519_dalek::Signer as _;
use rdy_engine::signing_crypto::{
    AgentIdentity, CryptoCheck, KeyInfo, NAMESPACES, SigningAgent, UnavailableAgent,
    key_fingerprint, resolve_key, sign_sshsig, verify_sshsig,
};
use rdy_engine::{
    E_BAD_INPUT, E_LIMIT, E_NO_AGENT, E_SIGNING_KEY_MISSING, E_SIGNING_KEY_TYPE, RdyError,
};
use ssh_encoding::base64::{Base64, Encoding};
use ssh_encoding::{Decode, Encode};
use ssh_key::public::{Ed25519PublicKey, KeyData, SkEd25519};
use ssh_key::sha2::{Digest, Sha256};
use ssh_key::{Algorithm, HashAlg, LineEnding, Signature, SshSig};
use std::path::PathBuf;
use std::sync::Mutex;

// ---- fixtures ----

const OPENSSH_ED25519_SIG: &str = include_str!("fixtures/sshsig/sshsig_ed25519");
const OPENSSH_ED25519_PUB: &str = include_str!("fixtures/sshsig/id_ed25519.pub");
const OPENSSH_SK_SIG: &str = include_str!("fixtures/sshsig/sshsig_sk_ed25519");
const OPENSSH_SK_PUB: &str = include_str!("fixtures/sshsig/id_sk_ed25519_2.pub");
const OPENSSH_ED25519_FP: &str = "SHA256:UCUiLr7Pjs9wFFJMDByLgc3NrtdU344OgUM45wZPcIQ";
const OPENSSH_SK_FP: &str = "SHA256:6gcI7q24Uf/4jZEyRdzDIZ2x5pW2AxPAQt/lgHoHae0";

/// The unsupported public keys: (file, its key type).
const UNSUPPORTED: [(&str, &str); 5] = [
    (include_str!("fixtures/sshsig/id_rsa_3072.pub"), "ssh-rsa"),
    (
        include_str!("fixtures/sshsig/id_ecdsa_p256.pub"),
        "ecdsa-sha2-nistp256",
    ),
    (
        include_str!("fixtures/sshsig/id_sk_ecdsa_p256.pub"),
        "sk-ecdsa-sha2-nistp256@openssh.com",
    ),
    (
        include_str!("fixtures/sshsig/id_ed25519-cert.pub"),
        "ssh-ed25519-cert-v01@openssh.com",
    ),
    (
        include_str!("fixtures/sshsig/id_sk_ed25519-cert.pub"),
        "sk-ssh-ed25519-cert-v01@openssh.com",
    ),
];

/// The statement every software vector signs.
const STATEMENT: &[u8] = b"rdy sig3 test statement\n";

/// The test-only seeds of the software signer (in memory only).
const ED_SEED: [u8; 32] = [0x5a; 32];
const SK_SEED: [u8; 32] = [0xa5; 32];
/// A third key, which no fake agent holds.
const OTHER_SEED: [u8; 32] = [0x3c; 32];

/// The committed software vectors: (file, namespace, key, flags, sha256 of the file).
const RDY_VECTORS: [(&str, &str, Key, u8, &str); 3] = [
    (
        "rdy_ed25519.sig",
        "rdy-approval-v1",
        Key::Ed,
        0,
        "ff087b353b6fa3a2c27d4f3144abcfbb9ad62c97936c3a928cd453bcda14b1ec",
    ),
    (
        "rdy_sk_flags0.sig",
        "rdy-tag-v1",
        Key::Sk,
        0,
        "f5a5a3ec6dec2b6c0352ee27fed5e146212a0dbe583c2d798a69f87863bcbfdd",
    ),
    (
        "rdy_sk_flags1.sig",
        "rdy-relay-v1",
        Key::Sk,
        1,
        "78c95e27fbe9a7bc9c5ad1605ea2633ac52c43e67490e3aa60dfbde671b46d8e",
    ),
];
const RESERVED_FILE: &str = "rdy_ed25519_reserved.sig";
const RESERVED_SHA256: &str = "6e7cf2e6515b51cf920cff97bc45386a909ac5d03936f79a1ecbf6fc39f53d57";
/// The sk counter of the software vectors.
const COUNTER: u32 = 42;

/// `Encode` into a new vector.
trait EncodeVec {
    fn encode_vec(&self) -> Result<Vec<u8>, ssh_encoding::Error>;
}

impl<T: Encode> EncodeVec for T {
    fn encode_vec(&self) -> Result<Vec<u8>, ssh_encoding::Error> {
        let mut out = Vec::new();
        self.encode(&mut out)?;
        Ok(out)
    }
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sshsig")
}

fn bless() -> bool {
    std::env::var_os("RDY_SIG3_BLESS").is_some()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---- the software signer (test only) ----

#[derive(Clone, Copy, Debug, PartialEq)]
enum Key {
    Ed,
    Sk,
}

fn dalek(seed: &[u8; 32]) -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(seed)
}

fn ed_data(seed: &[u8; 32]) -> KeyData {
    KeyData::Ed25519(Ed25519PublicKey(dalek(seed).verifying_key().to_bytes()))
}

fn sk_data(seed: &[u8; 32]) -> KeyData {
    KeyData::SkEd25519(SkEd25519::new(
        Ed25519PublicKey(dalek(seed).verifying_key().to_bytes()),
        "ssh:",
    ))
}

fn blob(data: &KeyData) -> Vec<u8> {
    data.encode_vec().unwrap()
}

fn ed_blob() -> Vec<u8> {
    blob(&ed_data(&ED_SEED))
}

fn sk_blob() -> Vec<u8> {
    blob(&sk_data(&SK_SEED))
}

/// The OpenSSH public-key line of `blob`, with `comment` when it is not empty.
fn line(blob: &[u8], comment: &str) -> String {
    let ty = key_type_of(blob);
    let b64 = Base64::encode_string(blob);
    if comment.is_empty() {
        format!("{ty} {b64}")
    } else {
        format!("{ty} {b64} {comment}")
    }
}

fn key_type_of(blob: &[u8]) -> String {
    let n = u32::from_be_bytes(blob[..4].try_into().unwrap()) as usize;
    String::from_utf8(blob[4..4 + n].to_vec()).unwrap()
}

/// The blob of a public-key line (its second field).
fn line_blob(line: &str) -> Vec<u8> {
    Base64::decode_vec(line.split_whitespace().nth(1).unwrap()).unwrap()
}

/// A plain Ed25519 SSH signature blob over `data`.
fn ed_sign(seed: &[u8; 32], data: &[u8]) -> Vec<u8> {
    let raw = dalek(seed).sign(data).to_bytes();
    Signature::new(Algorithm::Ed25519, raw.to_vec())
        .unwrap()
        .encode_vec()
        .unwrap()
}

/// The raw bytes an sk authenticator signs for `data` under application `ssh:`.
fn sk_message(flags: u8, counter: u32, data: &[u8]) -> Vec<u8> {
    let mut m = Sha256::digest(b"ssh:").to_vec();
    m.push(flags);
    m.extend(counter.to_be_bytes());
    m.extend(Sha256::digest(data));
    m
}

/// An `sk-ssh-ed25519@openssh.com` SSH signature blob over `data`.
fn sk_sign(seed: &[u8; 32], data: &[u8], flags: u8, counter: u32) -> Vec<u8> {
    let mut raw = dalek(seed)
        .sign(&sk_message(flags, counter, data))
        .to_bytes()
        .to_vec();
    raw.push(flags);
    raw.extend(counter.to_be_bytes());
    Signature::new(Algorithm::SkEd25519, raw)
        .unwrap()
        .encode_vec()
        .unwrap()
}

/// A binary SSHSIG of `msg` made with the software signer, independently of `sign_sshsig`.
fn software_sshsig(key: Key, ns: &str, hash: HashAlg, msg: &[u8], flags: u8) -> Vec<u8> {
    let data = SshSig::signed_data(ns, hash, msg).unwrap();
    let (kd, sig) = match key {
        Key::Ed => (ed_data(&ED_SEED), ed_sign(&ED_SEED, &data)),
        Key::Sk => (sk_data(&SK_SEED), sk_sign(&SK_SEED, &data, flags, COUNTER)),
    };
    let sig = Signature::decode(&mut sig.as_slice()).unwrap();
    SshSig::new(kd, ns, hash, sig)
        .unwrap()
        .encode_vec()
        .unwrap()
}

fn pem_to_bin(pem: &str) -> Vec<u8> {
    SshSig::from_pem(pem).unwrap().encode_vec().unwrap()
}

fn bin_to_pem(bin: &[u8]) -> String {
    SshSig::decode(&mut &bin[..])
        .unwrap()
        .to_pem(LineEnding::LF)
        .unwrap()
}

/// `bin` with the reserved field replaced by `reserved`.
fn with_reserved(bin: &[u8], reserved: &[u8]) -> Vec<u8> {
    let u32_at = |at: usize| u32::from_be_bytes(bin[at..at + 4].try_into().unwrap()) as usize;
    let mut at = 6 + 4;
    at += 4 + u32_at(at); // public key
    at += 4 + u32_at(at); // namespace
    assert_eq!(u32_at(at), 0, "the vector has empty reserved bytes");
    let mut out = bin[..at].to_vec();
    out.extend((reserved.len() as u32).to_be_bytes());
    out.extend(reserved);
    out.extend(&bin[at + 4..]);
    out
}

/// `bin` with its version field set to `version`.
fn with_version(bin: &[u8], version: u32) -> Vec<u8> {
    let mut out = bin.to_vec();
    out[6..10].copy_from_slice(&version.to_be_bytes());
    out
}

// ---- a fake agent in memory ----

enum Replies {
    /// Sign with the software signer (sk: flags 1, counter `COUNTER`).
    Software,
    /// Answer every sign request with these bytes.
    Fixed(Vec<u8>),
}

struct FakeAgent {
    keys: Vec<Vec<u8>>,
    replies: Replies,
    /// Each sign request: (key blob, payload).
    signs: Mutex<Vec<(Vec<u8>, Vec<u8>)>>,
}

impl FakeAgent {
    fn new(keys: Vec<Vec<u8>>) -> FakeAgent {
        FakeAgent {
            keys,
            replies: Replies::Software,
            signs: Mutex::new(vec![]),
        }
    }

    fn fixed(keys: Vec<Vec<u8>>, reply: Vec<u8>) -> FakeAgent {
        FakeAgent {
            keys,
            replies: Replies::Fixed(reply),
            signs: Mutex::new(vec![]),
        }
    }

    fn sign_count(&self) -> usize {
        self.signs.lock().unwrap().len()
    }
}

impl SigningAgent for FakeAgent {
    fn identities(&self) -> Result<Vec<AgentIdentity>, RdyError> {
        Ok(self
            .keys
            .iter()
            .map(|k| AgentIdentity {
                public_key: k.clone(),
            })
            .collect())
    }

    fn sign(&self, public_key: &[u8], payload: &[u8]) -> Result<Vec<u8>, RdyError> {
        self.signs
            .lock()
            .unwrap()
            .push((public_key.to_vec(), payload.to_vec()));
        match &self.replies {
            Replies::Fixed(r) => Ok(r.clone()),
            Replies::Software if public_key == ed_blob() => Ok(ed_sign(&ED_SEED, payload)),
            Replies::Software if public_key == sk_blob() => {
                Ok(sk_sign(&SK_SEED, payload, 1, COUNTER))
            }
            Replies::Software => panic!("the fake agent holds no such key"),
        }
    }
}

fn err(r: Result<KeyInfo, RdyError>) -> RdyError {
    match r {
        Ok(k) => panic!("expected an error, got the key {}", k.fingerprint),
        Err(e) => e,
    }
}

fn sign_err(r: Result<Vec<u8>, RdyError>) -> RdyError {
    match r {
        Ok(_) => panic!("expected an error, got a signature"),
        Err(e) => e,
    }
}

fn assert_no_agent(e: &RdyError, cause: &str) {
    assert_eq!(e.code, E_NO_AGENT, "{e}");
    assert_eq!(e.message, format!("the SSH agent could not sign: {cause}"));
    assert_eq!(
        e.fix,
        "check SSH_AUTH_SOCK and the configured key, then try again"
    );
}

fn assert_key_type(e: &RdyError, key: &str, ty: &str) {
    assert_eq!(e.code, E_SIGNING_KEY_TYPE, "{e}");
    assert_eq!(
        e.message,
        format!("{key} has unsupported signing key type {ty}")
    );
    assert_eq!(
        e.fix,
        "set an ssh-ed25519 or sk-ssh-ed25519@openssh.com key"
    );
}

fn invalid() -> CryptoCheck {
    CryptoCheck {
        valid: false,
        key: None,
        key_type: None,
        presence: false,
    }
}

fn check(valid: bool, key: &str, ty: &str, presence: bool) -> CryptoCheck {
    CryptoCheck {
        valid,
        key: Some(key.to_string()),
        key_type: Some(ty.to_string()),
        presence,
    }
}

// ---- the pinned tests ----

#[test]
fn sig3_openssh_vectors_verify_for_both_key_types() {
    // The independent OpenSSH vectors: PEM is the oracle's form, the binary blob rdy's.
    for (pem, public, fp, ty, presence) in [
        (
            OPENSSH_ED25519_SIG,
            OPENSSH_ED25519_PUB,
            OPENSSH_ED25519_FP,
            "ssh-ed25519",
            false,
        ),
        (
            OPENSSH_SK_SIG,
            OPENSSH_SK_PUB,
            OPENSSH_SK_FP,
            "sk-ssh-ed25519@openssh.com",
            true,
        ),
    ] {
        let bin = pem_to_bin(pem);
        assert_eq!(
            bin_to_pem(&bin).trim_end(),
            pem.trim_end(),
            "PEM round trip"
        );
        let sig = SshSig::decode(&mut &bin[..]).unwrap();
        assert_eq!(sig.hash_alg(), HashAlg::Sha512);
        assert_eq!(sig.version(), 1);
        assert_eq!(key_fingerprint(public.trim_end()), Ok(fp.to_string()));
        assert_eq!(
            verify_sshsig("example", b"testing", &bin),
            check(true, fp, ty, presence),
            "{ty}"
        );
        // another message, another namespace: the key is still reported
        assert_eq!(
            verify_sshsig("example", b"testinG", &bin),
            check(false, fp, ty, false)
        );
        assert_eq!(
            verify_sshsig("rdy-approval-v1", b"testing", &bin),
            check(false, fp, ty, false)
        );
        // a statement over 16384 bytes is never valid
        assert_eq!(
            verify_sshsig("example", &[b't'; 16385], &bin),
            check(false, fp, ty, false)
        );
        // trailing bytes, a damaged length, a version other than 1, over the bound: invalid
        let mut trailing = bin.clone();
        trailing.push(0);
        assert!(!verify_sshsig("example", b"testing", &trailing).valid);
        let mut cut = bin.clone();
        cut.truncate(bin.len() - 1);
        assert!(!verify_sshsig("example", b"testing", &cut).valid);
        assert_eq!(
            verify_sshsig("example", b"testing", &with_version(&bin, 0)),
            invalid()
        );
        assert_eq!(
            verify_sshsig("example", b"testing", &with_version(&bin, 2)),
            invalid()
        );
        assert_eq!(verify_sshsig("example", b"testing", b"SSHSIG"), invalid());
        assert_eq!(verify_sshsig("example", b"testing", &[]), invalid());
        let mut big = bin.clone();
        big.resize(16385, 0);
        assert_eq!(verify_sshsig("example", b"testing", &big), invalid());
    }

    // The software vectors under rdy namespaces, committed and verified by OpenSSH. The
    // software keys' public lines are committed too, for the OpenSSH oracle.
    let dir = fixture_dir();
    for (file, b) in [
        ("rdy_ed25519.pub", ed_blob()),
        ("rdy_sk_ed25519.pub", sk_blob()),
    ] {
        let text = format!("{}\n", line(&b, "rdy-sig3-test"));
        if bless() {
            std::fs::write(dir.join(file), &text).unwrap();
        } else {
            assert_eq!(
                std::fs::read_to_string(dir.join(file)).unwrap(),
                text,
                "{file}"
            );
        }
    }
    for (file, ns, key, flags, digest) in RDY_VECTORS {
        let bin = software_sshsig(key, ns, HashAlg::Sha512, STATEMENT, flags);
        let pem = bin_to_pem(&bin);
        if bless() {
            std::fs::write(dir.join(file), &pem).unwrap();
            println!("{file} {}", sha256_hex(pem.as_bytes()));
            continue;
        }
        let committed = std::fs::read_to_string(dir.join(file)).unwrap();
        assert_eq!(committed, pem, "{file} is the software signer's output");
        assert_eq!(sha256_hex(committed.as_bytes()), digest, "{file} checksum");
        let (fp, ty) = match key {
            Key::Ed => (key_fingerprint(&line(&ed_blob(), "")), "ssh-ed25519"),
            Key::Sk => (
                key_fingerprint(&line(&sk_blob(), "")),
                "sk-ssh-ed25519@openssh.com",
            ),
        };
        let fp = fp.unwrap();
        assert_eq!(
            verify_sshsig(ns, STATEMENT, &pem_to_bin(&committed)),
            check(true, &fp, ty, flags & 1 == 1),
            "{file}"
        );
    }

    // Reserved bytes are ignored, as OpenSSH ignores them: the OpenSSH ed25519 vector with
    // reserved `x` put in still verifies (OpenSSH 10.3p1 says Good for the committed file).
    let reserved = with_reserved(&pem_to_bin(OPENSSH_ED25519_SIG), b"x");
    let pem = bin_to_pem(&reserved);
    if bless() {
        std::fs::write(dir.join(RESERVED_FILE), &pem).unwrap();
        println!("{RESERVED_FILE} {}", sha256_hex(pem.as_bytes()));
    } else {
        let committed = std::fs::read_to_string(dir.join(RESERVED_FILE)).unwrap();
        assert_eq!(committed, pem);
        assert_eq!(sha256_hex(committed.as_bytes()), RESERVED_SHA256);
    }
    assert_eq!(
        verify_sshsig("example", b"testing", &reserved),
        check(true, OPENSSH_ED25519_FP, "ssh-ed25519", false)
    );

    // sha256 is refused for both key types, though the signature itself is good.
    for key in [Key::Ed, Key::Sk] {
        let bin = software_sshsig(key, "rdy-tag-v1", HashAlg::Sha256, STATEMENT, 1);
        let c = verify_sshsig("rdy-tag-v1", STATEMENT, &bin);
        assert!(!c.valid && !c.presence, "{key:?} sha256 is refused");
        assert!(
            c.key.is_some() && c.key_type.is_some(),
            "the key is reported"
        );
    }

    // A signature algorithm other than its key's is refused.
    let bin = software_sshsig(Key::Ed, "rdy-tag-v1", HashAlg::Sha512, STATEMENT, 0);
    let sig = SshSig::decode(&mut &bin[..]).unwrap();
    let data = SshSig::signed_data("rdy-tag-v1", HashAlg::Sha512, STATEMENT).unwrap();
    let sk_sig = Signature::decode(&mut sk_sign(&ED_SEED, &data, 1, 1).as_slice()).unwrap();
    let crossed = SshSig::new(
        sig.public_key().clone(),
        "rdy-tag-v1",
        HashAlg::Sha512,
        sk_sig,
    )
    .unwrap()
    .encode_vec()
    .unwrap();
    let c = verify_sshsig("rdy-tag-v1", STATEMENT, &crossed);
    assert!(!c.valid && !c.presence);
    assert_eq!(c.key_type.as_deref(), Some("ssh-ed25519"));
}

#[test]
fn sig3_presence_is_authenticated_not_inferred() {
    let ns = "rdy-approval-v1";
    let sk_fp = key_fingerprint(&line(&sk_blob(), "")).unwrap();
    let ed_fp = key_fingerprint(&line(&ed_blob(), "")).unwrap();
    let sk = "sk-ssh-ed25519@openssh.com";
    let flags0 = software_sshsig(Key::Sk, ns, HashAlg::Sha512, STATEMENT, 0);
    let flags1 = software_sshsig(Key::Sk, ns, HashAlg::Sha512, STATEMENT, 1);
    // valid sk signatures report the authenticated bit, set or clear
    assert_eq!(
        verify_sshsig(ns, STATEMENT, &flags0),
        check(true, &sk_fp, sk, false)
    );
    assert_eq!(
        verify_sshsig(ns, STATEMENT, &flags1),
        check(true, &sk_fp, sk, true)
    );
    // other flag bits do not make presence
    let flags4 = software_sshsig(Key::Sk, ns, HashAlg::Sha512, STATEMENT, 0x04);
    assert_eq!(
        verify_sshsig(ns, STATEMENT, &flags4),
        check(true, &sk_fp, sk, false)
    );
    // the flags byte is the fifth from the end, the counter the last four
    let n = flags0.len();
    assert_eq!(flags0[n - 5], 0);
    let mut flipped = flags0.clone();
    flipped[n - 5] = 1;
    assert_eq!(
        verify_sshsig(ns, STATEMENT, &flipped),
        check(false, &sk_fp, sk, false),
        "a flipped presence bit fails the signature"
    );
    let mut cleared = flags1.clone();
    cleared[n - 5] = 0;
    assert_eq!(
        verify_sshsig(ns, STATEMENT, &cleared),
        check(false, &sk_fp, sk, false)
    );
    let mut counter = flags1.clone();
    counter[n - 1] ^= 1;
    assert_eq!(
        verify_sshsig(ns, STATEMENT, &counter),
        check(false, &sk_fp, sk, false),
        "an altered counter fails the signature"
    );
    // ordinary Ed25519 never reports presence
    let ed = software_sshsig(Key::Ed, ns, HashAlg::Sha512, STATEMENT, 1);
    assert_eq!(
        verify_sshsig(ns, STATEMENT, &ed),
        check(true, &ed_fp, "ssh-ed25519", false)
    );
}

#[test]
fn sig3_agent_rejects_types_before_signing() {
    for (public, ty) in UNSUPPORTED {
        let public = public.trim_end();
        let unsupported = line_blob(public);
        let fp = key_fingerprint(public).unwrap();
        let agent = FakeAgent::new(vec![unsupported.clone(), ed_blob()]);
        // by its public line, and by its fingerprint
        assert_key_type(&err(resolve_key("signing.key", public, &agent)), &fp, ty);
        assert_key_type(&err(resolve_key("signing.key", &fp, &agent)), &fp, ty);
        // a key info that names it is refused by the wrapper too, whatever type it claims
        for claimed in [ty, "ssh-ed25519"] {
            let info = KeyInfo {
                public_key: unsupported.clone(),
                fingerprint: fp.clone(),
                key_type: claimed.to_string(),
            };
            let e = sign_err(sign_sshsig(&agent, &info, "rdy-approval-v1", STATEMENT));
            assert_key_type(&e, &fp, ty);
        }
        assert_eq!(agent.sign_count(), 0, "{ty}: no sign request was sent");
    }
}

#[test]
fn sig3_public_line_and_fingerprint_select_the_same_identity() {
    let agent = FakeAgent::new(vec![ed_blob(), sk_blob()]);
    for (b, ty) in [
        (ed_blob(), "ssh-ed25519"),
        (sk_blob(), "sk-ssh-ed25519@openssh.com"),
    ] {
        let fp = key_fingerprint(&line(&b, "")).unwrap();
        let expected = KeyInfo {
            public_key: b.clone(),
            fingerprint: fp.clone(),
            key_type: ty.to_string(),
        };
        for selector in [
            line(&b, ""),
            line(&b, "me@example.com"),
            line(&b, "a comment with spaces"),
            format!("  {}  \n", line(&b, "padded")),
            fp.clone(),
        ] {
            assert_eq!(key_fingerprint(&selector), Ok(fp.clone()), "{selector:?}");
            let got = resolve_key("signing.key", &selector, &agent).unwrap();
            assert_eq!(got, expected, "{selector:?}");
        }
    }
    // the fingerprints are OpenSSH's
    assert_eq!(
        key_fingerprint(OPENSSH_ED25519_PUB.trim_end()),
        Ok(OPENSSH_ED25519_FP.to_string())
    );
    assert_eq!(
        key_fingerprint(OPENSSH_SK_PUB.trim_end()),
        Ok(OPENSSH_SK_FP.to_string())
    );
    assert_eq!(
        key_fingerprint(OPENSSH_SK_FP),
        Ok(OPENSSH_SK_FP.to_string())
    );

    // a key the agent does not hold
    let other = blob(&ed_data(&OTHER_SEED));
    let other_fp = key_fingerprint(&line(&other, "")).unwrap();
    for selector in [line(&other, "x"), other_fp.clone()] {
        assert_no_agent(
            &err(resolve_key("signing.agent_key", &selector, &agent)),
            "key is not held",
        );
    }
    // an unsupported public line the agent does not hold
    let (rsa, _) = UNSUPPORTED[0];
    let rsa_fp = key_fingerprint(rsa.trim_end()).unwrap();
    assert_key_type(
        &err(resolve_key("signing.key", rsa.trim_end(), &agent)),
        &rsa_fp,
        "ssh-rsa",
    );

    // fingerprints in any other spelling, and lines that are not public keys
    let mut zero_bits = OPENSSH_SK_FP.to_string();
    zero_bits.pop();
    zero_bits.push('1'); // '0' ends in two zero bits, '1' does not
    let ed_line = line(&ed_blob(), "");
    let (ty, b64) = ed_line.split_once(' ').unwrap();
    for bad in [
        format!("{OPENSSH_SK_FP}="),
        OPENSSH_SK_FP.replace("SHA256:", "sha256:"),
        OPENSSH_SK_FP.replace("SHA256:", "SHA256: "),
        OPENSSH_SK_FP[..OPENSSH_SK_FP.len() - 1].to_string(),
        format!("{OPENSSH_SK_FP}A"),
        "SHA256:".to_string(),
        zero_bits,
        "MD5:6g:c1".to_string(),
        "ssh-ed25519".to_string(),
        format!("ssh-ed25519 {}", &b64[..b64.len() - 4]),
        format!("sk-ssh-ed25519@openssh.com {b64}"),
        format!("{ty} {b64}!"),
        format!("{ty}  not-base64"),
        format!("{ty} {}", Base64::encode_string(b"\0\0\0\x0bssh-ed25519")),
        "not a key".to_string(),
    ] {
        assert_eq!(key_fingerprint(&bad), Err(()), "{bad:?}");
        let e = err(resolve_key("signing.agent_key", &bad, &agent));
        assert_eq!(e.code, E_BAD_INPUT, "{bad:?}: {e}");
        assert_eq!(
            e.message,
            "signing.agent_key is not a public key or SHA256 fingerprint"
        );
        assert_eq!(
            e.fix,
            "set signing.agent_key to a public key line or its SHA256 fingerprint"
        );
    }
    // an empty selector is a missing key
    for empty in ["", "   "] {
        let e = err(resolve_key("signing.key", empty, &agent));
        assert_eq!(e.code, E_SIGNING_KEY_MISSING);
        assert_eq!(e.message, "signing.key is not set");
        assert_eq!(
            e.fix,
            "set signing.key in the user config or .rdy/local.yaml"
        );
    }
    // no agent at all
    assert_no_agent(
        &err(resolve_key(
            "signing.key",
            &line(&ed_blob(), ""),
            &UnavailableAgent,
        )),
        "SSH_AUTH_SOCK is not set",
    );
    assert_eq!(agent.sign_count(), 0);
}

#[test]
fn sig3_agent_reply_is_wrapped_without_crypto_check() {
    let agent = FakeAgent::new(vec![sk_blob(), ed_blob()]);
    for (selector, ty, presence) in [
        (line(&ed_blob(), "e"), "ssh-ed25519", false),
        (line(&sk_blob(), "s"), "sk-ssh-ed25519@openssh.com", true),
    ] {
        let key = resolve_key("signing.key", &selector, &agent).unwrap();
        for ns in NAMESPACES {
            let before = agent.sign_count();
            let out = sign_sshsig(&agent, &key, ns, STATEMENT).unwrap();
            assert_eq!(agent.sign_count(), before + 1);
            // the agent was asked to sign exactly the sha512 SSHSIG signed data with that key
            let (asked_key, payload) = agent.signs.lock().unwrap().last().unwrap().clone();
            assert_eq!(asked_key, key.public_key);
            assert_eq!(
                payload,
                SshSig::signed_data(ns, HashAlg::Sha512, STATEMENT).unwrap()
            );
            // the binary SSHSIG v1 fixes the key, the namespace and sha512, with empty reserved
            let sig = SshSig::decode(&mut &out[..]).unwrap();
            assert_eq!(sig.version(), 1);
            assert_eq!(blob(sig.public_key()), key.public_key);
            assert_eq!(sig.namespace(), ns);
            assert_eq!(sig.reserved(), b"");
            assert_eq!(sig.hash_alg(), HashAlg::Sha512);
            assert_eq!(sig.encode_vec().unwrap(), out, "binary, canonical");
            assert_eq!(
                verify_sshsig(ns, STATEMENT, &out),
                check(true, &key.fingerprint, ty, presence)
            );
        }
    }

    // A well-encoded bad signature reaches the caller, for its one pre-write check.
    let ed = blob(&ed_data(&ED_SEED));
    let zeros = Signature::new(Algorithm::Ed25519, vec![0u8; 64])
        .unwrap()
        .encode_vec()
        .unwrap();
    let bad = FakeAgent::fixed(vec![ed.clone()], zeros);
    let key = resolve_key("signing.key", &line(&ed, ""), &bad).unwrap();
    let out = sign_sshsig(&bad, &key, "rdy-relay-v1", STATEMENT).unwrap();
    let c = verify_sshsig("rdy-relay-v1", STATEMENT, &out);
    assert_eq!(c, check(false, &key.fingerprint, "ssh-ed25519", false));

    // Malformed replies: the wrapper refuses each one as an invalid reply.
    let data = SshSig::signed_data("rdy-relay-v1", HashAlg::Sha512, STATEMENT).unwrap();
    let good = ed_sign(&ED_SEED, &data);
    let mut trailing = good.clone();
    trailing.push(0);
    let short = {
        let mut s = b"\0\0\0\x0bssh-ed25519\0\0\0\x3f".to_vec();
        s.extend([0u8; 63]);
        s
    };
    let long_len = {
        let mut s = b"\0\0\0\x0bssh-ed25519\0\0\0\x41".to_vec();
        s.extend([0u8; 64]);
        s
    };
    let huge_len = {
        let mut s = b"\0\0\0\x0bssh-ed25519\xff\xff\xff\xff".to_vec();
        s.extend([0u8; 64]);
        s
    };
    let mut oversized = good.clone();
    oversized.resize(16385, 0);
    for (what, reply) in [
        ("empty", vec![]),
        ("truncated", good[..good.len() - 1].to_vec()),
        ("trailing bytes", trailing),
        ("63-byte signature", short),
        ("length past the end", long_len),
        ("length 2^32-1", huge_len),
        (
            "sk signature for an ed25519 key",
            sk_sign(&ED_SEED, &data, 1, 1),
        ),
        (
            "rsa algorithm",
            b"\0\0\0\x0crsa-sha2-512\0\0\0\x04abcd".to_vec(),
        ),
        ("over 16384 bytes", oversized),
    ] {
        let agent = FakeAgent::fixed(vec![ed.clone()], reply);
        let e = sign_err(sign_sshsig(&agent, &key, "rdy-relay-v1", STATEMENT));
        assert_no_agent(&e, "invalid reply");
        assert_eq!(agent.sign_count(), 1, "{what}");
    }

    // Namespaces outside the four, and inputs over their bounds, send nothing.
    let agent = FakeAgent::new(vec![ed.clone()]);
    for ns in ["example", "rdy-approval-v2", "", "file"] {
        let e = sign_err(sign_sshsig(&agent, &key, ns, STATEMENT));
        assert_eq!(e.code, E_BAD_INPUT, "{ns:?}");
    }
    let long_ns = "n".repeat(65);
    let e = sign_err(sign_sshsig(&agent, &key, &long_ns, STATEMENT));
    assert_eq!(e.code, E_LIMIT);
    assert_eq!(e.message, "signing input exceeds 64 bytes");
    assert_eq!(e.fix, "reduce the input to the stated limit");
    let big = vec![b'a'; 16385];
    let e = sign_err(sign_sshsig(&agent, &key, "rdy-tag-v1", &big));
    assert_eq!(e.code, E_LIMIT);
    assert_eq!(e.message, "signing input exceeds 16384 bytes");
    assert_eq!(agent.sign_count(), 0);
    // exactly 16384 bytes is in bounds
    sign_sshsig(&agent, &key, "rdy-tag-v1", &big[..16384]).unwrap();
    assert_eq!(agent.sign_count(), 1);
}
