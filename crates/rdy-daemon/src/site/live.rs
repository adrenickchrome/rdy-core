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
//! The versions page (`/~versions/`, D-2F29G, RULING F5) is one more scope here, with its own key
//! (`rdy_render::menu::menu_key`) and its own build: a view commit rebuilds it and never canon.
//! The number of docs each view or proposal version changes is cached by what it reads, so one
//! view commit recounts one view.
use crate::daemon::log;
use crate::shared::Shared;
use crate::site::guard::ScopeRef;
use crate::writer::panic_text;
use rdy_engine::{RdyError, ReadState, Scope};
use rdy_model::Hash;
use rdy_render::live::{LiveBuild, build_live, diff, inputs_key};
use rdy_render::menu::{build_versions, docs_changed, menu_key};
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
/// The scopes that keep a build while no page of theirs is open. A scope with an open page
/// keeps its build as long as the page is open (the next build is diffed against it), so at most
/// [`MAX_STREAMS`] builds stay, one per open page at worst: [`MAX_STREAMS`] bounds the memory.
pub const MAX_SCOPES: usize = 8;

/// How often the worker looks whether any `Shared` still lives while no publish wakes it. A
/// wake lost while the worker starts waits at most this long.
const WORKER_TICK: Duration = Duration::from_secs(1);

/// The doc counts the site keeps for the versions page; past this, the cache clears.
pub const MAX_COUNTS: usize = 256;

/// What counts the docs a view or proposal version changes: `rdy_render::menu::docs_changed`, or
/// a test's hook ([`LiveSite::set_docs_changed`]).
pub type DocsChanged = Box<dyn Fn(&ReadState, &Scope) -> Result<usize, RdyError> + Send + Sync>;

/// What a stream sends an open page: the closed set of the site's events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveEvent {
    /// The page's bytes changed, or the page is gone: load it again.
    Reload,
    /// Only the version label or the header changed: the new label text (the colophon) and the
    /// new header markup (the pill and its chips).
    Label { text: String, html: String },
}

/// The site has as many open streams as it keeps ([`MAX_STREAMS`]).
#[derive(Debug)]
pub struct SiteBusy;

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

    /// The state of `scope`, marked as used now. Only a resolved scope with a build, or a scope
    /// with an open stream, gets one: a request names any scope it likes.
    fn touch(&mut self, scope: &ScopeRef) -> &mut ScopeState {
        self.clock += 1;
        let now = self.clock;
        let s = self.scopes.entry(scope.clone()).or_default();
        s.used = now;
        s
    }

    /// Drops the state of `scope` when it holds neither a build nor a stream.
    fn forget_if_empty(&mut self, scope: &ScopeRef) {
        if self
            .scopes
            .get(scope)
            .is_some_and(|s| s.build.is_none() && s.streams.is_empty())
        {
            self.scopes.remove(scope);
        }
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
        })
    }

    /// Test hook: counts the docs a view or proposal version changes with `f` from now on.
    #[doc(hidden)]
    pub fn set_docs_changed(&self, f: DocsChanged) {
        *lock(&self.docs_changed) = Some(f);
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
        // an unknown scope leaves no state: nothing is touched before the scope resolves and
        // builds (or finds its build)
        let cached = |inner: &mut Inner, fresh: &dyn Fn(&ScopeState) -> bool| {
            let hit = inner
                .scopes
                .get(scope)
                .and_then(|s| s.build.as_ref().filter(|_| fresh(s)).map(Arc::clone))?;
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
        // the versions page is not a read scope: it has its own key and its own build
        let (resolved, key) = match scope {
            ScopeRef::Versions => (None, menu_key(&st, generation)),
            _ => {
                let resolved = resolve(&st, scope)?;
                let key = inputs_key(&st, &resolved, generation)?;
                (Some(resolved), key)
            }
        };
        if let Some(b) = cached(&mut lock(&self.inner), &|s| s.key == Some(key)) {
            return Ok((b, false));
        }
        let new = Arc::new(match &resolved {
            Some(resolved) => build_live(&st, resolved, &format!("/{}", scope.path_prefix()))?,
            None => build_versions(&st, &mut |s| self.docs_changed(&st, s, generation))?,
        });
        self.builds.fetch_add(1, Ordering::SeqCst);
        let mut inner = lock(&self.inner);
        let s = inner.touch(scope);
        // the render's warnings, logged once each when a build of the scope first has them
        // (FW-UX1: the person hears of them through the agent's `rdy_site` too)
        let before: &[String] = s.build.as_ref().map_or(&[], |old| &old.warnings);
        for w in new.warnings.iter().filter(|w| !before.contains(w)) {
            log(&format!("site: {}: warning: {w}", scope.text()));
        }
        let change = s.build.as_ref().map(|old| diff(old, &new));
        s.streams.retain_mut(|stream| {
            let event = match &change {
                // the fan-out rule: what changed between the two builds
                Some(change) if stream.checked => {
                    if change.reload.contains(&stream.page) {
                        Some(LiveEvent::Reload)
                    } else {
                        change.label.clone().map(|text| LiveEvent::Label {
                            text,
                            html: new.header.clone(),
                        })
                    }
                }
                // no build to diff, or the stream opened with no comparison: compared with
                // what it opened at, as a subscribe compares it
                _ => stream.against(&new),
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
        let mut stream = Stream {
            id,
            page: page.to_string(),
            rev: rev.to_string(),
            label_rev: label_rev.to_string(),
            checked: false,
            tx,
        };
        // compared with the scope's build as it stands now: a build since the refresh above
        // fanned out without this stream. With no comparison, the scope's next build compares.
        if refreshed && let Some(b) = &s.build {
            if let Some(event) = stream.against(b) {
                let _ = stream.tx.try_send(event);
            }
            stream.checked = true;
        }
        s.streams.push(stream);
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

    /// The scopes the site keeps state for: a build, an open stream or both.
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

    const C: ScopeRef = ScopeRef::Canon;

    /// Writes `ops` into `view` as a writer, then lands the view as an approver.
    fn write_and_land(r: &mut Rdy, view: &str, ops: Value) {
        let ops: Vec<Value> = serde_json::from_value(ops).unwrap();
        let writer = Session::human("human:qa".into(), Role::Writer);
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
        let p = r.propose(&writer, view, view).unwrap();
        r.land(&Session::human("human:qa".into(), Role::Approver), &p.id)
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
                .subscribe(&sh, &C, "index.html", &b.revs["index.html"], &b.label_rev)
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
        let fresh = build_live(&sh.state.load_full(), &Scope::Canon, "/").unwrap();
        assert_ne!(
            before.revs, fresh.revs,
            "the pack change moves canon's pages"
        );
        assert_eq!(
            served.revs, fresh.revs,
            "the served build is the reloaded pack's"
        );
        sh.shutdown().unwrap();
    }
}
