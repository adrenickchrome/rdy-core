//! Version-specific release trust: one release key for each preview version.
//!
//! The historical root key signed v0.17.0 through v0.35.0. It is pinned here, and no input can
//! replace it. A later machine key enters only through a transition statement that the root
//! signed. A revocation statement, also signed by the root, ends a machine epoch at its last
//! good tag. [`ReleaseKeys::load`] reads a `release-keys/` directory, checks every record
//! against the root before it parses it, and builds the epochs in numeric order.
//!
//! - [`ReleaseKeys::signer_for`] selects the one key of a version, for archival verification.
//! - [`ReleaseKeys::eligible_signer`] also refuses a revoked key and the retired root, for every
//!   new signing.
//! - [`ReleaseKeys::verify_release`] refuses a signature by any key other than the selected one,
//!   before anything is extracted.
//!
//! The directory holds JSON/signature pairs, `records.txt`, and optionally `versions.txt`,
//! `README.txt` and `root-allowed-signers`. Core also keeps `policy.json` and
//! `evidence-policy.json` there, under their own closed schemas. Any other entry refuses. Every
//! bound refuses the whole set; nothing is truncated or skipped.
use crate::release_install::{self, GuideCommand};
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::Value;
use sha2::{Digest, Sha256};
use ssh_key::{Algorithm, HashAlg, PublicKey, SshSig};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The repository every statement and policy names.
pub const REPOSITORY: &str = "adrenickchrome/rdy-core";
/// The principal of every release-domain allowed-signers line.
pub const PRINCIPAL: &str = release_install::RELEASE_PRINCIPAL;
/// The SSHSIG namespace of a release manifest.
pub const MANIFEST_NAMESPACE: &str = release_install::RELEASE_NAMESPACE;
/// The SSHSIG namespace, and the `scheme`, of a machine transition.
pub const TRANSITION_NAMESPACE: &str = "rdy-release-signer-transition-v1";
/// The SSHSIG namespace, and the `scheme`, of a revocation.
pub const REVOCATION_NAMESPACE: &str = "rdy-release-signer-revocation-v1";
/// The superseded statement namespace: never accepted.
pub const SUPERSEDED_NAMESPACE: &str = "rdy-release-key-v1";

/// The historical root key's type.
pub const ROOT_KEY_TYPE: &str = "sk-ssh-ed25519@openssh.com";
/// The historical root key's base64 blob.
pub const ROOT_KEY_BASE64: &str = "AAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2guY29tAAAAIDiiQ0vO3dvG2AtbJYDwsshJl75kvbbnRDOCUdsjfrDRAAAAD3NzaDpyZHktcmVsZWFzZQ==";
/// The historical root key's fingerprint.
pub const ROOT_FINGERPRINT: &str = "SHA256:Baa52ZPJSzazbmd2KDEkzUM6OYTnl8mznJXsmnwvvJc";
/// The pinned historical allowed-signers file, byte for byte.
pub const ROOT_ALLOWED_SIGNERS: &str = "rdy-release namespaces=\"rdy-release-v1\" sk-ssh-ed25519@openssh.com AAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2guY29tAAAAIDiiQ0vO3dvG2AtbJYDwsshJl75kvbbnRDOCUdsjfrDRAAAAD3NzaDpyZHktcmVsZWFzZQ==\n";
/// The first minor the root signed.
pub const HISTORICAL_FIRST: u32 = 17;
/// The last minor the root signed. The first machine epoch starts after it.
pub const HISTORICAL_LAST: u32 = 35;
/// The machine key type.
pub const MACHINE_KEY_TYPE: &str = "ssh-ed25519";
/// The approved release target set at this base. It never follows a wider target table.
pub const RELEASE_TARGETS: [&str; 2] = ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"];

/// The largest transition or revocation JSON, inclusive.
pub const STATEMENT_MAX: u64 = 4096;
/// The largest root-statement SSHSIG, inclusive.
pub const STATEMENT_SIG_MAX: u64 = 4096;
/// The most JSON/signature pairs.
pub const PAIRS_MAX: usize = 64;
/// The largest root allowed-signers file.
pub const ROOT_FILE_MAX: u64 = 4096;
/// The largest `records.txt`, `versions.txt` and `README.txt`.
pub const INDEX_MAX: u64 = 65536;
/// The largest `policy.json` and `evidence-policy.json`.
pub const POLICY_MAX: u64 = 65536;
/// The largest release manifest: the adapter's limit.
pub const MANIFEST_MAX: u64 = release_install::MANIFEST_MAX;
/// The largest release SSHSIG.
pub const RELEASE_SIG_MAX: u64 = 4096;
/// The most directory entries: every pair and the six support and control files.
pub const ENTRIES_MAX: usize = PAIRS_MAX * 2 + 6;
/// The longest string in a policy.
pub const POLICY_STRING_MAX: usize = 256;

/// The generated index of the records.
pub const RECORDS_FILE: &str = "records.txt";
/// The compiled version table.
pub const VERSIONS_FILE: &str = "versions.txt";
/// The registry's free-text note.
pub const README_FILE: &str = "README.txt";
/// The copy of the pinned root allowed-signers file.
pub const ROOT_FILE: &str = "root-allowed-signers";
/// Core's release signer policy.
pub const POLICY_FILE: &str = "policy.json";
/// Core's attestation policy, bound by digest in `policy.json`.
pub const EVIDENCE_POLICY_FILE: &str = "evidence-policy.json";

/// The fields of a transition, exactly.
pub const TRANSITION_FIELDS: [&str; 5] = [
    "scheme",
    "repository",
    "first_machine_tag",
    "public_key",
    "fingerprint",
];
/// The fields of a revocation, exactly.
pub const REVOCATION_FIELDS: [&str; 5] = [
    "scheme",
    "repository",
    "first_machine_tag",
    "fingerprint",
    "last_good_tag",
];
/// The fields of `policy.json`, exactly.
pub const POLICY_FIELDS: [&str; 10] = [
    "schema",
    "repository",
    "key_arn",
    "role_arn",
    "fingerprint",
    "first_machine_tag",
    "targets",
    "records_sha256",
    "evidence_policy_sha256",
    "signer_workflow_sha",
];
/// The top-level fields of `evidence-policy.json`, exactly. The release controller checks the
/// nested records.
pub const EVIDENCE_POLICY_FIELDS: [&str; 6] = [
    "schema",
    "repository_id",
    "coordinators",
    "evidence_workflow",
    "release_workflow_path",
    "approval_channel",
];
/// The repository id `evidence-policy.json` pins.
pub const REPOSITORY_ID: u64 = 1_403_751_388;
/// The release workflow path `evidence-policy.json` pins.
pub const RELEASE_WORKFLOW_PATH: &str = ".github/workflows/release.yml";

/// The base64 length of a 51-byte Ed25519 SSH key blob.
const MACHINE_KEY_B64_LEN: usize = 68;
/// The length of a 51-byte Ed25519 SSH key blob.
const MACHINE_KEY_BLOB_LEN: usize = 51;
/// The flag of an sk signature made with the user present.
const SK_USER_PRESENT: u8 = 0x01;
/// The length of an Ed25519 signature, before an sk signature's flags.
const ED25519_SIG_LEN: usize = 64;
/// The longest field name a refusal repeats.
const ECHO_MAX: usize = 64;

/// Why the trust input or a request is refused. [`fmt::Display`] gives the exact refusal line,
/// without its LF. No refusal repeats an input body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustError {
    /// An entry or the directory cannot be read.
    Read(String),
    /// An entry is not a recognized name.
    UnknownEntry(String),
    /// An entry is not a regular file.
    NotRegular(String),
    /// A JSON record or signature has no partner.
    MissingPartner(String),
    /// A file is over its bound, in bytes.
    TooLarge(String, u64),
    /// More than [`PAIRS_MAX`] pairs.
    TooManyRecords,
    /// More than [`ENTRIES_MAX`] directory entries.
    TooManyEntries,
    /// No `records.txt`.
    MissingIndex,
    /// `records.txt` is not the generated index of the complete directory.
    IndexMismatch,
    /// `versions.txt` is not the generated table of the authenticated records.
    VersionsMismatch,
    /// `root-allowed-signers` is not the pinned root file.
    RootFileMismatch,
    /// A record's root signature does not verify.
    StatementSignature(String),
    /// A record is not a valid statement, or breaks the epoch order: the record and the reason.
    Statement(String, String),
    /// `policy.json` is required and missing.
    PolicyMissing,
    /// `evidence-policy.json` is required and missing.
    EvidencePolicyMissing,
    /// A control file breaks its schema or its binding: the file and the reason.
    Policy(String, String),
    /// The version is not `0.N.0`.
    NotVersion,
    /// No epoch covers the version.
    NoTrust(String),
    /// The version's key is the historical root, retired for new releases.
    Retired(String),
    /// The version's key is revoked for new signing.
    Revoked(String),
    /// The release signature is by a key other than the version's.
    WrongKey(String),
    /// The release signature does not verify under the version's key.
    BadSignature(String),
    /// A record cannot be rendered byte for byte.
    Render(String),
}

impl fmt::Display for TrustError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "release trust: ")?;
        match self {
            TrustError::Read(name) => write!(f, "cannot read {name}"),
            TrustError::UnknownEntry(name) => write!(f, "unknown entry {name}"),
            TrustError::NotRegular(name) => write!(f, "{name} is not a regular file"),
            TrustError::MissingPartner(name) => write!(f, "{name} has no partner"),
            TrustError::TooLarge(name, max) => write!(f, "{name} is over {max} bytes"),
            TrustError::TooManyRecords => write!(f, "more than {PAIRS_MAX} record pairs"),
            TrustError::TooManyEntries => write!(f, "more than {ENTRIES_MAX} entries"),
            TrustError::MissingIndex => write!(f, "{RECORDS_FILE} is missing"),
            TrustError::IndexMismatch => {
                write!(f, "{RECORDS_FILE} is not the index of every record")
            }
            TrustError::VersionsMismatch => {
                write!(f, "{VERSIONS_FILE} does not match the records")
            }
            TrustError::RootFileMismatch => write!(f, "{ROOT_FILE} is not the pinned root"),
            TrustError::StatementSignature(name) => {
                write!(f, "{name}: the root signature does not verify")
            }
            TrustError::Statement(name, why) => write!(f, "{name}: {why}"),
            TrustError::PolicyMissing => write!(f, "{POLICY_FILE} is missing"),
            TrustError::EvidencePolicyMissing => write!(f, "{EVIDENCE_POLICY_FILE} is missing"),
            TrustError::Policy(name, why) => write!(f, "{name}: {why}"),
            TrustError::NotVersion => write!(f, "not a preview version"),
            TrustError::NoTrust(v) => write!(f, "no release key for {v}"),
            TrustError::Retired(v) => {
                write!(
                    f,
                    "the key for {v} is the historical key; it signs no new release"
                )
            }
            TrustError::Revoked(v) => write!(f, "the key for {v} is revoked for new signing"),
            TrustError::WrongKey(v) => write!(f, "the signature is not by the key for {v}"),
            TrustError::BadSignature(v) => {
                write!(f, "the release signature for {v} does not verify")
            }
            TrustError::Render(name) => write!(f, "{name} cannot be rendered exactly"),
        }
    }
}

impl std::error::Error for TrustError {}

/// The check of one root statement's signature. [`RootCheck`] is the only production check;
/// tests pass a structural mock, which proves no signature.
pub trait StatementCheck {
    /// Whether `signature`, an armored SSHSIG, is the pinned root's signature of exactly
    /// `statement` in `namespace`.
    fn check(&self, namespace: &str, statement: &[u8], signature: &[u8]) -> bool;
}

/// The pinned historical root, restricted to the two statement namespaces.
#[derive(Clone, Copy, Debug, Default)]
pub struct RootCheck;

impl StatementCheck for RootCheck {
    fn check(&self, namespace: &str, statement: &[u8], signature: &[u8]) -> bool {
        if namespace != TRANSITION_NAMESPACE && namespace != REVOCATION_NAMESPACE {
            return false;
        }
        match root_key() {
            Some(root) => sshsig_check(&root, namespace, statement, signature).is_ok(),
            None => false,
        }
    }
}

/// The pinned root public key.
fn root_key() -> Option<PublicKey> {
    PublicKey::from_openssh(&format!("{ROOT_KEY_TYPE} {ROOT_KEY_BASE64}")).ok()
}

/// Why an SSHSIG does not verify.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SigFault {
    /// Not one armored SSHSIG v1 within its bound.
    Malformed,
    /// A whole SSHSIG by another key.
    OtherKey,
    /// By the key, but not a valid sha512 signature of the message in the namespace.
    Invalid,
}

/// Verifies the armored SSHSIG `armored` over exactly `message` in `namespace` by `key`, as
/// `ssh-keygen -Y verify` does with an allowed-signers line restricted to that namespace:
/// SSHSIG v1, sha512, empty reserved bytes, the key's own algorithm, and for an sk key the
/// user-presence flag.
pub fn sshsig_check(
    key: &PublicKey,
    namespace: &str,
    message: &[u8],
    armored: &[u8],
) -> Result<(), SigFault> {
    if armored.len() as u64 > STATEMENT_SIG_MAX.max(RELEASE_SIG_MAX) {
        return Err(SigFault::Malformed);
    }
    let sig = SshSig::from_pem(armored).map_err(|_| SigFault::Malformed)?;
    if sig.public_key() != key.key_data() {
        return Err(SigFault::OtherKey);
    }
    if sig.hash_alg() != HashAlg::Sha512
        || !sig.reserved().is_empty()
        || sig.namespace() != namespace
        || sig.algorithm() != key.algorithm()
    {
        return Err(SigFault::Invalid);
    }
    key.verify(namespace, message, &sig)
        .map_err(|_| SigFault::Invalid)?;
    if key.algorithm() == Algorithm::SkEd25519
        && !sig
            .signature()
            .as_bytes()
            .get(ED25519_SIG_LEN)
            .is_some_and(|flags| flags & SK_USER_PRESENT != 0)
    {
        return Err(SigFault::Invalid);
    }
    Ok(())
}

/// One signed record: its file names, exact bytes and digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// The JSON basename.
    pub name: String,
    /// The JSON bytes, exactly as signed.
    pub json: Vec<u8>,
    /// The armored root signature, exactly as supplied.
    pub sig: Vec<u8>,
    /// SHA-256 of the JSON bytes, lowercase hex.
    pub sha256: String,
}

impl Record {
    /// The signature basename.
    pub fn sig_name(&self) -> String {
        format!("{}.sig", self.name)
    }
}

/// One key's range of versions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Epoch {
    /// The first minor.
    pub first: u32,
    /// The last good minor, inclusive; `None` while the epoch is active. A revocation with no good
    /// release gives `first - 1`.
    pub last: Option<u32>,
    pub key_type: String,
    pub key_base64: String,
    pub fingerprint: String,
    /// The transition that enrolled the key; `None` for the historical root.
    pub transition: Option<Record>,
    /// The revocation that ended the epoch.
    pub revocation: Option<Record>,
}

impl Epoch {
    /// Whether this is the historical root's epoch.
    pub fn is_historical(&self) -> bool {
        self.transition.is_none()
    }

    /// Whether the epoch has at least one good version.
    pub fn has_good_release(&self) -> bool {
        self.last.is_none_or(|last| last >= self.first)
    }

    /// Whether the epoch covers `minor`.
    pub fn covers(&self, minor: u32) -> bool {
        minor >= self.first && self.last.is_none_or(|last| minor <= last)
    }

    /// The release-domain allowed-signers line, without its LF.
    pub fn signer_line(&self) -> String {
        format!(
            "{PRINCIPAL} namespaces=\"{MANIFEST_NAMESPACE}\" {} {}",
            self.key_type, self.key_base64
        )
    }

    fn public_key(&self) -> Option<PublicKey> {
        PublicKey::from_openssh(&format!("{} {}", self.key_type, self.key_base64)).ok()
    }
}

/// The key selected for one version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signer {
    /// The plain version `0.N.0`.
    pub version: String,
    pub key_type: String,
    pub key_base64: String,
    pub fingerprint: String,
    /// The release-domain allowed-signers line, without its LF.
    pub signer_line: String,
    /// SHA-256 of the enrolling transition's bytes; `None` for the historical root.
    pub transition_sha256: Option<String>,
    /// The epoch is the historical root's.
    pub historical: bool,
    /// The epoch has a revocation.
    pub revoked: bool,
}

/// `policy.json`, checked against the records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub key_arn: String,
    pub role_arn: String,
    pub fingerprint: String,
    pub first_machine_tag: String,
    pub targets: Vec<String>,
    pub records_sha256: String,
    pub evidence_policy_sha256: String,
    pub signer_workflow_sha: String,
    /// SHA-256 of the file's bytes.
    pub sha256: String,
}

/// The authenticated release trust of one `release-keys/` directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseKeys {
    epochs: Vec<Epoch>,
    records: String,
    policy: Option<Policy>,
}

/// The historical root's epoch.
fn historical_epoch() -> Epoch {
    Epoch {
        first: HISTORICAL_FIRST,
        last: Some(HISTORICAL_LAST),
        key_type: ROOT_KEY_TYPE.to_string(),
        key_base64: ROOT_KEY_BASE64.to_string(),
        fingerprint: ROOT_FINGERPRINT.to_string(),
        transition: None,
        revocation: None,
    }
}

/// The minor of the plain preview version `0.N.0`: N from 1 through 4294967295 with no leading
/// zero. `None` for anything else, a patch, a pre-release or a 1.x version included.
pub fn version_minor(version: &str) -> Option<u32> {
    let minor = version.strip_prefix("0.")?.strip_suffix(".0")?;
    if minor.is_empty()
        || minor.len() > 10
        || minor.starts_with('0')
        || !minor.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let n: u64 = minor.parse().ok()?;
    u32::try_from(n).ok().filter(|n| *n >= 1)
}

/// The minor of the native preview tag `v0.N.0`.
pub fn tag_minor(tag: &str) -> Option<u32> {
    version_minor(tag.strip_prefix('v')?)
}

/// The tag of `minor`.
pub fn tag_of(minor: u32) -> String {
    format!("v0.{minor}.0")
}

/// The generated `records.txt` of these JSON basenames: sorted lexicographically, one
/// `JSON_BASENAME JSON_BASENAME.sig` line each with a final LF. Empty for no names.
pub fn records_index<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    let mut names: Vec<&str> = names.into_iter().collect();
    names.sort_unstable();
    names.dedup();
    names.iter().map(|n| format!("{n} {n}.sig\n")).collect()
}

/// SHA-256 of `bytes`, lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The kind of a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Transition,
    Revocation,
}

impl Kind {
    fn namespace(self) -> &'static str {
        match self {
            Kind::Transition => TRANSITION_NAMESPACE,
            Kind::Revocation => REVOCATION_NAMESPACE,
        }
    }
}

/// What a directory entry's name says it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Entry {
    Json(Kind, u32),
    Sig(Kind, u32),
    Records,
    Versions,
    Readme,
    Root,
    Policy,
    EvidencePolicy,
}

fn classify(name: &str) -> Option<Entry> {
    match name {
        RECORDS_FILE => return Some(Entry::Records),
        VERSIONS_FILE => return Some(Entry::Versions),
        README_FILE => return Some(Entry::Readme),
        ROOT_FILE => return Some(Entry::Root),
        POLICY_FILE => return Some(Entry::Policy),
        EVIDENCE_POLICY_FILE => return Some(Entry::EvidencePolicy),
        _ => {}
    }
    let (stem, sig) = match name.strip_suffix(".json.sig") {
        Some(stem) => (stem, true),
        None => (name.strip_suffix(".json")?, false),
    };
    let (kind, tag) = if let Some(tag) = stem.strip_prefix("transition-") {
        (Kind::Transition, tag)
    } else {
        (Kind::Revocation, stem.strip_prefix("revocation-")?)
    };
    let minor = tag_minor(tag)?;
    Some(if sig {
        Entry::Sig(kind, minor)
    } else {
        Entry::Json(kind, minor)
    })
}

/// A name a refusal may repeat: printable ASCII of at most [`ECHO_MAX`] bytes.
fn echo(name: &str) -> String {
    if name.len() <= ECHO_MAX && name.bytes().all(|b| b.is_ascii_graphic()) {
        name.to_string()
    } else {
        "(unprintable name)".to_string()
    }
}

/// The complete listing of a directory: each entry's name and kind. Bounded before it grows.
fn list(dir: &Path) -> Result<Vec<(String, Entry)>, TrustError> {
    let what = || TrustError::Read("release-keys".to_string());
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).map_err(|_| what())? {
        let entry = entry.map_err(|_| what())?;
        if out.len() == ENTRIES_MAX {
            return Err(TrustError::TooManyEntries);
        }
        let raw = entry.file_name();
        let Some(name) = raw.to_str() else {
            return Err(TrustError::UnknownEntry("(unprintable name)".to_string()));
        };
        let kind = classify(name).ok_or_else(|| TrustError::UnknownEntry(echo(name)))?;
        let meta = fs::symlink_metadata(entry.path()).map_err(|_| TrustError::Read(echo(name)))?;
        if !meta.file_type().is_file() {
            return Err(TrustError::NotRegular(echo(name)));
        }
        out.push((name.to_string(), kind));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// The bytes of `dir/name`, refused over `max` before and after the read.
fn read_bounded(dir: &Path, name: &str, max: u64) -> Result<Vec<u8>, TrustError> {
    let path = dir.join(name);
    let too_large = || TrustError::TooLarge(name.to_string(), max);
    let meta = fs::symlink_metadata(&path).map_err(|_| TrustError::Read(name.to_string()))?;
    if !meta.file_type().is_file() {
        return Err(TrustError::NotRegular(name.to_string()));
    }
    if meta.len() > max {
        return Err(too_large());
    }
    let file = fs::File::open(&path).map_err(|_| TrustError::Read(name.to_string()))?;
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| TrustError::Read(name.to_string()))?;
    if bytes.len() as u64 > max {
        return Err(too_large());
    }
    Ok(bytes)
}

/// The bound of an entry's bytes.
fn bound(kind: Entry) -> u64 {
    match kind {
        Entry::Json(..) => STATEMENT_MAX,
        Entry::Sig(..) => STATEMENT_SIG_MAX,
        Entry::Records | Entry::Versions | Entry::Readme => INDEX_MAX,
        Entry::Root => ROOT_FILE_MAX,
        Entry::Policy | Entry::EvidencePolicy => POLICY_MAX,
    }
}

/// The names of the JSON records in a listing, after the pairing and count checks.
fn paired(listing: &[(String, Entry)]) -> Result<Vec<(String, Kind, u32)>, TrustError> {
    let names: Vec<&str> = listing.iter().map(|(n, _)| n.as_str()).collect();
    let mut records = Vec::new();
    for (name, kind) in listing {
        match *kind {
            Entry::Json(k, n) => {
                if !names.contains(&format!("{name}.sig").as_str()) {
                    return Err(TrustError::MissingPartner(name.clone()));
                }
                records.push((name.clone(), k, n));
            }
            Entry::Sig(..) => {
                let json = name.strip_suffix(".sig").unwrap_or(name);
                if !names.contains(&json) {
                    return Err(TrustError::MissingPartner(name.clone()));
                }
            }
            _ => {}
        }
    }
    if records.len() > PAIRS_MAX {
        return Err(TrustError::TooManyRecords);
    }
    Ok(records)
}

/// The generated `records.txt` of the complete directory `dir`: every entry is checked by name,
/// kind and pairing, but no record is read or verified. For adding a pair before a load.
pub fn generated_records(dir: &Path) -> Result<String, TrustError> {
    let listing = list(dir)?;
    let records = paired(&listing)?;
    Ok(records_index(records.iter().map(|(n, _, _)| n.as_str())))
}

// ---- strict JSON objects ----

/// A JSON object's entries in order, duplicates kept, so a duplicate key can be refused.
struct Entries(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for Entries {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Entries;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Entries, A::Error> {
                let mut out = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    let value: Value = map.next_value()?;
                    out.push((key, value));
                }
                Ok(Entries(out))
            }
        }
        d.deserialize_map(V)
    }
}

/// The closed object `bytes` with exactly the fields `known`: the reason it is refused, or its
/// fields by name.
fn closed_object(bytes: &[u8], known: &[&str]) -> Result<BTreeMap<String, Value>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "not UTF-8".to_string())?;
    if text.starts_with('\u{feff}') {
        return Err("starts with a byte order mark".to_string());
    }
    let Entries(entries) = serde_json::from_str(text).map_err(|e| {
        if e.is_data() {
            "not a JSON object".to_string()
        } else {
            "malformed JSON".to_string()
        }
    })?;
    let mut out = BTreeMap::new();
    for (key, value) in entries {
        if !known.contains(&key.as_str()) {
            return Err(unknown_field(&key, known));
        }
        if out.insert(key.clone(), value).is_some() {
            return Err(format!("duplicate field '{}'", echo(&key)));
        }
    }
    if let Some(missing) = known.iter().find(|k| !out.contains_key(**k)) {
        return Err(format!("missing field '{missing}'"));
    }
    Ok(out)
}

/// `unknown field '<key>'`, with `; did you mean '<nearest>'?` when one is close.
fn unknown_field(key: &str, known: &[&str]) -> String {
    let shown = echo(key);
    let mut out = format!("unknown field '{shown}'");
    if shown == key
        && let Some(near) = rdy_model::did_you_mean::nearest(key, known)
    {
        out.push_str(&format!("; did you mean '{near}'?"));
    }
    out
}

fn invalid(field: &str) -> String {
    format!("field '{field}' is invalid")
}

/// The string field `field`.
fn string_field<'a>(fields: &'a BTreeMap<String, Value>, field: &str) -> Result<&'a str, String> {
    fields
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(field))
}

/// Whether `s` is `SHA256:` and 43 unpadded base64 characters.
fn valid_fingerprint(s: &str) -> bool {
    s.strip_prefix("SHA256:").is_some_and(|b64| {
        b64.len() == 43
            && b64
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
    })
}

fn valid_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The base64 and fingerprint of the exact machine `public_key` string, or the reason it is
/// refused.
fn machine_key(public_key: &str) -> Result<(String, String), String> {
    let b64 = public_key
        .strip_prefix("ssh-ed25519 ")
        .ok_or_else(|| invalid("public_key"))?;
    if b64.len() != MACHINE_KEY_B64_LEN
        || !b64
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
    {
        return Err(invalid("public_key"));
    }
    let key = PublicKey::from_openssh(public_key).map_err(|_| invalid("public_key"))?;
    let blob = key.to_bytes().map_err(|_| invalid("public_key"))?;
    if key.algorithm() != Algorithm::Ed25519
        || blob.len() != MACHINE_KEY_BLOB_LEN
        || !key.comment().is_empty()
        || key.to_openssh().ok().as_deref() != Some(public_key)
    {
        return Err(invalid("public_key"));
    }
    Ok((
        b64.to_string(),
        key.fingerprint(HashAlg::Sha256).to_string(),
    ))
}

/// A parsed transition.
struct Transition {
    minor: u32,
    key_base64: String,
    fingerprint: String,
}

fn parse_transition(record: &Record, minor: u32) -> Result<Transition, String> {
    let fields = closed_object(&record.json, &TRANSITION_FIELDS)?;
    if string_field(&fields, "scheme")? != TRANSITION_NAMESPACE {
        return Err(invalid("scheme"));
    }
    if string_field(&fields, "repository")? != REPOSITORY {
        return Err(invalid("repository"));
    }
    if tag_minor(string_field(&fields, "first_machine_tag")?) != Some(minor) {
        return Err("first_machine_tag is not the file name's tag".to_string());
    }
    let (key_base64, fingerprint) = machine_key(string_field(&fields, "public_key")?)?;
    let stated = string_field(&fields, "fingerprint")?;
    if !valid_fingerprint(stated) {
        return Err(invalid("fingerprint"));
    }
    if stated != fingerprint {
        return Err("fingerprint is not the public key's".to_string());
    }
    Ok(Transition {
        minor,
        key_base64,
        fingerprint,
    })
}

/// A parsed revocation.
struct Revocation {
    minor: u32,
    fingerprint: String,
    /// The last good minor; `None` for no good release.
    last_good: Option<u32>,
}

fn parse_revocation(record: &Record, minor: u32) -> Result<Revocation, String> {
    let fields = closed_object(&record.json, &REVOCATION_FIELDS)?;
    if string_field(&fields, "scheme")? != REVOCATION_NAMESPACE {
        return Err(invalid("scheme"));
    }
    if string_field(&fields, "repository")? != REPOSITORY {
        return Err(invalid("repository"));
    }
    if tag_minor(string_field(&fields, "first_machine_tag")?) != Some(minor) {
        return Err("first_machine_tag is not the file name's tag".to_string());
    }
    let fingerprint = string_field(&fields, "fingerprint")?;
    if !valid_fingerprint(fingerprint) {
        return Err(invalid("fingerprint"));
    }
    let last_good = match fields.get("last_good_tag") {
        Some(Value::Null) => None,
        Some(Value::String(tag)) => {
            let last = tag_minor(tag).ok_or_else(|| invalid("last_good_tag"))?;
            if last < minor {
                return Err("last_good_tag is before first_machine_tag".to_string());
            }
            Some(last)
        }
        _ => return Err(invalid("last_good_tag")),
    };
    Ok(Revocation {
        minor,
        fingerprint: fingerprint.to_string(),
        last_good,
    })
}

/// Appends the transition `t` after the epochs so far.
fn enroll(epochs: &mut Vec<Epoch>, t: Transition, record: Record) -> Result<(), TrustError> {
    let refuse = |why: &str| TrustError::Statement(record.name.clone(), why.to_string());
    if t.minor <= HISTORICAL_LAST {
        return Err(refuse("the first machine epoch starts after v0.35.0"));
    }
    let Some(prior) = epochs.last() else {
        return Err(refuse("no historical epoch"));
    };
    let Some(prior_last) = prior.last else {
        return Err(refuse("the previous epoch has no revocation"));
    };
    if t.minor <= prior.first || t.minor <= prior_last {
        return Err(refuse(
            "starts at or before the previous epoch's first or last good tag",
        ));
    }
    if epochs.iter().any(|e| e.fingerprint == t.fingerprint) {
        return Err(refuse("reuses an enrolled key"));
    }
    epochs.push(Epoch {
        first: t.minor,
        last: None,
        key_type: MACHINE_KEY_TYPE.to_string(),
        key_base64: t.key_base64,
        fingerprint: t.fingerprint,
        transition: Some(record),
        revocation: None,
    });
    Ok(())
}

/// Ends the active epoch with the revocation `r`.
fn revoke(epochs: &mut [Epoch], r: Revocation, record: Record) -> Result<(), TrustError> {
    let refuse = |why: &str| TrustError::Statement(record.name.clone(), why.to_string());
    let Some(active) = epochs
        .last_mut()
        .filter(|e| !e.is_historical() && e.last.is_none() && e.first == r.minor)
    else {
        return Err(refuse("no active machine epoch starts at this tag"));
    };
    if active.fingerprint != r.fingerprint {
        return Err(refuse("fingerprint is not the active epoch's key"));
    }
    active.last = Some(r.last_good.unwrap_or(r.minor - 1));
    active.revocation = Some(record);
    Ok(())
}

fn policy_error(why: impl Into<String>) -> TrustError {
    TrustError::Policy(POLICY_FILE.to_string(), why.into())
}

fn policy_string<'a>(
    fields: &'a BTreeMap<String, Value>,
    field: &str,
) -> Result<&'a str, TrustError> {
    let s = string_field(fields, field).map_err(policy_error)?;
    if s.is_empty() || s.len() > POLICY_STRING_MAX {
        return Err(policy_error(invalid(field)));
    }
    Ok(s)
}

/// Whether `arn` starts with `prefix` and holds only printable ASCII.
fn valid_arn(arn: &str, prefix: &str) -> bool {
    arn.len() > prefix.len() && arn.starts_with(prefix) && arn.bytes().all(|b| b.is_ascii_graphic())
}

/// `policy.json`, checked against the final epoch and the two digests it binds.
fn parse_policy(
    bytes: &[u8],
    epochs: &[Epoch],
    records: &[u8],
    evidence: &[u8],
) -> Result<Policy, TrustError> {
    let fields = closed_object(bytes, &POLICY_FIELDS).map_err(policy_error)?;
    if !fields.get("schema").is_some_and(|v| v.as_u64() == Some(1)) {
        return Err(policy_error(invalid("schema")));
    }
    if policy_string(&fields, "repository")? != REPOSITORY {
        return Err(policy_error(invalid("repository")));
    }
    let key_arn = policy_string(&fields, "key_arn")?;
    if !valid_arn(key_arn, "arn:aws:kms:") {
        return Err(policy_error(invalid("key_arn")));
    }
    let role_arn = policy_string(&fields, "role_arn")?;
    if !valid_arn(role_arn, "arn:aws:iam::") {
        return Err(policy_error(invalid("role_arn")));
    }
    let fingerprint = policy_string(&fields, "fingerprint")?;
    let first_machine_tag = policy_string(&fields, "first_machine_tag")?;
    let targets = match fields.get("targets") {
        Some(Value::Array(items)) => items
            .iter()
            .map(|t| t.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| policy_error(invalid("targets")))?,
        _ => return Err(policy_error(invalid("targets"))),
    };
    let records_sha256 = policy_string(&fields, "records_sha256")?;
    let evidence_policy_sha256 = policy_string(&fields, "evidence_policy_sha256")?;
    let signer_workflow_sha = policy_string(&fields, "signer_workflow_sha")?;
    for (field, value, len) in [
        ("records_sha256", records_sha256, 64),
        ("evidence_policy_sha256", evidence_policy_sha256, 64),
        ("signer_workflow_sha", signer_workflow_sha, 40),
    ] {
        if !valid_hex(value, len) {
            return Err(policy_error(invalid(field)));
        }
    }
    if targets != RELEASE_TARGETS {
        return Err(policy_error("targets are not the approved target set"));
    }
    let Some(current) = epochs.last().filter(|e| !e.is_historical()) else {
        return Err(policy_error("no machine epoch is enrolled"));
    };
    if fingerprint != current.fingerprint {
        return Err(policy_error("fingerprint is not the current machine key"));
    }
    if tag_minor(first_machine_tag) != Some(current.first) {
        return Err(policy_error(
            "first_machine_tag is not the current machine epoch's",
        ));
    }
    if records_sha256 != sha256_hex(records) {
        return Err(policy_error("records_sha256 does not match records.txt"));
    }
    if evidence_policy_sha256 != sha256_hex(evidence) {
        return Err(policy_error(
            "evidence_policy_sha256 does not match evidence-policy.json",
        ));
    }
    Ok(Policy {
        key_arn: key_arn.to_string(),
        role_arn: role_arn.to_string(),
        fingerprint: fingerprint.to_string(),
        first_machine_tag: first_machine_tag.to_string(),
        targets,
        records_sha256: records_sha256.to_string(),
        evidence_policy_sha256: evidence_policy_sha256.to_string(),
        signer_workflow_sha: signer_workflow_sha.to_string(),
        sha256: sha256_hex(bytes),
    })
}

/// `evidence-policy.json`'s top level: its closed field set and its frozen scalar pins. The
/// release controller checks its nested records.
fn check_evidence_policy(bytes: &[u8]) -> Result<(), TrustError> {
    let refuse = |why: String| TrustError::Policy(EVIDENCE_POLICY_FILE.to_string(), why);
    let fields = closed_object(bytes, &EVIDENCE_POLICY_FIELDS).map_err(refuse)?;
    if !fields.get("schema").is_some_and(|v| v.as_u64() == Some(1)) {
        return Err(refuse(invalid("schema")));
    }
    if !fields
        .get("repository_id")
        .is_some_and(|v| v.as_u64() == Some(REPOSITORY_ID))
    {
        return Err(refuse(invalid("repository_id")));
    }
    if fields.get("release_workflow_path").and_then(Value::as_str) != Some(RELEASE_WORKFLOW_PATH) {
        return Err(refuse(invalid("release_workflow_path")));
    }
    Ok(())
}

impl ReleaseKeys {
    /// The trust with no machine record: the historical root only.
    pub fn historical() -> ReleaseKeys {
        ReleaseKeys {
            epochs: vec![historical_epoch()],
            records: String::new(),
            policy: None,
        }
    }

    /// Loads and authenticates the complete `release-keys/` directory `dir` against the pinned
    /// root.
    pub fn load(dir: &Path) -> Result<ReleaseKeys, TrustError> {
        ReleaseKeys::load_checked_by(dir, &RootCheck)
    }

    /// [`ReleaseKeys::load`] with the statement check `check`. Production passes [`RootCheck`]
    /// only; a test's structural mock proves no signature.
    pub fn load_checked_by(
        dir: &Path,
        check: &dyn StatementCheck,
    ) -> Result<ReleaseKeys, TrustError> {
        let listing = list(dir)?;
        let named = paired(&listing)?;
        let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for (name, kind) in &listing {
            files.insert(name.clone(), read_bounded(dir, name, bound(*kind))?);
        }
        let index = files
            .get(RECORDS_FILE)
            .ok_or(TrustError::MissingIndex)?
            .clone();
        let generated = records_index(named.iter().map(|(n, _, _)| n.as_str()));
        if index != generated.as_bytes() {
            return Err(TrustError::IndexMismatch);
        }
        if files
            .get(ROOT_FILE)
            .is_some_and(|root| root != ROOT_ALLOWED_SIGNERS.as_bytes())
        {
            return Err(TrustError::RootFileMismatch);
        }
        // Every record's root signature, over its exact bytes, before any record is parsed.
        let mut records = Vec::new();
        for (name, kind, minor) in &named {
            let json = files.get(name).cloned().unwrap_or_default();
            let sig = files
                .get(&format!("{name}.sig"))
                .cloned()
                .unwrap_or_default();
            if !check.check(kind.namespace(), &json, &sig) {
                return Err(TrustError::StatementSignature(name.clone()));
            }
            let record = Record {
                name: name.clone(),
                sha256: sha256_hex(&json),
                json,
                sig,
            };
            records.push((*minor, *kind, record));
        }
        // Numeric order, a transition before a revocation at the same minor.
        records.sort_by_key(|r| (r.0, r.1));
        let mut epochs = vec![historical_epoch()];
        for (minor, kind, record) in records {
            let refuse = |why: String| TrustError::Statement(record.name.clone(), why);
            match kind {
                Kind::Transition => {
                    let t = parse_transition(&record, minor).map_err(refuse)?;
                    enroll(&mut epochs, t, record)?;
                }
                Kind::Revocation => {
                    let r = parse_revocation(&record, minor).map_err(refuse)?;
                    revoke(&mut epochs, r, record)?;
                }
            }
        }
        let mut keys = ReleaseKeys {
            epochs,
            records: generated,
            policy: None,
        };
        if files
            .get(VERSIONS_FILE)
            .is_some_and(|v| v != keys.versions_txt().as_bytes())
        {
            return Err(TrustError::VersionsMismatch);
        }
        let evidence = files.get(EVIDENCE_POLICY_FILE);
        match files.get(POLICY_FILE) {
            None if named.is_empty() && evidence.is_none() => {}
            None => return Err(TrustError::PolicyMissing),
            Some(policy) => {
                let evidence = evidence.ok_or(TrustError::EvidencePolicyMissing)?;
                check_evidence_policy(evidence)?;
                keys.policy = Some(parse_policy(policy, &keys.epochs, &index, evidence)?);
            }
        }
        Ok(keys)
    }

    /// The epochs in numeric order, the historical root first.
    pub fn epochs(&self) -> &[Epoch] {
        &self.epochs
    }

    /// The authenticated `records.txt`.
    pub fn records_txt(&self) -> &str {
        &self.records
    }

    /// The checked `policy.json`; `None` with no machine record.
    pub fn policy(&self) -> Option<&Policy> {
        self.policy.as_ref()
    }

    /// The generated `versions.txt`: one `FIRST LAST KEY_TYPE KEY_BASE64 FINGERPRINT` row per
    /// epoch with a good release, LAST `*` for the active epoch.
    pub fn versions_txt(&self) -> String {
        self.epochs
            .iter()
            .filter(|e| e.has_good_release())
            .map(|e| {
                let last = e.last.map_or_else(|| "*".to_string(), |l| l.to_string());
                format!(
                    "{} {last} {} {} {}\n",
                    e.first, e.key_type, e.key_base64, e.fingerprint
                )
            })
            .collect()
    }

    /// The one epoch that covers `version`.
    fn epoch_for(&self, version: &str) -> Result<&Epoch, TrustError> {
        let minor = version_minor(version).ok_or(TrustError::NotVersion)?;
        self.epochs
            .iter()
            .find(|e| e.covers(minor))
            .ok_or_else(|| TrustError::NoTrust(version.to_string()))
    }

    /// The one key of `version`, for archival verification: the root through v0.35.0, then each
    /// machine epoch through its last good tag. A gap, an earlier version, a patch, a
    /// pre-release and a 1.x version have none.
    pub fn signer_for(&self, version: &str) -> Result<Signer, TrustError> {
        let epoch = self.epoch_for(version)?;
        Ok(Signer {
            version: version.to_string(),
            key_type: epoch.key_type.clone(),
            key_base64: epoch.key_base64.clone(),
            fingerprint: epoch.fingerprint.clone(),
            signer_line: epoch.signer_line(),
            transition_sha256: epoch.transition.as_ref().map(|t| t.sha256.clone()),
            historical: epoch.is_historical(),
            revoked: epoch.revocation.is_some(),
        })
    }

    /// The key a new signing of `version` may use: [`ReleaseKeys::signer_for`], refused for the
    /// retired root and for a revoked key, even for one of its old good versions.
    pub fn eligible_signer(&self, version: &str) -> Result<Signer, TrustError> {
        let signer = self.signer_for(version)?;
        if signer.historical {
            return Err(TrustError::Retired(version.to_string()));
        }
        if signer.revoked {
            return Err(TrustError::Revoked(version.to_string()));
        }
        if self
            .policy
            .as_ref()
            .is_none_or(|p| p.fingerprint != signer.fingerprint)
        {
            return Err(TrustError::PolicyMissing);
        }
        Ok(signer)
    }

    /// Verifies the release manifest `manifest` and its armored SSHSIG `signature` for
    /// `version`, under the one selected key only, in namespace `rdy-release-v1`. A valid
    /// signature by another recognized key is refused.
    pub fn verify_release(
        &self,
        version: &str,
        manifest: &[u8],
        signature: &[u8],
    ) -> Result<Signer, TrustError> {
        let epoch = self.epoch_for(version)?;
        if manifest.len() as u64 > MANIFEST_MAX {
            return Err(TrustError::TooLarge(
                release_install::MANIFEST_FILE.to_string(),
                MANIFEST_MAX,
            ));
        }
        if signature.len() as u64 > RELEASE_SIG_MAX {
            return Err(TrustError::TooLarge(
                release_install::SIGNATURE_FILE.to_string(),
                RELEASE_SIG_MAX,
            ));
        }
        let key = epoch
            .public_key()
            .ok_or_else(|| TrustError::BadSignature(version.to_string()))?;
        match sshsig_check(&key, MANIFEST_NAMESPACE, manifest, signature) {
            Ok(()) => self.signer_for(version),
            Err(SigFault::OtherKey) => Err(TrustError::WrongKey(version.to_string())),
            Err(_) => Err(TrustError::BadSignature(version.to_string())),
        }
    }

    /// The README's trust section: each key with its fingerprint and versions, its release-domain
    /// allowed-signers line, then each root statement's exact bytes and armored signature with
    /// the commands that verify it offline from the root.
    pub fn trust_section(&self) -> Result<String, TrustError> {
        let mut out = String::from(
            "### Release keys\n\nEach version has exactly one release key. A signature by any other key is refused.\n\n| Versions | Key type | Fingerprint | State |\n| --- | --- | --- | --- |\n",
        );
        for e in &self.epochs {
            let versions = match e.last {
                _ if !e.has_good_release() => "none".to_string(),
                Some(last) => format!("{} to {}", tag_of(e.first), tag_of(last)),
                None => format!("{} and later", tag_of(e.first)),
            };
            let state = if e.is_historical() {
                "historical key, retired for new releases".to_string()
            } else {
                match (e.last, &e.revocation) {
                    (_, None) => "machine key, current".to_string(),
                    (Some(last), Some(_)) if last >= e.first => {
                        format!("machine key, revoked after {}", tag_of(last))
                    }
                    _ => format!(
                        "machine key from {}, revoked before any release",
                        tag_of(e.first)
                    ),
                }
            };
            out.push_str(&format!(
                "| {versions} | `{}` | `{}` | {state} |\n",
                e.key_type, e.fingerprint
            ));
        }
        out.push_str("\nNo other version has a release key.");
        if self.epochs.len() == 1 {
            out.push_str(
                " The machine release key is pending: no version after v0.35.0 has a release key yet.",
            );
        }
        out.push('\n');
        for e in self.epochs.iter().filter(|e| e.has_good_release()) {
            let versions = match e.last {
                Some(last) => format!("{} to {}", tag_of(e.first), tag_of(last)),
                None => format!("{} and later", tag_of(e.first)),
            };
            out.push_str(&format!(
                "\nThe allowed-signers line for {versions}:\n\n```text\n{}\n```\n",
                e.signer_line()
            ));
        }
        let mut statements: Vec<(&Record, Kind)> = Vec::new();
        for e in &self.epochs {
            statements.extend(e.transition.iter().map(|r| (r, Kind::Transition)));
            statements.extend(e.revocation.iter().map(|r| (r, Kind::Revocation)));
        }
        for (record, kind) in statements {
            out.push_str(&render_statement(record, kind)?);
        }
        Ok(out)
    }
}

/// One root statement: its exact bytes, its armored signature and the offline root check.
fn render_statement(record: &Record, kind: Kind) -> Result<String, TrustError> {
    let exact = |bytes: &[u8]| -> Option<String> {
        let text = std::str::from_utf8(bytes).ok()?;
        (text.ends_with('\n') && !text.contains("```")).then(|| text.to_string())
    };
    let refuse = || TrustError::Render(record.name.clone());
    let json = exact(&record.json).ok_or_else(refuse)?;
    let sig = exact(&record.sig).ok_or_else(refuse)?;
    let what = match kind {
        Kind::Transition => "transition",
        Kind::Revocation => "revocation",
    };
    let namespace = kind.namespace();
    let dir = PathBuf::from("/path/to/release-keys");
    let signers = format!("root-{what}-allowed-signers");
    let commands = [
        GuideCommand {
            program: "printf".into(),
            args: vec![
                r"%s\n".into(),
                format!("{PRINCIPAL} namespaces=\"{namespace}\" {ROOT_KEY_TYPE} {ROOT_KEY_BASE64}")
                    .into(),
            ],
            cwd: dir.clone(),
            stdin: None,
            stdout: Some(dir.join(&signers)),
        },
        GuideCommand {
            program: "ssh-keygen".into(),
            args: [
                "-Y",
                "verify",
                "-f",
                signers.as_str(),
                "-I",
                PRINCIPAL,
                "-n",
                namespace,
                "-s",
                record.sig_name().as_str(),
            ]
            .map(Into::into)
            .to_vec(),
            cwd: dir.clone(),
            stdin: Some(dir.join(&record.name)),
            stdout: None,
        },
    ];
    let block = release_install::render_commands(&commands).map_err(|_| refuse())?;
    Ok(format!(
        "\n#### {name}\n\nThe historical root key signed this {what} in namespace `{namespace}`. Its exact bytes:\n\n```json\n{json}```\n\nIts signature, `{sig_name}`:\n\n```text\n{sig}```\n\nVerify it offline with the root key:\n\n```sh\n{block}\n```\n",
        name = record.name,
        sig_name = record.sig_name(),
    ))
}

#[cfg(test)]
mod tests {
    //! Parser, order and selection pins.
    //!
    //! The cryptographic cases use only the public vectors in `fixtures/release-keys/receiver-v1`:
    //! the original v0.35.0 manifest and its root signature, and the installed Ed25519 vector that
    //! signs `testing` in namespace `example` (its keys are not release authority). No test makes
    //! a key or a signature.
    //!
    //! Every machine transition or revocation below is STRUCTURAL: [`Structural`] stands in for
    //! the root check, so those cases prove the parser, the bounds and the order only. Positive
    //! cryptographic acceptance of a machine statement stays UNVERIFIED until the
    //! founder-controlled rehearsal or the real endorsement supplies signed vectors.
    use super::*;
    use std::cell::RefCell;

    const ID_KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti";
    const ID_FP: &str = "SHA256:UCUiLr7Pjs9wFFJMDByLgc3NrtdU344OgUM45wZPcIQ";
    const RDY_KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIA11UHVOCACl0jfu9YJgNXZrmz5aFYaKlAqyiZWHiOOw";
    const RDY_FP: &str = "SHA256:8ZCuVRkvejTusxtyIn0cmdknpX24x3ZTBvaclwcO8tg";
    /// The placeholder every structural record carries: it is not an SSHSIG.
    const MOCK_SIG: &[u8] = b"STRUCTURAL MOCK: NOT AN SSHSIG\n";
    /// A synthetic evidence policy: its top level only, with placeholder nested values.
    const EVIDENCE: &[u8] = b"{\"approval_channel\":null,\"coordinators\":[],\"evidence_workflow\":{},\"release_workflow_path\":\".github/workflows/release.yml\",\"repository_id\":1403751388,\"schema\":1}\n";

    /// STRUCTURAL MOCK: accepts every record without any cryptography.
    struct Structural;
    impl StatementCheck for Structural {
        fn check(&self, _: &str, _: &[u8], _: &[u8]) -> bool {
            true
        }
    }

    /// Refuses every record.
    struct RefuseAll;
    impl StatementCheck for RefuseAll {
        fn check(&self, _: &str, _: &[u8], _: &[u8]) -> bool {
            false
        }
    }

    /// STRUCTURAL MOCK that records each call's namespace, statement and signature.
    #[derive(Default)]
    struct Recording(RefCell<Vec<Call>>);
    /// One check call: its namespace, statement and signature.
    type Call = (String, Vec<u8>, Vec<u8>);
    /// Records by name, with their JSON bytes.
    type Records = Vec<(String, Vec<u8>)>;
    impl StatementCheck for Recording {
        fn check(&self, namespace: &str, statement: &[u8], signature: &[u8]) -> bool {
            self.0.borrow_mut().push((
                namespace.to_string(),
                statement.to_vec(),
                signature.to_vec(),
            ));
            true
        }
    }

    fn fixture(path: &str) -> Vec<u8> {
        let full = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/release-keys/receiver-v1")
            .join(path);
        fs::read(&full).unwrap_or_else(|e| panic!("{}: {e}", full.display()))
    }

    fn real_manifest() -> (Vec<u8>, Vec<u8>) {
        (
            fixture("real-v0.35.0/SHA256SUMS"),
            fixture("real-v0.35.0/SHA256SUMS.sig"),
        )
    }

    /// A transition in the receiver fixtures' serialization.
    fn transition(minor: u32, key: &str, fp: &str) -> Vec<u8> {
        format!("{{\n  \"fingerprint\": \"{fp}\",\n  \"first_machine_tag\": \"v0.{minor}.0\",\n  \"public_key\": \"{key}\",\n  \"repository\": \"adrenickchrome/rdy-core\",\n  \"scheme\": \"rdy-release-signer-transition-v1\"\n}}\n").into_bytes()
    }

    /// A revocation in the receiver fixtures' serialization.
    fn revocation(minor: u32, fp: &str, last_good: Option<u32>) -> Vec<u8> {
        let last = last_good.map_or_else(|| "null".to_string(), |l| format!("\"v0.{l}.0\""));
        format!("{{\n  \"fingerprint\": \"{fp}\",\n  \"first_machine_tag\": \"v0.{minor}.0\",\n  \"last_good_tag\": {last},\n  \"repository\": \"adrenickchrome/rdy-core\",\n  \"scheme\": \"rdy-release-signer-revocation-v1\"\n}}\n").into_bytes()
    }

    fn t_name(minor: u32) -> String {
        format!("transition-v0.{minor}.0.json")
    }

    fn r_name(minor: u32) -> String {
        format!("revocation-v0.{minor}.0.json")
    }

    /// A synthetic policy for the machine key `fp` from `first`, bound to these bytes.
    fn policy(fp: &str, first: u32, records: &[u8], evidence: &[u8]) -> Vec<u8> {
        format!(
            "{{\"evidence_policy_sha256\":\"{}\",\"fingerprint\":\"{fp}\",\"first_machine_tag\":\"v0.{first}.0\",\"key_arn\":\"arn:aws:kms:us-east-1:000000000000:key/00000000-0000-0000-0000-000000000000\",\"records_sha256\":\"{}\",\"repository\":\"adrenickchrome/rdy-core\",\"role_arn\":\"arn:aws:iam::000000000000:role/synthetic-test-role\",\"schema\":1,\"signer_workflow_sha\":\"0000000000000000000000000000000000000000\",\"targets\":[\"aarch64-apple-darwin\",\"x86_64-unknown-linux-gnu\"]}}\n",
            sha256_hex(evidence),
            sha256_hex(records)
        )
        .into_bytes()
    }

    /// A temporary `release-keys/` directory.
    struct Reg(tempfile::TempDir);

    impl Reg {
        /// The records with structural signatures and their generated index; no policy.
        fn new(records: &[(String, Vec<u8>)]) -> Reg {
            let reg = Reg(tempfile::tempdir().expect("tempdir"));
            for (name, json) in records {
                reg.write(name, json);
                reg.write(&format!("{name}.sig"), MOCK_SIG);
            }
            reg.write(
                RECORDS_FILE,
                records_index(records.iter().map(|(n, _)| n.as_str())).as_bytes(),
            );
            reg
        }

        /// [`Reg::new`] with the synthetic evidence policy and a policy for `fp` from `first`.
        fn with_policy(records: &[(String, Vec<u8>)], fp: &str, first: u32) -> Reg {
            let reg = Reg::new(records);
            reg.bind(fp, first);
            reg
        }

        fn bind(&self, fp: &str, first: u32) {
            self.write(EVIDENCE_POLICY_FILE, EVIDENCE);
            let records = self.read(RECORDS_FILE);
            self.write(POLICY_FILE, &policy(fp, first, &records, EVIDENCE));
        }

        fn path(&self) -> &Path {
            self.0.path()
        }

        fn write(&self, name: &str, bytes: &[u8]) {
            fs::write(self.path().join(name), bytes).expect("write");
        }

        fn read(&self, name: &str) -> Vec<u8> {
            fs::read(self.path().join(name)).expect("read")
        }

        fn remove(&self, name: &str) {
            fs::remove_file(self.path().join(name)).expect("remove");
        }

        /// Loads with the structural mock.
        fn load(&self) -> Result<ReleaseKeys, TrustError> {
            ReleaseKeys::load_checked_by(self.path(), &Structural)
        }
    }

    /// The three unendorsed receiver records: key 1 from v0.40.0, revoked after v0.41.0, then
    /// key 2 from v0.44.0.
    fn unendorsed_records() -> Records {
        [
            "revocation-v0.40.0.json",
            "transition-v0.40.0.json",
            "transition-v0.44.0.json",
        ]
        .iter()
        .map(|n| (n.to_string(), fixture(&format!("unendorsed-schema/{n}"))))
        .collect()
    }

    fn unendorsed() -> Reg {
        Reg::with_policy(&unendorsed_records(), RDY_FP, 44)
    }

    fn refusal(r: Result<ReleaseKeys, TrustError>) -> String {
        r.expect_err("refused").to_string()
    }

    #[test]
    fn rsig2_versions_choose_exactly_one_key() {
        // the pinned root is the historical file and fingerprint
        assert_eq!(
            ROOT_ALLOWED_SIGNERS.as_bytes(),
            fixture("real-v0.35.0/allowed-signers")
        );
        let root = root_key().expect("root key");
        assert_eq!(
            root.fingerprint(HashAlg::Sha256).to_string(),
            ROOT_FINGERPRINT
        );
        assert_eq!(root.algorithm(), Algorithm::SkEd25519);
        assert!(
            RELEASE_TARGETS
                .iter()
                .all(|t| release_install::TARGETS.contains(t))
        );

        let h = ReleaseKeys::historical();
        for v in ["0.17.0", "0.20.0", "0.35.0"] {
            let s = h.signer_for(v).expect(v);
            assert_eq!(s.fingerprint, ROOT_FINGERPRINT);
            assert_eq!(format!("{}\n", s.signer_line), ROOT_ALLOWED_SIGNERS);
            assert!(s.historical && !s.revoked);
            assert_eq!(s.transition_sha256, None);
            assert_eq!(s.version, v);
        }
        for v in ["0.1.0", "0.16.0", "0.36.0", "0.4294967295.0"] {
            assert_eq!(
                h.signer_for(v),
                Err(TrustError::NoTrust(v.to_string())),
                "{v}"
            );
        }
        for v in [
            "v0.35.0",
            "0.35.1",
            "0.35.0-rc.1",
            "1.0.0",
            "1.35.0",
            "0.035.0",
            "0.0.0",
            "0.4294967296.0",
            "0.35",
            "",
            "0.+35.0",
        ] {
            assert_eq!(h.signer_for(v), Err(TrustError::NotVersion), "{v:?}");
        }
        assert_eq!(
            TrustError::NoTrust("0.36.0".into()).to_string(),
            "release trust: no release key for 0.36.0"
        );

        // structural epochs: key 1 for 40-41, key 2 from 44, nothing in the gaps
        let keys = unendorsed().load().expect("structural load");
        let expect = |minor: u32| match minor {
            17..=35 => Some(ROOT_FINGERPRINT),
            40 | 41 => Some(ID_FP),
            44.. => Some(RDY_FP),
            _ => None,
        };
        for minor in (1..=120).chain([4_294_967_295]) {
            let v = format!("0.{minor}.0");
            let covering = keys.epochs().iter().filter(|e| e.covers(minor)).count();
            assert!(covering <= 1, "{v}: {covering} epochs");
            match expect(minor) {
                Some(fp) => assert_eq!(keys.signer_for(&v).expect(&v).fingerprint, fp, "{v}"),
                None => assert_eq!(keys.signer_for(&v), Err(TrustError::NoTrust(v.clone()))),
            }
        }
        let s = keys.signer_for("0.44.0").unwrap();
        assert_eq!(
            s.signer_line,
            format!("rdy-release namespaces=\"rdy-release-v1\" {RDY_KEY}")
        );
        assert_eq!(
            s.transition_sha256.as_deref(),
            Some(sha256_hex(&fixture("unendorsed-schema/transition-v0.44.0.json")).as_str())
        );
        // the public root is not a release signer for new signing
        for k in [&h, &keys] {
            assert_eq!(
                k.eligible_signer("0.35.0"),
                Err(TrustError::Retired("0.35.0".into()))
            );
        }
        assert_eq!(keys.eligible_signer("0.44.0").unwrap().fingerprint, RDY_FP);

        // real cryptography: the original v0.35.0 manifest under the root
        let (m, sig) = real_manifest();
        for k in [&h, &keys] {
            assert_eq!(
                k.verify_release("0.35.0", &m, &sig).unwrap().fingerprint,
                ROOT_FINGERPRINT
            );
        }
        let mut altered = m.clone();
        altered[0] ^= 1;
        assert_eq!(
            h.verify_release("0.35.0", &altered, &sig),
            Err(TrustError::BadSignature("0.35.0".into()))
        );
        assert_eq!(
            h.verify_release("0.36.0", &m, &sig),
            Err(TrustError::NoTrust("0.36.0".into()))
        );
        // a valid signature by another epoch's key is refused before anything else
        assert_eq!(
            keys.verify_release("0.40.0", &m, &sig),
            Err(TrustError::WrongKey("0.40.0".into()))
        );
        let (message, synthetic) = (
            fixture("installed-synthetic/message.txt"),
            fixture("installed-synthetic/sshsig_ed25519"),
        );
        assert_eq!(
            keys.verify_release("0.44.0", &message, &synthetic),
            Err(TrustError::WrongKey("0.44.0".into()))
        );
        // the right key in the wrong domain
        assert_eq!(
            keys.verify_release("0.40.0", &message, &synthetic),
            Err(TrustError::BadSignature("0.40.0".into()))
        );
        assert_eq!(
            h.verify_release("0.35.0", &[b'0'; 1025], &sig),
            Err(TrustError::TooLarge("SHA256SUMS".into(), 1024))
        );
        assert_eq!(
            h.verify_release("0.35.0", &m, &[b'0'; 4097]),
            Err(TrustError::TooLarge("SHA256SUMS.sig".into(), 4096))
        );
        assert_eq!(
            TrustError::WrongKey("0.40.0".into()).to_string(),
            "release trust: the signature is not by the key for 0.40.0"
        );

        // the mechanism, with the installed vector in its own namespace
        let id = PublicKey::from_openssh(ID_KEY).unwrap();
        let rdy = PublicKey::from_openssh(RDY_KEY).unwrap();
        assert_eq!(sshsig_check(&id, "example", &message, &synthetic), Ok(()));
        assert_eq!(
            sshsig_check(&id, "example", b"testing!", &synthetic),
            Err(SigFault::Invalid)
        );
        assert_eq!(
            sshsig_check(&id, MANIFEST_NAMESPACE, &message, &synthetic),
            Err(SigFault::Invalid)
        );
        assert_eq!(
            sshsig_check(&rdy, "example", &message, &synthetic),
            Err(SigFault::OtherKey)
        );
        assert_eq!(
            sshsig_check(&id, "example", &message, b"not armored"),
            Err(SigFault::Malformed)
        );
    }

    #[test]
    fn rsig2_transition_bytes_and_namespaces_are_exact() {
        let root = RootCheck;
        let (m, sig) = real_manifest();
        // the real root signature is valid in its own namespace...
        assert_eq!(
            sshsig_check(&root_key().unwrap(), MANIFEST_NAMESPACE, &m, &sig),
            Ok(())
        );
        // ...and the statement check refuses it in every other one, and refuses the manifest
        // namespace and the superseded name outright
        for ns in [
            TRANSITION_NAMESPACE,
            REVOCATION_NAMESPACE,
            MANIFEST_NAMESPACE,
            SUPERSEDED_NAMESPACE,
        ] {
            assert!(!root.check(ns, &m, &sig), "{ns}");
        }
        // another key's valid signature is no root statement
        let synthetic = fixture("installed-synthetic/sshsig_ed25519");
        assert!(!root.check(TRANSITION_NAMESPACE, b"testing", &synthetic));

        // the unendorsed records carry no root signature: the real check refuses the first
        let reg = unendorsed();
        assert_eq!(
            refusal(ReleaseKeys::load(reg.path())),
            "release trust: revocation-v0.40.0.json: the root signature does not verify"
        );
        // the signature is checked before the bytes are parsed
        let bad = Reg::new(&[(t_name(40), b"{not json".to_vec())]);
        assert_eq!(
            ReleaseKeys::load_checked_by(bad.path(), &RefuseAll),
            Err(TrustError::StatementSignature(t_name(40)))
        );
        assert_eq!(
            refusal(bad.load()),
            "release trust: transition-v0.40.0.json: malformed JSON"
        );

        // each record is checked in its own namespace over its exact bytes
        let rec = Recording::default();
        ReleaseKeys::load_checked_by(reg.path(), &rec).expect("structural load");
        let calls = rec.0.into_inner();
        let want: Vec<Call> = unendorsed_records()
            .into_iter()
            .map(|(n, json)| {
                let ns = if n.starts_with("transition") {
                    TRANSITION_NAMESPACE
                } else {
                    REVOCATION_NAMESPACE
                };
                (ns.to_string(), json, MOCK_SIG.to_vec())
            })
            .collect();
        assert_eq!(calls, want);

        // whitespace and key order are authenticated as supplied: no reserialization
        let compact = format!(
            "{{\"scheme\":\"{TRANSITION_NAMESPACE}\",\"repository\":\"{REPOSITORY}\",\"first_machine_tag\":\"v0.40.0\",\"public_key\":\"{ID_KEY}\",\"fingerprint\":\"{ID_FP}\"}}"
        );
        let reg = Reg::with_policy(&[(t_name(40), compact.clone().into_bytes())], ID_FP, 40);
        let keys = reg.load().expect("compact");
        let t = keys.epochs()[1].transition.as_ref().unwrap();
        assert_eq!(t.json, compact.as_bytes());
        assert_eq!(t.sha256, sha256_hex(compact.as_bytes()));
        assert_ne!(t.sha256, sha256_hex(&transition(40, ID_KEY, ID_FP)));

        // the closed schema, each refused with its exact reason (structural)
        let good = String::from_utf8(transition(40, ID_KEY, ID_FP)).unwrap();
        let swap = |from: &str, to: &str| {
            assert!(good.contains(from), "{from}");
            good.replacen(from, to, 1).into_bytes()
        };
        let key_b64 = ID_KEY.strip_prefix("ssh-ed25519 ").unwrap();
        let cases: Vec<(Vec<u8>, &str)> = vec![
            (
                swap("{\n", "{\n  \"comment\": \"x\",\n"),
                "unknown field 'comment'",
            ),
            (
                swap("\"fingerprint\"", "\"fingerprnt\""),
                "unknown field 'fingerprnt'; did you mean 'fingerprint'?",
            ),
            (
                swap("{\n", &format!("{{\n  \"fingerprint\": \"{ID_FP}\",\n")),
                "duplicate field 'fingerprint'",
            ),
            (
                swap(&format!("  \"public_key\": \"{ID_KEY}\",\n"), ""),
                "missing field 'public_key'",
            ),
            (
                [b"\xef\xbb\xbf".as_slice(), good.as_bytes()].concat(),
                "starts with a byte order mark",
            ),
            (
                {
                    let mut bytes = good.clone().into_bytes();
                    bytes.insert(20, 0xff);
                    bytes
                },
                "not UTF-8",
            ),
            (b"{\"scheme\":".to_vec(), "malformed JSON"),
            (b"{\"scheme\": NaN}".to_vec(), "malformed JSON"),
            (b"{\"scheme\": Infinity}".to_vec(), "malformed JSON"),
            (b"[]".to_vec(), "not a JSON object"),
            (b"\"transition\"".to_vec(), "not a JSON object"),
            (
                swap(TRANSITION_NAMESPACE, SUPERSEDED_NAMESPACE),
                "field 'scheme' is invalid",
            ),
            (
                swap(TRANSITION_NAMESPACE, REVOCATION_NAMESPACE),
                "field 'scheme' is invalid",
            ),
            (
                swap(REPOSITORY, "rdyhq/rdy-core"),
                "field 'repository' is invalid",
            ),
            (
                swap("\"v0.40.0\"", "\"v0.41.0\""),
                "first_machine_tag is not the file name's tag",
            ),
            (
                swap("\"v0.40.0\"", "\"v0.040.0\""),
                "first_machine_tag is not the file name's tag",
            ),
            (
                swap("\"v0.40.0\"", "40"),
                "field 'first_machine_tag' is invalid",
            ),
            (
                swap(key_b64, &format!("{}=", &key_b64[..67])),
                "field 'public_key' is invalid",
            ),
            (
                swap(key_b64, &key_b64[..67]),
                "field 'public_key' is invalid",
            ),
            (
                swap(key_b64, &format!("{key_b64} user@example.com")),
                "field 'public_key' is invalid",
            ),
            (
                swap("ssh-ed25519 ", "ssh-ed25519  "),
                "field 'public_key' is invalid",
            ),
            (
                swap(key_b64, &format!("{key_b64}\\n")),
                "field 'public_key' is invalid",
            ),
            (
                swap(key_b64, &format!("{key_b64} {key_b64}")),
                "field 'public_key' is invalid",
            ),
            (
                swap("ssh-ed25519 ", "sk-ssh-ed25519@openssh.com "),
                "field 'public_key' is invalid",
            ),
            (
                swap("ssh-ed25519 ", "ssh-ed25519-cert-v01@openssh.com "),
                "field 'public_key' is invalid",
            ),
            (
                swap(&format!("\"{ID_KEY}\""), "[]"),
                "field 'public_key' is invalid",
            ),
            (swap(ID_FP, RDY_FP), "fingerprint is not the public key's"),
            (swap(ID_FP, "SHA256:abc"), "field 'fingerprint' is invalid"),
            (
                swap(&format!("\"{ID_FP}\""), "1"),
                "field 'fingerprint' is invalid",
            ),
        ];
        for (json, why) in cases {
            let reg = Reg::new(&[(t_name(40), json)]);
            assert_eq!(
                refusal(reg.load()),
                format!("release trust: transition-v0.40.0.json: {why}")
            );
        }
    }

    #[test]
    fn rsig2_revocation_preserves_only_last_good_versions() {
        // last good v0.41.0: archival verification keeps 40 and 41, new signing refuses both
        let keys = unendorsed().load().expect("structural load");
        for v in ["0.40.0", "0.41.0"] {
            let s = keys.signer_for(v).unwrap();
            assert_eq!(s.fingerprint, ID_FP);
            assert!(s.revoked && !s.historical);
            assert_eq!(keys.eligible_signer(v), Err(TrustError::Revoked(v.into())));
        }
        assert_eq!(
            TrustError::Revoked("0.40.0".into()).to_string(),
            "release trust: the key for 0.40.0 is revoked for new signing"
        );
        for v in ["0.42.0", "0.43.0"] {
            assert_eq!(keys.signer_for(v), Err(TrustError::NoTrust(v.into())));
        }

        // a null last-good tag: no good release, no row, and the successor starts after its tag
        let null = Reg::with_policy(
            &[
                (t_name(40), transition(40, ID_KEY, ID_FP)),
                (r_name(40), revocation(40, ID_FP, None)),
            ],
            ID_FP,
            40,
        );
        let keys = null.load().expect("null revocation");
        assert_eq!(keys.epochs()[1].last, Some(39));
        assert_eq!(
            keys.signer_for("0.40.0"),
            Err(TrustError::NoTrust("0.40.0".into()))
        );
        assert_eq!(
            keys.eligible_signer("0.40.0"),
            Err(TrustError::NoTrust("0.40.0".into()))
        );
        assert_eq!(
            keys.versions_txt(),
            ReleaseKeys::historical().versions_txt()
        );
        let next = Reg::with_policy(
            &[
                (t_name(40), transition(40, ID_KEY, ID_FP)),
                (r_name(40), revocation(40, ID_FP, None)),
                (t_name(41), transition(41, RDY_KEY, RDY_FP)),
            ],
            RDY_FP,
            41,
        );
        let keys = next.load().expect("successor after null");
        assert_eq!(
            keys.signer_for("0.40.0"),
            Err(TrustError::NoTrust("0.40.0".into()))
        );
        assert_eq!(keys.eligible_signer("0.41.0").unwrap().fingerprint, RDY_FP);

        // order rules, each refused at the record that breaks it
        let t40 = || (t_name(40), transition(40, ID_KEY, ID_FP));
        let cases: Vec<(Records, String, &str)> = vec![
            (
                vec![t40(), (t_name(44), transition(44, RDY_KEY, RDY_FP))],
                t_name(44),
                "the previous epoch has no revocation",
            ),
            (
                vec![
                    t40(),
                    (r_name(40), revocation(40, ID_FP, Some(44))),
                    (t_name(44), transition(44, RDY_KEY, RDY_FP)),
                ],
                t_name(44),
                "starts at or before the previous epoch's first or last good tag",
            ),
            (
                vec![
                    t40(),
                    (r_name(40), revocation(40, ID_FP, Some(41))),
                    (t_name(44), transition(44, ID_KEY, ID_FP)),
                ],
                t_name(44),
                "reuses an enrolled key",
            ),
            (
                vec![(t_name(35), transition(35, ID_KEY, ID_FP))],
                t_name(35),
                "the first machine epoch starts after v0.35.0",
            ),
            (
                vec![(t_name(17), transition(17, ID_KEY, ID_FP))],
                t_name(17),
                "the first machine epoch starts after v0.35.0",
            ),
            (
                vec![t40(), (r_name(40), revocation(40, RDY_FP, Some(41)))],
                r_name(40),
                "fingerprint is not the active epoch's key",
            ),
            (
                vec![t40(), (r_name(41), revocation(41, ID_FP, Some(41)))],
                r_name(41),
                "no active machine epoch starts at this tag",
            ),
            (
                vec![(r_name(40), revocation(40, ID_FP, Some(41)))],
                r_name(40),
                "no active machine epoch starts at this tag",
            ),
            (
                vec![t40(), (r_name(40), revocation(40, ID_FP, Some(39)))],
                r_name(40),
                "last_good_tag is before first_machine_tag",
            ),
            (
                vec![
                    t40(),
                    (
                        r_name(40),
                        String::from_utf8(revocation(40, ID_FP, Some(41)))
                            .unwrap()
                            .replace("\"v0.41.0\"", "\"v0.41.1\"")
                            .into_bytes(),
                    ),
                ],
                r_name(40),
                "field 'last_good_tag' is invalid",
            ),
            (
                vec![
                    t40(),
                    (
                        r_name(40),
                        String::from_utf8(revocation(40, ID_FP, Some(41)))
                            .unwrap()
                            .replace("\"v0.41.0\"", "41")
                            .into_bytes(),
                    ),
                ],
                r_name(40),
                "field 'last_good_tag' is invalid",
            ),
            (
                vec![
                    t40(),
                    (
                        r_name(40),
                        String::from_utf8(revocation(40, ID_FP, Some(41)))
                            .unwrap()
                            .replace("  \"last_good_tag\": \"v0.41.0\",\n", "")
                            .into_bytes(),
                    ),
                ],
                r_name(40),
                "missing field 'last_good_tag'",
            ),
        ];
        for (records, at, why) in cases {
            assert_eq!(
                refusal(Reg::new(&records).load()),
                format!("release trust: {at}: {why}"),
                "{at}: {why}"
            );
        }
        // the first machine tag after the root's range is accepted
        let first = Reg::with_policy(&[(t_name(36), transition(36, ID_KEY, ID_FP))], ID_FP, 36);
        assert_eq!(
            first
                .load()
                .unwrap()
                .eligible_signer("0.36.0")
                .unwrap()
                .fingerprint,
            ID_FP
        );
    }

    #[test]
    fn rsig2_bad_or_missing_policy_fails_closed() {
        let records = unendorsed_records();
        let reg = Reg::new(&records);
        assert_eq!(refusal(reg.load()), "release trust: policy.json is missing");
        reg.write(EVIDENCE_POLICY_FILE, EVIDENCE);
        assert_eq!(reg.load(), Err(TrustError::PolicyMissing));
        reg.bind(RDY_FP, 44);
        reg.remove(EVIDENCE_POLICY_FILE);
        assert_eq!(
            refusal(reg.load()),
            "release trust: evidence-policy.json is missing"
        );
        reg.bind(RDY_FP, 44);
        let keys = reg.load().expect("bound policy");
        let p = keys.policy().unwrap();
        assert_eq!(p.fingerprint, RDY_FP);
        assert_eq!(p.records_sha256, sha256_hex(&reg.read(RECORDS_FILE)));
        assert_eq!(p.sha256, sha256_hex(&reg.read(POLICY_FILE)));

        let index = reg.read(RECORDS_FILE);
        let good = String::from_utf8(policy(RDY_FP, 44, &index, EVIDENCE)).unwrap();
        let swap = |from: &str, to: &str| {
            assert!(good.contains(from), "{from}");
            good.replacen(from, to, 1)
        };
        let cases: Vec<(String, &str)> = vec![
            (
                String::from_utf8(policy(RDY_FP, 44, b"other", EVIDENCE)).unwrap(),
                "records_sha256 does not match records.txt",
            ),
            (
                String::from_utf8(policy(RDY_FP, 44, &index, b"other")).unwrap(),
                "evidence_policy_sha256 does not match evidence-policy.json",
            ),
            (
                String::from_utf8(policy(ID_FP, 44, &index, EVIDENCE)).unwrap(),
                "fingerprint is not the current machine key",
            ),
            (
                String::from_utf8(policy(RDY_FP, 40, &index, EVIDENCE)).unwrap(),
                "first_machine_tag is not the current machine epoch's",
            ),
            (
                swap(
                    "\"x86_64-unknown-linux-gnu\"]",
                    "\"x86_64-unknown-linux-gnu\",\"x86_64-pc-windows-msvc\"]",
                ),
                "targets are not the approved target set",
            ),
            (
                swap(
                    "[\"aarch64-apple-darwin\",\"x86_64-unknown-linux-gnu\"]",
                    "[\"x86_64-unknown-linux-gnu\",\"aarch64-apple-darwin\"]",
                ),
                "targets are not the approved target set",
            ),
            (
                swap(
                    "[\"aarch64-apple-darwin\",\"x86_64-unknown-linux-gnu\"]",
                    "\"aarch64-apple-darwin\"",
                ),
                "field 'targets' is invalid",
            ),
            (
                swap("\"key_arn\"", "\"key_arm\""),
                "unknown field 'key_arm'; did you mean 'key_arn'?",
            ),
            (swap("{", "{\"note\":\"x\","), "unknown field 'note'"),
            (swap("{", "{\"schema\":1,"), "duplicate field 'schema'"),
            (
                swap(
                    "\"role_arn\":\"arn:aws:iam::000000000000:role/synthetic-test-role\",",
                    "",
                ),
                "missing field 'role_arn'",
            ),
            (
                swap("\"schema\":1", "\"schema\":2"),
                "field 'schema' is invalid",
            ),
            (
                swap("\"schema\":1", "\"schema\":\"1\""),
                "field 'schema' is invalid",
            ),
            (
                swap("\"schema\":1", "\"schema\":1.0"),
                "field 'schema' is invalid",
            ),
            (
                swap(
                    "\"repository\":\"adrenickchrome/rdy-core\"",
                    "\"repository\":\"x/y\"",
                ),
                "field 'repository' is invalid",
            ),
            (swap("arn:aws:kms:", "kms:"), "field 'key_arn' is invalid"),
            (
                swap("arn:aws:iam::", "arn:aws:kms:"),
                "field 'role_arn' is invalid",
            ),
            (swap("key/0000", "key/ 0000"), "field 'key_arn' is invalid"),
            (
                swap(
                    "\"0000000000000000000000000000000000000000\"",
                    "\"000000000000000000000000000000000000000\"",
                ),
                "field 'signer_workflow_sha' is invalid",
            ),
            (
                swap(
                    "\"0000000000000000000000000000000000000000\"",
                    "\"000000000000000000000000000000000000000A\"",
                ),
                "field 'signer_workflow_sha' is invalid",
            ),
            ("{\"schema\":".to_string(), "malformed JSON"),
            ("[]".to_string(), "not a JSON object"),
        ];
        for (bytes, why) in cases {
            reg.write(POLICY_FILE, bytes.as_bytes());
            assert_eq!(
                refusal(reg.load()),
                format!("release trust: policy.json: {why}"),
                "{why}"
            );
        }
        reg.bind(RDY_FP, 44);
        // the evidence policy's top level is closed and pinned
        let evidence = String::from_utf8(EVIDENCE.to_vec()).unwrap();
        for (bytes, why) in [
            (
                evidence.replacen("{", "{\"extra\":1,", 1),
                "unknown field 'extra'",
            ),
            (
                evidence.replacen("\"schema\":1", "\"schema\":2", 1),
                "field 'schema' is invalid",
            ),
            (
                evidence.replacen("1403751388", "1403751389", 1),
                "field 'repository_id' is invalid",
            ),
            (
                evidence.replacen("release.yml", "other.yml", 1),
                "field 'release_workflow_path' is invalid",
            ),
            (
                evidence.replacen("\"approval_channel\":null,", "", 1),
                "missing field 'approval_channel'",
            ),
        ] {
            reg.write(EVIDENCE_POLICY_FILE, bytes.as_bytes());
            assert_eq!(
                refusal(reg.load()),
                format!("release trust: evidence-policy.json: {why}"),
                "{why}"
            );
        }
        reg.bind(RDY_FP, 44);
        reg.write(POLICY_FILE, &vec![b' '; 65537]);
        assert_eq!(
            refusal(reg.load()),
            "release trust: policy.json is over 65536 bytes"
        );

        // a policy with no machine epoch names no key
        let empty = Reg::new(&[]);
        empty.bind(RDY_FP, 44);
        assert_eq!(
            refusal(empty.load()),
            "release trust: policy.json: no machine epoch is enrolled"
        );
        // with no record and no control file, only the historical root remains
        let empty = Reg::new(&[]);
        let keys = empty.load().expect("empty registry");
        assert_eq!(keys, ReleaseKeys::historical());
        assert_eq!(
            keys.eligible_signer("0.36.0"),
            Err(TrustError::NoTrust("0.36.0".into()))
        );
    }

    #[test]
    fn rsig2_trust_generator_round_trips_public_records() {
        // the fixture serialization is the builders'
        assert_eq!(
            transition(40, ID_KEY, ID_FP),
            fixture("unendorsed-schema/transition-v0.40.0.json")
        );
        assert_eq!(
            transition(44, RDY_KEY, RDY_FP),
            fixture("unendorsed-schema/transition-v0.44.0.json")
        );
        assert_eq!(
            revocation(40, ID_FP, Some(41)),
            fixture("unendorsed-schema/revocation-v0.40.0.json")
        );
        // the index of the receiver's example, byte for byte
        let expected = fixture("unendorsed-schema/expected-records.txt");
        let records = unendorsed_records();
        assert_eq!(
            records_index(records.iter().map(|(n, _)| n.as_str())).as_bytes(),
            expected
        );
        let reg = unendorsed();
        assert_eq!(generated_records(reg.path()).unwrap().as_bytes(), expected);
        let keys = reg.load().unwrap();
        assert_eq!(keys.records_txt().as_bytes(), expected);
        assert_eq!(records_index([]), "");

        // the receiver's published files: the historical table and the root file (their digests
        // are in the receiver contract)
        let historical = ReleaseKeys::historical();
        assert_eq!(
            sha256_hex(historical.versions_txt().as_bytes()),
            "028019de9b6f9b2c9f789a291600449a97ed9eab2cf3a34c3b80e32e2cdc3f0b"
        );
        assert_eq!(
            sha256_hex(ROOT_ALLOWED_SIGNERS.as_bytes()),
            "da1570f3a3918c7b57c8cc9f1042a61d064e5c659ac8bb3ebfffc4e33ec426ca"
        );
        let id_b64 = ID_KEY.strip_prefix("ssh-ed25519 ").unwrap();
        let rdy_b64 = RDY_KEY.strip_prefix("ssh-ed25519 ").unwrap();
        let versions = format!(
            "17 35 {ROOT_KEY_TYPE} {ROOT_KEY_BASE64} {ROOT_FINGERPRINT}\n40 41 ssh-ed25519 {id_b64} {ID_FP}\n44 * ssh-ed25519 {rdy_b64} {RDY_FP}\n"
        );
        assert_eq!(keys.versions_txt(), versions);
        // generated files round trip; a changed copy refuses
        reg.write(VERSIONS_FILE, versions.as_bytes());
        reg.write(ROOT_FILE, ROOT_ALLOWED_SIGNERS.as_bytes());
        reg.write(README_FILE, b"Public release keys.\n");
        assert_eq!(reg.load().unwrap(), keys);
        reg.write(VERSIONS_FILE, versions.replace("44 *", "44 45").as_bytes());
        assert_eq!(
            refusal(reg.load()),
            "release trust: versions.txt does not match the records"
        );
        reg.write(VERSIONS_FILE, versions.as_bytes());
        reg.write(
            ROOT_FILE,
            ROOT_ALLOWED_SIGNERS
                .replace("rdy-release-v1", TRANSITION_NAMESPACE)
                .as_bytes(),
        );
        assert_eq!(
            refusal(reg.load()),
            "release trust: root-allowed-signers is not the pinned root"
        );

        // minor 100 sorts before 99 in the index; the epochs still follow the numbers
        let hundred = Reg::with_policy(
            &[
                (t_name(99), transition(99, ID_KEY, ID_FP)),
                (r_name(99), revocation(99, ID_FP, Some(99))),
                (t_name(100), transition(100, RDY_KEY, RDY_FP)),
            ],
            RDY_FP,
            100,
        );
        assert_eq!(
            String::from_utf8(hundred.read(RECORDS_FILE)).unwrap(),
            "revocation-v0.99.0.json revocation-v0.99.0.json.sig\ntransition-v0.100.0.json transition-v0.100.0.json.sig\ntransition-v0.99.0.json transition-v0.99.0.json.sig\n"
        );
        let keys = hundred.load().expect("99 and 100");
        assert_eq!(
            keys.versions_txt(),
            format!(
                "17 35 {ROOT_KEY_TYPE} {ROOT_KEY_BASE64} {ROOT_FINGERPRINT}\n99 99 ssh-ed25519 {id_b64} {ID_FP}\n100 * ssh-ed25519 {rdy_b64} {RDY_FP}\n"
            )
        );
        hundred.write(
            RECORDS_FILE,
            b"transition-v0.99.0.json transition-v0.99.0.json.sig\nrevocation-v0.99.0.json revocation-v0.99.0.json.sig\ntransition-v0.100.0.json transition-v0.100.0.json.sig\n",
        );
        assert_eq!(hundred.load(), Err(TrustError::IndexMismatch));

        // the rendered section: every key, then each statement's exact bytes and signature and
        // the offline root check in that statement's own namespace
        let section = unendorsed().load().unwrap().trust_section().unwrap();
        for part in [
            format!(
                "| v0.17.0 to v0.35.0 | `{ROOT_KEY_TYPE}` | `{ROOT_FINGERPRINT}` | historical key, retired for new releases |\n"
            ),
            format!(
                "| v0.40.0 to v0.41.0 | `ssh-ed25519` | `{ID_FP}` | machine key, revoked after v0.41.0 |\n"
            ),
            format!("| v0.44.0 and later | `ssh-ed25519` | `{RDY_FP}` | machine key, current |\n"),
            format!("```text\nrdy-release namespaces=\"rdy-release-v1\" {RDY_KEY}\n```\n"),
            format!(
                "```json\n{}```\n",
                String::from_utf8(fixture("unendorsed-schema/transition-v0.44.0.json")).unwrap()
            ),
            format!(
                "```text\n{}```\n",
                String::from_utf8(MOCK_SIG.to_vec()).unwrap()
            ),
            format!(
                "'rdy-release namespaces=\"{TRANSITION_NAMESPACE}\" {ROOT_KEY_TYPE} {ROOT_KEY_BASE64}' > '/path/to/release-keys/root-transition-allowed-signers'"
            ),
            format!(
                "'ssh-keygen' '-Y' 'verify' '-f' 'root-revocation-allowed-signers' '-I' 'rdy-release' '-n' '{REVOCATION_NAMESPACE}' '-s' 'revocation-v0.40.0.json.sig' < '/path/to/release-keys/revocation-v0.40.0.json'"
            ),
        ] {
            assert!(section.contains(&part), "missing {part}\nin\n{section}");
        }
        assert!(!section.contains("pending"));
        // before any machine record: the historical key and an honest pending state, exactly
        assert_eq!(
            historical.trust_section().unwrap(),
            format!(
                "### Release keys\n\nEach version has exactly one release key. A signature by any other key is refused.\n\n| Versions | Key type | Fingerprint | State |\n| --- | --- | --- | --- |\n| v0.17.0 to v0.35.0 | `{ROOT_KEY_TYPE}` | `{ROOT_FINGERPRINT}` | historical key, retired for new releases |\n\nNo other version has a release key. The machine release key is pending: no version after v0.35.0 has a release key yet.\n\nThe allowed-signers line for v0.17.0 to v0.35.0:\n\n```text\n{}```\n",
                ROOT_ALLOWED_SIGNERS
            )
        );
        // a statement without its final LF is not rendered as if it had one
        let compact = Reg::with_policy(
            &[(
                t_name(40),
                transition(40, ID_KEY, ID_FP).trim_ascii_end().to_vec(),
            )],
            ID_FP,
            40,
        );
        assert_eq!(
            compact.load().unwrap().trust_section(),
            Err(TrustError::Render(t_name(40)))
        );
    }

    #[test]
    fn rsig2_trust_input_bounds_and_unknown_keys() {
        let padded = |len: usize| {
            let json = transition(40, ID_KEY, ID_FP);
            let pad = vec![b' '; len - json.len()];
            [b"{".as_slice(), &pad, &json[1..]].concat()
        };
        // 4096 bytes is within the bound, 4097 is not
        let reg = Reg::with_policy(&[(t_name(40), padded(4096))], ID_FP, 40);
        assert!(reg.load().is_ok());
        reg.write(&format!("{}.sig", t_name(40)), &vec![b'S'; 4096]);
        assert!(reg.load().is_ok());
        reg.write(&format!("{}.sig", t_name(40)), &vec![b'S'; 4097]);
        assert_eq!(
            refusal(reg.load()),
            "release trust: transition-v0.40.0.json.sig is over 4096 bytes"
        );
        let reg = Reg::with_policy(&[(t_name(40), padded(4097))], ID_FP, 40);
        assert_eq!(
            reg.load(),
            Err(TrustError::TooLarge(t_name(40), STATEMENT_MAX))
        );

        // 64 pairs pass the count (and fail later); 65 refuse the whole set before any read
        let many =
            |n: u32| -> Records { (36..36 + n).map(|m| (t_name(m), b"{}".to_vec())).collect() };
        let reg = Reg::new(&many(64));
        assert_eq!(
            ReleaseKeys::load_checked_by(reg.path(), &RefuseAll),
            Err(TrustError::StatementSignature(t_name(36)))
        );
        let reg = Reg::new(&many(65));
        assert_eq!(
            refusal(reg.load()),
            "release trust: more than 64 record pairs"
        );
        assert_eq!(
            generated_records(reg.path()),
            Err(TrustError::TooManyRecords)
        );
        let reg = Reg::new(&many(68));
        assert_eq!(refusal(reg.load()), "release trust: more than 134 entries");

        // support files over their bounds
        for (name, max) in [
            (RECORDS_FILE, 65536),
            (VERSIONS_FILE, 65536),
            (README_FILE, 65536),
            (ROOT_FILE, 4096),
            (EVIDENCE_POLICY_FILE, 65536),
        ] {
            let reg = unendorsed();
            reg.write(name, &vec![b'x'; max + 1]);
            assert_eq!(
                refusal(reg.load()),
                format!("release trust: {name} is over {max} bytes")
            );
        }

        // unknown entries refuse; the index never filters them away
        for name in [
            "notes.txt",
            "transition-v0.040.0.json",
            "transition-v0.40.0.json.bak",
            "transition-v0.40.1.json",
            "transition-v1.40.0.json",
            // (no case variant: a case-insensitive file system would overwrite the record)
            "transitions-v0.40.0.json",
            "revocation-v0.40.0.sig",
            "transition-0.40.0.json",
            "transition-v0.4294967296.0.json",
            "revocation-v0.0.0.json",
            "records.txt~",
            "versions.json",
        ] {
            let reg = unendorsed();
            reg.write(name, b"{}");
            assert_eq!(
                refusal(reg.load()),
                format!("release trust: unknown entry {name}"),
                "{name}"
            );
        }
        let reg = unendorsed();
        fs::create_dir(reg.path().join(README_FILE)).unwrap();
        assert_eq!(
            refusal(reg.load()),
            "release trust: README.txt is not a regular file"
        );
        let reg = unendorsed();
        if crate::platform::symlink(
            &reg.path().join(RECORDS_FILE),
            &reg.path().join(VERSIONS_FILE),
        )
        .is_ok()
        {
            assert_eq!(
                refusal(reg.load()),
                "release trust: versions.txt is not a regular file"
            );
        }

        // pairs: both partners, in both directions
        let reg = unendorsed();
        reg.remove("transition-v0.44.0.json.sig");
        assert_eq!(
            refusal(reg.load()),
            "release trust: transition-v0.44.0.json has no partner"
        );
        let reg = unendorsed();
        reg.remove("revocation-v0.40.0.json");
        assert_eq!(
            refusal(reg.load()),
            "release trust: revocation-v0.40.0.json.sig has no partner"
        );

        // the index is the complete generated one, byte for byte
        let expected =
            String::from_utf8(fixture("unendorsed-schema/expected-records.txt")).unwrap();
        let lines: Vec<&str> = expected.lines().collect();
        for index in [
            format!("{}\n{}\n", lines[0], lines[1]),
            format!("{expected}transition-v0.48.0.json transition-v0.48.0.json.sig\n"),
            expected.trim_end().to_string(),
            expected.replace(".json ", ".json\t"),
            expected.replace(".json ", ".json  "),
            expected.replace(".sig\n", ".sig \n"),
            expected.replace(".sig\n", ".sig\r\n"),
            format!("{}\n\n{}\n{}\n", lines[0], lines[1], lines[2]),
            format!("# records\n{expected}"),
            lines
                .iter()
                .map(|l| format!("{}\n", l.split(' ').next().unwrap()))
                .collect(),
            String::new(),
        ] {
            let reg = unendorsed();
            reg.write(RECORDS_FILE, index.as_bytes());
            assert_eq!(
                refusal(reg.load()),
                "release trust: records.txt is not the index of every record",
                "{index:?}"
            );
        }
        let reg = unendorsed();
        reg.remove(RECORDS_FILE);
        assert_eq!(refusal(reg.load()), "release trust: records.txt is missing");
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            ReleaseKeys::load(empty.path()),
            Err(TrustError::MissingIndex)
        );
        assert_eq!(
            refusal(ReleaseKeys::load(&empty.path().join("absent"))),
            "release trust: cannot read release-keys"
        );
    }
}
