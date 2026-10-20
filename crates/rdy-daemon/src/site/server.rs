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
use crate::site::guard::{Guard, Refusal, Route, SiteTarget, refusal};
use crate::site::http::{
    MAX_HEAD_BYTES, Method, ParseError, Response, content_type, parse_head, to_bytes,
};
use crate::site::live::{label, resolve};
use crate::site::{LanOverride, SiteInfo, addr, sse};
use rdy_engine::engine::{SITE_LAN_FIX, SiteWarning};
use rdy_engine::publication::{IMPLICIT_ORG, profile_at};
use rdy_engine::serve_config;
use rdy_engine::{E_NOT_FOUND, E_REDACTED_STATE, ReadState};
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
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

/// The connections the site serves at once; past that, a connection gets `Busy`.
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

/// What every connection of the site shares.
struct Ctx {
    guard: Guard,
    shared: Arc<Shared>,
    handle: DaemonHandle,
    limit: Arc<Semaphore>,
    /// The tasks that answer `Busy` past `limit`.
    busy: Arc<Semaphore>,
}

/// A listening socket on `at`, with `SO_REUSEADDR` so a successor binds the port at once.
fn listen(at: SocketAddr) -> io::Result<TcpListener> {
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
        let mut last = None;
        let mut bound = None;
        for port in addr::candidates(cfg.site_port, root) {
            match listen(SocketAddr::from((Ipv4Addr::LOCALHOST, port))) {
                Ok(l) => {
                    bound = Some((port, l));
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        let Some((port, v4)) = bound else {
            let reason = last.map_or_else(|| "no port to try".to_string(), |e| e.to_string());
            let info = SiteInfo {
                url: Err(reason),
                lan_url: None,
                warning: None,
                addrs: Vec::new(),
            };
            return (None, info);
        };
        let mut addrs = vec![SocketAddr::from((Ipv4Addr::LOCALHOST, port))];
        let mut listeners = vec![v4];
        let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
        match listen(v6) {
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
                        listen(at).map(|l| {
                            listeners.push(l);
                            addrs.push(at);
                        })
                    };
                    match out {
                        Ok(()) => {
                            let url = addr::url(&ip.to_string(), port);
                            warning = Some(SiteWarning {
                                message: format!(
                                    "anyone on your network can read these docs at {url}"
                                ),
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
        let server = SiteServer {
            listeners,
            guard: Guard { port, lan },
        };
        (Some(server), info)
    }

    /// Accepts on every bound address until the daemon stops (`handle.stopped()`), and serves
    /// each connection on a task of its own.
    pub async fn run(self, shared: Arc<Shared>, handle: DaemonHandle) {
        let ctx = Arc::new(Ctx {
            guard: self.guard,
            shared,
            handle,
            limit: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
            busy: Arc::new(Semaphore::new(BUSY_RESPONDERS)),
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
    match Arc::clone(&ctx.limit).try_acquire_owned() {
        Ok(permit) => {
            let ctx = Arc::clone(ctx);
            tokio::spawn(async move {
                let _permit = permit;
                serve_conn(stream, ctx).await;
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

async fn serve_conn(stream: TcpStream, ctx: Arc<Ctx>) {
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
    let state = ctx.shared.state.load_full();
    let current = state.ensure_current_epoch().is_ok();
    drop(state);
    let response = match ctx.guard.check(&request) {
        Err(f) => refusal(f),
        Ok(_) if !current => refusal(Refusal::Busy),
        Ok(Route::LiveJs) => Response {
            status: 200,
            headers: vec![("Content-Type", content_type("live.js").to_string())],
            body: rdy_render::live::LIVE_JS.as_bytes().to_vec(),
        },
        Ok(Route::Page { target, path }) => page(&ctx, target, path).await,
        Ok(Route::Events {
            target,
            page,
            rev,
            label,
        }) => {
            let sh = Arc::clone(&ctx.shared);
            let sub = tokio::task::spawn_blocking(move || {
                sh.site.subscribe(&sh, &target, &page, &rev, &label)
            })
            .await;
            match sub {
                Ok(Ok(sub)) => {
                    // the hold before the head: once the reader has the head, the page (in
                    // view: live.js closes the stream of a hidden page) holds the daemon
                    let hold = ctx.handle.hold();
                    let epoch = sse::epoch_now(Arc::clone(&ctx.shared));
                    sse::serve(stream, sub, hold, ctx.handle.stopped(), epoch, now_secs()).await;
                    return;
                }
                Ok(Err(_busy)) => refusal(Refusal::Busy),
                Err(_panicked) => refusal(Refusal::Broken),
            }
        }
    };
    send(stream, &response, head_only).await;
}

/// A page or file of `target`: the config check on an HTML page, then the target's build as of
/// the latest publish, then the bytes the server sends for `path`.
async fn page(ctx: &Ctx, target: SiteTarget, path: String) -> Response {
    let sh = Arc::clone(&ctx.shared);
    let built = tokio::task::spawn_blocking(move || {
        // a page load reloads a changed config, as a new connection does (`session_started`)
        if path.ends_with(".html") && sh.watch.changed() {
            let _ = sh.writer.reload();
        }
        let build = match sh.site.refresh(&sh, &target) {
            Ok(b) => b,
            // a removal since the entry check: the build of the old state was discarded
            Err(e) if e.code == E_REDACTED_STATE => return Err(Refusal::Busy),
            // a scope that does not resolve names no view or proposal, and a profile its scope
            // does not hold has no pages: not found, the plain answer (the menu of canon always
            // resolves, and a failed build of it is broken, not missing)
            Err(_) if missing(&sh.state.load_full(), &target) => return Err(Refusal::NotFound),
            Err(e) => {
                log(&format!(
                    "site: the {} pages did not build ({e})",
                    label(&target)
                ));
                return Err(Refusal::Broken);
            }
        };
        rdy_render::live::page(&build.live, &path, &target)
            .map(|body| (content_type(&path), body))
            .ok_or(Refusal::NotFound)
    })
    .await;
    match built {
        Ok(Ok((kind, body))) => Response {
            status: 200,
            headers: vec![("Content-Type", kind.to_string())],
            body,
        },
        Ok(Err(f)) => refusal(f),
        Err(_panicked) => refusal(Refusal::Broken),
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
