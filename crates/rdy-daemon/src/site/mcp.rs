//! MCP over HTTP on the site's loopback port (D-6M0AF, D-44NVC, D-QG54Z; mht6).
//!
//! The route is exactly `/~mcp` on a full listener whose bound local address is loopback, while
//! `serve.mcp_http` is on and the project's token is usable. Anywhere else the request is an
//! ordinary site request and gets the listener's normal answer, with no MCP dispatch. The
//! listener comes from the connection's local address, never from a Host header.
//!
//! The site's own reader reads and bounds the head. This module then checks, in this order: the
//! Host (a loopback name with the listener's port), the Origin (any is refused), the bearer, the
//! method (`POST` only, D-44NVC), and the body framing and its bound (D-QG54Z). Only then is the
//! daemon's activity permit taken and the connection handed to a narrow hyper HTTP/1 bridge.
//! The bridge replays the head and the bytes read with it once, serves one request, and the
//! connection closes. The body reaches the shared admission layer
//! ([`rdy_mcp::http::HttpMcp`]) under a byte bound and a read deadline. An event stream holds
//! the connection and its permit until it ends. It ends at a stop, or when a check finds the
//! endpoint disabled.
use crate::daemon::{DaemonHandle, HttpWork};
use crate::shared::Shared;
use crate::site::guard::{Refusal, one_host, refusal};
use crate::site::http::{Method, Request, Response, text};
use crate::site::ui_request::UI_POST;
use bytes::Bytes;
use http_body_util::Limited;
use hyper::body::{Body, Frame, Incoming, SizeHint};
use rdy_engine::serve_config;
use rdy_mcp::http::{Activity, ActivityPermit, HttpMcp, ToListen};
use rdy_mcp::tools::MAX_REQUEST_BYTES;
use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio::time::Sleep;

/// The one MCP route, exactly: no query, no encoded spelling, no trailing slash.
pub const ROUTE: &str = "/~mcp";

/// How often an open event stream checks that `serve.mcp_http` is still on.
pub const CHECK_EVERY: Duration = Duration::from_secs(15);

/// How long a request body may take to arrive once its head is read. Past it the connection
/// closes with no reply, as for a slow head.
pub const BODY_TIMEOUT: Duration = Duration::from_secs(10);

/// The 401 body: a missing, wrong or malformed bearer.
const NO_TOKEN: &str = "this endpoint needs the project's MCP token; run rdy mcp --http-config";

/// The 403 body: any Origin header.
const ORIGIN_REFUSED: &str = "MCP over HTTP does not accept an Origin header.";

/// A future that completes when an open event stream must end.
type Until = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Whether `r` asks for the MCP route on a listener bound to `local`: a full listener (`full`)
/// on a loopback address, and exactly [`ROUTE`]. The config and the token are checked after.
pub fn is_route(r: &Request, local: SocketAddr, full: bool) -> bool {
    full && local.ip().is_loopback() && r.target == ROUTE
}

/// The Host check of the MCP route: exactly one Host, a loopback name with the listener's
/// `port`. The listener's LAN address is a page host, never an MCP host (C6).
pub fn host(r: &Request, port: u16) -> Result<(), Refusal> {
    one_host(r, port, None).map(|_| ())
}

/// Whether the endpoint serves now: the token is usable and `serve.mcp_http` is on, read afresh.
pub(crate) async fn enabled(sh: &Arc<Shared>) -> bool {
    if !sh.mcp_token.get().is_some_and(|t| t.is_ok()) {
        return false;
    }
    config_on(sh).await
}

/// `serve.mcp_http`, read afresh off the runtime.
async fn config_on(sh: &Arc<Shared>) -> bool {
    let sh = Arc::clone(sh);
    matches!(
        tokio::task::spawn_blocking(move || serve_config(&sh.root).mcp_http).await,
        Ok(true)
    )
}

/// Whether `r` carries exactly one `Authorization: Bearer <token>` with the project's token, and
/// no cookie. When the daemon has no token, nothing matches.
fn bearer(sh: &Shared, r: &Request) -> bool {
    let Some(Ok(token)) = sh.mcp_token.get().map(|t| &**t) else {
        return false;
    };
    if !r.all("cookie").is_empty() {
        return false;
    }
    match r.all("authorization").as_slice() {
        [one] => one
            .strip_prefix("Bearer ")
            .is_some_and(|t| token.matches(t.as_bytes())),
        _ => false,
    }
}

/// How a request's body is framed, once its framing is unambiguous.
#[derive(Debug, PartialEq, Eq)]
enum Framing {
    /// `Content-Length`, or no framing header (no body).
    Length,
    /// `Transfer-Encoding: chunked`, alone.
    Chunked,
}

/// Why a request's framing is refused.
#[derive(Debug, PartialEq, Eq)]
enum Unframed {
    /// Ambiguous framing or a control character in a header: `Bad request.`
    Malformed,
    /// A `Content-Length` over [`MAX_REQUEST_BYTES`]: 413 before any body byte is read.
    Over,
}

/// The body framing of `r`. No header value may hold a control character. Then either at most
/// one `Content-Length` of digits only, or exactly one `Transfer-Encoding: chunked` and no
/// length.
fn framing(r: &Request) -> Result<Framing, Unframed> {
    let control = |v: &str| v.bytes().any(|b| (b < 0x20 && b != b'\t') || b == 0x7f);
    if r.headers.iter().any(|(_, v)| control(v)) {
        return Err(Unframed::Malformed);
    }
    match (
        r.all("content-length").as_slice(),
        r.all("transfer-encoding").as_slice(),
    ) {
        ([], []) => Ok(Framing::Length),
        ([len], []) => {
            if len.is_empty() || !len.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Unframed::Malformed);
            }
            match len.parse::<u64>() {
                Ok(n) if n <= MAX_REQUEST_BYTES as u64 => Ok(Framing::Length),
                _ => Err(Unframed::Over),
            }
        }
        ([], [te]) if te.eq_ignore_ascii_case("chunked") => Ok(Framing::Chunked),
        _ => Err(Unframed::Malformed),
    }
}

/// The 413 answer, with D-QG54Z's text.
fn over() -> Response {
    text(
        413,
        &format!("the MCP request body is over {MAX_REQUEST_BYTES} bytes"),
    )
}

/// The checks of an MCP request after its route, in order: Host, Origin, bearer, method, body
/// framing. `Ok` when the request may enter.
fn check(sh: &Shared, r: &Request, port: u16) -> Result<(), Response> {
    host(r, port).map_err(refusal)?;
    if !r.all("origin").is_empty() {
        return Err(text(403, ORIGIN_REFUSED));
    }
    if !bearer(sh, r) {
        let mut no = text(401, NO_TOKEN);
        no.headers.push(("WWW-Authenticate", "Bearer".to_string()));
        return Err(no);
    }
    if !matches!(&r.method, Method::Other(m) if m == "POST") {
        return Err(refusal(Refusal::UiMethod { allow: UI_POST }));
    }
    framing(r).map(|_| ()).map_err(|e| match e {
        Unframed::Malformed => refusal(Refusal::Malformed),
        Unframed::Over => over(),
    })
}

/// What the site's server hands this module for one MCP request.
pub(crate) struct Conn {
    pub stream: TcpStream,
    /// The head's bytes.
    pub head: Vec<u8>,
    /// The bytes read past the head with it.
    pub rest: Vec<u8>,
    /// The listener's port.
    pub port: u16,
}

/// Serves one request on the MCP route: the checks, then the bridge. `mcp` is the daemon's
/// admission layer. The site's own writer answers a refusal.
pub(crate) async fn serve(
    conn: Conn,
    request: &Request,
    sh: Arc<Shared>,
    handle: DaemonHandle,
    mcp: &HttpMcp,
) {
    let head_only = request.method == Method::Head;
    if let Err(r) = check(&sh, request, conn.port) {
        return crate::site::server::send(conn.stream, &r, head_only).await;
    }
    // the activity permit only now, after the bearer; none once the daemon is leaving
    let admission = handle.http_admission();
    let Some(permit) = admission.admit(HttpWork::Call) else {
        return;
    };
    let to_listen: ToListen = Box::new(move || {
        admission
            .admit(HttpWork::Listen)
            .map(|p| Box::new(p) as ActivityPermit)
    });
    let activity = Activity::new(Box::new(permit), Some(to_listen));
    bridge(conn, sh, handle, mcp, activity).await;
}

/// The hyper HTTP/1 bridge for one request. The bytes already read are replayed once, the body
/// is bounded and timed, and the connection closes after the response (an event stream's when
/// it ends). A body that comes too slowly closes the connection with no reply.
async fn bridge(
    conn: Conn,
    sh: Arc<Shared>,
    handle: DaemonHandle,
    mcp: &HttpMcp,
    activity: Activity,
) {
    let Conn {
        stream,
        mut head,
        rest,
        ..
    } = conn;
    head.extend_from_slice(&rest);
    drop(rest);
    let io = hyper_util::rt::TokioIo::new(Replay {
        prefix: head,
        at: 0,
        stream,
        stalled: None,
    });
    let slow = Arc::new(Notify::new());
    let activity = Mutex::new(Some(activity));
    let service = hyper::service::service_fn(|req: http::Request<Incoming>| {
        let (mut parts, body) = req.into_parts();
        if let Some(a) = activity.lock().unwrap_or_else(|p| p.into_inner()).take() {
            parts.extensions.insert(a);
        }
        let body = Timed::new(body, Arc::clone(&slow), BODY_TIMEOUT);
        let body = Limited::new(body, MAX_REQUEST_BYTES);
        let (sh, handle) = (Arc::clone(&sh), handle.clone());
        async move {
            let response = mcp.handle(http::Request::from_parts(parts, body)).await;
            let events = response
                .headers()
                .get(http::header::CONTENT_TYPE)
                .is_some_and(|t| t.as_bytes().starts_with(b"text/event-stream"));
            let until = events.then(|| until(sh, handle));
            Ok::<_, Infallible>(response.map(|b| Ending::new(b, until)))
        }
    });
    let connection = hyper::server::conn::http1::Builder::new()
        .keep_alive(false)
        .header_read_timeout(None)
        .serve_connection(io, service)
        .without_shutdown();
    let parts = tokio::select! {
        done = connection => done.ok(),
        // the body did not arrive in time: close with no reply
        () = slow.notified() => None,
    };
    if let Some(parts) = parts {
        let Replay { mut stream, .. } = parts.io.into_inner();
        crate::site::server::close(&mut stream).await;
    }
}

/// Completes when an open event stream must end: the daemon stops, or a check finds
/// `serve.mcp_http` off. A check runs every [`CHECK_EVERY`], and at once on
/// [`Shared::recheck_mcp_http`].
fn until(sh: Arc<Shared>, handle: DaemonHandle) -> Until {
    Box::pin(async move {
        let mut stop = handle.stopped();
        let mut recheck = sh.mcp_recheck();
        let start = tokio::time::Instant::now() + CHECK_EVERY;
        let mut tick = tokio::time::interval_at(start, CHECK_EVERY);
        loop {
            let stopping = async {
                let _ = stop.wait_for(|stopped| *stopped).await;
            };
            tokio::select! {
                () = stopping => return,
                _ = tick.tick() => {}
                changed = recheck.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
            if !config_on(&sh).await {
                return;
            }
        }
    })
}

/// A response body that ends early when its `until` completes (an event stream at a stop or a
/// disabled check). It then drops the inner body, which cancels the stream's request.
struct Ending<B> {
    inner: Option<B>,
    until: Option<Until>,
}

impl<B> Ending<B> {
    fn new(inner: B, until: Option<Until>) -> Ending<B> {
        Ending {
            inner: Some(inner),
            until,
        }
    }
}

impl<B: Body<Data = Bytes, Error = Infallible> + Unpin> Body for Ending<B> {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        let this = &mut *self;
        if let Some(until) = this.until.as_mut()
            && until.as_mut().poll(cx).is_ready()
        {
            this.until = None;
            this.inner = None;
        }
        let Some(inner) = this.inner.as_mut() else {
            return Poll::Ready(None);
        };
        let frame = ready!(Pin::new(inner).poll_frame(cx));
        if frame.is_none() {
            this.inner = None;
        }
        Poll::Ready(frame)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.as_ref().is_none_or(|b| b.is_end_stream())
    }

    fn size_hint(&self) -> SizeHint {
        match (&self.inner, &self.until) {
            (Some(b), None) => b.size_hint(),
            (None, _) => SizeHint::with_exact(0),
            (Some(_), Some(_)) => SizeHint::default(),
        }
    }
}

/// A request body read within its time ([`BODY_TIMEOUT`] on the route). Past it, `slow` is told and the body never
/// yields again; the bridge then closes the connection.
struct Timed {
    inner: Incoming,
    deadline: Pin<Box<Sleep>>,
    slow: Arc<Notify>,
    done: bool,
}

impl Timed {
    fn new(inner: Incoming, slow: Arc<Notify>, within: Duration) -> Timed {
        Timed {
            inner,
            deadline: Box::pin(tokio::time::sleep(within)),
            slow,
            done: false,
        }
    }
}

impl Body for Timed {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, hyper::Error>>> {
        let this = &mut *self;
        if this.done {
            return Poll::Ready(None);
        }
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(None) => {
                this.done = true;
                Poll::Ready(None)
            }
            Poll::Ready(Some(frame)) => Poll::Ready(Some(frame)),
            Poll::Pending => {
                if this.deadline.as_mut().poll(cx).is_ready() {
                    this.slow.notify_one();
                }
                Poll::Pending
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.done || self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// The connection as the bridge reads it: the bytes the site already read, once, then the
/// socket. A write blocked for the site's send bound fails, so a stalled reader frees its
/// connection.
struct Replay {
    prefix: Vec<u8>,
    at: usize,
    stream: TcpStream,
    /// While a write waits, the end of its bound.
    stalled: Option<Pin<Box<Sleep>>>,
}

impl Replay {
    /// The send bound on a write that is `Pending`: an error once the bound passes, else
    /// `Pending`.
    fn wait<T>(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<T>> {
        let stalled = self
            .stalled
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(crate::site::server::SEND_TIMEOUT)));
        match stalled.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the reader took no bytes",
            ))),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncRead for Replay {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        if this.at < this.prefix.len() {
            let n = (this.prefix.len() - this.at).min(buf.remaining());
            buf.put_slice(&this.prefix[this.at..this.at + n]);
            this.at += n;
            if this.at == this.prefix.len() {
                this.prefix = Vec::new();
                this.at = 0;
            }
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for Replay {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        match Pin::new(&mut this.stream).poll_write(cx, buf) {
            Poll::Ready(r) => {
                this.stalled = None;
                Poll::Ready(r)
            }
            Poll::Pending => this.wait(cx),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = &mut *self;
        match Pin::new(&mut this.stream).poll_flush(cx) {
            Poll::Ready(r) => {
                this.stalled = None;
                Poll::Ready(r)
            }
            Poll::Pending => this.wait(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::guard::Guard;
    use crate::site::http::parse_head;
    use std::net::Ipv4Addr;

    fn req(text: &str) -> Request {
        parse_head(text.replace('\n', "\r\n").as_bytes()).unwrap()
    }

    /// C6: the full guard accepts its bound LAN address as a page host; the MCP route never.
    #[test]
    fn mht6_lan_host_is_a_page_host_and_no_mcp_host() {
        let lan = Ipv4Addr::new(192, 168, 1, 20);
        let r = req(&format!("GET / HTTP/1.1\nHost: {lan}:51000\n\n"));
        assert!(Guard::full(51000, Some(lan)).check(&r).is_ok());
        assert_eq!(host(&r, 51000), Err(Refusal::Host));
        for own in [
            "127.0.0.1:51000",
            "localhost:51000",
            "LOCALHOST:51000",
            "[::1]:51000",
        ] {
            let r = req(&format!("POST /~mcp HTTP/1.1\nHost: {own}\n\n"));
            assert_eq!(host(&r, 51000), Ok(()), "{own}");
        }
        let r = req("POST /~mcp HTTP/1.1\nHost: 127.0.0.1:51001\n\n");
        assert_eq!(host(&r, 51000), Err(Refusal::Host));
    }

    #[test]
    fn mht6_framing_is_one_unambiguous_form() {
        let of = |h: &str| framing(&req(&format!("POST /~mcp HTTP/1.1\n{h}\n")));
        assert_eq!(of(""), Ok(Framing::Length));
        assert_eq!(of("Content-Length: 0\n"), Ok(Framing::Length));
        assert_eq!(
            of(&format!("Content-Length: {MAX_REQUEST_BYTES}\n")),
            Ok(Framing::Length)
        );
        assert_eq!(
            of(&format!("Content-Length: {}\n", MAX_REQUEST_BYTES + 1)),
            Err(Unframed::Over)
        );
        assert_eq!(
            of("Content-Length: 99999999999999999999999\n"),
            Err(Unframed::Over)
        );
        assert_eq!(of("Transfer-Encoding: Chunked\n"), Ok(Framing::Chunked));
        for bad in [
            "Content-Length: 1\nContent-Length: 1\n",
            "Content-Length: 1\nTransfer-Encoding: chunked\n",
            "Content-Length: -1\n",
            "Content-Length: +1\n",
            "Content-Length: 1 \tx\n",
            "Transfer-Encoding: chunked, gzip\n",
            "Transfer-Encoding: gzip\n",
            "Transfer-Encoding: chunked\nTransfer-Encoding: chunked\n",
            "X-A: a\u{0}b\n",
            "X-A: a\rb\n",
        ] {
            assert_eq!(of(bad), Err(Unframed::Malformed), "{bad:?}");
        }
    }

    /// The body deadline: an unfinished body tells the bridge at its deadline and not before.
    #[tokio::test]
    async fn mht6_an_unfinished_body_closes_at_its_deadline() {
        const WITHIN: Duration = Duration::from_millis(300);
        let (mut client, server) = tokio::io::duplex(4096);
        let slow = Arc::new(Notify::new());
        let told = Arc::clone(&slow);
        let served = tokio::spawn(async move {
            let svc = hyper::service::service_fn(move |req: http::Request<Incoming>| {
                let slow = Arc::clone(&slow);
                async move {
                    let body = Timed::new(req.into_body(), slow, WITHIN);
                    let _ = http_body_util::BodyExt::collect(body).await;
                    Ok::<_, Infallible>(http::Response::new(http_body_util::Empty::<Bytes>::new()))
                }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .keep_alive(false)
                .header_read_timeout(None)
                .serve_connection(hyper_util::rt::TokioIo::new(server), svc)
                .await;
        });
        tokio::io::AsyncWriteExt::write_all(
            &mut client,
            b"POST /~mcp HTTP/1.1\r\nHost: x\r\nContent-Length: 10\r\n\r\nabc",
        )
        .await
        .unwrap();
        let notified = told.notified();
        tokio::pin!(notified);
        let start = tokio::time::Instant::now();
        tokio::time::sleep(WITHIN / 2).await;
        assert!(
            poll_once(notified.as_mut()).is_pending(),
            "not before the deadline"
        );
        tokio::time::timeout(WITHIN * 10, notified)
            .await
            .expect("told at the deadline");
        assert!(start.elapsed() >= WITHIN);
        served.abort();
    }

    /// One poll of `f`, with a waker that does nothing.
    fn poll_once<F: Future + ?Sized>(f: Pin<&mut F>) -> Poll<F::Output> {
        let mut cx = Context::from_waker(std::task::Waker::noop());
        f.poll(&mut cx)
    }
}
