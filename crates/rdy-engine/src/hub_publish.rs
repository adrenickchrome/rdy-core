//! The one publish coordinator (D-Q0AVZ, D-PJ65F, D-48NYM, D-0VCQ5, D-QYCRB, D-VRW0A, D-50JXR;
//! hub i-6ZXW0, i-DETA1, i-F19YZ, r-K71GM): one tag's publication, rendered from that tag and
//! sent to a publish remote's hub as a candidate. Publishing makes the selection readable by its
//! audience on the hub; it never moves canon, writes a tag, signs, edits config or stores a
//! publication record or receipt here (D-WDKWQ: the hub owns the record).
//!
//! - **One snapshot.** [`PublishService::run`] reads one [`ReadState`] and the tag's stored name
//!   and landing in it. The profile resolves at that tag, never in current canon: the site's
//!   profile id, or `implicit:org` when the hub names none. The audience is the tagged profile's
//!   own; the hub's site audience, when it has one, must equal it. No caller supplies an
//!   audience or a profile.
//! - **Composed steps.** Settings and access come from HA2a ([`HubConfig`], [`HubCredentials`]);
//!   discovery, state and every upload step from the typed wire ([`PublishWire`]); the history
//!   from the history sender, the files and the final request from the file sender. The order is
//!   fixed: request checks, settings, access, discovery, state, repo identity, profile, audience
//!   and reference check, render and artifact check, history, files, an epoch recheck, publish.
//! - **Current access, one claim.** Every upload request gets its context right before it is
//!   sent: current access to the selected install ([`HubCredentials::access`], which renews an
//!   expiring token) and then the source's redaction epoch. The install, its CA file and the
//!   bound claim never change within a run. A refused actor claim reads `/me` once
//!   ([`HubCredentials::refresh_handle`]) and returns the original refusal; nothing is resent.
//! - **No writer.** Nothing runs on the project's writer and no lock is held across a request.
//!   A stale epoch stops the run before its next phase; bytes already sent stay in the hub's
//!   quarantine under its expiry policy. The epoch is checked again after the publish reply: a
//!   redaction that landed while publish was in flight returns no success, and its refusal
//!   keeps the push id with the publish phase, since the candidate may exist.
//! - **Uncertainty.** A write whose reply was lost, or a publish reply that breaks the candidate
//!   contract, is `E_UPSTREAM_UNCERTAIN` naming the push id and the phase: the upload may have
//!   completed. Reads that fail stay `E_UPSTREAM_TRANSPORT`. Nothing is retried and nothing is
//!   kept locally to resume.
//! - **The result.** The candidate's id, state and page link, the push id, the tagged profile's
//!   audience, the full record, the reference summary, a bounded inventory, the settings and
//!   credential warnings and the renderer's warnings, and `review: hub`: the hub's own review is
//!   authoritative. The inventory is no disclosure diff and claims no baseline. The page link is
//!   returned, never fetched or opened.
use crate::error::{
    E_BAD_INPUT, E_LIMIT, E_REPO_MISMATCH, E_UPSTREAM_ACTOR_CLAIM, E_UPSTREAM_UNCERTAIN, RdyError,
    upstream_auth, upstream_protocol,
};
use crate::formats::formats;
use crate::hub_config::{
    BAD_REMOTE_NAME_FIX, BAD_REMOTE_NAME_MESSAGE, HubConfig, RemoteMode, is_remote_name,
};
use crate::hub_credentials::HubCredentials;
use crate::hub_publish_files::{ArtifactLimits, ArtifactSource, publish_record, upload_files};
use crate::hub_publish_stream::{BatchLimits, PreparedHistory, PushId, send_history};
use crate::hub_publish_wire::{
    Candidate, MAX_SITE_BYTES, ProfileIdentity, PublicationInputs, PublishState, PublishWire,
    StateRequest, UploadContext,
};
use crate::hub_types::{HubClock, HubTransport, Origin, OriginFence, RequestClaim};
use crate::publication::{
    self, AttachmentEntry, Audience, DocEntry, IMPLICIT_ORG, Resolved, SelectionEntry,
    reference_summary,
};
use crate::publication_manifest::{MANIFEST_RECIPE, Manifest};
use crate::render_inputs::RenderInputs;
use crate::session::Session;
use crate::version::parse_version;
use crate::{ReadState, Scope};
use rdy_model::{BlockId, Hash, RepoId, Tag};
use rdy_schema::{Issue, Severity};
use rdy_store::take_batch::MAX_UPLOAD_TAG_BYTES;
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Serialize, Serializer};
use std::cmp::Ordering;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The most bytes a publish result serializes to.
pub const MAX_RESULT_BYTES: usize = 67_108_864;
/// The most rows each inventory list shows; its omitted count gives the rest.
pub const INVENTORY_ROWS: usize = 200;
/// Where a candidate is reviewed: on the hub, which is authoritative.
pub const REVIEW: &str = "hub";
/// The bytes the result budget keeps for each of the candidate's id and page link, the reply
/// contract's bound.
const BUDGET_ID_BYTES: usize = 4096;

/// What `rdy_publish` asks for: the tag (`v1.2.0`, or a name of the same version), the publish
/// remote and the hub site. `keep_latest` is optional and reaches the hub unchanged; absent and
/// null are both `None` (D-50JXR). Unknown keys are refused.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishRequest {
    pub tag: String,
    pub to: String,
    pub site: String,
    #[serde(default)]
    pub keep_latest: Option<bool>,
}

/// A rendered publication ready to upload: the immutable artifact and the renderer's warnings,
/// in its order (an org profile's reference warning first, then the scope's).
pub struct RenderedUpload {
    pub artifact: Box<dyn ArtifactSource>,
    pub warnings: Vec<String>,
}

/// The renderer the coordinator calls: it captures the render inputs of a scope, then renders a
/// profile's publication of that scope from them. It takes no code, command or shell option.
pub trait PublishRenderer {
    /// The render inputs of `scope` in `read`.
    fn capture(&self, read: &ReadState, scope: &Scope) -> Result<RenderInputs, RdyError>;
    /// The publication of `profile` (a profile id or `implicit:org`) in `scope`, the landing of
    /// the tag stored as `tag`, from `inputs` alone.
    fn render(
        &self,
        read: &ReadState,
        scope: &Scope,
        tag: &str,
        profile: &str,
        inputs: &RenderInputs,
    ) -> Result<RenderedUpload, RdyError>;
}

/// The local inventory of a publication: the first [`INVENTORY_ROWS`] selected docs and
/// attachments in their sorted order, how many of each were left out, and how many blocks the
/// selection holds. It is no disclosure diff and names no baseline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventorySummary {
    pub docs: Vec<DocEntry>,
    pub docs_omitted: u64,
    pub attachments: Vec<AttachmentEntry>,
    pub attachments_omitted: u64,
    pub selection_count: u64,
}

/// A publish result: the candidate the hub made (its id, state and page link), the push id, the
/// tagged profile's audience, the record sent, the reference summary, the inventory, the
/// settings and credential warnings, the renderer's warnings and where to review (`hub`).
#[derive(Clone, Debug, PartialEq)]
pub struct PublishResult {
    pub id: String,
    pub state: String,
    pub url: String,
    pub push: String,
    pub audience: Audience,
    pub record: PublicationInputs,
    pub references: serde_json::Value,
    pub inventory: InventorySummary,
    pub warnings: Vec<Issue>,
    pub render_warnings: Vec<String>,
    pub review: String,
}

// ---------------------------------------------------------------------------------------------
// The result's JSON
// ---------------------------------------------------------------------------------------------

/// A hash as its lowercase hex text.
fn hex(h: &Hash) -> String {
    h.to_string()
}

struct DocRows<'a>(&'a [DocEntry]);

impl Serialize for DocRows<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.0.len()))?;
        for d in self.0 {
            seq.serialize_element(&serde_json::json!({
                "id": d.id.to_string(),
                "path": d.path,
                "digest": hex(&d.digest),
                "redacted": d.redacted,
            }))?;
        }
        seq.end()
    }
}

struct AttachmentRows<'a>(&'a [AttachmentEntry]);

impl Serialize for AttachmentRows<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.0.len()))?;
        for a in self.0 {
            seq.serialize_element(&serde_json::json!({
                "block": a.block.to_string(),
                "home_doc": a.home_doc.to_string(),
                "blob": hex(&a.blob),
                "available": a.available,
            }))?;
        }
        seq.end()
    }
}

impl Serialize for InventorySummary {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(5))?;
        m.serialize_entry("docs", &DocRows(&self.docs))?;
        m.serialize_entry("docs_omitted", &self.docs_omitted)?;
        m.serialize_entry("attachments", &AttachmentRows(&self.attachments))?;
        m.serialize_entry("attachments_omitted", &self.attachments_omitted)?;
        m.serialize_entry("selection_count", &self.selection_count)?;
        m.end()
    }
}

struct SelectionRows<'a>(&'a [SelectionEntry]);

impl Serialize for SelectionRows<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.0.len()))?;
        for e in self.0 {
            seq.serialize_element(&serde_json::json!({
                "id": e.id.to_string(),
                "version": hex(&e.version),
                "redaction": e.redaction.as_ref().map(hex),
            }))?;
        }
        seq.end()
    }
}

/// The record as the publish body spells it (hub i-F19YZ): lowercase hashes and repo id.
struct RecordJson<'a>(&'a PublicationInputs);

impl Serialize for RecordJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let r = self.0;
        let mut m = s.serialize_map(Some(9))?;
        m.serialize_entry("repo_id", &r.repo_id.to_string())?;
        m.serialize_entry("tag", &r.tag)?;
        m.serialize_entry("landing", &hex(&r.landing))?;
        m.serialize_entry("profile", &r.profile)?;
        m.serialize_entry("selection", &SelectionRows(&r.selection))?;
        m.serialize_entry("selection_hash", &hex(&r.selection_hash))?;
        m.serialize_entry("manifest_recipe", &r.manifest_recipe)?;
        m.serialize_entry("manifest_hash", &hex(&r.manifest_hash))?;
        m.serialize_entry("render_inputs", &r.render_inputs)?;
        m.end()
    }
}

impl Serialize for PublishResult {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(11))?;
        m.serialize_entry("id", &self.id)?;
        m.serialize_entry("state", &self.state)?;
        m.serialize_entry("url", &self.url)?;
        m.serialize_entry("push", &self.push)?;
        m.serialize_entry("audience", &self.audience)?;
        m.serialize_entry("record", &RecordJson(&self.record))?;
        m.serialize_entry("references", &self.references)?;
        m.serialize_entry("inventory", &self.inventory)?;
        m.serialize_entry("warnings", &self.warnings)?;
        m.serialize_entry("render_warnings", &self.render_warnings)?;
        m.serialize_entry("review", &self.review)?;
        m.end()
    }
}

/// `E_LIMIT`: a result over [`MAX_RESULT_BYTES`].
fn result_too_large() -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("publication result exceeds {MAX_RESULT_BYTES} bytes"),
        "reduce the publication inputs or selection and try again",
    )
}

/// A sink that counts what it is given and refuses any byte past its cap, so an oversized value
/// stops serializing at the cap and nothing is kept.
struct Bounded {
    written: usize,
}

impl Write for Bounded {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.written.checked_add(buf.len()) {
            Some(n) if n <= MAX_RESULT_BYTES => {
                self.written = n;
                Ok(buf.len())
            }
            _ => Err(io::Error::other("over the result bound")),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The compact JSON length of `r`, or the result-limit refusal at more than
/// [`MAX_RESULT_BYTES`] bytes.
fn check_result_size(r: &PublishResult) -> Result<usize, RdyError> {
    let mut sink = Bounded { written: 0 };
    serde_json::to_writer(&mut sink, r).map_err(|_| result_too_large())?;
    Ok(sink.written)
}

/// The JSON of `r`, serialized through the bounded sink first: over [`MAX_RESULT_BYTES`] bytes is
/// `E_LIMIT`.
pub fn result_json(r: &PublishResult) -> Result<serde_json::Value, RdyError> {
    check_result_size(r)?;
    serde_json::to_value(r).map_err(|_| result_too_large())
}

// ---------------------------------------------------------------------------------------------
// Request checks and texts
// ---------------------------------------------------------------------------------------------

fn bad_input(message: impl Into<String>, fix: impl Into<String>) -> RdyError {
    RdyError::new(E_BAD_INPUT, message, fix)
}

/// The tag bound (HA1's upload bound) first, then the version grammar.
fn check_tag(tag: &str) -> Result<semver::Version, RdyError> {
    if tag.len() > MAX_UPLOAD_TAG_BYTES {
        return Err(RdyError::new(
            E_LIMIT,
            format!("upload tag exceeds {MAX_UPLOAD_TAG_BYTES} bytes"),
            "reduce the tag name to the stated limit",
        ));
    }
    parse_version(tag)
}

/// 1 to 64 ASCII letters, digits, underscores or hyphens, a letter or digit first (the wire's
/// site grammar).
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
            "the site name is invalid",
            "use 1 to 64 letters, digits, underscores or hyphens, starting with a letter or digit",
        ))
    }
}

fn not_publish_remote(remote: &str) -> RdyError {
    bad_input(
        format!("remote {remote} is not a publish remote"),
        "set its mode to publish, or choose a publish remote",
    )
}

fn repo_mismatch() -> RdyError {
    RdyError::new(
        E_REPO_MISMATCH,
        "the remote holds another source project",
        "choose a remote for this project",
    )
}

fn audience_mismatch() -> RdyError {
    bad_input(
        "the tagged profile audience does not match the site",
        "ask an owner to create a site for this audience",
    )
}

/// The upload phases a lost write can belong to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Objects,
    Files,
    Publish,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Phase::Objects => "objects",
            Phase::Files => "files",
            Phase::Publish => "publish",
        }
    }
}

/// `E_UPSTREAM_UNCERTAIN` for push `push`: the upload may have completed in `phase`. It carries
/// exactly one issue, which names the phase.
fn uncertain(push: &PushId, phase: Phase) -> RdyError {
    let fix = format!(
        "check push {} on the hub before publishing again",
        push.as_str()
    );
    let mut e = RdyError::new(
        E_UPSTREAM_UNCERTAIN,
        "the publication upload may have completed",
        fix.clone(),
    );
    e.issues = vec![Issue {
        severity: Severity::Error,
        code: E_UPSTREAM_UNCERTAIN,
        block: None,
        message: format!("upload phase {}", phase.as_str()),
        fix,
    }];
    e
}

/// `e`, the stale epoch found after the publish request was answered: it keeps its code and
/// gains the push-bearing issue of the publish phase, since the candidate may exist.
fn stale_after_publish(mut e: RdyError, push: &PushId) -> RdyError {
    e.issues.extend(uncertain(push, Phase::Publish).issues);
    e
}

/// `e` of a write step: a lost or invalid reply becomes the push-bearing uncertainty of `phase`;
/// any other error is unchanged.
fn in_phase(e: RdyError, push: &PushId, phase: Phase) -> RdyError {
    if e.code == E_UPSTREAM_UNCERTAIN {
        uncertain(push, phase)
    } else {
        e
    }
}

/// The hub's actor-claim refusal code (hub tb-68J50).
const E_HUB_ACTOR_CLAIM: &str = "E_HUB_ACTOR_CLAIM";

/// Whether `e` is a refused actor claim, the hub's or the core's.
fn actor_claim(e: &RdyError) -> bool {
    e.code == E_HUB_ACTOR_CLAIM || e.code == E_UPSTREAM_ACTOR_CLAIM
}

/// Whether `url` is an HTTPS page on `install` with no userinfo or fragment, within the URL
/// bound: the install's own fence, with no pin, admits it.
fn candidate_url_ok(install: &Origin, url: &str) -> bool {
    OriginFence::new(install.clone(), Vec::new())
        .and_then(|f| f.check(url))
        .is_ok_and(|o| o == *install)
}

/// The settings and credential warnings of a run, each once, in the order first seen.
#[derive(Default)]
struct Warnings(Vec<Issue>);

impl Warnings {
    fn add(&mut self, more: impl IntoIterator<Item = Issue>) {
        for w in more {
            if !self.0.contains(&w) {
                self.0.push(w);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------------------------

/// The selected publish remote and what its requests need: its URL, its install, the install's
/// CA file, the session's claim and that claim bound to the install's account. Fixed for the
/// whole run.
struct Target {
    url: String,
    install: Origin,
    ca_file: Option<PathBuf>,
    claim: RequestClaim,
    bound: RequestClaim,
}

/// The publication coordinator of one project for the user config at `user_file`, with the
/// credential transport `api`, the clock `clock` and the upload wire `wire`.
pub struct PublishService<'a> {
    project: PathBuf,
    user_file: PathBuf,
    api: &'a dyn HubTransport,
    clock: &'a dyn HubClock,
    wire: &'a dyn PublishWire,
}

/// The run's state that its error path reads: the warnings so far and whether `/me` was read
/// for a refused claim.
struct Run<'r> {
    warnings: Warnings,
    target: Option<&'r Target>,
    handle_read: bool,
}

impl<'a> PublishService<'a> {
    /// The coordinator of the project at `project` with the user config `user_file`
    /// (`<home>/rdy/config.yaml`), whose sibling `credentials.yaml` holds the sign-ins.
    pub fn new(
        project: &Path,
        user_file: &Path,
        api: &'a dyn HubTransport,
        clock: &'a dyn HubClock,
        wire: &'a dyn PublishWire,
    ) -> PublishService<'a> {
        PublishService {
            project: project.to_path_buf(),
            user_file: user_file.to_path_buf(),
            api,
            clock,
            wire,
        }
    }

    fn credentials(&self) -> HubCredentials<'a> {
        HubCredentials::new(Some(&self.project), &self.user_file, self.api, self.clock)
    }

    /// `e` as the run returns it: a refused actor claim first reads `/me` once and adds its
    /// warnings; then every warning collected so far is added to the issues.
    fn fail(&self, run: &mut Run<'_>, mut e: RdyError) -> RdyError {
        if actor_claim(&e)
            && let Some(t) = run.target
            && !std::mem::replace(&mut run.handle_read, true)
        {
            match self.credentials().refresh_handle(&t.install, &t.claim) {
                Ok(update) => run.warnings.add(update.warnings),
                Err(f) => run.warnings.add([Issue {
                    severity: Severity::Error,
                    code: f.code,
                    block: None,
                    message: f.message,
                    fix: f.fix,
                }]),
            }
        }
        e.issues.extend(run.warnings.0.iter().cloned());
        e
    }

    /// The context of one request on `t`: current access to its install (renewed when it
    /// expires), checked to hold the run's account and bound claim, then the source's
    /// redaction epoch.
    fn context(
        &self,
        t: &Target,
        account: &str,
        read: &ReadState,
        sync_base: Option<&str>,
    ) -> Result<UploadContext, RdyError> {
        let grant = self.credentials().access(&t.install, &t.claim)?;
        if grant.account != account || grant.claim != t.bound {
            return Err(upstream_auth(t.install.as_str()));
        }
        read.ensure_current_epoch()?;
        Ok(UploadContext {
            install: t.install.clone(),
            sync_base: sync_base.map(str::to_string),
            ca_file: t.ca_file.clone(),
            claim: grant.claim,
            bearer: grant.access_token,
        })
    }

    /// Publishes `request.tag` of `read` to the hub site `request.site` of the publish remote
    /// `request.to` (see the module docs), rendered by `renderer`. The result names a candidate
    /// to review on the hub.
    pub fn run(
        &self,
        read: Arc<ReadState>,
        session: &Session,
        request: PublishRequest,
        renderer: &dyn PublishRenderer,
    ) -> Result<PublishResult, RdyError> {
        // 1. the request, then the tag in this one snapshot
        let PublishRequest {
            tag,
            to,
            site,
            keep_latest,
        } = request;
        let want = check_tag(&tag)?;
        if !is_remote_name(&to) {
            return Err(bad_input(BAD_REMOTE_NAME_MESSAGE, BAD_REMOTE_NAME_FIX));
        }
        check_site(&site)?;
        let stored = stored_tag(&read, &tag, &want)?;
        let tag_hash = stored.id();

        // 2. the settings and the publish remote
        let settings = HubConfig::load(Some(&self.project), Some(&self.user_file))?;
        let mut run = Run {
            warnings: Warnings::default(),
            target: None,
            handle_read: false,
        };
        run.warnings.add(settings.warnings.iter().cloned());
        let remote = match settings.select_remote(Some(&to)) {
            Ok(r) => r,
            Err(e) => return Err(self.fail(&mut run, e)),
        };
        if remote.mode != RemoteMode::Publish {
            return Err(self.fail(&mut run, not_publish_remote(&remote.name)));
        }

        // 3. its install, CA file and pins; the session's claim
        let install = remote.url.origin().clone();
        if let Err(e) = settings.fence(&install) {
            return Err(self.fail(&mut run, e));
        }
        let ca_file = settings.install(&install).and_then(|i| i.ca_file.clone());
        let claim = match RequestClaim::from_session(session) {
            Ok(c) => c,
            Err(e) => return Err(self.fail(&mut run, e)),
        };
        // 4. access, discovery, state
        let first = match self.credentials().access(&install, &claim) {
            Ok(g) => g,
            Err(e) => return Err(self.fail(&mut run, e)),
        };
        let target = Target {
            url: remote.url.as_str().to_string(),
            install,
            ca_file,
            claim,
            bound: first.claim,
        };
        let account = first.account;
        run.target = Some(&target);
        let result = self.upload(
            &mut run,
            &target,
            &account,
            &read,
            Upload {
                stored: &stored,
                tag_hash,
                site: &site,
                keep_latest,
            },
            renderer,
        );
        result.map_err(|e| self.fail(&mut run, e))
    }

    /// Steps 4 to 11 on the selected `t` (see the module docs).
    fn upload(
        &self,
        run: &mut Run<'_>,
        t: &Target,
        account: &str,
        read: &ReadState,
        u: Upload<'_>,
        renderer: &dyn PublishRenderer,
    ) -> Result<PublishResult, RdyError> {
        let cx = self.context(t, account, read, None)?;
        let discovery = self.wire.discovery(&cx, &t.url)?;
        let base = discovery.sync_base;
        let cx = self.context(t, account, read, Some(&base))?;
        let state = self
            .wire
            .state(&cx, &StateRequest::new(&cx.claim, u.site))?;
        drop(cx);

        // 5. stream support and repo identity
        check_streams(&state)?;
        let repo_id = read.repo_id()?;
        if state.repo_id.is_some_and(|r| r != repo_id) {
            return Err(repo_mismatch());
        }

        // 6. the site's profile, resolved at the tag
        let scope = Scope::At(u.stored.landing);
        let selector = state
            .profile
            .as_ref()
            .map_or_else(|| IMPLICIT_ORG.to_string(), |p| p.id.to_string());
        let resolved = publication::resolve(read, &scope, &selector)?;

        // 7. the audience and the reference check
        let audience = resolved.profile.audience;
        if state.audience.is_some_and(|a| a != audience) {
            return Err(audience_mismatch());
        }
        resolved.require_output()?;

        // 8. the render inputs, the artifact and its check
        let inputs = renderer.capture(read, &scope)?;
        let rendered = renderer.render(read, &scope, &u.stored.name, &selector, &inputs)?;
        let limits = ArtifactLimits {
            bytes: state.artifact_bytes,
            files: u32::try_from(state.artifact_files).unwrap_or(u32::MAX),
        };
        let manifest_hash =
            crate::hub_publish_files::validate_artifact(&*rendered.artifact, limits)?;
        let record = record(repo_id, u.stored, &resolved, manifest_hash, inputs)?;

        // the reply is budgeted whole before the first upload request
        let mut result = PublishResult {
            id: "\"".repeat(BUDGET_ID_BYTES),
            state: "candidate".to_string(),
            url: "\"".repeat(BUDGET_ID_BYTES),
            push: "0".repeat(crate::hub_publish_wire::PUSH_ID_CHARS),
            audience,
            record,
            references: reference_summary(&resolved.references),
            inventory: inventory(&resolved),
            warnings: run.warnings.0.clone(),
            render_warnings: rendered.warnings,
            review: REVIEW.to_string(),
        };
        check_result_size(&result)?;
        drop(resolved);

        // 9. the tagged history and one push id
        let mut history = PreparedHistory::capture(read, u.tag_hash)?;
        let push = PushId::new();
        let mut context = || self.context(t, account, read, Some(&base));

        // 10. history, then files, then the epoch again
        send_history(
            &mut history,
            &push,
            BatchLimits {
                bytes: state.batch_bytes,
                records: state.batch_records,
            },
            self.wire,
            &mut context,
        )
        .map_err(|e| in_phase(e, &push, Phase::Objects))?;
        drop(history);
        upload_files(
            &*rendered.artifact,
            push.as_str(),
            limits,
            self.wire,
            &mut context,
        )
        .map_err(|e| in_phase(e, &push, Phase::Files))?;
        read.ensure_current_epoch()?;

        // 11. publish, then the candidate's checks
        let cx = context()?;
        let manifest: &Manifest = rendered.artifact.manifest();
        let candidate = publish_record(
            &result.record,
            manifest,
            push.as_str(),
            u.site,
            u.keep_latest,
            self.wire,
            cx,
        )
        .map_err(|e| in_phase(e, &push, Phase::Publish))?;
        // a redaction that landed while publish was in flight: no success, and the push id is
        // kept because the candidate may exist; nothing is sent again
        read.ensure_current_epoch()
            .map_err(|e| stale_after_publish(e, &push))?;
        let Candidate { id, state, url } = candidate;
        if state != "candidate" || !candidate_url_ok(&t.install, &url) {
            return Err(uncertain(&push, Phase::Publish));
        }
        result.id = id;
        result.state = state;
        result.url = url;
        result.push = push.as_str().to_string();
        // the budget held every bound part; a reply past it would still name the candidate
        check_result_size(&result).map_err(|_| uncertain(&push, Phase::Publish))?;
        Ok(result)
    }
}

/// What one run uploads besides the target: the stored tag and its hash, the site and the
/// optional `keep_latest`.
struct Upload<'u> {
    stored: &'u Tag,
    tag_hash: Hash,
    site: &'u str,
    keep_latest: Option<bool>,
}

/// The stored tag of `read` whose version is `want` (the request text `text`): its name and
/// landing are the record's.
fn stored_tag(read: &ReadState, text: &str, want: &semver::Version) -> Result<Tag, RdyError> {
    read.version_tags()?
        .into_iter()
        .find(|(v, _)| v.cmp_precedence(want) == Ordering::Equal)
        .map(|(_, t)| t)
        .ok_or_else(|| {
            RdyError::new(
                crate::E_NOT_FOUND,
                format!("no version {text}"),
                "list versions with rdy tags",
            )
        })
}

/// The hub reads this rdy's stream format (the wire checked it; checked again where it is used).
fn check_streams(state: &PublishState) -> Result<(), RdyError> {
    if state.stream_read.contains(&formats().stream) {
        Ok(())
    } else {
        Err(upstream_protocol())
    }
}

/// The record's inputs: the tag's stored name and landing, the tagged profile revision (none
/// for `implicit:org`), the full selection and its hash, the manifest recipe and hash, and the
/// captured render inputs.
fn record(
    repo_id: RepoId,
    tag: &Tag,
    resolved: &Resolved,
    manifest_hash: Hash,
    render_inputs: RenderInputs,
) -> Result<PublicationInputs, RdyError> {
    let profile = match (&resolved.profile.id, resolved.profile.version) {
        (Some(id), Some(version)) => Some(ProfileIdentity {
            id: BlockId::clone(id),
            version,
        }),
        (None, None) => None,
        _ => {
            return Err(RdyError::new(
                crate::E_INTERNAL,
                "a resolved profile has an id without a version",
                "report this as a bug in rdy",
            ));
        }
    };
    Ok(PublicationInputs {
        repo_id,
        tag: tag.name.clone(),
        landing: tag.landing,
        profile,
        selection: resolved.entries.clone(),
        selection_hash: resolved.selection_hash,
        manifest_recipe: MANIFEST_RECIPE,
        manifest_hash,
        render_inputs,
    })
}

/// The first [`INVENTORY_ROWS`] of each list, with the counts left out.
fn inventory(r: &Resolved) -> InventorySummary {
    let docs = r.docs.len().min(INVENTORY_ROWS);
    let attachments = r.attachments.len().min(INVENTORY_ROWS);
    InventorySummary {
        docs: r.docs[..docs].to_vec(),
        docs_omitted: (r.docs.len() - docs) as u64,
        attachments: r.attachments[..attachments].to_vec(),
        attachments_omitted: (r.attachments.len() - attachments) as u64,
        selection_count: r.entries.len() as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warning(message: &str) -> Issue {
        Issue {
            severity: Severity::Warning,
            code: "W_CONFIG_UNKNOWN",
            block: None,
            message: message.to_string(),
            fix: "remove it".to_string(),
        }
    }

    #[test]
    fn hpb14_warnings_keep_first_seen_order_without_exact_repeats() {
        let mut w = Warnings::default();
        w.add([warning("b"), warning("a"), warning("b")]);
        w.add([warning("a"), warning("c")]);
        let mut other = warning("a");
        other.fix = "another fix".to_string();
        w.add([other.clone()]);
        assert_eq!(
            w.0,
            vec![warning("b"), warning("a"), warning("c"), other],
            "first seen, each exact issue once; a differing fix is another issue"
        );
    }

    #[test]
    fn hpb14_a_candidate_page_is_an_https_page_on_the_install() {
        let install = Origin::parse("https://127.0.0.1:4430").unwrap();
        for ok in [
            "https://127.0.0.1:4430/app/candidates/c1",
            "https://127.0.0.1:4430/app/c?x=1",
        ] {
            assert!(candidate_url_ok(&install, ok), "{ok}");
        }
        for bad in [
            "http://127.0.0.1:4430/app/c",
            "https://127.0.0.1:4431/app/c",
            "https://user@127.0.0.1:4430/app/c",
            "https://127.0.0.1:4430/app/c#top",
            "/app/c",
            "",
        ] {
            assert!(!candidate_url_ok(&install, bad), "{bad}");
        }
    }

    #[test]
    fn hpb14_an_uncertain_upload_names_its_push_and_phase() {
        let push = PushId::from_bytes([0; 16]);
        let e = uncertain(&push, Phase::Files);
        let fix = "check push 00000000000000000000000000 on the hub before publishing again";
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_UPSTREAM_UNCERTAIN,
                "the publication upload may have completed",
                fix
            )
        );
        assert_eq!(e.issues.len(), 1);
        assert_eq!(e.issues[0].code, E_UPSTREAM_UNCERTAIN);
        assert_eq!(e.issues[0].message, "upload phase files");
        assert_eq!(e.issues[0].fix, fix);
        let other = in_phase(upstream_protocol(), &push, Phase::Objects);
        assert_eq!(other.code, crate::E_UPSTREAM_PROTOCOL, "only a lost write");
    }
}
