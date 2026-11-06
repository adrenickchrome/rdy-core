//! The daemon's listener and stream through the `sys` seam, over a real endpoint: a named pipe on
//! Windows, a Unix socket elsewhere. The hello, the ctl loop and the MCP pump here are the
//! daemon's own, and only the seam's exports reach the endpoint. The Windows crate does not build
//! until every `sys` part exists, so an isolated probe runs this file there until then.

use rdy_daemon::ctl::{CtlRequest, DaemonInfo, serve_ctl};
use rdy_daemon::error::ErrorReply;
use rdy_daemon::handler::{Handler, McpStream};
use rdy_daemon::hello::{Hello, MAX_HELLO_BYTES, PROTO_VERSION, read_hello};
use rdy_daemon::session::{Proto, SessionInfo, Sessions};
use rdy_daemon::signing_transport::{HostMcp, host_mcp};
use rdy_daemon::sys::{self, Liveness};
use rdy_engine::signing_crypto::SigningAgent;
use rdy_store::fileid::FileId;
use serde_json::{Value, json};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// A hung test fails here. It asserts no timing: every wait below ends on an event.
const HANG_LIMIT: Duration = Duration::from_secs(120);

async fn guarded<T>(test: impl Future<Output = T>) -> T {
    tokio::time::timeout(HANG_LIMIT, test)
        .await
        .expect("the test hung")
}

/// One endpoint name, unused by any other test or process.
struct Endpoint {
    path: PathBuf,
    /// Holds a Unix socket's directory.
    _dir: Option<tempfile::TempDir>,
}

static NEXT_ENDPOINT: AtomicUsize = AtomicUsize::new(0);

#[cfg(windows)]
fn endpoint() -> Endpoint {
    let n = NEXT_ENDPOINT.fetch_add(1, Ordering::SeqCst);
    Endpoint {
        path: PathBuf::from(format!(
            r"\\.\pipe\rdy-wx13-test-{}-{n}",
            std::process::id()
        )),
        _dir: None,
    }
}

#[cfg(not(windows))]
fn endpoint() -> Endpoint {
    NEXT_ENDPOINT.fetch_add(1, Ordering::SeqCst);
    // under /tmp, so the path fits `sun_path` on macOS
    let dir = tempfile::Builder::new()
        .prefix("rdy-wx13")
        .tempdir_in("/tmp")
        .unwrap();
    Endpoint {
        path: dir.path().join("s.sock"),
        _dir: Some(dir),
    }
}

/// Frees the name of a dropped listener: a named pipe's name ends with its last instance, and a
/// socket's file stays until it is removed.
async fn wait_until_free(path: &Path) {
    #[cfg(not(windows))]
    std::fs::remove_file(path).unwrap();
    while sys::endpoint_liveness(path).await != Liveness::Gone {
        tokio::task::yield_now().await;
    }
}

fn info() -> DaemonInfo {
    DaemonInfo {
        pid: std::process::id(),
        version: "t".into(),
        build: rdy_daemon::BUILD.into(),
        started_at: Instant::now(),
        sessions: Sessions::default(),
        connections: Arc::new(AtomicUsize::new(1)),
        stop: Arc::new(tokio::sync::watch::channel(false).0),
    }
}

struct Echo;

impl Handler for Echo {
    fn ctl(
        &self,
        session: &SessionInfo,
        request: CtlRequest,
        _agent: Arc<dyn SigningAgent>,
    ) -> Result<Value, ErrorReply> {
        Ok(json!({"session": session.id, "args": request.args}))
    }

    fn mcp(
        &self,
        _session: SessionInfo,
        stream: McpStream,
        _agent: Arc<dyn SigningAgent>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            let (mut r, mut w) = tokio::io::split(stream);
            tokio::io::copy(&mut r, &mut w).await.unwrap();
            w.shutdown().await.unwrap();
        })
    }
}

fn hello_line(proto: Proto) -> Vec<u8> {
    let hello = Hello {
        rdy: PROTO_VERSION,
        version: "t".into(),
        build: rdy_daemon::BUILD.into(),
        proto,
        cwd: std::env::temp_dir(),
        project: FileId::lstat(&std::env::temp_dir()).unwrap(),
    };
    let mut line = serde_json::to_vec(&hello).unwrap();
    line.push(b'\n');
    line
}

/// One connection, as the daemon's accept loop serves it after its peer check.
async fn serve_connection(mut stream: sys::Stream, info: DaemonInfo) {
    let hello = read_hello(&mut stream).await.unwrap();
    let session = info
        .sessions
        .insert(hello.proto, hello.version, hello.build, hello.cwd, 0);
    let handler: Arc<dyn Handler> = Arc::new(Echo);
    match hello.proto {
        Proto::Ctl => {
            let (r, w) = sys::split(stream);
            serve_ctl(r, w, session, handler, info).await.unwrap();
        }
        Proto::Mcp => {
            let HostMcp {
                stream,
                agent,
                pump,
            } = host_mcp(stream);
            let service = tokio::spawn(handler.mcp(session, stream, agent));
            pump.await;
            service.await.unwrap();
        }
    }
}

/// A request line over a ctl connection and its reply.
async fn ctl_round_trip(path: PathBuf, tag: usize) -> Value {
    let mut stream = sys::connect(&path).await.unwrap();
    stream.write_all(&hello_line(Proto::Ctl)).await.unwrap();
    // over the pipe's own buffers, so both ends wait for each other
    let payload = "x".repeat(256 * 1024);
    let line = format!(
        "{}\n",
        json!({"id": tag, "op": "echo", "args": {"tag": tag, "payload": payload}})
    );
    let (r, mut w) = sys::split(stream);
    let mut r = BufReader::new(r);
    let write = async {
        w.write_all(line.as_bytes()).await.unwrap();
        w.flush().await.unwrap();
    };
    let mut reply = String::new();
    let read = r.read_line(&mut reply);
    let ((), read) = tokio::join!(write, read);
    read.unwrap();
    let reply: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["ok"]["args"]["payload"], json!(payload));
    assert_eq!(reply["id"], json!(tag));
    reply
}

/// Every byte value but the newline, on lines, over the pipe's own buffers.
fn mcp_payload() -> Vec<u8> {
    let line: Vec<u8> = (0..=u8::MAX)
        .rev()
        .filter(|b| *b != b'\n')
        .chain(b"\n".iter().copied())
        .collect();
    line.repeat(1024)
}

/// The payload through an MCP session. The client's half-close ends the session, and the session's
/// bytes still reach the client after it.
async fn mcp_round_trip(path: PathBuf) {
    let mut stream = sys::connect(&path).await.unwrap();
    stream.write_all(&hello_line(Proto::Mcp)).await.unwrap();
    let (mut r, mut w) = sys::split(stream);
    let payload = mcp_payload();
    let write = async {
        w.write_all(&payload).await.unwrap();
        w.shutdown().await.unwrap();
    };
    let mut echoed = Vec::new();
    let read = r.read_to_end(&mut echoed);
    let ((), read) = tokio::join!(write, read);
    read.unwrap();
    assert!(echoed == payload, "the session's bytes came back changed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wx13_pipe_carries_ctl_and_duplex_mcp_bytes() {
    guarded(async {
        let e = endpoint();
        let mut listener = sys::bind(&e.path).unwrap();
        let info = info();
        let served = info.clone();
        let accepting = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let stream = listener.accept().await.unwrap();
                connections.spawn(serve_connection(stream, served.clone()));
            }
        });

        // simultaneous clients of both protocols
        let mut clients = tokio::task::JoinSet::new();
        for tag in 0..8 {
            let path = e.path.clone();
            clients.spawn(async move {
                ctl_round_trip(path, tag).await;
            });
            let path = e.path.clone();
            clients.spawn(mcp_round_trip(path));
        }
        while let Some(done) = clients.join_next().await {
            done.unwrap();
        }

        // a ctl connection the daemon's stop closes: the client reads an end of file
        let mut stream = sys::connect(&e.path).await.unwrap();
        stream.write_all(&hello_line(Proto::Ctl)).await.unwrap();
        let mut stream = BufReader::new(stream);
        stream
            .get_mut()
            .write_all(b"{\"id\":1,\"op\":\"echo\"}\n")
            .await
            .unwrap();
        let mut reply = String::new();
        stream.read_line(&mut reply).await.unwrap();
        assert!(reply.contains("\"ok\""), "{reply}");
        info.stop.send_replace(true);
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty());
        accepting.abort();
    })
    .await;
}

#[tokio::test]
async fn wx13_the_hello_bound_holds_over_the_endpoint() {
    guarded(async {
        let e = endpoint();
        let mut listener = sys::bind(&e.path).unwrap();
        let client = sys::connect(&e.path).await.unwrap();
        let mut server = listener.accept().await.unwrap();
        let (_cr, mut cw) = sys::split(client);
        let write = async {
            // its bound in bytes, no newline, then bytes it must leave unread
            cw.write_all(&vec![b'x'; MAX_HELLO_BYTES + 16])
                .await
                .unwrap();
            cw.flush().await.unwrap();
        };
        let (refused, ()) = tokio::join!(read_hello(&mut server), write);
        assert_eq!(
            refused.unwrap_err().message,
            format!("the hello line is over {MAX_HELLO_BYTES} bytes")
        );
        // not one byte past the bound was consumed
        let mut rest = [0u8; 16];
        server.read_exact(&mut rest).await.unwrap();
        assert_eq!(rest, [b'x'; 16]);

        // a hello read one byte at a time leaves what follows it, whatever its bytes
        let client = sys::connect(&e.path).await.unwrap();
        let mut server = listener.accept().await.unwrap();
        let (_cr, mut cw) = sys::split(client);
        let mut line = hello_line(Proto::Mcp);
        let following = b"\xff\x00\xff\x01\xfe\n";
        line.extend_from_slice(following);
        cw.write_all(&line).await.unwrap();
        cw.flush().await.unwrap();
        assert_eq!(read_hello(&mut server).await.unwrap().proto, Proto::Mcp);
        let mut rest = vec![0u8; following.len()];
        server.read_exact(&mut rest).await.unwrap();
        assert_eq!(&rest, following);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wx13_only_the_first_listener_claims_the_pipe() {
    guarded(async {
        let e = endpoint();
        let first = sys::bind(&e.path).unwrap();
        let refused = sys::bind(&e.path).unwrap_err();
        #[cfg(windows)]
        {
            assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(
                refused.to_string(),
                format!(
                    "could not create named pipe {}: Access is denied. (os error 5)",
                    e.path.display()
                )
            );
        }
        #[cfg(not(windows))]
        assert_eq!(refused.kind(), io::ErrorKind::AddrInUse);
        // the loser took nothing from the winner
        assert_eq!(sys::endpoint_liveness(&e.path).await, Liveness::Live);
        drop(first);
        wait_until_free(&e.path).await;

        // racing listeners: exactly one claims the name, and it can then be claimed again
        for _ in 0..4 {
            const RACERS: usize = 8;
            let barrier = Arc::new(Barrier::new(RACERS));
            let racers: Vec<_> = (0..RACERS)
                .map(|_| {
                    let (barrier, path) = (Arc::clone(&barrier), e.path.clone());
                    let runtime = tokio::runtime::Handle::current();
                    std::thread::spawn(move || {
                        let _in_runtime = runtime.enter();
                        barrier.wait();
                        sys::bind(&path)
                    })
                })
                .collect();
            let bound: Vec<io::Result<sys::Listener>> =
                racers.into_iter().map(|r| r.join().unwrap()).collect();
            assert_eq!(bound.iter().filter(|b| b.is_ok()).count(), 1, "{bound:?}");
            drop(bound);
            wait_until_free(&e.path).await;
        }
    })
    .await;
}

#[tokio::test]
async fn wx13_accept_handoff_keeps_the_namespace_owned() {
    guarded(async {
        let e = endpoint();
        let mut listener = sys::bind(&e.path).unwrap();
        let mut kept = Vec::new();
        for round in 0u8..4 {
            // no accept is running: an instance is there for the connect all the same
            let client = sys::connect(&e.path).await.unwrap();
            let server = listener.accept().await.unwrap();
            // the handoff has happened, and no next accept has started: the name stays ours
            assert!(sys::bind(&e.path).is_err(), "round {round}");
            assert_eq!(sys::endpoint_liveness(&e.path).await, Liveness::Live);
            // the probe's connection arrives like any other, and has already ended
            let mut probe = listener.accept().await.unwrap();
            let mut byte = [0u8; 1];
            assert_eq!(probe.read(&mut byte).await.unwrap(), 0, "round {round}");
            drop(probe);
            kept.push((round, client, server));
            // a reconnect after a client of an earlier round left
            if round == 2 {
                kept.remove(0);
            }
        }
        // the connections of earlier rounds work on, each with its own instance
        for (round, client, server) in kept {
            let (mut cr, mut cw) = sys::split(client);
            let (mut sr, mut sw) = sys::split(server);
            cw.write_all(&[round]).await.unwrap();
            cw.flush().await.unwrap();
            sw.write_all(&[round, 0xff]).await.unwrap();
            sw.flush().await.unwrap();
            let mut got = [0u8; 1];
            sr.read_exact(&mut got).await.unwrap();
            assert_eq!(got, [round]);
            let mut got = [0u8; 2];
            cr.read_exact(&mut got).await.unwrap();
            assert_eq!(got, [round, 0xff]);
        }
        assert!(sys::bind(&e.path).is_err(), "the listener still holds it");
        drop(listener);
        wait_until_free(&e.path).await;
        drop(sys::bind(&e.path).unwrap());
    })
    .await;
}

/// Polls `future` until its first `Pending`, and says whether it got that far.
async fn poll_once<F: Future>(future: Pin<&mut F>) -> bool {
    let mut future = future;
    std::future::poll_fn(|cx| std::task::Poll::Ready(future.as_mut().poll(cx).is_pending())).await
}

#[tokio::test]
async fn wx13_cancelled_connect_and_broken_pipe_release_handles() {
    guarded(async {
        let e = endpoint();
        let mut listener = sys::bind(&e.path).unwrap();

        // a cancelled accept loses no connection
        for round in 0u8..3 {
            {
                let accept = listener.accept();
                tokio::pin!(accept);
                assert!(poll_once(accept).await, "no client yet");
            }
            let client = sys::connect(&e.path).await.unwrap();
            let (_cr, mut cw) = sys::split(client);
            cw.write_all(&[round]).await.unwrap();
            cw.flush().await.unwrap();
            let mut server = listener.accept().await.unwrap();
            let mut got = [0u8; 1];
            server.read_exact(&mut got).await.unwrap();
            assert_eq!(got, [round]);
        }

        // a cancelled connect leaves nothing: it never appears as a connection
        let holding = sys::connect(&e.path).await.unwrap();
        #[cfg(windows)]
        {
            // the one waiting instance is taken, so this connect waits for the next
            let connect = sys::connect(&e.path);
            tokio::pin!(connect);
            assert!(poll_once(connect).await, "a connect to a busy pipe waits");
        }
        let mut server = listener.accept().await.unwrap();
        let (_cr, mut cw) = sys::split(holding);
        cw.write_all(b"h").await.unwrap();
        cw.flush().await.unwrap();
        let mut got = [0u8; 1];
        server.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"h");
        {
            let next = listener.accept();
            tokio::pin!(next);
            assert!(poll_once(next).await, "the cancelled connect left a client");
        }

        // a broken pipe, each way: the read ends, and a write fails
        drop((_cr, cw));
        let mut byte = [0u8; 1];
        assert_eq!(server.read(&mut byte).await.unwrap(), 0, "the client left");
        drop(server);
        let client = sys::connect(&e.path).await.unwrap();
        let server = listener.accept().await.unwrap();
        // what the server wrote before it left is still read
        let (sr, mut sw) = sys::split(server);
        sw.write_all(b"last").await.unwrap();
        sw.flush().await.unwrap();
        drop((sr, sw));
        let (mut cr, mut cw) = sys::split(client);
        let mut rest = Vec::new();
        cr.read_to_end(&mut rest).await.unwrap();
        assert_eq!(rest, b"last");
        let failed = match cw.write_all(b"x").await {
            Ok(()) => cw.flush().await,
            failed => failed,
        };
        assert_eq!(failed.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        drop((cr, cw));

        // a task with a read in progress, aborted: its handle closes, so the peer reads the end
        let client = sys::connect(&e.path).await.unwrap();
        let mut server = listener.accept().await.unwrap();
        let reading = tokio::spawn(async move {
            let mut client = client;
            let mut byte = [0u8; 1];
            client.read(&mut byte).await
        });
        tokio::task::yield_now().await;
        reading.abort();
        assert!(reading.await.unwrap_err().is_cancelled());
        assert_eq!(
            server.read(&mut byte).await.unwrap(),
            0,
            "the handle closed"
        );
        drop(server);

        // a connection made in a runtime that ends with it, as `client::peer_pid` makes one
        let path = e.path.clone();
        let made = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async { sys::connect(&path).await.map(drop) })
        });
        let mut server = listener.accept().await.unwrap();
        made.join().unwrap().unwrap();
        assert_eq!(
            server.read(&mut byte).await.unwrap(),
            0,
            "the handle closed"
        );
        drop(server);

        // a shutdown: with the last handle gone, the name is free and can be claimed again
        drop(listener);
        wait_until_free(&e.path).await;
        assert_eq!(sys::endpoint_liveness(&e.path).await, Liveness::Gone);
        drop(sys::bind(&e.path).unwrap());
    })
    .await;
}
