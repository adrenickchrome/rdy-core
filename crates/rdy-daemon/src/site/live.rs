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
use crate::daemon::log;
use crate::shared::Shared;
use crate::site::guard::{ScopeRef, SiteTarget};
use crate::site::profiles::Availability;
use crate::writer::panic_text;
use rdy_engine::publication::ResolvedProfile;
use rdy_engine::site_scope::{CHANGES_PAGE, SINCE_PAGE, summary_page_taken};
use rdy_engine::{E_REDACTED_STATE, RdyError, ReadState, Scope};
use rdy_model::Hash;
use rdy_render::live::{
    LiveBuild, ProfileHistoryMemo, build_live, build_profile_live, diff, inputs_key,
};
use rdy_render::menu::{build_versions_for, docs_changed, menu_key_for};
use rdy_render::review;
use std::collections::BTreeMap;
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
/// keeps its build as long as the page is open (the next build is diffed against it), outside
/// this count, so at most [`MAX_STREAMS`] such builds stay beside the inactive ones.
/// Each profile target's slot also keeps its history memo, so at most `MAX_STREAMS + MAX_SCOPES`
/// memos stay, each of at most `history_facts::MAX_LANDINGS` results.
pub const MAX_SCOPES: usize = 8;

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

/// One open page: its stream's sender and what it shows.
struct Stream {
    id: u64,
    page: String,
    /// The page's revision and the label's when the stream opened.
    rev: String,
    label_rev: String,
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
        if b.revs.get(&self.page) != Some(&self.rev) {
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
}

/// One served target: its last build and the open pages of it.
#[derive(Default)]
struct ScopeState {
    build: Option<Arc<SiteBuild>>,
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
}

#[derive(Default)]
struct Inner {
    scopes: BTreeMap<SiteTarget, ScopeState>,
    clock: u64,
    next_id: u64,
    closed: bool,
}

impl Inner {
    fn streams(&self) -> usize {
        self.scopes.values().map(|s| s.streams.len()).sum()
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

    /// Drops the state of `scope` when it holds neither a build nor a stream.
    fn forget_if_empty(&mut self, scope: &SiteTarget) {
        if self
            .scopes
            .get(scope)
            .is_some_and(|s| s.build.is_none() && s.streams.is_empty())
        {
            self.scopes.remove(scope);
        }
    }

    /// Drops inactive targets (a build and no open stream) past [`MAX_SCOPES`], least recently
    /// used first, with their memos; a target with neither a build nor a stream goes too. A
    /// target with an open stream keeps its build outside that count ([`MAX_STREAMS`] bounds
    /// those), so open pages never push an inactive build out of its slot.
    fn evict(&mut self) {
        self.scopes
            .retain(|_, s| s.build.is_some() || !s.streams.is_empty());
        let inactive = |s: &ScopeState| s.build.is_some() && s.streams.is_empty();
        while self.scopes.values().filter(|s| inactive(s)).count() > MAX_SCOPES {
            let Some(oldest) = self
                .scopes
                .iter()
                .filter(|(_, s)| inactive(s))
                .min_by_key(|(_, s)| s.used)
                .map(|(k, _)| k.clone())
            else {
                return;
            };
            self.scopes.remove(&oldest);
        }
    }
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
    builds: AtomicU64,
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
    scope: SiteTarget,
    inner: Weak<Mutex<Inner>>,
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
            if let Some(s) = inner.scopes.get_mut(&self.scope) {
                s.streams.retain(|st| st.id != self.id);
            }
            inner.forget_if_empty(&self.scope);
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
            inner: Arc::new(Mutex::new(Inner::default())),
            builds: AtomicU64::new(0),
            counts: Mutex::new(BTreeMap::new()),
            docs_changed: Mutex::new(None),
            before_publish: Mutex::new(None),
            after_subscribe_refresh: Mutex::new(None),
            log_hook: Mutex::new(None),
            history_ceiling: Mutex::new(None),
        })
    }

    /// Test hook: from now on, `f` also gets each line the site logs (`None` removes it).
    #[doc(hidden)]
    pub fn set_log_hook(&self, f: Option<LogHook>) {
        *lock(&self.log_hook) = f;
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

    /// The build of `target` as of the latest publish, built when its inputs changed. A new
    /// build sends each open page of the target what changed for it (see the module docs). A
    /// failed build is the error, and the last build stays. `Source(Versions)` is the menu of
    /// canon; an invalid target is refused before anything is kept.
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
        let _one = lock(&self.building);
        // the count before the state: a publish after this load counts past `now`, so the next
        // refresh reads it
        let now = self.published.load(Ordering::SeqCst);
        // the store's epoch now: a cached build of another epoch is never served (a removal
        // between a build's check and its insertion leaves it in the cache, unused)
        let current = sh.state.load().reader().redaction_epoch();
        // an unknown target leaves no state: nothing is touched before the target resolves and
        // builds (or finds its build)
        let cached = |inner: &mut Inner, fresh: &dyn Fn(&ScopeState) -> bool| {
            let hit = inner.scopes.get(scope).and_then(|s| {
                s.build
                    .as_ref()
                    .filter(|b| b.live.epoch == current && fresh(s))
                    .map(Arc::clone)
            })?;
            let s = inner.touch(scope);
            s.seen = now;
            Some(hit)
        };
        if let Some(b) = cached(&mut lock(&self.inner), &|s| s.seen == now) {
            return Ok((b, false));
        }
        // the generation before the state: the writer moves it only after it stores the state
        // that holds the move, so a newer generation never pairs with an older state (an older
        // generation with a newer state only builds once more, to the same pages)
        let generation = sh.pack_gen.load(Ordering::SeqCst);
        let st = sh.state.load_full();
        // a state older than a removal builds nothing: the writer publishes the fresh one next
        st.ensure_current_epoch()?;
        let key = (st.redaction_epoch(), target_key(&st, scope, generation)?);
        if let Some(b) = cached(&mut lock(&self.inner), &|s| s.key == Some(key)) {
            return Ok((b, false));
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
        s.build = Some(Arc::clone(&new));
        s.key = Some(key);
        s.seen = now;
        if selected {
            s.memo = memo;
        }
        inner.evict();
        Ok((new, true))
    }

    /// Builds `target` (normalized and valid) from `st`, with `memo` for a profile's history.
    fn build(
        &self,
        st: &ReadState,
        target: &SiteTarget,
        generation: u64,
        memo: &mut ProfileHistoryMemo,
    ) -> Result<SiteBuild, RdyError> {
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

    /// One wake of the worker: refreshes each target with an open stream, and returns how many
    /// it built. A failure is logged once per run of failures of a target.
    pub fn step(&self, sh: &Shared) -> usize {
        // every profile-only key against one captured canon state, open pages or not; a key
        // canon no longer holds has its target removed before anything builds (D-DK03K)
        sh.profile_sites.inspect(sh);
        let open: Vec<SiteTarget> = lock(&self.inner)
            .scopes
            .iter()
            .filter(|(_, s)| !s.streams.is_empty())
            .map(|(k, _)| k.clone())
            .collect();
        let mut built = 0;
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
            rev: rev.to_string(),
            label_rev: label_rev.to_string(),
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
            scope: scope.clone(),
            inner: Arc::downgrade(&self.inner),
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

    /// Stops the worker and drops every stream's sender, so each open page's stream ends.
    pub fn close(&self) {
        {
            let mut inner = lock(&self.inner);
            inner.closed = true;
            for s in inner.scopes.values_mut() {
                s.streams.clear();
            }
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
