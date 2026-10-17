//! SSH signatures of rdy acts: SSHSIG v1 with sha512, made through the caller's SSH agent and
//! checked offline.
//!
//! Only `ssh-ed25519` and `sk-ssh-ed25519@openssh.com` keys sign. Product code never reads,
//! writes or creates a private key: [`sign_sshsig`] asks a [`SigningAgent`], which the calling
//! process supplies (the daemon's `CallerAgent` talks to that process's own `SSH_AUTH_SOCK`).
//!
//! A key's identity is the SHA256 fingerprint of its exact public-key blob, spelled as OpenSSH
//! spells it: `SHA256:` and 43 unpadded base64 characters. A public-key line and its fingerprint
//! select the same key.
//!
//! Every length read from a blob or a reply is checked against the bytes that remain before
//! anything is allocated or handed to the SSH codec.

use crate::{
    E_BAD_INPUT, NoAgentCause, RdyError, no_agent, signing_key_missing, signing_key_type,
    signing_limit, signing_selector,
};
use ssh_encoding::base64::{Base64, Base64Unpadded, Encoding};
use ssh_encoding::{Decode, Encode};
use ssh_key::public::KeyData;
use ssh_key::{Algorithm, HashAlg, PublicKey, Signature, SshSig};

/// The SSHSIG namespace of a person's approval.
pub const NS_APPROVAL: &str = "rdy-approval-v1";
/// The SSHSIG namespace of an agent's relay.
pub const NS_RELAY: &str = "rdy-relay-v1";
/// The SSHSIG namespace of a tag.
pub const NS_TAG: &str = "rdy-tag-v1";
/// The SSHSIG namespace of a key binding proof.
pub const NS_KEYBIND: &str = "rdy-keybind-v1";
/// The namespaces rdy signs under, exactly.
pub const NAMESPACES: [&str; 4] = [NS_APPROVAL, NS_RELAY, NS_TAG, NS_KEYBIND];

/// The key type of a plain Ed25519 key.
pub const KEY_TYPE_ED25519: &str = "ssh-ed25519";
/// The key type of a FIDO (security key) Ed25519 key.
pub const KEY_TYPE_SK_ED25519: &str = "sk-ssh-ed25519@openssh.com";
/// The key types that sign, exactly.
pub const KEY_TYPES: [&str; 2] = [KEY_TYPE_ED25519, KEY_TYPE_SK_ED25519];

/// The largest statement, in bytes.
pub const STATEMENT_MAX: usize = 16384;
/// The longest namespace, in bytes.
pub const NAMESPACE_MAX: usize = 64;
/// The largest SSHSIG blob, in bytes.
pub const SSHSIG_MAX: usize = 16384;
/// The largest agent reply packet, in bytes.
pub const AGENT_PACKET_MAX: usize = 262144;
/// The most identities one agent answer may list.
pub const IDENTITIES_MAX: usize = 1024;
/// The largest public-key blob, in bytes.
pub const PUBLIC_KEY_MAX: usize = 16384;
/// The longest identity comment, in bytes.
pub const COMMENT_MAX: usize = 4096;

/// The flag bit of an sk signature that says the authenticator saw the user present.
pub const SK_USER_PRESENT: u8 = 0x01;

/// The SSHSIG preamble.
const MAGIC: &[u8] = b"SSHSIG";
/// The hash algorithm name rdy signs with.
const SHA512: &[u8] = b"sha512";
/// The length of an Ed25519 public key and of its signature.
const ED25519_KEY_LEN: usize = 32;
const ED25519_SIG_LEN: usize = 64;
/// The longest key type or hash name rdy reads.
const NAME_MAX: usize = 64;
/// The prefix of a SHA256 fingerprint.
const FP_PREFIX: &str = "SHA256:";
/// The unpadded base64 length of 32 bytes.
const FP_B64_LEN: usize = 43;

/// One identity an SSH agent holds: its public-key blob. Comments are never kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentIdentity {
    pub public_key: Vec<u8>,
}

/// The caller's SSH agent: the only way rdy signs. An implementation holds no private material.
pub trait SigningAgent: Send + Sync {
    /// The public-key blobs the agent holds.
    fn identities(&self) -> Result<Vec<AgentIdentity>, RdyError>;
    /// The agent's SSH signature blob (algorithm and signature, as the agent's sign response
    /// carries it) of `payload` by the key `public_key`.
    fn sign(&self, public_key: &[u8], payload: &[u8]) -> Result<Vec<u8>, RdyError>;
}

/// The agent of a caller that has none: every request is `E_NO_AGENT`
/// `SSH_AUTH_SOCK is not set`.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableAgent;

impl SigningAgent for UnavailableAgent {
    fn identities(&self) -> Result<Vec<AgentIdentity>, RdyError> {
        Err(no_agent(NoAgentCause::Unset))
    }

    fn sign(&self, _public_key: &[u8], _payload: &[u8]) -> Result<Vec<u8>, RdyError> {
        Err(no_agent(NoAgentCause::Unset))
    }
}

/// A signing key the caller's agent holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInfo {
    /// The canonical public-key blob.
    pub public_key: Vec<u8>,
    /// Its SHA256 fingerprint.
    pub fingerprint: String,
    /// Its key type: one of [`KEY_TYPES`].
    pub key_type: String,
}

/// The cryptographic check of one SSHSIG blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CryptoCheck {
    /// The blob is SSHSIG v1, sha512, of an accepted key type, in the namespace asked for, and
    /// its signature verifies.
    pub valid: bool,
    /// The embedded key's fingerprint; `None` when the key cannot be read.
    pub key: Option<String>,
    /// The embedded key's type; `None` when it cannot be read.
    pub key_type: Option<String>,
    /// The authenticated user-presence bit of a valid sk signature; false for every other
    /// signature, and always false for plain Ed25519.
    pub presence: bool,
}

/// The fingerprint of the public-key blob `blob`: `SHA256:` and the unpadded base64 of the
/// SHA256 of its bytes.
pub fn blob_fingerprint(blob: &[u8]) -> String {
    let digest = HashAlg::Sha256.digest(blob);
    format!("{FP_PREFIX}{}", Base64Unpadded::encode_string(&digest))
}

/// The canonical SHA256 fingerprint `selector` names: a fingerprint in canonical spelling, or a
/// public-key line (`<type> <base64> [comment]`, the comment ignored). `Err` for anything else.
/// Needs no agent and reads no file. The error carries nothing: each caller reports its own
/// setting's `E_BAD_INPUT` ([`crate::signing_selector`]).
#[allow(clippy::result_unit_err)]
pub fn key_fingerprint(selector: &str) -> Result<String, ()> {
    match parse_selector(selector.trim()).ok_or(())? {
        Selector::Fingerprint(fp) => Ok(fp),
        Selector::Public { blob, .. } => Ok(blob_fingerprint(&blob)),
    }
}

/// The key the selector `selector` of setting `setting` names, as the caller's agent holds it.
///
/// - An empty selector is `E_SIGNING_KEY_MISSING`; one that is neither a public-key line nor a
///   canonical fingerprint is `E_BAD_INPUT`.
/// - A public-key line of an unsupported type is `E_SIGNING_KEY_TYPE`, held or not, and asks the
///   agent nothing.
/// - A key the agent does not hold is `E_NO_AGENT` `key is not held`; a held key of an
///   unsupported type is `E_SIGNING_KEY_TYPE`, before any sign request.
pub fn resolve_key(
    setting: &str,
    selector: &str,
    agent: &dyn SigningAgent,
) -> Result<KeyInfo, RdyError> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Err(signing_key_missing(setting));
    }
    let wanted = match parse_selector(selector).ok_or_else(|| signing_selector(setting))? {
        Selector::Fingerprint(fp) => fp,
        Selector::Public { blob, key_type } => {
            let fp = blob_fingerprint(&blob);
            if !KEY_TYPES.contains(&key_type.as_str()) {
                return Err(signing_key_type(&fp, &key_type));
            }
            fp
        }
    };
    let held = agent
        .identities()?
        .into_iter()
        .find(|id| blob_fingerprint(&id.public_key) == wanted)
        .ok_or_else(|| no_agent(NoAgentCause::NotHeld))?;
    key_info(held.public_key)
}

/// The SSHSIG v1 blob (binary, never PEM) of `statement` in `namespace`, signed by the agent with
/// `key`: the agent signs the sha512 SSHSIG signed data, and its reply is wrapped with the key,
/// the namespace, sha512 and empty reserved bytes.
///
/// The namespace must be one of [`NAMESPACES`] and the statement at most [`STATEMENT_MAX`]
/// bytes; the key is checked again from its blob. The reply's encoding, algorithm and bounds are
/// checked (a malformed reply is `E_NO_AGENT` `invalid reply`), but not its signature: the
/// caller checks each new signature once before it is stored.
pub fn sign_sshsig(
    agent: &dyn SigningAgent,
    key: &KeyInfo,
    namespace: &str,
    statement: &[u8],
) -> Result<Vec<u8>, RdyError> {
    if namespace.len() > NAMESPACE_MAX {
        return Err(signing_limit(&format!("{NAMESPACE_MAX} bytes")));
    }
    if !NAMESPACES.contains(&namespace) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("{namespace:?} is not an rdy signing namespace"),
            format!("sign under one of {}", NAMESPACES.join(", ")),
        ));
    }
    if statement.len() > STATEMENT_MAX {
        return Err(signing_limit(&format!("{STATEMENT_MAX} bytes")));
    }
    // Whatever type the key info claims, its blob decides.
    let checked = key_info(key.public_key.clone())?;
    let key_data = decode_key(&checked.public_key).ok_or_else(invalid_reply)?;
    let signed_data =
        SshSig::signed_data(namespace, HashAlg::Sha512, statement).map_err(|_| invalid_reply())?;
    let reply = agent.sign(&checked.public_key, &signed_data)?;
    let signature = decode_signature(&reply, &checked.key_type).ok_or_else(invalid_reply)?;
    let sig = SshSig::new(key_data, namespace, HashAlg::Sha512, signature)
        .map_err(|_| invalid_reply())?;
    let out = encode(&sig).ok_or_else(invalid_reply)?;
    if out.len() > SSHSIG_MAX {
        return Err(invalid_reply());
    }
    Ok(out)
}

/// The cryptographic check of the SSHSIG blob `blob` over `statement` in `namespace`.
///
/// Valid only for SSHSIG v1, sha512, an accepted key type whose signature algorithm is its own,
/// the namespace asked for, and a signature that verifies (for sk keys: over the application,
/// the message, the flags and the counter). Reserved bytes are ignored, as OpenSSH ignores them.
/// The embedded key and its type are reported whenever they can be read, valid or not.
pub fn verify_sshsig(namespace: &str, statement: &[u8], blob: &[u8]) -> CryptoCheck {
    let mut out = CryptoCheck {
        valid: false,
        key: None,
        key_type: None,
        presence: false,
    };
    if blob.len() > SSHSIG_MAX {
        return out;
    }
    let Some(parts) = sshsig_parts(blob) else {
        return out;
    };
    out.key = Some(blob_fingerprint(parts.public_key));
    out.key_type = blob_key_type(parts.public_key).map(str::to_string);
    let Some(key_type) = out.key_type.clone() else {
        return out;
    };
    if statement.len() > STATEMENT_MAX
        || parts.hash_alg != SHA512
        || !KEY_TYPES.contains(&key_type.as_str())
    {
        return out;
    }
    let (Some(key_data), Some(signature), Ok(sig_namespace)) = (
        decode_key(parts.public_key),
        decode_signature(parts.signature, &key_type),
        std::str::from_utf8(parts.namespace),
    ) else {
        return out;
    };
    // Verify the SSHSIG with empty reserved bytes: OpenSSH signs and checks the signed data with
    // an empty reserved field, whatever the blob carries. The stored blob is never changed.
    let Ok(normalized) = SshSig::new(
        key_data.clone(),
        sig_namespace,
        HashAlg::Sha512,
        signature.clone(),
    ) else {
        return out;
    };
    if PublicKey::from(key_data)
        .verify(namespace, statement, &normalized)
        .is_err()
    {
        return out;
    }
    out.valid = true;
    // The flags are read only now that the signature over them verified.
    out.presence = key_type == KEY_TYPE_SK_ED25519
        && signature
            .as_bytes()
            .get(ED25519_SIG_LEN)
            .is_some_and(|flags| flags & SK_USER_PRESENT != 0);
    out
}

// ---- selectors and keys ----

enum Selector {
    Fingerprint(String),
    Public { blob: Vec<u8>, key_type: String },
}

/// The selector `s` (already trimmed); `None` when it is malformed.
fn parse_selector(s: &str) -> Option<Selector> {
    if s.starts_with(FP_PREFIX) {
        return canonical_fingerprint(s).then(|| Selector::Fingerprint(s.to_string()));
    }
    let mut fields = s.split_ascii_whitespace();
    let key_type = fields.next()?;
    let b64 = fields.next()?;
    // at most the padded base64 of the largest blob
    if b64.len() > PUBLIC_KEY_MAX.div_ceil(3).checked_mul(4)? {
        return None;
    }
    let blob = Base64::decode_vec(b64).ok()?;
    if blob.len() > PUBLIC_KEY_MAX || blob_key_type(&blob) != Some(key_type) {
        return None;
    }
    // A supported key's blob must be a whole, canonical key of its type.
    if KEY_TYPES.contains(&key_type) && decode_key(&blob).is_none() {
        return None;
    }
    Some(Selector::Public {
        blob,
        key_type: key_type.to_string(),
    })
}

/// Whether `s` is `SHA256:` and 43 unpadded base64 characters of 32 bytes, with the two unused
/// trailing bits zero.
fn canonical_fingerprint(s: &str) -> bool {
    let Some(b64) = s.strip_prefix(FP_PREFIX) else {
        return false;
    };
    let value = |c: u8| -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let bytes = b64.as_bytes();
    bytes.len() == FP_B64_LEN
        && bytes.iter().all(|&c| value(c).is_some())
        && bytes
            .last()
            .and_then(|&c| value(c))
            .is_some_and(|v| v & 0b11 == 0)
}

/// The key type name at the start of a public-key blob, when it is a short printable ASCII name.
fn blob_key_type(blob: &[u8]) -> Option<&str> {
    let name = Bounded::new(blob).string(NAME_MAX)?;
    if name.is_empty() || !name.iter().all(|c| c.is_ascii_graphic()) {
        return None;
    }
    std::str::from_utf8(name).ok()
}

/// The key info of a held blob: `E_SIGNING_KEY_TYPE` for an unsupported type, `E_NO_AGENT`
/// `invalid reply` for a blob that is not a whole, canonical key.
fn key_info(public_key: Vec<u8>) -> Result<KeyInfo, RdyError> {
    let fingerprint = blob_fingerprint(&public_key);
    let key_type = blob_key_type(&public_key)
        .ok_or_else(invalid_reply)?
        .to_string();
    if !KEY_TYPES.contains(&key_type.as_str()) {
        return Err(signing_key_type(&fingerprint, &key_type));
    }
    if decode_key(&public_key).is_none() {
        return Err(invalid_reply());
    }
    Ok(KeyInfo {
        public_key,
        fingerprint,
        key_type,
    })
}

/// The key of an accepted type in `blob`, when `blob` is exactly its canonical encoding. Every
/// length is checked against the bytes that remain before the SSH codec sees the blob.
fn decode_key(blob: &[u8]) -> Option<KeyData> {
    if blob.len() > PUBLIC_KEY_MAX {
        return None;
    }
    let mut r = Bounded::new(blob);
    let name = r.string(NAME_MAX)?;
    let key = r.string(ED25519_KEY_LEN)?;
    if key.len() != ED25519_KEY_LEN {
        return None;
    }
    let algorithm = match name {
        b"ssh-ed25519" => Algorithm::Ed25519,
        b"sk-ssh-ed25519@openssh.com" => {
            let application = r.string(PUBLIC_KEY_MAX)?;
            std::str::from_utf8(application).ok()?;
            Algorithm::SkEd25519
        }
        _ => return None,
    };
    if !r.is_empty() {
        return None;
    }
    let mut slice = blob;
    let data = KeyData::decode(&mut slice).ok()?;
    if !slice.is_empty() || data.algorithm() != algorithm || encode(&data)? != blob {
        return None;
    }
    Some(data)
}

/// The signature in an SSH signature blob of the key type `key_type`, when the blob is exactly
/// its canonical encoding and its algorithm is the key's own.
fn decode_signature(blob: &[u8], key_type: &str) -> Option<Signature> {
    if blob.len() > SSHSIG_MAX {
        return None;
    }
    let expected = match key_type {
        KEY_TYPE_ED25519 => Algorithm::Ed25519,
        KEY_TYPE_SK_ED25519 => Algorithm::SkEd25519,
        _ => return None,
    };
    let mut r = Bounded::new(blob);
    if r.string(NAME_MAX)? != key_type.as_bytes() {
        return None;
    }
    if r.string(ED25519_SIG_LEN)?.len() != ED25519_SIG_LEN {
        return None;
    }
    if expected == Algorithm::SkEd25519 {
        r.bytes(1 + 4)?; // flags, counter
    }
    if !r.is_empty() {
        return None;
    }
    let mut slice = blob;
    let signature = Signature::decode(&mut slice).ok()?;
    if !slice.is_empty() || signature.algorithm() != expected || encode(&signature)? != blob {
        return None;
    }
    Some(signature)
}

/// The fields of a whole SSHSIG v1 blob, every length checked.
struct SshsigParts<'a> {
    public_key: &'a [u8],
    namespace: &'a [u8],
    hash_alg: &'a [u8],
    signature: &'a [u8],
}

fn sshsig_parts(blob: &[u8]) -> Option<SshsigParts<'_>> {
    let mut r = Bounded::new(blob);
    if r.bytes(MAGIC.len())? != MAGIC || r.u32()? != 1 {
        return None;
    }
    let public_key = r.string(PUBLIC_KEY_MAX)?;
    let namespace = r.string(NAMESPACE_MAX)?;
    let _reserved = r.string(SSHSIG_MAX)?;
    let hash_alg = r.string(NAME_MAX)?;
    let signature = r.string(SSHSIG_MAX)?;
    if namespace.is_empty() || !r.is_empty() {
        return None;
    }
    Some(SshsigParts {
        public_key,
        namespace,
        hash_alg,
        signature,
    })
}

fn encode<T: Encode>(value: &T) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    value.encode(&mut out).ok()?;
    Some(out)
}

fn invalid_reply() -> RdyError {
    no_agent(NoAgentCause::InvalidReply)
}

/// A reader over SSH wire data that never reads past its end: each length is checked against
/// the bytes that remain, and nothing is allocated.
struct Bounded<'a> {
    rest: &'a [u8],
}

impl<'a> Bounded<'a> {
    fn new(bytes: &'a [u8]) -> Bounded<'a> {
        Bounded { rest: bytes }
    }

    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        if n > self.rest.len() {
            return None;
        }
        let (head, tail) = self.rest.split_at(n);
        self.rest = tail;
        Some(head)
    }

    fn u32(&mut self) -> Option<u32> {
        let b = self.bytes(4)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// A length-prefixed string of at most `max` bytes.
    fn string(&mut self, max: usize) -> Option<&'a [u8]> {
        let n = usize::try_from(self.u32()?).ok()?;
        if n > max {
            return None;
        }
        self.bytes(n)
    }

    fn is_empty(&self) -> bool {
        self.rest.is_empty()
    }
}
