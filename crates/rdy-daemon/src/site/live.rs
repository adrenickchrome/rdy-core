//! The live state of the site (FW-LS1, D-N5NSN; FW-LS1a, D-FV6G9): the last build of each
//! served scope, the open pages of each scope (their event streams), and the worker that builds
//! a scope again after a publish while a page of it is open.
//!
//! The writer's only work per publish is [`LiveSite::published`]: one atomic add and one unpark
//! of the worker, with no lock.
//! Every build runs on the `rdy-site` worker or on a request's thread, one at a time. A build
//! reads the published state, not the event list: a tag logs no event and a page reads more
//! than its own blocks. The same input key means the same pages, so a publish that touches
//! nothing a scope reads builds nothing for it. A scope with no open page builds nothing until a
//! request asks for it (pages build on demand).
//!
//! After a build, each open page of the scope gets `Reload` when its bytes changed (or it is
//! gone), else `Label` when only the version label or the header changed, else nothing; the
//! pages of other scopes get nothing (the founder addition of 2026-10-12). An event is best
//! effort: a full or closed queue drops the stream, and the page's revision repairs it when it
//! reconnects.
//!
//! A local approval page's stream (D-WPE4R, TASK-979) is no target: it has no build and no diff.
//! Each publish that moved the store, and each config refresh, sends every UI stream one `Reload`,
//! whatever it did to a site page, since an ANSWER can leave every site byte unchanged. A publish
//! that moved neither (a refused answer, a UI Later) sends nothing: the page's form is on its way,
//! and a reload would cancel the page that answers it. UI streams count in [`MAX_STREAMS`], and
//! take the same queue, epoch check, hold and shutdown.
//!
//! The versions page (`/~versions/`, D-2F29G, RULING F5) is one more target here, with its own
//! key (`rdy_render::menu::menu_key_for`, which reads profile metadata and expands no selection)
//! and its own build: a view commit rebuilds it and never canon. The number of docs each view or
//! proposal version changes is cached by what it reads, so one view commit recounts one view.
//!
//! What the site serves is a [`SiteTarget`] (D-CWC91): a scope's source, a publication profile
//! under a scope, the profile-only site of canon, or the versions menu seen from a scope and
//! profile. The cache keys builds and streams by the whole normalized target, so no two scopes,
//! profiles or menu contexts share bytes, warnings or events. A profile build keeps one
//! [`ProfileHistoryMemo`] in its slot across successful builds, so a rebuild resolves only the
//! landings it has not seen; the slot's eviction drops it, and a failed build leaves no new memo
//! state behind.
//!
//! Every build, cache key and doc count belongs to one redaction epoch (D-CC90E: "every view that
//! holds the content shows the stub"). A build is published only while the store's epoch is
//! still the one its state captured, checked right before publication; a build of an older
//! epoch is discarded and never served, and a cached build of an older epoch is never served to
//! a new request. After a removal, each open page of a rebuilt scope that was opened in an older
//! epoch gets `Reload`, which carries nothing. No response lease or drain holds a removal back:
//! a response begun before it may complete as an outside copy.
//!
//! A review target ([`is_review`]: a view, a proposal version, or a profile route under one) is
//! rendered on request (D-F44G6, refined by D-WYZ97 and D-SMP5A), never built whole. Its slot keeps
//! the detached render preparation of one captured state, the pages a request asked for, each
//! with its own recency, and its complete warnings pass with the warnings logged for it. One
//! ledger, the site's cap, counts the eager builds and every review target's pages, shared parts
//! and warning state. Under pressure the review pages go first, least recently used across every
//! target, then the shared parts of review targets that keep no page; canon and tag builds are
//! never dropped for a review target. A write that changes a review target's inputs drops its
//! pages and preparation; the worker renders again only the pages that have an open stream.
use crate::daemon::log;
use crate::shared::Shared;
use crate::site::guard::{Refusal, ScopeRef, SiteTarget};
use crate::site::profiles::Availability;
use crate::writer::panic_text;
use rdy_engine::engine::SiteFacts;
use rdy_engine::publication::ResolvedProfile;
use rdy_engine::site_scope::{CHANGES_PAGE, SINCE_PAGE, summary_page_taken};
use rdy_engine::{E_BAD_INPUT, E_REDACTED_STATE, RdyError, ReadState, Scope, serve_config};
use rdy_model::Hash;
use rdy_render::live::{
    LiveBuild, ProfileHistoryMemo, build_live, build_profile_live, diff, inputs_key,
};
use rdy_render::live_review::{
    PreparedReview, ReviewCheck, ReviewFile, prepare_profile_review, prepare_review,
};
use rdy_render::menu::{build_versions_for, docs_changed, menu_key_for};
use rdy_render::retained::RetainedAllocations;
use rdy_render::{NotFoundKind, Slot, review};
use std::collections::{BTreeMap, BTreeSet};
use std::mem::size_of;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::thread::{JoinHandle, Thread};
use std::time::Duration;
use tokio::sync::mpsc;

/// The open event streams the site keeps, across every scope.
pub const MAX_STREAMS: usize = 32;
/// The events a stream holds before it is dropped.
pub const STREAM_QUEUE: usize = 8;
/// The targets that keep a build while no page of theirs is open. A target with an open page
/// keeps its slot as long as the page is open, outside this count, so at most [`MAX_STREAMS`]
/// such slots stay beside the inactive ones; its build counts in
/// [`MAX_INACTIVE_BUILD_BYTES`].
/// Each profile target's slot also keeps its history memo, so at most `MAX_STREAMS + MAX_SCOPES`
/// memos stay, each of at most `history_facts::MAX_LANDINGS` results.
pub const MAX_SCOPES: usize = 8;
/// The bytes the held builds may hold together, of targets with an open page or not (TASK-2307):
/// past it, builds are dropped least recently used first, keeping the most recently used one.
/// An inactive target goes with its build, as past [`MAX_SCOPES`]; a target with an open page
/// keeps its streams and memo, and builds again on its next request, its pages compared with
/// the revisions they opened at. A fixed limit by judgement: a build holds every figure's bytes
/// and every page, so a count alone let a few large builds hold gigabytes.
pub const MAX_INACTIVE_BUILD_BYTES: u64 = 256 * 1024 * 1024;
/// What a held build counts per file beside its bytes: the file's path and the map's own entry.
const FILE_OVERHEAD: u64 = 64;

/// How often the worker looks whether any `Shared` still lives while no publish wakes it. A
/// wake lost while the worker starts waits at most this long.
const WORKER_TICK: Duration = Duration::from_secs(1);

/// The doc counts the site keeps for the versions page; past this, the cache clears.
pub const MAX_COUNTS: usize = 256;

/// What counts the docs a view or proposal version changes: `rdy_render::menu::docs_changed`, or
/// a test's hook ([`LiveSite::set_docs_changed`]).
pub type DocsChanged = Box<dyn Fn(&ReadState, &Scope) -> Result<usize, RdyError> + Send + Sync>;

/// A test's barrier at the publication check of a build ([`LiveSite::set_before_publish`]): it
/// runs with the (normalized) target after the build and before the epoch check that may
/// discard it.
pub type BeforePublish = Box<dyn Fn(&SiteTarget) + Send + Sync>;

/// A [`BeforePublish`] as the site keeps it: cloned out of its lock before it runs.
type Barrier = Arc<dyn Fn(&SiteTarget) + Send + Sync>;

/// A test's barrier in a UI subscribe ([`LiveSite::set_before_ui_compare`]).
pub type BeforeUiCompare = Box<dyn Fn() + Send + Sync>;

/// A [`BeforeUiCompare`] as the site keeps it: cloned out of its lock before it runs.
type UiBarrier = Arc<dyn Fn() + Send + Sync>;

/// A test's sink for the lines the site logs ([`LiveSite::set_log_hook`]), beside the log.
pub type LogHook = Box<dyn Fn(&str) + Send + Sync>;

/// One build of a target: its pages, and the facts the same build captured, from one read
/// state: the profile it resolved to (a profile target only), and the change summary page it
/// generated (`changes.html`, or `changes-since.html` for canon's source), if any.
pub struct SiteBuild {
    pub live: LiveBuild,
    pub profile: Option<ResolvedProfile>,
    pub changes: Option<&'static str>,
}

/// `target` in its one form for the cache: the source of the versions page is the menu of
/// canon.
pub fn normalize(target: &SiteTarget) -> SiteTarget {
    match target {
        SiteTarget::Source(ScopeRef::Versions) => SiteTarget::Menu {
            scope: ScopeRef::Canon,
            profile: None,
        },
        other => other.clone(),
    }
}

/// How the site's log names `target`: a source scope as rdy reads it (`canon`, `view:<name>`),
/// a profile by its complete root (`/~view/<name>/~profile/<key>/`), the menu of canon as
/// `versions` and another menu by its link.
pub fn label(target: &SiteTarget) -> String {
    match normalize(target) {
        SiteTarget::Source(scope) => scope.text(),
        t @ (SiteTarget::Profile { .. } | SiteTarget::ProfileOnly { .. }) => t.root(),
        SiteTarget::Menu {
            scope: ScopeRef::Canon,
            profile: None,
        } => ScopeRef::Versions.text(),
        t @ SiteTarget::Menu { .. } => t.menu_url().unwrap_or_default(),
    }
}

/// What a stream sends an open page: the closed set of the site's events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveEvent {
    /// The page's bytes changed, or the page is gone: load it again.
    Reload,
    /// Only the version label or the header changed: the new label text (the colophon) and the
    /// new header markup (the pill and its chips).
    Label { text: String, html: String },
}

/// Why a subscribe opened no stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoStream {
    /// The site has as many open streams as it keeps ([`MAX_STREAMS`]), or it is closed.
    Busy,
    /// A profile-only target whose key canon does not hold in the state published when the
    /// stream would register (D-DK03K): its listener serves the plain not found.
    NotFound,
    /// A profile-only target whose key names a block whose profile metadata does not read.
    Broken,
}

/// Why `target` opens no stream in the published state `st`: a profile-only target whose key
/// canon does not hold (D-DK03K). `None` for every other target, and for a state older than a
/// removal (a subscribe then compares again; the fresh state's publish wakes the inspection
/// that clears a key canon no longer holds).
fn unavailable(sh: &Shared, target: &SiteTarget, st: &Arc<ReadState>) -> Option<NoStream> {
    let SiteTarget::ProfileOnly { key } = target else {
        return None;
    };
    if st.ensure_current_epoch().is_err() {
        return None;
    }
    match sh.profile_sites.availability(st, key) {
        Availability::Held => None,
        Availability::Missing => Some(NoStream::NotFound),
        Availability::Broken => Some(NoStream::Broken),
    }
}

/// The scope `scope` names, read from `st`. A view must exist; `P-x` reads as the proposal's
/// latest version each time; a tag must exist. The versions page is not a read scope.
pub fn resolve(st: &ReadState, scope: &ScopeRef) -> Result<Scope, RdyError> {
    match scope {
        ScopeRef::Canon => Ok(Scope::Canon),
        ScopeRef::View(name) => st.resolve_read_scope(&format!("view:{name}"), None),
        ScopeRef::Proposal(r) => st.resolve_scope(&format!("proposal:{r}")),
        ScopeRef::Tag(name) => st.resolve_scope(&format!("tag:{name}")),
        ScopeRef::Versions => Err(RdyError::new(
            rdy_engine::E_BAD_INPUT,
            "the versions page is not a read scope",
            "read canon, a version, a view or a proposal version",
        )),
    }
}

/// Whether `target` (normalized) is a review target, rendered on request: a view, a proposal
/// version, or a profile route under one (D-F44G6). Canon, tags, their profile routes, the
/// profile-only site and every versions menu keep their whole builds.
pub fn is_review(target: &SiteTarget) -> bool {
    matches!(
        target,
        SiteTarget::Source(ScopeRef::View(_) | ScopeRef::Proposal(_))
            | SiteTarget::Profile {
                scope: ScopeRef::View(_) | ScopeRef::Proposal(_),
                ..
            }
    )
}

/// The error of an eager-only call on a review target: the invalid live target.
fn not_eager() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "invalid live site target",
        "use a source scope and a publication profile key",
    )
}

/// One open page: its stream's sender and what it shows.
struct Stream {
    id: u64,
    page: String,
    /// The page's revision and the label's when the stream opened; for a review target, what the
    /// page shows now as far as the stream told it (`None`: the page is gone).
    rev: Option<String>,
    label_rev: String,
    /// A review target's stream: the input key it was last compared at (`None`: never).
    key: Option<(u64, Hash)>,
    /// The redaction epoch the page was compared in: the epoch of the build it was compared
    /// with on subscribe, else the store's epoch when it opened. A build of another epoch sends
    /// it `Reload`.
    epoch: u64,
    /// The stream was compared with a build of its scope (on subscribe, or by a fan-out since).
    /// Until it is, the next build is compared with `rev` and `label_rev`, not diffed.
    checked: bool,
    tx: mpsc::Sender<LiveEvent>,
}

impl Stream {
    /// What the page needs to show what `b` built: `Reload` when `b` has the page at another
    /// revision or not at all, else the label when only it differs.
    fn against(&self, b: &LiveBuild) -> Option<LiveEvent> {
        if b.revs.get(&self.page) != self.rev.as_ref() {
            Some(LiveEvent::Reload)
        } else if b.label_rev != self.label_rev {
            Some(LiveEvent::Label {
                text: b.label.clone(),
                html: b.header.clone(),
            })
        } else {
            None
        }
    }

    /// What a review page needs to show `now`, the page as the target's state at `key` renders
    /// it: `Reload` when the page was compared in another epoch, or its revision moved or it is
    /// gone, else the label when only the label or the header moved. The stream then holds what
    /// it told its page, so a later state is compared with it.
    fn follow(&mut self, now: &PageFacts, key: (u64, Hash)) -> Option<LiveEvent> {
        let event = if self.epoch != now.epoch || self.rev != now.rev {
            Some(LiveEvent::Reload)
        } else if self.label_rev != now.label_rev {
            Some(LiveEvent::Label {
                text: now.label.clone(),
                html: now.header.clone(),
            })
        } else {
            None
        };
        self.rev.clone_from(&now.rev);
        self.label_rev.clone_from(&now.label_rev);
        self.key = Some(key);
        self.checked = true;
        event
    }
}

/// What a stream compares a review page with: the page's revision as its target's state renders
/// it (`None`: no page there), and that state's label facts and epoch.
#[derive(Clone, Debug)]
struct PageFacts {
    rev: Option<String>,
    label: String,
    header: String,
    label_rev: String,
    epoch: u64,
}

impl PageFacts {
    fn of(prep: &PreparedReview, file: Option<&ReviewFile>) -> PageFacts {
        PageFacts {
            rev: file.and_then(|f| f.rev.clone()),
            label: prep.label().to_string(),
            header: prep.header().to_string(),
            label_rev: prep.label_rev().to_string(),
            epoch: prep.epoch(),
        }
    }
}

/// The state a review request reads: the published state (request-scoped, never kept), its
/// input key with its redaction epoch, and the publish count read before it.
struct Fresh {
    st: Arc<ReadState>,
    key: (u64, Hash),
    now: u64,
}

/// A review target's preparation for one request: kept, or new with the profile memo it
/// continued.
struct Prep {
    prep: Arc<PreparedReview>,
    new: bool,
    memo: Option<ProfileHistoryMemo>,
}

/// One requested file of a review target, as the server sends it ([`LiveSite::review_page`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewPage {
    /// The bytes sent: the label and header filled in, and a page's live script.
    pub bytes: Vec<u8>,
    /// The page's revision: none for a permalink page and any file but a page.
    pub rev: Option<String>,
    /// The version label's text, the header's markup and their revision, of the target's state.
    pub label: String,
    pub header: String,
    pub label_rev: String,
    /// The redaction epoch the target's state captured.
    pub epoch: u64,
}

/// A requested page of a review target that the site keeps.
struct KeptPage {
    file: Arc<ReviewFile>,
    /// What the page counts in the ledger ([`page_charge`]), set when it is kept.
    bytes: u64,
    /// When the page was last requested, on `Inner::clock`.
    used: u64,
}

/// What a review target keeps for one captured state (the input key `key`): its detached
/// preparation, its requested pages and its complete warnings pass. The warnings logged for the
/// target stay across its states while the target is kept (D-SMP5A): a full drop of the target
/// takes them, a page's eviction does not.
#[derive(Default)]
struct Review {
    key: Option<(u64, Hash)>,
    prep: Option<Arc<PreparedReview>>,
    pages: BTreeMap<String, KeptPage>,
    check: Option<Arc<ReviewCheck>>,
    /// The warnings logged for the target: added by every pass and page render, and replaced by
    /// a complete pass, which alone proves a warning gone.
    logged: BTreeSet<String>,
}

impl Review {
    /// Whether the target keeps anything for its state or its warnings.
    fn holds(&self) -> bool {
        self.prep.is_some()
            || !self.pages.is_empty()
            || self.check.is_some()
            || !self.logged.is_empty()
    }

    /// Drops the pages, preparation and pass of the state the target kept; the logged warnings
    /// stay. Returns whether anything was dropped.
    fn forget_state(&mut self) -> bool {
        let had = self.prep.is_some() || !self.pages.is_empty() || self.check.is_some();
        self.key = None;
        self.prep = None;
        self.pages.clear();
        self.check = None;
        had
    }

    /// The bytes the target's warning state counts: each logged warning, and the kept pass.
    fn warning_bytes(&self) -> u64 {
        let text = |s: &String| s.capacity() as u64 + size_of::<String>() as u64;
        let logged: u64 = self.logged.iter().map(|w| text(w) + FILE_OVERHEAD).sum();
        let check = self.check.as_ref().map_or(0, |c| {
            ARC_HEADER
                + size_of::<ReviewCheck>() as u64
                + (c.warnings.capacity() - c.warnings.len()) as u64 * size_of::<String>() as u64
                + c.warnings.iter().map(text).sum::<u64>()
        });
        logged + check
    }
}

/// A review file request's answer: the preparation it read, the file (None for a path the
/// target has no file at), and whether kept data was dropped.
type FileAt = (Arc<PreparedReview>, Option<Arc<ReviewFile>>, bool);

/// The bytes of an `Arc`'s counts, beside its value.
const ARC_HEADER: u64 = 16;

/// What a kept review page at `path` counts in the ledger: its bytes' buffer, its slots,
/// revision, warnings and path, the file's own size and [`FILE_OVERHEAD`] for its entry.
fn page_charge(path: &str, f: &ReviewFile) -> u64 {
    let text = |s: &String| s.capacity() as u64 + size_of::<String>() as u64;
    path.len() as u64
        + FILE_OVERHEAD
        + ARC_HEADER
        + size_of::<ReviewFile>() as u64
        + f.bytes.capacity() as u64
        + (f.label_at.capacity() * size_of::<Slot>()) as u64
        + f.rev.as_ref().map_or(0, text)
        + f.warnings.iter().map(text).sum::<u64>()
}

/// What a review target's preparation counts on its own, beside none other: the renderer's
/// retained estimate, and the site's handle of it.
fn prep_alone(prep: &PreparedReview) -> u64 {
    ARC_HEADER
        + size_of::<PreparedReview>() as u64
        + prep.retained_bytes(&mut RetainedAllocations::new())
}

/// One served target: its last build and the open pages of it.
#[derive(Default)]
struct ScopeState {
    build: Option<Arc<SiteBuild>>,
    /// The bytes `build` holds (see [`held_size`]), set with it.
    bytes: u64,
    /// The input key of `build`, with the redaction epoch its state captured.
    key: Option<(u64, Hash)>,
    /// The publish count the build (or a check that found the same key) read.
    seen: u64,
    /// When the scope was last used, on `Inner::clock`.
    used: u64,
    /// A build of the target failed on the worker and the failure was logged; cleared by a
    /// success.
    failing: bool,
    streams: Vec<Stream>,
    /// A profile target's neutral-chip memo, kept across its successful builds (empty for any
    /// other target). Taken out for a build and put back only when the build is published.
    memo: ProfileHistoryMemo,
    /// A review target's kept state (empty for any other target).
    review: Review,
}

impl ScopeState {
    /// Whether the target keeps anything but its streams: a build, or a review target's state
    /// or warnings.
    fn holds(&self) -> bool {
        self.build.is_some() || self.review.holds()
    }
}

/// The ledger of the site's cap, as a test reads it ([`LiveSite::ledger`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ledger {
    /// Everything the cap counts: the four parts below.
    pub total: u64,
    /// The eager builds (canon, tags, their profiles, the profile-only site, the menus).
    pub eager: u64,
    /// The review targets' preparations, each shared allocation counted once.
    pub shared: u64,
    /// The kept review pages.
    pub pages: u64,
    /// The review targets' warning state: logged warnings and kept passes.
    pub warnings: u64,
    /// The kept review pages, least recently used first, each with what it counts.
    pub kept: Vec<(SiteTarget, String, u64)>,
    /// The review targets that keep a preparation.
    pub prepared: Vec<SiteTarget>,
    /// The logged warnings the review targets keep, across every target.
    pub logged: usize,
    /// The targets the site keeps state for.
    pub targets: usize,
}

/// What a stream is the stream of: a target of the site, or the approval pages.
#[derive(Clone, Debug)]
enum StreamKey {
    Site(SiteTarget),
    Ui,
}

/// One open approval page's stream: it only ever gets `Reload`.
struct UiStream {
    id: u64,
    /// The store revision and the pack generation the page last heard of.
    seen: (String, u64),
    tx: mpsc::Sender<LiveEvent>,
}

#[derive(Default)]
struct Inner {
    scopes: BTreeMap<SiteTarget, ScopeState>,
    ui_streams: Vec<UiStream>,
    clock: u64,
    next_id: u64,
    closed: bool,
    /// The cap on the bytes of the held builds: [`MAX_INACTIVE_BUILD_BYTES`], or a test's. Kept
    /// here so every path that adds a build or makes one inactive (a new build, a cache hit, the
    /// last stream of a target closing) enforces it under this lock.
    cap: u64,
}

impl Inner {
    fn streams(&self) -> usize {
        self.scopes.values().map(|s| s.streams.len()).sum::<usize>() + self.ui_streams.len()
    }

    /// The state of target `scope`, marked as used now. Only a resolved target with a build, or
    /// a target with an open stream, gets one: a request names any target it likes.
    fn touch(&mut self, scope: &SiteTarget) -> &mut ScopeState {
        self.clock += 1;
        let now = self.clock;
        let s = self.scopes.entry(scope.clone()).or_default();
        s.used = now;
        s
    }

    /// The bytes the inactive targets' builds hold (a test's view; the cap counts every build).
    fn inactive_bytes(&self) -> u64 {
        self.scopes
            .values()
            .filter(|s| s.build.is_some() && s.streams.is_empty())
            .map(|s| s.bytes)
            .sum()
    }

    /// The bytes the eager builds hold.
    fn eager_bytes(&self) -> u64 {
        self.scopes.values().map(|s| s.bytes).sum()
    }

    /// The bytes the review targets' preparations hold, each shared allocation counted once.
    fn shared_bytes(&self) -> u64 {
        let mut seen = RetainedAllocations::new();
        self.scopes
            .values()
            .filter_map(|s| s.review.prep.as_ref())
            .map(|p| ARC_HEADER + size_of::<PreparedReview>() as u64 + p.retained_bytes(&mut seen))
            .sum()
    }

    /// The bytes the kept review pages hold.
    fn page_bytes(&self) -> u64 {
        let pages = self.scopes.values().flat_map(|s| s.review.pages.values());
        pages.map(|p| p.bytes).sum()
    }

    /// The bytes the review targets' warning state holds.
    fn warning_bytes(&self) -> u64 {
        self.scopes.values().map(|s| s.review.warning_bytes()).sum()
    }

    /// Everything the cap counts.
    fn total(&self) -> u64 {
        self.eager_bytes() + self.shared_bytes() + self.page_bytes() + self.warning_bytes()
    }

    /// The least recently used kept review page, across every target.
    fn oldest_page(&self) -> Option<(SiteTarget, String)> {
        self.scopes
            .iter()
            .flat_map(|(t, s)| s.review.pages.iter().map(move |(p, k)| (k.used, t, p)))
            .min_by_key(|(used, ..)| *used)
            .map(|(_, t, p)| (t.clone(), p.clone()))
    }

    /// Drops inactive targets (no open stream, and a build, or a review target's state or
    /// warnings) past [`MAX_SCOPES`], least recently used first, with their memos; a target that
    /// keeps nothing and has no stream goes too. Then, while the ledger holds more than
    /// [`Inner::cap`] bytes (D-WYZ97): first the review pages go, least recently used across
    /// every target, open pages' too; then each review target that keeps no page goes whole, least
    /// recently used first: its preparation, its pass and its logged warnings (D-SMP5A), an open
    /// target keeping its streams and memo; then, while more than one is held, the least recently
    /// used eager build: an inactive target goes with it, and a target with an open stream keeps
    /// its streams and memo and loses only its build and key. Canon and tags are never dropped
    /// for a review target, and the most recently used eager build always stays. What was
    /// dropped is built or rendered again on its next request. Returns whether anything was
    /// dropped, so the caller can give the memory back once it releases the lock.
    fn evict(&mut self) -> bool {
        let cap = self.cap;
        self.scopes
            .retain(|_, s| s.holds() || !s.streams.is_empty());
        let inactive = |s: &ScopeState| s.holds() && s.streams.is_empty();
        let mut dropped = false;
        while self.scopes.values().filter(|s| inactive(s)).count() > MAX_SCOPES {
            let Some(oldest) = self
                .scopes
                .iter()
                .filter(|(_, s)| inactive(s))
                .min_by_key(|(_, s)| s.used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.scopes.remove(&oldest);
            dropped = true;
        }
        while self.total() > cap {
            // the review pages first, by global recency
            if let Some((target, path)) = self.oldest_page() {
                if let Some(s) = self.scopes.get_mut(&target) {
                    s.review.pages.remove(&path);
                }
                dropped = true;
                continue;
            }
            // then the review targets that keep no page, whole
            let Some((oldest, open)) = self
                .scopes
                .iter()
                .filter(|(_, s)| s.review.holds())
                .min_by_key(|(_, s)| s.used)
                .map(|(k, s)| (k.clone(), !s.streams.is_empty()))
            else {
                break;
            };
            if open {
                // its streams keep what they told their pages, and are compared with that
                if let Some(s) = self.scopes.get_mut(&oldest) {
                    s.review = Review::default();
                }
            } else {
                self.scopes.remove(&oldest);
            }
            dropped = true;
        }
        loop {
            let held = self.scopes.values().filter(|s| s.build.is_some());
            let (count, bytes) = held.fold((0usize, 0u64), |(n, b), s| (n + 1, b + s.bytes));
            if count <= 1 || bytes <= cap {
                return dropped;
            }
            let Some((oldest, open)) = self
                .scopes
                .iter()
                .filter(|(_, s)| s.build.is_some())
                .min_by_key(|(_, s)| s.used)
                .map(|(k, s)| (k.clone(), !s.streams.is_empty()))
            else {
                return dropped;
            };
            if open {
                // the next build compares each open page with the revision it opened at
                // (`Stream::against`), as with no build to diff
                if let Some(s) = self.scopes.get_mut(&oldest) {
                    s.build = None;
                    s.bytes = 0;
                    s.key = None;
                }
            } else {
                self.scopes.remove(&oldest);
            }
            dropped = true;
        }
    }
}

/// The bytes `b` holds, counted once when it is built: each file's bytes and path, plus
/// [`FILE_OVERHEAD`] per file.
fn held_size(b: &SiteBuild) -> u64 {
    b.live
        .files
        .iter()
        .map(|(k, v)| k.len() as u64 + v.len() as u64 + FILE_OVERHEAD)
        .sum()
}

/// Gives the allocator's free memory back and counts it in `count`. Never called under the site
/// lock: the trim holds the allocator's arenas for a fraction of a second.
fn release_free_memory(count: &AtomicU64) {
    crate::sys::release_free_memory();
    count.fetch_add(1, Ordering::SeqCst);
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The site's live state: one per daemon, in [`Shared::site`].
pub struct LiveSite {
    /// Publishes so far: the writer adds one after each.
    published: AtomicU64,
    /// The worker's thread, which `published` unparks: an unpark is never lost, as a park after
    /// it returns at once, so the writer takes no lock.
    worker: OnceLock<Thread>,
    stopped: AtomicBool,
    /// Held for each build and its fan-out: one build at a time.
    building: Mutex<()>,
    /// Shared with each [`Subscription`], which removes its stream when dropped.
    inner: Arc<Mutex<Inner>>,
    /// Whole builds of eager targets.
    builds: AtomicU64,
    /// Whole builds asked of a review target: none, by construction ([`LiveSite::build`]).
    review_builds: AtomicU64,
    /// Review preparations, requested files rendered, and complete warnings passes.
    preparations: AtomicU64,
    renders: AtomicU64,
    checks: AtomicU64,
    /// The times the site gave the allocator's free memory back
    /// ([`crate::sys::release_free_memory`]): after a build, or after an eviction that dropped a
    /// build. Shared with each [`Subscription`].
    memory_releases: Arc<AtomicU64>,
    /// The docs each view or proposal version changes, for the versions page: keyed by the scope
    /// and what its count reads (see [`count_key`]). At most [`MAX_COUNTS`] entries.
    counts: Mutex<BTreeMap<(ScopeRef, Hash), usize>>,
    /// Test hook: counts in place of `rdy_render::menu::docs_changed`.
    docs_changed: Mutex<Option<DocsChanged>>,
    /// Test hook: the barrier at each build's publication check.
    before_publish: Mutex<Option<Barrier>>,
    /// Test hook: the barrier of a subscribe after its refresh, before its stream is registered.
    after_subscribe_refresh: Mutex<Option<Barrier>>,
    /// Test hook: each line the site logs, beside the log.
    log_hook: Mutex<Option<LogHook>>,
    /// Test hook: the history ceiling of every profile build (`None`: the real one).
    history_ceiling: Mutex<Option<usize>>,
    /// Test hook: the barrier of a UI subscribe after its stream is registered, before its last
    /// comparison.
    before_ui_compare: Mutex<Option<UiBarrier>>,
}

/// The cache key of the doc count of `scope` (a view or a proposal version), with the pack
/// generation `generation`: the scope, and the hash of what its review reads. A view's count reads
/// its head op and its effective base; a proposal version's reads the version. Each also reads the
/// base its review compares against ([`review::base_of`]): canon at a landing, canon at its head
/// (so canon's head is an input: a pinned view made before the first landing has no base and is
/// read against canon), or none. The pack decides which docs have a page. Any other scope has no
/// count to cache.
fn count_key(st: &ReadState, scope: &Scope, generation: u64) -> Option<(ScopeRef, Hash)> {
    let mut k = generation.to_le_bytes().to_vec();
    // a count belongs to the redaction epoch of the state it read, as a build does
    k.extend_from_slice(&st.redaction_epoch().to_le_bytes());
    let at = |k: &mut Vec<u8>, h: Option<Hash>| match h {
        Some(h) => {
            k.push(1);
            k.extend_from_slice(&h.0);
        }
        None => k.push(0),
    };
    let r = match scope {
        Scope::View(name) => {
            at(&mut k, st.view_head(name).map(|h| h.head));
            at(&mut k, st.effective_base(name));
            ScopeRef::View(name.clone())
        }
        Scope::Proposal(v) => {
            at(&mut k, Some(*v));
            ScopeRef::Proposal(v.to_string())
        }
        _ => return None,
    };
    match review::base_of(st, scope) {
        Some(Scope::At(h)) => {
            k.push(1);
            k.extend_from_slice(&h.0);
        }
        Some(Scope::Canon) => {
            k.push(2);
            at(&mut k, st.canon_head());
        }
        // a base is canon or canon at a landing; nothing else to tell apart
        Some(_) => k.push(3),
        None => k.push(0),
    }
    Some((r, Hash::of(&k)))
}

/// The input key of `target` (normalized and valid) in `st`: for a source or profile target,
/// the scope's [`inputs_key`] with the target's mode, scope and key; for a menu, the
/// metadata-only [`menu_key_for`]. No key expands a profile's selection: the scope's head
/// already covers a change inside a selected doc.
fn target_key(st: &ReadState, target: &SiteTarget, generation: u64) -> Result<Hash, RdyError> {
    let mut k = Vec::new();
    let text = |k: &mut Vec<u8>, s: &str| {
        k.extend_from_slice(&(s.len() as u64).to_le_bytes());
        k.extend_from_slice(s.as_bytes());
    };
    let (mode, base) = match target {
        SiteTarget::Source(scope) => (0u8, inputs_key(st, &resolve(st, scope)?, generation)?),
        SiteTarget::Profile { scope, .. } => (1, inputs_key(st, &resolve(st, scope)?, generation)?),
        SiteTarget::ProfileOnly { .. } => (2, inputs_key(st, &Scope::Canon, generation)?),
        SiteTarget::Menu { .. } => (3, menu_key_for(st, target, generation)?),
    };
    k.push(mode);
    k.extend_from_slice(&base.0);
    text(&mut k, &target.scope().text());
    match target {
        SiteTarget::Profile { key, .. } | SiteTarget::ProfileOnly { key } => {
            k.push(1);
            text(&mut k, key);
        }
        SiteTarget::Menu {
            profile: Some(key), ..
        } => {
            k.push(1);
            text(&mut k, key);
        }
        _ => k.push(0),
    }
    Ok(Hash::of(&k))
}

/// The change summary a source build of `scope` (`resolved` in `st`) generated, by the rules
/// `rdy_site` gives its link by: a view or a proposal version has `changes.html`, and canon
/// `changes-since.html` while it has landings since its tag, unless a top-level doc owns that
/// page (the build then keeps the doc's page). A tag has none.
fn source_changes(
    st: &ReadState,
    resolved: &Scope,
    scope: &ScopeRef,
    live: &LiveBuild,
) -> Result<Option<&'static str>, RdyError> {
    let page = match scope {
        ScopeRef::View(_) | ScopeRef::Proposal(_) => CHANGES_PAGE,
        ScopeRef::Canon => SINCE_PAGE,
        ScopeRef::Tag(_) | ScopeRef::Versions => return Ok(None),
    };
    if !live.files.contains_key(page) {
        return Ok(None);
    }
    let owned = summary_page_taken(&*st.materialized(resolved)?, page);
    Ok((!owned).then_some(page))
}

/// An open page's stream. Dropping it removes the stream.
pub struct Subscription {
    pub id: u64,
    pub rx: mpsc::Receiver<LiveEvent>,
    /// The redaction epoch the stream was compared in (see `Stream::epoch`).
    epoch: u64,
    key: StreamKey,
    inner: Weak<Mutex<Inner>>,
    /// The site's count of memory releases: a close that drops a build gives memory back.
    releases: Arc<AtomicU64>,
}

impl Subscription {
    /// The redaction epoch the stream was compared in: an event sent while the store's epoch is
    /// another one goes out as `Reload` ([`crate::site::sse::serve`]).
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            let mut inner = lock(&inner);
            match &self.key {
                StreamKey::Site(scope) => {
                    if let Some(s) = inner.scopes.get_mut(scope) {
                        s.streams.retain(|st| st.id != self.id);
                    }
                    // the target's build may be inactive now: the count and byte caps hold
                    // from here, not from the next build (review F1); this also forgets the
                    // target when it holds neither a build nor a stream
                    if inner.evict() {
                        // the dropped build's memory goes back, never under the site lock
                        drop(inner);
                        release_free_memory(&self.releases);
                    }
                }
                StreamKey::Ui => inner.ui_streams.retain(|st| st.id != self.id),
            }
        }
    }
}

impl LiveSite {
    pub fn new() -> Arc<LiveSite> {
        Arc::new(LiveSite {
            published: AtomicU64::new(0),
            worker: OnceLock::new(),
            stopped: AtomicBool::new(false),
            building: Mutex::new(()),
            inner: Arc::new(Mutex::new(Inner {
                cap: MAX_INACTIVE_BUILD_BYTES,
                ..Inner::default()
            })),
            builds: AtomicU64::new(0),
            review_builds: AtomicU64::new(0),
            preparations: AtomicU64::new(0),
            renders: AtomicU64::new(0),
            checks: AtomicU64::new(0),
            memory_releases: Arc::new(AtomicU64::new(0)),
            counts: Mutex::new(BTreeMap::new()),
            docs_changed: Mutex::new(None),
            before_publish: Mutex::new(None),
            after_subscribe_refresh: Mutex::new(None),
            log_hook: Mutex::new(None),
            history_ceiling: Mutex::new(None),
            before_ui_compare: Mutex::new(None),
        })
    }

    /// Test hook: from now on, `f` also gets each line the site logs (`None` removes it).
    #[doc(hidden)]
    pub fn set_log_hook(&self, f: Option<LogHook>) {
        *lock(&self.log_hook) = f;
    }

    /// Test hook: from now on, the held builds hold at most `cap` bytes together (`None`:
    /// [`MAX_INACTIVE_BUILD_BYTES`]).
    #[doc(hidden)]
    pub fn set_inactive_build_cap(&self, cap: Option<u64>) {
        let mut inner = lock(&self.inner);
        inner.cap = cap.unwrap_or(MAX_INACTIVE_BUILD_BYTES);
        let _ = inner.evict();
    }

    /// Test hook: the builds the site holds, of every target.
    #[doc(hidden)]
    pub fn held_builds(&self) -> usize {
        lock(&self.inner)
            .scopes
            .values()
            .filter(|s| s.build.is_some())
            .count()
    }

    /// Test hook: everything the site's cap counts: the eager builds, and the review targets'
    /// pages, shared parts and warning state.
    #[doc(hidden)]
    pub fn held_bytes(&self) -> u64 {
        lock(&self.inner).total()
    }

    /// Test hook: the ledger of the site's cap, part by part.
    #[doc(hidden)]
    pub fn ledger(&self) -> Ledger {
        let inner = lock(&self.inner);
        let mut kept: Vec<(u64, SiteTarget, String, u64)> = inner
            .scopes
            .iter()
            .flat_map(|(t, s)| {
                s.review
                    .pages
                    .iter()
                    .map(move |(p, k)| (k.used, t.clone(), p.clone(), k.bytes))
            })
            .collect();
        kept.sort_by_key(|(used, ..)| *used);
        let (eager, shared, pages, warnings) = (
            inner.eager_bytes(),
            inner.shared_bytes(),
            inner.page_bytes(),
            inner.warning_bytes(),
        );
        Ledger {
            total: eager + shared + pages + warnings,
            eager,
            shared,
            pages,
            warnings,
            kept: kept.into_iter().map(|(_, t, p, b)| (t, p, b)).collect(),
            prepared: inner
                .scopes
                .iter()
                .filter(|(_, s)| s.review.prep.is_some())
                .map(|(t, _)| t.clone())
                .collect(),
            logged: inner.scopes.values().map(|s| s.review.logged.len()).sum(),
            targets: inner.scopes.len(),
        }
    }

    /// Test hook: what the preparation `target` keeps counts on its own, if it keeps one.
    #[doc(hidden)]
    pub fn prepared_bytes(&self, target: &SiteTarget) -> Option<u64> {
        let inner = lock(&self.inner);
        let prep = inner.scopes.get(&normalize(target))?.review.prep.clone()?;
        drop(inner);
        Some(prep_alone(&prep))
    }

    /// Test hook: a weak handle of the preparation `target` keeps, if it keeps one, to prove
    /// its release.
    #[doc(hidden)]
    pub fn prepared_handle(&self, target: &SiteTarget) -> Option<Weak<PreparedReview>> {
        let inner = lock(&self.inner);
        let s = inner.scopes.get(&normalize(target))?;
        s.review.prep.as_ref().map(Arc::downgrade)
    }

    /// Test hook: the warnings logged for `target` that the site keeps, if it keeps the target.
    #[doc(hidden)]
    pub fn logged_warnings(&self, target: &SiteTarget) -> Option<Vec<String>> {
        let inner = lock(&self.inner);
        let s = inner.scopes.get(&normalize(target))?;
        Some(s.review.logged.iter().cloned().collect())
    }

    /// Whole builds asked of a review target; none is ever made.
    pub fn review_builds(&self) -> u64 {
        self.review_builds.load(Ordering::SeqCst)
    }

    /// Review target preparations made so far.
    pub fn preparations(&self) -> u64 {
        self.preparations.load(Ordering::SeqCst)
    }

    /// Requested files of review targets rendered so far (a kept page is not rendered again).
    pub fn page_renders(&self) -> u64 {
        self.renders.load(Ordering::SeqCst)
    }

    /// Complete warnings passes of review targets run so far.
    pub fn checks(&self) -> u64 {
        self.checks.load(Ordering::SeqCst)
    }

    /// Test hook: the bytes of the inactive targets' builds (a build and no open stream).
    #[doc(hidden)]
    pub fn held_inactive_bytes(&self) -> u64 {
        lock(&self.inner).inactive_bytes()
    }

    /// Test hook: from now on, each profile build counts at most `n` landings since its tag
    /// before it omits the neutral chip (`None`: `history_facts::MAX_LANDINGS`).
    #[doc(hidden)]
    pub fn set_history_ceiling(&self, n: Option<usize>) {
        *lock(&self.history_ceiling) = n;
    }

    /// Test hook: how many landing pairs the history memo kept for `target` has evaluated, if
    /// the site keeps a slot for it.
    #[doc(hidden)]
    pub fn history_evaluations(&self, target: &SiteTarget) -> Option<u64> {
        lock(&self.inner)
            .scopes
            .get(&normalize(target))
            .map(|s| s.memo.evaluations_for_tests())
    }

    /// A line of the site's log, and the test hook's.
    fn log(&self, text: &str) {
        log(text);
        if let Some(f) = &*lock(&self.log_hook) {
            f(text);
        }
    }

    /// Test hook: counts the docs a view or proposal version changes with `f` from now on.
    #[doc(hidden)]
    pub fn set_docs_changed(&self, f: DocsChanged) {
        *lock(&self.docs_changed) = Some(f);
    }

    /// Test hook: from now on, each build runs `f` with its target at its publication check,
    /// after the build and before the epoch check (`None` removes it). A barrier in `f` holds
    /// the build there; the build runs on the thread that refreshes, holding the one-build lock.
    #[doc(hidden)]
    pub fn set_before_publish(&self, f: Option<BeforePublish>) {
        *lock(&self.before_publish) = f.map(Arc::from);
    }

    /// Test hook: from now on, each subscribe runs `f` with its target after its refresh returns
    /// and before its stream is registered (`None` removes it). A barrier in `f` holds the
    /// subscribe there, holding no lock.
    #[doc(hidden)]
    pub fn set_after_subscribe_refresh(&self, f: Option<BeforePublish>) {
        *lock(&self.after_subscribe_refresh) = f.map(Arc::from);
    }

    /// The docs `scope` changes, from the cache when what it reads is unchanged.
    fn docs_changed(
        &self,
        st: &ReadState,
        scope: &Scope,
        generation: u64,
    ) -> Result<usize, RdyError> {
        let key = count_key(st, scope, generation);
        if let Some(n) = key
            .as_ref()
            .and_then(|k| lock(&self.counts).get(k).copied())
        {
            return Ok(n);
        }
        let n = match &*lock(&self.docs_changed) {
            Some(f) => f(st, scope)?,
            None => docs_changed(st, scope)?,
        };
        if let Some(key) = key {
            let mut counts = lock(&self.counts);
            if counts.len() >= MAX_COUNTS {
                counts.clear();
            }
            counts.insert(key, n);
        }
        Ok(n)
    }

    /// A publish happened: counts it and wakes the worker. Called by the writer after each
    /// store into `Shared::state`; it never builds, never waits and takes no lock.
    pub fn published(&self) {
        self.published.fetch_add(1, Ordering::SeqCst);
        // the worker looks at the count before it parks, and a park after this unpark returns
        // at once, so the wake is never lost
        if let Some(worker) = self.worker.get() {
            worker.unpark();
        }
    }

    /// The build of eager `target` as of the latest publish, built when its inputs changed. A
    /// new build sends each open page of the target what changed for it (see the module docs). A
    /// failed build is the error, and the last build stays. `Source(Versions)` is the menu of
    /// canon; an invalid target is refused before anything is kept. A review target has no whole
    /// build: it is refused with the invalid-target error, and its files come from
    /// [`Self::review_page`].
    pub fn refresh(&self, sh: &Shared, target: &SiteTarget) -> Result<Arc<SiteBuild>, RdyError> {
        self.refresh_counted(sh, target).map(|(b, _)| b)
    }

    /// [`Self::refresh`], and whether it built.
    fn refresh_counted(
        &self,
        sh: &Shared,
        target: &SiteTarget,
    ) -> Result<(Arc<SiteBuild>, bool), RdyError> {
        let scope = &normalize(target);
        scope.validate()?;
        if is_review(scope) {
            return Err(not_eager());
        }
        let _one = lock(&self.building);
        // the count before the state: a publish after this load counts past `now`, so the next
        // refresh reads it
        let now = self.published.load(Ordering::SeqCst);
        // the store's epoch now: a cached build of another epoch is never served (a removal
        // between a build's check and its insertion leaves it in the cache, unused)
        let current = sh.state.load().reader().redaction_epoch();
        // an unknown target leaves no state: nothing is touched before the target resolves and
        // builds (or finds its build). A hit comes with whether its eviction dropped a build.
        let cached = |inner: &mut Inner, fresh: &dyn Fn(&ScopeState) -> bool| {
            let hit = inner.scopes.get(scope).and_then(|s| {
                s.build
                    .as_ref()
                    .filter(|b| b.live.epoch == current && fresh(s))
                    .map(Arc::clone)
            })?;
            let s = inner.touch(scope);
            s.seen = now;
            Some((hit, inner.evict()))
        };
        // a dropped build's memory goes back once the site lock is released: each lookup is its
        // own statement, so its guard is gone before `hit` runs
        let hit = |found: Option<(Arc<SiteBuild>, bool)>| {
            found.map(|(b, dropped)| {
                if dropped {
                    self.release_free_memory();
                }
                (b, false)
            })
        };
        let found = cached(&mut lock(&self.inner), &|s| s.seen == now);
        if let Some(out) = hit(found) {
            return Ok(out);
        }
        // the generation before the state: the writer moves it only after it stores the state
        // that holds the move, so a newer generation never pairs with an older state (an older
        // generation with a newer state only builds once more, to the same pages)
        let generation = sh.pack_gen.load(Ordering::SeqCst);
        let st = sh.state.load_full();
        // a state older than a removal builds nothing: the writer publishes the fresh one next
        st.ensure_current_epoch()?;
        let key = (st.redaction_epoch(), target_key(&st, scope, generation)?);
        let found = cached(&mut lock(&self.inner), &|s| s.key == Some(key));
        if let Some(out) = hit(found) {
            return Ok(out);
        }
        // a profile build continues its slot's memo; the memo goes back only with a published
        // build, so a failed build leaves no new cursor or result behind
        let selected = matches!(
            scope,
            SiteTarget::Profile { .. } | SiteTarget::ProfileOnly { .. }
        );
        let mut memo = if selected {
            lock(&self.inner)
                .scopes
                .get_mut(scope)
                .map(|s| std::mem::take(&mut s.memo))
                .unwrap_or_default()
        } else {
            ProfileHistoryMemo::default()
        };
        memo.set_ceiling_for_tests(*lock(&self.history_ceiling));
        let new = Arc::new(self.build(&st, scope, generation, &mut memo)?);
        self.builds.fetch_add(1, Ordering::SeqCst);
        let hook = lock(&self.before_publish).clone();
        if let Some(hook) = hook {
            hook(scope);
        }
        let mut inner = lock(&self.inner);
        // the publication check, under the lock that publishes: a build whose state a removal
        // made old is discarded, and the last build stays (never served in the new epoch)
        st.ensure_current_epoch()?;
        let s = inner.touch(scope);
        // the render's warnings, logged once each when a build of the target first has them
        // (FW-UX1: the person hears of them through the agent's `rdy_site` too); a warning that
        // stays while it holds is not logged again, and one that comes back after a build
        // without it is
        let before: &[String] = s.build.as_ref().map_or(&[], |old| &old.live.warnings);
        let fresh: Vec<&String> = new
            .live
            .warnings
            .iter()
            .filter(|w| !before.contains(w))
            .collect();
        for w in fresh {
            self.log(&format!("site: {}: warning: {w}", label(scope)));
        }
        let change = s.build.as_ref().map(|old| diff(&old.live, &new.live));
        let built = &new.live;
        s.streams.retain_mut(|stream| {
            let event = match &change {
                // a page compared in another epoch (opened before a removal) loads again: the
                // event carries nothing, and the page it loads is this epoch's
                _ if stream.epoch != built.epoch => Some(LiveEvent::Reload),
                // the fan-out rule: what changed between the two builds
                Some(change) if stream.checked => {
                    if change.reload.contains(&stream.page) {
                        Some(LiveEvent::Reload)
                    } else {
                        change.label.clone().map(|text| LiveEvent::Label {
                            text,
                            html: built.header.clone(),
                        })
                    }
                }
                // no build to diff, or the stream opened with no comparison: compared with
                // what it opened at, as a subscribe compares it
                _ => stream.against(built),
            };
            stream.checked = true;
            match event {
                // a full or closed queue drops the stream: its page reconnects with its revision
                Some(event) => stream.tx.try_send(event).is_ok(),
                None => true,
            }
        });
        s.bytes = held_size(&new);
        s.build = Some(Arc::clone(&new));
        s.key = Some(key);
        s.seen = now;
        if selected {
            s.memo = memo;
        }
        let _ = inner.evict();
        // the build's freed memory goes back to the system, never while the site lock is held:
        // the trim holds the allocator's arenas for a fraction of a second
        drop(inner);
        self.release_free_memory();
        Ok((new, true))
    }

    /// Gives the allocator's free memory back after a build or an eviction, and counts it.
    fn release_free_memory(&self) {
        release_free_memory(&self.memory_releases);
    }

    /// Builds eager `target` (normalized and valid) from `st`, with `memo` for a profile's
    /// history. A review target is counted and refused: it has no whole build.
    fn build(
        &self,
        st: &ReadState,
        target: &SiteTarget,
        generation: u64,
        memo: &mut ProfileHistoryMemo,
    ) -> Result<SiteBuild, RdyError> {
        if is_review(target) {
            self.review_builds.fetch_add(1, Ordering::SeqCst);
            return Err(not_eager());
        }
        match target {
            SiteTarget::Source(scope) => {
                let resolved = resolve(st, scope)?;
                let live = build_live(st, &resolved, target)?;
                let changes = source_changes(st, &resolved, scope, &live)?;
                Ok(SiteBuild {
                    live,
                    profile: None,
                    changes,
                })
            }
            SiteTarget::Profile { .. } | SiteTarget::ProfileOnly { .. } => {
                let b = build_profile_live(st, target, memo)?;
                Ok(SiteBuild {
                    live: b.live,
                    profile: Some(b.profile),
                    changes: b.changes,
                })
            }
            SiteTarget::Menu { .. } => Ok(SiteBuild {
                live: build_versions_for(st, target, &mut |s| {
                    self.docs_changed(st, s, generation)
                })?,
                profile: None,
                changes: None,
            }),
        }
    }

    /// The state of review target `scope` (normalized) as of the latest publish: the published
    /// state, its input key and the publish count it read. A kept state of another key is dropped
    /// here with every page of it, before anything of the new state is kept: a request never gets
    /// a page of an older state while the worker catches up. The logged warnings stay. Called
    /// holding the one-build lock; also says whether kept data was dropped.
    fn review_fresh(&self, sh: &Shared, scope: &SiteTarget) -> Result<(Fresh, bool), RdyError> {
        // the count, then the generation, then the state, as for a build
        let now = self.published.load(Ordering::SeqCst);
        let generation = sh.pack_gen.load(Ordering::SeqCst);
        let st = sh.state.load_full();
        // a state older than a removal renders nothing: the writer publishes the fresh one next
        st.ensure_current_epoch()?;
        let epoch = st.redaction_epoch();
        // a key found at this publish count needs no new reading
        let known = lock(&self.inner)
            .scopes
            .get(scope)
            .and_then(|s| s.review.key.filter(|k| s.seen == now && k.0 == epoch));
        let key = match known {
            Some(key) => key,
            None => (epoch, target_key(&st, scope, generation)?),
        };
        let mut dropped = false;
        if let Some(s) = lock(&self.inner).scopes.get_mut(scope) {
            if s.review.key.is_some_and(|k| k != key) {
                dropped = s.review.forget_state();
            }
            if s.review.key == Some(key) {
                s.seen = now;
            }
        }
        Ok((Fresh { st, key, now }, dropped))
    }

    /// The preparation of review target `scope` at `fresh`: the one it keeps, or a new one
    /// (counted). A profile route continues its slot's memo, which goes back with the admission
    /// of the new preparation; a failed preparation leaves no new memo state behind.
    fn review_prep(&self, scope: &SiteTarget, fresh: &Fresh) -> Result<Prep, RdyError> {
        let kept = lock(&self.inner)
            .scopes
            .get(scope)
            .filter(|s| s.review.key == Some(fresh.key))
            .and_then(|s| s.review.prep.clone());
        if let Some(prep) = kept {
            return Ok(Prep {
                prep,
                new: false,
                memo: None,
            });
        }
        let st = &*fresh.st;
        let (prep, memo) = match scope {
            SiteTarget::Source(s) => (prepare_review(st, &resolve(st, s)?, scope)?, None),
            _ => {
                let mut memo = lock(&self.inner)
                    .scopes
                    .get_mut(scope)
                    .map(|s| std::mem::take(&mut s.memo))
                    .unwrap_or_default();
                memo.set_ceiling_for_tests(*lock(&self.history_ceiling));
                (prepare_profile_review(st, scope, &mut memo)?, Some(memo))
            }
        };
        self.preparations.fetch_add(1, Ordering::SeqCst);
        Ok(Prep {
            prep: Arc::new(prep),
            new: true,
            memo,
        })
    }

    /// Keeps what a review request of `scope` at `fresh` made, under the lock that publishes it,
    /// only while its state is of the store's epoch (else nothing is kept, and the request fails
    /// as a discarded build does). Each warning the request found that the target has not logged
    /// is logged once: a new preparation's, a page's diagrams', a complete pass's in its order. A
    /// complete pass replaces the logged set, as it alone proves a warning gone. A page larger
    /// than the cap is not kept; a preparation and warning state that cannot fit beside the
    /// eager builds served this request and are dropped with the target's other records. Then
    /// the cap holds. Says whether kept data was dropped.
    fn review_admit(
        &self,
        scope: &SiteTarget,
        fresh: &Fresh,
        prep: &mut Prep,
        page: Option<(&str, &Arc<ReviewFile>)>,
        check: Option<&Arc<ReviewCheck>>,
    ) -> Result<bool, RdyError> {
        let hook = lock(&self.before_publish).clone();
        if let Some(hook) = hook {
            hook(scope);
        }
        let mut inner = lock(&self.inner);
        // the publication check: a state a removal made old keeps nothing
        fresh.st.ensure_current_epoch()?;
        let (cap, eager) = (inner.cap, inner.eager_bytes());
        inner.clock += 1;
        let now = inner.clock;
        let s = inner.scopes.entry(scope.clone()).or_default();
        s.used = now;
        s.seen = fresh.now;
        let mut dropped = false;
        if s.review.key != Some(fresh.key) {
            dropped |= s.review.forget_state();
            s.review.key = Some(fresh.key);
        }
        if let Some(memo) = prep.memo.take() {
            s.memo = memo;
        }
        let mut found: Vec<&String> = vec![];
        if prep.new {
            found.extend(prep.prep.warnings());
        }
        if let Some((_, f)) = page {
            found.extend(&f.warnings);
        }
        if let Some(c) = check {
            found.extend(&c.warnings);
        }
        let mut lines = vec![];
        for w in found {
            if s.review.logged.insert(w.clone()) {
                lines.push(format!("site: {}: warning: {w}", label(scope)));
            }
        }
        if let Some(c) = check {
            s.review.logged = c.warnings.iter().cloned().collect();
            s.review.check = Some(Arc::clone(c));
        }
        if s.review.prep.is_none() {
            s.review.prep = Some(Arc::clone(&prep.prep));
        }
        if let Some((path, f)) = page {
            let bytes = page_charge(path, f);
            if bytes <= cap {
                let file = Arc::clone(f);
                s.review.pages.insert(
                    path.to_string(),
                    KeptPage {
                        file,
                        bytes,
                        used: now,
                    },
                );
            }
        }
        // shared parts and warning state that cannot fit beside the eager builds served this
        // request only: the target keeps its streams' facts and nothing else (D-WYZ97, D-SMP5A)
        if eager + prep_alone(&prep.prep) + s.review.warning_bytes() > cap {
            s.review = Review::default();
            dropped = true;
        }
        for line in &lines {
            self.log(line);
        }
        dropped |= inner.evict();
        Ok(dropped)
    }

    /// The file at `path` of review target `scope` at `fresh`: the kept page, or the file
    /// rendered alone (counted) and admitted. None for a path the target has no file at; only
    /// the preparation is then kept. Also says whether kept data was dropped.
    fn review_file(
        &self,
        scope: &SiteTarget,
        fresh: &Fresh,
        path: &str,
    ) -> Result<FileAt, RdyError> {
        {
            let mut inner = lock(&self.inner);
            inner.clock += 1;
            let now = inner.clock;
            if let Some(s) = inner.scopes.get_mut(scope)
                && s.review.key == Some(fresh.key)
                && let Some(prep) = s.review.prep.clone()
                && let Some(p) = s.review.pages.get_mut(path)
            {
                // a hit moves only this page's recency
                p.used = now;
                s.used = now;
                s.seen = fresh.now;
                return Ok((prep, Some(Arc::clone(&p.file)), false));
            }
        }
        let mut prep = self.review_prep(scope, fresh)?;
        let file = prep.prep.render(&fresh.st, path)?.map(|mut f| {
            f.bytes.shrink_to_fit();
            Arc::new(f)
        });
        if file.is_some() {
            self.renders.fetch_add(1, Ordering::SeqCst);
        }
        let page = file.as_ref().map(|f| (path, f));
        let dropped = self.review_admit(scope, fresh, &mut prep, page, None)?;
        Ok((prep.prep, file, dropped))
    }

    /// Compares each open page of review target `scope` not yet compared at `fresh` with the
    /// page as that state renders it: each distinct page renders once (or comes from the cache),
    /// and every stream on it shares that render. Other pages are not rendered. Also says
    /// whether kept data was dropped.
    fn review_fanout(&self, scope: &SiteTarget, fresh: &Fresh) -> Result<bool, RdyError> {
        let pages: BTreeSet<String> = lock(&self.inner)
            .scopes
            .get(scope)
            .map(|s| {
                s.streams
                    .iter()
                    .filter(|st| st.key != Some(fresh.key))
                    .map(|st| st.page.clone())
                    .collect()
            })
            .unwrap_or_default();
        let mut dropped = false;
        let mut facts = BTreeMap::new();
        for page in pages {
            let (prep, file, d) = self.review_file(scope, fresh, &page)?;
            dropped |= d;
            facts.insert(page, PageFacts::of(&prep, file.as_deref()));
        }
        if facts.is_empty() {
            return Ok(dropped);
        }
        let mut inner = lock(&self.inner);
        if let Some(s) = inner.scopes.get_mut(scope) {
            s.streams.retain_mut(|stream| {
                if stream.key == Some(fresh.key) {
                    return true;
                }
                // a stream registered since is compared by its own subscribe
                let Some(now) = facts.get(&stream.page) else {
                    return true;
                };
                match stream.follow(now, fresh.key) {
                    // a full or closed queue drops the stream: its page reconnects with its
                    // revision
                    Some(event) => stream.tx.try_send(event).is_ok(),
                    None => true,
                }
            });
        }
        Ok(dropped)
    }

    /// Review target `scope` (normalized) brought to the latest publish: a changed state drops
    /// what it kept, and each open page not yet compared with the new state is. Gives the
    /// dropped memory back. Takes the one-build lock.
    fn review_refresh(&self, sh: &Shared, scope: &SiteTarget) -> Result<(), RdyError> {
        let _one = lock(&self.building);
        let (fresh, dropped) = self.review_fresh(sh, scope)?;
        let out = self.review_fanout(scope, &fresh);
        if dropped || matches!(out, Ok(true)) {
            self.release_free_memory();
        }
        out.map(|_| ())
    }

    /// Refuses a target that is not a review target, after it is normalized and valid.
    fn review_target(target: &SiteTarget) -> Result<SiteTarget, RdyError> {
        let scope = normalize(target);
        scope.validate()?;
        if !is_review(&scope) {
            return Err(not_eager());
        }
        Ok(scope)
    }

    /// The file at `path` of review target `target` as of the latest publish, as the server
    /// sends it: rendered alone on request, or the kept page (D-F44G6). The target's open pages
    /// hear of a new state first, as a build fans out. None for a path the target has no file
    /// at. A target that does not resolve is the error, and keeps nothing; an eager target is
    /// refused with the invalid-target error.
    pub fn review_page(
        &self,
        sh: &Shared,
        target: &SiteTarget,
        path: &str,
    ) -> Result<Option<ReviewPage>, RdyError> {
        let scope = &Self::review_target(target)?;
        let _one = lock(&self.building);
        let (fresh, mut dropped) = self.review_fresh(sh, scope)?;
        let out = self.review_fanout(scope, &fresh).and_then(|d| {
            dropped |= d;
            self.review_file(scope, &fresh, path)
        });
        let out = out.map(|(prep, file, d)| {
            dropped |= d;
            file.map(|f| ReviewPage {
                bytes: prep.serve(&f, path, target),
                rev: f.rev.clone(),
                label: prep.label().to_string(),
                header: prep.header().to_string(),
                label_rev: prep.label_rev().to_string(),
                epoch: prep.epoch(),
            })
        });
        if dropped {
            self.release_free_memory();
        }
        out
    }

    /// The not-found page of review target `target` as of the latest publish (the Page variant,
    /// rooted at its root), rendered on request from its preparation, with the epoch its state
    /// captured. It has no stream, revision or index entry, and no missing path is kept.
    pub fn review_not_found(
        &self,
        sh: &Shared,
        target: &SiteTarget,
    ) -> Result<Option<(u64, Vec<u8>)>, RdyError> {
        let scope = &Self::review_target(target)?;
        let _one = lock(&self.building);
        let (fresh, mut dropped) = self.review_fresh(sh, scope)?;
        let mut prep = self.review_prep(scope, &fresh)?;
        if prep.new {
            dropped |= self.review_admit(scope, &fresh, &mut prep, None, None)?;
        }
        if dropped {
            self.release_free_memory();
        }
        let html = prep.prep.not_found(NotFoundKind::Page);
        Ok(html.map(|h| (prep.prep.epoch(), h)))
    }

    /// The link facts of review target `target` as of the latest publish: the profile it resolves
    /// to, its change summary's page, and the complete warnings pass of its state, in the whole
    /// build's order (p-BFXZS). The pass runs once per kept state, renders no page, and logs
    /// each warning the target has not logged; concurrent asks share it.
    pub fn review_facts(&self, sh: &Shared, target: &SiteTarget) -> Result<SiteFacts, RdyError> {
        let scope = &Self::review_target(target)?;
        let _one = lock(&self.building);
        let (fresh, mut dropped) = self.review_fresh(sh, scope)?;
        let facts = |prep: &PreparedReview, check: &ReviewCheck| SiteFacts {
            profile: prep.profile().cloned(),
            changes: prep.changes(),
            warnings: check.warnings.clone(),
        };
        let kept = {
            let mut inner = lock(&self.inner);
            inner.clock += 1;
            let now = inner.clock;
            inner.scopes.get_mut(scope).and_then(|s| {
                let r = &s.review;
                let kept = (r.key == Some(fresh.key))
                    .then(|| r.prep.clone().zip(r.check.clone()))
                    .flatten();
                if kept.is_some() {
                    s.used = now;
                    s.seen = fresh.now;
                }
                kept
            })
        };
        let out = match kept {
            Some((prep, check)) => Ok(facts(&prep, &check)),
            None => self.review_prep(scope, &fresh).and_then(|mut prep| {
                let check = Arc::new(prep.prep.check(&fresh.st)?);
                self.checks.fetch_add(1, Ordering::SeqCst);
                dropped |= self.review_admit(scope, &fresh, &mut prep, None, Some(&check))?;
                Ok(facts(&prep.prep, &check))
            }),
        };
        if dropped {
            self.release_free_memory();
        }
        out
    }

    /// One wake of the worker: reloads the UI streams, then refreshes each target with an open
    /// stream, and returns how many it built. A failure is logged once per run of failures of a
    /// target.
    pub fn step(&self, sh: &Shared) -> usize {
        self.published_to_ui(sh);
        // every profile-only key against one captured canon state, open pages or not; a key
        // canon no longer holds has its target removed before anything builds (D-DK03K)
        sh.profile_sites.inspect(sh);
        let (open, review): (Vec<SiteTarget>, Vec<SiteTarget>) = {
            let inner = lock(&self.inner);
            let open = inner
                .scopes
                .iter()
                .filter(|(k, s)| !is_review(k) && !s.streams.is_empty())
                .map(|(k, _)| k.clone())
                .collect();
            // every kept review target, open or not: a changed state drops what it kept
            let review = inner
                .scopes
                .iter()
                .filter(|(k, s)| is_review(k) && (s.review.holds() || !s.streams.is_empty()))
                .map(|(k, _)| k.clone())
                .collect();
            (open, review)
        };
        let renders = self.page_renders();
        self.step_review(sh, review);
        let mut built = (self.page_renders() - renders) as usize;
        for scope in open {
            let out = self.refresh_counted(sh, &scope);
            let mut inner = lock(&self.inner);
            let Some(s) = inner.scopes.get_mut(&scope) else {
                continue;
            };
            match out {
                Ok((_, did)) => {
                    built += usize::from(did);
                    s.failing = false;
                }
                // a build of an older epoch was discarded: the fresh state's publish wakes the
                // worker again, and that build fans out
                Err(e) if e.code == E_REDACTED_STATE => {}
                Err(e) => {
                    if !s.failing {
                        s.failing = true;
                        drop(inner);
                        self.log(&format!(
                            "site: the {} pages did not build ({e}); open pages keep what they show until a build succeeds",
                            label(&scope)
                        ));
                    }
                }
            }
        }
        built
    }

    /// The worker's review targets: each is brought to the latest publish ([`Self::
    /// review_refresh`]): a changed state drops its pages and preparation, and only the pages
    /// with an open stream render again. A target that no longer reads drops what it kept of
    /// its older state; its open pages keep what they show, and the failure is logged once per
    /// run of failures of a target with an open page.
    fn step_review(&self, sh: &Shared, targets: Vec<SiteTarget>) {
        for scope in targets {
            match self.review_refresh(sh, &scope) {
                Ok(()) => {
                    if let Some(s) = lock(&self.inner).scopes.get_mut(&scope) {
                        s.failing = false;
                    }
                }
                // the fresh state's publish wakes the worker again
                Err(e) if e.code == E_REDACTED_STATE => {}
                Err(e) => {
                    let mut inner = lock(&self.inner);
                    let Some(s) = inner.scopes.get_mut(&scope) else {
                        continue;
                    };
                    let mut dropped = s.review.forget_state();
                    let open = !s.streams.is_empty();
                    let first = open && !s.failing;
                    s.failing |= open;
                    dropped |= inner.evict();
                    drop(inner);
                    if first {
                        self.log(&format!(
                            "site: the {} pages did not build ({e}); open pages keep what they show until a build succeeds",
                            label(&scope)
                        ));
                    }
                    if dropped {
                        self.release_free_memory();
                    }
                }
            }
        }
    }

    /// Opens a stream for `page` of `target`, which the reader has at revision `rev` with the
    /// label at `label_rev`. The target is refreshed first. A page the build does not have, or
    /// has at another revision, gets `Reload` at once; else a page with another label gets the
    /// label. When the scope does not build, the stream opens with no event. When a removal
    /// makes the build old before it is compared, the stream is compared with a build of the
    /// fresh state once registered (or by the worker's step for its publish). Past
    /// [`MAX_STREAMS`] open streams, and after `close`, the site is busy.
    pub fn subscribe(
        &self,
        sh: &Shared,
        target: &SiteTarget,
        page: &str,
        rev: &str,
        label_rev: &str,
    ) -> Result<Subscription, NoStream> {
        let scope = &normalize(target);
        {
            let inner = lock(&self.inner);
            if inner.closed || inner.streams() >= MAX_STREAMS {
                return Err(NoStream::Busy);
            }
        }
        if is_review(scope) {
            return self.subscribe_review(sh, scope, page, rev, label_rev);
        }
        let first = self.refresh(sh, scope);
        // the build read a state that a removal made old: the stream is compared again below
        let stale = matches!(&first, Err(e) if e.code == E_REDACTED_STATE);
        let refreshed = first.is_ok();
        let hook = lock(&self.after_subscribe_refresh).clone();
        if let Some(hook) = hook {
            hook(scope);
        }
        let mut inner = lock(&self.inner);
        if inner.closed || inner.streams() >= MAX_STREAMS {
            return Err(NoStream::Busy);
        }
        // a profile-only target opens a stream only while canon holds its key (D-DK03K), in the
        // state published now, read under the lock a removal's clearing takes
        // ([`Self::remove`]): a removal whose state was published before this load is seen
        // here, so no stream registers after its target was cleared; the clearing of one
        // published after it finds the stream registered below. A request's check has usually
        // remembered the answer for this state already.
        if let Some(no) = unavailable(sh, scope, &sh.state.load_full()) {
            return Err(no);
        }
        // the store's epoch, sampled under the lock a build's fan-out takes (final review
        // finding 4): a removal whose epoch moved before this sample is seen here, so the
        // scope's old build is never compared as current; one whose epoch moves after it
        // publishes its fresh state after the stream below is registered, and the fan-out of
        // that state's build finds the stream
        let current = sh.state.load().reader().redaction_epoch();
        inner.next_id += 1;
        let id = inner.next_id;
        let (tx, rx) = mpsc::channel(STREAM_QUEUE);
        let s = inner.touch(scope);
        let mut stream = Stream {
            id,
            page: page.to_string(),
            rev: Some(rev.to_string()),
            label_rev: label_rev.to_string(),
            key: None,
            epoch: current,
            checked: false,
            tx,
        };
        // compared with the scope's build as it stands now: a build since the refresh above
        // fanned out without this stream. With no comparison, the scope's next build compares;
        // a build of an older epoch is never compared with (the epoch check before each event)
        if refreshed
            && let Some(b) = &s.build
            && b.live.epoch == current
        {
            if let Some(event) = stream.against(&b.live) {
                let _ = stream.tx.try_send(event);
            }
            stream.checked = true;
        }
        let epoch = stream.epoch;
        // not compared for want of a build of the current epoch (not for a scope that does not
        // build): compared again once the stream is registered
        let again = !stream.checked && (stale || refreshed);
        s.streams.push(stream);
        drop(inner);
        let sub = Subscription {
            id,
            rx,
            epoch,
            key: StreamKey::Site(scope.clone()),
            inner: Arc::downgrade(&self.inner),
            releases: Arc::clone(&self.memory_releases),
        };
        // A removal came between the build and the comparison. Its fresh state may be
        // published already, and the worker's step for that publish may have run before the
        // stream was registered: nothing else would refresh the scope, so it is built and
        // compared here. While the published state is still old, its publish comes after the
        // registration above, so the worker's step for it finds the stream.
        if again
            && sh.state.load().ensure_current_epoch().is_ok()
            && let Ok(b) = self.refresh(sh, scope)
        {
            self.compare(scope, id, &b.live);
        }
        Ok(sub)
    }

    /// [`Self::subscribe`] of review target `scope` (normalized): the requested page as the
    /// latest state renders it (the kept page, or rendered alone) is compared with what the reader
    /// has, as a subscribe compares a build. Once the stream is registered, the target is brought
    /// to the latest publish, so a state published meanwhile is compared with it too; a removal
    /// between the render and the comparison is repaired the same way.
    fn subscribe_review(
        &self,
        sh: &Shared,
        scope: &SiteTarget,
        page: &str,
        rev: &str,
        label_rev: &str,
    ) -> Result<Subscription, NoStream> {
        let first = {
            let _one = lock(&self.building);
            let mut dropped = false;
            let out = self.review_fresh(sh, scope).and_then(|(fresh, d)| {
                dropped |= d;
                dropped |= self.review_fanout(scope, &fresh)?;
                let (prep, file, d) = self.review_file(scope, &fresh, page)?;
                dropped |= d;
                Ok((PageFacts::of(&prep, file.as_deref()), fresh.key))
            });
            if dropped {
                self.release_free_memory();
            }
            out
        };
        // the render read a state that a removal made old: the stream is compared again below
        let stale = matches!(&first, Err(e) if e.code == E_REDACTED_STATE);
        let hook = lock(&self.after_subscribe_refresh).clone();
        if let Some(hook) = hook {
            hook(scope);
        }
        let mut inner = lock(&self.inner);
        if inner.closed || inner.streams() >= MAX_STREAMS {
            return Err(NoStream::Busy);
        }
        // the store's epoch, sampled under the lock a fan-out takes, as for a build
        let current = sh.state.load().reader().redaction_epoch();
        inner.next_id += 1;
        let id = inner.next_id;
        let (tx, rx) = mpsc::channel(STREAM_QUEUE);
        let s = inner.touch(scope);
        let mut stream = Stream {
            id,
            page: page.to_string(),
            rev: Some(rev.to_string()),
            label_rev: label_rev.to_string(),
            key: None,
            epoch: current,
            checked: false,
            tx,
        };
        if let Ok((facts, key)) = &first
            && facts.epoch == current
            && let Some(event) = stream.follow(facts, *key)
        {
            let _ = stream.tx.try_send(event);
        }
        let epoch = stream.epoch;
        let resolves = stale || first.is_ok();
        s.streams.push(stream);
        drop(inner);
        let sub = Subscription {
            id,
            rx,
            epoch,
            key: StreamKey::Site(scope.clone()),
            inner: Arc::downgrade(&self.inner),
            releases: Arc::clone(&self.memory_releases),
        };
        // a state published since the render may have fanned out before the stream was
        // registered, and a render of an older epoch was compared with nothing: nothing else
        // would compare the stream with the latest state. A target that does not resolve opens
        // with no comparison, and its first state is compared by its next refresh.
        if resolves && sh.state.load().ensure_current_epoch().is_ok() {
            let _ = self.review_refresh(sh, scope);
        }
        Ok(sub)
    }

    /// Compares stream `id` of `scope` with `b` if no build has been compared with it yet (a
    /// build's fan-out compares every registered stream): `Reload` when `b` is of another
    /// epoch, else what [`Stream::against`] finds.
    fn compare(&self, scope: &SiteTarget, id: u64, b: &LiveBuild) {
        let mut inner = lock(&self.inner);
        let Some(stream) = inner
            .scopes
            .get_mut(scope)
            .and_then(|s| s.streams.iter_mut().find(|st| st.id == id))
        else {
            return;
        };
        if stream.checked {
            return;
        }
        let event = if stream.epoch == b.epoch {
            stream.against(b)
        } else {
            Some(LiveEvent::Reload)
        };
        if let Some(event) = event {
            let _ = stream.tx.try_send(event);
        }
        stream.checked = true;
    }

    /// Removes `target`: its build, its key and its memo go, and each open page of it gets
    /// `Reload` before its stream closes. Taken under the one-build lock, so no build of the
    /// target is publishing meanwhile; nothing is built.
    pub(crate) fn remove(&self, target: &SiteTarget) {
        let target = normalize(target);
        let _one = lock(&self.building);
        let gone = lock(&self.inner).scopes.remove(&target);
        if let Some(s) = gone {
            for stream in s.streams {
                // the sender drops here: the stream ends once its queue is read
                let _ = stream.tx.try_send(LiveEvent::Reload);
            }
        }
    }

    /// The revision an approval page read `st` at: the first 16 hex digits of the hash of the
    /// store's location and redaction epoch. Any publish that moves the store moves it.
    pub fn ui_revision(st: &ReadState) -> String {
        let tuple = (st.seq(), st.redaction_epoch());
        let bytes = serde_json::to_vec(&tuple).unwrap_or_default();
        let mut rev = Hash::of(&bytes).to_string();
        rev.truncate(16);
        rev
    }

    /// Opens a stream for an approval page that read the store at revision `rev`. The stream is
    /// registered first, then compared with the state published now under the same lock: a publish
    /// after the comparison finds it registered, and a mismatch queues `Reload` at once. Past
    /// [`MAX_STREAMS`] open streams, and after `close`, the site is busy; with `serve.ui` off since
    /// the request's check, it is the ordinary answer to an event request.
    pub fn subscribe_ui(&self, sh: &Shared, rev: &str) -> Result<Subscription, Refusal> {
        let (tx, rx) = mpsc::channel(STREAM_QUEUE);
        let (id, epoch) = {
            let mut inner = lock(&self.inner);
            if inner.closed || inner.streams() >= MAX_STREAMS {
                return Err(Refusal::Busy);
            }
            let epoch = sh.state.load().reader().redaction_epoch();
            inner.next_id += 1;
            let id = inner.next_id;
            inner.ui_streams.push(UiStream {
                id,
                seen: (rev.to_string(), sh.pack_gen.load(Ordering::SeqCst)),
                tx: tx.clone(),
            });
            (id, epoch)
        };
        // dropped on the way out of an error, which removes the stream
        let sub = Subscription {
            id,
            rx,
            epoch,
            key: StreamKey::Ui,
            inner: Arc::downgrade(&self.inner),
            releases: Arc::clone(&self.memory_releases),
        };
        let hook = lock(&self.before_ui_compare).clone();
        if let Some(hook) = hook {
            hook();
        }
        // a refresh that turned the UI off found the stream, or comes after this read
        if !serve_config(&sh.root).ui {
            return Err(Refusal::Malformed);
        }
        let mut inner = lock(&self.inner);
        let now = LiveSite::ui_revision(&sh.state.load());
        if now != rev {
            let _ = tx.try_send(LiveEvent::Reload);
        }
        if let Some(stream) = inner.ui_streams.iter_mut().find(|s| s.id == id) {
            stream.seen.0 = now;
        }
        drop(inner);
        Ok(sub)
    }

    /// Test hook: from now on, each UI subscribe runs `f` after its stream is registered and
    /// before its last comparison, holding no lock (`None` removes it).
    #[doc(hidden)]
    pub fn set_before_ui_compare(&self, f: Option<BeforeUiCompare>) {
        *lock(&self.before_ui_compare) = f.map(Arc::from);
    }

    /// A publish: one `Reload` for each UI stream whose page has not heard of the store or the
    /// config as they stand now; a full or closed queue drops its stream, as for a site page. With
    /// `serve.ui` off, each stream closes after it. No page is rendered here.
    fn published_to_ui(&self, sh: &Shared) {
        if lock(&self.inner).ui_streams.is_empty() {
            return;
        }
        if !serve_config(&sh.root).ui {
            return self.close_ui();
        }
        // the generation before the state, as for a build: a newer one is never lost
        let generation = sh.pack_gen.load(Ordering::SeqCst);
        let now = (LiveSite::ui_revision(&sh.state.load()), generation);
        lock(&self.inner).ui_streams.retain_mut(|s| {
            if s.seen == now {
                return true;
            }
            s.seen.clone_from(&now);
            s.tx.try_send(LiveEvent::Reload).is_ok()
        });
    }

    /// `serve.ui` is off: each UI stream gets `Reload`, and then ends when its queue is read.
    pub(crate) fn close_ui(&self) {
        let gone = std::mem::take(&mut lock(&self.inner).ui_streams);
        for stream in gone {
            let _ = stream.tx.try_send(LiveEvent::Reload);
        }
    }

    /// The open streams, across every target.
    pub fn streams(&self) -> usize {
        lock(&self.inner).streams()
    }

    /// The targets the site keeps state for: a build, an open stream or both.
    pub fn scopes(&self) -> usize {
        lock(&self.inner).scopes.len()
    }

    /// The builds done so far.
    pub fn builds(&self) -> u64 {
        self.builds.load(Ordering::SeqCst)
    }

    /// Test hook: the times the site gave the allocator's free memory back.
    #[doc(hidden)]
    pub fn memory_releases(&self) -> u64 {
        self.memory_releases.load(Ordering::SeqCst)
    }

    /// Stops the worker and drops every stream's sender, so each open page's stream ends.
    pub fn close(&self) {
        {
            let mut inner = lock(&self.inner);
            inner.closed = true;
            for s in inner.scopes.values_mut() {
                s.streams.clear();
            }
            inner.ui_streams.clear();
        }
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.get() {
            worker.unpark();
        }
    }
}

/// Starts the site worker (`rdy-site`): after each publish it refreshes the scopes that have an
/// open page ([`LiveSite::step`]). It ends on [`LiveSite::close`] or when no `Shared` lives, so
/// start it once `shared` is built (after `Arc::new_cyclic` returns, never inside it).
pub fn spawn_site_worker(shared: Weak<Shared>, site: Arc<LiveSite>) -> JoinHandle<()> {
    let worker = Arc::clone(&site);
    let handle = std::thread::Builder::new()
        .name("rdy-site".into())
        .spawn(move || {
            let site = worker;
            let mut last = 0;
            loop {
                if site.stopped.load(Ordering::SeqCst) || shared.strong_count() == 0 {
                    return;
                }
                // the count before the park: a publish after this load unparks the park below
                let now = site.published.load(Ordering::SeqCst);
                if now == last {
                    std::thread::park_timeout(WORKER_TICK);
                    continue;
                }
                last = now;
                let Some(sh) = shared.upgrade() else { return };
                if let Err(p) = catch_unwind(AssertUnwindSafe(|| site.step(&sh))) {
                    log(&format!("site: a build panicked: {}", panic_text(&*p)));
                }
            }
        })
        .expect("the rdy-site thread could not be started");
    let _ = site.worker.set(handle.thread().clone());
    handle
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_engine::{Rdy, ReloadOutcome, Session, WriteRequest};
    use rdy_model::Role;
    use serde_json::{Value, json};

    const C: SiteTarget = SiteTarget::Source(ScopeRef::Canon);

    /// Writes `ops` into `view` as a writer, then lands the view as an approver.
    fn write_and_land(r: &mut Rdy, view: &str, ops: Value) {
        let ops: Vec<Value> = serde_json::from_value(ops).unwrap();
        let writer = Session::human("human:qa".into(), Role::Writer).unwrap();
        r.write(
            &writer,
            WriteRequest {
                view: view.into(),
                ops: rdy_engine::parse_ops(ops).unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
        let p = r
            .propose(&writer, view, view, Some("test summary"))
            .unwrap();
        r.land(
            &Session::human("human:qa".into(), Role::Approver).unwrap(),
            &p.id,
        )
        .unwrap();
    }

    fn one_doc(name: &str) -> Value {
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": name, "title": name, "doc_type": "overview"}},
            {"op": "create", "type": "section", "parent": "$d", "fields": {"title": "Part"}}
        ])
    }

    /// Fix round 1, finding 1: the worker starts once its `Shared` is built. Started inside
    /// `Arc::new_cyclic`, it could look before the `Shared` existed, see none, and end, so no
    /// publish built anything again. Every daemon's worker builds on its own after a write.
    #[test]
    fn ls5_every_daemons_worker_lives_from_the_start() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        for i in 0..20 {
            let dir = tempfile::tempdir().unwrap();
            let sh = Shared::new(Rdy::init(dir.path(), "rdy-standard").unwrap());
            let b = sh.site.refresh(&sh, &C).unwrap();
            let mut home = sh
                .site
                .subscribe(
                    &sh,
                    &C,
                    "index.html",
                    &b.live.revs["index.html"],
                    &b.live.label_rev,
                )
                .unwrap();
            sh.writer
                .run(Box::new(|r: &mut Rdy| {
                    write_and_land(r, "work", one_doc("alpha"));
                    Ok(Value::Null)
                }))
                .unwrap();
            // no refresh here: only the worker builds
            let got = rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(10), home.rx.recv()).await
            });
            assert_eq!(
                got,
                Ok(Some(LiveEvent::Reload)),
                "daemon {i}: the worker built nothing"
            );
            drop(home);
            sh.shutdown().unwrap();
        }
    }

    /// A build gives the allocator's free memory back once, after it publishes; a refresh that
    /// finds its build in the cache builds nothing and releases nothing.
    #[test]
    fn site_build_releases_free_memory_and_a_cache_hit_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let sh = Shared::new(Rdy::init(dir.path(), "rdy-standard").unwrap());
        let site = &sh.site;
        assert_eq!(site.memory_releases(), 0);
        let (_, built) = site.refresh_counted(&sh, &C).unwrap();
        assert!(built);
        assert_eq!((site.builds(), site.memory_releases()), (1, 1));
        let (_, built) = site.refresh_counted(&sh, &C).unwrap();
        assert!(!built, "the second refresh is a cache hit");
        assert_eq!((site.builds(), site.memory_releases()), (1, 1));
        sh.writer
            .run(Box::new(|r: &mut Rdy| {
                write_and_land(r, "work", one_doc("alpha"));
                Ok(Value::Null)
            }))
            .unwrap();
        let (_, built) = site.refresh_counted(&sh, &C).unwrap();
        assert!(built, "a landing builds canon again");
        assert_eq!((site.builds(), site.memory_releases()), (2, 2));
        sh.shutdown().unwrap();
    }

    /// Fix round 1, finding 2: a refresh that runs while the writer reloads the pack, after the
    /// pack moved and before the state that holds it is published, must not leave a build of the
    /// old pack under the new pack's key.
    #[test]
    fn ls5_a_refresh_inside_a_pack_reload_never_keeps_the_old_packs_pages() {
        let dir = tempfile::tempdir().unwrap();
        drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
        let pack = dir.path().join(".rdy/packs/p.yaml");
        let listed = "name: p\nversion: '1'\nextends: rdy-standard\ndoc_types:\n  record: { skeleton: [] }\n";
        let hidden = "name: p\nversion: '2'\nextends: rdy-standard\ndoc_types:\n  record: { nav: false, skeleton: [] }\n";
        std::fs::write(&pack, listed).unwrap();
        std::fs::write(dir.path().join(".rdy.yaml"), "pack: p\nname: Records\n").unwrap();
        let mut r = Rdy::open(dir.path()).unwrap();
        write_and_land(
            &mut r,
            "work",
            json!([
                {"op": "create", "type": "doc", "id": "$v", "fields": {"name": "visible", "title": "Visible", "doc_type": "overview"}},
                {"op": "create", "type": "section", "parent": "$v", "fields": {"title": "Part"}},
                {"op": "create", "type": "doc", "id": "$r", "fields": {"name": "rec", "title": "Record", "doc_type": "record"}},
                {"op": "create", "type": "section", "parent": "$r", "fields": {"title": "Facts"}}
            ]),
        );
        let sh = Shared::new(r);
        let before = sh.site.refresh(&sh, &C).unwrap();
        // a publish that changes nothing canon reads: canon's build is now behind the count
        sh.writer
            .run(Box::new(|_: &mut Rdy| Ok(Value::Null)))
            .unwrap();
        std::fs::write(&pack, hidden).unwrap();
        // a refresh inside the reload, right before the writer stores the reloaded state
        let weak = Arc::downgrade(&sh);
        *sh.before_reload_store.lock().unwrap() = Some(Box::new(move || {
            let sh = weak.upgrade().unwrap();
            sh.site.refresh(&sh, &C).unwrap();
        }));
        let out = sh.writer.reload().unwrap();
        assert!(matches!(out, ReloadOutcome::Reloaded), "{out:?}");
        let served = sh.site.refresh(&sh, &C).unwrap();
        let fresh = build_live(&sh.state.load_full(), &Scope::Canon, &C).unwrap();
        assert_ne!(
            before.live.revs, fresh.revs,
            "the pack change moves canon's pages"
        );
        assert_eq!(
            served.live.revs, fresh.revs,
            "the served build is the reloaded pack's"
        );
        sh.shutdown().unwrap();
    }
}
