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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
/// public-key line (`<type> <base64> [comment]`, the comment ignored) whose blob is one whole
/// public key or certificate of a type rdy can read, that type being the line's. `Err` for
/// anything else, an unknown key type or cut key material included.
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
/// - An empty selector is `E_SIGNING_KEY_MISSING`; one that is neither a public-key line (see
///   [`key_fingerprint`]) nor a canonical fingerprint is `E_BAD_INPUT`.
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
/// Valid only for one of the four rdy [`NAMESPACES`] (any other is refused before any
/// cryptographic check), SSHSIG v1, sha512, an accepted key type whose signature algorithm is
/// its own, the namespace asked for, and a signature that verifies (for sk keys: over the
/// application, the message, the flags and the counter). Reserved bytes are ignored, as OpenSSH
/// ignores them. The embedded key and its type are reported whenever the key is a whole public
/// key of a known type, valid or not, even when a later field is damaged.
pub fn verify_sshsig(namespace: &str, statement: &[u8], blob: &[u8]) -> CryptoCheck {
    if !NAMESPACES.contains(&namespace) {
        let mut out = CryptoCheck::default();
        if let Some(parts) = sshsig_parts(blob) {
            parts.report_key(&mut out);
        }
        return out;
    }
    verify_generic(namespace, statement, blob)
}

/// [`verify_sshsig`] for any namespace: the generic SSHSIG check, which the public verifier
/// calls after its namespace check. Only this module's tests call it directly, with the
/// interoperability vectors of other namespaces.
pub(crate) fn verify_generic(namespace: &str, statement: &[u8], blob: &[u8]) -> CryptoCheck {
    let mut out = CryptoCheck::default();
    let Some(parts) = sshsig_parts(blob) else {
        return out;
    };
    parts.report_key(&mut out);
    let key_type = parts.key_type;
    let Some(tail) = parts.tail else {
        return out;
    };
    if statement.len() > STATEMENT_MAX || tail.hash_alg != SHA512 || !KEY_TYPES.contains(&key_type)
    {
        return out;
    }
    let (Some(key_data), Some(signature), Ok(sig_namespace)) = (
        decode_key(parts.public_key),
        decode_signature(tail.signature, key_type),
        std::str::from_utf8(tail.namespace),
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
    // The blob must be a whole public key or certificate of a known type, and that type the
    // line's: an unsupported key type is reported only for a key rdy could read.
    if public_key_type(&blob) != Some(key_type) {
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
///
/// A held blob of a type rdy knows must be a whole key or certificate of that type; one whose
/// type rdy does not know is reported by its name, which is all rdy can read of it.
fn key_info(public_key: Vec<u8>) -> Result<KeyInfo, RdyError> {
    let fingerprint = blob_fingerprint(&public_key);
    let name = blob_key_type(&public_key).ok_or_else(invalid_reply)?;
    let key_type = match public_key_type(&public_key) {
        Some(ty) => ty,
        None if is_known_type(name) => return Err(invalid_reply()),
        None => name,
    }
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

/// The fields of an SSHSIG v1 blob, every length checked: its public key, read on its own, and
/// the fields after it when they are whole.
struct SshsigParts<'a> {
    public_key: &'a [u8],
    /// The key's type; the key is a whole public key of a known type.
    key_type: &'a str,
    /// `None` when a later field is damaged or bytes trail the last one.
    tail: Option<SshsigTail<'a>>,
}

struct SshsigTail<'a> {
    namespace: &'a [u8],
    hash_alg: &'a [u8],
    signature: &'a [u8],
}

impl SshsigParts<'_> {
    /// Puts the key's fingerprint and type in `out`.
    fn report_key(&self, out: &mut CryptoCheck) {
        out.key = Some(blob_fingerprint(self.public_key));
        out.key_type = Some(self.key_type.to_string());
    }
}

/// The parts of `blob`; `None` when it is over the bound, is not SSHSIG v1, or its public key
/// is not a whole key of a known type. The key is read before, and independently of, the fields
/// after it.
fn sshsig_parts(blob: &[u8]) -> Option<SshsigParts<'_>> {
    if blob.len() > SSHSIG_MAX {
        return None;
    }
    let mut r = Bounded::new(blob);
    if r.bytes(MAGIC.len())? != MAGIC || r.u32()? != 1 {
        return None;
    }
    let public_key = r.string(PUBLIC_KEY_MAX)?;
    let key_type = public_key_type(public_key)?;
    Some(SshsigParts {
        public_key,
        key_type,
        tail: sshsig_tail(r),
    })
}

fn sshsig_tail(mut r: Bounded<'_>) -> Option<SshsigTail<'_>> {
    let namespace = r.string(NAMESPACE_MAX)?;
    let _reserved = r.string(SSHSIG_MAX)?;
    let hash_alg = r.string(NAME_MAX)?;
    let signature = r.string(SSHSIG_MAX)?;
    if namespace.is_empty() || !r.is_empty() {
        return None;
    }
    Some(SshsigTail {
        namespace,
        hash_alg,
        signature,
    })
}

// ---- the structure of public keys and certificates ----

/// The plain public-key types rdy can read, accepted or not.
const PLAIN_TYPES: [&str; 8] = [
    KEY_TYPE_ED25519,
    KEY_TYPE_SK_ED25519,
    "ssh-rsa",
    "ssh-dss",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "sk-ecdsa-sha2-nistp256@openssh.com",
];

/// The OpenSSH certificate types rdy can read, each with the plain type of its key.
const CERT_TYPES: [(&str, &str); 8] = [
    ("ssh-ed25519-cert-v01@openssh.com", KEY_TYPE_ED25519),
    ("sk-ssh-ed25519-cert-v01@openssh.com", KEY_TYPE_SK_ED25519),
    ("ssh-rsa-cert-v01@openssh.com", "ssh-rsa"),
    ("ssh-dss-cert-v01@openssh.com", "ssh-dss"),
    (
        "ecdsa-sha2-nistp256-cert-v01@openssh.com",
        "ecdsa-sha2-nistp256",
    ),
    (
        "ecdsa-sha2-nistp384-cert-v01@openssh.com",
        "ecdsa-sha2-nistp384",
    ),
    (
        "ecdsa-sha2-nistp521-cert-v01@openssh.com",
        "ecdsa-sha2-nistp521",
    ),
    (
        "sk-ecdsa-sha2-nistp256-cert-v01@openssh.com",
        "sk-ecdsa-sha2-nistp256@openssh.com",
    ),
];

/// Whether `name` is a key or certificate type rdy can read.
fn is_known_type(name: &str) -> bool {
    PLAIN_TYPES.contains(&name) || CERT_TYPES.iter().any(|(cert, _)| *cert == name)
}

/// The type of `blob` when it is exactly one whole public key or certificate of a type rdy can
/// read: every field present and of its type's shape, every length within the bytes that
/// remain, nothing after the last field. `None` for anything else, an unknown type included.
/// This checks the encoding only: it proves no curve point valid and no certificate signature.
fn public_key_type(blob: &[u8]) -> Option<&str> {
    if blob.len() > PUBLIC_KEY_MAX {
        return None;
    }
    let name = blob_key_type(blob)?;
    let mut r = Bounded::new(blob);
    r.string(NAME_MAX)?;
    if let Some((_, plain)) = CERT_TYPES.iter().find(|(cert, _)| *cert == name) {
        certificate_fields(plain, &mut r)?;
    } else {
        plain_key_fields(name, &mut r)?;
    }
    r.is_empty().then_some(name)
}

/// The key fields (after the type name) of a plain key of type `name`.
fn plain_key_fields(name: &str, r: &mut Bounded<'_>) -> Option<()> {
    match name {
        KEY_TYPE_ED25519 => ed25519_point(r),
        KEY_TYPE_SK_ED25519 => {
            ed25519_point(r)?;
            application(r)
        }
        "ssh-rsa" => {
            mpint(r)?;
            mpint(r)
        }
        "ssh-dss" => (0..4).try_for_each(|_| mpint(r)),
        "ecdsa-sha2-nistp256" => ec_point(r, b"nistp256", 65),
        "ecdsa-sha2-nistp384" => ec_point(r, b"nistp384", 97),
        "ecdsa-sha2-nistp521" => ec_point(r, b"nistp521", 133),
        "sk-ecdsa-sha2-nistp256@openssh.com" => {
            ec_point(r, b"nistp256", 65)?;
            application(r)
        }
        _ => None,
    }
}

/// The fields (after the type name) of an OpenSSH certificate whose key is of type `plain`.
fn certificate_fields(plain: &str, r: &mut Bounded<'_>) -> Option<()> {
    r.string(PUBLIC_KEY_MAX)?; // nonce
    plain_key_fields(plain, r)?;
    r.bytes(8)?; // serial
    if !matches!(r.u32()?, 1 | 2) {
        return None; // user or host
    }
    r.string(PUBLIC_KEY_MAX)?; // key id
    let mut principals = Bounded::new(r.string(PUBLIC_KEY_MAX)?);
    while !principals.is_empty() {
        principals.string(PUBLIC_KEY_MAX)?;
    }
    r.bytes(8 + 8)?; // valid after, valid before
    for _ in 0..2 {
        // critical options, then extensions: name and data pairs
        let mut options = Bounded::new(r.string(PUBLIC_KEY_MAX)?);
        while !options.is_empty() {
            options.string(PUBLIC_KEY_MAX)?;
            options.string(PUBLIC_KEY_MAX)?;
        }
    }
    r.string(PUBLIC_KEY_MAX)?; // reserved
    // the signing CA's key: a whole plain key
    let ca = r.string(PUBLIC_KEY_MAX)?;
    let ca_type = blob_key_type(ca)?;
    let mut ca_r = Bounded::new(ca);
    ca_r.string(NAME_MAX)?;
    plain_key_fields(ca_type, &mut ca_r)?;
    if !ca_r.is_empty() {
        return None;
    }
    signature_fields(ca_type, r.string(PUBLIC_KEY_MAX)?)
}

/// Whether `sig` is a whole SSH signature blob of a key of type `key_type`, by its shape.
fn signature_fields(key_type: &str, sig: &[u8]) -> Option<()> {
    let mut r = Bounded::new(sig);
    let name = r.string(NAME_MAX)?;
    let name_ok = match key_type {
        "ssh-rsa" => matches!(name, b"ssh-rsa" | b"rsa-sha2-256" | b"rsa-sha2-512"),
        _ => name == key_type.as_bytes(),
    };
    if !name_ok {
        return None;
    }
    let raw = r.string(PUBLIC_KEY_MAX)?;
    let raw_ok = match key_type {
        KEY_TYPE_ED25519 | KEY_TYPE_SK_ED25519 => raw.len() == ED25519_SIG_LEN,
        "ssh-dss" => raw.len() == 40,
        "ssh-rsa" => !raw.is_empty(),
        _ => {
            // ECDSA: the two integers r and s
            let mut rs = Bounded::new(raw);
            mpint(&mut rs).and_then(|()| mpint(&mut rs)).is_some() && rs.is_empty()
        }
    };
    if !raw_ok {
        return None;
    }
    if key_type.starts_with("sk-") {
        r.bytes(1 + 4)?; // flags, counter
    }
    r.is_empty().then_some(())
}

/// A 32-byte Ed25519 public key.
fn ed25519_point(r: &mut Bounded<'_>) -> Option<()> {
    (r.string(ED25519_KEY_LEN)?.len() == ED25519_KEY_LEN).then_some(())
}

/// The UTF-8 application of an sk key.
fn application(r: &mut Bounded<'_>) -> Option<()> {
    std::str::from_utf8(r.string(PUBLIC_KEY_MAX)?)
        .ok()
        .map(|_| ())
}

/// An ECDSA key's curve (it must be `curve`) and its uncompressed point of `len` bytes.
fn ec_point(r: &mut Bounded<'_>, curve: &[u8], len: usize) -> Option<()> {
    if r.string(NAME_MAX)? != curve {
        return None;
    }
    let q = r.string(len)?;
    (q.len() == len && q.first() == Some(&0x04)).then_some(())
}

/// A positive SSH multiple-precision integer: not empty, sign bit clear, not zero.
fn mpint(r: &mut Bounded<'_>) -> Option<()> {
    let n = r.string(PUBLIC_KEY_MAX)?;
    let first = *n.first()?;
    (first & 0x80 == 0 && n.iter().any(|&b| b != 0)).then_some(())
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

#[cfg(test)]
mod tests {
    //! The generic SSHSIG verifier, which the public verifier calls after its namespace check,
    //! against the independent OpenSSH interoperability vectors in their own namespace
    //! `example`. Provenance and the OpenSSH 10.3p1 cross-check are in
    //! `tests/signing_crypto.rs`; the checksums are pinned here again.

    use super::{CryptoCheck, verify_generic, verify_sshsig};
    use ssh_key::SshSig;

    const ED_SIG: &str = include_str!("../tests/fixtures/sshsig/sshsig_ed25519");
    const SK_SIG: &str = include_str!("../tests/fixtures/sshsig/sshsig_sk_ed25519");
    const RESERVED_SIG: &str = include_str!("../tests/fixtures/sshsig/rdy_ed25519_reserved.sig");
    const ED_FP: &str = "SHA256:UCUiLr7Pjs9wFFJMDByLgc3NrtdU344OgUM45wZPcIQ";
    const SK_FP: &str = "SHA256:6gcI7q24Uf/4jZEyRdzDIZ2x5pW2AxPAQt/lgHoHae0";

    fn sha256_hex(bytes: &[u8]) -> String {
        ssh_key::HashAlg::Sha256
            .digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn bin(pem: &str) -> Vec<u8> {
        let mut out = Vec::new();
        ssh_encoding::Encode::encode(&SshSig::from_pem(pem).unwrap(), &mut out).unwrap();
        out
    }

    fn check(valid: bool, key: &str, ty: &str, presence: bool) -> CryptoCheck {
        CryptoCheck {
            valid,
            key: Some(key.to_string()),
            key_type: Some(ty.to_string()),
            presence,
        }
    }

    #[test]
    fn sig3_generic_verifier_accepts_the_openssh_example_vectors() {
        for (pem, digest, fp, ty, presence) in [
            (
                ED_SIG,
                "3a93dffb17b883d281b6b22750bd2106cb458544c1b70973c502f82f49991f80",
                ED_FP,
                "ssh-ed25519",
                false,
            ),
            (
                SK_SIG,
                "c71d66aaea3f24c7e3bc29c85c63c603a25df982cc2ad4f15dac205cb725872b",
                SK_FP,
                "sk-ssh-ed25519@openssh.com",
                true,
            ),
            // the OpenSSH ed25519 vector with reserved bytes `x` put in (OpenSSH says Good)
            (
                RESERVED_SIG,
                "6e7cf2e6515b51cf920cff97bc45386a909ac5d03936f79a1ecbf6fc39f53d57",
                ED_FP,
                "ssh-ed25519",
                false,
            ),
        ] {
            assert_eq!(sha256_hex(pem.as_bytes()), digest, "pinned vector");
            let b = bin(pem);
            assert_eq!(
                verify_generic("example", b"testing", &b),
                check(true, fp, ty, presence),
                "{ty}"
            );
            // the generic verifier still binds message and namespace
            assert_eq!(
                verify_generic("example", b"testinG", &b),
                check(false, fp, ty, false)
            );
            assert_eq!(
                verify_generic("examplf", b"testing", &b),
                check(false, fp, ty, false)
            );
            // the public verifier refuses `example`: it is not an rdy namespace
            assert_eq!(
                verify_sshsig("example", b"testing", &b),
                check(false, fp, ty, false)
            );
        }
    }
}
