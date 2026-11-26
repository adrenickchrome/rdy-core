//! The sync exchange's wire steps that rdyhub's frozen wire fixture v2 covers (D-H0BZN):
//! discovery, the state step with named views, the known step in its push and pull forms, get,
//! put, write and the outcome lookup. Each reuses the publication upload's request, refusal and
//! reply code ([`HttpPublishWire`]): the origin fence, the closed header set, the actor binding,
//! the hub code table, and D-N8PR2's additive reply decoding with its token scan over every key
//! and string. Every JSON body goes as the sync media type.
//!
//! - **State.** The request names this rdy, the stream formats the client reads and writes,
//!   the bound actor and run, a null site and, only when the call names views, up to 256 view
//!   names, each once. The reply keeps publish's members and checks, except that an incompatible
//!   feature is known when the real feature registry names it (D-4XHPH), and no stream format
//!   is required here: the exchange checks the one it sends. Its `canon` is a hex string (Share
//!   off) or a landing's object key (Share on); its `views` names each named view in input
//!   order, with a head and a root that are both null or both view op keys.
//! - **Known, pull form.** One page of a history's keys after `after`, in core's export order,
//!   with `next`, the page's last key, or null at the end.
//! - **Get.** One format-4 batch of the named keys, streamed to the caller's sink within its
//!   bound, never held whole in memory.
//! - **Write.** The body is bounded at 16,777,216 bytes and its render inputs at their own
//!   bounds before it is sent. A refusal is the hub's error object; a write that may have reached
//!   the hub with no decodable reply is uncertain.
//! - **Outcome.** The outcome of a write by its push id: taken, held, refused or unknown.
//! - **Dump** (D-7CDEQ, wire fixture v5). `POST <base>/dump` names the id of a dump that the
//!   hub's dump job made, as it is. The reply is the binary dump file, streamed to the caller's
//!   sink, never held whole in memory. A refusal is the hub's error object, mapped as publish
//!   maps it: a dump that does not exist, is not ready or another account started is the hub's
//!   `E_HUB_NOT_FOUND`.
use crate::error::{E_BAD_INPUT, E_UPSTREAM_UNCERTAIN, RdyError, actor_unbound, upstream_protocol};
use crate::formats::formats;
use crate::hub_http::{HttpHubTransport, ReplyEncoding};
use crate::hub_publish_stream::PreparedObjectBody;
use crate::hub_publish_wire::{
    Discovery, Effect, HttpPublishWire, KNOWN_REPLY_BYTES, KNOWN_REQUEST_BYTES, KeyWire,
    MAX_JSON_VALUES, MAX_KNOWN_KEYS, ObjectPut, PostError, PublishState, PublishWire, REPLY_BYTES,
    STATE_REQUEST_BYTES, SYNC_MEDIA_TYPE, UploadContext, at_most, check_push, check_sync_state,
    code_ok, encode, formats_ok, hex, known_values, limits, nullable, reply, route,
};
use crate::hub_types::{LimitUnit, RequestClaim, Secret, hub_limit};
use crate::render_inputs::{DisagreementInput, RenderInputs, validate_inputs};
use crate::sync_views::{HubView, unique_names};
use rdy_model::Hash;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::{HISTORY_FORMAT, StreamPurpose};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::ops::{Deref, DerefMut};

/// The most views one state request names (D-H0BZN).
pub const MAX_STATE_VIEWS: usize = 256;
/// The bound of a write request's body (rdyhub-design D-3TBYC).
pub const WRITE_REQUEST_BYTES: usize = 16_777_216;

/// The state step's request (D-H0BZN, wire fixture v2). `actor` and `run` must be the context
/// claim's; a `None` run is JSON null. `site` is always null for a sync exchange. `views` is
/// sent only when the call names views.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SyncStateRequest {
    pub rdy: String,
    pub stream_read: Vec<u32>,
    pub stream_write: Vec<u32>,
    pub actor: String,
    pub run: Option<String>,
    pub site: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub views: Option<Vec<String>>,
}

impl SyncStateRequest {
    /// The request of this rdy under `claim`: its version, and Plan A's history stream format
    /// to read and write. It names no view.
    pub fn new(claim: &RequestClaim) -> SyncStateRequest {
        SyncStateRequest {
            rdy: formats().rdy,
            stream_read: vec![HISTORY_FORMAT],
            stream_write: vec![HISTORY_FORMAT],
            actor: claim.actor.clone(),
            run: claim.run.clone(),
            site: None,
            views: None,
        }
    }

    /// The request naming `names`; an empty list names no view.
    pub fn with_views(mut self, names: &[String]) -> SyncStateRequest {
        self.views = (!names.is_empty()).then(|| names.to_vec());
        self
    }
}

/// Refuses a list of view names that one state request cannot name: more than
/// [`MAX_STATE_VIEWS`] (`E_LIMIT`) or a name given twice (`E_BAD_INPUT`).
pub fn check_view_names(names: &[String]) -> Result<(), RdyError> {
    if names.len() > MAX_STATE_VIEWS {
        return Err(hub_limit(MAX_STATE_VIEWS, LimitUnit::Entries));
    }
    unique_names(names)
}

/// One named view in the state reply: its head and root view ops, or none when the hub repo has
/// no such view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HubViewState {
    pub name: String,
    pub head: Option<Hash>,
    pub root: Option<Hash>,
}

/// The sync state reply: publish's members and checks, with hub canon's tip read from either
/// form, and the named views in input order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncState {
    pub state: PublishState,
    pub views: Vec<HubViewState>,
}

impl SyncState {
    /// The reply `state` with no named view.
    pub fn of(state: PublishState) -> SyncState {
        SyncState {
            state,
            views: Vec::new(),
        }
    }

    /// The named views the hub repo holds, as Plan A's view rules read them.
    pub fn hub_views(&self) -> BTreeMap<String, HubView> {
        self.views
            .iter()
            .filter_map(|v| {
                Some((
                    v.name.clone(),
                    HubView {
                        root: v.root?,
                        head: v.head?,
                    },
                ))
            })
            .collect()
    }

    /// The hub's head of the named view `name`; None when the hub lacks it or it was not named.
    pub fn view_head(&self, name: &str) -> Option<Hash> {
        self.views
            .iter()
            .find(|v| v.name == name)
            .and_then(|v| v.head)
    }
}

impl Deref for SyncState {
    type Target = PublishState;

    fn deref(&self) -> &PublishState {
        &self.state
    }
}

impl DerefMut for SyncState {
    fn deref_mut(&mut self) -> &mut PublishState {
        &mut self.state
    }
}

#[derive(Deserialize)]
struct ViewDto {
    name: String,
    #[serde(deserialize_with = "nullable")]
    head: Option<KeyWire>,
    #[serde(deserialize_with = "nullable")]
    root: Option<KeyWire>,
}

/// Decodes a sync state reply `body` to a state request that named `names` (see the module
/// docs). Any reply outside the form is `E_UPSTREAM_PROTOCOL`.
pub fn decode_state(body: &[u8], names: &[String], bearer: &Secret) -> Result<SyncState, RdyError> {
    let mut value: serde_json::Value = reply(
        body,
        limits(REPLY_BYTES, MAX_JSON_VALUES),
        Effect::Read,
        bearer,
    )?;
    let obj = value.as_object_mut().ok_or_else(upstream_protocol)?;
    // canon: a hex string (Share off) or a landing's object key (Share on)
    if let Some(canon) = obj.get_mut("canon")
        && canon.is_object()
    {
        let key: KeyWire = serde_json::from_value(canon.take()).map_err(|_| upstream_protocol())?;
        if key.kind != kinds::LANDING {
            return Err(upstream_protocol());
        }
        *canon = serde_json::Value::String(key.hash.to_string());
    }
    let views = match obj.remove("views") {
        None => Vec::new(),
        Some(v) => {
            let dtos: Vec<ViewDto> =
                at_most::<_, _, MAX_STATE_VIEWS>(v).map_err(|_| upstream_protocol())?;
            dtos.into_iter()
                .map(|d| {
                    let head = view_op(d.head)?;
                    let root = view_op(d.root)?;
                    if head.is_some() != root.is_some() {
                        return Err(upstream_protocol());
                    }
                    Ok(HubViewState {
                        name: d.name,
                        head,
                        root,
                    })
                })
                .collect::<Result<Vec<_>, RdyError>>()?
        }
    };
    if views.len() != names.len() || views.iter().zip(names).any(|(v, n)| v.name != *n) {
        return Err(upstream_protocol());
    }
    let mut state: PublishState = serde_json::from_value(value).map_err(|_| upstream_protocol())?;
    if !check_sync_state(&mut state) {
        return Err(upstream_protocol());
    }
    Ok(SyncState { state, views })
}

/// A view op key's hash; another kind is a protocol failure.
fn view_op(k: Option<KeyWire>) -> Result<Option<Hash>, RdyError> {
    match k {
        None => Ok(None),
        Some(k) if k.kind == kinds::VIEW_OP => Ok(Some(k.hash)),
        Some(_) => Err(upstream_protocol()),
    }
}

/// Why a settings reply holds no pack (rdyhub-design D-BYZR0): the hub keeps no render inputs
/// for hub canon (`none`), or its kept gate settings name another pack's digest (`mismatch`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackReason {
    None,
    Mismatch,
}

/// The settings step's reply (D-D5J18, D-7PYYS, rdyhub-design D-BYZR0). The hub's own members
/// (`rdy` among them, as a version) are read here; the captured members are kept whole, each
/// None when null or absent, for the clone to read in full. A member this client does not know
/// is left out, as every reply allows (D-N8PR2).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SettingsBundle {
    /// The version of the rdy that captured the settings.
    pub rdy: Option<semver::Version>,
    pub pack_reason: Option<PackReason>,
    pub gate_source: Option<ObjectKey>,
    pub render_source: Option<ObjectKey>,
    pub pack: Option<serde_json::Value>,
    pub coverage_gate: Option<serde_json::Value>,
    pub coverage_target: Option<serde_json::Value>,
    pub decisions_home: Option<serde_json::Value>,
    pub propose: Option<serde_json::Value>,
    pub title: Option<serde_json::Value>,
    pub description: Option<serde_json::Value>,
}

/// The captured members of a settings reply, in the order a clone reads them.
pub const SETTINGS_MEMBERS: [&str; 7] = [
    "pack",
    "coverage_gate",
    "coverage_target",
    "decisions_home",
    "propose",
    "title",
    "description",
];

impl SettingsBundle {
    /// The captured member `name` of [`SETTINGS_MEMBERS`], None when the bundle lacks it.
    pub fn member(&self, name: &str) -> Option<&serde_json::Value> {
        match name {
            "pack" => self.pack.as_ref(),
            "coverage_gate" => self.coverage_gate.as_ref(),
            "coverage_target" => self.coverage_target.as_ref(),
            "decisions_home" => self.decisions_home.as_ref(),
            "propose" => self.propose.as_ref(),
            "title" => self.title.as_ref(),
            "description" => self.description.as_ref(),
            _ => None,
        }
    }
}

/// Decodes a settings reply `body`: at most 16,777,216 bytes, its members one level deeper than
/// a reply's depth bound and within eight render inputs' values. `pack_reason` is `none`,
/// `mismatch` or null; `rdy` is null or a version; each source is null or a landing's key.
/// Anything else is `E_UPSTREAM_PROTOCOL`.
pub fn decode_settings(body: &[u8], bearer: &Secret) -> Result<SettingsBundle, RdyError> {
    let limits = crate::hub_http::JsonLimits {
        bytes: KNOWN_REPLY_BYTES,
        depth: crate::hub_publish_wire::MAX_JSON_DEPTH + 1,
        values: 8 * crate::render_inputs::MAX_INPUT_VALUES + 64,
        scalar_bytes: crate::render_inputs::MAX_INPUT_SCALAR_BYTES,
    };
    let value: serde_json::Value = reply(body, limits, Effect::Read, bearer)?;
    let mut obj = match value {
        serde_json::Value::Object(o) => o,
        _ => return Err(upstream_protocol()),
    };
    let mut take = |name: &str| obj.remove(name).filter(|v| !v.is_null());
    let rdy = match take("rdy") {
        None => None,
        Some(serde_json::Value::String(t)) => {
            Some(semver::Version::parse(&t).map_err(|_| upstream_protocol())?)
        }
        Some(_) => return Err(upstream_protocol()),
    };
    let pack_reason = match take("pack_reason") {
        None => None,
        Some(serde_json::Value::String(t)) if t == "none" => Some(PackReason::None),
        Some(serde_json::Value::String(t)) if t == "mismatch" => Some(PackReason::Mismatch),
        Some(_) => return Err(upstream_protocol()),
    };
    let source = |v: Option<serde_json::Value>| -> Result<Option<ObjectKey>, RdyError> {
        match v {
            None => Ok(None),
            Some(v) => {
                let key: KeyWire = serde_json::from_value(v).map_err(|_| upstream_protocol())?;
                let key = ObjectKey::from(key);
                if key.kind != kinds::LANDING {
                    return Err(upstream_protocol());
                }
                Ok(Some(key))
            }
        }
    };
    let gate_source = source(take("gate_source"))?;
    let render_source = source(take("render_source"))?;
    let bundle = SettingsBundle {
        rdy,
        pack_reason,
        gate_source,
        render_source,
        pack: take("pack"),
        coverage_gate: take("coverage_gate"),
        coverage_target: take("coverage_target"),
        decisions_home: take("decisions_home"),
        propose: take("propose"),
        title: take("title"),
        description: take("description"),
    };
    // a pack comes with no reason; a reason comes with no pack
    if bundle.pack.is_some() == bundle.pack_reason.is_some() {
        return Err(upstream_protocol());
    }
    Ok(bundle)
}

/// The known step's pull form (D-H0BZN): the keys of `root`'s closure less `base`'s link
/// closure, after `after`, of a history of `purpose`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct KnownPull {
    pub after: Option<ObjectKey>,
    pub base: Option<ObjectKey>,
    pub purpose: StreamPurpose,
    pub root: ObjectKey,
}

/// One page of the pull form: its keys in the hub's order, and `next`, its last key, or None at
/// the end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownPage {
    pub keys: Vec<ObjectKey>,
    pub next: Option<ObjectKey>,
}

#[derive(Deserialize)]
struct KnownPageDto {
    #[serde(deserialize_with = "at_most::<_, _, MAX_KNOWN_KEYS>")]
    keys: Vec<KeyWire>,
    #[serde(deserialize_with = "nullable")]
    next: Option<KeyWire>,
}

/// The known pull reply `body`: at most 100,000 distinct keys, and `next` null or the last key.
pub fn decode_known_page(body: &[u8], bearer: &Secret) -> Result<KnownPage, RdyError> {
    // Per key, one element and two members: 3 values, with headroom for added members.
    let values = known_values(MAX_KNOWN_KEYS + 1, 3);
    let dto: KnownPageDto = reply(
        body,
        limits(KNOWN_REPLY_BYTES, values),
        Effect::Read,
        bearer,
    )?;
    let keys: Vec<ObjectKey> = dto.keys.into_iter().map(ObjectKey::from).collect();
    let next = dto.next.map(ObjectKey::from);
    let distinct = keys.iter().collect::<BTreeSet<_>>().len() == keys.len();
    let next_ok = next.is_none_or(|n| keys.last() == Some(&n));
    if !distinct || !next_ok {
        return Err(upstream_protocol());
    }
    Ok(KnownPage { keys, next })
}

/// The get step's request: the keys of one batch of a history of `purpose` rooted at `root`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GetRequest {
    pub keys: Vec<ObjectKey>,
    pub purpose: StreamPurpose,
    pub root: ObjectKey,
}

/// A write's kind (rdyhub-design v-B8E4M).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteKind {
    Landings,
    Force,
    View,
    /// A restore of a dump's canon (D-J3YA0): the hub chooses the seed or the force move.
    Restore,
}

/// One tag a write carries and the hub lacks, with the disagreements at its scope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WriteTag {
    pub name: String,
    pub disagreements: Vec<DisagreementInput>,
}

/// The `propose` member of the gate settings: rdy's cadence and changes count.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProposeGate {
    pub cadence: String,
    pub changes: u32,
}

/// The gate settings a canon write sends (D-0WWYR): the pack by its digest, and the settings
/// readiness reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GateSettings {
    pub coverage_gate: String,
    pub coverage_target: u32,
    pub decisions_home: Option<String>,
    #[serde(with = "hex")]
    pub pack_digest: Hash,
    pub propose: ProposeGate,
}

/// The write step's request. A canon write (`landings`, `force`) has no view and sends `gate`
/// and `render_inputs`; a view write names its view and sends no gate. A restore (`restore`)
/// has no view; its `gate` and `render_inputs` are null when the dump has none, and it sends
/// render inputs only with a gate (rdyhub-design i-B40VF, D-D5J18). The view writes of a
/// restore send no render inputs. `expected` is the hub tip or head the write expects, or None
/// for a seed or a new view.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WriteRequest {
    pub expected: Option<ObjectKey>,
    pub gate: Option<GateSettings>,
    pub kind: WriteKind,
    pub push: String,
    pub render_inputs: Option<RenderInputs>,
    pub tags: Vec<WriteTag>,
    pub view: Option<String>,
}

impl WriteRequest {
    /// The kind of object key the write's tip is.
    pub fn root_kind(&self) -> u8 {
        match self.kind {
            WriteKind::View => kinds::VIEW_OP,
            _ => kinds::LANDING,
        }
    }
}

/// The write body of `w`, checked: its push id, its kind's members, its render inputs within
/// their bounds and the body within [`WRITE_REQUEST_BYTES`]. Nothing is sent.
pub fn encode_write(w: &WriteRequest) -> Result<Vec<u8>, RdyError> {
    check_push(&w.push)?;
    let fits = match w.kind {
        WriteKind::Landings | WriteKind::Force => {
            w.gate.is_some() && w.render_inputs.is_some() && w.view.is_none()
        }
        WriteKind::View => w.gate.is_none() && w.view.is_some(),
        WriteKind::Restore => w.view.is_none() && (w.gate.is_some() || w.render_inputs.is_none()),
    };
    if !fits {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "the write's kind does not match its members",
            "send gate settings with a canon write, and a view name with a view write",
        ));
    }
    if w.expected.is_some_and(|k| k.kind != w.root_kind()) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "the write expects an object of another kind",
            "expect the hub's tip or view head",
        ));
    }
    if let Some(inputs) = &w.render_inputs {
        validate_inputs(inputs)?;
    }
    encode(w, WRITE_REQUEST_BYTES)
}

/// A request a landing request opened: its number and version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct HubRequest {
    pub number: u64,
    pub version: u64,
}

/// How the hub ended a write that it did not refuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    Taken,
    Held,
}

/// The write reply: the outcome, the tip or head after it, the request it opened and whether
/// the minimum rdy rose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteReply {
    pub outcome: WriteOutcome,
    pub tip: Option<ObjectKey>,
    pub request: Option<HubRequest>,
    pub min_rdy_raised: bool,
}

#[derive(Deserialize)]
struct WriteReplyDto {
    outcome: String,
    #[serde(deserialize_with = "nullable")]
    tip: Option<KeyWire>,
    #[serde(deserialize_with = "nullable")]
    request: Option<HubRequest>,
    min_rdy_raised: bool,
}

/// Why a write has no reply to apply.
/// The errors are boxed: an unboxed `RdyError` with the tag passes the workspace's
/// `large-error-threshold`.
#[derive(Debug)]
pub enum WriteFailure {
    /// The hub refused it: the hub's error object, mapped as publish maps it.
    Refused(Box<RdyError>),
    /// It may have reached the hub, and no decodable reply came.
    Uncertain,
    /// It was not sent: a local refusal, or a failure before the request left.
    NotSent(Box<RdyError>),
}

impl WriteFailure {
    /// The hub's refusal `e`.
    pub fn refused(e: RdyError) -> WriteFailure {
        WriteFailure::Refused(Box::new(e))
    }

    /// A write that never left: `e` says why.
    pub fn not_sent(e: RdyError) -> WriteFailure {
        WriteFailure::NotSent(Box::new(e))
    }
}

/// The write reply `body` to `w`: `taken` with a tip of the write's root kind, or `held` (a
/// canon write only) with its request. Anything else is uncertain.
pub fn decode_write(
    body: &[u8],
    w: &WriteRequest,
    bearer: &Secret,
) -> Result<WriteReply, WriteFailure> {
    let dto: WriteReplyDto = reply(
        body,
        limits(REPLY_BYTES, MAX_JSON_VALUES),
        Effect::Write,
        bearer,
    )
    .map_err(|_| WriteFailure::Uncertain)?;
    let tip: Option<ObjectKey> = dto.tip.map(ObjectKey::from);
    if tip.is_some_and(|t| t.kind != w.root_kind()) {
        return Err(WriteFailure::Uncertain);
    }
    let outcome = match dto.outcome.as_str() {
        "taken" if tip.is_some() => WriteOutcome::Taken,
        "held" if dto.request.is_some() && w.kind != WriteKind::View => WriteOutcome::Held,
        _ => return Err(WriteFailure::Uncertain),
    };
    Ok(WriteReply {
        outcome,
        tip,
        request: dto.request,
        min_rdy_raised: dto.min_rdy_raised,
    })
}

/// What the outcome lookup found for one push.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LookupOutcome {
    /// The write took: the tip or head after it.
    Taken { tip: ObjectKey },
    /// The hub held the write as a landing request.
    Held {
        request: HubRequest,
        tip: Option<ObjectKey>,
    },
    /// The write did not take: the hub's code.
    Refused { code: String },
    /// The hub knows no outcome of the push.
    Unknown,
}

/// The outcome reply: the outcome, the write's kind and view, and a held request's state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutcomeReply {
    pub outcome: LookupOutcome,
    pub kind: Option<String>,
    pub view: Option<String>,
    pub request_state: Option<String>,
}

#[derive(Deserialize)]
struct OutcomeDto {
    outcome: String,
    #[serde(deserialize_with = "nullable")]
    code: Option<String>,
    #[serde(deserialize_with = "nullable")]
    kind: Option<String>,
    #[serde(deserialize_with = "nullable")]
    request: Option<HubRequest>,
    #[serde(deserialize_with = "nullable")]
    request_state: Option<String>,
    #[serde(deserialize_with = "nullable")]
    tip: Option<KeyWire>,
    #[serde(deserialize_with = "nullable")]
    view: Option<String>,
}

/// The outcome reply `body`. Anything outside the form is `E_UPSTREAM_PROTOCOL`.
pub fn decode_outcome(body: &[u8], bearer: &Secret) -> Result<OutcomeReply, RdyError> {
    let dto: OutcomeDto = reply(
        body,
        limits(REPLY_BYTES, MAX_JSON_VALUES),
        Effect::Read,
        bearer,
    )?;
    let tip = dto.tip.map(ObjectKey::from);
    if tip.is_some_and(|t| t.kind != kinds::LANDING && t.kind != kinds::VIEW_OP) {
        return Err(upstream_protocol());
    }
    if dto
        .request_state
        .as_deref()
        .is_some_and(|s| !matches!(s, "open" | "landed" | "closed"))
    {
        return Err(upstream_protocol());
    }
    let outcome = match (dto.outcome.as_str(), tip, dto.request, dto.code) {
        ("taken", Some(tip), _, _) => LookupOutcome::Taken { tip },
        ("held", tip, Some(request), _) => LookupOutcome::Held { request, tip },
        ("refused", _, _, Some(code)) if code_ok(&code) => LookupOutcome::Refused { code },
        ("unknown", _, _, _) => LookupOutcome::Unknown,
        _ => return Err(upstream_protocol()),
    };
    Ok(OutcomeReply {
        outcome,
        kind: dto.kind,
        view: dto.view,
        request_state: dto.request_state,
    })
}

/// The dump step's request (D-7CDEQ, wire fixture v5): the id the hub's dump job gave, sent as
/// it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DumpRequest {
    pub dump: String,
}

/// The sync exchange's wire steps. [`HttpSyncWire`] sends them; tests inject another
/// implementation.
pub trait SyncWire: Send + Sync {
    /// `GET <remote>` with the sync media type.
    fn discovery(&self, cx: &UploadContext, remote: &str) -> Result<Discovery, RdyError>;
    /// `POST <base>/state`.
    fn state(&self, cx: &UploadContext, request: &SyncStateRequest) -> Result<SyncState, RdyError>;
    /// `POST <base>/known`, push form: the keys of `keys` that neither the hub's store nor the
    /// quarantine of `push` holds.
    fn known(
        &self,
        cx: &UploadContext,
        push: &str,
        keys: &[ObjectKey],
    ) -> Result<Vec<ObjectKey>, RdyError>;
    /// `POST <base>/objects/put?push=<push>`: one batch of `len` bytes.
    fn put(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut dyn Read,
        len: u64,
    ) -> Result<ObjectPut, RdyError>;
    /// `POST <base>/objects/put?push=<push>`: one prepared batch. The default sends the batch's
    /// own bytes through [`SyncWire::put`].
    fn put_prepared(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut PreparedObjectBody,
    ) -> Result<ObjectPut, RdyError> {
        let (raw, len) = body.identity()?;
        self.put(cx, push, raw, len)
    }
    /// `POST <base>/known`, pull form: one page.
    fn known_pull(&self, cx: &UploadContext, request: &KnownPull) -> Result<KnownPage, RdyError>;
    /// `POST <base>/objects/get`: the batch streamed to `sink`, at most `limit` bytes. Returns
    /// its bytes. The request offers zstd (`Accept-Encoding: zstd`); a reply that takes the offer
    /// is decoded as it arrives, and `limit` and the returned count are decoded bytes. The reply
    /// streams in pieces of at most
    /// [`UPLOAD_BUFFER_BYTES`](crate::hub_http::UPLOAD_BUFFER_BYTES), each written while the
    /// bytes so far are within `limit`; the first piece past it fails the get unwritten.
    fn get(
        &self,
        cx: &UploadContext,
        request: &GetRequest,
        sink: &mut dyn Write,
        limit: u64,
    ) -> Result<u64, RdyError>;
    /// `POST <base>/write`.
    fn write(&self, cx: &UploadContext, request: &WriteRequest)
    -> Result<WriteReply, WriteFailure>;
    /// `POST <base>/outcome`: the outcome of the write of push `push`.
    fn outcome(&self, cx: &UploadContext, push: &str) -> Result<OutcomeReply, RdyError>;
    /// `POST <base>/settings` with an empty body: the hub repo's settings bundle. A wire with no
    /// settings step refuses with `E_UPSTREAM_PROTOCOL`.
    fn settings(&self, _cx: &UploadContext) -> Result<SettingsBundle, RdyError> {
        Err(upstream_protocol())
    }
    /// `POST <base>/dump` (D-7CDEQ): the dump file streamed to `sink`, at most `limit` bytes, as
    /// [`SyncWire::get`] streams a batch. Returns its bytes. A wire with no dump step refuses
    /// with `E_UPSTREAM_PROTOCOL`.
    fn dump(
        &self,
        _cx: &UploadContext,
        _request: &DumpRequest,
        _sink: &mut dyn Write,
        _limit: u64,
    ) -> Result<u64, RdyError> {
        Err(upstream_protocol())
    }
}

/// The sync wire over [`HttpHubTransport::upload`], through the publication upload's code.
#[derive(Debug)]
pub struct HttpSyncWire {
    inner: HttpPublishWire,
}

impl HttpSyncWire {
    pub fn new(transport: HttpHubTransport) -> HttpSyncWire {
        HttpSyncWire {
            inner: HttpPublishWire::new(transport),
        }
    }

    /// A read step's POST of `json` to `<base>/<path>`: the reply body.
    fn read_post(
        &self,
        cx: &UploadContext,
        path: &[&str],
        json: Vec<u8>,
        reply_limit: usize,
    ) -> Result<Vec<u8>, RdyError> {
        let url = route(cx, path, &[])?;
        self.inner.post_as(
            cx,
            Effect::Read,
            &url,
            SYNC_MEDIA_TYPE,
            json,
            reply_limit,
            &[200],
        )
    }
}

/// The request's own checks: the context's claim, a version, 1 to 16 distinct formats, and at
/// most 256 distinct view names.
fn check_request(cx: &UploadContext, r: &SyncStateRequest) -> Result<(), RdyError> {
    if r.actor != cx.claim.actor || r.run != cx.claim.run {
        return Err(actor_unbound());
    }
    if semver::Version::parse(&r.rdy).is_err() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "the client version is not a version",
            "send this rdy's version",
        ));
    }
    if !formats_ok(&r.stream_read) || !formats_ok(&r.stream_write) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "the stream formats are not valid",
            "send 1 to 16 distinct stream formats",
        ));
    }
    if let Some(names) = &r.views {
        check_view_names(names)?;
    }
    Ok(())
}

/// A history request's own checks: a canon or view history whose root (and base) are its root
/// kind.
fn check_history(
    purpose: StreamPurpose,
    root: &ObjectKey,
    base: Option<&ObjectKey>,
) -> Result<(), RdyError> {
    let ok = matches!(
        purpose,
        StreamPurpose::CanonHistory | StreamPurpose::ViewHistory
    ) && root.kind == purpose.root_kind()
        && base.is_none_or(|b| b.kind == purpose.root_kind());
    if !ok {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "the history request names another kind of root",
            "name a landing for canon history, or a view op for view history",
        ));
    }
    Ok(())
}

impl SyncWire for HttpSyncWire {
    fn discovery(&self, cx: &UploadContext, remote: &str) -> Result<Discovery, RdyError> {
        self.inner.discovery(cx, remote)
    }

    fn state(&self, cx: &UploadContext, request: &SyncStateRequest) -> Result<SyncState, RdyError> {
        check_request(cx, request)?;
        let json = encode(request, STATE_REQUEST_BYTES)?;
        let body = self.read_post(cx, &["state"], json, REPLY_BYTES)?;
        let names = request.views.as_deref().unwrap_or_default();
        decode_state(&body, names, &cx.bearer)
    }

    fn known(
        &self,
        cx: &UploadContext,
        push: &str,
        keys: &[ObjectKey],
    ) -> Result<Vec<ObjectKey>, RdyError> {
        self.inner.known_as(cx, push, keys, SYNC_MEDIA_TYPE)
    }

    fn put(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut dyn Read,
        len: u64,
    ) -> Result<ObjectPut, RdyError> {
        self.inner.put_objects(cx, push, body, len)
    }

    fn put_prepared(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut PreparedObjectBody,
    ) -> Result<ObjectPut, RdyError> {
        self.inner.put_objects_prepared(cx, push, body)
    }

    fn known_pull(&self, cx: &UploadContext, request: &KnownPull) -> Result<KnownPage, RdyError> {
        check_history(request.purpose, &request.root, request.base.as_ref())?;
        let json = encode(request, KNOWN_REQUEST_BYTES)?;
        let body = self.read_post(cx, &["known"], json, KNOWN_REPLY_BYTES)?;
        decode_known_page(&body, &cx.bearer)
    }

    fn get(
        &self,
        cx: &UploadContext,
        request: &GetRequest,
        sink: &mut dyn Write,
        limit: u64,
    ) -> Result<u64, RdyError> {
        check_history(request.purpose, &request.root, None)?;
        if request.keys.is_empty() || request.keys.len() > MAX_KNOWN_KEYS {
            return Err(hub_limit(MAX_KNOWN_KEYS, LimitUnit::Entries));
        }
        let json = encode(request, KNOWN_REQUEST_BYTES)?;
        let url = route(cx, &["objects", "get"], &[])?;
        self.inner.post_streamed(
            cx,
            &url,
            SYNC_MEDIA_TYPE,
            json,
            sink,
            limit,
            ReplyEncoding::ZstdOrIdentity,
        )
    }

    fn write(
        &self,
        cx: &UploadContext,
        request: &WriteRequest,
    ) -> Result<WriteReply, WriteFailure> {
        let json = encode_write(request).map_err(WriteFailure::not_sent)?;
        let url = route(cx, &["write"], &[]).map_err(WriteFailure::not_sent)?;
        let sent = self.inner.post_split(
            cx,
            Effect::Write,
            &url,
            SYNC_MEDIA_TYPE,
            json,
            REPLY_BYTES,
            &[200],
        );
        match sent {
            Ok(body) => decode_write(&body, request, &cx.bearer),
            Err(PostError::Refused(e)) => Err(WriteFailure::refused(e)),
            Err(PostError::Other(e)) if e.code == E_UPSTREAM_UNCERTAIN => {
                Err(WriteFailure::Uncertain)
            }
            Err(PostError::Other(e)) => Err(WriteFailure::not_sent(e)),
        }
    }

    fn outcome(&self, cx: &UploadContext, push: &str) -> Result<OutcomeReply, RdyError> {
        check_push(push)?;
        #[derive(Serialize)]
        struct Body<'a> {
            push: &'a str,
        }
        let json = encode(&Body { push }, REPLY_BYTES)?;
        let body = self.read_post(cx, &["outcome"], json, REPLY_BYTES)?;
        decode_outcome(&body, &cx.bearer)
    }

    fn settings(&self, cx: &UploadContext) -> Result<SettingsBundle, RdyError> {
        let body = self.read_post(cx, &["settings"], b"{}".to_vec(), KNOWN_REPLY_BYTES)?;
        decode_settings(&body, &cx.bearer)
    }

    fn dump(
        &self,
        cx: &UploadContext,
        request: &DumpRequest,
        sink: &mut dyn Write,
        limit: u64,
    ) -> Result<u64, RdyError> {
        crate::sync_dump::check_dump_id(&request.dump)?;
        let json = encode(request, REPLY_BYTES)?;
        let url = route(cx, &["dump"], &[])?;
        self.inner.post_streamed(
            cx,
            &url,
            SYNC_MEDIA_TYPE,
            json,
            sink,
            limit,
            ReplyEncoding::Identity,
        )
    }
}
