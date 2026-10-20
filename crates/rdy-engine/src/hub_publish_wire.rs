//! The typed publication upload protocol (D-Q0AVZ; hub D-6BXJY, i-6ZXW0): discovery, state,
//! known, objects, files and publish, over HA2a's HTTPS sender ([`HttpHubTransport::upload`]).
//!
//! - **Closed spellings.** Each request and reply is one fixed JSON object. Hashes and repo ids
//!   are lowercase text; an object key is `{kind, hash}`. Every reply object refuses unknown
//!   members, duplicate keys and wrong types, and each nullable member must be present. The one
//!   optional request member is `keep_latest`, beside `record` in the publish body and absent
//!   when not given (D-50JXR); core never computes its default.
//! - **Bounds before allocation.** Each request is checked against its count bounds and
//!   encoded through a capped writer. Each reply is read within its byte cap and walked by the
//!   bounded JSON guard, whose value budget a known step sets from its own request, before it is
//!   decoded.
//! - **Origins.** Discovery's sync base is a path on the install or an HTTPS URL the install's
//!   origin fence admits. Fixed routes are appended as path segments and the push id and hash
//!   as query pairs; no supplied text is concatenated into a URL. A redirect is never followed.
//! - **Refusals.** A hub error object keeps a recognized hub or core code with its bounded
//!   message and fix, its valid issues and a request-id issue. Any other code is the generic
//!   refusal. A body that echoes the bearer or a token prefix, or that breaks the object's
//!   bounds, is discarded.
//! - **Effects.** Discovery, state and both known steps are reads: a failed exchange is
//!   `E_UPSTREAM_TRANSPORT` and an invalid reply `E_UPSTREAM_PROTOCOL`. Objects, file and publish
//!   are writes: once the request may have been sent, a lost or invalid reply is
//!   `E_UPSTREAM_UNCERTAIN`. Nothing is retried.
use crate::error::{
    E_BAD_INPUT, E_BAD_TAG_NAME, E_UPSTREAM_REFUSED, RdyError, actor_unbound, core_error_code,
    upstream_protocol, upstream_refused, upstream_transport, upstream_uncertain,
};
use crate::formats::formats;
use crate::hub_http::{
    HttpHubTransport, JsonLimits, UploadHttpRequest, UploadMethod, api_error_entry,
    decode_bounded_json,
};
use crate::hub_types::{
    HubFailure, LimitUnit, MAX_ID_BYTES, MAX_URL_BYTES, Origin, OriginFence, RemoteUrl,
    RequestClaim, Secret, TOKEN_PREFIXES, check_count, hub_limit,
};
use crate::publication::{Audience, SelectionEntry};
use crate::publication_manifest::{Manifest, ManifestEntry, manifest_hash};
use crate::render_inputs::{RenderInputs, validate_inputs};
use rdy_model::{BlockId, Hash, RepoId};
use rdy_schema::{Issue, Severity};
use rdy_store::object_refs::ObjectKey;
use serde::de::{self, DeserializeOwned};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::io::{Cursor, Read, Write};
use std::path::PathBuf;

/// The media type a discovery request accepts.
pub const SYNC_MEDIA_TYPE: &str = "application/vnd.rdy.sync+json";
/// The sync protocol version this client speaks.
pub const SYNC_PROTOCOL: u32 = 1;
/// The push id's length: 26 lower-case Crockford base32 characters (hub D-6BXJY, D-502J1).
pub const PUSH_ID_CHARS: usize = 26;

/// Core's caps on the state reply's limits: a larger advertised value is clamped to them.
pub const CORE_BATCH_BYTES: u64 = 2_147_483_648;
pub const CORE_BATCH_RECORDS: u64 = 1_000_000;
pub const CORE_QUARANTINE_SECONDS: u64 = 31_536_000;
pub const CORE_ARTIFACT_BYTES: u64 = 2_147_483_648;
pub const CORE_ARTIFACT_FILES: u64 = 32_000;

/// The most object keys one known request names.
pub const MAX_KNOWN_KEYS: usize = 100_000;
/// The most distinct file hashes one files/known request names.
pub const MAX_KNOWN_FILES: usize = 32_000;
/// The most selection rows in a publication record.
pub const MAX_SELECTION_ROWS: usize = 100_000;
/// The most features a state reply lists, and the longest feature name.
pub const MAX_FEATURES: usize = 100_000;
pub const MAX_FEATURE_NAME_BYTES: usize = 128;
/// The longest site name.
pub const MAX_SITE_BYTES: usize = 64;
/// The bounds of a hub error object: its code, each message and fix, its issues and its wait.
pub const MAX_ERROR_CODE_BYTES: usize = 64;
pub const MAX_ERROR_TEXT_BYTES: usize = 4096;
pub const MAX_ERROR_ISSUES: usize = 200;
pub const MAX_RETRY_AFTER_SECONDS: u64 = 31_536_000;

/// Request body caps, in bytes. Discovery has no body.
pub const STATE_REQUEST_BYTES: usize = 1_048_576;
pub const KNOWN_REQUEST_BYTES: usize = 16_777_216;
pub const FILES_KNOWN_REQUEST_BYTES: usize = 8_388_608;
pub const PUBLISH_REQUEST_BYTES: usize = 268_435_456;
/// Reply body caps, in bytes: discovery, state, objects, file and publish share the first.
pub const REPLY_BYTES: usize = 1_048_576;
pub const KNOWN_REPLY_BYTES: usize = 16_777_216;
pub const FILES_KNOWN_REPLY_BYTES: usize = 8_388_608;
/// The JSON bounds of every reply: depth, scalar length and total members and elements.
pub const MAX_JSON_DEPTH: usize = 32;
pub const MAX_JSON_SCALAR_BYTES: usize = 1_048_576;
pub const MAX_JSON_VALUES: usize = 1_000_000;

/// The most stream formats or protocol versions one list holds.
const MAX_FORMATS: usize = 16;
const MAX_PROTOCOLS: usize = 64;
/// The hub codes that always carry `retry_after`.
const RETRY_CODES: [&str; 4] = [
    "E_HUB_OVER_BUDGET",
    "E_HUB_BUSY",
    "E_HUB_REPO_MOVING",
    "E_HUB_READ_ONLY",
];
/// The incompatible store features this client knows. HA1 has no feature registry, so it
/// knows none, and any incompatible feature the hub repo uses is refused (D-WWAK7).
const KNOWN_INCOMPATIBLE: [&str; 0] = [];
/// The texts of the issues a refusal appends.
const REQUEST_ID_FIX: &str = "quote this request id when you contact the operator";
const RETRY_FIX: &str = "wait before trying again";

// ---------------------------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------------------------

/// What every upload call needs: the selected install and its sync base (`None` before
/// discovery), the user CA bundle, the bound request claim and the current access token. Debug
/// never prints the bearer.
#[derive(Clone)]
pub struct UploadContext {
    pub install: Origin,
    pub sync_base: Option<String>,
    pub ca_file: Option<PathBuf>,
    pub claim: RequestClaim,
    pub bearer: Secret,
}

impl fmt::Debug for UploadContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UploadContext")
            .field("install", &self.install)
            .field("sync_base", &self.sync_base)
            .field("ca_file", &self.ca_file)
            .field("actor", &self.claim.actor)
            .field("run", &self.claim.run)
            .finish_non_exhaustive()
    }
}

/// A discovery reply: the absolute sync base and the protocol versions the install speaks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Discovery {
    pub sync_base: String,
    pub protocols: Vec<u32>,
}

/// The state step's request. `actor` and `run` must be the context claim's; a `None` run is
/// JSON null, and the request then has no `Rdy-Run` header.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StateRequest {
    pub rdy: String,
    pub stream_read: Vec<u32>,
    pub stream_write: Vec<u32>,
    pub actor: String,
    pub run: Option<String>,
    pub site: String,
}

impl StateRequest {
    /// The request of this rdy for `site` under `claim`: its version and its stream format.
    pub fn new(claim: &RequestClaim, site: &str) -> StateRequest {
        let f = formats();
        StateRequest {
            rdy: f.rdy,
            stream_read: vec![f.stream],
            stream_write: vec![f.stream],
            actor: claim.actor.clone(),
            run: claim.run.clone(),
            site: site.to_string(),
        }
    }
}

/// A profile revision: its block id and version (hub D-6BXJY). In the state reply it is hub
/// metadata; in the record it is the revision the tag pins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileIdentity {
    pub id: BlockId,
    #[serde(with = "hex")]
    pub version: Hash,
}

/// A store feature's class (D-WWAK7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureClass {
    Compatible,
    ReadOnly,
    Incompatible,
}

/// A feature the hub repo's records use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureUse {
    pub name: String,
    pub class: FeatureClass,
}

/// The state reply, validated: limits are positive and clamped to core's caps. `min_rdy` is
/// null in H1 (hub i-DETA1).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishState {
    #[serde(deserialize_with = "nullable_hash")]
    pub canon: Option<Hash>,
    #[serde(deserialize_with = "nullable_repo_id")]
    pub repo_id: Option<RepoId>,
    pub rdy: String,
    pub stream_read: Vec<u32>,
    pub stream_write: Vec<u32>,
    pub features: Vec<FeatureUse>,
    #[serde(deserialize_with = "nullable")]
    pub min_rdy: Option<String>,
    pub batch_bytes: u64,
    pub batch_records: u64,
    pub quarantine_seconds: u64,
    #[serde(deserialize_with = "nullable")]
    pub profile: Option<ProfileIdentity>,
    #[serde(deserialize_with = "nullable")]
    pub audience: Option<Audience>,
    pub artifact_bytes: u64,
    pub artifact_files: u64,
}

/// An objects/put reply: the push quarantine's distinct records and bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectPut {
    pub records: u64,
    pub bytes: u64,
}

/// One artifact file by content: its hash and size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileIdentity {
    #[serde(with = "hex")]
    pub hash: Hash,
    pub size: u64,
}

/// A publish reply: the candidate's hub id, its state and its page link.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub id: String,
    pub state: String,
    pub url: String,
}

/// The publication record's inputs (hub i-F19YZ). `profile` is null for `implicit:org`. The
/// record carries no audience and no selection identity: the hub reads both from the tagged
/// profile and core's take.
#[derive(Clone, Debug, PartialEq)]
pub struct PublicationInputs {
    pub repo_id: RepoId,
    pub tag: String,
    pub landing: Hash,
    pub profile: Option<ProfileIdentity>,
    pub selection: Vec<SelectionEntry>,
    pub selection_hash: Hash,
    pub manifest_recipe: u32,
    pub manifest_hash: Hash,
    pub render_inputs: RenderInputs,
}

/// The upload steps. [`HttpPublishWire`] sends them; tests inject another implementation.
pub trait PublishWire: Send + Sync {
    /// `GET <remote>` with the sync media type.
    fn discovery(&self, cx: &UploadContext, remote: &str) -> Result<Discovery, RdyError>;
    /// `POST <base>/state`.
    fn state(&self, cx: &UploadContext, request: &StateRequest) -> Result<PublishState, RdyError>;
    /// `POST <base>/known`: the keys of `keys` the hub lacks.
    fn known(
        &self,
        cx: &UploadContext,
        push: &str,
        keys: &[ObjectKey],
    ) -> Result<Vec<ObjectKey>, RdyError>;
    /// `POST <base>/objects/put?push=<push>`: one batch of `len` bytes.
    fn put_objects(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut dyn Read,
        len: u64,
    ) -> Result<ObjectPut, RdyError>;
    /// `POST <base>/files/known`: the hashes of `files` the hub repo lacks.
    fn known_files(
        &self,
        cx: &UploadContext,
        push: &str,
        files: &[FileIdentity],
    ) -> Result<Vec<Hash>, RdyError>;
    /// `POST <base>/files/put?push=<push>&hash=<hash>`: one file's bytes.
    fn put_file(
        &self,
        cx: &UploadContext,
        push: &str,
        file: &FileIdentity,
        media_type: &str,
        body: &mut dyn Read,
    ) -> Result<FileIdentity, RdyError>;
    /// `POST <base>/publish`.
    fn publish(
        &self,
        cx: &UploadContext,
        push: &str,
        site: &str,
        record: &PublicationInputs,
        manifest: &Manifest,
        keep_latest: Option<bool>,
    ) -> Result<Candidate, RdyError>;
}

/// The upload steps over [`HttpHubTransport::upload`].
#[derive(Debug)]
pub struct HttpPublishWire {
    transport: HttpHubTransport,
}

impl HttpPublishWire {
    pub fn new(transport: HttpHubTransport) -> HttpPublishWire {
        HttpPublishWire { transport }
    }
}

// ---------------------------------------------------------------------------------------------
// JSON spellings
// ---------------------------------------------------------------------------------------------

/// A hash as 64 lowercase hex digits.
mod hex {
    use rdy_model::Hash;
    use serde::{Deserialize, Deserializer, Serializer, de};

    pub fn serialize<S: Serializer>(h: &Hash, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(h)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Hash, D::Error> {
        let text = String::deserialize(d)?;
        text.parse().map_err(|_| de::Error::custom("bad hash"))
    }
}

/// A member every reply holds whose value may be null.
fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}

fn nullable_hash<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Hash>, D::Error> {
    Option::<String>::deserialize(d)?
        .map(|t| t.parse().map_err(|_| de::Error::custom("bad hash")))
        .transpose()
}

/// A repo id as exactly 32 lowercase hex digits.
fn nullable_repo_id<'de, D: Deserializer<'de>>(d: D) -> Result<Option<RepoId>, D::Error> {
    Option::<String>::deserialize(d)?
        .map(|t| match t.parse::<RepoId>() {
            Ok(id) if id.to_string() == t => Ok(id),
            _ => Err(de::Error::custom("bad repo id")),
        })
        .transpose()
}

/// An optional member that, when present, is an integer.
fn present_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    u64::deserialize(d).map(Some)
}

/// An optional member that, when present, is an object; its value is not kept.
fn present_object<'de, D: Deserializer<'de>>(d: D) -> Result<Option<()>, D::Error> {
    serde_json::Map::<String, serde_json::Value>::deserialize(d).map(|_| Some(()))
}

fn ser_hash<S: Serializer>(h: &Hash, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(h)
}

fn ser_opt_hash<S: Serializer>(h: &Option<Hash>, s: S) -> Result<S::Ok, S::Error> {
    match h {
        Some(h) => s.collect_str(h),
        None => s.serialize_none(),
    }
}

fn ser_repo_id<S: Serializer>(id: &RepoId, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(id)
}

#[derive(Serialize)]
struct KnownBody<'a> {
    push: &'a str,
    keys: &'a [ObjectKey],
}

#[derive(Serialize)]
struct FilesKnownBody<'a> {
    push: &'a str,
    files: &'a [FileIdentity],
}

#[derive(Serialize)]
struct SelectionRow<'a> {
    id: &'a BlockId,
    #[serde(serialize_with = "ser_hash")]
    version: Hash,
    #[serde(serialize_with = "ser_opt_hash")]
    redaction: Option<Hash>,
}

/// The selection rows, in their order.
struct Rows<'a>(&'a [SelectionEntry]);

impl Serialize for Rows<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.0.iter().map(|e| SelectionRow {
            id: &e.id,
            version: e.version,
            redaction: e.redaction,
        }))
    }
}

#[derive(Serialize)]
struct RecordBody<'a> {
    #[serde(serialize_with = "ser_repo_id")]
    repo_id: RepoId,
    tag: &'a str,
    #[serde(serialize_with = "ser_hash")]
    landing: Hash,
    profile: Option<&'a ProfileIdentity>,
    selection: Rows<'a>,
    #[serde(serialize_with = "ser_hash")]
    selection_hash: Hash,
    manifest_recipe: u32,
    #[serde(serialize_with = "ser_hash")]
    manifest_hash: Hash,
    render_inputs: &'a RenderInputs,
}

#[derive(Serialize)]
struct ManifestBody<'a> {
    files: Vec<&'a ManifestEntry>,
}

#[derive(Serialize)]
struct PublishBody<'a> {
    push: &'a str,
    site: &'a str,
    record: RecordBody<'a>,
    manifest: ManifestBody<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    keep_latest: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscoveryDto {
    sync_base: String,
    protocols: Vec<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MissingKeys {
    missing: Vec<ObjectKey>,
}

/// A hash in a files/known reply.
struct HexHash(Hash);

impl<'de> Deserialize<'de> for HexHash {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<HexHash, D::Error> {
        hex::deserialize(d).map(HexHash)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MissingFiles {
    missing: Vec<HexHash>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum SeverityDto {
    Error,
    Warning,
}

/// One entry of a hub error's `issues`: an Issue's shape.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IssueDto {
    severity: SeverityDto,
    code: String,
    #[serde(deserialize_with = "nullable")]
    block: Option<BlockId>,
    message: String,
    fix: String,
}

/// The hub's error object (hub tb-68J50). `corrected` is bounded and discarded: the upload has
/// no payload to send again.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorDto {
    code: String,
    message: String,
    fix: String,
    request_id: String,
    issues: Vec<IssueDto>,
    #[serde(default, deserialize_with = "present_u64")]
    retry_after: Option<u64>,
    #[serde(default, deserialize_with = "present_object", rename = "corrected")]
    _corrected: Option<()>,
}

// ---------------------------------------------------------------------------------------------
// Request checks
// ---------------------------------------------------------------------------------------------

fn bad_input(message: &str, fix: &str) -> RdyError {
    RdyError::new(E_BAD_INPUT, message, fix)
}

/// A push id: 26 lower-case Crockford base32 characters.
fn check_push(push: &str) -> Result<(), RdyError> {
    let crockford = |b: u8| {
        rdy_model::CROCKFORD
            .iter()
            .any(|c| c.to_ascii_lowercase() == b)
    };
    if push.len() == PUSH_ID_CHARS && push.bytes().all(crockford) {
        Ok(())
    } else {
        Err(bad_input(
            "the push id is not valid",
            "use 26 lower-case Crockford base32 characters",
        ))
    }
}

/// A site name: 1 to 64 ASCII letters, digits, underscores or hyphens, a letter or digit first.
fn check_site(site: &str) -> Result<(), RdyError> {
    let ok = site.len() <= MAX_SITE_BYTES
        && site
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && site
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(bad_input(
            "the site name is not valid",
            "use 1 to 64 ASCII letters, digits, underscores or hyphens, starting with a letter or digit",
        ))
    }
}

/// 1 to 16 distinct positive stream formats.
fn formats_ok(list: &[u32]) -> bool {
    !list.is_empty()
        && list.len() <= MAX_FORMATS
        && list.iter().all(|&f| f > 0)
        && list.iter().enumerate().all(|(i, f)| !list[..i].contains(f))
}

fn check_state_request(cx: &UploadContext, r: &StateRequest) -> Result<(), RdyError> {
    if r.actor != cx.claim.actor || r.run != cx.claim.run {
        return Err(actor_unbound());
    }
    check_site(&r.site)?;
    if semver::Version::parse(&r.rdy).is_err() {
        return Err(bad_input(
            "the client version is not a version",
            "send this rdy's version",
        ));
    }
    if !formats_ok(&r.stream_read) || !formats_ok(&r.stream_write) {
        return Err(bad_input(
            "the stream formats are not valid",
            "send 1 to 16 distinct stream formats",
        ));
    }
    Ok(())
}

/// 1 to `max` entries, else `E_BAD_INPUT` or `E_LIMIT`.
fn check_entries(n: usize, max: usize, what: &str) -> Result<(), RdyError> {
    if n == 0 {
        return Err(bad_input(
            &format!("the {what} list is empty"),
            &format!("name at least one {what}"),
        ));
    }
    check_count(n, max, LimitUnit::Entries)
}

fn unordered(what: &str) -> RdyError {
    bad_input(
        &format!("the {what} are not sorted and distinct"),
        &format!("sort the {what} and name each once"),
    )
}

/// Checks the record and its manifest before the publish body is encoded: the count bounds
/// first, then the manifest (validated and hashed), then the render inputs.
fn check_record(record: &PublicationInputs, manifest: &Manifest) -> Result<(), RdyError> {
    if !crate::site_scope::tag_name_ok(&record.tag) {
        return Err(RdyError::new(
            E_BAD_TAG_NAME,
            "the publication tag is not a version name",
            "publish a tag named like v1.2.0",
        ));
    }
    check_count(
        record.selection.len(),
        MAX_SELECTION_ROWS,
        LimitUnit::Entries,
    )?;
    if record.selection.windows(2).any(|w| w[0].id >= w[1].id) {
        return Err(unordered("selection rows"));
    }
    if manifest_hash(manifest, record.manifest_recipe)? != record.manifest_hash {
        return Err(bad_input(
            "the publication record does not match its manifest",
            "compute the record's manifest hash from this manifest",
        ));
    }
    validate_inputs(&record.render_inputs)
}

/// A writer that refuses any byte past `cap`.
struct Capped {
    out: Vec<u8>,
    cap: usize,
    over: bool,
}

impl Write for Capped {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let fits = self
            .out
            .len()
            .checked_add(buf.len())
            .is_some_and(|n| n <= self.cap);
        if !fits {
            self.over = true;
            return Err(std::io::Error::other("over the cap"));
        }
        self.out.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The compact JSON of `value`, refused with `E_LIMIT` at more than `cap` bytes.
fn encode<T: Serialize>(value: &T, cap: usize) -> Result<Vec<u8>, RdyError> {
    let mut w = Capped {
        out: Vec::new(),
        cap,
        over: false,
    };
    match serde_json::to_writer(&mut w, value) {
        Ok(()) => Ok(w.out),
        Err(_) if w.over => Err(hub_limit(cap, LimitUnit::Bytes)),
        Err(_) => Err(bad_input(
            "the upload request could not be encoded",
            "check the publication inputs",
        )),
    }
}

/// The publish body: `{push, site, record, manifest}` and, only when given, `keep_latest`
/// beside `record`. The push id, site, record and manifest are checked first; the manifest's
/// files are written in path order. Over 268,435,456 bytes is `E_LIMIT`.
pub fn encode_publish(
    push: &str,
    site: &str,
    record: &PublicationInputs,
    manifest: &Manifest,
    keep_latest: Option<bool>,
) -> Result<Vec<u8>, RdyError> {
    check_push(push)?;
    check_site(site)?;
    check_record(record, manifest)?;
    let mut files: Vec<&ManifestEntry> = manifest.files.iter().collect();
    files.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    let body = PublishBody {
        push,
        site,
        record: RecordBody {
            repo_id: record.repo_id,
            tag: &record.tag,
            landing: record.landing,
            profile: record.profile.as_ref(),
            selection: Rows(&record.selection),
            selection_hash: record.selection_hash,
            manifest_recipe: record.manifest_recipe,
            manifest_hash: record.manifest_hash,
            render_inputs: &record.render_inputs,
        },
        manifest: ManifestBody { files },
        keep_latest,
    };
    encode(&body, PUBLISH_REQUEST_BYTES)
}

// ---------------------------------------------------------------------------------------------
// URLs
// ---------------------------------------------------------------------------------------------

/// Whether a sync-base path is one absolute path: one leading slash, no query, fragment or
/// backslash, valid percent escapes, no dot segment (percent-encoded ones included) and no empty
/// segment but a trailing one.
fn base_path_ok(path: &str) -> bool {
    let Some(rest) = path.strip_prefix('/') else {
        return false;
    };
    if rest.starts_with('/') || path.contains(['?', '#', '\\']) {
        return false;
    }
    let segments: Vec<&str> = rest.split('/').collect();
    let last = segments.len() - 1;
    segments.iter().enumerate().all(|(i, s)| {
        if s.is_empty() {
            return i == last;
        }
        let b = s.as_bytes();
        let escapes_ok = b.iter().enumerate().all(|(j, &c)| {
            c != b'%'
                || (b.get(j + 1).is_some_and(u8::is_ascii_hexdigit)
                    && b.get(j + 2).is_some_and(u8::is_ascii_hexdigit))
        });
        let dots = s.to_ascii_lowercase().replace("%2e", ".");
        escapes_ok && dots != "." && dots != ".."
    })
}

/// Raw checks every sync base passes: within 4096 bytes, no white space or control character.
fn base_text_ok(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_URL_BYTES
        && !text.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// An absolute HTTPS sync base that the URL parser keeps byte for byte: no userinfo, query or
/// fragment, and a path [`base_path_ok`] accepts. The origin is not checked here.
fn absolute_base(text: &str) -> Option<url::Url> {
    if !base_text_ok(text) || text.contains(['?', '#', '\\']) {
        return None;
    }
    let url = url::Url::parse(text).ok()?;
    let ok = url.scheme() == "https"
        && url.as_str() == text
        && url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some_and(|h| !h.is_empty())
        && base_path_ok(url.path());
    ok.then_some(url)
}

/// The sync base of a discovery reply as an absolute URL: a path on `install`, or an HTTPS URL
/// that `fence` admits (its refusal keeps the pin-aware fix). A listed origin adds no trust.
fn resolve_base(fence: &OriginFence, install: &Origin, text: &str) -> Result<String, RdyError> {
    if !base_text_ok(text) {
        return Err(upstream_protocol());
    }
    if text.starts_with('/') {
        if !base_path_ok(text) {
            return Err(upstream_protocol());
        }
        let full = format!("{install}{text}");
        if absolute_base(&full).is_none() {
            return Err(upstream_protocol());
        }
        return Ok(full);
    }
    let https = text
        .get(..8)
        .is_some_and(|s| s.eq_ignore_ascii_case("https://"));
    if !https {
        return Err(upstream_protocol());
    }
    fence.check(text)?;
    absolute_base(text)
        .map(|_| text.to_string())
        .ok_or_else(upstream_protocol)
}

/// The URL of the fixed route `segments` under the context's sync base, with `query` pairs.
fn route(
    cx: &UploadContext,
    segments: &[&str],
    query: &[(&str, &str)],
) -> Result<String, RdyError> {
    let text = cx.sync_base.as_deref().ok_or_else(|| {
        bad_input(
            "the upload has no sync base",
            "run discovery for the remote first",
        )
    })?;
    let mut url = absolute_base(text).ok_or_else(|| {
        bad_input(
            "the sync base is not a valid upload address",
            "use the sync base that discovery returned",
        )
    })?;
    url.path_segments_mut()
        .map_err(|_| upstream_protocol())?
        .pop_if_empty()
        .extend(segments);
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    let out: String = url.into();
    check_count(out.len(), MAX_URL_BYTES, LimitUnit::Bytes)?;
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Exchanges and refusals
// ---------------------------------------------------------------------------------------------

/// Whether a step only reads hub state or may write it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Effect {
    Read,
    Write,
}

/// A reply outside the contract after the request was sent.
fn invalid_reply(effect: Effect) -> RdyError {
    match effect {
        Effect::Read => upstream_protocol(),
        Effect::Write => upstream_uncertain(),
    }
}

/// One request of a step.
struct Exchange<'a> {
    effect: Effect,
    method: UploadMethod,
    url: &'a str,
    content_type: &'a str,
    body: Option<&'a mut dyn Read>,
    body_len: u64,
    reply_limit: usize,
    success: &'a [u16],
}

fn limits(bytes: usize, values: usize) -> JsonLimits {
    JsonLimits {
        bytes,
        depth: MAX_JSON_DEPTH,
        values,
        scalar_bytes: MAX_JSON_SCALAR_BYTES,
    }
}

/// `body` read as `T` within `limits`; anything else is an invalid reply.
fn reply<T: DeserializeOwned>(
    body: &[u8],
    limits: JsonLimits,
    effect: Effect,
) -> Result<T, RdyError> {
    decode_bounded_json(body, limits).map_err(|_| invalid_reply(effect))
}

/// Whether `text` holds the bearer or any hub token prefix.
fn echoes_token(text: &str, bearer: &Secret) -> bool {
    let b = bearer.expose();
    (!b.is_empty() && text.contains(b)) || TOKEN_PREFIXES.iter().any(|p| text.contains(p))
}

/// 1 to 64 upper-case ASCII letters, digits and underscores.
fn code_ok(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= MAX_ERROR_CODE_BYTES
        && code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// At most 4096 bytes with no control character but LF.
fn text_ok(text: &str) -> bool {
    text.len() <= MAX_ERROR_TEXT_BYTES && !text.chars().any(|c| c.is_control() && c != '\n')
}

/// 1 to 4096 bytes with no control character.
fn id_ok(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_ID_BYTES && !text.chars().any(char::is_control)
}

/// The static spelling of a recognized hub or core code, without a status check.
fn known_code(code: &str) -> Option<&'static str> {
    match api_error_entry(code, None) {
        Ok(Some((c, _))) => Some(c),
        _ => core_error_code(code),
    }
}

fn issue(dto: IssueDto) -> Issue {
    let severity = match dto.severity {
        SeverityDto::Error => Severity::Error,
        SeverityDto::Warning => Severity::Warning,
    };
    match known_code(&dto.code) {
        Some(code) => Issue {
            severity,
            code,
            block: dto.block,
            message: dto.message,
            fix: dto.fix,
        },
        None => {
            let generic = upstream_refused();
            Issue {
                severity,
                code: generic.code,
                block: dto.block,
                message: generic.message,
                fix: generic.fix,
            }
        }
    }
}

fn notice(message: String, fix: &str) -> Issue {
    Issue {
        severity: Severity::Error,
        code: E_UPSTREAM_REFUSED,
        block: None,
        message,
        fix: fix.to_string(),
    }
}

/// A hub error object at a refusal `status`, as the caller's error. `None` for a body outside
/// the object's grammar or bounds, a known code at another status, a retry code without its
/// wait, or a body that echoes the bearer or a token prefix: the body is then discarded whole.
fn refusal(status: u16, body: &[u8], cap: usize, bearer: &Secret) -> Option<RdyError> {
    let dto: ErrorDto = decode_bounded_json(body, limits(cap, MAX_JSON_VALUES)).ok()?;
    let raw = std::str::from_utf8(body).ok()?;
    let issues_ok = dto.issues.len() <= MAX_ERROR_ISSUES
        && dto
            .issues
            .iter()
            .all(|i| code_ok(&i.code) && text_ok(&i.message) && text_ok(&i.fix));
    let shape_ok = code_ok(&dto.code)
        && text_ok(&dto.message)
        && text_ok(&dto.fix)
        && id_ok(&dto.request_id)
        && issues_ok
        && dto
            .retry_after
            .is_none_or(|n| (1..=MAX_RETRY_AFTER_SECONDS).contains(&n));
    if !shape_ok {
        return None;
    }
    // Every decoded string, and the raw body for what is not decoded (`corrected`).
    let texts_echo = [&dto.code, &dto.message, &dto.fix, &dto.request_id]
        .into_iter()
        .chain(
            dto.issues
                .iter()
                .flat_map(|i| [&i.code, &i.message, &i.fix]),
        )
        .any(|t| echoes_token(t, bearer));
    let block_echo = dto
        .issues
        .iter()
        .filter_map(|i| i.block.as_ref())
        .any(|b| echoes_token(b.as_str(), bearer));
    if texts_echo || block_echo || echoes_token(raw, bearer) {
        return None;
    }
    // The pair first, so a code with several statuses reaches each of its rows.
    let code = match api_error_entry(&dto.code, Some(status)) {
        Err(_) => return None,
        Ok(Some((code, _))) => Some(code),
        Ok(None) => core_error_code(&dto.code),
    };
    if code.is_some_and(|c| RETRY_CODES.contains(&c)) && dto.retry_after.is_none() {
        return None;
    }
    let mut e = match code {
        Some(code) => RdyError::new(code, dto.message, dto.fix),
        None => upstream_refused(),
    };
    e.issues = dto.issues.into_iter().map(issue).collect();
    e.issues.push(notice(
        format!("hub request {}", dto.request_id),
        REQUEST_ID_FIX,
    ));
    if let Some(n) = dto.retry_after {
        e.issues
            .push(notice(format!("retry after {n} seconds"), RETRY_FIX));
    }
    Some(e)
}

impl HttpPublishWire {
    /// Sends one request: the success body, or the step's error. Nothing is retried.
    fn exchange(&self, cx: &UploadContext, x: Exchange<'_>) -> Result<Vec<u8>, RdyError> {
        let effect = x.effect;
        let sent = self.transport.upload(
            &cx.install,
            cx.ca_file.as_deref(),
            &cx.claim,
            &cx.bearer,
            UploadHttpRequest {
                method: x.method,
                destination: x.url,
                content_type: x.content_type,
                body: x.body,
                body_len: x.body_len,
                reply_limit: x.reply_limit,
            },
        );
        let (status, body) = match sent {
            Ok(r) => r,
            Err(HubFailure::Local(e)) => return Err(e),
            Err(HubFailure::Transport { may_have_sent }) => {
                return Err(if effect == Effect::Write && may_have_sent {
                    upstream_uncertain()
                } else {
                    upstream_transport()
                });
            }
            // A redirect or a reply over its bounds: the request was sent.
            Err(HubFailure::Protocol) => return Err(invalid_reply(effect)),
            Err(f) => return Err(f.into_error(&cx.install)),
        };
        if x.success.contains(&status) {
            return Ok(body);
        }
        if (200..300).contains(&status) {
            return Err(invalid_reply(effect));
        }
        Err(refusal(status, &body, x.reply_limit, &cx.bearer)
            .unwrap_or_else(|| invalid_reply(effect)))
    }

    /// A JSON POST of a step.
    fn post_json(
        &self,
        cx: &UploadContext,
        effect: Effect,
        url: &str,
        json: Vec<u8>,
        reply_limit: usize,
        success: &[u16],
    ) -> Result<Vec<u8>, RdyError> {
        let len = u64::try_from(json.len()).map_err(|_| hub_limit(usize::MAX, LimitUnit::Bytes))?;
        let mut body = Cursor::new(json);
        self.exchange(
            cx,
            Exchange {
                effect,
                method: UploadMethod::Post,
                url,
                content_type: "application/json",
                body: Some(&mut body),
                body_len: len,
                reply_limit,
                success,
            },
        )
    }
}

/// The state reply's checks; it clamps the limits to core's caps.
fn check_state(st: &mut PublishState) -> bool {
    let semver_ok = |v: &str| semver::Version::parse(v).is_ok();
    let versions_ok = semver_ok(&st.rdy) && st.min_rdy.as_deref().is_none_or(semver_ok);
    let streams_ok = formats_ok(&st.stream_read)
        && formats_ok(&st.stream_write)
        && st.stream_read.contains(&formats().stream);
    let names_ok = st.features.len() <= MAX_FEATURES
        && st.features.iter().all(|f| {
            !f.name.is_empty()
                && f.name.len() <= MAX_FEATURE_NAME_BYTES
                && f.name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        && st.features.windows(2).all(|w| w[0].name < w[1].name);
    // Only an incompatible feature this client does not know is refused.
    let features_ok = st.features.iter().all(|f| {
        f.class != FeatureClass::Incompatible || KNOWN_INCOMPATIBLE.contains(&f.name.as_str())
    });
    let caps = [
        (&mut st.batch_bytes, CORE_BATCH_BYTES),
        (&mut st.batch_records, CORE_BATCH_RECORDS),
        (&mut st.quarantine_seconds, CORE_QUARANTINE_SECONDS),
        (&mut st.artifact_bytes, CORE_ARTIFACT_BYTES),
        (&mut st.artifact_files, CORE_ARTIFACT_FILES),
    ];
    let mut limits_ok = true;
    for (value, cap) in caps {
        limits_ok &= *value > 0;
        *value = (*value).min(cap);
    }
    versions_ok && streams_ok && names_ok && features_ok && limits_ok
}

impl PublishWire for HttpPublishWire {
    fn discovery(&self, cx: &UploadContext, remote: &str) -> Result<Discovery, RdyError> {
        let remote = RemoteUrl::parse(remote)?;
        let body = self.exchange(
            cx,
            Exchange {
                effect: Effect::Read,
                method: UploadMethod::Get,
                url: remote.as_str(),
                content_type: SYNC_MEDIA_TYPE,
                body: None,
                body_len: 0,
                reply_limit: REPLY_BYTES,
                success: &[200],
            },
        )?;
        let dto: DiscoveryDto = reply(&body, limits(REPLY_BYTES, MAX_JSON_VALUES), Effect::Read)?;
        let p = &dto.protocols;
        let protocols_ok = !p.is_empty()
            && p.len() <= MAX_PROTOCOLS
            && p.iter().all(|&v| v > 0)
            && p.iter().enumerate().all(|(i, v)| !p[..i].contains(v))
            && p.contains(&SYNC_PROTOCOL);
        if !protocols_ok {
            return Err(upstream_protocol());
        }
        let sync_base = resolve_base(self.transport.fence(), &cx.install, &dto.sync_base)?;
        Ok(Discovery {
            sync_base,
            protocols: dto.protocols,
        })
    }

    fn state(&self, cx: &UploadContext, request: &StateRequest) -> Result<PublishState, RdyError> {
        check_state_request(cx, request)?;
        let url = route(cx, &["state"], &[])?;
        let json = encode(request, STATE_REQUEST_BYTES)?;
        let body = self.post_json(cx, Effect::Read, &url, json, REPLY_BYTES, &[200])?;
        let mut st: PublishState =
            reply(&body, limits(REPLY_BYTES, MAX_JSON_VALUES), Effect::Read)?;
        if !check_state(&mut st) {
            return Err(upstream_protocol());
        }
        Ok(st)
    }

    fn known(
        &self,
        cx: &UploadContext,
        push: &str,
        keys: &[ObjectKey],
    ) -> Result<Vec<ObjectKey>, RdyError> {
        check_push(push)?;
        check_entries(keys.len(), MAX_KNOWN_KEYS, "object key")?;
        if keys.windows(2).any(|w| w[0] >= w[1]) {
            return Err(unordered("object keys"));
        }
        let url = route(cx, &["known"], &[])?;
        let json = encode(&KnownBody { push, keys }, KNOWN_REQUEST_BYTES)?;
        let body = self.post_json(cx, Effect::Read, &url, json, KNOWN_REPLY_BYTES, &[200])?;
        // `missing` and, per key, one element and two members.
        let values = keys.len().saturating_mul(3).saturating_add(1);
        let dto: MissingKeys = reply(&body, limits(KNOWN_REPLY_BYTES, values), Effect::Read)?;
        let subset = dto.missing.windows(2).all(|w| w[0] < w[1])
            && dto.missing.iter().all(|k| keys.binary_search(k).is_ok());
        if !subset {
            return Err(upstream_protocol());
        }
        Ok(dto.missing)
    }

    fn put_objects(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut dyn Read,
        len: u64,
    ) -> Result<ObjectPut, RdyError> {
        check_push(push)?;
        let url = route(cx, &["objects", "put"], &[("push", push)])?;
        let reply_body = self.exchange(
            cx,
            Exchange {
                effect: Effect::Write,
                method: UploadMethod::Post,
                url: &url,
                content_type: "application/octet-stream",
                body: Some(body),
                body_len: len,
                reply_limit: REPLY_BYTES,
                success: &[200, 201],
            },
        )?;
        reply(
            &reply_body,
            limits(REPLY_BYTES, MAX_JSON_VALUES),
            Effect::Write,
        )
    }

    fn known_files(
        &self,
        cx: &UploadContext,
        push: &str,
        files: &[FileIdentity],
    ) -> Result<Vec<Hash>, RdyError> {
        check_push(push)?;
        check_entries(files.len(), MAX_KNOWN_FILES, "file")?;
        let mut held: Vec<Hash> = files.iter().map(|f| f.hash).collect();
        held.sort_unstable();
        if held.windows(2).any(|w| w[0] == w[1]) {
            return Err(unordered("file hashes"));
        }
        let url = route(cx, &["files", "known"], &[])?;
        let json = encode(&FilesKnownBody { push, files }, FILES_KNOWN_REQUEST_BYTES)?;
        let body = self.post_json(
            cx,
            Effect::Read,
            &url,
            json,
            FILES_KNOWN_REPLY_BYTES,
            &[200],
        )?;
        // `missing` and one element per hash.
        let values = files.len().saturating_add(1);
        let dto: MissingFiles =
            reply(&body, limits(FILES_KNOWN_REPLY_BYTES, values), Effect::Read)?;
        let missing: Vec<Hash> = dto.missing.into_iter().map(|h| h.0).collect();
        let subset = missing.windows(2).all(|w| w[0] < w[1])
            && missing.iter().all(|h| held.binary_search(h).is_ok());
        if !subset {
            return Err(upstream_protocol());
        }
        Ok(missing)
    }

    fn put_file(
        &self,
        cx: &UploadContext,
        push: &str,
        file: &FileIdentity,
        media_type: &str,
        body: &mut dyn Read,
    ) -> Result<FileIdentity, RdyError> {
        check_push(push)?;
        let hash = file.hash.to_string();
        let url = route(cx, &["files", "put"], &[("push", push), ("hash", &hash)])?;
        let reply_body = self.exchange(
            cx,
            Exchange {
                effect: Effect::Write,
                method: UploadMethod::Post,
                url: &url,
                content_type: media_type,
                body: Some(body),
                body_len: file.size,
                reply_limit: REPLY_BYTES,
                success: &[200, 201],
            },
        )?;
        let held: FileIdentity = reply(
            &reply_body,
            limits(REPLY_BYTES, MAX_JSON_VALUES),
            Effect::Write,
        )?;
        if held != *file {
            return Err(upstream_uncertain());
        }
        Ok(held)
    }

    fn publish(
        &self,
        cx: &UploadContext,
        push: &str,
        site: &str,
        record: &PublicationInputs,
        manifest: &Manifest,
        keep_latest: Option<bool>,
    ) -> Result<Candidate, RdyError> {
        let json = encode_publish(push, site, record, manifest, keep_latest)?;
        let url = route(cx, &["publish"], &[])?;
        let body = self.post_json(cx, Effect::Write, &url, json, REPLY_BYTES, &[200, 201])?;
        let c: Candidate = reply(&body, limits(REPLY_BYTES, MAX_JSON_VALUES), Effect::Write)?;
        let ok = id_ok(&c.id)
            && c.state == "candidate"
            && c.url.len() <= MAX_URL_BYTES
            && id_ok(&c.url)
            && !echoes_token(&c.id, &cx.bearer)
            && !echoes_token(&c.url, &cx.bearer);
        if !ok {
            return Err(upstream_uncertain());
        }
        Ok(c)
    }
}
