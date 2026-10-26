//! What every connection of the daemon shares: the published state, the writer, the open report,
//! the pack watch and its generation, the hub of connected MCP sessions and its notifier, and
//! the session-end hooks.
use crate::notify::{NotifyMsg, spawn_notifier};
use crate::session::SessionId;
use crate::site::SiteInfo;
use crate::site::live::{LiveSite, spawn_site_worker};
use crate::watch::PackWatch;
use crate::writer::{Writer, WriterHandle};
use arc_swap::ArcSwap;
use rdy_engine::engine::{
    Engine, ExportRequest, ReadFn, RenderRequest, SiteFacts, SiteLink, WriteFn,
};
use rdy_engine::hub_publish::PublishRequest;
use rdy_engine::semantic::lease::LeaseRegistry;
use rdy_engine::site_target::SiteTarget;
use rdy_engine::{BriefRegistry, E_SITE, OpenReport, Rdy, RdyError, ReadState, Session};
use rdy_mcp::notify::NotifyHub;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

type SessionEndHook = Box<dyn Fn(SessionId) + Send + Sync>;

pub struct Shared {
    /// The published state: every read reads it, the writer stores into it before each reply.
    pub state: ArcSwap<ReadState>,
    /// What opening the store did.
    pub open_report: OpenReport,
    pub root: PathBuf,
    pub writer: WriterHandle,
    /// The pack files as last seen by the writer.
    pub watch: PackWatch,
    /// Bumped by the writer on each reload and on each new or fixed pack error, after it stores
    /// the state that holds the change: read it before `state`, and a state is never older than
    /// its generation.
    pub pack_gen: AtomicU64,
    /// Why the pack files on disk do not load, while they do not. The writer sets it before it
    /// bumps `pack_gen`.
    pub pack_error: Mutex<Option<String>>,
    /// The project's briefing registry, taken from the `Rdy` before the writer owns it.
    pub briefs: Arc<BriefRegistry>,
    /// The project's lease registry for review requests, taken from the `Rdy` the same way.
    pub leases: Arc<LeaseRegistry>,
    /// The MCP sessions connected to this daemon, who they are and what they subscribed to
    /// (ib10). A session leaves it when it ends (a session-end hook).
    pub hub: Arc<NotifyHub>,
    /// The notifier thread's queue (ib11): the writer forwards each batch's events here, and a
    /// pack reload, after it publishes. Unbounded, so a forward never blocks the writer.
    pub(crate) notify: Sender<NotifyMsg>,
    /// The live site's state: the last build of each served scope and its open pages. The
    /// writer tells it of each publish; its `rdy-site` worker builds off the writer.
    pub site: Arc<LiveSite>,
    /// Where the site listens: set once by the daemon's start, after the bind and before
    /// `ready` (ls6). Unset for a `Shared` that no daemon serves.
    pub site_info: OnceLock<SiteInfo>,
    /// The profile-only listeners this daemon owns (D-DK03K) and the HTTP limits every listener
    /// shares; filled in by the daemon's start, closed and joined at its stop.
    pub profile_sites: crate::site::profiles::ProfileSites,
    session_end: Mutex<Vec<SessionEndHook>>,
    /// Test hook: run once by the writer right after `reload_pack` returns.
    #[cfg(test)]
    pub(crate) after_reload: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Test hook: run once by the writer in a pack reload, right before it stores the state.
    #[cfg(test)]
    pub(crate) before_reload_store: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Test count: the log lines the writer wrote for a failed clean republish.
    #[cfg(test)]
    pub(crate) republish_failure_logs: std::sync::atomic::AtomicUsize,
    /// Test record: each (session, URI) the notifier told about a batch's events, in order.
    #[cfg(test)]
    pub(crate) delivered: Mutex<Vec<(u64, String)>>,
}

impl Shared {
    /// Publishes `rdy`'s state and starts the writer that owns it from now on, the notifier
    /// that tells subscribed MCP sessions what the writer's batches changed (ib11), and the site
    /// worker that builds the scopes with an open page after each publish (ls5). The writer
    /// receives every event `rdy` emits from now on (`Rdy::subscribe`, before the writer owns it).
    pub fn new(rdy: Rdy) -> Arc<Shared> {
        let open_report = rdy.open_report();
        let root = rdy.root().to_path_buf();
        let state = ArcSwap::new(rdy.read());
        let watch = PackWatch::new(&root, rdy.pack_fingerprint());
        let briefs = rdy.briefs();
        let leases = rdy.leases();
        let events = rdy.subscribe();
        let owned = Arc::clone(&leases);
        let hub = NotifyHub::new();
        let leaving = Arc::clone(&hub);
        let (notify, queue) = channel();
        let site = LiveSite::new();
        let sh = Arc::new_cyclic(|weak| {
            // the notifier ends when its queue's sender, held here, is dropped with the last
            // `Shared`
            drop(spawn_notifier(weak.clone(), queue));
            Shared {
                state,
                open_report,
                root,
                writer: Writer::spawn(rdy, events, weak.clone()),
                watch,
                pack_gen: AtomicU64::new(0),
                pack_error: Mutex::new(None),
                briefs,
                leases,
                hub,
                notify,
                site,
                site_info: OnceLock::new(),
                profile_sites: Default::default(),
                session_end: Mutex::new(vec![
                    Box::new(move |id: SessionId| {
                        owned.release_owner(id.0);
                    }),
                    Box::new(move |id: SessionId| leaving.disconnect(id.0)),
                ]),
                #[cfg(test)]
                after_reload: Mutex::new(None),
                #[cfg(test)]
                before_reload_store: Mutex::new(None),
                #[cfg(test)]
                republish_failure_logs: std::sync::atomic::AtomicUsize::new(0),
                #[cfg(test)]
                delivered: Mutex::new(Vec::new()),
            }
        });
        // the site worker ends on `LiveSite::close` (the shutdown) or with the last `Shared`.
        // It starts once `sh` lives: inside `new_cyclic` it could see no `Shared` yet and end.
        drop(spawn_site_worker(Arc::downgrade(&sh), Arc::clone(&sh.site)));
        sh
    }

    /// The engine over this daemon: reads from the published state, writes on the writer.
    pub fn engine(self: &Arc<Self>) -> Arc<dyn Engine> {
        Arc::new(DaemonEngine(Arc::clone(self)))
    }

    /// Registers `f` to run whenever a session ends.
    pub fn on_session_end(&self, f: impl Fn(SessionId) + Send + Sync + 'static) {
        self.hooks().push(Box::new(f));
    }

    /// Runs every session-end hook for `id`, in registration order. A hook must not register
    /// another hook.
    pub fn end_session(&self, id: SessionId) {
        for f in self.hooks().iter() {
            f(id);
        }
    }

    /// The pack's generation and its standing error. The generation is read first: the writer
    /// sets the error before it bumps the generation, so a newer generation never pairs with an
    /// older error.
    pub fn pack_status(&self) -> (u64, Option<String>) {
        let generation = self.pack_gen.load(Ordering::SeqCst);
        (generation, self.pack_error_lock().clone())
    }

    pub(crate) fn pack_error_lock(&self) -> MutexGuard<'_, Option<String>> {
        self.pack_error.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn hooks(&self) -> MutexGuard<'_, Vec<SessionEndHook>> {
        self.session_end.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Stops the site worker and ends every open page's stream, then stops the writer: the last
    /// state is published and the store is closed.
    pub fn shutdown(&self) -> Result<(), RdyError> {
        self.site.close();
        self.writer.shutdown()
    }
}

/// The daemon's [`Engine`]: reads load the published state, writes run on the writer.
pub struct DaemonEngine(Arc<Shared>);

impl Engine for DaemonEngine {
    fn read(&self) -> Arc<ReadState> {
        self.0.state.load_full()
    }

    fn write(&self, f: WriteFn) -> Result<Value, RdyError> {
        self.0.writer.run(f)
    }

    fn read_serialized(&self, f: ReadFn) -> Result<Value, RdyError> {
        self.0.writer.read(f)
    }

    fn republish(&self) -> Result<(), RdyError> {
        self.0.writer.republish()
    }

    fn open_report(&self) -> OpenReport {
        self.0.open_report.clone()
    }

    fn root(&self) -> PathBuf {
        self.0.root.clone()
    }

    fn briefs(&self) -> Arc<BriefRegistry> {
        Arc::clone(&self.0.briefs)
    }

    fn leases(&self) -> Arc<LeaseRegistry> {
        Arc::clone(&self.0.leases)
    }

    fn pack_generation(&self) -> u64 {
        self.0.pack_gen.load(Ordering::SeqCst)
    }

    fn pack_error(&self) -> Option<String> {
        self.0.pack_error_lock().clone()
    }

    /// The full site's link for a source or profile target, and the profile-only listener's
    /// (bound on the first ask, reused after) for [`SiteTarget::ProfileOnly`]. The profile-only
    /// link carries the listener's own LAN diagnostic; the reply words it for the build's title.
    fn site(&self, target: &SiteTarget) -> Result<SiteLink, RdyError> {
        let off = |reason: &str| {
            RdyError::new(
                E_SITE,
                format!("the live site is off: {reason}"),
                "set serve.site_port to a free port from 1024 to 65535 in .rdy/local.yaml, then run rdy serve --stop",
            )
        };
        let info = match target {
            SiteTarget::ProfileOnly { key } => self.0.profile_sites.ensure(&self.0, key)?.1,
            _ => self
                .0
                .site_info
                .get()
                .ok_or_else(|| off("the daemon has not started it"))?
                .clone(),
        };
        match &info.url {
            Ok(url) => Ok(SiteLink {
                url: url.clone(),
                lan_url: info.lan_url.clone(),
                warning: info.warning.clone(),
            }),
            Err(reason) => Err(off(reason)),
        }
    }

    /// The profile, summary page and warnings of the one build the site holds for `target`,
    /// built first when it has none or its inputs changed.
    fn site_facts(&self, target: &SiteTarget) -> Result<SiteFacts, RdyError> {
        let target = crate::site::live::normalize(target);
        let build = self.0.site.refresh(&self.0, &target)?;
        Ok(SiteFacts {
            profile: build.profile.clone(),
            changes: build.changes,
            warnings: build.live.warnings.clone(),
        })
    }

    /// `rdy_export`: the `export` op's print. A relative `out` is under the project directory;
    /// the scope defaults to canon. The session's own view reads as the `export` op reads it.
    /// One read state serves the whole print.
    fn export(&self, req: ExportRequest) -> Result<Value, RdyError> {
        let st = self.read();
        crate::ops::export_print(
            &st,
            req.scope.as_deref().unwrap_or("canon"),
            req.view.as_deref(),
            &self.0.root.join(&req.out),
            &req.format,
            req.layout.as_deref(),
            req.profile.as_deref(),
        )
    }

    /// `rdy_render`: the `render` op's static site, from one read state, as `export` reads it.
    fn render(&self, req: RenderRequest) -> Result<Value, RdyError> {
        let st = self.read();
        crate::ops::render_static(
            &st,
            req.scope.as_deref().unwrap_or("canon"),
            req.view.as_deref(),
            &self.0.root.join(&req.out),
            req.profile.as_deref(),
        )
    }

    /// `rdy_upgrade`: the connected project's facts from the published state, through the same
    /// captured-state helper as the engine's default. It never calls the stop coordinator
    /// (`crate::maintenance`): the daemon answering here is never stopped by its own result.
    fn upgrade(
        &self,
        req: rdy_engine::maintenance::UpgradeRequest,
    ) -> Result<rdy_engine::maintenance::UpgradeReport, RdyError> {
        rdy_engine::maintenance::connected(&self.read(), req.dry_run)
    }

    /// `rdy_publish`: the publish helper's candidate upload, from one read state, off the
    /// writer: no write runs and no lock is held while it sends.
    fn publish(&self, req: PublishRequest, session: &Session) -> Result<Value, RdyError> {
        crate::ops::publish::run(self.read(), &self.0.root, session, req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_mcp::notify::Sink;

    #[test]
    fn ib11_a_session_end_removes_it_from_the_hub() {
        let dir = tempfile::tempdir().unwrap();
        let sh = Shared::new(Rdy::init(dir.path(), "rdy-standard").unwrap());
        sh.hub.connect(5, Sink::None);
        sh.hub.identify(5, "agent:a", Some("a"));
        sh.hub.subscribe(5, "rdy://inbox");
        sh.hub.connect(6, Sink::None);
        assert_eq!(sh.hub.targets().len(), 2);
        sh.end_session(SessionId(5));
        let sids: Vec<u64> = sh.hub.targets().iter().map(|t| t.sid).collect();
        assert_eq!(
            sids,
            vec![6],
            "the ended session left the hub, the other stays"
        );
        sh.shutdown().unwrap();
    }

    /// Final review I1 (ruling final): `rdy_site` carries the render's warnings for the scope
    /// it links, building the scope first when the site has none of it; a scope with none has
    /// no `warnings`.
    #[test]
    fn ls7_rdy_site_carries_the_render_warnings_of_its_scope() {
        use rdy_engine::engine::site_reply;
        use rdy_engine::{Session, WriteRequest};
        use rdy_model::Role;
        let log = site_log();
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        let writer = Session::human("human:qa".into(), Role::Writer).unwrap();
        let ops = vec![
            serde_json::json!({"op": "create", "type": "doc", "id": "$g", "fields": {"name": "glossary", "title": "Our words", "doc_type": "page"}}),
            serde_json::json!({"op": "create", "type": "section", "id": "$s", "parent": "$g", "fields": {"title": "Words"}}),
            serde_json::json!({"op": "create", "type": "term", "parent": "$s", "fields": {"name": "Hidden"}, "body": "Kept apart."}),
        ];
        r.write(
            &writer,
            WriteRequest {
                view: "g1".into(),
                ops: rdy_engine::parse_ops(ops).unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
        let sh = Shared::new(r);
        sh.site_info
            .set(SiteInfo {
                url: Ok("http://127.0.0.1:51000/".into()),
                lan_url: None,
                warning: None,
                addrs: vec![],
            })
            .unwrap();
        let e = sh.engine();
        assert_eq!(sh.site.builds(), 0, "guard: nothing built yet");
        let v = site_reply(&*e, Some("g1"), None, false).unwrap();
        assert_eq!(
            v["warnings"],
            serde_json::json!([
                "the top-level doc named glossary has no page: glossary.html is the site's generated glossary. Rename the doc to publish it."
            ]),
            "{v}"
        );
        assert_eq!(sh.site.builds(), 1, "the scope was built for its warnings");
        let canon = site_reply(&*e, None, None, false).unwrap();
        assert!(canon.get("warnings").is_none(), "{canon}");
        // the site logs the warning of the scope it built
        assert_eq!(
            log.lines(),
            [
                "site: view:g1: warning: the top-level doc named glossary has no page: glossary.html is the site's generated glossary. Rename the doc to publish it."
            ]
        );
        sh.shutdown().unwrap();
    }

    /// Final review M1 and M2: every link `rdy_site` gives routes, through the site's guard, to
    /// the scope it names, in the one spelling of that scope; a view named `.` or `..` has no
    /// link (a browser reads it as a path step).
    #[test]
    fn ls7_every_site_link_round_trips_through_the_guard() {
        use crate::site::guard::{Guard, Route};
        use crate::site::http::parse_head;
        use rdy_engine::engine::site_reply;
        use rdy_engine::{Session, WriteRequest};
        use rdy_model::Role;
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        let writer = Session::human("human:qa".into(), Role::Writer).unwrap();
        for view in ["a b", "team/a.b", "ä", "x%2Fy", "p", ".", ".."] {
            let ops = vec![serde_json::json!({"op": "create", "type": "doc",
                "fields": {"name": "d", "title": "D", "doc_type": "overview"}})];
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
        }
        let p = r.propose(&writer, "p", "P", None).unwrap();
        let canonical = p.r#ref.clone();
        let (stable, _) = canonical.split_once('@').unwrap();
        let upper = format!("P-{}", stable[2..].to_ascii_uppercase());
        let sh = Shared::new(r);
        const ROOT: &str = "http://127.0.0.1:51000";
        sh.site_info
            .set(SiteInfo {
                url: Ok(format!("{ROOT}/")),
                lan_url: None,
                warning: None,
                addrs: vec![],
            })
            .unwrap();
        let e = sh.engine();
        let cases = [
            ("canon".to_string(), "canon".to_string()),
            ("a b".into(), "view:a b".into()),
            ("view:team/a.b".into(), "view:team/a.b".into()),
            ("ä".into(), "view:ä".into()),
            ("x%2Fy".into(), "view:x%2Fy".into()),
            (
                format!("proposal:{canonical}"),
                format!("proposal:{canonical}"),
            ),
            (
                format!("proposal:{upper}@+01"),
                format!("proposal:{canonical}"),
            ),
            (
                format!("proposal:{upper}@01"),
                format!("proposal:{canonical}"),
            ),
            (format!("proposal:{upper}"), format!("proposal:{stable}")),
            (
                format!("proposal:{}", p.id),
                format!("proposal:{canonical}"),
            ),
        ];
        let guard = Guard::full(51000, None);
        for (given, want) in cases {
            let v = site_reply(&*e, Some(&given), None, false).unwrap();
            assert_eq!(v["scope"], want.as_str(), "{given}: {v}");
            let url = v["url"].as_str().unwrap();
            let path = url.strip_prefix(ROOT).unwrap();
            let head = format!("GET {path}index.html HTTP/1.1\r\nHost: 127.0.0.1:51000\r\n\r\n");
            match guard.check(&parse_head(head.as_bytes()).unwrap()) {
                Ok(Route::Page {
                    target: SiteTarget::Source(scope),
                    path,
                }) => {
                    assert_eq!(scope.text(), want, "{given}: {url}");
                    assert_eq!(path, "index.html", "{given}: {url}");
                }
                other => panic!("{given}: {url} routes to {other:?}"),
            }
        }
        for name in [".", ".."] {
            let err = site_reply(&*e, Some(name), None, false).unwrap_err();
            assert_eq!(err.code, rdy_engine::E_BAD_INPUT, "{name}: {err:?}");
            assert_eq!(
                err.message,
                format!(
                    "a view named {name:?} has no link on the live site: a browser reads that name as a step in the path"
                ),
                "{name}"
            );
            assert_eq!(
                err.fix,
                format!("render the view with rdy render --scope view:{name} --out <dir>"),
                "{name}"
            );
        }
        sh.shutdown().unwrap();
    }

    // --- spv8: profile link replies --------------------------------------------------------------

    /// The site lines these tests' daemons log (the profile listener, its LAN and caveat
    /// warnings, render warnings), taken while no other such test runs, so each test asserts
    /// its own; every other line stays visible.
    struct SiteLog {
        capture: crate::daemon::LogCapture,
        _serial: std::sync::MutexGuard<'static, ()>,
    }

    impl SiteLog {
        /// The lines taken, with each listener's port as `<port>`.
        fn lines(&self) -> Vec<String> {
            self.capture
                .lines()
                .into_iter()
                .map(|l| match l.split_once("http://127.0.0.1:") {
                    Some((head, _)) => format!("{head}http://127.0.0.1:<port>/"),
                    None => l,
                })
                .collect()
        }
    }

    fn site_log() -> SiteLog {
        static SERIAL: Mutex<()> = Mutex::new(());
        let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        SiteLog {
            capture: crate::daemon::capture_log_for_tests(&[
                "site: profile guide: ",
                "site: profile \"Guide book\": ",
                "site: warning: this site shows only the profile ",
                "site: warning: canon no longer holds the profile ",
                ": warning: the top-level doc named glossary has no page",
            ]),
            _serial: serial,
        }
    }

    /// The lines the profile-only listener of `guide` logs when it binds: its address, the LAN
    /// setup that found no private address, and the caveat.
    fn guide_listener_lines() -> Vec<String> {
        vec![
            "site: profile guide: http://127.0.0.1:<port>/".to_string(),
            "site: profile \"Guide book\": serve.site_lan is on, but this machine has no private network address; the site stays on this machine; fix: connect this machine to a private network, then run rdy serve --stop; or set serve.site_lan to false".to_string(),
            "site: warning: this site shows only the profile \"Guide book\"; anyone with the project still reads everything; fix: give the full site's link from rdy site when the person needs everything".to_string(),
        ]
    }

    use rdy_engine::engine::{SiteFacts, site_reply, with_writer_result};
    use rdy_engine::{E_BAD_INPUT, E_NOT_FOUND, WriteRequest};
    use rdy_model::Role;
    use serde_json::json;

    fn writer() -> Session {
        Session::human("human:qa".into(), Role::Writer).unwrap()
    }

    fn approver() -> Session {
        Session::human("human:qa".into(), Role::Approver).unwrap()
    }

    fn put(r: &mut Rdy, view: &str, ops: Value) -> std::collections::BTreeMap<String, String> {
        r.write(
            &writer(),
            WriteRequest {
                view: view.into(),
                ops: rdy_engine::parse_ops(ops.as_array().unwrap().clone()).unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap_or_else(|e| panic!("write to {view}: {e}"))
        .created
    }

    fn land(r: &mut Rdy, view: &str) {
        let p = r
            .propose(&writer(), view, view, Some("test summary"))
            .unwrap();
        r.land(&approver(), &p.id).unwrap();
    }

    /// A project whose canon holds the doc `intro` and the org profile `guide` ("Guide book",
    /// selecting the doc), and whose view `work` edits the doc; the site is set up as the daemon's start leaves it.
    struct World {
        dir: tempfile::TempDir,
        sh: Arc<Shared>,
        ids: std::collections::BTreeMap<String, String>,
    }

    const FULL: &str = "http://127.0.0.1:51000/";

    fn world() -> World {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        let mut ids = put(
            &mut r,
            "w1",
            json!([
                {"op": "create", "type": "doc", "id": "$intro", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
                {"op": "create", "type": "prose", "id": "$ip", "parent": "$intro", "body": "INTRO-ONE"},
                {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
            ]),
        );
        land(&mut r, "w1");
        let made = put(
            &mut r,
            "admin",
            json!([{"op": "create", "type": "publication_profile", "id": "$guide",
                "parent": ids["$admin"],
                "fields": {"key": "guide", "title": "Guide book", "description": "About it.",
                    "audience": "org", "select": [ids["$intro"]]}}]),
        );
        land(&mut r, "admin");
        ids.extend(made);
        put(
            &mut r,
            "work",
            json!([{"op": "update", "id": ids["$ip"], "body": "INTRO-VIEW"}]),
        );
        let sh = Shared::new(r);
        sh.site_info
            .set(SiteInfo {
                url: Ok(FULL.into()),
                lan_url: None,
                warning: None,
                addrs: vec![],
            })
            .unwrap();
        World { dir, sh, ids }
    }

    /// Lets the registry of `sh` bind profile-only listeners, as a daemon's start does, with
    /// `serve.site_lan` on and no private address to bind (the LAN setup fails).
    fn open_listeners(sh: &Arc<Shared>, root: &std::path::Path) -> tokio::runtime::Runtime {
        std::fs::write(root.join(".rdy/local.yaml"), "serve:\n  site_lan: true\n").unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let handler = crate::ServeHandler::new(Arc::clone(sh), None);
        let daemon = crate::Daemon::new(
            crate::DaemonConfig::new(crate::RunDir::of_project(root), "t".into()),
            Arc::new(handler),
        );
        sh.profile_sites.init(
            rt.handle().clone(),
            daemon.handle(),
            root.canonicalize().unwrap(),
            Some(crate::site::LanOverride::NoneFound),
        );
        rt
    }

    type Before = Box<dyn FnOnce(&dyn Engine) + Send>;

    /// An engine that runs `before` once, between the profile lookup and the build facts: right
    /// after the link is given.
    struct Barrier {
        inner: Arc<dyn Engine>,
        before: Mutex<Option<Before>>,
    }

    impl Engine for Barrier {
        fn read(&self) -> Arc<ReadState> {
            self.inner.read()
        }
        fn write(&self, f: WriteFn) -> Result<Value, RdyError> {
            self.inner.write(f)
        }
        fn open_report(&self) -> OpenReport {
            self.inner.open_report()
        }
        fn root(&self) -> PathBuf {
            self.inner.root()
        }
        fn briefs(&self) -> Arc<BriefRegistry> {
            self.inner.briefs()
        }
        fn leases(&self) -> Arc<LeaseRegistry> {
            self.inner.leases()
        }
        fn site(&self, target: &SiteTarget) -> Result<SiteLink, RdyError> {
            let link = self.inner.site(target)?;
            if let Some(f) = self.before.lock().unwrap().take() {
                f(&*self.inner);
            }
            Ok(link)
        }
        fn site_facts(&self, target: &SiteTarget) -> Result<SiteFacts, RdyError> {
            self.inner.site_facts(target)
        }
    }

    fn barrier(w: &World, before: impl FnOnce(&dyn Engine) + Send + 'static) -> Barrier {
        Barrier {
            inner: w.sh.engine(),
            before: Mutex::new(Some(Box::new(before))),
        }
    }

    /// spv8: the profile object, the render warnings and the LAN wording of a reply come from
    /// the build the call captured, not from the lookup before it; a profile that is gone by the
    /// build is the build's failure.
    #[test]
    fn spv8_reply_facts_match_the_captured_selected_build() {
        let log = site_log();
        let w = world();
        let guide = w.ids["$guide"].clone();
        let rt = open_listeners(&w.sh, w.dir.path());
        // a route in a view: the profile is renamed after the lookup
        let ok = barrier(&w, {
            let guide = guide.clone();
            move |e| {
                with_writer_result(e, move |r| {
                    put(
                        r,
                        "work",
                        json!([{"op": "update", "id": guide, "fields": {"title": "Renamed book"}}]),
                    );
                    Ok(())
                })
                .unwrap();
            }
        });
        let v = site_reply(&ok, Some("work"), Some("guide"), false).unwrap();
        assert_eq!(v["profile"]["title"], "Renamed book", "{v}");
        assert_eq!(v["url"], format!("{FULL}~view/work/~profile/guide/"), "{v}");
        // the same view: the profile is gone by the build, the call refuses
        let gone = barrier(&w, {
            let guide = guide.clone();
            move |e| {
                with_writer_result(e, move |r| {
                    put(
                        r,
                        "work",
                        json!([{"op": "update", "id": guide, "fields": {"key": "other"}}]),
                    );
                    Ok(())
                })
                .unwrap();
            }
        });
        let err = site_reply(&gone, Some("work"), Some("guide"), false).unwrap_err();
        assert_eq!(err.code, E_NOT_FOUND, "{err:?}");
        // profile-only: canon is renamed by a landing after the listener is bound
        let only = barrier(&w, {
            let guide = guide.clone();
            move |e| {
                with_writer_result(e, move |r| {
                    put(
                        r,
                        "rename",
                        json!([{"op": "update", "id": guide, "fields": {"title": "Landed book"}}]),
                    );
                    land(r, "rename");
                    Ok(())
                })
                .unwrap();
            }
        });
        let v = site_reply(&only, None, Some("guide"), true).unwrap();
        assert_eq!(v["profile"]["title"], "Landed book", "{v}");
        assert_eq!(v["profile_only"], true, "{v}");
        assert_ne!(v["url"], FULL, "{v}");
        assert_eq!(
            v["warnings"][0],
            "this site shows only the profile \"Landed book\"; anyone with the project still reads everything give the full site's link from rdy site when the person needs everything",
            "{v}"
        );
        // the LAN setup could not name an address: it names the build's title
        let message = v["warning"]["message"].as_str().unwrap();
        assert!(
            message.starts_with("profile \"Landed book\": "),
            "{message}"
        );
        // the selection changes between the lookup and the build: a render warning comes into
        // the build (the profile now shows a top-level doc named glossary, which has no page)
        const GLOSSARY: &str = "the top-level doc named glossary has no page: the site keeps the name glossary.html for its generated glossary. Rename the doc to publish it.";
        let intro = w.ids["$intro"].clone();
        let gl = with_writer_result(&*w.sh.engine(), |r| {
            let made = put(
                r,
                "gl",
                json!([{"op": "create", "type": "doc", "id": "$gl",
                    "fields": {"name": "glossary", "title": "Glossary page", "doc_type": "page"}}]),
            );
            land(r, "gl");
            Ok(made["$gl"].clone())
        })
        .unwrap();
        let select_in = |title: &'static str, select: Vec<String>, view: &'static str| {
            let guide = guide.clone();
            barrier(&w, move |e| {
                with_writer_result(e, move |r| {
                    put(
                        r,
                        view,
                        json!([{"op": "update", "id": guide,
                            "fields": {"title": title, "select": select}}]),
                    );
                    land(r, view);
                    Ok(())
                })
                .unwrap();
            })
        };
        let appears = select_in("Selected book", vec![intro.clone(), gl.clone()], "sel1");
        let v = site_reply(&appears, None, Some("guide"), true).unwrap();
        assert_eq!(v["profile"]["title"], "Selected book", "{v}");
        assert_eq!(
            v["warnings"],
            json!([
                GLOSSARY,
                "this site shows only the profile \"Selected book\"; anyone with the project still reads everything give the full site's link from rdy site when the person needs everything"
            ]),
            "the build's render warning, then the caveat: {v}"
        );
        let message = v["warning"]["message"].as_str().unwrap();
        assert!(
            message.starts_with("profile \"Selected book\": "),
            "{message}"
        );
        // the warning is gone with the selection that brought it: nothing stale is kept
        let gone_again = select_in("Pruned book", vec![intro.clone()], "sel2");
        let v = site_reply(&gone_again, None, Some("guide"), false).unwrap();
        assert_eq!(v["profile"]["title"], "Pruned book", "{v}");
        assert!(v.get("warnings").is_none(), "no stale render warning: {v}");
        assert!(v.get("profile_only").is_none(), "{v}");
        // and in only-mode the caveat alone remains
        let again = select_in("Final book", vec![intro.clone()], "sel3");
        let v = site_reply(&again, None, Some("guide"), true).unwrap();
        assert_eq!(v["profile"]["title"], "Final book", "{v}");
        assert_eq!(
            v["warnings"],
            json!([
                "this site shows only the profile \"Final book\"; anyone with the project still reads everything give the full site's link from rdy site when the person needs everything"
            ]),
            "{v}"
        );
        // a route over a selection that gains the warning, with no caveat
        let appears = select_in("Again book", vec![intro, gl], "sel4");
        let v = site_reply(&appears, None, Some("guide"), false).unwrap();
        assert_eq!(v["profile"]["title"], "Again book", "{v}");
        assert_eq!(v["warnings"], json!([GLOSSARY]), "{v}");
        // a profile canon no longer holds fails the whole call, the listener already open
        let gone = barrier(&w, {
            let guide = guide.clone();
            move |e| {
                with_writer_result(e, move |r| {
                    put(
                        r,
                        "rekey",
                        json!([{"op": "update", "id": guide, "fields": {"key": "moved"}}]),
                    );
                    land(r, "rekey");
                    Ok(())
                })
                .unwrap();
            }
        });
        let err = site_reply(&gone, None, Some("guide"), true).unwrap_err();
        assert_eq!(err.code, E_NOT_FOUND, "{err:?}");
        w.sh.shutdown().unwrap();
        drop(rt);
        // the listener's lines, the render warnings of the builds that held the glossary doc,
        // and the profile canon no longer holds
        let mut expected = guide_listener_lines();
        expected.extend([
            format!("site: /: warning: {GLOSSARY}"),
            format!("site: /~profile/guide/: warning: {GLOSSARY}"),
            "site: warning: canon no longer holds the profile \"guide\"; its profile-only site serves no page; fix: land the profile again, or run rdy serve --stop to end the site".to_string(),
        ]);
        assert_eq!(log.lines(), expected);
    }

    /// spv8: a refused `only` or an unknown profile registers no listener.
    #[test]
    fn spv8_only_validation_precedes_binding() {
        let log = site_log();
        let w = world();
        let rt = open_listeners(&w.sh, w.dir.path());
        let e = w.sh.engine();
        for (scope, profile) in [(None, None), (Some("work"), Some("guide"))] {
            let err = site_reply(&*e, scope, profile, true).unwrap_err();
            assert_eq!(err.code, E_BAD_INPUT, "{err:?}");
            assert_eq!(
                err.message,
                "a profile-only site needs a profile and shows canon only"
            );
            assert_eq!(err.fix, "pass profile with scope canon, or leave out only");
        }
        let err = site_reply(&*e, None, Some("missing"), true).unwrap_err();
        assert_eq!(err.code, E_NOT_FOUND, "{err:?}");
        assert!(w.sh.profile_sites.keys().is_empty(), "nothing registered");
        // false is an ordinary route, and binds nothing either
        let v = site_reply(&*e, None, Some("guide"), false).unwrap();
        assert!(v.get("profile_only").is_none(), "{v}");
        assert!(w.sh.profile_sites.keys().is_empty(), "nothing registered");
        // a valid only-mode registers its folded key once, and reuses it
        let a = site_reply(&*e, None, Some("GUIDE"), true).unwrap();
        let b = site_reply(&*e, Some("canon"), Some(&w.ids["$guide"]), true).unwrap();
        assert_eq!(a, b);
        assert_eq!(w.sh.profile_sites.keys(), vec!["guide".to_string()]);
        w.sh.shutdown().unwrap();
        drop(rt);
        assert_eq!(log.lines(), guide_listener_lines());
    }

    /// spv8: the `site_url` op reads `{scope?, profile?, only?}` with the strict parser's hints,
    /// and gives the value the direct helper gives.
    #[test]
    fn spv8_ctl_site_url_is_strict_and_shares_the_reply() {
        use crate::ops::{OpContext, dispatch};
        let log = site_log();
        let w = world();
        let rt = open_listeners(&w.sh, w.dir.path());
        let e = w.sh.engine();
        let s = Session::human("human:qa".into(), Role::Writer).unwrap();
        let cx = OpContext {
            engine: &*e,
            session: &s,
            human: "human:qa",
            cwd: w.dir.path(),
            agent: &rdy_engine::signing_crypto::UnavailableAgent,
        };
        let err = dispatch(&cx, "site_url", json!({"profiel": "guide"})).unwrap_err();
        assert_eq!(err.code, E_BAD_INPUT, "{err:?}");
        assert_eq!(
            err.message, "site_url: unknown field \"profiel\"",
            "{err:?}"
        );
        assert!(err.fix.contains("profile"), "{err:?}");
        let err =
            dispatch(&cx, "site_url", json!({"profile": "guide", "only": "yes"})).unwrap_err();
        assert_eq!(err.code, E_BAD_INPUT, "{err:?}");
        assert!(err.message.starts_with("site_url: "), "{err:?}");
        assert_eq!(
            err.fix, "args for site_url: {scope?, profile?, only?}",
            "{err:?}"
        );
        assert!(
            w.sh.profile_sites.keys().is_empty(),
            "a refusal binds nothing"
        );
        for (args, scope, profile, only) in [
            (json!({}), None, None, false),
            (json!(null), None, None, false),
            (json!({"scope": "canon"}), Some("canon"), None, false),
            (
                json!({"scope": "canon", "profile": "guide"}),
                Some("canon"),
                Some("guide"),
                false,
            ),
            (
                json!({"profile": "guide", "only": false}),
                None,
                Some("guide"),
                false,
            ),
            (
                json!({"profile": "guide", "only": true}),
                None,
                Some("guide"),
                true,
            ),
        ] {
            let via_op = dispatch(&cx, "site_url", args.clone()).unwrap();
            let direct = site_reply(&*e, scope, profile, only).unwrap();
            assert_eq!(via_op, direct, "{args}");
        }
        let err = dispatch(&cx, "site_url", json!({"only": true})).unwrap_err();
        assert_eq!(err.code, E_BAD_INPUT, "{err:?}");
        w.sh.shutdown().unwrap();
        drop(rt);
        assert_eq!(log.lines(), guide_listener_lines());
    }

    /// ec11 fix round 1 (finding 3): `rdy_export` reads a scope as the `export` op does. The
    /// session's own view needs no record (it is made by the first write); another view with no
    /// record is not found. The same holds for a session view `rdy_session` selects.
    #[test]
    fn ec11_rdy_export_takes_the_session_view_as_the_export_op_does() {
        use crate::ops::{OpContext, dispatch};
        use rdy_engine::{E_NOT_FOUND, Session};
        use rdy_mcp::tools::{ExportFormat, ExportParams, McpState, rdy_export, rdy_session};
        use rdy_model::Role;
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let sh = Shared::new(Rdy::init(dir.path(), "rdy-standard").unwrap());
        let e = sh.engine();
        let out = tempfile::tempdir().unwrap();
        let path = |name: &str| out.path().join(name).to_str().unwrap().to_string();
        let op = |run: &str, scope: &str, name: &str| {
            let s = Session::agent("agent:review".into(), Some(run.into()), Role::Reader).unwrap();
            let cx = OpContext {
                engine: &*e,
                session: &s,
                human: "human:t",
                cwd: out.path(),
                agent: &rdy_engine::signing_crypto::UnavailableAgent,
            };
            dispatch(
                &cx,
                "export",
                json!({"out": path(name), "format": "html", "scope": scope}),
            )
        };
        let mcp = |session: Value| {
            let mut st = McpState::new(Arc::clone(&e));
            rdy_session(&mut st, serde_json::from_value(session).unwrap()).unwrap();
            st
        };
        let tool = |st: &mut McpState, scope: &str, name: &str| {
            rdy_export(
                st,
                ExportParams {
                    out: path(name),
                    format: ExportFormat::Html,
                    layout: None,
                    scope: Some(scope.into()),
                    profile: None,
                },
            )
        };

        // before the session's first write: its run names its view
        let mut st = mcp(json!({"actor": "agent:review", "run": "fresh", "role": "reader"}));
        op("fresh", "fresh", "op-fresh.html").expect("the op reads its own view");
        tool(&mut st, "fresh", "mcp-fresh.html").expect("the tool reads its own view");
        assert_eq!(
            std::fs::read(path("op-fresh.html")).unwrap(),
            std::fs::read(path("mcp-fresh.html")).unwrap(),
            "the same print"
        );
        // another view with no record is not found, alike
        let a = op("fresh", "nobody", "op-nobody.html").unwrap_err();
        let b = tool(&mut st, "nobody", "mcp-nobody.html").unwrap_err();
        assert_eq!((a.code, &a.message), (E_NOT_FOUND, &b.message));
        assert_eq!(b.code, E_NOT_FOUND);
        assert!(!out.path().join("op-nobody.html").exists());
        assert!(!out.path().join("mcp-nobody.html").exists());

        // a session view rdy_session selects is the session's own view
        let mut st = mcp(
            json!({"actor": "agent:review", "run": "other", "role": "reader", "view": "draft"}),
        );
        op("draft", "draft", "op-draft.html").expect("the op reads its own view");
        tool(&mut st, "draft", "mcp-draft.html").expect("the tool reads its selected view");
        let e = tool(&mut st, "other", "mcp-other.html").unwrap_err();
        assert_eq!(
            e.code, E_NOT_FOUND,
            "the run's name is not this session's view"
        );
        sh.shutdown().unwrap();
    }
}
