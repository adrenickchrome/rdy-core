#![cfg(unix)]
// Unix only: it writes 0600 credential fixtures in private config directories. FW-XP1 Phase 4 ports it.
//! hsa7: key bind, retire and revoke through SIG and the hub's key API (D-XXA9D, D-17AA8,
//! D-MRQB6). Bind proves possession of the configured key with SIG's keybind statement and the
//! caller's agent, and returns the hub's confirmation link; retire and revoke look the hub id up
//! by fingerprint, then change it once. A fake transport records every typed request; the
//! loopback TLS fixture checks the raw requests where the wire bytes matter. Channels order the
//! agent wait; nothing sleeps or measures time.

#[path = "support/hub_tls.rs"]
mod hub_tls;

use ed25519_dalek::Signer;
use hub_tls::{Reply as TlsReply, Server};
use rdy_engine::config_edit::LockedConfig;
use rdy_engine::config_levels::effective;
use rdy_engine::hub_config::{
    Credential, CredentialFile, HubConfig, ReadyCredential, RemoteMode, encode_credentials,
    parse_credentials,
};
use rdy_engine::hub_credentials::HubCredentials;
use rdy_engine::hub_http::HttpHubTransport;
use rdy_engine::hub_keys::{BindReply, KeyService, KeyStateReply, RevokeReply};
use rdy_engine::hub_remotes::{RemoteLevel, RemoteService};
use rdy_engine::hub_types::{
    BindResult, HubCall, HubClock, HubFailure, HubKey, HubOperation, HubReply, HubTransport,
    KeyChallenge, KeyKind, KeyState, MeReply, Origin, Secret,
};
use rdy_engine::signing_config::{SignMode, SigningConfig};
use rdy_engine::signing_crypto::{
    AgentIdentity, NS_KEYBIND, SigningAgent, blob_fingerprint, key_fingerprint, verify_sshsig,
};
use rdy_engine::{
    E_ACTOR_UNBOUND, E_BAD_INPUT, E_NO_AGENT, E_NOT_FOUND, E_REMOTE_REQUIRED,
    E_SIGNING_KEY_MISSING, E_UPSTREAM_ACTOR_CLAIM, E_UPSTREAM_AUTH, E_UPSTREAM_KEY_PROOF,
    E_UPSTREAM_PROTOCOL, E_UPSTREAM_REFUSED, E_UPSTREAM_TRANSPORT, E_UPSTREAM_UNCERTAIN, Rdy,
    RdyError, Session, SessionKind, W_UPSTREAM_HANDLE_CHANGED,
};
use rdy_model::Role;
use rdy_model::signature::{StatementSource, statement};
use ssh_encoding::base64::{Base64, Encoding};
use ssh_encoding::{Decode, Encode};
use ssh_key::public::{Ed25519PublicKey, KeyData};
use ssh_key::{Algorithm, HashAlg, Signature, SshSig};
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, mpsc};

// ---------------------------------------------------------------------------------------------
// The settled fixtures (TASK-237: cd-540FX, cd-VMRX2, cd-8EQJ5, cd-HXGW6, cd-V4D9H, cd-BA018)
// ---------------------------------------------------------------------------------------------

const INSTALL: &str = "https://hub.test";
const SECOND: &str = "https://second.test";
const THIRD: &str = "https://third.test";
const ACCOUNT: &str = "acc_00000000000000000000000001";
const OTHER_ACCOUNT: &str = "acc_00000000000000000000000009";
const HANDLE: &str = "test-person";
const CHALLENGE_ID: &str = "kch_00000000000000000000000002";
const KEY_ID: &str = "skey_00000000000000000000000003";
const FOUND_ID: &str = "skey_00000000000000000000000077";
const NONCE: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const FINGERPRINT: &str = "SHA256:RHFBqxmd4FLrdHDM2DGtlKdMxg/nN9xD3ZpkWHSvlaM";
const PUBLIC_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMxZFwIQHLofF5C/SY2UPr6zgqjzUfcX6+XICjAhsvur";
const PROOF: &str = "U1NIU0lHAAAAAQAAADMAAAALc3NoLWVkMjU1MTkAAAAgzFkXAhAcuh8XkL9JjZQ+vrOCqPNR9xfr5cgKMCGy+6sAAAAOcmR5LWtleWJpbmQtdjEAAAAAAAAABnNoYTUxMgAAAFMAAAALc3NoLWVkMjU1MTkAAABA12AaXZO7xGPGx60J5a1JLmiKfRLNj0h+d0zhRo6jsA9h9riGCaqukgi6feNhe0qfxybjswnpsk/FX3YA0v/nDQ==";
/// cd-VMRX2, every line ending LF.
const STATEMENT: &str = "rdy-statement: 1\n\
kind: keybind\n\
account: acc_00000000000000000000000001\n\
challenge: kch_00000000000000000000000002\n\
key: SHA256:RHFBqxmd4FLrdHDM2DGtlKdMxg/nN9xD3ZpkWHSvlaM\n";
const LABEL: &str = "laptop key";
/// The challenge fixture's issue time (2026-10-17T12:00:00Z) and its end ten minutes later.
const T_ISSUE: i64 = 1_792_238_400_000;
const EXPIRES_AT: &str = "2026-10-17T12:10:00Z";
const T_EXPIRES: i64 = 1_792_239_000_000;
const HOUR_MS: i64 = 3_600_000;
const COMPROMISED_INPUT: &str = "2026-10-17T13:00:00+02:00";
const COMPROMISED: &str = "2026-10-17T11:00:00Z";
const HUB_NOW: &str = "2026-10-18T09:00:00Z";
const FIND_TARGET: &str =
    "/api/v1/me/keys?fingerprint=SHA256%3ARHFBqxmd4FLrdHDM2DGtlKdMxg%2FnN9xD3ZpkWHSvlaM";
const REVOKE_REQUEST: &str = r#"{"compromised_from":"2026-10-17T11:00:00Z"}"#;
const LIST_REPLY: &str = r#"{"items":[{"id":"skey_00000000000000000000000003","account":"acc_00000000000000000000000001","fingerprint":"SHA256:RHFBqxmd4FLrdHDM2DGtlKdMxg/nN9xD3ZpkWHSvlaM","public_key":"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMxZFwIQHLofF5C/SY2UPr6zgqjzUfcX6+XICjAhsvur","label":"laptop key","kind":"person","state":"active","expires_at":null,"bound_at":"2026-10-17T12:05:00Z","retired_at":null,"revoked_at":null,"compromised_from":null}],"next":null}"#;
const RETIRE_REPLY: &str = r#"{"id":"skey_00000000000000000000000003","account":"acc_00000000000000000000000001","fingerprint":"SHA256:RHFBqxmd4FLrdHDM2DGtlKdMxg/nN9xD3ZpkWHSvlaM","public_key":"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMxZFwIQHLofF5C/SY2UPr6zgqjzUfcX6+XICjAhsvur","label":"laptop key","kind":"person","state":"retired","expires_at":null,"bound_at":"2026-10-17T12:05:00Z","retired_at":"2026-10-18T09:00:00Z","revoked_at":null,"compromised_from":null}"#;
const REVOKE_REPLY: &str = r#"{"id":"skey_00000000000000000000000003","account":"acc_00000000000000000000000001","fingerprint":"SHA256:RHFBqxmd4FLrdHDM2DGtlKdMxg/nN9xD3ZpkWHSvlaM","public_key":"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMxZFwIQHLofF5C/SY2UPr6zgqjzUfcX6+XICjAhsvur","label":"laptop key","kind":"person","state":"revoked","expires_at":null,"bound_at":"2026-10-17T12:05:00Z","retired_at":null,"revoked_at":"2026-10-18T09:00:00Z","compromised_from":"2026-10-17T11:00:00Z"}"#;
/// The revoke reply when the request named no time: the hub chose its request time.
const REVOKE_NOW_REPLY: &str = r#"{"id":"skey_00000000000000000000000003","account":"acc_00000000000000000000000001","fingerprint":"SHA256:RHFBqxmd4FLrdHDM2DGtlKdMxg/nN9xD3ZpkWHSvlaM","public_key":"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMxZFwIQHLofF5C/SY2UPr6zgqjzUfcX6+XICjAhsvur","label":"laptop key","kind":"person","state":"revoked","expires_at":null,"bound_at":"2026-10-17T12:05:00Z","retired_at":null,"revoked_at":"2026-10-18T09:00:00Z","compromised_from":"2026-10-18T09:00:00Z"}"#;

/// Tokens of the settled form (hsa4's vectors).
const ACCESS: &str = "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf";
const REFRESH: &str = "rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC";

const BAD_KEY_MESSAGE: &str = "key is not a public key or SHA256 fingerprint";
const BAD_KEY_FIX: &str = "pass a public key line or its SHA256 fingerprint";
const EXPIRED_MESSAGE: &str = "the key binding challenge expired";
const EXPIRED_FIX: &str = "start key binding again";

// Software keys of the test-only signer (fixed seeds; no private key is committed).
const PERSON_SEED: [u8; 32] = [7; 32];
const AGENT_SEED: [u8; 32] = [9; 32];
const ROGUE_SEED: [u8; 32] = [11; 32];

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

fn origin(s: &str) -> Origin {
    Origin::parse(s).unwrap()
}

fn dalek(seed: &[u8; 32]) -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(seed)
}

fn soft_blob(seed: &[u8; 32]) -> Vec<u8> {
    KeyData::Ed25519(Ed25519PublicKey(dalek(seed).verifying_key().to_bytes()))
        .encode_vec()
        .unwrap()
}

fn line_of(blob: &[u8]) -> String {
    format!("ssh-ed25519 {}", Base64::encode_string(blob))
}

fn soft_sign(seed: &[u8; 32], data: &[u8]) -> Vec<u8> {
    let raw = dalek(seed).sign(data).to_bytes();
    Signature::new(Algorithm::Ed25519, raw.to_vec())
        .unwrap()
        .encode_vec()
        .unwrap()
}

/// The fixture key's blob, from its public-key line.
fn fixture_blob() -> Vec<u8> {
    let b64 = PUBLIC_KEY.split(' ').nth(1).unwrap();
    Base64::decode_vec(b64).unwrap()
}

/// The agent's signature inside the fixture proof: what an agent holding the fixture key
/// answers for the fixture statement.
fn fixture_agent_signature() -> Vec<u8> {
    let proof = Base64::decode_vec(PROOF).unwrap();
    let sig = SshSig::decode(&mut proof.as_slice()).unwrap();
    sig.signature().encode_vec().unwrap()
}

/// The keybind statement for `fingerprint`, written out byte for byte.
fn keybind_bytes(account: &str, challenge: &str, fingerprint: &str) -> Vec<u8> {
    format!(
        "rdy-statement: 1\nkind: keybind\naccount: {account}\nchallenge: {challenge}\nkey: {fingerprint}\n"
    )
    .into_bytes()
}

/// What the agent is asked to sign for `statement`: SSHSIG's sha512 signed data.
fn signed_data(statement: &[u8]) -> Vec<u8> {
    SshSig::signed_data(NS_KEYBIND, HashAlg::Sha512, statement).unwrap()
}

// ---------------------------------------------------------------------------------------------
// The environment: a real project, a user config and its private credential sibling
// ---------------------------------------------------------------------------------------------

struct Env {
    _home: tempfile::TempDir,
    _project: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
    user_file: PathBuf,
    creds: PathBuf,
}

fn private_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

/// The user settings: `signing.key`, `signing.agent_key` and `signing.sign`, then `extra`.
fn signing_yaml(person: Option<&str>, agent: Option<&str>, sign: &str, extra: &str) -> String {
    let mut out = String::from("# person settings, kept as written\nsigning:\n");
    if let Some(k) = person {
        out.push_str(&format!("  key: \"{k}\"\n"));
    }
    if let Some(k) = agent {
        out.push_str(&format!("  agent_key: \"{k}\"\n"));
    }
    out.push_str(&format!("  sign: {sign}\n"));
    out.push_str(extra);
    out
}

/// A real project (`Rdy::init`) and `<home>/rdy/config.yaml` (0644) holding `config`.
fn env(config: &str) -> Env {
    let home_dir = private_dir();
    let project_dir = private_dir();
    let home = home_dir.path().to_path_buf();
    let rdy = home.join("rdy");
    fs::DirBuilder::new().mode(0o700).create(&rdy).unwrap();
    let user_file = rdy.join("config.yaml");
    fs::write(&user_file, config).unwrap();
    fs::set_permissions(&user_file, fs::Permissions::from_mode(0o644)).unwrap();
    let project = project_dir.path().to_path_buf();
    drop(Rdy::init(&project, "rdy-standard").unwrap());
    Env {
        _home: home_dir,
        _project: project_dir,
        creds: rdy.join("credentials.yaml"),
        home,
        project,
        user_file,
    }
}

/// The default environment: the fixture key as `signing.key`, a software key as
/// `signing.agent_key`, one share remote `origin` on [`INSTALL`] and a fresh sign-in there.
fn standard() -> Env {
    let e = env(&signing_yaml(
        Some(PUBLIC_KEY),
        Some(&blob_fingerprint(&soft_blob(&AGENT_SEED))),
        "auto",
        "",
    ));
    add_remote(&e, "origin", INSTALL, RemoteMode::Share);
    seed(&e, &[(INSTALL, ready(ACCOUNT, HANDLE))]);
    e
}

fn add_remote(e: &Env, name: &str, url: &str, mode: RemoteMode) {
    RemoteService::new(&e.project, &e.user_file)
        .add(name, url, mode, RemoteLevel::Project)
        .unwrap();
}

fn ready(account: &str, handle: &str) -> Credential {
    Credential::Ready(ReadyCredential {
        access_token: Secret::new(ACCESS),
        refresh_token: Secret::new(REFRESH),
        expires_at_ms: T_ISSUE + HOUR_MS,
        account: account.to_string(),
        handle: handle.to_string(),
    })
}

/// Writes the credential file (0600) holding exactly `records`.
fn seed(e: &Env, records: &[(&str, Credential)]) {
    seed_at(&e.creds, records);
}

fn seed_at(creds: &Path, records: &[(&str, Credential)]) {
    let mut file = CredentialFile::default();
    for (o, c) in records {
        file.set(origin(o), Some(c.clone()));
    }
    let bytes = encode_credentials(&file).unwrap();
    let tmp = creds.with_extension("seed");
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .unwrap();
    f.write_all(&bytes).unwrap();
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(&tmp, creds).unwrap();
}

fn handle_on_disk(e: &Env, install: &str) -> Option<String> {
    let bytes = fs::read(&e.creds).ok()?;
    match parse_credentials(&bytes).unwrap().get(&origin(install)) {
        Some(Credential::Ready(r)) => Some(r.handle.clone()),
        _ => None,
    }
}

/// Every file below `dir`, by relative path, with its bytes.
fn tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().to_path_buf();
                out.insert(rel, fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn signing_config(e: &Env) -> SigningConfig {
    let eff = effective(&e.project, Some(&e.user_file)).unwrap();
    SigningConfig::from_effective(&e.project, &eff).unwrap()
}

// ---------------------------------------------------------------------------------------------
// The fake transport and clock
// ---------------------------------------------------------------------------------------------

type Answer = Result<HubReply, HubFailure>;
type Handler = Box<dyn FnMut(&HubCall) -> Answer + Send>;

fn op_name(op: &HubOperation) -> &'static str {
    match op {
        HubOperation::Metadata => "Metadata",
        HubOperation::DeviceStart { .. } => "DeviceStart",
        HubOperation::DevicePoll { .. } => "DevicePoll",
        HubOperation::Refresh { .. } => "Refresh",
        HubOperation::Me => "Me",
        HubOperation::SignOut { .. } => "SignOut",
        HubOperation::BindChallenge { .. } => "BindChallenge",
        HubOperation::Bind { .. } => "Bind",
        HubOperation::FindKey { .. } => "FindKey",
        HubOperation::Retire { .. } => "Retire",
        HubOperation::Revoke { .. } => "Revoke",
    }
}

/// A transport that answers each request with the next queued handler, which must be for that
/// operation, and records every call it saw.
#[derive(Default)]
struct Fake {
    queue: Mutex<VecDeque<(&'static str, Handler)>>,
    seen: Mutex<Vec<HubCall>>,
}

impl Fake {
    fn on(&self, op: &'static str, f: impl FnMut(&HubCall) -> Answer + Send + 'static) -> &Fake {
        self.queue.lock().unwrap().push_back((op, Box::new(f)));
        self
    }

    fn reply(&self, op: &'static str, r: Answer) -> &Fake {
        self.on(op, move |_| r.clone())
    }

    fn calls(&self) -> Vec<HubCall> {
        self.seen.lock().unwrap().clone()
    }

    fn ops(&self) -> Vec<&'static str> {
        self.calls().iter().map(|c| op_name(&c.operation)).collect()
    }

    /// Every queued reply was used.
    fn done(&self) {
        let left: Vec<&str> = self.queue.lock().unwrap().iter().map(|(o, _)| *o).collect();
        assert!(left.is_empty(), "unused replies: {left:?}");
    }
}

impl HubTransport for Fake {
    fn exchange(&self, call: HubCall) -> Answer {
        let op = op_name(&call.operation);
        self.seen.lock().unwrap().push(call.clone());
        let next = self.queue.lock().unwrap().pop_front();
        let (want, mut handler) = next.unwrap_or_else(|| panic!("an unexpected {op} request"));
        assert_eq!(want, op, "the request order");
        handler(&call)
    }
}

/// A clock that moves only when a test moves it.
struct Clock(AtomicI64);

impl Clock {
    fn at(ms: i64) -> Clock {
        Clock(AtomicI64::new(ms))
    }

    fn set(&self, ms: i64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl HubClock for Clock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------------------------
// The test agents
// ---------------------------------------------------------------------------------------------

/// How the test agent answers a sign request for one key.
#[derive(Clone)]
enum How {
    /// The software signer with this seed (the key's own seed, or another one).
    Seed([u8; 32]),
    /// These signature bytes.
    Fixed(Vec<u8>),
}

type Hook = Box<dyn FnMut() + Send>;

/// An agent holding `keys`; every sign request is recorded, and `hook` runs inside it.
struct TestAgent {
    keys: Vec<(Vec<u8>, How)>,
    identities: Mutex<usize>,
    signs: Mutex<Vec<(Vec<u8>, Vec<u8>)>>,
    hook: Mutex<Option<Hook>>,
}

impl TestAgent {
    fn new(keys: Vec<(Vec<u8>, How)>) -> TestAgent {
        TestAgent {
            keys,
            identities: Mutex::new(0),
            signs: Mutex::new(Vec::new()),
            hook: Mutex::new(None),
        }
    }

    /// The fixture key (answering with the fixture proof's signature) and the person and agent
    /// software keys.
    fn standard() -> TestAgent {
        TestAgent::new(vec![
            (fixture_blob(), How::Fixed(fixture_agent_signature())),
            (soft_blob(&PERSON_SEED), How::Seed(PERSON_SEED)),
            (soft_blob(&AGENT_SEED), How::Seed(AGENT_SEED)),
        ])
    }

    fn with_hook(self, hook: impl FnMut() + Send + 'static) -> TestAgent {
        *self.hook.lock().unwrap() = Some(Box::new(hook));
        self
    }

    fn signs(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.signs.lock().unwrap().clone()
    }
}

impl SigningAgent for TestAgent {
    fn identities(&self) -> Result<Vec<AgentIdentity>, RdyError> {
        *self.identities.lock().unwrap() += 1;
        Ok(self
            .keys
            .iter()
            .map(|(k, _)| AgentIdentity {
                public_key: k.clone(),
            })
            .collect())
    }

    fn sign(&self, public_key: &[u8], payload: &[u8]) -> Result<Vec<u8>, RdyError> {
        self.signs
            .lock()
            .unwrap()
            .push((public_key.to_vec(), payload.to_vec()));
        if let Some(hook) = self.hook.lock().unwrap().as_mut() {
            hook();
        }
        let (_, how) = self
            .keys
            .iter()
            .find(|(k, _)| k == public_key)
            .expect("the test agent holds the key");
        Ok(match how {
            How::Seed(seed) => soft_sign(seed, payload),
            How::Fixed(bytes) => bytes.clone(),
        })
    }
}

/// An agent that fails on every invocation and counts them.
#[derive(Default)]
struct FailingAgent {
    calls: Mutex<usize>,
}

impl FailingAgent {
    fn calls(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

impl SigningAgent for FailingAgent {
    fn identities(&self) -> Result<Vec<AgentIdentity>, RdyError> {
        *self.calls.lock().unwrap() += 1;
        Err(RdyError::new(E_NO_AGENT, "the test agent fails", "none"))
    }

    fn sign(&self, _public_key: &[u8], _payload: &[u8]) -> Result<Vec<u8>, RdyError> {
        *self.calls.lock().unwrap() += 1;
        Err(RdyError::new(E_NO_AGENT, "the test agent fails", "none"))
    }
}

// ---------------------------------------------------------------------------------------------
// Sessions and replies
// ---------------------------------------------------------------------------------------------

fn human() -> Session {
    Session::human("human:nick".to_string(), Role::Writer)
}

fn agent_session() -> Session {
    Session::agent(
        "agent:claude".to_string(),
        Some("r7".to_string()),
        Role::Writer,
    )
    .unwrap()
}

fn svc<'a>(
    e: &Env,
    t: &'a dyn HubTransport,
    c: &'a Clock,
    a: &'a dyn SigningAgent,
) -> KeyService<'a> {
    KeyService::new(&e.project, &e.user_file, t, c, a)
}

fn challenge_of(kind: KeyKind, label: &str) -> KeyChallenge {
    KeyChallenge {
        id: CHALLENGE_ID.to_string(),
        account: ACCOUNT.to_string(),
        kind,
        label: label.to_string(),
        namespace: NS_KEYBIND.to_string(),
        nonce: NONCE.to_string(),
        expires_at: EXPIRES_AT.to_string(),
    }
}

fn challenged(c: KeyChallenge) -> Answer {
    Ok(HubReply::BindChallenge(c))
}

fn hub_key(fingerprint: &str, public_key: &str, kind: KeyKind, state: KeyState) -> HubKey {
    HubKey {
        id: KEY_ID.to_string(),
        account: ACCOUNT.to_string(),
        fingerprint: fingerprint.to_string(),
        public_key: public_key.to_string(),
        label: LABEL.to_string(),
        kind,
        state,
        expires_at: None,
        bound_at: None,
        retired_at: None,
        revoked_at: None,
        compromised_from: None,
    }
}

fn confirm_url(install: &str, id: &str) -> String {
    format!("{install}/app/settings/keys/{id}/confirm")
}

/// The pending key a Bind reply holds for the sent public key, as the hub returns it.
fn bound_reply(install: &str, call: &HubCall, kind: KeyKind) -> Answer {
    let HubOperation::Bind { public_key, .. } = &call.operation else {
        panic!("not a Bind");
    };
    let fp = key_fingerprint(public_key).unwrap();
    Ok(HubReply::Bind(BindResult {
        key: hub_key(&fp, public_key, kind, KeyState::Pending),
        confirm_url: confirm_url(install, KEY_ID),
    }))
}

fn found(fingerprint: &str, id: &str) -> Answer {
    let mut k = hub_key(fingerprint, PUBLIC_KEY, KeyKind::Person, KeyState::Active);
    k.id = id.to_string();
    k.bound_at = Some("2026-10-17T12:05:00Z".into());
    Ok(HubReply::FindKey(Some(k)))
}

fn retired(id: &str) -> Answer {
    let mut k = hub_key(FINGERPRINT, PUBLIC_KEY, KeyKind::Person, KeyState::Retired);
    k.id = id.to_string();
    k.retired_at = Some(HUB_NOW.into());
    Ok(HubReply::Retire(k))
}

fn revoked(id: &str, compromised_from: &str) -> Answer {
    let mut k = hub_key(FINGERPRINT, PUBLIC_KEY, KeyKind::Person, KeyState::Revoked);
    k.id = id.to_string();
    k.revoked_at = Some(HUB_NOW.into());
    k.compromised_from = Some(compromised_from.into());
    Ok(HubReply::Revoke(k))
}

fn lost() -> HubFailure {
    HubFailure::Transport {
        may_have_sent: true,
    }
}

fn unsent() -> HubFailure {
    HubFailure::Transport {
        may_have_sent: false,
    }
}

fn err<T: std::fmt::Debug>(r: Result<T, RdyError>) -> RdyError {
    match r {
        Ok(v) => panic!("expected an error, got {v:?}"),
        Err(e) => e,
    }
}

/// No error names the challenge, the statement or the proof.
fn no_echo(e: &RdyError) {
    for text in [&e.message, &e.fix] {
        for secret in [CHALLENGE_ID, "rdy-statement", NONCE, "U1NIU0lH"] {
            assert!(!text.contains(secret), "{} echoes {secret}: {text}", e.code);
        }
    }
}

fn find_destination(install: &str) -> String {
    format!("{install}{FIND_TARGET}")
}

// ---------------------------------------------------------------------------------------------
// Bind
// ---------------------------------------------------------------------------------------------

#[test]
fn hsa7_bind_uses_the_exact_keybind_bytes_and_selected_key() {
    // SIG's statement of the challenge fixture's account and id and the fixture key is exactly
    // cd-VMRX2, and the fixture proof verifies over it under rdy-keybind-v1.
    let bytes = statement(
        StatementSource::Keybind {
            account: ACCOUNT,
            challenge: CHALLENGE_ID,
        },
        None,
        FINGERPRINT,
    )
    .unwrap();
    assert_eq!(bytes, STATEMENT.as_bytes());
    assert_eq!(bytes, keybind_bytes(ACCOUNT, CHALLENGE_ID, FINGERPRINT));
    let proof = Base64::decode_vec(PROOF).unwrap();
    let check = verify_sshsig("rdy-keybind-v1", &bytes, &proof);
    assert!(check.valid, "the fixture proof verifies: {check:?}");
    assert_eq!(check.key.as_deref(), Some(FINGERPRINT));
    assert_eq!(key_fingerprint(PUBLIC_KEY).as_deref(), Ok(FINGERPRINT));

    // Person: signing.key (the fixture key) signs; the Bind request is the fixture request
    // byte for byte.
    let e = standard();
    let clock = Clock::at(T_ISSUE);
    let agent = TestAgent::standard();
    let t = Fake::default();
    t.reply(
        "BindChallenge",
        challenged(challenge_of(KeyKind::Person, LABEL)),
    )
    .on("Bind", |call| bound_reply(INSTALL, call, KeyKind::Person));
    let reply = svc(&e, &t, &clock, &agent)
        .bind(KeyKind::Person, Some(LABEL), None, &human())
        .unwrap();
    t.done();
    let calls = t.calls();
    match &calls[0].operation {
        HubOperation::BindChallenge { kind, label } => {
            assert_eq!(*kind, KeyKind::Person);
            assert_eq!(label.as_deref(), Some(LABEL));
        }
        op => panic!("first request {op:?}"),
    }
    match &calls[1].operation {
        HubOperation::Bind {
            challenge,
            public_key,
            proof,
        } => {
            assert_eq!(challenge, CHALLENGE_ID);
            assert_eq!(public_key, PUBLIC_KEY);
            assert_eq!(proof, PROOF, "the proof is the fixture proof exactly");
        }
        op => panic!("second request {op:?}"),
    }
    let signs = agent.signs();
    assert_eq!(signs.len(), 1, "the agent signs once");
    assert_eq!(signs[0].0, fixture_blob(), "the person key signs");
    assert_eq!(signs[0].1, signed_data(STATEMENT.as_bytes()));
    assert_eq!(
        reply,
        BindReply {
            key: FINGERPRINT.to_string(),
            kind: KeyKind::Person,
            state: KeyState::Pending,
            confirm_url: confirm_url(INSTALL, KEY_ID),
            remote: "origin".to_string(),
            warnings: vec![],
        }
    );

    // Agent: only signing.agent_key signs, over the statement naming its own fingerprint; no
    // label is sent when none is given, and the hub's default is required back.
    let agent_blob = soft_blob(&AGENT_SEED);
    let agent_fp = blob_fingerprint(&agent_blob);
    let want = keybind_bytes(ACCOUNT, CHALLENGE_ID, &agent_fp);
    let t = Fake::default();
    let checked = want.clone();
    let line = line_of(&agent_blob);
    t.reply(
        "BindChallenge",
        challenged(challenge_of(KeyKind::Agent, "agent key")),
    )
    .on("Bind", move |call| {
        let HubOperation::Bind {
            public_key, proof, ..
        } = &call.operation
        else {
            panic!("not a Bind");
        };
        assert_eq!(*public_key, line, "the agent key's line, no comment");
        assert!(!proof.contains('\n'));
        let blob = Base64::decode_vec(proof).unwrap();
        let check = verify_sshsig(NS_KEYBIND, &checked, &blob);
        assert!(check.valid);
        assert_eq!(check.key, key_fingerprint(public_key).ok());
        bound_reply(INSTALL, call, KeyKind::Agent)
    });
    let agent = TestAgent::standard();
    let reply = svc(&e, &t, &clock, &agent)
        .bind(KeyKind::Agent, None, None, &agent_session())
        .unwrap();
    t.done();
    match &t.calls()[0].operation {
        HubOperation::BindChallenge { kind, label } => {
            assert_eq!(*kind, KeyKind::Agent);
            assert_eq!(*label, None, "no label is sent when none is given");
        }
        op => panic!("first request {op:?}"),
    }
    let signs = agent.signs();
    assert_eq!(signs.len(), 1);
    assert_eq!(signs[0].0, agent_blob, "only the agent key signs");
    assert_eq!(signs[0].1, signed_data(&want));
    assert_eq!(reply.key, agent_fp);
    assert_eq!(reply.kind, KeyKind::Agent);

    // Each kind selects only its own setting.
    let only_agent = env(&signing_yaml(None, Some(&agent_fp), "auto", ""));
    add_remote(&only_agent, "origin", INSTALL, RemoteMode::Share);
    seed(&only_agent, &[(INSTALL, ready(ACCOUNT, HANDLE))]);
    let t = Fake::default();
    let agent = TestAgent::standard();
    let e1 = err(svc(&only_agent, &t, &clock, &agent).bind(KeyKind::Person, None, None, &human()));
    assert_eq!(
        (e1.code, e1.message.as_str(), e1.fix.as_str()),
        (
            E_SIGNING_KEY_MISSING,
            "signing.key is not set",
            "set signing.key in the user config or .rdy/local.yaml"
        )
    );
    let only_person = env(&signing_yaml(Some(PUBLIC_KEY), None, "auto", ""));
    add_remote(&only_person, "origin", INSTALL, RemoteMode::Share);
    seed(&only_person, &[(INSTALL, ready(ACCOUNT, HANDLE))]);
    let e2 = err(svc(&only_person, &t, &clock, &agent).bind(
        KeyKind::Agent,
        None,
        None,
        &agent_session(),
    ));
    assert_eq!(
        (e2.code, e2.message.as_str(), e2.fix.as_str()),
        (
            E_SIGNING_KEY_MISSING,
            "signing.agent_key is not set",
            "set signing.agent_key in the user config or .rdy/local.yaml"
        )
    );
    assert!(t.ops().is_empty(), "no challenge without a selected key");
    assert!(agent.signs().is_empty());
}

#[test]
fn hsa7_explicit_bind_works_when_automatic_signing_is_never() {
    let e = env(&signing_yaml(Some(PUBLIC_KEY), None, "never", ""));
    add_remote(&e, "origin", INSTALL, RemoteMode::Share);
    seed(&e, &[(INSTALL, ready(ACCOUNT, HANDLE))]);
    let before = fs::read(&e.user_file).unwrap();
    assert_eq!(signing_config(&e).sign, SignMode::Never);
    let clock = Clock::at(T_ISSUE);
    let agent = TestAgent::standard();
    let t = Fake::default();
    t.reply(
        "BindChallenge",
        challenged(challenge_of(KeyKind::Person, LABEL)),
    )
    .on("Bind", |call| bound_reply(INSTALL, call, KeyKind::Person));
    let reply = svc(&e, &t, &clock, &agent)
        .bind(KeyKind::Person, Some(LABEL), None, &human())
        .unwrap();
    t.done();
    assert_eq!(reply.state, KeyState::Pending);
    assert_eq!(agent.signs().len(), 1, "an explicit bind signs");
    assert_eq!(fs::read(&e.user_file).unwrap(), before);
    assert_eq!(
        signing_config(&e).sign,
        SignMode::Never,
        "the setting stays"
    );
}

#[test]
fn hsa7_account_key_namespace_and_proof_substitution_are_refused() {
    let e = standard();
    let person = soft_blob(&PERSON_SEED);
    let soft = env(&signing_yaml(Some(&line_of(&person)), None, "auto", ""));
    add_remote(&soft, "origin", INSTALL, RemoteMode::Share);
    seed(&soft, &[(INSTALL, ready(ACCOUNT, HANDLE))]);

    // Challenge substitution: refused before the agent is asked to sign.
    let base = || challenge_of(KeyKind::Person, LABEL);
    let cases: Vec<(&str, KeyChallenge)> = vec![
        (
            "another namespace",
            KeyChallenge {
                namespace: "rdy-approval-v1".into(),
                ..base()
            },
        ),
        (
            "another account",
            KeyChallenge {
                account: OTHER_ACCOUNT.into(),
                ..base()
            },
        ),
        (
            "another kind",
            KeyChallenge {
                kind: KeyKind::Agent,
                ..base()
            },
        ),
        (
            "another label",
            KeyChallenge {
                label: "other key".into(),
                ..base()
            },
        ),
        (
            "an empty id",
            KeyChallenge {
                id: String::new(),
                ..base()
            },
        ),
        (
            "an empty nonce",
            KeyChallenge {
                nonce: String::new(),
                ..base()
            },
        ),
        (
            "an expired challenge",
            KeyChallenge {
                expires_at: "2026-10-17T11:59:59Z".into(),
                ..base()
            },
        ),
        (
            "a challenge ending now",
            KeyChallenge {
                expires_at: "2026-10-17T12:00:00Z".into(),
                ..base()
            },
        ),
        (
            "a challenge past ten minutes",
            KeyChallenge {
                expires_at: "2026-10-17T12:10:01Z".into(),
                ..base()
            },
        ),
        (
            "a malformed time",
            KeyChallenge {
                expires_at: "tomorrow".into(),
                ..base()
            },
        ),
    ];
    for (what, c) in cases {
        let clock = Clock::at(T_ISSUE);
        let agent = TestAgent::standard();
        let t = Fake::default();
        t.reply("BindChallenge", challenged(c));
        let e1 =
            err(svc(&e, &t, &clock, &agent).bind(KeyKind::Person, Some(LABEL), None, &human()));
        assert_eq!(e1.code, E_UPSTREAM_PROTOCOL, "{what}");
        no_echo(&e1);
        assert!(agent.signs().is_empty(), "{what}: the agent signs nothing");
        assert_eq!(t.ops(), ["BindChallenge"], "{what}: zero Bind requests");
        t.done();
    }

    // Proof substitution: the agent's answer must verify for the selected key and statement.
    let proofs: Vec<(&str, How)> = vec![
        ("another key's signature", How::Seed(ROGUE_SEED)),
        (
            "a zero signature",
            How::Fixed(
                Signature::new(Algorithm::Ed25519, vec![0; 64])
                    .unwrap()
                    .encode_vec()
                    .unwrap(),
            ),
        ),
        (
            "a signature of another statement",
            How::Fixed(fixture_agent_signature()),
        ),
    ];
    for (what, how) in proofs {
        let clock = Clock::at(T_ISSUE);
        let agent = TestAgent::new(vec![(person.clone(), how)]);
        let t = Fake::default();
        t.reply("BindChallenge", challenged(base()));
        let e1 =
            err(svc(&soft, &t, &clock, &agent).bind(KeyKind::Person, Some(LABEL), None, &human()));
        assert_eq!(e1.code, E_UPSTREAM_KEY_PROOF, "{what}");
        assert_eq!(e1.message, "the agent's key proof is invalid");
        no_echo(&e1);
        assert_eq!(agent.signs().len(), 1, "{what}");
        assert_eq!(t.ops(), ["BindChallenge"], "{what}: zero Bind requests");
        t.done();
    }

    // The account changes while the agent signs: stop with E_UPSTREAM_AUTH.
    let clock = Clock::at(T_ISSUE);
    let creds = e.creds.clone();
    let agent = TestAgent::standard().with_hook(move || {
        seed_at(&creds, &[(INSTALL, ready(OTHER_ACCOUNT, HANDLE))]);
    });
    let t = Fake::default();
    t.reply("BindChallenge", challenged(base()));
    let e1 = err(svc(&e, &t, &clock, &agent).bind(KeyKind::Person, Some(LABEL), None, &human()));
    assert_eq!(e1.code, E_UPSTREAM_AUTH);
    assert_eq!(t.ops(), ["BindChallenge"], "zero Bind requests");
    seed(&e, &[(INSTALL, ready(ACCOUNT, HANDLE))]);

    // The challenge expires while the agent waits.
    let clock = std::sync::Arc::new(Clock::at(T_ISSUE));
    let moved = std::sync::Arc::clone(&clock);
    let agent = TestAgent::standard().with_hook(move || moved.set(T_EXPIRES));
    let t = Fake::default();
    t.reply("BindChallenge", challenged(base()));
    let e1 = err(svc(&e, &t, &clock, &agent).bind(KeyKind::Person, Some(LABEL), None, &human()));
    assert_eq!(
        (e1.code, e1.message.as_str(), e1.fix.as_str()),
        (E_BAD_INPUT, EXPIRED_MESSAGE, EXPIRED_FIX)
    );
    assert_eq!(t.ops(), ["BindChallenge"], "zero Bind requests");

    // A label outside the rules is refused before any request or agent call.
    for label in ["", " key", "key\u{7}", &"k".repeat(65)] {
        let clock = Clock::at(T_ISSUE);
        let agent = TestAgent::standard();
        let t = Fake::default();
        let e1 =
            err(svc(&e, &t, &clock, &agent).bind(KeyKind::Person, Some(label), None, &human()));
        assert_eq!(e1.code, E_BAD_INPUT, "label {label:?}");
        assert!(t.ops().is_empty());
        assert_eq!(*agent.identities.lock().unwrap(), 0);
    }
}

#[test]
fn hsa7_agent_wait_holds_no_config_or_writer_lock() {
    let e = standard();
    let clock = Clock::at(T_ISSUE);
    // `true`: the agent is waiting inside its sign request; `false`: bind returned.
    let (event_tx, event_rx) = mpsc::channel::<bool>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go_rx = Mutex::new(go_rx);
    let in_sign_tx = Mutex::new(event_tx.clone());
    let agent = TestAgent::standard().with_hook(move || {
        in_sign_tx.lock().unwrap().send(true).unwrap();
        go_rx.lock().unwrap().recv().unwrap();
    });
    let t = Fake::default();
    t.reply(
        "BindChallenge",
        challenged(challenge_of(KeyKind::Person, LABEL)),
    )
    .on("Bind", |call| bound_reply(INSTALL, call, KeyKind::Person));
    let other = Fake::default();
    let reply = std::thread::scope(|scope| {
        let (e2, t2, clock2, agent2) = (&e, &t, &clock, &agent);
        let bind = scope.spawn(move || {
            let r = svc(e2, t2, clock2, agent2).bind(KeyKind::Person, Some(LABEL), None, &human());
            event_tx.send(false).unwrap();
            r
        });
        assert!(
            event_rx.recv().unwrap(),
            "bind returned before it asked the agent: {:?}",
            bind.join().unwrap()
        );
        // The agent is waiting: every lock is free and independent work completes.
        for lock in [
            e.home.join("rdy/credentials.yaml.lock"),
            e.home.join("rdy/config.yaml.lock"),
        ] {
            if let Ok(f) = File::open(&lock) {
                match f.try_lock() {
                    Ok(()) => f.unlock().unwrap(),
                    Err(TryLockError::WouldBlock) => panic!("{} is held", lock.display()),
                    Err(TryLockError::Error(err)) => panic!("{err}"),
                }
            }
        }
        let installs = HubCredentials::new(Some(&e.project), &e.user_file, &other, &clock)
            .installs()
            .unwrap();
        assert_eq!(installs.len(), 1);
        drop(LockedConfig::open(&e.home, Path::new("rdy/config.yaml"), false, 1 << 20).unwrap());
        drop(LockedConfig::open(&e.project, Path::new(".rdy/local.yaml"), false, 1 << 20).unwrap());
        RemoteService::new(&e.project, &e.user_file).list().unwrap();
        drop(Rdy::open(&e.project).unwrap());
        go_tx.send(()).unwrap();
        bind.join().unwrap()
    })
    .unwrap();
    assert!(other.ops().is_empty());
    assert_eq!(reply.state, KeyState::Pending);
    t.done();
}

#[test]
fn hsa7_bind_returns_a_confirmation_link_without_confirming() {
    let e = standard();
    let clock = Clock::at(T_ISSUE);
    let agent = TestAgent::standard();
    let t = Fake::default();
    t.reply(
        "BindChallenge",
        challenged(challenge_of(KeyKind::Person, LABEL)),
    )
    .on("Bind", |call| bound_reply(INSTALL, call, KeyKind::Person));
    let reply = svc(&e, &t, &clock, &agent)
        .bind(KeyKind::Person, Some(LABEL), None, &human())
        .unwrap();
    t.done();
    assert_eq!(
        t.ops(),
        ["BindChallenge", "Bind"],
        "no poll or confirm request"
    );
    for call in t.calls() {
        assert!(
            !call.destination.contains("confirm"),
            "{}",
            call.destination
        );
        assert!(!call.destination.contains("/app/"), "{}", call.destination);
    }
    assert_eq!(reply.state, KeyState::Pending);
    assert_eq!(reply.confirm_url, confirm_url(INSTALL, KEY_ID));
    let json = serde_json::to_value(&reply).unwrap();
    let keys: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut want = vec!["confirm_url", "key", "kind", "remote", "state", "warnings"];
    want.sort();
    let mut got = keys.clone();
    got.sort();
    assert_eq!(got, want);
    assert_eq!(json["state"], "pending");
    assert_eq!(json["kind"], "person");
    assert_eq!(json["key"], FINGERPRINT);

    // A reply that is not the pending key just sent, with its own link, is a contract mismatch.
    type Tamper = fn(&mut BindResult);
    let tampers: Vec<(&str, Tamper)> = vec![
        ("a link on another origin", |b| {
            b.confirm_url = confirm_url(SECOND, KEY_ID);
        }),
        ("a link for another key", |b| {
            b.confirm_url = confirm_url(INSTALL, FOUND_ID);
        }),
        ("an active key", |b| b.key.state = KeyState::Active),
        ("another fingerprint", |b| {
            b.key.fingerprint = blob_fingerprint(&soft_blob(&ROGUE_SEED));
        }),
        ("another account", |b| b.key.account = OTHER_ACCOUNT.into()),
        ("another kind", |b| b.key.kind = KeyKind::Agent),
    ];
    for (what, tamper) in tampers {
        let agent = TestAgent::standard();
        let t = Fake::default();
        t.reply(
            "BindChallenge",
            challenged(challenge_of(KeyKind::Person, LABEL)),
        )
        .on("Bind", move |call| {
            let Ok(HubReply::Bind(mut b)) = bound_reply(INSTALL, call, KeyKind::Person) else {
                unreachable!()
            };
            tamper(&mut b);
            Ok(HubReply::Bind(b))
        });
        let e1 =
            err(svc(&e, &t, &clock, &agent).bind(KeyKind::Person, Some(LABEL), None, &human()));
        assert_eq!(e1.code, E_UPSTREAM_PROTOCOL, "{what}");
        assert_eq!(t.ops(), ["BindChallenge", "Bind"], "{what}");
    }

    // A lost Bind reply is uncertain; no fresh challenge is asked for.
    let agent = TestAgent::standard();
    let t = Fake::default();
    t.reply(
        "BindChallenge",
        challenged(challenge_of(KeyKind::Person, LABEL)),
    )
    .reply("Bind", Err(lost()));
    let e1 = err(svc(&e, &t, &clock, &agent).bind(KeyKind::Person, Some(LABEL), None, &human()));
    assert_eq!(e1.code, E_UPSTREAM_UNCERTAIN);
    assert_eq!(t.ops(), ["BindChallenge", "Bind"]);
    t.done();
}

#[test]
fn hsa7_key_commands_select_explicit_share_or_only_remote() {
    let e = standard();
    // Only `origin` (share, INSTALL) so far; replace it with one publish remote.
    RemoteService::new(&e.project, &e.user_file)
        .remove("origin", RemoteLevel::Project)
        .unwrap();
    add_remote(&e, "a", SECOND, RemoteMode::Publish);
    seed(
        &e,
        &[
            (INSTALL, ready(ACCOUNT, HANDLE)),
            (SECOND, ready(ACCOUNT, HANDLE)),
            (THIRD, ready(ACCOUNT, HANDLE)),
        ],
    );
    let clock = Clock::at(T_ISSUE);
    let agent = FailingAgent::default();
    let retire = |name: Option<&str>, install: &str| {
        let t = Fake::default();
        t.reply("FindKey", found(FINGERPRINT, KEY_ID))
            .reply("Retire", retired(KEY_ID));
        let r = svc(&e, &t, &clock, &agent).retire(FINGERPRINT, name, &human());
        for call in t.calls() {
            assert_eq!(call.install, origin(install));
            assert!(call.destination.starts_with(&format!("{install}/")));
        }
        r
    };

    // The only remote.
    assert_eq!(retire(None, SECOND).unwrap().remote, "a");
    // A share remote wins over the others, whatever their names.
    add_remote(&e, "b", THIRD, RemoteMode::Share);
    assert_eq!(retire(None, THIRD).unwrap().remote, "b");
    // An explicit name wins over the share remote.
    assert_eq!(retire(Some("a"), SECOND).unwrap().remote, "a");
    // Bind and revoke select the same way.
    let tagent = TestAgent::standard();
    let t = Fake::default();
    let mut c = challenge_of(KeyKind::Person, LABEL);
    c.label = "person key".into();
    t.reply("BindChallenge", challenged(c))
        .on("Bind", |call| bound_reply(THIRD, call, KeyKind::Person));
    let b = svc(&e, &t, &clock, &tagent)
        .bind(KeyKind::Person, None, None, &human())
        .unwrap();
    assert_eq!(b.remote, "b");
    assert_eq!(b.confirm_url, confirm_url(THIRD, KEY_ID));
    assert!(t.calls().iter().all(|c| c.install == origin(THIRD)));
    let t = Fake::default();
    t.reply("FindKey", found(FINGERPRINT, KEY_ID))
        .reply("Revoke", revoked(KEY_ID, COMPROMISED));
    let r = svc(&e, &t, &clock, &agent)
        .revoke(FINGERPRINT, None, Some("a"), &human())
        .unwrap();
    assert_eq!(r.remote, "a");
    assert!(t.calls().iter().all(|c| c.install == origin(SECOND)));

    // No share remote and two remotes: one must be named; nothing is sent.
    RemoteService::new(&e.project, &e.user_file)
        .set_mode("b", RemoteMode::Publish, RemoteLevel::Project)
        .unwrap();
    let t = Fake::default();
    let e1 = err(svc(&e, &t, &clock, &agent).retire(FINGERPRINT, None, &human()));
    assert_eq!(e1.code, E_REMOTE_REQUIRED);
    let e2 = err(svc(&e, &t, &clock, &agent).revoke(FINGERPRINT, None, None, &human()));
    assert_eq!(e2.code, E_REMOTE_REQUIRED);
    let e3 = err(svc(&e, &t, &clock, &tagent).bind(KeyKind::Person, None, None, &human()));
    assert_eq!(e3.code, E_REMOTE_REQUIRED);
    let e4 = err(svc(&e, &t, &clock, &agent).retire(FINGERPRINT, Some("zzz"), &human()));
    assert_eq!(e4.code, E_NOT_FOUND);
    assert!(t.ops().is_empty());
    assert_eq!(agent.calls(), 0);
}

/// A loopback hub serving the key routes, with its CA in the user config, one share remote on
/// it and a fresh sign-in there.
struct Tls {
    e: Env,
    server: Server,
    transport: HttpHubTransport,
}

fn tls() -> Tls {
    let server = Server::start();
    let o = server.origin().to_string();
    let e = env(&signing_yaml(Some(PUBLIC_KEY), None, "auto", ""));
    let ca = e.home.join("ca.pem");
    fs::write(&ca, server.ca_pem()).unwrap();
    let mut config = fs::read_to_string(&e.user_file).unwrap();
    config.push_str(&format!(
        "hub:\n  installs:\n    \"{o}\":\n      ca_file: \"{}\"\n",
        ca.display()
    ));
    fs::write(&e.user_file, config).unwrap();
    add_remote(&e, "origin", &o, RemoteMode::Share);
    seed(&e, &[(&o, ready(ACCOUNT, HANDLE))]);
    let fence = HubConfig::load(Some(&e.project), Some(&e.user_file))
        .unwrap()
        .fence(&origin(&o))
        .unwrap();
    let transport = HttpHubTransport::new(fence).unwrap();
    Tls {
        e,
        server,
        transport,
    }
}

#[test]
fn hsa7_revoke_preserves_time_and_omits_an_absent_time() {
    let mut h = tls();
    let clock = Clock::at(T_ISSUE);
    let agent = FailingAgent::default();

    // A given time, in any offset, reaches the hub as canonical UTC.
    h.server.reply(TlsReply::json(200, LIST_REPLY));
    h.server.reply(TlsReply::json(200, REVOKE_REPLY));
    h.server.accept_n(2);
    let r = svc(&h.e, &h.transport, &clock, &agent)
        .revoke(FINGERPRINT, Some(COMPROMISED_INPUT), None, &human())
        .unwrap();
    assert_eq!(
        r,
        RevokeReply {
            key: FINGERPRINT.to_string(),
            state: KeyState::Revoked,
            revoked_at: HUB_NOW.to_string(),
            compromised_from: COMPROMISED.to_string(),
            remote: "origin".to_string(),
            warnings: vec![],
        }
    );
    // An absent time is omitted: the hub chooses its request time, returned as it is.
    h.server.reply(TlsReply::json(200, LIST_REPLY));
    h.server.reply(TlsReply::json(200, REVOKE_NOW_REPLY));
    h.server.accept_n(2);
    let r = svc(&h.e, &h.transport, &clock, &agent)
        .revoke(PUBLIC_KEY, None, None, &human())
        .unwrap();
    assert_eq!(r.revoked_at, HUB_NOW);
    assert_eq!(r.compromised_from, HUB_NOW, "the hub's chosen time");
    let json = serde_json::to_value(&r).unwrap();
    let mut keys: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "compromised_from",
            "key",
            "remote",
            "revoked_at",
            "state",
            "warnings"
        ]
    );

    let reqs = h.server.requests();
    assert_eq!(reqs.len(), 4);
    for find in [&reqs[0], &reqs[2]] {
        assert_eq!(
            (find.method.as_str(), find.target.as_str()),
            ("GET", FIND_TARGET)
        );
        assert!(find.body.is_empty());
    }
    let revoke_target = format!("/api/v1/me/keys/{KEY_ID}/revoke");
    assert_eq!(reqs[1].method, "POST");
    assert_eq!(reqs[1].target, revoke_target);
    assert_eq!(reqs[1].body_text(), REVOKE_REQUEST);
    assert_eq!(reqs[3].method, "POST");
    assert_eq!(reqs[3].target, revoke_target);
    assert!(
        reqs[3].body.is_empty(),
        "no compromised_from member is sent"
    );
    assert_eq!(reqs[3].header("content-type"), None);
    for r in &reqs {
        assert_eq!(r.header("rdy-actor"), Some("human:test-person"));
        assert_eq!(r.header("rdy-run"), Some("cli"));
    }
    assert_eq!(h.server.finish(), 0);
    assert_eq!(agent.calls(), 0);
}

#[test]
fn hsa7_retire_and_revoke_do_not_use_an_agent() {
    let e = standard();
    let clock = Clock::at(T_ISSUE);
    let agent = FailingAgent::default();
    let t = Fake::default();
    t.reply("FindKey", found(FINGERPRINT, KEY_ID))
        .reply("Retire", retired(KEY_ID))
        .reply("FindKey", found(FINGERPRINT, KEY_ID))
        .reply("Revoke", revoked(KEY_ID, COMPROMISED));
    let r = svc(&e, &t, &clock, &agent)
        .retire(PUBLIC_KEY, None, &human())
        .unwrap();
    assert_eq!(
        r,
        KeyStateReply {
            key: FINGERPRINT.to_string(),
            state: KeyState::Retired,
            remote: "origin".to_string(),
            warnings: vec![],
        }
    );
    let r = svc(&e, &t, &clock, &agent)
        .revoke(FINGERPRINT, Some(COMPROMISED), None, &agent_session())
        .unwrap();
    assert_eq!(r.state, KeyState::Revoked);
    t.done();
    assert_eq!(agent.calls(), 0, "retire and revoke never contact an agent");
}

#[test]
fn hsa7_retire_and_revoke_find_the_hub_id_first() {
    let e = standard();
    let clock = Clock::at(T_ISSUE);
    let agent = FailingAgent::default();

    // FindKey by the normalized fingerprint, then one mutation of the hub id it returned.
    for revoke in [false, true] {
        let t = Fake::default();
        t.reply("FindKey", found(FINGERPRINT, FOUND_ID));
        if revoke {
            t.on("Revoke", |call| {
                let HubOperation::Revoke {
                    key_id,
                    compromised_from,
                } = &call.operation
                else {
                    unreachable!()
                };
                assert_eq!(key_id, FOUND_ID);
                assert_eq!(compromised_from.as_deref(), Some(COMPROMISED));
                revoked(FOUND_ID, COMPROMISED)
            });
        } else {
            t.on("Retire", |call| {
                let HubOperation::Retire { key_id } = &call.operation else {
                    unreachable!()
                };
                assert_eq!(key_id, FOUND_ID);
                retired(FOUND_ID)
            });
        }
        let s = svc(&e, &t, &clock, &agent);
        let key = format!("{PUBLIC_KEY} laptop comment");
        if revoke {
            s.revoke(&key, Some(COMPROMISED_INPUT), None, &human())
                .unwrap();
        } else {
            s.retire(&key, None, &human()).unwrap();
        }
        t.done();
        let calls = t.calls();
        assert_eq!(calls.len(), 2);
        match &calls[0].operation {
            HubOperation::FindKey { fingerprint } => assert_eq!(fingerprint, FINGERPRINT),
            op => panic!("first request {op:?}"),
        }
        assert_eq!(calls[0].destination, find_destination(INSTALL));
        let act = if revoke { "revoke" } else { "retire" };
        assert_eq!(
            calls[1].destination,
            format!("{INSTALL}/api/v1/me/keys/{FOUND_ID}/{act}")
        );
    }

    // An empty or foreign list is refused with zero mutations.
    let rogue = blob_fingerprint(&soft_blob(&ROGUE_SEED));
    for (what, reply, code) in [
        ("empty", Ok(HubReply::FindKey(None)), E_UPSTREAM_REFUSED),
        ("foreign", found(&rogue, KEY_ID), E_UPSTREAM_PROTOCOL),
        ("lost", Err(lost()), E_UPSTREAM_TRANSPORT),
        ("unsent", Err(unsent()), E_UPSTREAM_TRANSPORT),
    ] {
        for revoke in [false, true] {
            let t = Fake::default();
            t.reply("FindKey", reply.clone());
            let s = svc(&e, &t, &clock, &agent);
            let e1 = if revoke {
                err(s.revoke(FINGERPRINT, None, None, &human()))
            } else {
                err(s.retire(FINGERPRINT, None, &human()))
            };
            assert_eq!(e1.code, code, "{what}");
            assert_eq!(t.ops(), ["FindKey"], "{what}: zero mutations");
        }
    }

    // A lost mutation reply is uncertain and never resent.
    for revoke in [false, true] {
        let t = Fake::default();
        t.reply("FindKey", found(FINGERPRINT, KEY_ID));
        t.reply(if revoke { "Revoke" } else { "Retire" }, Err(lost()));
        let s = svc(&e, &t, &clock, &agent);
        let e1 = if revoke {
            err(s.revoke(FINGERPRINT, None, None, &human()))
        } else {
            err(s.retire(FINGERPRINT, None, &human()))
        };
        assert_eq!(e1.code, E_UPSTREAM_UNCERTAIN);
        assert_eq!(t.ops().len(), 2);
        t.done();
    }

    // A malformed key argument is refused before any request.
    for key in ["", "not a key", "SHA256:short", "ssh-ed25519 AAAA"] {
        let t = Fake::default();
        let e1 = err(svc(&e, &t, &clock, &agent).retire(key, None, &human()));
        assert_eq!(
            (e1.code, e1.message.as_str(), e1.fix.as_str()),
            (E_BAD_INPUT, BAD_KEY_MESSAGE, BAD_KEY_FIX),
            "{key:?}"
        );
        let e2 = err(svc(&e, &t, &clock, &agent).revoke(key, None, None, &human()));
        assert_eq!(e2.code, E_BAD_INPUT);
        assert!(t.ops().is_empty());
    }
    let t = Fake::default();
    let e1 =
        err(svc(&e, &t, &clock, &agent).revoke(FINGERPRINT, Some("yesterday"), None, &human()));
    assert_eq!(e1.code, E_BAD_INPUT);
    assert!(t.ops().is_empty());

    // On the wire: the form-encoded query, and an empty, foreign, paged or two-item list is
    // refused with exactly one request.
    let mut h = tls();
    let item = |fp: &str| {
        let list: serde_json::Value = serde_json::from_str(LIST_REPLY).unwrap();
        let mut k = list["items"][0].clone();
        k["fingerprint"] = fp.into();
        k
    };
    let lists = [
        (
            r#"{"items":[],"next":null}"#.to_string(),
            E_UPSTREAM_REFUSED,
        ),
        (r#"{"items":[]}"#.to_string(), E_UPSTREAM_REFUSED),
        (
            serde_json::json!({"items": [item(&rogue)], "next": null}).to_string(),
            E_UPSTREAM_PROTOCOL,
        ),
        (
            serde_json::json!({"items": [item(FINGERPRINT)], "next": "c2"}).to_string(),
            E_UPSTREAM_PROTOCOL,
        ),
        (
            serde_json::json!({"items": [item(FINGERPRINT), item(FINGERPRINT)], "next": null})
                .to_string(),
            E_UPSTREAM_PROTOCOL,
        ),
    ];
    let mut n = 0;
    for (body, code) in &lists {
        for revoke in [false, true] {
            h.server.reply(TlsReply::json(200, body));
            h.server.accept_n(1);
            let s = svc(&h.e, &h.transport, &clock, &agent);
            let e1 = if revoke {
                err(s.revoke(FINGERPRINT, None, None, &human()))
            } else {
                err(s.retire(FINGERPRINT, None, &human()))
            };
            assert_eq!(e1.code, *code, "{body}");
            n += 1;
            let reqs = h.server.requests();
            assert_eq!(reqs.len(), n, "{body}: zero mutations");
            assert_eq!(reqs[n - 1].method, "GET");
            assert_eq!(reqs[n - 1].target, FIND_TARGET);
        }
    }
    // The settled pair: FindKey, then one Retire of the listed id.
    h.server.reply(TlsReply::json(200, LIST_REPLY));
    h.server.reply(TlsReply::json(200, RETIRE_REPLY));
    h.server.accept_n(2);
    let r = svc(&h.e, &h.transport, &clock, &agent)
        .retire(FINGERPRINT, None, &human())
        .unwrap();
    assert_eq!(r.state, KeyState::Retired);
    let reqs = h.server.requests();
    assert_eq!(reqs[n].target, FIND_TARGET);
    assert_eq!(reqs[n + 1].method, "POST");
    assert_eq!(
        reqs[n + 1].target,
        format!("/api/v1/me/keys/{KEY_ID}/retire")
    );
    assert!(reqs[n + 1].body.is_empty());
    assert_eq!(h.server.finish(), 0);
    assert_eq!(agent.calls(), 0);
}

#[test]
fn hsa7_actor_claim_refresh_does_not_replay_the_mutation() {
    let e = standard();
    let clock = Clock::at(T_ISSUE);
    let none = FailingAgent::default();
    let me = |handle: &str| {
        Ok(HubReply::Me(MeReply {
            account: ACCOUNT.into(),
            handle: handle.into(),
        }))
    };

    // Retire refused for its claim: `/me` once, the new handle saved and reported, no resend.
    let t = Fake::default();
    t.reply("FindKey", found(FINGERPRINT, KEY_ID))
        .reply("Retire", Err(HubFailure::ActorClaim))
        .reply("Me", me("renamed"));
    let e1 = err(svc(&e, &t, &clock, &none).retire(FINGERPRINT, None, &human()));
    t.done();
    assert_eq!(e1.code, E_UPSTREAM_ACTOR_CLAIM);
    assert_eq!(e1.message, "the hub refused the actor claim");
    assert_eq!(e1.issues.len(), 1);
    assert_eq!(e1.issues[0].code, W_UPSTREAM_HANDLE_CHANGED);
    assert_eq!(
        e1.issues[0].message,
        "the hub handle changed from test-person to renamed"
    );
    assert_eq!(t.ops(), ["FindKey", "Retire", "Me"]);
    let calls = t.calls();
    for c in &calls[..2] {
        let claim = c.claim.as_ref().unwrap();
        assert_eq!(
            claim.actor, "human:test-person",
            "the cached install binding"
        );
        assert_eq!(claim.run.as_deref(), Some("cli"));
    }
    assert!(calls[2].claim.is_none(), "/me carries no claim");
    assert_eq!(handle_on_disk(&e, INSTALL).as_deref(), Some("renamed"));

    // Bind refused for its claim: no second Bind and no new challenge.
    let agent = TestAgent::standard();
    let t = Fake::default();
    t.reply(
        "BindChallenge",
        challenged(challenge_of(KeyKind::Person, LABEL)),
    )
    .reply("Bind", Err(HubFailure::ActorClaim))
    .reply("Me", me("renamed"));
    let e1 = err(svc(&e, &t, &clock, &agent).bind(KeyKind::Person, Some(LABEL), None, &human()));
    t.done();
    assert_eq!(e1.code, E_UPSTREAM_ACTOR_CLAIM);
    assert!(e1.issues.is_empty(), "the handle did not change again");
    assert_eq!(t.ops(), ["BindChallenge", "Bind", "Me"]);
    assert_eq!(agent.signs().len(), 1);
    for c in &t.calls()[..2] {
        assert_eq!(c.claim.as_ref().unwrap().actor, "human:renamed");
    }

    // `/me` itself fails: that error, with no retry of anything.
    let t = Fake::default();
    t.reply("FindKey", found(FINGERPRINT, KEY_ID))
        .reply("Revoke", Err(HubFailure::ActorClaim))
        .reply("Me", Err(unsent()));
    let e1 = err(svc(&e, &t, &clock, &none).revoke(FINGERPRINT, None, None, &human()));
    t.done();
    assert_eq!(e1.code, E_UPSTREAM_TRANSPORT);
    assert_eq!(t.ops(), ["FindKey", "Revoke", "Me"]);

    // An agent's claim stays byte for byte, before and after the refusal.
    let t = Fake::default();
    t.reply("FindKey", found(FINGERPRINT, KEY_ID))
        .reply("Retire", Err(HubFailure::ActorClaim))
        .reply("Me", me("renamed"));
    let e1 = err(svc(&e, &t, &clock, &none).retire(FINGERPRINT, None, &agent_session()));
    t.done();
    assert_eq!(e1.code, E_UPSTREAM_ACTOR_CLAIM);
    for c in &t.calls()[..2] {
        let claim = c.claim.as_ref().unwrap();
        assert_eq!(claim.actor, "agent:claude");
        assert_eq!(claim.run.as_deref(), Some("r7"));
    }

    // An agent session claiming a human actor is refused before any request or agent call.
    let forged = Session {
        actor: "human:nick".into(),
        run: Some("r7".into()),
        role: Role::Writer,
        kind: SessionKind::Agent,
    };
    let t = Fake::default();
    let agent = TestAgent::standard();
    let s = svc(&e, &t, &clock, &agent);
    assert_eq!(
        err(s.retire(FINGERPRINT, None, &forged)).code,
        E_ACTOR_UNBOUND
    );
    assert_eq!(
        err(s.revoke(FINGERPRINT, None, None, &forged)).code,
        E_ACTOR_UNBOUND
    );
    assert_eq!(
        err(s.bind(KeyKind::Person, None, None, &forged)).code,
        E_ACTOR_UNBOUND
    );
    assert!(t.ops().is_empty());
    assert_eq!(*agent.identities.lock().unwrap(), 0);
}

#[test]
fn hsa7_key_commands_leave_log_and_signing_config_unchanged() {
    let e = standard();
    let clock = Clock::at(T_ISSUE);
    let project_before = tree(&e.project);
    let config_before = fs::read(&e.user_file).unwrap();
    let signing_before = signing_config(&e);
    let agent = TestAgent::standard();
    let none = FailingAgent::default();

    // Bind: success, then a refusal.
    let t = Fake::default();
    t.reply(
        "BindChallenge",
        challenged(challenge_of(KeyKind::Person, LABEL)),
    )
    .on("Bind", |call| bound_reply(INSTALL, call, KeyKind::Person));
    svc(&e, &t, &clock, &agent)
        .bind(KeyKind::Person, Some(LABEL), None, &human())
        .unwrap();
    let t = Fake::default();
    let mut c = challenge_of(KeyKind::Person, LABEL);
    c.namespace = "rdy-tag-v1".into();
    t.reply("BindChallenge", challenged(c));
    err(svc(&e, &t, &clock, &agent).bind(KeyKind::Person, Some(LABEL), None, &human()));

    // Retire: success, then a refusal.
    let t = Fake::default();
    t.reply("FindKey", found(FINGERPRINT, KEY_ID))
        .reply("Retire", retired(KEY_ID));
    svc(&e, &t, &clock, &none)
        .retire(FINGERPRINT, None, &human())
        .unwrap();
    let t = Fake::default();
    t.reply("FindKey", Ok(HubReply::FindKey(None)));
    err(svc(&e, &t, &clock, &none).retire(FINGERPRINT, None, &human()));

    // Revoke: success, then a refusal.
    let t = Fake::default();
    t.reply("FindKey", found(FINGERPRINT, KEY_ID))
        .reply("Revoke", revoked(KEY_ID, COMPROMISED));
    svc(&e, &t, &clock, &none)
        .revoke(FINGERPRINT, Some(COMPROMISED), None, &human())
        .unwrap();
    let t = Fake::default();
    t.reply("FindKey", found(FINGERPRINT, KEY_ID))
        .reply("Revoke", Err(lost()));
    err(svc(&e, &t, &clock, &none).revoke(FINGERPRINT, None, None, &human()));

    assert_eq!(
        tree(&e.project),
        project_before,
        "the project files and log"
    );
    assert_eq!(fs::read(&e.user_file).unwrap(), config_before);
    assert_eq!(signing_config(&e), signing_before);
}
