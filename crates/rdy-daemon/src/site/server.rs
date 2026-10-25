//! The site's HTTP server (FW-LS1, D-N5NSN; FW-LS1a, D-FV6G9): bind, accept, read one request
//! head, guard, route, answer.
//!
//! It binds only after the daemon owns the project (the store lock is held and the socket is
//! bound and recorded, D-WZ4ZP, D-R7N9J), and it accepts until the daemon stops, for any reason.
//! Each accepted connection runs the daemon's project check first, and again when its request
//! head arrives (D-A6ZZA): a gone or replaced project gets no reply, and the daemon exits. Every
//! request passes the closed guard; every answer comes from an in-memory build or a compiled-in
//! asset, and closes the connection.
use crate::daemon::{DaemonHandle, log};
use crate::shared::Shared;
use crate::site::guard::{Guard, ListenerMode, Refusal, Route, ScopeRef, SiteTarget, refusal};
use crate::site::http::{
    MAX_HEAD_BYTES, Method, ParseError, Response, content_type, parse_head, to_bytes,
};
use crate::site::live::{NoStream, label, resolve};
use crate::site::profiles::{self, Availability, TestEvent};
use crate::site::{LanOverride, SiteInfo, addr, sse};
use rdy_engine::engine::{SITE_LAN_FIX, SiteWarning};
use rdy_engine::publication::{IMPLICIT_ORG, profile_at};
use rdy_engine::{E_NOT_FOUND, E_REDACTED_STATE, ReadState};
use rdy_engine::{ServeConfig, serve_config};
use rdy_render::NotFoundKind;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// The listen backlog of each bound address.
const BACKLOG: u32 = 64;

/// The fix of the `W_SITE_LAN` warning when `serve.site_lan` is on and the machine has no
/// private network address: the site already stays on this machine.
pub const NO_LAN_ADDRESS_FIX: &str = "connect this machine to a private network, then run rdy serve --stop; or set serve.site_lan to false";

/// The fix of the `W_SITE_LAN` warning when the LAN address could not be bound.
pub const LAN_BIND_FIX: &str =
    "set serve.site_lan_address to a private address of this machine, then run rdy serve --stop";

/// The connections the site serves at once, across the full listener and every profile-only
/// one; past that, a connection gets `Busy`.
pub const MAX_CONNECTIONS: usize = 64;
/// The connections past [`MAX_CONNECTIONS`] that get the `Busy` answer at once; past that, a
/// connection gets what the socket takes at once, and closes.
const BUSY_RESPONDERS: usize = 8;
/// How long a connection past [`MAX_CONNECTIONS`] may take to send its head before it gets the
/// `Busy` answer all the same.
const BUSY_HEAD_TIMEOUT: Duration = Duration::from_secs(1);
/// How long a connection may take to send its request head; then it closes with no reply.
pub const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
/// How long an answer may take to write; then the connection closes.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
/// Read past an answer at most this much before closing, so the close sends no reset.
const DRAIN_BYTES: usize = 64 * 1024;

/// The bound site, ready to [`run`](SiteServer::run).
pub struct SiteServer {
    listeners: Vec<TcpListener>,
    guard: Guard,
}

/// The HTTP limits every listener of one daemon shares (D-DK03K): [`MAX_CONNECTIONS`]
/// connections served at once and [`BUSY_RESPONDERS`] `Busy` answers past them, whatever port
/// each connection arrived on, so more ports never mean more of either.
pub struct HttpLimits {
    conns: Arc<Semaphore>,
    busy: Arc<Semaphore>,
}

impl Default for HttpLimits {
    fn default() -> HttpLimits {
        HttpLimits {
            conns: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
            busy: Arc::new(Semaphore::new(BUSY_RESPONDERS)),
        }
    }
}

/// What every connection of one listener shares. The daemon's `Shared` is held weakly (a
/// connection upgrades it), so a listener's task, which the registry on `Shared` keeps a handle
/// of, makes no cycle with it.
struct Ctx {
    guard: Guard,
    shared: Weak<Shared>,
    handle: DaemonHandle,
    limit: Arc<Semaphore>,
    /// The tasks that answer `Busy` past `limit`.
    busy: Arc<Semaphore>,
}

/// What one bind opened: the listeners of one port (`127.0.0.1`, then `[::1]` and the LAN
/// address when they bind), and the site's info.
pub(crate) struct Opened {
    pub listeners: Vec<TcpListener>,
    pub port: u16,
    pub lan: Option<Ipv4Addr>,
    pub info: SiteInfo,
}

/// A listening socket on `at`, with `SO_REUSEADDR` so a successor binds the port at once. The
/// project at `root` hears of each attempt (a test hook).
fn listen(root: &Path, at: SocketAddr) -> io::Result<TcpListener> {
    profiles::report(root, TestEvent::Bind(at));
    let socket = match at {
        SocketAddr::V4(_) => TcpSocket::new_v4()?,
        SocketAddr::V6(_) => TcpSocket::new_v6()?,
    };
    socket.set_reuseaddr(true)?;
    socket.bind(at)?;
    socket.listen(BACKLOG)
}

/// The time now, in seconds since the epoch, for the `Date` header.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Binds `127.0.0.1` on the first port of `ports` that is free, then `[::1]` on that port (best
/// effort), then the LAN address of `cfg` on that port when `serve.site_lan` is on; never a
/// wildcard or public address. `lan_override` stands in for `addr::lan_address()` (a test hook).
/// Runs inside a Tokio runtime (it registers the listeners); it never waits. With no loopback
/// port, it is the last bind error, and every socket it opened is closed.
pub(crate) fn open(
    root: &Path,
    cfg: &ServeConfig,
    ports: &[u16],
    lan_override: Option<LanOverride>,
) -> Result<Opened, String> {
    let mut last = None;
    let mut bound = None;
    for &port in ports {
        match listen(root, SocketAddr::from((Ipv4Addr::LOCALHOST, port))) {
            Ok(l) => {
                bound = Some((port, l));
                break;
            }
            Err(e) => last = Some(e),
        }
    }
    let Some((port, v4)) = bound else {
        return Err(last.map_or_else(|| "no port to try".to_string(), |e| e.to_string()));
    };
    let mut addrs = vec![SocketAddr::from((Ipv4Addr::LOCALHOST, port))];
    let mut listeners = vec![v4];
    let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
    match listen(root, v6) {
        Ok(l) => {
            listeners.push(l);
            addrs.push(v6);
        }
        Err(e) => log(&format!(
            "site: could not listen on [::1] ({e}); the site answers on 127.0.0.1"
        )),
    }
    let (mut lan, mut lan_url, mut warning) = (None, None, None);
    if cfg.site_lan {
        let ip = match (cfg.site_lan_address, lan_override) {
            (Some(ip), _) => Some(ip),
            (None, Some(LanOverride::Addr(ip))) => Some(ip),
            (None, Some(LanOverride::NoneFound)) => None,
            (None, None) => addr::lan_address(),
        };
        match ip {
            None => {
                warning = Some(SiteWarning {
                    message: "serve.site_lan is on, but this machine has no private network address; the site stays on this machine".to_string(),
                    fix: NO_LAN_ADDRESS_FIX.to_string(),
                });
            }
            Some(ip) => {
                let at = SocketAddr::from((ip, port));
                // an address already bound (the test stand-in 127.0.0.1) counts as bound
                let out = if addrs.contains(&at) {
                    Ok(())
                } else {
                    listen(root, at).map(|l| {
                        listeners.push(l);
                        addrs.push(at);
                    })
                };
                match out {
                    Ok(()) => {
                        let url = addr::url(&ip.to_string(), port);
                        warning = Some(SiteWarning {
                            message: format!("anyone on your network can read these docs at {url}"),
                            fix: SITE_LAN_FIX.to_string(),
                        });
                        lan_url = Some(url);
                        lan = Some(ip);
                    }
                    Err(e) => {
                        warning = Some(SiteWarning {
                            message: format!(
                                "could not listen on {ip} ({e}); the site stays on this machine"
                            ),
                            fix: LAN_BIND_FIX.to_string(),
                        });
                    }
                }
            }
        }
    }
    let info = SiteInfo {
        url: Ok(addr::url("127.0.0.1", port)),
        lan_url,
        warning,
        addrs,
    };
    Ok(Opened {
        listeners,
        port,
        lan,
        info,
    })
}

impl SiteServer {
    /// Binds the site of the project at `root` (the canonical root): `127.0.0.1` on the first
    /// free port of `addr::candidates`, then `[::1]` on that port (best effort), then the LAN
    /// address when `serve.site_lan` is on. `lan_override` stands in for `addr::lan_address()`
    /// (a test hook). With no loopback port, there is no server and the info's `url` is the
    /// last bind error.
    pub async fn bind(
        root: &Path,
        lan_override: Option<LanOverride>,
    ) -> (Option<SiteServer>, SiteInfo) {
        let cfg = serve_config(root);
        match open(
            root,
            &cfg,
            &addr::candidates(cfg.site_port, root),
            lan_override,
        ) {
            Ok(o) => {
                let server = SiteServer::new(o.listeners, Guard::full(o.port, o.lan));
                (Some(server), o.info)
            }
            Err(reason) => (
                None,
                SiteInfo {
                    url: Err(reason),
                    lan_url: None,
                    warning: None,
                    addrs: Vec::new(),
                },
            ),
        }
    }

    /// The server of `listeners`, answering by `guard`.
    pub(crate) fn new(listeners: Vec<TcpListener>, guard: Guard) -> SiteServer {
        SiteServer { listeners, guard }
    }

    /// Accepts on every bound address until the daemon stops (`handle.stopped()`), and serves
    /// each connection on a task of its own, within the limits every listener of the daemon
    /// shares.
    pub async fn run(self, shared: Arc<Shared>, handle: DaemonHandle) {
        self.run_weak(Arc::downgrade(&shared), handle).await
    }

    /// [`Self::run`], holding `shared` weakly: each connection upgrades it, and none is served
    /// once no `Shared` lives.
    pub(crate) async fn run_weak(self, shared: Weak<Shared>, handle: DaemonHandle) {
        let Some(limits) = shared.upgrade().map(|sh| {
            let l = &sh.profile_sites.limits;
            (Arc::clone(&l.conns), Arc::clone(&l.busy))
        }) else {
            return;
        };
        let ctx = Arc::new(Ctx {
            guard: self.guard,
            shared,
            handle,
            limit: limits.0,
            busy: limits.1,
        });
        let mut loops = JoinSet::new();
        for listener in self.listeners {
            loops.spawn(accept_loop(listener, Arc::clone(&ctx)));
        }
        while loops.join_next().await.is_some() {}
    }
}

async fn accept_loop(listener: TcpListener, ctx: Arc<Ctx>) {
    let mut stopped = ctx.handle.stopped();
    loop {
        if *stopped.borrow_and_update() {
            return;
        }
        tokio::select! {
            changed = stopped.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => accept(stream, &ctx),
                Err(e) => {
                    log(&format!("site: accept failed: {e}"));
                    // e.g. out of fds: don't spin
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
        }
    }
}

/// One accepted connection: the project check, then a task while fewer than
/// [`MAX_CONNECTIONS`] run, else `Busy`.
fn accept(stream: TcpStream, ctx: &Arc<Ctx>) {
    // D-A6ZZA: the old daemon of a gone or replaced project never serves its pages
    if ctx.handle.check_project() {
        return;
    }
    let Some(sh) = ctx.shared.upgrade() else {
        return;
    };
    match Arc::clone(&ctx.limit).try_acquire_owned() {
        Ok(permit) => {
            let ctx = Arc::clone(ctx);
            tokio::spawn(async move {
                let _permit = permit;
                serve_conn(stream, ctx, sh).await;
            });
        }
        Err(_) => match Arc::clone(&ctx.busy).try_acquire_owned() {
            // a short task outside the limit: the answer goes out after the head, so the
            // head's arrival at a closed socket does not reset the connection and lose it
            Ok(permit) => {
                let ctx = Arc::clone(ctx);
                tokio::spawn(async move {
                    let _permit = permit;
                    let head = tokio::time::timeout(BUSY_HEAD_TIMEOUT, read_head(&stream)).await;
                    if matches!(head, Ok(Head::Closed)) || gone(&ctx) {
                        return;
                    }
                    send(stream, &refusal(Refusal::Busy), false).await;
                });
            }
            // no task: what the reader sent so far is read off, the answer goes out if the
            // socket takes it at once, and the connection closes
            Err(_) => {
                drain_now(&stream);
                let _ = stream.try_write(&to_bytes(&refusal(Refusal::Busy), false, now_secs()));
            }
        },
    }
}

/// Reads off what the peer already sent, up to [`DRAIN_BYTES`], without waiting.
fn drain_now(stream: &TcpStream) {
    let mut buf = [0u8; 4096];
    let mut drained = 0;
    while drained < DRAIN_BYTES {
        match stream.try_read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => drained += n,
        }
    }
}

/// D-A6ZZA again, once a request is here: true when the daemon is stopping, or its project is
/// gone or replaced (and the daemon then stops). A connection accepted before the project was
/// replaced, such as a browser's spare socket, then gets no reply either.
fn gone(ctx: &Ctx) -> bool {
    *ctx.handle.stopped().borrow() || ctx.handle.check_project()
}

/// A request head as read off a connection.
enum Head {
    /// The bytes up to and including the first empty line, or all the bytes the peer sent
    /// before it closed its side.
    Bytes(Vec<u8>),
    /// Past [`MAX_HEAD_BYTES`] with no empty line.
    TooLarge,
    /// The peer closed with nothing sent, or the read failed.
    Closed,
}

async fn read_head(stream: &TcpStream) -> Head {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    loop {
        if stream.readable().await.is_err() {
            return Head::Closed;
        }
        let n = match stream.try_read(&mut chunk) {
            Ok(0) if buf.is_empty() => return Head::Closed,
            Ok(0) => return Head::Bytes(buf),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
            Err(_) => return Head::Closed,
        };
        let from = buf.len().saturating_sub(3);
        buf.extend_from_slice(&chunk[..n]);
        if let Some(at) = buf[from..].windows(4).position(|w| w == b"\r\n\r\n") {
            buf.truncate(from + at + 4);
            return Head::Bytes(buf);
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Head::TooLarge;
        }
    }
}

async fn serve_conn(stream: TcpStream, ctx: Arc<Ctx>, sh: Arc<Shared>) {
    let head = match tokio::time::timeout(HEAD_TIMEOUT, read_head(&stream)).await {
        Ok(head) => head,
        // no head in time: close with no reply
        Err(_) => return,
    };
    if gone(&ctx) {
        return;
    }
    let bytes = match head {
        Head::Closed => return,
        Head::TooLarge => return send(stream, &refusal(Refusal::TooLarge), false).await,
        Head::Bytes(bytes) => bytes,
    };
    let request = match parse_head(&bytes) {
        Ok(r) => r,
        Err(ParseError::TooLarge) => {
            return send(stream, &refusal(Refusal::TooLarge), false).await;
        }
        Err(ParseError::Malformed) => {
            return send(stream, &refusal(Refusal::Malformed), false).await;
        }
    };
    let head_only = request.method == Method::Head;
    // the one entry check (D-CC90E): while the published state predates a removal
    // (E_REDACTED_STATE), no page, file or stream is served; the existing Busy answer stands until
    // the writer publishes the fresh state. The state is not held past the check.
    let state = sh.state.load_full();
    let current = state.ensure_current_epoch().is_ok();
    drop(state);
    let route = match ctx.guard.check(&request) {
        // a path the guard does not route, after the Host, Origin and method checks: the
        // listener's own not-found page, once the same epoch and availability checks as a page
        // pass. The path is not read again
        Err(Refusal::NotFound) if !current => Err(refusal(Refusal::Busy)),
        Err(Refusal::NotFound) => match held(&ctx, &sh, None).await {
            Some(f) => Err(refusal(f)),
            None => Err(not_found(Arc::clone(&sh), Miss::Page(listener_target(&ctx.guard))).await),
        },
        Err(f) => Err(refusal(f)),
        Ok(_) if !current => Err(refusal(Refusal::Busy)),
        // a profile-only listener serves nothing while canon does not hold its key: every
        // guarded path, cached or not, is then the plain not found (D-DK03K)
        Ok(route) => match held(&ctx, &sh, Some(&route)).await {
            Some(f) => Err(refusal(f)),
            None => Ok(route),
        },
    };
    let response = match route {
        Err(r) => r,
        Ok(Route::LiveJs) => Response {
            status: 200,
            headers: vec![("Content-Type", content_type("live.js").to_string())],
            body: rdy_render::live::LIVE_JS.as_bytes().to_vec(),
        },
        Ok(Route::Page { target, path }) => page(Arc::clone(&sh), target, path).await,
        Ok(Route::Events {
            target,
            page,
            rev,
            label,
        }) => {
            let sub = {
                let sh = Arc::clone(&sh);
                tokio::task::spawn_blocking(move || {
                    sh.site.subscribe(&sh, &target, &page, &rev, &label)
                })
                .await
            };
            match sub {
                Ok(Ok(sub)) => {
                    // the hold before the head: once the reader has the head, the page (in
                    // view: live.js closes the stream of a hidden page) holds the daemon
                    let hold = ctx.handle.hold();
                    let epoch = sse::epoch_now(Arc::clone(&sh));
                    sse::serve(stream, sub, hold, ctx.handle.stopped(), epoch, now_secs()).await;
                    return;
                }
                Ok(Err(NoStream::Busy)) => refusal(Refusal::Busy),
                Ok(Err(NoStream::NotFound)) => refusal(Refusal::NotFound),
                Ok(Err(NoStream::Broken)) => refusal(Refusal::Broken),
                Err(_panicked) => refusal(Refusal::Broken),
            }
        }
    };
    send(stream, &response, head_only).await;
}

/// The availability check of a profile-only listener, after the epoch check and before any
/// bytes: `None` when the listener is full, or when canon still holds its key. A key canon no
/// longer holds is not found. Malformed or redacted metadata lets a page take the build path,
/// which logs why it is broken, and a path the guard does not route (`route` is `None`) take
/// its not-found page's build, which then gives the plain answer; the script and the events are
/// then broken at once.
async fn held(ctx: &Ctx, sh: &Arc<Shared>, route: Option<&Route>) -> Option<Refusal> {
    let ListenerMode::ProfileOnly { key } = ctx.guard.mode() else {
        return None;
    };
    let (sh, key) = (Arc::clone(sh), key.clone());
    let found = tokio::task::spawn_blocking(move || {
        let st = sh.state.load_full();
        sh.profile_sites.availability(&st, &key)
    })
    .await;
    match found {
        Ok(Availability::Held) => None,
        Ok(Availability::Missing) => Some(Refusal::NotFound),
        Ok(Availability::Broken) if matches!(route, None | Some(Route::Page { .. })) => None,
        Ok(Availability::Broken) | Err(_) => Some(Refusal::Broken),
    }
}

/// A page or file of `target`: the config check on an HTML page, then the target's build as of
/// the latest publish, then the bytes the server sends for `path`. A path the build does not
/// have is the target's not-found page; a target the site does not hold now is canon's.
async fn page(sh: Arc<Shared>, target: SiteTarget, path: String) -> Response {
    let built = tokio::task::spawn_blocking(move || {
        // a page load reloads a changed config, as a new connection does (`session_started`)
        if path.ends_with(".html") && sh.watch.changed() {
            let _ = sh.writer.reload();
        }
        let build = match sh.site.refresh(&sh, &target) {
            Ok(b) => b,
            // a removal since the entry check: the build of the old state was discarded
            Err(e) if e.code == E_REDACTED_STATE => return Err(refusal(Refusal::Busy)),
            // a scope that does not resolve names no view or proposal, and a profile its scope
            // does not hold has no pages: canon's page for a scope that is not on the site now,
            // which never names it. A profile-only listener whose profile is gone has no layout:
            // the plain answer (the menu of canon always resolves, and a failed build of it is
            // broken, not missing)
            Err(_) if missing(&sh.state.load_full(), &target) => {
                let miss = match target {
                    SiteTarget::ProfileOnly { .. } => Miss::Plain,
                    _ => Miss::Scope,
                };
                return Err(not_found_now(&sh, miss));
            }
            Err(e) => {
                log(&format!(
                    "site: the {} pages did not build ({e})",
                    label(&target)
                ));
                return Err(refusal(Refusal::Broken));
            }
        };
        match rdy_render::live::page(&build.live, &path, &target) {
            Some(body) => Ok((content_type(&path), body)),
            None => {
                drop(build);
                Err(not_found_now(&sh, Miss::Page(target)))
            }
        }
    })
    .await;
    match built {
        Ok(Ok((kind, body))) => Response {
            status: 200,
            headers: vec![("Content-Type", kind.to_string())],
            body,
        },
        Ok(Err(r)) => r,
        Err(_panicked) => refusal(Refusal::Broken),
    }
}

/// Which not-found answer a request gets (D-94BT2, D-E8P8J; the matrix of system/site n-REYZ9).
enum Miss {
    /// The Page variant of a target: a path it does not have, rooted at its prefix.
    Page(SiteTarget),
    /// Canon's Scope variant: a scope or profile that is not on the site now.
    Scope,
    /// The plain not found: a profile-only listener whose profile is gone.
    Plain,
}

/// The target whose Page variant a path the guard does not route gets on this listener: canon
/// on the full listener, the listener's own profile on a profile-only one.
fn listener_target(guard: &Guard) -> SiteTarget {
    match guard.mode() {
        ListenerMode::Full => SiteTarget::Source(ScopeRef::Canon),
        ListenerMode::ProfileOnly { key } => SiteTarget::ProfileOnly { key: key.clone() },
    }
}

/// [`not_found_now`] off the async runtime; a panicked build is the plain answer.
async fn not_found(sh: Arc<Shared>, miss: Miss) -> Response {
    tokio::task::spawn_blocking(move || not_found_now(&sh, miss))
        .await
        .unwrap_or_else(|_panicked| refusal(Refusal::NotFound))
}

/// The not-found answer of `miss`, from the state published now: status 404 and the themed
/// page, which has no live script, stream or revision and joins no cache. The redaction epoch
/// is checked before the build and again before the bytes go out, so a page of a state a
/// removal made old is never sent: the existing `Busy` stands. A page whose layout cannot be
/// built (a build error or a panic) is the plain not found, never another status. The versions
/// menu, which has no page of its own to miss, gets canon's.
fn not_found_now(sh: &Shared, miss: Miss) -> Response {
    let (target, kind) = match miss {
        Miss::Plain => return refusal(Refusal::NotFound),
        Miss::Scope => (SiteTarget::Source(ScopeRef::Canon), NotFoundKind::Scope),
        Miss::Page(SiteTarget::Menu { .. } | SiteTarget::Source(ScopeRef::Versions)) => {
            (SiteTarget::Source(ScopeRef::Canon), NotFoundKind::Page)
        }
        Miss::Page(target) => (target, NotFoundKind::Page),
    };
    let st = sh.state.load_full();
    if st.ensure_current_epoch().is_err() {
        return refusal(Refusal::Busy);
    }
    let built = catch_unwind(AssertUnwindSafe(|| {
        rdy_render::live::build_not_found(&st, &target, kind)
    }));
    // a removal while it built: the page of the old state is not sent
    if st.ensure_current_epoch().is_err() {
        return refusal(Refusal::Busy);
    }
    match built {
        Ok(Ok(page)) => Response {
            status: 404,
            headers: vec![("Content-Type", content_type("404.html").to_string())],
            body: page.html,
        },
        Ok(Err(e)) if e.code == E_REDACTED_STATE => refusal(Refusal::Busy),
        Ok(Err(_)) | Err(_) => refusal(Refusal::NotFound),
    }
}

/// Whether `target` names what `st` does not hold: a scope that does not resolve, or a profile
/// (of a route, of only-mode or of a menu's context) its scope has no block for. Only the
/// profile's metadata is read, by its lookup; malformed or redacted metadata is not missing, so
/// it takes the build-error path.
fn missing(st: &ReadState, target: &SiteTarget) -> bool {
    let Ok(scope) = resolve(st, &target.scope()) else {
        return true;
    };
    let key = match target {
        SiteTarget::Profile { key, .. } | SiteTarget::ProfileOnly { key } => key,
        SiteTarget::Menu {
            profile: Some(key), ..
        } => key,
        _ => return false,
    };
    key != IMPLICIT_ORG && matches!(profile_at(st, &scope, key), Err(e) if e.code == E_NOT_FOUND)
}

/// Writes `r` (its head only for `HEAD`), then closes: the write side first, then what the peer
/// sent past its head is read off, so the close sends no reset that could hide the answer.
async fn send(mut stream: TcpStream, r: &Response, head_only: bool) {
    let bytes = to_bytes(r, head_only, now_secs());
    let sent = tokio::time::timeout(SEND_TIMEOUT, async {
        stream.write_all(&bytes).await?;
        stream.shutdown().await
    })
    .await;
    if matches!(sent, Ok(Ok(()))) {
        drain_now(&stream);
    }
}
