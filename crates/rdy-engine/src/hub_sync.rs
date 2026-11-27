//! The sync exchange's push and pull (D-H0BZN, D-3RSCW, D-4XHPH, D-5Y8Y1, D-47HVF, D-0SC9C,
//! D-KE83X, D-DZDB7, D-QMZ92, D-0WWYR, D-QCE8M).
//!
//! 1. **Remote.** The named remote, else the share remote, else the only remote, else
//!    `E_REMOTE_REQUIRED`. A remote that is not a share remote is `E_BAD_INPUT`
//!    (`SYNC_REMOTE_MODE`). The named views pass Plan A's checks first: at most 256, each once.
//! 2. **Guard.** One push or pull per remote of a project runs in this process; a second is
//!    `E_LOCKED` and sends nothing ([`RemoteGuard`]).
//! 3. **Trust and access.** The install's origin fence, its `ca_file`, the session's bound
//!    claim and the machine's sign-in credential, as `rdy publish` uses them. Access is asked
//!    for immediately before every request.
//! 4. **Discovery, then state** with the named views. A push and a pull use only the tip and
//!    heads that their own state step returned.
//! 5. **Checks before any transfer.** The hub's `min_rdy` (`E_STORE_FORMAT`), the hub repo's id
//!    (`E_REPO_MISMATCH`) and the history stream format.
//! 6. **Outcome lookup.** A `pending` write found here is looked up before any known, put, get
//!    or write step: taken and held apply the write reply's rules (a taken tip other than the
//!    pushed head is fetched first), refused clears it, and unknown stops with the uncertain
//!    words until the quarantine life has passed since it was sent. A refusal of the lookup
//!    keeps it. An outcome is never inferred from store facts. While the hub still runs the
//!    write it answers the lookup `E_HUB_BUSY` with `retry_after`: the lookup waits that many
//!    seconds (1 to 5) and asks again, for at most 120 seconds and 82 asks, and then stops with
//!    the uncertain words, with `pending` kept and no write sent. A completed refusal that
//!    carries the code `E_HUB_BUSY` is a refusal, not a busy reply, and is never waited on. A
//!    taken or held outcome learned after a busy wait ends the write that the state of step 4
//!    saw running, so the push or pull takes a new state (steps 4 and 5) before any further
//!    sync work, and compares local state with that one only. A failure of that capture is
//!    returned; what the recovery applied stays.
//! 7. **Push.** One render-input capture per push, at the pushed head's scope, with the gate
//!    settings of the same read state. Canon first, then each named view in Plan A's push
//!    order. Each has its own push id, upload, known, put and write; a view write starts only
//!    after the canon write has its reply, and a canon refusal does not stop the views. Canon's
//!    base rule: a null hub tip seeds, a tip local canon holds is the base, any other tip is
//!    `E_DIVERGED` unless the push forces. While local canon's head is the held head, canon
//!    sends no write and reports the open request.
//! 8. **Write.** `[Upload (when this run wrote none), Send]`, then the write. `taken` records
//!    the tip as applied (and clears a held request the tip reaches); `held` records the held
//!    head and request; a refusal clears `pending` only. A write that may have reached the hub
//!    with no reply keeps `pending` and stops with `E_UPSTREAM_UNCERTAIN` and the sync words;
//!    it is never sent again. A write that was certainly not sent restores the upload.
//! 9. **Pull.** For hub canon and each named view: the known pull pages from this store's
//!    fetched tip or head (when the store holds it), the keys this store lacks fetched in
//!    batches within the hub's limits, each streamed to a scratch file and checked, then Plan
//!    A's fetch and replay. Before the fetch, a pull with a held request asks the outcome
//!    lookup for the held push and clears `held` when the request closed with no landing
//!    (D-PGQZM). An `unknown` answer clears `held` after hub canon applies; a refused lookup
//!    keeps it and warns `W_UPSTREAM_HELD_LOOKUP` (D-H67EN). The pull goes on in each case.
//! 10. **Restore.** A dump goes to the hub through a temporary store (`crate::sync_restore`).
//! 11. **Dump fetch.** The hub's dump goes to a new file through the state and dump steps, with
//!     no guard, store write or sync state (`crate::sync_dump`, D-7CDEQ).
//!
//! A hub refusal keeps the hub's message, fix and request id. `E_HUB_TAG_TAKEN` takes core's
//! code `E_TAG_EXISTS` with the hub's words (rdyhub-design i-B40VF).
use crate::engine::{Engine, with_writer_result};
use crate::error::{
    E_BAD_INPUT, E_DIVERGED, E_INTERNAL, E_IO, E_STORE_FORMAT, E_TAG_EXISTS,
    E_UPSTREAM_ACTOR_CLAIM, E_UPSTREAM_UNCERTAIN, RdyError, SYNC_DIVERGED_FIX,
    SYNC_DIVERGED_MESSAGE, SYNC_MIN_RDY_FIX, SYNC_MIN_RDY_MESSAGE, UPSTREAM_HELD_LOOKUP_FIX,
    UPSTREAM_HELD_LOOKUP_MESSAGE, W_UPSTREAM_HELD_LOOKUP, sync_uncertain, upstream_auth,
    upstream_protocol,
};
use crate::formats::formats;
use crate::hub_config::{BAD_REMOTE_NAME_FIX, BAD_REMOTE_NAME_MESSAGE, HubConfig, RemoteMode};
use crate::hub_credentials::HubCredentials;
use crate::hub_http::UPLOAD_BUFFER_BYTES;
use crate::hub_publish::{PublishRenderer, not_share_remote, repo_mismatch};
use crate::hub_publish_stream::{
    BatchLimits, HistorySent, ObjectWire, PreparedHistory, PreparedObjectBody, PushId, WorkerMark,
    missing_with, scratch_error, send_objects, send_root, upload_probe, upload_worker_setup,
};
use crate::hub_publish_wire::{MAX_KNOWN_KEYS, ObjectPut, PublishState, UploadContext};
use crate::hub_sync_wire::{
    GateSettings, GetRequest, HubRequest, KnownPull, LookupOutcome, OutcomeReply, ProposeGate,
    SyncState, SyncStateRequest, SyncWire, WriteFailure, WriteKind, WriteOutcome, WriteReply,
    WriteRequest, WriteTag, check_view_names, encode_write,
};
use crate::hub_types::{HubClock, HubTransport, Origin, RequestClaim};
use crate::landing_check::CheckInputs;
use crate::readiness::assessment::pack_digest;
use crate::render_inputs::{RenderInputs, validate_inputs};
use crate::session::{Scope, Session};
use crate::sync_fetch::{FetchOutcome, HubState, usable_canon_state, usable_view_state};
use crate::sync_restore::{RestoreOutcome, RestoreRequest};
use crate::sync_state::{RemoteGuard, remote_refs, write_steps};
use crate::sync_views::{
    ViewPullRequest, ViewPulled, pull_views, push_plan, view_diverged, view_missing,
};
use crate::{ReadState, hub_config, sync_replay};
use rdy_model::Hash;
use rdy_schema::Issue;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_state::{
    KIND_CANON, KIND_VIEW, SyncCanon, SyncHeld, SyncPending, SyncStep, SyncUpload, SyncView,
};
use rdy_store::sync_stream::{HISTORY_FORMAT, StreamPurpose};
use rdy_store::take_batch::FetchTarget;
use rdy_store::{StoreError, StoreReader};
use serde::Serialize;
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A push's request: the remote (none for the default one), the named views, and whether it
/// forces hub canon to local canon's head.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PushRequest {
    pub remote: Option<String>,
    pub views: Vec<String>,
    pub force: bool,
}

/// A pull's request: the remote (none for the default one), the named views, whether it takes
/// the hub's tip (or, with views, the hub's view heads) with no replay, and whether it stops at
/// the first replay.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PullRequest {
    pub remote: Option<String>,
    pub views: Vec<String>,
    pub take: bool,
    pub strict: bool,
}

/// How one canon or view upload of a push ended.
#[derive(Clone, Debug)]
pub enum WriteEnd {
    /// The hub tip or head is the local one, and the hub holds every attachment of its
    /// history: nothing was sent.
    UpToDate { tip: Option<Hash> },
    /// The hub took the write of push `push`: `head` above `base` (the hub tip or head it
    /// extended), now at `tip`.
    Taken {
        push: String,
        base: Option<Hash>,
        head: Hash,
        tip: Hash,
        resumed: bool,
        sent: HistorySent,
        min_rdy_raised: bool,
    },
    /// The hub held the write of push `push` as the landing request `request`.
    Held {
        push: String,
        head: Hash,
        request: HubRequest,
        resumed: bool,
        sent: HistorySent,
        min_rdy_raised: bool,
    },
    /// The write was refused: by the hub (the write of push `push`), or here before any request
    /// (no push).
    Refused {
        push: Option<String>,
        error: RdyError,
    },
    /// Local canon's head is the held head of the open request `request`: no write was sent.
    HeldOpen { head: Hash, request: u64 },
}

/// What the outcome lookup of a found `pending` write did: its push, kind and view, the hub's
/// answer, and whether a changed tip or head was fetched first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub push: String,
    pub kind: u8,
    pub view: Option<String>,
    pub outcome: LookupOutcome,
    pub fetched: bool,
}

/// How a push ended without an error: the remote, the outcome lookup of a found pending write,
/// canon's end, whether a forced push found force not needed, and each named view's end in the
/// order it was sent.
#[derive(Clone, Debug)]
pub struct PushOutcome {
    pub remote: String,
    pub lookup: Option<Resolved>,
    pub canon: WriteEnd,
    pub force_not_needed: bool,
    pub views: Vec<(String, WriteEnd)>,
}

/// How a pull ended without an error: the remote, the outcome lookup of a found pending write,
/// canon's fetch (None when nothing was fetched) and replay (None for a null hub canon), and
/// each named view's fetch and pull.
#[derive(Clone, Debug)]
pub struct PullOutcome {
    pub remote: String,
    pub lookup: Option<Resolved>,
    pub fetched: Option<FetchOutcome>,
    pub canon: Option<sync_replay::PullOutcome>,
    pub views_fetched: Vec<(String, FetchOutcome)>,
    pub views: Vec<ViewPulled>,
    /// The pull's warnings: `W_UPSTREAM_HELD_LOOKUP` when the hub refused the lookup of the
    /// held push (D-H67EN).
    pub warnings: Vec<SyncWarning>,
}

/// A warning of a push or pull: its code, message and fix.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SyncWarning {
    pub code: String,
    pub message: String,
    pub fix: String,
}

/// What a pull's lookup of the held push found (D-PGQZM, D-H67EN).
enum HeldLookup {
    /// No held request of this hub repo, or one the lookup keeps or cleared.
    Settled,
    /// The hub knows no outcome of held push `push`: `held` clears after hub canon applies.
    Unknown(String),
    /// The hub refused the lookup: `held` stays and the pull warns.
    Refused(SyncWarning),
}

/// Which canon move a preview names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    /// A forced push: hub canon moves to local canon's head.
    ForcePush,
    /// A pull that takes the hub tip: local canon moves to it.
    TakePull,
}

/// What a canon move removes, for the person's card: the remote, canon's tip before and after
/// the move, and each landing that leaves that canon, oldest first. `complete` is false when
/// this store lacks part of either chain: part of the chain before the move (the list may miss a
/// landing that leaves, and may be empty), or part of the chain kept (the list may name a
/// landing the move keeps). A complete preview needs that history here first, as a pull fetches
/// it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonPreview {
    pub remote: String,
    pub before: Option<Hash>,
    pub after: Option<Hash>,
    pub leaving: Vec<Hash>,
    pub complete: bool,
}

/// E_STORE_FORMAT when the hub repo `name` needs a newer rdy than this one (D-4XHPH).
pub fn check_minimum(state: &PublishState, name: &str) -> Result<(), RdyError> {
    let Some(min) = state.min_rdy.as_deref() else {
        return Ok(());
    };
    let (Ok(want), Ok(mine)) = (
        semver::Version::parse(min),
        semver::Version::parse(&formats().rdy),
    ) else {
        return Err(upstream_protocol());
    };
    if want <= mine {
        return Ok(());
    }
    Err(RdyError::new(
        E_STORE_FORMAT,
        SYNC_MIN_RDY_MESSAGE
            .replace("<name>", name)
            .replace("<version>", min),
        SYNC_MIN_RDY_FIX.replace("<version>", min),
    ))
}

/// E_DIVERGED: local canon does not hold the hub tip `tip`.
fn diverged(tip: &Hash) -> RdyError {
    RdyError::new(
        E_DIVERGED,
        SYNC_DIVERGED_MESSAGE.replace("<tip>", &tip.to_string()),
        SYNC_DIVERGED_FIX,
    )
}

/// The hub repo's name in the words of D-4XHPH: the remote URL's path without its slashes.
pub(crate) fn repo_name(url: &str) -> String {
    url::Url::parse(url)
        .map(|u| u.path().trim_matches('/').to_string())
        .unwrap_or_default()
}

/// Whether local canon, from `head` back, holds the landing `tip`.
fn canon_holds(read: &ReadState, head: Option<Hash>, tip: &Hash) -> Result<bool, RdyError> {
    let reader = read.reader();
    let mut at = head;
    while let Some(h) = at {
        if h == *tip {
            return Ok(true);
        }
        at = reader.landing(&h)?.prev;
    }
    Ok(false)
}

/// The landings of the chain from `head` back, newest first, as far as this store holds them,
/// and whether the walk reached the chain's first landing.
fn chain(reader: &StoreReader, head: Option<Hash>) -> (Vec<Hash>, bool) {
    let mut out = Vec::new();
    let mut at = head;
    while let Some(h) = at {
        match reader.landing(&h) {
            Ok(l) => {
                out.push(h);
                at = l.prev;
            }
            Err(_) => return (out, false),
        }
    }
    (out, true)
}

/// Whether the chain from `from` back holds `target`, as far as this store holds it.
fn chain_holds(reader: &StoreReader, from: Option<Hash>, target: &Hash) -> bool {
    chain(reader, from).0.contains(target)
}

/// What a canon move of `kind` removes, read from this store alone: no request and no write.
/// `hub_tip` is the hub's tip from a state step.
pub fn canon_preview(
    read: &ReadState,
    remote: &str,
    hub_tip: Option<Hash>,
    kind: PreviewKind,
) -> Result<CanonPreview, RdyError> {
    let local = read.canon_head();
    let (before, after) = match kind {
        PreviewKind::ForcePush => (hub_tip, local),
        PreviewKind::TakePull => (local, hub_tip),
    };
    Ok(preview_move(read.reader(), remote, before, after))
}

/// What a move of canon from `before` to `after` removes, read from `reader` alone (see
/// [`CanonPreview`]).
pub(crate) fn preview_move(
    reader: &StoreReader,
    remote: &str,
    before: Option<Hash>,
    after: Option<Hash>,
) -> CanonPreview {
    let (kept, kept_complete) = chain(reader, after);
    let kept: BTreeSet<Hash> = kept.into_iter().collect();
    // the chain before the move, back to the first landing it shares with the chain kept (all
    // older landings are shared too) or to its first landing
    let mut leaving = Vec::new();
    let mut gone_known = true;
    let mut at = before;
    while let Some(h) = at.filter(|h| !kept.contains(h)) {
        match reader.landing(&h) {
            Ok(l) => {
                leaving.push(h);
                at = l.prev;
            }
            Err(_) => {
                gone_known = false;
                break;
            }
        }
    }
    leaving.reverse();
    // a list is complete only when this store holds the chain that leaves, and holds enough of
    // the chain kept to show that no listed landing stays
    let complete = gone_known && (kept_complete || leaving.is_empty());
    CanonPreview {
        remote: remote.to_string(),
        before,
        after,
        leaving,
        complete,
    }
}

/// A hub refusal `e` of a write as core reports it: `E_HUB_TAG_TAKEN` is `E_TAG_EXISTS` with
/// the hub's words and request id (rdyhub-design i-B40VF); any other refusal is unchanged.
pub(crate) fn write_refusal(mut e: RdyError) -> RdyError {
    if e.code == "E_HUB_TAG_TAKEN" {
        e.code = E_TAG_EXISTS;
    }
    e
}

/// The gate settings of `read` (D-0WWYR): the settings readiness reads, and the pack of
/// `inputs` by its digest.
pub(crate) fn gate_settings(read: &ReadState, inputs: &RenderInputs) -> GateSettings {
    let gate = CheckInputs::of_state(read);
    GateSettings {
        coverage_gate: gate.coverage_gate.as_str().to_string(),
        coverage_target: gate.coverage_target,
        decisions_home: gate.decisions_home,
        pack_digest: pack_digest(&inputs.pack),
        propose: ProposeGate {
            cadence: gate.propose_cadence.as_str().to_string(),
            changes: gate.propose_changes,
        },
    }
}

/// E_BAD_INPUT: a pull that takes the hub tip cannot also stop at the first replay.
fn take_and_strict() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "a pull cannot both take the hub tip and stop at the first replay",
        "pass take or strict, not both",
    )
}

/// The selected remote's install and trust.
#[derive(Clone)]
pub(crate) struct Target {
    pub(crate) name: String,
    pub(crate) url: String,
    install: Origin,
    ca_file: Option<PathBuf>,
    claim: RequestClaim,
    bound: RequestClaim,
    account: String,
}

/// The message of [`capture_start_error`].
pub const CAPTURE_START_MESSAGE: &str = "the canon input check could not start";
/// The fix of [`capture_start_error`].
pub const CAPTURE_START_FIX: &str = "retry the push";
/// The message of [`capture_failed_error`].
pub const CAPTURE_FAILED_MESSAGE: &str = "the canon input check failed";
/// The fix of [`capture_failed_error`].
pub const CAPTURE_FAILED_FIX: &str = "retry the push; report the error if it repeats";

/// E_IO: the push's canon capture worker could not be started.
fn capture_start_error() -> RdyError {
    RdyError::new(E_IO, CAPTURE_START_MESSAGE, CAPTURE_START_FIX)
}

/// E_INTERNAL: the push's canon capture worker panicked. Its payload is never shown.
fn capture_failed_error() -> RdyError {
    RdyError::new(E_INTERNAL, CAPTURE_FAILED_MESSAGE, CAPTURE_FAILED_FIX)
}

/// A test's hook at the start of a push's canon walk, on the thread that walks.
type WalkHook = Arc<dyn Fn() + Send + Sync>;

thread_local! {
    static WALK_HOOK: std::cell::RefCell<Option<WalkHook>> = const { std::cell::RefCell::new(None) };
}

/// Runs `f` with `hook` called when a push on this thread starts its canon walk, while the
/// canon capture may run (a test hook; production installs none). The prior hook comes back
/// when `f` returns or panics.
#[doc(hidden)]
pub fn with_canon_walk_hook_for_tests<T>(hook: WalkHook, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<WalkHook>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let prior = self.0.take();
            WALK_HOOK.with(|c| *c.borrow_mut() = prior);
        }
    }
    let prior = WALK_HOOK.with(|c| c.borrow_mut().replace(hook));
    let _restore = Restore(prior);
    f()
}

/// The render inputs a push captured once, with the gate settings of the same read state, and
/// the head whose scope they hold.
struct Capture {
    inputs: RenderInputs,
    gate: GateSettings,
    head: Option<Hash>,
}

/// Whose upload the `upload` ref holds, as a push reads it.
enum Slot {
    Free,
    Canon(PushId),
    View(String, PushId),
    Foreign,
}

/// What canon's part of a push found before its upload: its end (with whether a forced push
/// found force not needed), or the history to send and what its write needs.
enum CanonPrep {
    End(WriteEnd, bool),
    Send {
        head: Hash,
        base: Option<Hash>,
        tip: Option<Hash>,
        holds: bool,
        history: PreparedHistory,
        force_not_needed: bool,
    },
}

/// One canon or view upload and its write.
struct Job {
    kind: WriteKind,
    view: Option<String>,
    head: Hash,
    base: Option<Hash>,
    expected: Option<Hash>,
    at_tip: bool,
    history: PreparedHistory,
    resumed: Option<PushId>,
}

/// The sync coordinator of one project for the user config at `user_file`, with the credential
/// transport `api`, the clock `clock`, the sync wire `wire` and the render-input capture of
/// `renderer`.
pub struct SyncService<'a> {
    project: PathBuf,
    /// A URL clone's coordinator: no project yet, so the credentials read the user config alone.
    projectless: bool,
    user_file: PathBuf,
    api: &'a dyn HubTransport,
    clock: &'a dyn HubClock,
    pub(crate) wire: &'a dyn SyncWire,
    /// Shared with a push's one capture worker, so it is `Sync`.
    pub(crate) renderer: &'a (dyn PublishRenderer + Sync),
    /// Where a restore makes its temporary store: the system temp root when None.
    pub(crate) scratch: Option<PathBuf>,
    /// The clock of a busy outcome lookup's waits: a private monotonic one when None.
    poll_clock: Option<&'a dyn OutcomePollClock>,
}

/// The hub's refusal of an outcome lookup while it still runs the write.
const HUB_BUSY: &str = "E_HUB_BUSY";
/// The most time the lookups and waits of one recovery take together, in milliseconds.
const OUTCOME_POLL_BUDGET_MS: u64 = 120_000;
/// The time one lookup may take at most (30 seconds and the byte-scaled part of the read
/// deadline for the largest outcome body and reply), kept free before every request.
const OUTCOME_LOOKUP_RESERVE_MS: u64 = 39_000;
/// The most lookups one recovery begins.
const OUTCOME_POLL_MAX_REQUESTS: u32 = 82;
/// The shortest and longest wait between two lookups, in seconds.
const OUTCOME_RETRY_MIN_SECONDS: u64 = 1;
const OUTCOME_RETRY_MAX_SECONDS: u64 = 5;

/// The monotonic time and the sleep of a busy outcome lookup. Production uses the system's;
/// a test injects a clock that moves only when it says so.
#[doc(hidden)]
pub trait OutcomePollClock: Send + Sync {
    /// The time since this clock began, never going back.
    fn now(&self) -> Duration;
    /// Waits for `delay`.
    fn wait(&self, delay: Duration);
}

/// The system's monotonic clock and thread sleep.
struct SystemPollClock {
    start: Instant,
}

impl SystemPollClock {
    fn new() -> SystemPollClock {
        SystemPollClock {
            start: Instant::now(),
        }
    }
}

impl OutcomePollClock for SystemPollClock {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }

    fn wait(&self, delay: Duration) {
        std::thread::sleep(delay);
    }
}

/// Asks `ask` for an outcome until it gives one, waiting between the hub's busy refusals. The
/// 120-second budget covers the asks and waits, and starts here. No ask begins unless 39
/// seconds (the longest an ask may take) remain and fewer than 82 have begun. A busy refusal
/// waits its `retry_after` clamped to 1 to 5 seconds, and ends the polling with `None` when that
/// wait and the next ask's reserve no longer fit. A refusal with another code, and any other
/// failure, is returned at once. Only the current reply is kept.
fn poll_outcome(
    clock: &dyn OutcomePollClock,
    mut ask: impl FnMut() -> Result<OutcomeReply, RdyError>,
) -> Result<Option<OutcomeReply>, RdyError> {
    let budget = Duration::from_millis(OUTCOME_POLL_BUDGET_MS);
    let reserve = Duration::from_millis(OUTCOME_LOOKUP_RESERVE_MS);
    let start = clock.now();
    let remaining = |now: Duration| budget.saturating_sub(now.saturating_sub(start));
    let mut begun = 0u32;
    loop {
        if begun >= OUTCOME_POLL_MAX_REQUESTS || remaining(clock.now()) < reserve {
            return Ok(None);
        }
        begun += 1;
        let e = match ask() {
            Ok(reply) => return Ok(Some(reply)),
            Err(e) if e.code == HUB_BUSY => e,
            Err(e) => return Err(e),
        };
        let seconds = e.upstream_retry_after.ok_or_else(upstream_protocol)?;
        let delay = Duration::from_secs(
            seconds.clamp(OUTCOME_RETRY_MIN_SECONDS, OUTCOME_RETRY_MAX_SECONDS),
        );
        if begun >= OUTCOME_POLL_MAX_REQUESTS
            || delay.saturating_add(reserve) > remaining(clock.now())
        {
            return Ok(None);
        }
        clock.wait(delay);
    }
}

/// The sync wire's known and put steps for the history sender.
pub(crate) struct SyncObjects<'a>(pub(crate) &'a dyn SyncWire);

impl ObjectWire for SyncObjects<'_> {
    fn known(
        &self,
        cx: &UploadContext,
        push: &str,
        keys: &[ObjectKey],
    ) -> Result<Vec<ObjectKey>, RdyError> {
        self.0.known(cx, push, keys)
    }

    fn put(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut dyn Read,
        len: u64,
    ) -> Result<ObjectPut, RdyError> {
        self.0.put(cx, push, body, len)
    }

    fn put_prepared(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut PreparedObjectBody,
    ) -> Result<ObjectPut, RdyError> {
        self.0.put_prepared(cx, push, body)
    }
}

fn store(e: StoreError) -> RdyError {
    e.into()
}

/// `E_LOCKED`: another upload or write of remote `remote` holds its sync state.
fn busy(remote: &str) -> RdyError {
    RdyError::from(StoreError::SyncBusy(remote.to_string()))
}

/// A canon or view end for an upload that waits behind another upload of the remote.
fn busy_end(remote: &str) -> WriteEnd {
    WriteEnd::Refused {
        push: None,
        error: busy(remote),
    }
}

impl<'a> SyncService<'a> {
    /// The coordinator of the project at `project` with the user config `user_file`
    /// (`<home>/rdy/config.yaml`), whose sibling `credentials.yaml` holds the sign-ins.
    pub fn new(
        project: &Path,
        user_file: &Path,
        api: &'a dyn HubTransport,
        clock: &'a dyn HubClock,
        wire: &'a dyn SyncWire,
        renderer: &'a (dyn PublishRenderer + Sync),
    ) -> SyncService<'a> {
        SyncService {
            project: project.to_path_buf(),
            user_file: user_file.to_path_buf(),
            api,
            clock,
            wire,
            renderer,
            scratch: None,
            projectless: false,
            poll_clock: None,
        }
    }

    /// The coordinator of a URL clone (`crate::clone::clone_url`): it has no project, so its
    /// credentials and trust come from the user config `user_file` alone. It runs only
    /// [`SyncService::clone_target`] and the clone steps below it.
    pub(crate) fn for_clone(
        user_file: &Path,
        api: &'a dyn HubTransport,
        clock: &'a dyn HubClock,
        wire: &'a dyn SyncWire,
        renderer: &'a (dyn PublishRenderer + Sync),
    ) -> SyncService<'a> {
        SyncService {
            projectless: true,
            ..SyncService::new(Path::new(""), user_file, api, clock, wire, renderer)
        }
    }

    /// This coordinator with its restores' temporary stores made under `root` in place of the
    /// system temp root (a test's private directory).
    #[doc(hidden)]
    pub fn scratch_in(mut self, root: &Path) -> SyncService<'a> {
        self.scratch = Some(root.to_path_buf());
        self
    }

    /// This coordinator with its busy outcome lookups timed and paced by `clock` in place of the
    /// system's monotonic clock and thread sleep (a test's fake clock).
    #[doc(hidden)]
    pub fn with_outcome_poll_clock(mut self, clock: &'a dyn OutcomePollClock) -> SyncService<'a> {
        self.poll_clock = Some(clock);
        self
    }

    /// The outcome of write `push`, asked again while the hub still runs it (module docs,
    /// "Busy outcome lookups"). `Some` is a decoded outcome, `Unknown` among them; `None` means
    /// the polling budget ran out with the hub still busy; an error is any other failure of the
    /// lookup. It never sends a write.
    pub(crate) fn lookup_outcome(
        &self,
        cx: &UploadContext,
        push: &str,
    ) -> Result<Option<OutcomeReply>, RdyError> {
        self.lookup_outcome_noted(cx, push).map(|(reply, _)| reply)
    }

    /// [`SyncService::lookup_outcome`] and whether the hub answered busy at least once, so the
    /// outcome was learned after a wait.
    fn lookup_outcome_noted(
        &self,
        cx: &UploadContext,
        push: &str,
    ) -> Result<(Option<OutcomeReply>, bool), RdyError> {
        let own = SystemPollClock::new();
        let clock: &dyn OutcomePollClock = match self.poll_clock {
            Some(c) => c,
            None => &own,
        };
        let mut asked = 0u32;
        let reply = poll_outcome(clock, || {
            asked += 1;
            self.wire.outcome(cx, push)
        })?;
        Ok((reply, asked > 1))
    }

    fn credentials(&self) -> HubCredentials<'a> {
        let project = (!self.projectless).then_some(self.project.as_path());
        HubCredentials::new(project, &self.user_file, self.api, self.clock)
    }

    /// The context of one request on `t`: current access to its install (renewed when it
    /// expires), checked to hold the run's account and bound claim, then the state's
    /// redaction epoch.
    pub(crate) fn context(
        &self,
        t: &Target,
        read: &ReadState,
        sync_base: Option<&str>,
    ) -> Result<UploadContext, RdyError> {
        self.context_at(t, Some(read), sync_base)
    }

    /// [`SyncService::context`] with no state to check when `read` is None: a URL clone's
    /// steps before its project exists.
    pub(crate) fn context_at(
        &self,
        t: &Target,
        read: Option<&ReadState>,
        sync_base: Option<&str>,
    ) -> Result<UploadContext, RdyError> {
        let grant = self.credentials().access(&t.install, &t.claim)?;
        if grant.account != t.account || grant.claim != t.bound {
            return Err(upstream_auth(t.install.as_str()));
        }
        if let Some(read) = read {
            read.ensure_current_epoch()?;
        }
        Ok(UploadContext {
            install: t.install.clone(),
            sync_base: sync_base.map(str::to_string),
            ca_file: t.ca_file.clone(),
            claim: grant.claim,
            bearer: grant.access_token,
            zstd: false,
        })
    }

    /// `e` with the settings' warnings, and with the account handle refreshed once when the hub
    /// refused the actor claim (as `rdy publish` does).
    pub(crate) fn fail(&self, t: Option<&Target>, warnings: &[Issue], mut e: RdyError) -> RdyError {
        if e.code == E_UPSTREAM_ACTOR_CLAIM
            && let Some(t) = t
        {
            match self.credentials().refresh_handle(&t.install, &t.claim) {
                Ok(update) => e.issues.extend(update.warnings),
                Err(f) => e.issues.push(Issue {
                    severity: rdy_schema::Severity::Error,
                    code: f.code,
                    block: None,
                    message: f.message,
                    fix: f.fix,
                }),
            }
        }
        e.issues.extend(warnings.iter().cloned());
        e
    }

    /// Steps 1 to 3: the remote, its guard and its trust; then `run` on them.
    pub(crate) fn with_target<T>(
        &self,
        session: &Session,
        remote: Option<&str>,
        run: impl FnOnce(&Target) -> Result<T, RdyError>,
    ) -> Result<T, RdyError> {
        self.with_target_guarded(session, remote, true, run)
    }

    /// [`SyncService::with_target`], with the remote's guard taken only when `guarded`: the
    /// fetch of a hub dump takes no sync lock (D-7CDEQ).
    pub(crate) fn with_target_guarded<T>(
        &self,
        session: &Session,
        remote: Option<&str>,
        guarded: bool,
        run: impl FnOnce(&Target) -> Result<T, RdyError>,
    ) -> Result<T, RdyError> {
        let (target, _guard, warnings) = self.resolve_target(session, remote, guarded)?;
        run(&target).map_err(|e| self.fail(Some(&target), &warnings, e))
    }

    /// Steps 1 to 3 without the run: the selected remote's target, its guard when `guarded`
    /// (held by the caller for as long as its sync runs) and the settings' warnings. A history
    /// fetch keeps all three across its steps ([`crate::history_fetch::HistoryFetch`]).
    pub(crate) fn resolve_target(
        &self,
        session: &Session,
        remote: Option<&str>,
        guarded: bool,
    ) -> Result<(Target, Option<RemoteGuard>, Vec<Issue>), RdyError> {
        // 1. the remote
        if let Some(name) = remote
            && !hub_config::is_remote_name(name)
        {
            return Err(RdyError::new(
                E_BAD_INPUT,
                BAD_REMOTE_NAME_MESSAGE,
                BAD_REMOTE_NAME_FIX,
            ));
        }
        let settings = HubConfig::load(Some(&self.project), Some(&self.user_file))?;
        let warnings = settings.warnings.clone();
        let remote = settings
            .select_remote(remote)
            .map_err(|e| self.fail(None, &warnings, e))?;
        if remote.mode != RemoteMode::Share {
            return Err(self.fail(None, &warnings, not_share_remote(&remote.name)));
        }

        // 2. one push or pull per remote
        let guard = if guarded {
            Some(RemoteGuard::take(&self.project, &remote.name)?)
        } else {
            None
        };

        // 3. the install, its CA file and pins; the session's claim; access
        let install = remote.url.origin().clone();
        settings
            .fence(&install)
            .map_err(|e| self.fail(None, &warnings, e))?;
        let ca_file = settings.install(&install).and_then(|i| i.ca_file.clone());
        let claim =
            RequestClaim::from_session(session).map_err(|e| self.fail(None, &warnings, e))?;
        let first = self
            .credentials()
            .access_on_use(&install, &claim)
            .map_err(|e| self.fail(None, &warnings, e))?;
        let target = Target {
            name: remote.name.clone(),
            url: remote.url.as_str().to_string(),
            install,
            ca_file,
            claim,
            bound: first.claim,
            account: first.account,
        };
        Ok((target, guard, warnings))
    }

    /// A URL clone's steps 1 to 3 (D-D5J18): the remote `hub` at `url`, refused with
    /// `E_REMOTE_EXISTS` when the user config already holds a share remote; then the install's
    /// fence, its `ca_file`, the session's claim and access, from the user config alone. No
    /// project exists yet, so no remote guard is taken: the clone's directory is new.
    pub(crate) fn clone_target<T>(
        &self,
        session: &Session,
        url: &crate::hub_types::RemoteUrl,
        run: impl FnOnce(&Target) -> Result<T, RdyError>,
    ) -> Result<T, RdyError> {
        self.url_target(session, url, true, run)
    }

    /// The steps of [`SyncService::clone_target`] for the hub repo at `url`, with the settings of
    /// this coordinator's project when it has one. A share remote in the settings is
    /// `E_REMOTE_EXISTS` only when `new_remote`: a URL clone makes one, the fetch of a hub dump
    /// (D-7CDEQ) makes none.
    pub(crate) fn url_target<T>(
        &self,
        session: &Session,
        url: &crate::hub_types::RemoteUrl,
        new_remote: bool,
        run: impl FnOnce(&Target) -> Result<T, RdyError>,
    ) -> Result<T, RdyError> {
        let project = (!self.projectless).then_some(self.project.as_path());
        let settings = HubConfig::load(project, Some(&self.user_file))?;
        let warnings = settings.warnings.clone();
        if new_remote
            && settings
                .remotes()
                .iter()
                .any(|r| r.mode == RemoteMode::Share)
        {
            return Err(self.fail(None, &warnings, crate::error::remote_exists()));
        }
        let install = url.origin().clone();
        settings
            .fence(&install)
            .map_err(|e| self.fail(None, &warnings, e))?;
        let ca_file = settings.install(&install).and_then(|i| i.ca_file.clone());
        let claim =
            RequestClaim::from_session(session).map_err(|e| self.fail(None, &warnings, e))?;
        let first = self
            .credentials()
            .access_on_use(&install, &claim)
            .map_err(|e| self.fail(None, &warnings, e))?;
        let target = Target {
            name: crate::clone::HUB_REMOTE.to_string(),
            url: url.as_str().to_string(),
            install,
            ca_file,
            claim,
            bound: first.claim,
            account: first.account,
        };
        run(&target).map_err(|e| self.fail(Some(&target), &warnings, e))
    }

    /// A URL clone's discovery and state step, then the `min_rdy` stop and the stream format
    /// check. No repo id is compared: the clone takes the hub repo's.
    pub(crate) fn clone_open(&self, t: &Target) -> Result<(String, SyncState), RdyError> {
        let cx = self.context_at(t, None, None)?;
        let base_url = self.wire.discovery(&cx, &t.url)?.sync_base;
        let cx = self.context_at(t, None, Some(&base_url))?;
        let state = self.wire.state(&cx, &SyncStateRequest::new(&cx.claim))?;
        drop(cx);
        check_minimum(&state, &repo_name(&t.url))?;
        if !state.stream_read.contains(&HISTORY_FORMAT)
            || !state.stream_write.contains(&HISTORY_FORMAT)
        {
            return Err(upstream_protocol());
        }
        Ok((base_url, state))
    }

    /// A URL clone's settings step.
    pub(crate) fn clone_settings(
        &self,
        t: &Target,
        base_url: &str,
    ) -> Result<crate::hub_sync_wire::SettingsBundle, RdyError> {
        let cx = self.context_at(t, None, Some(base_url))?;
        self.wire.settings(&cx)
    }

    /// A URL clone's fetch and replay into the new project of `engine`: hub canon's history
    /// rooted at the captured tip of `state`, with no base, as a pull fetches it (D-KE83X),
    /// then the pull's second batch, which moves the empty canon to that tip and records it as
    /// applied.
    pub(crate) fn clone_fetch(
        &self,
        engine: &dyn Engine,
        session: &Session,
        t: &Target,
        base_url: &str,
        state: &SyncState,
    ) -> Result<(Option<FetchOutcome>, sync_replay::PullOutcome), RdyError> {
        let tip = state.canon.ok_or_else(upstream_protocol)?;
        let fetched = self.fetch_history(
            engine,
            t,
            base_url,
            state,
            StreamPurpose::CanonHistory,
            tip,
            None,
        )?;
        let req = sync_replay::PullRequest {
            remote: t.name.clone(),
            hub: HubState {
                repo_id: state.repo_id,
                canon: state.canon,
            },
            take: false,
            strict: false,
        };
        let s = session.clone();
        let canon = with_writer_result(engine, move |rdy| sync_replay::pull(rdy, &s, &req))?;
        Ok((fetched, canon))
    }

    /// Pushes local canon and the named views to the share remote (see the module docs).
    pub fn push(
        &self,
        engine: &dyn Engine,
        session: &Session,
        request: PushRequest,
    ) -> Result<PushOutcome, RdyError> {
        check_view_names(&request.views)?;
        // a push sends history the project does not hold yet: refused before any capture
        engine.read().require_complete_history()?;
        self.with_target(session, request.remote.as_deref(), |t| {
            self.push_exchange(engine, t, &request)
        })
    }

    /// Pulls hub canon and the named views from the share remote (see the module docs).
    pub fn pull(
        &self,
        engine: &dyn Engine,
        session: &Session,
        request: PullRequest,
    ) -> Result<PullOutcome, RdyError> {
        if request.take && request.strict && request.views.is_empty() {
            return Err(take_and_strict());
        }
        check_view_names(&request.views)?;
        // a view's changes sit on history behind the frontier: refused before any fetch
        if !request.views.is_empty() {
            engine.read().require_complete_history()?;
        }
        self.with_target(session, request.remote.as_deref(), |t| {
            // the remote's incomplete history comes first, under this pull's own guard; the
            // pull then reads the hub's state again (D-GGTYA)
            crate::history_fetch::resume_under_guard(self, engine, session, t)?;
            self.pull_exchange(engine, session, t, &request)
        })
    }

    /// What a forced push or a taking pull would remove from canon (see [`canon_preview`]),
    /// after discovery and the state step. It sends no write and writes nothing.
    pub fn preview(
        &self,
        engine: &dyn Engine,
        session: &Session,
        remote: Option<&str>,
        kind: PreviewKind,
    ) -> Result<CanonPreview, RdyError> {
        self.with_target(session, remote, |t| {
            let read = engine.read();
            let (_, state) = self.open(t, &read, &[])?;
            canon_preview(&read, &t.name, state.canon, kind)
        })
    }

    /// Restores the dump of `request` into the hub repo of its share remote through a
    /// temporary store (`crate::sync_restore`). The caller's project gives the remote, its
    /// trust and the sign-in; its store is not read or written.
    pub fn restore(
        &self,
        session: &Session,
        request: RestoreRequest,
    ) -> Result<RestoreOutcome, RdyError> {
        crate::sync_restore::restore(self, session, &request)
    }

    /// [`SyncService::restore`] entered from the project of `engine` (`push --restore`): while
    /// that project's history is still arriving it is E_HISTORY_PENDING, before any capture,
    /// put or write (D-4PV25's refusal matrix).
    pub fn restore_from(
        &self,
        engine: &dyn Engine,
        session: &Session,
        request: RestoreRequest,
    ) -> Result<RestoreOutcome, RdyError> {
        engine.read().require_complete_history()?;
        self.restore(session, request)
    }

    /// [`SyncService::restore_preview`] entered from the project of `engine`, refused as
    /// [`SyncService::restore_from`] is while its history is still arriving.
    pub fn restore_preview_from(
        &self,
        engine: &dyn Engine,
        session: &Session,
        request: RestoreRequest,
    ) -> Result<CanonPreview, RdyError> {
        engine.read().require_complete_history()?;
        self.restore_preview(session, request)
    }

    /// What the restore of `request` would remove from hub canon (see [`CanonPreview`]): the
    /// remote, the hub tip before, the dump's tip after, and each landing of hub canon that the
    /// dump's store holds and its chain does not. It sends no write and writes nothing.
    pub fn restore_preview(
        &self,
        session: &Session,
        request: RestoreRequest,
    ) -> Result<CanonPreview, RdyError> {
        crate::sync_restore::preview(self, session, &request)
    }

    /// Steps 4 and 5: discovery, state with `views`, and the checks before any transfer.
    /// Returns the sync base and the state.
    pub(crate) fn open(
        &self,
        t: &Target,
        read: &ReadState,
        views: &[String],
    ) -> Result<(String, SyncState), RdyError> {
        let cx = self.context(t, read, None)?;
        let base_url = self.wire.discovery(&cx, &t.url)?.sync_base;
        let cx = self.context(t, read, Some(&base_url))?;
        let request = SyncStateRequest::new(&cx.claim).with_views(views);
        let state = self.wire.state(&cx, &request)?;
        drop(cx);
        check_minimum(&state, &repo_name(&t.url))?;
        let repo_id = read.repo_id()?;
        if state.repo_id.is_some_and(|r| r != repo_id) {
            return Err(repo_mismatch());
        }
        if !state.stream_read.contains(&HISTORY_FORMAT)
            || !state.stream_write.contains(&HISTORY_FORMAT)
        {
            return Err(upstream_protocol());
        }
        Ok((base_url, state))
    }

    /// Step 6 for the `pending` ref found on `t`. The flag is true when a taken or held outcome
    /// was learned after the hub answered busy: the caller's hub state predates it, and it takes a fresh one
    /// (`SyncService::recapture`) before any further sync work.
    fn lookup(
        &self,
        engine: &dyn Engine,
        t: &Target,
        base_url: &str,
        state: &SyncState,
    ) -> Result<(Option<Resolved>, bool), RdyError> {
        let Some(p) = remote_refs(engine, &t.name)?.pending else {
            return Ok((None, false));
        };
        let read = engine.read();
        if p.url != t.url || p.repo_id != read.repo_id()? {
            // another hub repo's write: never looked up or cleared here
            return Err(busy(&t.name));
        }
        let cx = self.context(t, &read, Some(base_url))?;
        let (reply, waited) = self.lookup_outcome_noted(&cx, &p.push)?;
        drop(cx);
        // the hub still runs the write: it may yet complete, so `pending` stays and no write goes
        let Some(reply) = reply else {
            return Err(sync_uncertain(&p.push));
        };
        let want = if p.kind == KIND_VIEW {
            kinds::VIEW_OP
        } else {
            kinds::LANDING
        };
        let mut fetched = false;
        match &reply.outcome {
            LookupOutcome::Taken { tip } => {
                if tip.kind != want {
                    return Err(upstream_protocol());
                }
                fetched = self.apply_taken(engine, t, base_url, state, &p, tip.hash)?;
            }
            LookupOutcome::Held { request, .. } => {
                self.apply_held(engine, t, &p, request.number)?
            }
            LookupOutcome::Refused { .. } => self.reply_only(engine, t, &p.push)?,
            LookupOutcome::Unknown => {
                let life = i64::try_from(state.quarantine_seconds.saturating_mul(1000))
                    .unwrap_or(i64::MAX);
                if self.clock.now_ms().saturating_sub(p.at_ms) < life {
                    // a late write may still take the quarantine
                    return Err(sync_uncertain(&p.push));
                }
                self.reply_only(engine, t, &p.push)?;
            }
        }
        // only a taken or held outcome changes what the hub's state means for this store
        let stale = waited
            && matches!(
                reply.outcome,
                LookupOutcome::Taken { .. } | LookupOutcome::Held { .. }
            );
        let resolved = Resolved {
            push: p.push,
            kind: p.kind,
            view: p.view,
            outcome: reply.outcome,
            fetched,
        };
        Ok((Some(resolved), stale))
    }

    /// The discovery and state steps again, for the hub state a recovery made stale: the write
    /// the first capture saw running has ended (D-H0BZN, D-GS59E). A failure is returned; what
    /// the recovery applied stays.
    fn recapture(
        &self,
        engine: &dyn Engine,
        t: &Target,
        views: &[String],
    ) -> Result<(String, SyncState), RdyError> {
        let read = engine.read();
        self.open(t, &read, views)
    }

    /// Clears `pending` of push `push`.
    fn reply_only(&self, engine: &dyn Engine, t: &Target, push: &str) -> Result<(), RdyError> {
        write_steps(
            engine,
            &t.name,
            vec![SyncStep::Reply {
                push: push.to_string(),
            }],
        )
    }

    /// The taken rule for the write `p`, whose tip or head is now `tip`: a tip other than the
    /// pushed head is fetched first (D-KE83X); then `pending` clears and `applied` is `tip`. A
    /// canon tip that reaches the held head clears `held`. Returns whether it fetched.
    fn apply_taken(
        &self,
        engine: &dyn Engine,
        t: &Target,
        base_url: &str,
        state: &SyncState,
        p: &SyncPending,
        tip: Hash,
    ) -> Result<bool, RdyError> {
        let view = if p.kind == KIND_VIEW {
            Some(p.view.clone().ok_or_else(upstream_protocol)?)
        } else {
            None
        };
        let fetched = tip != p.head;
        if fetched {
            let purpose = if view.is_some() {
                StreamPurpose::ViewHistory
            } else {
                StreamPurpose::CanonHistory
            };
            self.fetch_history(engine, t, base_url, state, purpose, tip, view.as_deref())?;
        }
        let read = engine.read();
        let reader = read.reader();
        let step = match view {
            Some(name) => {
                let cur = reader
                    .sync_view(&t.name, &name)
                    .map_err(store)?
                    .filter(|c| c.repo_id == p.repo_id);
                let root = match &cur {
                    Some(c) => c.root,
                    None => reader.view_chain_root(&name, &p.head).map_err(store)?,
                };
                SyncStep::View {
                    name,
                    value: SyncView {
                        repo_id: p.repo_id,
                        url: t.url.clone(),
                        root,
                        fetched: cur.and_then(|c| c.fetched),
                        applied: Some(tip),
                    },
                }
            }
            None => {
                let cur = reader
                    .sync_canon(&t.name)
                    .map_err(store)?
                    .filter(|c| c.repo_id == p.repo_id);
                let held = cur
                    .as_ref()
                    .and_then(|c| c.held.clone())
                    .filter(|h| !chain_holds(reader, Some(tip), &h.head));
                SyncStep::Canon(SyncCanon {
                    repo_id: p.repo_id,
                    url: t.url.clone(),
                    fetched: cur.and_then(|c| c.fetched),
                    applied: Some(tip),
                    held,
                })
            }
        };
        write_steps(
            engine,
            &t.name,
            vec![
                SyncStep::Reply {
                    push: p.push.clone(),
                },
                step,
            ],
        )?;
        Ok(fetched)
    }

    /// The held rule for the write `p`: `pending` clears, and a canon write records its head,
    /// request `number` and push id as held, in the same batch (D-PGQZM). `applied` does not
    /// move (D-QCE8M).
    fn apply_held(
        &self,
        engine: &dyn Engine,
        t: &Target,
        p: &SyncPending,
        number: u64,
    ) -> Result<(), RdyError> {
        let mut steps = vec![SyncStep::Reply {
            push: p.push.clone(),
        }];
        if p.kind == KIND_CANON {
            let read = engine.read();
            let cur = read
                .reader()
                .sync_canon(&t.name)
                .map_err(store)?
                .filter(|c| c.repo_id == p.repo_id);
            steps.push(SyncStep::Canon(SyncCanon {
                repo_id: p.repo_id,
                url: t.url.clone(),
                fetched: cur.as_ref().and_then(|c| c.fetched),
                applied: cur.as_ref().and_then(|c| c.applied),
                held: Some(SyncHeld {
                    head: p.head,
                    request: number,
                    push: p.push.clone(),
                }),
            }));
        }
        write_steps(engine, &t.name, steps)
    }

    /// The render inputs at the scope of local canon's head and the gate settings, from `read`.
    fn capture(&self, read: &ReadState) -> Result<Capture, RdyError> {
        capture_with(self.renderer, read)
    }

    /// The push's capture and canon's walk (`walk`), from the one captured `read`: with a local
    /// canon head, the capture runs on one scoped worker while this thread walks, and both end
    /// before anything is sent or recorded; with none, they run here in turn. When both fail,
    /// the capture's error is returned, as when the capture ran first; the worker is always
    /// joined. On success the read's epoch is checked again.
    fn capture_and_walk<T>(
        &self,
        read: &Arc<ReadState>,
        walk: impl FnOnce() -> Result<T, RdyError>,
    ) -> Result<(Capture, T), RdyError> {
        if read.canon_head().is_none() {
            let capture = self.capture(read)?;
            return Ok((capture, walk()?));
        }
        let renderer = self.renderer;
        let setup = upload_worker_setup();
        let probe = upload_probe();
        let hook = WALK_HOOK.with(|c| c.borrow().clone());
        let both = std::thread::scope(|s| {
            let worker = std::thread::Builder::new()
                .name("rdy-canon-capture".into())
                .spawn_scoped(s, move || {
                    let _guards = setup.as_ref().map(|f| f());
                    let _mark = WorkerMark::new(probe);
                    capture_with(renderer, read)
                })
                .map_err(|_| capture_start_error())?;
            if let Some(hook) = &hook {
                hook();
            }
            let walked = walk();
            let captured = worker
                .join()
                .unwrap_or_else(|_| Err(capture_failed_error()));
            match (captured, walked) {
                (Err(e), _) | (Ok(_), Err(e)) => Err(e),
                (Ok(c), Ok(w)) => Ok((c, w)),
            }
        })?;
        read.ensure_current_epoch()?;
        Ok(both)
    }

    /// The tags a write carries that the hub lacks: the TAG records of `pool` (the keys known
    /// named missing, or a resumed upload's whole record set), each with the disagreements at
    /// its scope, by name.
    fn tags(
        &self,
        read: &ReadState,
        capture: &Capture,
        pool: &[ObjectKey],
    ) -> Result<Vec<WriteTag>, RdyError> {
        let mut out = Vec::new();
        for k in pool.iter().filter(|k| k.kind == kinds::TAG) {
            let tag = read.reader().read_tag(&k.hash).map_err(store)?;
            let disagreements = if Some(tag.landing) == capture.head {
                capture.inputs.disagreements.clone()
            } else {
                self.renderer
                    .capture(read, &Scope::At(tag.landing))?
                    .disagreements
            };
            out.push(WriteTag {
                name: tag.name,
                disagreements,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Whose upload the `upload` ref of `t` holds, for a push naming `views`.
    fn slot(&self, engine: &dyn Engine, t: &Target, views: &[String]) -> Result<Slot, RdyError> {
        let Some(u) = remote_refs(engine, &t.name)?.upload else {
            return Ok(Slot::Free);
        };
        let repo_id = engine.read().repo_id()?;
        if u.url != t.url || u.repo_id != repo_id {
            return Ok(Slot::Foreign);
        }
        let Some(push) = PushId::parse(&u.push) else {
            return Ok(Slot::Foreign);
        };
        Ok(match (u.kind, u.view) {
            (KIND_CANON, None) => Slot::Canon(push),
            (KIND_VIEW, Some(v)) if views.contains(&v) => Slot::View(v, push),
            _ => Slot::Foreign,
        })
    }

    /// Steps 4 to 8 of a push on the selected `t`.
    fn push_exchange(
        &self,
        engine: &dyn Engine,
        t: &Target,
        request: &PushRequest,
    ) -> Result<PushOutcome, RdyError> {
        let read = engine.read();
        let (base_url, state) = self.open(t, &read, &request.views)?;
        let hub_views = state.hub_views();
        // the view rules refuse before any write
        if !request.views.is_empty() {
            push_plan(&read, &request.views, &hub_views)?;
        }
        drop(read);
        let (lookup, waited) = self.lookup(engine, t, &base_url, &state)?;
        // an outcome learned after a busy wait ends the write the capture saw running: the rest
        // of the push goes on a fresh capture, with the view rules checked again
        let (base_url, state, hub_views) = if waited {
            let (base_url, state) = self.recapture(engine, t, &request.views)?;
            let hub_views = state.hub_views();
            if !request.views.is_empty() {
                push_plan(&engine.read(), &request.views, &hub_views)?;
            }
            (base_url, state, hub_views)
        } else {
            (base_url, state, hub_views)
        };

        let read = engine.read();
        let (capture, prep) =
            self.capture_and_walk(&read, || self.canon_prepare(t, &read, &state, request))?;
        let (canon, force_not_needed) =
            self.push_canon(engine, t, &base_url, &read, &state, &capture, request, prep)?;
        let mut views = Vec::new();
        if !request.views.is_empty() {
            for plan in push_plan(&read, &request.views, &hub_views)? {
                let name = plan.view.clone();
                if !plan.holds_hub_head {
                    let base = plan.base.map(|b| b.to_string()).unwrap_or_default();
                    let error = view_diverged(&name, &base);
                    views.push((name, WriteEnd::Refused { push: None, error }));
                    continue;
                }
                let history = PreparedHistory::view(Arc::clone(&read), plan.head, plan.base)?;
                let at_tip = plan.base == Some(plan.head);
                let resumed = match self.slot(engine, t, &request.views)? {
                    Slot::View(v, p) if v == name => Some(p),
                    Slot::Free => None,
                    _ if at_tip && history.keys().is_empty() => None,
                    _ => {
                        views.push((name, busy_end(&t.name)));
                        continue;
                    }
                };
                let end = if at_tip && history.keys().is_empty() && resumed.is_none() {
                    WriteEnd::UpToDate {
                        tip: Some(plan.head),
                    }
                } else {
                    let job = Job {
                        kind: WriteKind::View,
                        view: Some(name.clone()),
                        head: plan.head,
                        base: plan.base,
                        expected: plan.base,
                        at_tip,
                        history,
                        resumed,
                    };
                    self.send_job(engine, t, &base_url, &read, &state, &capture, job)?
                };
                views.push((name, end));
            }
        }
        Ok(PushOutcome {
            remote: t.name.clone(),
            lookup,
            canon,
            force_not_needed,
            views,
        })
    }

    /// Canon's part of a push before its upload: the held rule, the base rule (or force), then
    /// the history's walk from the captured `read`. Nothing is sent or recorded.
    fn canon_prepare(
        &self,
        t: &Target,
        read: &Arc<ReadState>,
        state: &SyncState,
        request: &PushRequest,
    ) -> Result<CanonPrep, RdyError> {
        let repo_id = read.repo_id()?;
        let head = read.canon_head();
        let tip = state.canon;
        let holds = match &tip {
            Some(tip) => canon_holds(read, head, tip)?,
            None => true,
        };
        if let Some(tip) = &tip
            && !holds
            && (!request.force || head.is_none())
        {
            return Err(diverged(tip));
        }
        let force_not_needed = request.force && holds;
        let Some(head) = head else {
            // both are empty: the base rule refused any hub tip
            return Ok(CanonPrep::End(WriteEnd::UpToDate { tip }, force_not_needed));
        };
        // the held rule: at the held head, no write (D-QCE8M)
        let held = read
            .reader()
            .sync_canon(&t.name)
            .map_err(store)?
            .filter(|c| c.repo_id == repo_id && c.url == t.url)
            .and_then(|c| c.held);
        if let Some(h) = held
            && h.head == head
        {
            let end = WriteEnd::HeldOpen {
                head,
                request: h.request,
            };
            return Ok(CanonPrep::End(end, force_not_needed));
        }
        let base = if holds { tip } else { None };
        // at the hub tip, the history is the attachments made since (D-KE83X)
        let history = PreparedHistory::canon(Arc::clone(read), head, base)?;
        Ok(CanonPrep::Send {
            head,
            base,
            tip,
            holds,
            history,
            force_not_needed,
        })
    }

    /// Canon's part of a push after [`SyncService::canon_prepare`]: its upload and write.
    /// Returns its end and whether a forced push found force not needed.
    #[allow(clippy::too_many_arguments)]
    fn push_canon(
        &self,
        engine: &dyn Engine,
        t: &Target,
        base_url: &str,
        read: &Arc<ReadState>,
        state: &SyncState,
        capture: &Capture,
        request: &PushRequest,
        prep: CanonPrep,
    ) -> Result<(WriteEnd, bool), RdyError> {
        let (head, base, tip, holds, history, force_not_needed) = match prep {
            CanonPrep::End(end, force_not_needed) => return Ok((end, force_not_needed)),
            CanonPrep::Send {
                head,
                base,
                tip,
                holds,
                history,
                force_not_needed,
            } => (head, base, tip, holds, history, force_not_needed),
        };
        let at_tip = tip == Some(head);
        let nothing = at_tip && history.keys().is_empty();
        let resumed = match self.slot(engine, t, &request.views)? {
            Slot::Canon(p) => Some(p),
            Slot::Free => None,
            _ if nothing => None,
            // a stopped upload of a view this push names waits for its own write first
            Slot::View(..) => return Ok((busy_end(&t.name), force_not_needed)),
            // another upload of this remote waits for its write: never replaced
            Slot::Foreign => return Err(busy(&t.name)),
        };
        if nothing && resumed.is_none() {
            return Ok((WriteEnd::UpToDate { tip }, force_not_needed));
        }
        let kind = if holds {
            WriteKind::Landings
        } else {
            WriteKind::Force
        };
        let job = Job {
            kind,
            view: None,
            head,
            base,
            expected: tip,
            at_tip,
            history,
            resumed,
        };
        let end = self.send_job(engine, t, base_url, read, state, capture, job)?;
        Ok((end, force_not_needed))
    }

    /// One upload and its write (step 8).
    #[allow(clippy::too_many_arguments)]
    fn send_job(
        &self,
        engine: &dyn Engine,
        t: &Target,
        base_url: &str,
        read: &Arc<ReadState>,
        state: &SyncState,
        capture: &Capture,
        job: Job,
    ) -> Result<WriteEnd, RdyError> {
        let repo_id = read.repo_id()?;
        let keys = job.history.keys();
        let was_resumed = job.resumed.is_some();
        let push = job.resumed.clone().unwrap_or_else(PushId::new);
        let id = push.as_str().to_string();
        let ref_kind = if job.kind == WriteKind::View {
            KIND_VIEW
        } else {
            KIND_CANON
        };
        let upload = SyncUpload {
            repo_id,
            url: t.url.clone(),
            push: id.clone(),
            kind: ref_kind,
            view: job.view.clone(),
            at_ms: self.clock.now_ms(),
        };
        let wire = SyncObjects(self.wire);
        // the put bodies go as zstd when the state reply says the hub takes it (D-SEGWK)
        let zstd = state.zstd;
        let mut context = || {
            self.context(t, read, Some(base_url))
                .map(|cx| UploadContext { zstd, ..cx })
        };
        // a new canon push to a hub repo that holds nothing (no repo id, no canon, no view
        // head) lacks every key: the known step is skipped, and the first put opens the
        // quarantine (r-H7H64). The final write's compare still decides a racing seed.
        let empty_hub = job.kind != WriteKind::View
            && !was_resumed
            && state.repo_id.is_none()
            && state.canon.is_none()
            && state.views.iter().all(|v| v.head.is_none())
            && !keys.is_empty();
        let missing = if empty_hub {
            keys.clone()
        } else {
            missing_with(&keys, &push, &wire, &mut context)?
        };
        let limits = BatchLimits {
            bytes: state.batch_bytes,
            records: state.batch_records,
        };
        let mut wrote = false;
        let mut sent = HistorySent::default();
        let mut before_put = || {
            if !was_resumed {
                write_steps(engine, &t.name, vec![SyncStep::Upload(upload.clone())])?;
                wrote = true;
            }
            Ok(())
        };
        let mut result = send_objects(
            &job.history,
            &missing,
            &push,
            limits,
            &wire,
            &mut context,
            &mut before_put,
            &mut sent,
        );
        if result.is_ok() && !job.at_tip && sent.batches == 0 {
            // the hub holds every record, yet the write moves canon or the view: a batch of no
            // record names the head the write moves to, after the upload is recorded
            result = send_root(
                &job.history,
                &push,
                limits,
                &wire,
                &mut context,
                &mut before_put,
                &mut sent,
            );
        }
        if let Err(e) = result {
            // a failed push clears only its own upload, and only when no put of it may have
            // reached the quarantine
            if wrote && sent.batches == 0 && e.code != E_UPSTREAM_UNCERTAIN {
                let _ = write_steps(
                    engine,
                    &t.name,
                    vec![SyncStep::ClearUpload { push: id.clone() }],
                );
            }
            return Err(e);
        }
        if job.at_tip && sent.batches == 0 && !was_resumed {
            // the hub holds every attachment: no upload was recorded
            return Ok(WriteEnd::UpToDate {
                tip: Some(job.head),
            });
        }

        // the write: checked before it is recorded, so a local refusal leaves no pending
        let pool = if was_resumed { &keys } else { &missing };
        let tags = self.tags(read, capture, pool)?;
        let root_kind = if job.kind == WriteKind::View {
            kinds::VIEW_OP
        } else {
            kinds::LANDING
        };
        let request = WriteRequest {
            expected: job.expected.map(|hash| ObjectKey {
                kind: root_kind,
                hash,
            }),
            gate: (job.kind != WriteKind::View).then(|| capture.gate.clone()),
            kind: job.kind,
            push: id.clone(),
            render_inputs: Some(capture.inputs.clone()),
            tags,
            view: job.view.clone(),
        };
        encode_write(&request)?;
        let cx = self.context(t, read, Some(base_url))?;
        let pending = SyncPending {
            repo_id,
            url: t.url.clone(),
            push: id.clone(),
            kind: ref_kind,
            view: job.view.clone(),
            // the store keeps a pending tip only when it holds it: a forced write's hub tip
            // may be one this store never fetched; the lookup reads the push id alone
            expected: match job.expected {
                Some(h) if read.reader().has_kind(root_kind, &h).map_err(store)? => Some(h),
                _ => None,
            },
            head: job.head,
            at_ms: self.clock.now_ms(),
        };
        let mut steps = Vec::new();
        if !was_resumed && !wrote {
            steps.push(SyncStep::Upload(upload.clone()));
        }
        steps.push(SyncStep::Send(pending.clone()));
        write_steps(engine, &t.name, steps)?;
        let reply: WriteReply = match self.wire.write(&cx, &request) {
            Ok(r) => r,
            Err(WriteFailure::Refused(error)) => {
                self.reply_only(engine, t, &id)?;
                return Ok(WriteEnd::Refused {
                    push: Some(id),
                    error: write_refusal(*error),
                });
            }
            Err(WriteFailure::Uncertain) => return Err(sync_uncertain(&id)),
            Err(WriteFailure::NotSent(e)) => {
                // certainly not sent: the upload comes back, so the next push resumes it
                let _ = write_steps(
                    engine,
                    &t.name,
                    vec![
                        SyncStep::Reply { push: id.clone() },
                        SyncStep::Upload(upload),
                    ],
                );
                return Err(*e);
            }
        };
        drop(cx);
        match (reply.outcome, reply.tip, reply.request) {
            (WriteOutcome::Taken, Some(tip), _) => {
                self.apply_taken(engine, t, base_url, state, &pending, tip.hash)?;
                Ok(WriteEnd::Taken {
                    push: id,
                    base: job.base,
                    head: job.head,
                    tip: tip.hash,
                    resumed: was_resumed,
                    sent,
                    min_rdy_raised: reply.min_rdy_raised,
                })
            }
            (WriteOutcome::Held, _, Some(request)) => {
                self.apply_held(engine, t, &pending, request.number)?;
                Ok(WriteEnd::Held {
                    push: id,
                    head: job.head,
                    request,
                    resumed: was_resumed,
                    sent,
                    min_rdy_raised: reply.min_rdy_raised,
                })
            }
            // a reply the wire should not have passed: not one to apply
            _ => Err(sync_uncertain(&id)),
        }
    }

    /// Steps 4 to 6 and 9 of a pull on the selected `t`.
    fn pull_exchange(
        &self,
        engine: &dyn Engine,
        session: &Session,
        t: &Target,
        request: &PullRequest,
    ) -> Result<PullOutcome, RdyError> {
        let read = engine.read();
        let (base_url, state) = self.open(t, &read, &request.views)?;
        drop(read);
        for name in &request.views {
            if state.view_head(name).is_none() {
                return Err(view_missing(name, &t.name));
            }
        }
        let (lookup, waited) = self.lookup(engine, t, &base_url, &state)?;
        // an outcome learned after a busy wait ends the write the capture saw running: the rest
        // of the pull compares local state with a fresh capture, never the old one
        let (base_url, state) = if waited {
            let (base_url, state) = self.recapture(engine, t, &request.views)?;
            for name in &request.views {
                if state.view_head(name).is_none() {
                    return Err(view_missing(name, &t.name));
                }
            }
            (base_url, state)
        } else {
            (base_url, state)
        };
        let held = self.held_lookup(engine, t, &base_url)?;
        let hub = HubState {
            repo_id: state.repo_id,
            canon: state.canon,
        };
        let mut out = PullOutcome {
            remote: t.name.clone(),
            lookup,
            fetched: None,
            canon: None,
            views_fetched: Vec::new(),
            views: Vec::new(),
            warnings: Vec::new(),
        };
        if let HeldLookup::Refused(w) = &held {
            out.warnings.push(w.clone());
        }
        if let Some(tip) = state.canon {
            out.fetched = self.fetch_history(
                engine,
                t,
                &base_url,
                &state,
                StreamPurpose::CanonHistory,
                tip,
                None,
            )?;
            let req = sync_replay::PullRequest {
                remote: t.name.clone(),
                hub,
                // with views, take takes only those views (D-4E32E)
                take: request.take && request.views.is_empty(),
                strict: request.strict,
            };
            let s = session.clone();
            out.canon = Some(with_writer_result(engine, move |rdy| {
                sync_replay::pull(rdy, &s, &req)
            })?);
            self.clear_held(engine, t)?;
        }
        if let HeldLookup::Unknown(push) = &held {
            // the request closed with no landing and its outcome aged out (D-H67EN)
            self.drop_held(engine, t, push)?;
        }
        if !request.views.is_empty() {
            let mut named = Vec::new();
            for name in &request.views {
                let head = state
                    .view_head(name)
                    .ok_or_else(|| view_missing(name, &t.name))?;
                if let Some(f) = self.fetch_history(
                    engine,
                    t,
                    &base_url,
                    &state,
                    StreamPurpose::ViewHistory,
                    head,
                    Some(name),
                )? {
                    out.views_fetched.push((name.clone(), f));
                }
                named.push((name.clone(), Some(head)));
            }
            let req = ViewPullRequest {
                remote: t.name.clone(),
                hub,
                views: named,
                take: request.take,
            };
            let s = session.clone();
            out.views = with_writer_result(engine, move |rdy| pull_views(rdy, &s, &req))?;
        }
        Ok(out)
    }

    /// A pull's lookup of the held push (D-PGQZM "Clear", D-H67EN): with `held` set for this
    /// hub repo, the outcome lookup of `held.push`. A held outcome whose `request_state` is
    /// `closed` (the request closed with no landing) clears `held`; `open`, `landed` and any
    /// other outcome keep it. `unknown` keeps it through the fetch (the caller clears it after
    /// hub canon applies). A refusal of the lookup keeps it and gives `W_UPSTREAM_HELD_LOOKUP`;
    /// the pull goes on in every case.
    fn held_lookup(
        &self,
        engine: &dyn Engine,
        t: &Target,
        base_url: &str,
    ) -> Result<HeldLookup, RdyError> {
        let read = engine.read();
        let Some(c) = read.reader().sync_canon(&t.name).map_err(store)? else {
            return Ok(HeldLookup::Settled);
        };
        let Some(h) = &c.held else {
            return Ok(HeldLookup::Settled);
        };
        if c.url != t.url || c.repo_id != read.repo_id()? {
            // another hub repo's request: never looked up or cleared here
            return Ok(HeldLookup::Settled);
        }
        let push = h.push.clone();
        let cx = self.context(t, &read, Some(base_url))?;
        let reply = self.wire.outcome(&cx, &push);
        drop(cx);
        drop(read);
        let reply = match reply {
            Ok(r) => r,
            Err(e) => {
                let fix = if e.fix.is_empty() {
                    UPSTREAM_HELD_LOOKUP_FIX.to_string()
                } else {
                    e.fix
                };
                return Ok(HeldLookup::Refused(SyncWarning {
                    code: W_UPSTREAM_HELD_LOOKUP.to_string(),
                    message: UPSTREAM_HELD_LOOKUP_MESSAGE
                        .replace("<push>", &push)
                        .replace("<code>", e.code),
                    fix,
                }));
            }
        };
        match reply.outcome {
            LookupOutcome::Held { .. } if reply.request_state.as_deref() == Some("closed") => {
                self.drop_held(engine, t, &push)?;
                Ok(HeldLookup::Settled)
            }
            LookupOutcome::Unknown => Ok(HeldLookup::Unknown(push)),
            _ => Ok(HeldLookup::Settled),
        }
    }

    /// Clears `held` of `t` when it is still the request of push `push`.
    fn drop_held(&self, engine: &dyn Engine, t: &Target, push: &str) -> Result<(), RdyError> {
        let read = engine.read();
        let Some(c) = read.reader().sync_canon(&t.name).map_err(store)? else {
            return Ok(());
        };
        if c.held.as_ref().is_none_or(|h| h.push != push) {
            return Ok(());
        }
        drop(read);
        let step = SyncStep::Canon(SyncCanon { held: None, ..c });
        write_steps(engine, &t.name, vec![step])
    }

    /// Clears the held request of `t` once `applied` reaches its head (D-H0BZN "Held").
    fn clear_held(&self, engine: &dyn Engine, t: &Target) -> Result<(), RdyError> {
        let read = engine.read();
        let reader = read.reader();
        let Some(c) = reader.sync_canon(&t.name).map_err(store)? else {
            return Ok(());
        };
        let Some(h) = &c.held else {
            return Ok(());
        };
        if !chain_holds(reader, c.applied, &h.head) {
            return Ok(());
        }
        let step = SyncStep::Canon(SyncCanon { held: None, ..c });
        write_steps(engine, &t.name, vec![step])
    }

    /// The fetch batch of one history (D-KE83X): the known pull pages of `root` from this
    /// store's fetched tip or head (when it holds it), the keys it lacks fetched in batches
    /// within the hub's limits, each streamed to a scratch file and checked, then Plan A's
    /// fetch. None when the store already fetched `root` and lacks no key of it.
    #[allow(clippy::too_many_arguments)]
    fn fetch_history(
        &self,
        engine: &dyn Engine,
        t: &Target,
        base_url: &str,
        state: &SyncState,
        purpose: StreamPurpose,
        root: Hash,
        view: Option<&str>,
    ) -> Result<Option<FetchOutcome>, RdyError> {
        let read = engine.read();
        let reader = read.reader();
        let repo_id = read.repo_id()?;
        let hub = HubState {
            repo_id: state.repo_id,
            canon: state.canon,
        };
        let kind = purpose.root_kind();
        let fetched = match view {
            None => usable_canon_state(reader, &t.name, &hub)?.and_then(|s| s.fetched),
            Some(n) => usable_view_state(reader, &t.name, n, &hub)?.and_then(|s| s.fetched),
        };
        let mut base = None;
        if let Some(f) = fetched
            && reader.has_kind(kind, &f).map_err(store)?
        {
            base = Some(ObjectKey { kind, hash: f });
        }
        let root_key = ObjectKey { kind, hash: root };
        // the known pages, in the hub's order; the keys this store holds stay home
        let mut want = Vec::new();
        for k in self.known_listing(&read, t, base_url, purpose, root_key, base)? {
            if !reader.has_kind(k.kind, &k.hash).map_err(store)? {
                want.push(k);
            }
        }
        if want.is_empty() {
            if fetched == Some(root) {
                return Ok(None);
            }
            // the store holds the whole history: record the fetched tip or head with no get
            let Some(hub_repo) = state.repo_id else {
                return Err(upstream_protocol());
            };
            if !reader.has_kind(kind, &root).map_err(store)? {
                return Err(upstream_protocol());
            }
            let step = match view {
                None => {
                    let old = reader
                        .sync_canon(&t.name)
                        .map_err(store)?
                        .filter(|o| o.repo_id == hub_repo);
                    SyncStep::Canon(SyncCanon {
                        repo_id: hub_repo,
                        url: t.url.clone(),
                        fetched: Some(root),
                        applied: old.as_ref().and_then(|o| o.applied),
                        held: old.and_then(|o| o.held),
                    })
                }
                Some(name) => {
                    let old = reader
                        .sync_view(&t.name, name)
                        .map_err(store)?
                        .filter(|o| o.repo_id == hub_repo);
                    SyncStep::View {
                        name: name.to_string(),
                        value: SyncView {
                            repo_id: hub_repo,
                            url: t.url.clone(),
                            root: reader.view_chain_root(name, &root).map_err(store)?,
                            fetched: Some(root),
                            applied: old.and_then(|o| o.applied),
                        },
                    }
                }
            };
            drop(read);
            write_steps(engine, &t.name, vec![step])?;
            return Ok(None);
        }
        let files = self.get_all(&read, t, base_url, state, purpose, root_key, &want, repo_id)?;
        let target = FetchTarget {
            remote: t.name.clone(),
            url: t.url.clone(),
            purpose,
            root,
            view: view.map(str::to_string),
        };
        drop(read);
        with_writer_result(engine, move |rdy| {
            crate::sync_fetch::fetch_checked(rdy, &files, &target)
        })
        .map(Some)
    }

    /// Every key of the known pull pages of `root` (purpose `purpose`, above `base`), in the
    /// hub's order. A key a page repeats is the hub's protocol error.
    pub(crate) fn known_listing(
        &self,
        read: &ReadState,
        t: &Target,
        base_url: &str,
        purpose: StreamPurpose,
        root: ObjectKey,
        base: Option<ObjectKey>,
    ) -> Result<Vec<ObjectKey>, RdyError> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        let mut after = None;
        loop {
            let cx = self.context(t, read, Some(base_url))?;
            let page = self.wire.known_pull(
                &cx,
                &KnownPull {
                    after,
                    base,
                    purpose,
                    root,
                },
            )?;
            for k in page.keys {
                if !seen.insert(k) {
                    return Err(upstream_protocol());
                }
                out.push(k);
            }
            match page.next {
                None => break,
                Some(n) => after = Some(n),
            }
        }
        known_listing_done();
        Ok(out)
    }

    /// Gets the keys `want` of `root` (purpose `purpose`) in groups within the hub's limits,
    /// each streamed to a scratch file and checked once (the take reuses that check).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn get_all(
        &self,
        read: &ReadState,
        t: &Target,
        base_url: &str,
        state: &SyncState,
        purpose: StreamPurpose,
        root: ObjectKey,
        want: &[ObjectKey],
        repo_id: rdy_model::RepoId,
    ) -> Result<Vec<crate::stream_check::CheckedBatchFile>, RdyError> {
        self.get_all_with(
            read,
            t,
            base_url,
            state,
            purpose,
            root,
            want,
            &mut |file, written, sizer| check_got(file, repo_id, purpose, root, sizer, written),
        )
    }

    /// [`Self::get_all`] with each reply's file and bytes passed to `got`, which checks it (and
    /// teaches the sizer its records) or keeps it for a check of the files together.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn get_all_with<T>(
        &self,
        read: &ReadState,
        t: &Target,
        base_url: &str,
        state: &SyncState,
        purpose: StreamPurpose,
        root: ObjectKey,
        want: &[ObjectKey],
        got: &mut dyn FnMut(std::fs::File, u64, &mut GroupSizer) -> Result<T, RdyError>,
    ) -> Result<Vec<T>, RdyError> {
        // the groups to get (TASK-3869): the known step gives no record's bytes, so each new
        // group is sized from the bytes per kind that the replies so far held (`GroupSizer`),
        // within batch_records. A group whose reply still passes batch_bytes is split in two and
        // its halves got in turn (`split`, last first), down to one key, which is refused.
        let mut sizer = GroupSizer::for_state(state);
        let mut split: Vec<&[ObjectKey]> = Vec::new();
        let mut rest: &[ObjectKey] = want;
        let mut files = Vec::new();
        loop {
            let keys = match split.pop() {
                Some(k) => k,
                None if rest.is_empty() => break,
                None => {
                    let (k, r) = rest.split_at(sizer.next(rest));
                    rest = r;
                    k
                }
            };
            let mut file = tempfile::tempfile().map_err(|_| scratch_error())?;
            match self.get_one(read, t, base_url, state, purpose, root, keys, &mut file)? {
                GetEnd::Split => {
                    let (front, back) = keys.split_at(keys.len() / 2);
                    split.push(back);
                    split.push(front);
                }
                GetEnd::Got(written) => files.push(got(file, written, &mut sizer)?),
            }
        }
        Ok(files)
    }

    /// One get of `keys` of `root` (purpose `purpose`) into `file`, within the hub's
    /// `batch_bytes`. A reply past the cap, or the hub's refusal over its cap with the structured
    /// marker, asks for a split when it holds two keys or more; one key past the cap is the
    /// hub's protocol error, and any other refusal is the hub's error as it came.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn get_one(
        &self,
        read: &ReadState,
        t: &Target,
        base_url: &str,
        state: &SyncState,
        purpose: StreamPurpose,
        root: ObjectKey,
        keys: &[ObjectKey],
        file: &mut std::fs::File,
    ) -> Result<GetEnd, RdyError> {
        let cx = self.context(t, read, Some(base_url))?;
        let request = GetRequest {
            keys: keys.to_vec(),
            purpose,
            root,
        };
        let mut sink = Capped {
            out: file,
            cap: state.batch_bytes,
            bytes: 0,
            over: false,
        };
        // one transport piece of room past the cap: a reply past it always shows its first
        // byte past the cap to the sink before the transport stops it
        let limit = state.batch_bytes.saturating_add(UPLOAD_BUFFER_BYTES as u64);
        let got = self.wire.get(&cx, &request, &mut sink, limit);
        let over = sink.over;
        let written = sink.bytes;
        drop(cx);
        if over {
            if keys.len() == 1 {
                return Err(upstream_protocol());
            }
            return Ok(GetEnd::Split);
        }
        // a get that the hub refuses over its cap, with its structured marker, is split in
        // two as a get over `batch_bytes` is; a refusal of one key, or without the marker,
        // is the hub's error as it came
        if let Err(e) = &got
            && e.hub_get_cap.is_some()
            && keys.len() > 1
        {
            return Ok(GetEnd::Split);
        }
        got?;
        Ok(GetEnd::Got(written))
    }

    /// A URL clone's fetch of hub canon's state at the captured tip of `state` (D-GGTYA, O16):
    /// the state's known pages and gets (purpose `canon_state`), then every canon history key
    /// at the same tip, keys only, with no base, written as the one checked key file
    /// (`.rdy/sync/<remote>/history/keys`) with its exact distinct count, and only then the
    /// state take: one batch with the state's records, canon at the tip, the remote's canon
    /// state, and the incomplete history ref naming that count.
    pub(crate) fn clone_state(
        &self,
        engine: &dyn Engine,
        t: &Target,
        base_url: &str,
        state: &SyncState,
    ) -> Result<FetchOutcome, RdyError> {
        let tip = state.canon.ok_or_else(upstream_protocol)?;
        let hub_repo = state.repo_id.ok_or_else(upstream_protocol)?;
        let read = engine.read();
        let root = ObjectKey {
            kind: kinds::LANDING,
            hash: tip,
        };
        let mut want = Vec::new();
        for k in self.known_listing(&read, t, base_url, StreamPurpose::CanonState, root, None)? {
            if !read.reader().has_kind(k.kind, &k.hash).map_err(store)? {
                want.push(k);
            }
        }
        // the state's files are checked together (one header, the state's link table), then
        // taken by the store, which checks each again
        let mut files: Vec<std::fs::File> = self.get_all_with(
            &read,
            t,
            base_url,
            state,
            StreamPurpose::CanonState,
            root,
            &want,
            &mut |file, _, _| Ok(file),
        )?;
        let mut readers = Vec::with_capacity(files.len());
        for f in &mut files {
            use std::io::Seek as _;
            f.seek(std::io::SeekFrom::Start(0))
                .map_err(|_| scratch_error())?;
        }
        for f in &files {
            readers.push(std::io::BufReader::new(f));
        }
        if !readers.is_empty() {
            let reports = crate::stream_check::check_state_batches(readers, Some(hub_repo))?;
            if reports.iter().any(|r| r.header.root() != root) {
                return Err(upstream_protocol());
            }
        }
        // the history's keys at the same tip, before the state take: the denominator of Ready
        let keys =
            self.known_listing(&read, t, base_url, StreamPurpose::CanonHistory, root, None)?;
        let history = crate::history_fetch::write_key_file(
            read.root(),
            &t.name,
            &t.url,
            hub_repo,
            tip,
            keys,
        )?;
        drop(read);
        let target = FetchTarget {
            remote: t.name.clone(),
            url: t.url.clone(),
            purpose: StreamPurpose::CanonState,
            root: tip,
            view: None,
        };
        with_writer_result(engine, move |rdy| {
            crate::sync_fetch::take_state(rdy, &files, &target, &history)
        })
    }
}

/// How one get ended: its reply's bytes in the file, or a split asked for.
pub(crate) enum GetEnd {
    Got(u64),
    Split,
}

/// Checks the get reply in `file` (`written` bytes) once, as one batch of repo `repo_id` with
/// purpose `purpose` and root `root`, and teaches `sizer` its records.
pub(crate) fn check_got(
    file: std::fs::File,
    repo_id: rdy_model::RepoId,
    purpose: StreamPurpose,
    root: ObjectKey,
    sizer: &mut GroupSizer,
    written: u64,
) -> Result<crate::stream_check::CheckedBatchFile, RdyError> {
    let mut records = Vec::new();
    let checked = crate::stream_check::check_batch_file(file, Some(repo_id), |kind, at, len| {
        records.push((kind, at, len))
    })?;
    sizer.taken(written, &records);
    let header = &checked.report().header;
    if header.purpose != purpose || header.root() != root {
        return Err(upstream_protocol());
    }
    Ok(checked)
}

thread_local! {
    /// This thread's count of known listings that reached their final page.
    static KNOWN_LISTINGS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn known_listing_done() {
    KNOWN_LISTINGS.with(|c| c.set(c.get() + 1));
}

/// This thread's count of known listings that reached their final page so far. A test reads
/// the difference over a call.
#[doc(hidden)]
pub fn known_listings_for_tests() -> u64 {
    KNOWN_LISTINGS.with(std::cell::Cell::get)
}

/// The bytes that a fetch assumes for a key of a kind that no reply has shown yet: a fixed,
/// conservative guess by judgement (TASK-3869), near the largest records a history holds (a
/// sealed plan can pass 800 KB), so a first get, or the first get that reaches a new kind,
/// stays small and learns the kind. At 100 MiB batches a first group asks for 80 keys.
const FIRST_KEY_BYTES: u64 = 1_048_576;

/// The share of `batch_bytes` that a sized group aims at, in fifths: the room left covers
/// records larger than their kind's mean so far.
const FETCH_FILL_FIFTHS: u64 = 4;

/// The most a sized group's estimate may be, as a multiple of the largest reply so far.
const FETCH_GROWTH: u64 = 4;

/// The size of each new fetch group. The known step gives each key's kind but not its bytes,
/// and a history's export order runs through its kinds in turn, whose records differ in size
/// by orders of magnitude, so the sizer keeps, for each kind, the records and payload bytes that the
/// replies so far held. It also keeps the most framing bytes of one record (the gap from a
/// record's payload end to the next record's start) and of one reply (its bytes past its
/// records). A group takes keys in order while its estimate stays within `FETCH_FILL_FIFTHS` of
/// `batch_bytes`, and within `FETCH_GROWTH` times the largest reply so far (a mean from a small
/// sample runs low when later records of a kind grow): at least one key, at most `per`.
pub(crate) struct GroupSizer {
    target: u64,
    per: usize,
    kinds: std::collections::BTreeMap<u8, (u64, u64)>,
    framing: u64,
    batch: u64,
    largest: u64,
}

impl GroupSizer {
    /// The sizer of the hub limits that `state` names.
    pub(crate) fn for_state(state: &SyncState) -> GroupSizer {
        let per = usize::try_from(state.batch_records)
            .unwrap_or(MAX_KNOWN_KEYS)
            .clamp(1, MAX_KNOWN_KEYS);
        GroupSizer::new(state.batch_bytes, per)
    }

    fn new(batch_bytes: u64, per: usize) -> GroupSizer {
        GroupSizer {
            target: batch_bytes / 5 * FETCH_FILL_FIFTHS,
            per,
            kinds: std::collections::BTreeMap::new(),
            framing: 0,
            batch: 0,
            largest: 0,
        }
    }

    /// The estimated reply bytes of one key of `kind`.
    fn key_bytes(&self, kind: u8) -> u64 {
        match self.kinds.get(&kind) {
            Some(&(n, bytes)) if n > 0 => bytes.div_ceil(n).saturating_add(self.framing),
            _ => FIRST_KEY_BYTES,
        }
    }

    /// How many of `rest`'s first keys the next group takes.
    pub(crate) fn next(&self, rest: &[ObjectKey]) -> usize {
        let target = match self.largest {
            0 => self.target,
            l => self.target.min(l.saturating_mul(FETCH_GROWTH)),
        };
        let mut sum = self.batch;
        let mut n = 0;
        for k in rest.iter().take(self.per) {
            sum = sum.saturating_add(self.key_bytes(k.kind));
            if n > 0 && sum > target {
                break;
            }
            n += 1;
        }
        n.max(1).min(rest.len())
    }

    /// A reply within the limit: `bytes` in all, holding `records`, each `(kind, frame start,
    /// payload bytes)` in stream order.
    pub(crate) fn taken(&mut self, bytes: u64, records: &[(u8, u64, u64)]) {
        for pair in records.windows(2) {
            let gap = pair[1]
                .1
                .saturating_sub(pair[0].1.saturating_add(pair[0].2));
            self.framing = self.framing.max(gap);
        }
        let mut framed = 0u64;
        for &(kind, _, len) in records {
            let e = self.kinds.entry(kind).or_insert((0, 0));
            e.0 = e.0.saturating_add(1);
            e.1 = e.1.saturating_add(len);
            framed = framed.saturating_add(len).saturating_add(self.framing);
        }
        self.batch = self.batch.max(bytes.saturating_sub(framed));
        self.largest = self.largest.max(bytes);
    }
}

/// A fetch batch's scratch file within the hub's `batch_bytes` (`cap`): a write that would pass
/// the cap marks the batch `over` and is dropped, as is every write after it.
struct Capped<'a> {
    out: &'a mut std::fs::File,
    cap: u64,
    bytes: u64,
    over: bool,
}

impl Write for Capped<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if !self.over {
            let next = self.bytes.saturating_add(buf.len() as u64);
            if next > self.cap {
                self.over = true;
            } else {
                self.out.write_all(buf)?;
                self.bytes = next;
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

/// The render inputs at the scope of `read`'s canon head through `renderer`, checked, with the
/// gate settings of the same read state.
fn capture_with(
    renderer: &(dyn PublishRenderer + Sync),
    read: &ReadState,
) -> Result<Capture, RdyError> {
    let head = read.canon_head();
    let scope = head.map_or(Scope::Canon, Scope::At);
    let inputs = renderer.capture(read, &scope)?;
    validate_inputs(&inputs)?;
    let gate = gate_settings(read, &inputs);
    Ok(Capture { inputs, gate, head })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub_sync_wire::OutcomeReply;
    use std::sync::Mutex;

    /// A clock that moves only when a lookup or a wait moves it; `slack` is the time each wait
    /// takes beyond its delay (a late wake).
    #[derive(Default)]
    struct Fake {
        now: Mutex<Duration>,
        waits: Mutex<Vec<Duration>>,
        slack: Mutex<Duration>,
    }

    impl Fake {
        fn advance(&self, by: Duration) {
            *self.now.lock().unwrap() += by;
        }

        fn seconds(&self) -> Vec<u64> {
            self.waits
                .lock()
                .unwrap()
                .iter()
                .map(Duration::as_secs)
                .collect()
        }
    }

    impl OutcomePollClock for Fake {
        fn now(&self) -> Duration {
            *self.now.lock().unwrap()
        }

        fn wait(&self, delay: Duration) {
            self.waits.lock().unwrap().push(delay);
            let slack = *self.slack.lock().unwrap();
            self.advance(delay + slack);
        }
    }

    fn busy(seconds: Option<u64>) -> RdyError {
        let mut e = RdyError::new(HUB_BUSY, "busy", "wait");
        e.upstream_retry_after = seconds;
        e
    }

    fn unknown() -> OutcomeReply {
        OutcomeReply {
            outcome: LookupOutcome::Unknown,
            kind: None,
            view: None,
            request_state: None,
        }
    }

    /// Polls with a hub that is busy for `busy_for` lookups (each taking `cost` and asking for
    /// `retry` seconds), then answers unknown. Returns the result, the lookups begun and the
    /// seconds waited.
    fn run(
        clock: &Fake,
        busy_for: usize,
        retry: Option<u64>,
        cost: Duration,
    ) -> (Result<Option<OutcomeReply>, RdyError>, usize, Vec<u64>) {
        let mut begun = 0;
        let r = poll_outcome(clock, || {
            begun += 1;
            clock.advance(cost);
            if begun <= busy_for {
                Err(busy(retry))
            } else {
                Ok(unknown())
            }
        });
        (r, begun, clock.seconds())
    }

    #[test]
    fn r59016_poll_bounds_and_clock() {
        let none = Duration::ZERO;
        let always = usize::MAX;

        // the wait is the hub's retry_after clamped to 1 to 5 seconds
        for (asked, waited) in [
            (0, 1),
            (1, 1),
            (5, 5),
            (6, 5),
            (31_536_000, 5),
            (u64::MAX, 5),
        ] {
            let clock = Fake::default();
            let (r, begun, seconds) = run(&clock, 1, Some(asked), none);
            assert!(matches!(r, Ok(Some(_))), "{asked}");
            assert_eq!((begun, seconds), (2, vec![waited]), "{asked}");
        }

        // metadata the decoder always sets is missing: the protocol error, no guessed wait
        let clock = Fake::default();
        let (r, begun, seconds) = run(&clock, always, None, none);
        assert_eq!(r.unwrap_err().code, crate::error::E_UPSTREAM_PROTOCOL);
        assert_eq!((begun, seconds), (1, vec![]));

        // the lookup count and the 120-second budget
        for (retry, cost, calls, waits) in
            [(1, 0, 82, 81), (5, 0, 17, 16), (1, 39, 3, 2), (5, 39, 2, 1)]
        {
            let clock = Fake::default();
            let (r, begun, seconds) = run(&clock, always, Some(retry), Duration::from_secs(cost));
            assert!(matches!(r, Ok(None)), "retry {retry} cost {cost}");
            assert_eq!(
                (begun, seconds.len()),
                (calls, waits),
                "retry {retry} cost {cost}"
            );
            // never past the budget: the last lookup began with its reserve free
            assert!(
                clock.now()
                    <= Duration::from_millis(OUTCOME_POLL_BUDGET_MS) + Duration::from_secs(cost)
            );
        }

        // a lookup begins with exactly 39,000 ms left, and not with 38,999
        for (slack_ms, begins) in [(1_000, 2), (1_001, 1)] {
            let clock = Fake::default();
            *clock.slack.lock().unwrap() = Duration::from_millis(slack_ms);
            // the first lookup takes 79 s; its wait takes 1 s and the slack: 39,000 or 38,999 ms
            // are left
            let (r, begun, seconds) = run(&clock, 1, Some(1), Duration::from_secs(79));
            if begins == 2 {
                assert!(matches!(r, Ok(Some(_))));
            } else {
                assert!(matches!(r, Ok(None)));
            }
            assert_eq!((begun, seconds), (begins, vec![1]), "slack {slack_ms}");
        }

        // the wait itself needs its delay and the next lookup's reserve: 40,000 ms
        for (cost_ms, waits) in [(80_000, 1), (80_001, 0)] {
            let clock = Fake::default();
            let (r, _, seconds) = run(&clock, always, Some(1), Duration::from_millis(cost_ms));
            assert!(matches!(r, Ok(None)));
            assert_eq!(seconds.len(), waits, "cost {cost_ms}");
        }

        // a completed outcome and a failure of another kind stop at once
        let clock = Fake::default();
        let (r, begun, seconds) = run(&clock, 0, Some(1), none);
        assert!(matches!(r, Ok(Some(_))));
        assert_eq!((begun, seconds), (1, vec![]));
        let clock = Fake::default();
        let mut begun = 0;
        let r = poll_outcome(&clock, || {
            begun += 1;
            Err(RdyError::new("E_HUB_NOT_FOUND", "no push", "check it"))
        });
        assert_eq!(r.unwrap_err().code, "E_HUB_NOT_FOUND");
        assert_eq!((begun, clock.seconds()), (1, vec![]));

        // the clock starts with the first lookup, wherever it stands
        let clock = Fake::default();
        clock.advance(Duration::from_secs(3_600));
        let (r, begun, _) = run(&clock, always, Some(1), none);
        assert!(matches!(r, Ok(None)));
        assert_eq!(begun, 82);
    }
}
