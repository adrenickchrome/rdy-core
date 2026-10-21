//! The profile-only listeners a daemon owns (FW-PB10a, D-DK03K): one port per profile key of
//! canon, whose listener serves that profile's pages at its root and nothing else. "The listener
//! a request arrives on sets the mode, never a path, a Host header or a link."
//!
//! The registry lives on `Shared` and is filled in once at the daemon's start with its runtime,
//! its handle, the project's canonical root and the LAN stand-in. A start runs on the caller's
//! (blocking) thread, under the registry's mutex: it refuses a stopping daemon, resolves the
//! profile in the current canon, then reuses the listener of that folded key or binds a new
//! one. Binding is synchronous (the runtime is entered, never blocked on), and the listener's
//! task is spawned on the daemon's runtime. A failed bind closes what it opened and keeps no
//! entry. At most [`MAX_PROFILE_SITES`] keys are registered; a slot stays until the daemon
//! stops, also while canon no longer holds its key.
//!
//! Each publish wake checks every registered key against one captured canon state. A key canon
//! no longer holds clears its target's cache and closes its open pages' streams (after a
//! `reload`), and its listener answers every path with the plain not found; the warning is
//! logged once per disappearance. The same key coming back serves again at the same URL. On
//! stop, the registry closes, and the listeners' tasks are joined on the daemon's runtime before
//! the store closes or is abandoned.
use crate::daemon::{DaemonHandle, log};
use crate::layout::RunDir;
use crate::shared::Shared;
use crate::site::guard::{Guard, SiteTarget};
use crate::site::server::{self, HttpLimits, SiteServer};
use crate::site::{LanOverride, SiteInfo, addr};
use rdy_engine::engine::{SITE_LAN_FIX, SiteWarning};
use rdy_engine::publication::{IMPLICIT_ORG, ResolvedProfile, profile_at};
use rdy_engine::{E_NOT_FOUND, E_SITE, RdyError, ReadState, Scope, serve_config};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

/// The profile-only sites one daemon serves at most.
pub const MAX_PROFILE_SITES: usize = 32;

/// Whether canon holds a registered key, as a check of one state found it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    /// The key names a profile of canon.
    Held,
    /// No block of canon holds the key (`E_NOT_FOUND`).
    Missing,
    /// The key names a block whose profile metadata does not read (malformed or redacted, or a
    /// block of another type): its pages take the build-error path.
    Broken,
}

/// The registry of one daemon's profile-only listeners, and the HTTP limits every listener of
/// the daemon shares (the full one too).
#[derive(Default)]
pub struct ProfileSites {
    inner: Mutex<Registry>,
    pub(crate) limits: HttpLimits,
}

#[derive(Default)]
struct Registry {
    /// Set once at the daemon's start; taken at its stop.
    owner: Option<Owner>,
    closing: bool,
    /// By folded key (or `implicit:org`); at most [`MAX_PROFILE_SITES`].
    entries: BTreeMap<String, Entry>,
}

/// What a start needs of the daemon that owns the registry.
struct Owner {
    runtime: tokio::runtime::Handle,
    daemon: DaemonHandle,
    /// The project's canonical root.
    root: PathBuf,
    lan: Option<LanOverride>,
}

struct Entry {
    info: SiteInfo,
    /// The listener's task, joined at the daemon's stop.
    task: Option<tokio::task::JoinHandle<()>>,
    /// What the last publish wake found; the warning is logged on the change to `Missing`.
    seen: Availability,
    /// The availability in the last state checked for this key. The weak reference keeps the
    /// state's allocation, so no other state compares equal while it is kept.
    checked: Option<(Weak<ReadState>, Availability)>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `E_SITE` for a start on a daemon that is stopping, or has stopped.
fn stopping() -> RdyError {
    RdyError::new(
        E_SITE,
        "the daemon is stopping",
        "ask for the site's link again after the daemon stops",
    )
}

/// `E_SITE` past [`MAX_PROFILE_SITES`] distinct keys.
fn full() -> RdyError {
    RdyError::new(
        E_SITE,
        format!("this daemon already serves {MAX_PROFILE_SITES} profile-only sites"),
        "run rdy serve --stop, then start the profiles you need",
    )
}

/// `E_SITE` when no candidate port binds on `127.0.0.1`.
fn exhausted(key: &str) -> RdyError {
    RdyError::new(
        E_SITE,
        format!(
            "could not bind a loopback port for profile \"{key}\" after {} attempts",
            addr::TRIES
        ),
        "free a port in 49152..65535, then ask for the profile site again",
    )
}

/// `E_SITE` on a `Shared` whose daemon never set up its site (the base text of
/// `DaemonEngine::site`).
fn not_started() -> RdyError {
    RdyError::new(
        E_SITE,
        "the live site is off: the daemon has not started it",
        "set serve.site_port to a free port from 1024 to 65535 in .rdy/local.yaml, then run rdy serve --stop",
    )
}

/// The key a resolved profile's listener is registered by: its stored key folded, or
/// `implicit:org` for the implicit profile.
fn canonical_key(p: &ResolvedProfile) -> String {
    match &p.key {
        Some(k) => rdy_model::key::fold(k),
        None => IMPLICIT_ORG.to_string(),
    }
}

/// Whether canon in `st` holds `key`, by its metadata only.
fn held_in(st: &ReadState, key: &str) -> Availability {
    match profile_at(st, &Scope::Canon, key) {
        Ok(_) => Availability::Held,
        Err(e) if e.code == E_NOT_FOUND => Availability::Missing,
        Err(_) => Availability::Broken,
    }
}

/// The caveat of every profile-only site, for the profile titled `title`.
pub fn caveat(title: &str) -> SiteWarning {
    SiteWarning {
        message: format!(
            "this site shows only the profile \"{title}\"; anyone with the project still reads everything"
        ),
        fix: "give the full site's link from rdy site when the person needs everything".to_string(),
    }
}

/// The LAN warning of `info` (a profile-only listener's original diagnostic) for the profile
/// titled `title`: a bound LAN address names the profile, and a failed setup prefixes its
/// message with the profile. `None` without a LAN warning.
pub fn profile_lan_warning(info: &SiteInfo, title: &str) -> Option<SiteWarning> {
    let w = info.warning.as_ref()?;
    Some(match &info.lan_url {
        Some(url) => SiteWarning {
            message: format!("anyone on your network can read the profile \"{title}\" at {url}"),
            fix: SITE_LAN_FIX.to_string(),
        },
        None => SiteWarning {
            message: format!("profile \"{title}\": {}", w.message),
            fix: w.fix.clone(),
        },
    })
}

/// The lines a new profile-only listener logs: its address, its LAN warning in the base LAN
/// log's form, then the caveat.
fn open_lines(key: &str, info: &SiteInfo, title: &str) -> Vec<String> {
    let mut lines = vec![format!(
        "site: profile {key}: {}",
        info.url.as_deref().unwrap_or_default()
    )];
    if let Some(w) = profile_lan_warning(info, title) {
        lines.push(match info.lan_url {
            Some(_) => format!("site: warning: {}; fix: {}", w.message, w.fix),
            None => format!("site: {}; fix: {}", w.message, w.fix),
        });
    }
    let c = caveat(title);
    lines.push(format!("site: warning: {}; fix: {}", c.message, c.fix));
    lines
}

/// The warning of a key canon no longer holds.
fn removed_line(key: &str) -> String {
    format!(
        "site: warning: canon no longer holds the profile \"{key}\"; its profile-only site serves no page; fix: land the profile again, or run rdy serve --stop to end the site"
    )
}

impl ProfileSites {
    /// Fills in the daemon that owns the registry; called once at its start, after the full
    /// site binds.
    pub(crate) fn init(
        &self,
        runtime: tokio::runtime::Handle,
        daemon: DaemonHandle,
        root: PathBuf,
        lan: Option<LanOverride>,
    ) {
        let mut reg = lock(&self.inner);
        if !reg.closing {
            reg.owner = Some(Owner {
                runtime,
                daemon,
                root,
                lan,
            });
        }
    }

    /// The registered keys, in order.
    pub fn keys(&self) -> Vec<String> {
        lock(&self.inner).entries.keys().cloned().collect()
    }

    /// The listener of profile `name` (an id, a key in any case, or `implicit:org`) of canon:
    /// its folded key and its info. See [`ensure_profile_site`].
    pub(crate) fn ensure(
        &self,
        sh: &Arc<Shared>,
        name: &str,
    ) -> Result<(String, SiteInfo), RdyError> {
        let mut reg = lock(&self.inner);
        if reg.closing {
            return Err(stopping());
        }
        let Some(owner) = &reg.owner else {
            return Err(not_started());
        };
        if *owner.daemon.stopped().borrow() {
            return Err(stopping());
        }
        let (runtime, daemon, root, lan) = (
            owner.runtime.clone(),
            owner.daemon.clone(),
            owner.root.clone(),
            owner.lan,
        );
        // the current canon, before any socket or slot: a missing or stale key fails here
        let st = sh.state.load_full();
        let profile = profile_at(&st, &Scope::Canon, name)?;
        let key = canonical_key(&profile);
        if let Some(e) = reg.entries.get(&key) {
            return Ok((key, e.info.clone()));
        }
        if reg.entries.len() >= MAX_PROFILE_SITES {
            return Err(full());
        }
        let opened = {
            let _rt = runtime.enter();
            let cfg = serve_config(&root);
            server::open(&root, &cfg, &addr::profile_candidates(&root, &key), lan)
        };
        let Ok(o) = opened else {
            return Err(exhausted(&key));
        };
        let info = o.info;
        let server = SiteServer::new(o.listeners, Guard::profile_only(o.port, o.lan, key.clone()));
        let ended = root.clone();
        let run = server.run_weak(Arc::downgrade(sh), daemon);
        let task = runtime.spawn(async move {
            run.await;
            report(&ended, TestEvent::Stage("profile-listener-ended"));
        });
        reg.entries.insert(
            key.clone(),
            Entry {
                info: info.clone(),
                task: Some(task),
                seen: Availability::Held,
                checked: Some((Arc::downgrade(&st), Availability::Held)),
            },
        );
        drop(reg);
        for line in open_lines(&key, &info, &profile.title) {
            say(&root, &line);
        }
        Ok((key, info))
    }

    /// Whether canon in `st` holds the registered `key` (a request's check, after its epoch
    /// check): read once per published state, then remembered for it.
    pub(crate) fn availability(&self, st: &Arc<ReadState>, key: &str) -> Availability {
        {
            let reg = lock(&self.inner);
            if let Some((w, a)) = reg.entries.get(key).and_then(|e| e.checked.as_ref())
                && std::ptr::eq(w.as_ptr(), Arc::as_ptr(st))
            {
                return *a;
            }
        }
        let a = held_in(st, key);
        if let Some(e) = lock(&self.inner).entries.get_mut(key) {
            e.checked = Some((Arc::downgrade(st), a));
        }
        a
    }

    /// A publish wake: checks every registered key against one captured canon state, with or
    /// without open pages. A key canon does not hold has its target's cache and its open pages'
    /// streams cleared; a key that became missing is logged once. Nothing is built.
    pub(crate) fn inspect(&self, sh: &Shared) {
        let (keys, root) = {
            let reg = lock(&self.inner);
            if reg.entries.is_empty() {
                return;
            }
            let root = reg.owner.as_ref().map(|o| o.root.clone());
            (reg.entries.keys().cloned().collect::<Vec<_>>(), root)
        };
        let st = sh.state.load_full();
        // a state older than a removal is no answer: the fresh state's publish wakes again
        if st.ensure_current_epoch().is_err() {
            return;
        }
        let (mut cleared, mut lost) = (Vec::new(), Vec::new());
        for key in keys {
            let a = held_in(&st, &key);
            let before = {
                let mut reg = lock(&self.inner);
                let Some(e) = reg.entries.get_mut(&key) else {
                    continue;
                };
                e.checked = Some((Arc::downgrade(&st), a));
                std::mem::replace(&mut e.seen, a)
            };
            // cleared on every wake that finds it not held, not only on the change: a stream
            // or build of a state the last wake never inspected (a restore and a removal
            // between two wakes) goes too; clearing a target the site keeps nothing of is a
            // no-op
            if a != Availability::Held {
                cleared.push(key.clone());
            }
            if a == Availability::Missing && before != Availability::Missing {
                lost.push(key);
            }
        }
        // the registry's mutex is released: the site's builder lock comes next
        for key in cleared {
            sh.site.remove(&SiteTarget::ProfileOnly { key });
        }
        if let Some(root) = root {
            for key in lost {
                say(&root, &removed_line(&key));
            }
        }
    }

    /// The daemon stops: no start is taken from now on, every slot is released, and the
    /// listeners' tasks are handed back to be joined. Each listener ends as the daemon's stop
    /// reaches its accept loops.
    pub(crate) fn close(&self) -> Vec<tokio::task::JoinHandle<()>> {
        let mut reg = lock(&self.inner);
        reg.closing = true;
        reg.owner = None;
        let tasks = reg
            .entries
            .values_mut()
            .filter_map(|e| e.task.take())
            .collect();
        reg.entries.clear();
        tasks
    }
}

/// The listener of profile `key` (an id, a key in any case, or `implicit:org`) of canon, opened
/// on its first start and reused after (D-DK03K). Runs synchronously on the caller's thread: a
/// stopping daemon, a key canon does not hold, a 33rd distinct key and a key none of whose 16
/// candidate ports binds are refused before anything is kept. The info's `warning` is the
/// listener's original LAN diagnostic.
pub fn ensure_profile_site(sh: Arc<Shared>, key: &str) -> Result<SiteInfo, RdyError> {
    sh.profile_sites.ensure(&sh, key).map(|(_, info)| info)
}

// --- test hooks -----------------------------------------------------------------------------------

/// What a daemon tells a test about the project it serves ([`set_test_hooks`]).
#[derive(Debug)]
pub enum TestEvent<'a> {
    /// An HTTP listening socket is about to bind this address.
    Bind(SocketAddr),
    /// A line the daemon logs about its site or its start.
    Log(&'a str),
    /// A step of its stop: `profile-listener-ended` (once per profile-only listener, when its
    /// task ends), `profiles-joined`, then `store-closed` or `store-abandoned`.
    Stage(&'static str),
}

/// A test's sink for [`TestEvent`]s.
pub type TestHook = Box<dyn Fn(TestEvent<'_>) + Send + Sync>;

type Hooks = BTreeMap<PathBuf, Arc<TestHook>>;

static HOOKS: Mutex<Hooks> = Mutex::new(BTreeMap::new());

/// Test hook: from now on, every daemon of the project at `project` (any spelling of it) tells
/// `hook` what it binds, logs and does at its stop (`None` removes it).
#[doc(hidden)]
pub fn set_test_hooks(project: &Path, hook: Option<TestHook>) {
    let root = RunDir::of_project(project).root();
    let mut hooks = lock(&HOOKS);
    match hook {
        Some(f) => {
            hooks.insert(root, Arc::new(f));
        }
        None => {
            hooks.remove(&root);
        }
    }
}

/// Tells the hook of the project at `root` (canonical) of `event`, if a test set one.
pub(crate) fn report(root: &Path, event: TestEvent<'_>) {
    let hook = {
        let hooks = lock(&HOOKS);
        if hooks.is_empty() {
            return;
        }
        hooks.get(root).cloned()
    };
    if let Some(f) = hook {
        f(event);
    }
}

/// Logs `text` for the project at `root`, and tells its hook.
pub(crate) fn say(root: &Path, text: &str) {
    log(text);
    report(root, TestEvent::Log(text));
}
