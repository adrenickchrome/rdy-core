//! The live state of the site (FW-LS1, D-N5NSN; FW-LS1a, D-FV6G9): the last build of each
//! served scope, the open pages of each scope (their event streams), and the worker that builds
//! a scope again after a publish while a page of it is open.
//!
//! The writer's only work per publish is [`LiveSite::published`]: one atomic add and one wake.
//! Every build runs on the `rdy-site` worker or on a request's thread, one at a time. A build
//! reads the published state, not the event list: a tag logs no event and a page reads more
//! than its own blocks. The same input key means the same pages, so a publish that touches
//! nothing a scope reads builds nothing for it. A scope with no open page builds nothing until a
//! request asks for it (pages build on demand).
//!
//! After a build, each open page of the scope gets `Reload` when its bytes changed (or it is
//! gone), else `Label` when only the version label changed, else nothing; the pages of other
//! scopes get nothing (the founder addition of 2026-10-12). An event is best effort: a full or
//! closed queue drops the stream, and the page's revision repairs it when it reconnects.
use crate::daemon::log;
use crate::shared::Shared;
use crate::site::guard::ScopeRef;
use crate::writer::panic_text;
use rdy_engine::{RdyError, ReadState, Scope};
use rdy_model::Hash;
use rdy_render::live::{LiveBuild, build_live, diff, inputs_key};
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::sync::mpsc;

/// The open event streams the site keeps, across every scope.
pub const MAX_STREAMS: usize = 32;
/// The events a stream holds before it is dropped.
pub const STREAM_QUEUE: usize = 8;
/// The scopes that keep a build while no page of theirs is open.
pub const MAX_SCOPES: usize = 8;

/// How often the worker looks whether any `Shared` still lives while no publish wakes it.
const WORKER_TICK: Duration = Duration::from_secs(1);

/// What a stream sends an open page: the closed set of the site's events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveEvent {
    /// The page's bytes changed, or the page is gone: load it again.
    Reload,
    /// Only the version label changed: this is its new text.
    Label(String),
}

/// The site has as many open streams as it keeps ([`MAX_STREAMS`]).
#[derive(Debug)]
pub struct SiteBusy;

/// The scope `scope` names, read from `st`. A view must exist; `P-x` reads as the proposal's
/// latest version each time.
pub fn resolve(st: &ReadState, scope: &ScopeRef) -> Result<Scope, RdyError> {
    match scope {
        ScopeRef::Canon => Ok(Scope::Canon),
        ScopeRef::View(name) => st.resolve_read_scope(&format!("view:{name}"), None),
        ScopeRef::Proposal(r) => st.resolve_scope(&format!("proposal:{r}")),
    }
}

/// One open page: its stream's sender and what it shows.
struct Stream {
    id: u64,
    page: String,
    tx: mpsc::Sender<LiveEvent>,
}

/// One served scope: its last build and the open pages of it.
#[derive(Default)]
struct ScopeState {
    build: Option<Arc<LiveBuild>>,
    /// The input key of `build`.
    key: Option<Hash>,
    /// The publish count the build (or a check that found the same key) read.
    seen: u64,
    /// When the scope was last used, on `Inner::clock`.
    used: u64,
    /// A build of the scope failed on the worker and the failure was logged; cleared by a success.
    failing: bool,
    streams: Vec<Stream>,
}

#[derive(Default)]
struct Inner {
    scopes: BTreeMap<ScopeRef, ScopeState>,
    clock: u64,
    next_id: u64,
    closed: bool,
}

impl Inner {
    fn streams(&self) -> usize {
        self.scopes.values().map(|s| s.streams.len()).sum()
    }

    /// The state of `scope`, marked as used now.
    fn touch(&mut self, scope: &ScopeRef) -> &mut ScopeState {
        self.clock += 1;
        let now = self.clock;
        let s = self.scopes.entry(scope.clone()).or_default();
        s.used = now;
        s
    }

    /// Drops scopes past [`MAX_SCOPES`] builds, least recently used first, among those with no
    /// open stream; a scope with neither a build nor a stream goes too.
    fn evict(&mut self) {
        self.scopes
            .retain(|_, s| s.build.is_some() || !s.streams.is_empty());
        while self.scopes.values().filter(|s| s.build.is_some()).count() > MAX_SCOPES {
            let Some(oldest) = self
                .scopes
                .iter()
                .filter(|(_, s)| s.streams.is_empty())
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
    /// The worker's wake: it waits on `wake` under `wake_lock`.
    wake_lock: Mutex<()>,
    wake: Condvar,
    stopped: AtomicBool,
    /// Held for each build and its fan-out: one build at a time.
    building: Mutex<()>,
    /// Shared with each [`Subscription`], which removes its stream when dropped.
    inner: Arc<Mutex<Inner>>,
    builds: AtomicU64,
}

/// An open page's stream. Dropping it removes the stream.
pub struct Subscription {
    pub id: u64,
    pub rx: mpsc::Receiver<LiveEvent>,
    scope: ScopeRef,
    inner: Weak<Mutex<Inner>>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            let mut inner = lock(&inner);
            if let Some(s) = inner.scopes.get_mut(&self.scope) {
                s.streams.retain(|st| st.id != self.id);
            }
        }
    }
}

impl LiveSite {
    pub fn new() -> Arc<LiveSite> {
        Arc::new(LiveSite {
            published: AtomicU64::new(0),
            wake_lock: Mutex::new(()),
            wake: Condvar::new(),
            stopped: AtomicBool::new(false),
            building: Mutex::new(()),
            inner: Arc::new(Mutex::new(Inner::default())),
            builds: AtomicU64::new(0),
        })
    }

    /// A publish happened: counts it and wakes the worker. Called by the writer after each
    /// store into `Shared::state`; it never builds and never waits on a build.
    pub fn published(&self) {
        self.published.fetch_add(1, Ordering::SeqCst);
        // the worker holds this lock only between its look at the count and its wait, so the
        // wake cannot fall in between and be lost
        drop(lock(&self.wake_lock));
        self.wake.notify_one();
    }

    /// The build of `scope` as of the latest publish, built when its inputs changed. A new
    /// build sends each open page of the scope what changed for it (see the module docs). A
    /// failed build is the error, and the last build stays.
    pub fn refresh(&self, sh: &Shared, scope: &ScopeRef) -> Result<Arc<LiveBuild>, RdyError> {
        self.refresh_counted(sh, scope).map(|(b, _)| b)
    }

    /// [`Self::refresh`], and whether it built.
    fn refresh_counted(
        &self,
        sh: &Shared,
        scope: &ScopeRef,
    ) -> Result<(Arc<LiveBuild>, bool), RdyError> {
        let _one = lock(&self.building);
        // the count before the state: a publish after this load counts past `now`, so the next
        // refresh reads it
        let now = self.published.load(Ordering::SeqCst);
        {
            let mut inner = lock(&self.inner);
            let s = inner.touch(scope);
            if let Some(b) = &s.build
                && s.seen == now
            {
                return Ok((Arc::clone(b), false));
            }
        }
        let st = sh.state.load_full();
        let generation = sh.pack_gen.load(Ordering::SeqCst);
        let resolved = resolve(&st, scope)?;
        let key = inputs_key(&st, &resolved, generation)?;
        {
            let mut inner = lock(&self.inner);
            let s = inner.touch(scope);
            if let Some(b) = &s.build
                && s.key == Some(key)
            {
                s.seen = now;
                return Ok((Arc::clone(b), false));
            }
        }
        let new = Arc::new(build_live(&st, &resolved)?);
        self.builds.fetch_add(1, Ordering::SeqCst);
        let mut inner = lock(&self.inner);
        let s = inner.touch(scope);
        if let Some(old) = &s.build {
            let change = diff(old, &new);
            s.streams.retain(|stream| {
                let event = if change.reload.contains(&stream.page) {
                    LiveEvent::Reload
                } else if let Some(text) = &change.label {
                    LiveEvent::Label(text.clone())
                } else {
                    return true;
                };
                // a full or closed queue drops the stream: its page reconnects with its revision
                stream.tx.try_send(event).is_ok()
            });
        }
        s.build = Some(Arc::clone(&new));
        s.key = Some(key);
        s.seen = now;
        inner.evict();
        Ok((new, true))
    }

    /// One wake of the worker: refreshes each scope with an open stream, and returns how many
    /// it built. A failure is logged once per run of failures of a scope.
    pub fn step(&self, sh: &Shared) -> usize {
        let open: Vec<ScopeRef> = lock(&self.inner)
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
                Err(e) => {
                    if !s.failing {
                        s.failing = true;
                        log(&format!(
                            "site: the {} pages did not build ({e}); open pages keep what they show until a build succeeds",
                            scope.text()
                        ));
                    }
                }
            }
        }
        built
    }

    /// Opens a stream for `page` of `scope`, which the reader has at revision `rev` with the
    /// label at `label_rev`. The scope is refreshed first. A page the build does not have, or
    /// has at another revision, gets `Reload` at once; else a page with another label gets the
    /// label. When the scope does not build, the stream opens with no event. Past
    /// [`MAX_STREAMS`] open streams, and after `close`, the site is busy.
    pub fn subscribe(
        &self,
        sh: &Shared,
        scope: &ScopeRef,
        page: &str,
        rev: &str,
        label_rev: &str,
    ) -> Result<Subscription, SiteBusy> {
        {
            let inner = lock(&self.inner);
            if inner.closed || inner.streams() >= MAX_STREAMS {
                return Err(SiteBusy);
            }
        }
        let refreshed = self.refresh(sh, scope).is_ok();
        let mut inner = lock(&self.inner);
        if inner.closed || inner.streams() >= MAX_STREAMS {
            return Err(SiteBusy);
        }
        inner.next_id += 1;
        let id = inner.next_id;
        let (tx, rx) = mpsc::channel(STREAM_QUEUE);
        let s = inner.touch(scope);
        // compared with the scope's build as it stands now: a build since the refresh above
        // fanned out without this stream
        if refreshed && let Some(b) = &s.build {
            if b.revs.get(page).map(String::as_str) != Some(rev) {
                let _ = tx.try_send(LiveEvent::Reload);
            } else if b.label_rev != label_rev {
                let _ = tx.try_send(LiveEvent::Label(b.label.clone()));
            }
        }
        s.streams.push(Stream {
            id,
            page: page.to_string(),
            tx,
        });
        Ok(Subscription {
            id,
            rx,
            scope: scope.clone(),
            inner: Arc::downgrade(&self.inner),
        })
    }

    /// The open streams, across every scope.
    pub fn streams(&self) -> usize {
        lock(&self.inner).streams()
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
        drop(lock(&self.wake_lock));
        self.wake.notify_all();
    }
}

/// Starts the site worker (`rdy-site`): after each publish it refreshes the scopes that have an
/// open page ([`LiveSite::step`]). It ends on [`LiveSite::close`] or when no `Shared` lives.
pub fn spawn_site_worker(shared: Weak<Shared>, site: Arc<LiveSite>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("rdy-site".into())
        .spawn(move || {
            let mut last = 0;
            loop {
                {
                    let mut g = lock(&site.wake_lock);
                    loop {
                        if site.stopped.load(Ordering::SeqCst) || shared.strong_count() == 0 {
                            return;
                        }
                        let now = site.published.load(Ordering::SeqCst);
                        if now != last {
                            last = now;
                            break;
                        }
                        g = site
                            .wake
                            .wait_timeout(g, WORKER_TICK)
                            .unwrap_or_else(|e| e.into_inner())
                            .0;
                    }
                }
                let Some(sh) = shared.upgrade() else { return };
                if let Err(p) = catch_unwind(AssertUnwindSafe(|| site.step(&sh))) {
                    log(&format!("site: a build panicked: {}", panic_text(&*p)));
                }
            }
        })
        .expect("the rdy-site thread could not be started")
}
