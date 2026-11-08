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
    MAX_HEAD_BYTES, Method, ParseError, Request, Response, content_type, parse_head, to_bytes,
};
use crate::site::live::{NoStream, label, resolve};
use crate::site::profiles::{self, Availability, TestEvent};
use crate::site::{LanOverride, SiteInfo, addr, sse, ui, ui_request};
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
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::Instant;

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

/// A listening socket on `at`, with the platform's site option ([`configure`]): on Unix
/// `SO_REUSEADDR`, so a successor binds the port at once. The project at `root` hears of each
/// attempt (a test hook).
fn listen(root: &Path, at: SocketAddr) -> io::Result<TcpListener> {
    profiles::report(root, TestEvent::Bind(at));
    let socket = match at {
        SocketAddr::V4(_) => TcpSocket::new_v4()?,
        SocketAddr::V6(_) => TcpSocket::new_v6()?,
    };
    configure(root, &socket, at)?;
    #[cfg(test)]
    seam::record(root, seam::Step::Bind(at));
    socket.bind(at)?;
    socket.listen(BACKLOG)
}

/// The platform's option on a site socket before it binds (`sys::configure_site_socket`, one
/// call for every listener: full, LAN and profile-only). A failure takes the bind's failure path.
fn configure(root: &Path, socket: &TcpSocket, at: SocketAddr) -> io::Result<()> {
    #[cfg(test)]
    if let Some(e) = seam::injected(root, at) {
        return Err(e);
    }
    crate::sys::configure_site_socket(socket)?;
    #[cfg(test)]
    seam::record(root, seam::Step::Configure(at));
    let _ = (root, at);
    Ok(())
}

/// Test seam: what [`listen`] does on each project root a test watches, in order, and the
/// addresses whose option call fails there (with [`seam::FAILURE`]).
#[cfg(test)]
pub(crate) mod seam {
    use std::collections::BTreeMap;
    use std::net::{IpAddr, SocketAddr};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    /// One step of a listener's socket.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Step {
        /// The platform option was set on the socket for this address.
        Configure(SocketAddr),
        /// The option call for this address failed (an injected failure).
        Refused(SocketAddr),
        /// The socket is about to bind this address.
        Bind(SocketAddr),
    }

    /// The error text of an injected option failure.
    pub const FAILURE: &str = "the socket option could not be set (test)";

    #[derive(Default)]
    struct Watched {
        steps: Vec<Step>,
        fail: Vec<IpAddr>,
    }

    static WATCHED: Mutex<BTreeMap<PathBuf, Watched>> = Mutex::new(BTreeMap::new());

    fn with<T>(f: impl FnOnce(&mut BTreeMap<PathBuf, Watched>) -> T) -> T {
        f(&mut WATCHED.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Stops watching its root when dropped.
    pub struct Watch(PathBuf);

    impl Drop for Watch {
        fn drop(&mut self) {
            with(|w| w.remove(&self.0));
        }
    }

    /// Watches the project at `root` (canonical) from now on.
    pub fn watch(root: &Path) -> Watch {
        with(|w| w.insert(root.to_path_buf(), Watched::default()));
        Watch(root.to_path_buf())
    }

    /// The option call fails for these addresses (any port) from now on.
    pub fn fail(root: &Path, ips: &[IpAddr]) {
        with(|w| {
            if let Some(x) = w.get_mut(root) {
                x.fail = ips.to_vec();
            }
        });
    }

    /// The steps recorded since the last call.
    pub fn take(root: &Path) -> Vec<Step> {
        with(|w| {
            w.get_mut(root)
                .map(|x| std::mem::take(&mut x.steps))
                .unwrap_or_default()
        })
    }

    pub(super) fn record(root: &Path, step: Step) {
        with(|w| {
            if let Some(x) = w.get_mut(root) {
                x.steps.push(step);
            }
        });
    }

    /// The injected failure for `at`, recorded as refused; `None` when its call goes on.
    pub(super) fn injected(root: &Path, at: SocketAddr) -> Option<std::io::Error> {
        let fails = with(|w| w.get(root).is_some_and(|x| x.fail.contains(&at.ip())));
        if !fails {
            return None;
        }
        record(root, Step::Refused(at));
        Some(std::io::Error::other(FAILURE))
    }
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
                let mut stream = stream;
                tokio::spawn(async move {
                    let _permit = permit;
                    let head =
                        tokio::time::timeout(BUSY_HEAD_TIMEOUT, read_head(&mut stream)).await;
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
#[derive(Debug)]
pub enum Head {
    /// The bytes up to and including the first empty line, or all the bytes the peer sent
    /// before it closed its side; and the bytes read past that line with it (a body's first
    /// bytes, or what a client pipelined), at most one read buffer's worth.
    Bytes { head: Vec<u8>, rest: Vec<u8> },
    /// Past [`MAX_HEAD_BYTES`] with no empty line.
    TooLarge,
    /// The peer closed with nothing sent, or the read failed.
    Closed,
}

/// Reads one request head off `reader`, in reads of at most 2048 bytes, and keeps the bytes
/// read past its empty line.
pub async fn read_head<R: AsyncRead + Unpin>(reader: &mut R) -> Head {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    loop {
        let n = match reader.read(&mut chunk).await {
            Ok(0) if buf.is_empty() => return Head::Closed,
            Ok(0) => {
                return Head::Bytes {
                    head: buf,
                    rest: Vec::new(),
                };
            }
            Ok(n) => n,
            Err(_) => return Head::Closed,
        };
        let from = buf.len().saturating_sub(3);
        buf.extend_from_slice(&chunk[..n]);
        if let Some(at) = buf[from..].windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buf.split_off(from + at + 4);
            return Head::Bytes { head: buf, rest };
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Head::TooLarge;
        }
    }
}

/// The body of a UI answer request `req` ([`ui_request::content_length`]): exactly its
/// `Content-Length` bytes, the bytes read with the head first (`prefetched`), then the rest off
/// `reader`, within the request's one read budget: `deadline`, [`HEAD_TIMEOUT`] after the
/// connection's start. `None` when the budget ends first: the connection then closes with no
/// reply, as for a slow head. A length past the limit is refused before anything is allocated
/// or read; bytes past the length are dropped, never read as another request.
pub async fn read_ui_body<R: AsyncRead + Unpin>(
    reader: &mut R,
    req: &Request,
    prefetched: Vec<u8>,
    deadline: Instant,
) -> Option<Result<Vec<u8>, Refusal>> {
    let len = match ui_request::content_length(req) {
        Ok(len) => len,
        Err(f) => return Some(Err(f)),
    };
    tokio::time::timeout_at(deadline, read_body(reader, prefetched, len))
        .await
        .ok()
}

/// `len` bytes (at most [`ui_request::MAX_UI_BODY`]): one allocation of `len`, filled from
/// `prefetched` and then by reads of at most 2048 bytes that never ask past `len`. The end of the
/// stream or a failed read before `len` is a truncated body: `Bad request.`
async fn read_body<R: AsyncRead + Unpin>(
    reader: &mut R,
    mut prefetched: Vec<u8>,
    len: usize,
) -> Result<Vec<u8>, Refusal> {
    if len > ui_request::MAX_UI_BODY {
        return Err(Refusal::UiBodyTooLarge);
    }
    let mut body = Vec::new();
    body.try_reserve_exact(len).map_err(|_| Refusal::Broken)?;
    prefetched.truncate(len);
    body.extend_from_slice(&prefetched);
    drop(prefetched);
    let mut chunk = [0u8; 2048];
    while body.len() < len {
        let want = (len - body.len()).min(chunk.len());
        match reader.read(&mut chunk[..want]).await {
            Ok(0) | Err(_) => return Err(Refusal::Malformed),
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    Ok(body)
}

async fn serve_conn(mut stream: TcpStream, ctx: Arc<Ctx>, sh: Arc<Shared>) {
    // the request's one read budget: the head now, and a UI answer's body after it
    let deadline = Instant::now() + HEAD_TIMEOUT;
    let head = match tokio::time::timeout_at(deadline, read_head(&mut stream)).await {
        Ok(head) => head,
        // no head in time: close with no reply
        Err(_) => return,
    };
    if gone(&ctx) {
        return;
    }
    let (bytes, rest) = match head {
        Head::Closed => return,
        Head::TooLarge => return send(stream, &refusal(Refusal::TooLarge), false).await,
        Head::Bytes { head, rest } => (head, rest),
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
    match ui_route(&ctx, &sh, &request, stream.local_addr().ok()).await {
        Err(f) => return send(stream, &refusal(f), head_only).await,
        Ok(None) => {}
        Ok(Some(_)) if !current => return send(stream, &refusal(Refusal::Busy), head_only).await,
        Ok(Some(route)) => {
            return serve_ui(stream, &ctx, &sh, route, &request, rest, deadline).await;
        }
    }
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

/// The UI route of `request` on the listener bound to `local`, after its checks; `None` when the
/// ordinary guard answers it. A path that names no UI route reads no config. `serve.ui` is read
/// afresh for one that does ([`ui::enabled`]); while it is off, that path is an ordinary path, so
/// the UI route's own checks do not apply.
async fn ui_route(
    ctx: &Ctx,
    sh: &Arc<Shared>,
    request: &Request,
    local: Option<SocketAddr>,
) -> Result<Option<ui_request::Route>, Refusal> {
    let Some(local) = local else {
        return Ok(None);
    };
    let full = *ctx.guard.mode() == ListenerMode::Full;
    let found = ui_request::classify(request, local, full, true);
    if matches!(found, Ok(None)) {
        return Ok(None);
    }
    let sh = Arc::clone(sh);
    match tokio::task::spawn_blocking(move || ui::enabled(&sh)).await {
        Ok(true) => found,
        Ok(false) | Err(_) => Ok(None),
    }
}

/// Answers a UI route, off the runtime for what reads or writes the project: an event stream, or
/// a response and the close. A body that comes too slowly closes with no reply.
async fn serve_ui(
    mut stream: TcpStream,
    ctx: &Ctx,
    sh: &Arc<Shared>,
    route: ui_request::Route,
    request: &Request,
    rest: Vec<u8>,
    deadline: Instant,
) {
    let head_only = request.method == Method::Head;
    let run = Arc::clone(sh);
    let done = match route {
        ui_request::Route::Events { rev } => {
            // as on the ordinary route: an event stream has no `HEAD`
            if head_only {
                return send(stream, &refusal(Refusal::Method), true).await;
            }
            let opened =
                tokio::task::spawn_blocking(move || run.site.subscribe_ui(&run, &rev)).await;
            let refused = match opened {
                Ok(Ok(sub)) => {
                    let hold = ctx.handle.hold();
                    let epoch = sse::epoch_now(Arc::clone(sh));
                    sse::serve(stream, sub, hold, ctx.handle.stopped(), epoch, now_secs()).await;
                    return;
                }
                Ok(Err(f)) => f,
                Err(_panicked) => Refusal::Broken,
            };
            return send(stream, &refusal(refused), false).await;
        }
        ui_request::Route::Answer => {
            let body = match read_ui_body(&mut stream, request, rest, deadline).await {
                None => return,
                Some(Ok(body)) => body,
                Some(Err(f)) => return send(stream, &refusal(f), false).await,
            };
            if gone(ctx) {
                return;
            }
            let form = match ui_request::parse_form(&body) {
                Ok(form) => form,
                Err(e) => return send(stream, &ui_request::form_refusal(&e), false).await,
            };
            drop(body);
            tokio::task::spawn_blocking(move || ui::Outcome::Done(ui::answer(&run, form))).await
        }
        ui_request::Route::Pending => {
            tokio::task::spawn_blocking(move || ui::Outcome::Done(ui::pending(&run))).await
        }
        ui_request::Route::Item(reference) => {
            tokio::task::spawn_blocking(move || ui::item(&run, &reference)).await
        }
    };
    let response = match done {
        Ok(ui::Outcome::Done(response)) => response,
        Ok(ui::Outcome::Missing) => {
            not_found(Arc::clone(sh), Miss::Page(listener_target(&ctx.guard))).await
        }
        Err(_panicked) => refusal(Refusal::Broken),
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
/// page, which has no live script, stream or revision. The page is the one the target's live
/// build made with its pages (ruling final, I1): the build the site keeps for the target as of
/// the latest publish, built first when it has none, so a miss on a kept build reads no source.
/// The redaction epoch is checked before the build is looked up and again before the bytes go
/// out, and the build must be of the store's epoch now, so a page of a state a removal made old
/// is never sent: the existing `Busy` stands. A target that does not build, or a page whose
/// layout could not be built (a build error or a panic), is the plain not found, never another
/// status. The versions menu, which has no page of its own to miss, gets canon's.
fn not_found_now(sh: &Shared, miss: Miss) -> Response {
    let (target, kind) = match miss {
        Miss::Plain => return refusal(Refusal::NotFound),
        Miss::Scope => (SiteTarget::Source(ScopeRef::Canon), NotFoundKind::Scope),
        Miss::Page(SiteTarget::Menu { .. } | SiteTarget::Source(ScopeRef::Versions)) => {
            (SiteTarget::Source(ScopeRef::Canon), NotFoundKind::Page)
        }
        Miss::Page(target) => (target, NotFoundKind::Page),
    };
    if sh.state.load().ensure_current_epoch().is_err() {
        return refusal(Refusal::Busy);
    }
    let built = catch_unwind(AssertUnwindSafe(|| sh.site.refresh(sh, &target)));
    let page = match built {
        Ok(Ok(build)) => build
            .live
            .not_found
            .get(kind)
            .map(|html| (build.live.epoch, html.to_vec())),
        Ok(Err(e)) if e.code == E_REDACTED_STATE => return refusal(Refusal::Busy),
        Ok(Err(_)) | Err(_) => None,
    };
    // a removal since the build: the page of the old state is not sent
    let st = sh.state.load();
    if st.ensure_current_epoch().is_err()
        || page
            .as_ref()
            .is_some_and(|(epoch, _)| *epoch != st.redaction_epoch())
    {
        return refusal(Refusal::Busy);
    }
    match page {
        Some((_, html)) => Response {
            status: 404,
            headers: vec![("Content-Type", content_type("404.html").to_string())],
            body: html,
        },
        None => refusal(Refusal::NotFound),
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

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_engine::{Rdy, Session, WriteRequest};
    use rdy_model::Role;
    use rdy_render::probe::preparations;
    use serde_json::{Value, json};
    use std::collections::BTreeMap;

    fn writer() -> Session {
        Session::human("human:qa".into(), Role::Writer).unwrap()
    }

    fn put(r: &mut Rdy, view: &str, ops: Value) -> BTreeMap<String, String> {
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
        .iter()
        .map(|(k, v)| (k.clone(), v.to_string()))
        .collect()
    }

    fn land(r: &mut Rdy, view: &str) {
        let p = r
            .propose(&writer(), view, view, Some("test summary"))
            .unwrap();
        let approver = Session::human("human:qa".into(), Role::Approver).unwrap();
        r.land(&approver, &p.id).unwrap();
    }

    /// Canon holds the doc `intro` and the org profile `guide`, which selects it; view `work`
    /// edits the doc.
    fn world() -> (tempfile::TempDir, Arc<Shared>) {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        let ids = put(
            &mut r,
            "w1",
            json!([
                {"op": "create", "type": "doc", "id": "$intro", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
                {"op": "create", "type": "prose", "id": "$ip", "parent": "$intro", "body": "INTRO-ONE"},
                {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
            ]),
        );
        land(&mut r, "w1");
        put(
            &mut r,
            "admin",
            json!([{"op": "create", "type": "publication_profile", "parent": ids["$admin"],
                "fields": {"key": "guide", "title": "Guide book", "description": "About it.",
                    "audience": "org", "select": [ids["$intro"]]}}]),
        );
        land(&mut r, "admin");
        put(
            &mut r,
            "work",
            json!([{"op": "update", "id": ids["$ip"], "body": "INTRO-VIEW"}]),
        );
        (dir, Shared::new(r))
    }

    /// Final review I1 (ruling final): a target's not-found page is built once, with its pages,
    /// in the live build the site already keeps for it. A miss on a target whose build is kept
    /// runs no preparation: no source or profile preparation, no setup and no history.
    #[test]
    fn dgm11_a_second_miss_on_one_entry_runs_no_preparation() {
        let (_dir, sh) = world();
        let canon = SiteTarget::Source(ScopeRef::Canon);
        let targets = [
            canon.clone(),
            SiteTarget::Source(ScopeRef::View("work".into())),
            SiteTarget::Profile {
                scope: ScopeRef::Canon,
                key: "guide".into(),
            },
            SiteTarget::Profile {
                scope: ScopeRef::View("work".into()),
                key: "guide".into(),
            },
            SiteTarget::ProfileOnly {
                key: "guide".into(),
            },
        ];
        let miss = |m: Miss| {
            let before = preparations();
            let r = not_found_now(&sh, m);
            (r, preparations() - before)
        };
        for target in &targets {
            let (first, _) = miss(Miss::Page(target.clone()));
            assert_eq!(first.status, 404, "{target:?}");
            assert!(
                String::from_utf8_lossy(&first.body).contains("data-rdy-kind=\"not-found\""),
                "{target:?}: the themed page"
            );
            for _ in 0..3 {
                let (again, prepared) = miss(Miss::Page(target.clone()));
                assert_eq!(
                    prepared, 0,
                    "{target:?}: a miss on a kept build prepares nothing"
                );
                assert_eq!(
                    (again.status, &again.body),
                    (404, &first.body),
                    "{target:?}"
                );
            }
        }
        // canon's Scope variant comes from canon's kept build too
        let (scope, prepared) = miss(Miss::Scope);
        assert_eq!(prepared, 0, "the Scope variant prepares nothing");
        assert_eq!(scope.status, 404);
        assert!(String::from_utf8_lossy(&scope.body).contains("is not on the site now"));
        // a page view builds the target once; its misses then prepare nothing
        let tag_less = SiteTarget::Source(ScopeRef::View("admin".into()));
        let before = preparations();
        sh.site.refresh(&sh, &tag_less).unwrap();
        assert_eq!(preparations() - before, 1, "the page's build");
        let (r, prepared) = miss(Miss::Page(tag_less));
        assert_eq!((r.status, prepared), (404, 0));
        sh.shutdown().unwrap();
    }

    /// Asserts that every bind attempt in `steps` follows its own address's option call at
    /// once, and that a refused option call is followed by no bind of its address.
    #[track_caller]
    fn configured_before_bind(steps: &[seam::Step]) {
        for (i, step) in steps.iter().enumerate() {
            match step {
                seam::Step::Bind(at) => assert_eq!(
                    i.checked_sub(1).map(|j| &steps[j]),
                    Some(&seam::Step::Configure(*at)),
                    "the bind of {at} follows its option call: {steps:?}"
                ),
                seam::Step::Refused(at) => assert_ne!(
                    steps.get(i + 1),
                    Some(&seam::Step::Bind(*at)),
                    "a refused option call binds nothing: {steps:?}"
                ),
                seam::Step::Configure(_) => {}
            }
        }
    }

    fn bound(steps: &[seam::Step]) -> Vec<SocketAddr> {
        steps
            .iter()
            .filter_map(|s| match s {
                seam::Step::Bind(at) => Some(*at),
                _ => None,
            })
            .collect()
    }

    /// D-FPKPS: the full listener's IPv4, IPv6 and LAN sockets and a profile-only listener's
    /// sockets all take the platform option before they bind; an option call that fails takes
    /// the existing listener failure path (the next port, the IPv6 log, the LAN warning, the
    /// profile's port exhaustion).
    #[test]
    fn ui4_every_site_listener_uses_the_platform_option() {
        let (dir, sh) = world();
        let root = dir.path().canonicalize().unwrap();
        // a private LAN address this machine does not have: its bind is attempted and fails
        let lan_ip = Ipv4Addr::new(10, 255, 255, 254);
        std::fs::write(
            root.join(".rdy/local.yaml"),
            format!("serve:\n  site_lan: true\n  site_lan_address: {lan_ip}\n"),
        )
        .unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let _watch = seam::watch(&root);
        let cfg = serve_config(&root);

        // the full site: 127.0.0.1, [::1] and the LAN address, each configured, then bound
        let o = {
            let _rt = rt.enter();
            open(&root, &cfg, &addr::candidates(None, &root), None).unwrap()
        };
        let steps = seam::take(&root);
        configured_before_bind(&steps);
        let port = o.port;
        let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
        let lan = SocketAddr::from((lan_ip, port));
        let binds = bound(&steps);
        for at in [v4, v6, lan] {
            assert!(
                binds.contains(&at),
                "{at} was bound after its option: {steps:?}"
            );
        }
        drop(o);

        // a profile-only listener: its port refused first (the existing exhaustion error), then
        // bound with the option on each address
        let daemon = crate::Daemon::new(
            crate::DaemonConfig::new(crate::RunDir::of_project(&root), "t".into()),
            Arc::new(crate::ServeHandler::new(Arc::clone(&sh), None)),
        );
        sh.profile_sites
            .init(rt.handle().clone(), daemon.handle(), root.clone(), None);
        seam::fail(&root, &[Ipv4Addr::LOCALHOST.into()]);
        let err = sh.profile_sites.ensure(&sh, "guide").unwrap_err();
        assert_eq!(err.code, rdy_engine::E_SITE);
        assert_eq!(
            err.message,
            format!(
                "could not bind a loopback port for profile \"guide\" after {} attempts",
                addr::TRIES
            )
        );
        let steps = seam::take(&root);
        configured_before_bind(&steps);
        assert!(bound(&steps).is_empty(), "{steps:?}");
        assert_eq!(
            steps.len(),
            usize::from(addr::TRIES),
            "one refused option call per port"
        );
        // the profile listener's own bind (`ensure` opens it with exactly this call); called
        // directly, so no profile-listener log line reaches another test's log capture
        seam::fail(&root, &[]);
        let p = {
            let _rt = rt.enter();
            open(&root, &cfg, &addr::profile_candidates(&root, "guide"), None).unwrap()
        };
        let steps = seam::take(&root);
        configured_before_bind(&steps);
        let only = p.port;
        assert_ne!(only, port);
        assert!(
            sh.profile_sites.keys().is_empty(),
            "the refusal kept no slot"
        );
        let binds = bound(&steps);
        for at in [
            SocketAddr::from((Ipv4Addr::LOCALHOST, only)),
            SocketAddr::from((Ipv6Addr::LOCALHOST, only)),
            SocketAddr::from((lan_ip, only)),
        ] {
            assert!(
                binds.contains(&at),
                "{at} was bound after its option: {steps:?}"
            );
        }
        drop(p);

        // a failed option call on [::1] and on the LAN address: the existing log and warning
        seam::fail(&root, &[Ipv6Addr::LOCALHOST.into(), lan_ip.into()]);
        let o = {
            let _rt = rt.enter();
            open(&root, &cfg, &addr::candidates(None, &root), None).unwrap()
        };
        let steps = seam::take(&root);
        configured_before_bind(&steps);
        assert_eq!(
            o.info.addrs,
            vec![SocketAddr::from((Ipv4Addr::LOCALHOST, o.port))]
        );
        let w = o.info.warning.clone().expect("the LAN warning");
        assert_eq!(
            w.message,
            format!(
                "could not listen on {lan_ip} ({}); the site stays on this machine",
                seam::FAILURE
            )
        );
        assert_eq!(w.fix, LAN_BIND_FIX);
        assert_eq!(o.info.lan_url, None);
        drop(o);
        // on 127.0.0.1 too: no loopback port, and the last error is the option's
        seam::fail(&root, &[Ipv4Addr::LOCALHOST.into()]);
        let failed = {
            let _rt = rt.enter();
            open(&root, &cfg, &addr::candidates(None, &root), None)
        };
        assert_eq!(failed.err(), Some(seam::FAILURE.to_string()));
        assert!(bound(&seam::take(&root)).is_empty());

        sh.shutdown().unwrap();
        for task in sh.profile_sites.close() {
            task.abort();
        }
        drop(rt);
    }

    /// Windows plan task 19: the profile-only listener is as exclusive as the full one.
    #[test]
    fn wx19_profile_listener_second_bind_fails() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".rdy")).unwrap();
        let root = dir.path().canonicalize().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _rt = rt.enter();
        let cfg = serve_config(&root);
        let p = open(&root, &cfg, &addr::profile_candidates(&root, "guide"), None).unwrap();
        for at in &p.info.addrs {
            for reuse in [false, true] {
                let socket = if at.is_ipv4() {
                    TcpSocket::new_v4().unwrap()
                } else {
                    TcpSocket::new_v6().unwrap()
                };
                if reuse {
                    socket.set_reuseaddr(true).unwrap();
                }
                assert!(
                    socket
                        .bind(*at)
                        .and_then(|()| socket.listen(8).map(drop))
                        .is_err(),
                    "{at} (reuseaddr {reuse})"
                );
            }
        }
    }
}
