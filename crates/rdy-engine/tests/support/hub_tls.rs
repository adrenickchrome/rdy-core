//! A loopback TLS hub for the transport tests: a generated CA, a listener on a generated
//! loopback address, a log of the raw requests it read and the replies a test queues for it.
//!
//! Nothing here sleeps: a test queues every reply before it calls the transport, the fixture
//! thread serves exactly the connections `accept_n` names, and `finish` joins it and proves that
//! no further connection is waiting.
//!
//! By default the fixture closes each connection after its one reply (close_notify, then a
//! socket shutdown). [`Server::keep_alive`] instead holds every connection open after a reply and
//! serves each further request on it until the client closes it, so a client that reused a
//! connection would be seen.
#![allow(dead_code)]

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use std::collections::BTreeSet;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::JoinHandle;

/// The loopback address every fixture listens on.
pub const LOOPBACK: &str = "127.0.0.1";

/// A generated certificate authority.
pub struct Ca {
    pem: String,
    issuer: Issuer<'static, KeyPair>,
}

impl Ca {
    pub fn new() -> Ca {
        let key = KeyPair::generate().expect("CA key");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params
            .distinguished_name
            .push(DnType::CommonName, "rdy transport test CA");
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let cert = params.self_signed(&key).expect("CA certificate");
        Ca {
            pem: cert.pem(),
            issuer: Issuer::new(params, key),
        }
    }

    /// The CA certificate in PEM.
    pub fn pem(&self) -> &str {
        &self.pem
    }

    /// A server certificate for `names` (IP addresses or names), signed by this CA, and its key.
    pub fn leaf(&self, names: &[&str]) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
        let key = KeyPair::generate().expect("leaf key");
        let mut params =
            CertificateParams::new(names.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .expect("leaf params");
        params.is_ca = IsCa::NoCa;
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params
            .distinguished_name
            .push(DnType::CommonName, "rdy transport test server");
        let cert = params
            .signed_by(&key, &self.issuer)
            .expect("leaf certificate");
        (
            vec![cert.der().clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        )
    }
}

/// What the fixture does with the next request it reads.
pub enum Reply {
    /// Writes these bytes, then closes the connection cleanly.
    Raw(Vec<u8>),
    /// Closes the connection without a reply: a lost response.
    Close,
}

impl Reply {
    /// A JSON reply with `status`.
    pub fn json(status: u16, body: &str) -> Reply {
        Reply::Raw(
            format!(
                "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                reason(status),
                body.len()
            )
            .into_bytes(),
        )
    }

    /// A reply with `status`, the extra header lines `headers` and the raw `body`.
    pub fn with_headers(status: u16, headers: &[String], body: &[u8]) -> Reply {
        let mut out = format!("HTTP/1.1 {status} {}\r\n", reason(status)).into_bytes();
        for h in headers {
            out.extend_from_slice(h.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(body);
        Reply::Raw(out)
    }

    /// An empty reply with `status`.
    pub fn empty(status: u16) -> Reply {
        Reply::json(status, "")
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        _ => "Status",
    }
}

/// One request the fixture read, as raw bytes and in parts.
#[derive(Clone, Debug)]
pub struct Recorded {
    /// The connection it came on, counted from 0.
    pub conn: usize,
    /// The request line and header lines exactly as read, without the blank line.
    pub head: String,
    pub method: String,
    pub target: String,
    /// Header names in lower case, in order.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn header_names(&self) -> Vec<&str> {
        self.headers.iter().map(|(n, _)| n.as_str()).collect()
    }

    pub fn body_text(&self) -> &str {
        std::str::from_utf8(&self.body).expect("UTF-8 body")
    }
}

struct Shared {
    log: Mutex<Vec<Recorded>>,
    replies: Mutex<mpsc::Receiver<Reply>>,
    config: Arc<ServerConfig>,
    /// Hold each connection open after a reply and serve further requests on it.
    keep_alive: bool,
    /// The connections accepted so far.
    accepted: AtomicUsize,
    /// The connections that have ended: the client closed it, or the fixture did.
    closed: Mutex<BTreeSet<usize>>,
    closed_changed: Condvar,
}

impl Shared {
    fn mark_closed(&self, conn: usize) {
        self.closed.lock().expect("closed").insert(conn);
        self.closed_changed.notify_all();
    }
}

/// A loopback TLS hub.
pub struct Server {
    listener: Arc<TcpListener>,
    origin: String,
    ca: Ca,
    shared: Arc<Shared>,
    tx: mpsc::Sender<Reply>,
    threads: Vec<JoinHandle<()>>,
    next_conn: usize,
}

impl Server {
    /// A hub whose certificate names its own loopback address.
    pub fn start() -> Server {
        Server::with_names(&[LOOPBACK])
    }

    /// A hub that holds every connection open after a reply, with no close_notify and no
    /// shutdown, and reads each further request the client sends on it.
    pub fn keep_alive() -> Server {
        Server::build(&[LOOPBACK], true)
    }

    /// A hub whose certificate, signed by its own CA, names only `names`.
    pub fn with_names(names: &[&str]) -> Server {
        Server::build(names, false)
    }

    fn build(names: &[&str], keep_alive: bool) -> Server {
        let ca = Ca::new();
        let (chain, key) = ca.leaf(names);
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("protocol versions")
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .expect("server certificate");
        let listener = TcpListener::bind((LOOPBACK, 0)).expect("bind loopback");
        let port = listener.local_addr().expect("address").port();
        let (tx, rx) = mpsc::channel();
        Server {
            listener: Arc::new(listener),
            origin: format!("https://{LOOPBACK}:{port}"),
            ca,
            shared: Arc::new(Shared {
                log: Mutex::new(Vec::new()),
                replies: Mutex::new(rx),
                config: Arc::new(config),
                keep_alive,
                accepted: AtomicUsize::new(0),
                closed: Mutex::new(BTreeSet::new()),
                closed_changed: Condvar::new(),
            }),
            tx,
            threads: Vec::new(),
            next_conn: 0,
        }
    }

    /// `https://127.0.0.1:<port>`.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// This hub's CA certificate in PEM.
    pub fn ca_pem(&self) -> &str {
        self.ca.pem()
    }

    /// Queues the reply to the next request read.
    pub fn reply(&self, reply: Reply) {
        self.tx.send(reply).expect("queue reply");
    }

    /// Serves the next `n` connections on a fixture thread, one request each, or in keep-alive
    /// mode each on its own thread for as long as the client keeps it. A connection whose
    /// handshake or request fails is counted and logs nothing.
    pub fn accept_n(&mut self, n: usize) {
        let listener = Arc::clone(&self.listener);
        let shared = Arc::clone(&self.shared);
        let first = self.next_conn;
        self.next_conn += n;
        self.threads.push(std::thread::spawn(move || {
            let mut held = Vec::new();
            for conn in first..first + n {
                let (stream, _) = listener.accept().expect("accept");
                shared.accepted.fetch_add(1, Ordering::SeqCst);
                if shared.keep_alive {
                    let shared = Arc::clone(&shared);
                    held.push(std::thread::spawn(move || serve(conn, stream, &shared)));
                } else {
                    serve(conn, stream, &shared);
                }
            }
            for t in held {
                t.join().expect("connection thread");
            }
        }));
    }

    /// The connections accepted so far.
    pub fn accepted(&self) -> usize {
        self.shared.accepted.load(Ordering::SeqCst)
    }

    /// Blocks until connection `conn` has ended. In keep-alive mode the fixture never ends one
    /// itself, so this returns only once the client has closed it.
    pub fn wait_closed(&self, conn: usize) {
        let mut closed = self.shared.closed.lock().expect("closed");
        while !closed.contains(&conn) {
            closed = self.shared.closed_changed.wait(closed).expect("closed");
        }
    }

    /// The requests read so far, in order.
    pub fn requests(&self) -> Vec<Recorded> {
        self.shared.log.lock().expect("log").clone()
    }

    /// Joins the fixture threads, checks that no further connection is waiting and returns the
    /// number of queued replies no request used.
    pub fn finish(mut self) -> usize {
        for t in self.threads.drain(..) {
            t.join().expect("fixture thread");
        }
        self.listener.set_nonblocking(true).expect("nonblocking");
        match self.listener.accept() {
            Err(e) if e.kind() == ErrorKind::WouldBlock => {}
            Ok(_) => panic!("an unexpected connection reached {}", self.origin),
            Err(e) => panic!("accept failed: {e}"),
        }
        let replies = self.shared.replies.lock().expect("replies");
        replies.try_iter().count()
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn serve(conn: usize, stream: TcpStream, shared: &Shared) {
    let Ok(server) = ServerConnection::new(Arc::clone(&shared.config)) else {
        shared.mark_closed(conn);
        return;
    };
    let mut tls = StreamOwned::new(server, stream);
    let mut buf = Vec::new();
    loop {
        let Some(request) = read_request(conn, &mut tls, &mut buf, shared.keep_alive) else {
            // The client closed the connection, or its handshake or request failed.
            let _ = tls.sock.shutdown(Shutdown::Both);
            break;
        };
        shared.log.lock().expect("log").push(request);
        // Every reply is queued before the call it answers; a missing one closes the connection.
        let reply = shared.replies.lock().expect("replies").try_recv();
        match reply {
            Ok(Reply::Raw(bytes)) => {
                let _ = tls.write_all(&bytes);
                let _ = tls.flush();
                if shared.keep_alive {
                    // Held open: no close_notify and no shutdown, ready for another request.
                    continue;
                }
                tls.conn.send_close_notify();
                let _ = tls.conn.complete_io(&mut tls.sock);
                let _ = tls.sock.shutdown(Shutdown::Both);
            }
            Ok(Reply::Close) | Err(_) => {
                let _ = tls.sock.shutdown(Shutdown::Both);
            }
        }
        break;
    }
    shared.mark_closed(conn);
}

/// Reads one request from `tls`, `buf` holding bytes already read past the previous one.
/// `None` when the stream ends or fails first. A held connection's body is exactly its
/// content-length, the rest kept for the next request; otherwise the body is every byte read
/// after the head.
fn read_request(
    conn: usize,
    tls: &mut StreamOwned<ServerConnection, TcpStream>,
    buf: &mut Vec<u8>,
    keep_alive: bool,
) -> Option<Recorded> {
    let mut chunk = [0u8; 8192];
    let end = loop {
        if let Some(i) = find(buf, b"\r\n\r\n") {
            break i;
        }
        match tls.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8(buf[..end].to_vec()).expect("UTF-8 head");
    let mut lines = head.split("\r\n");
    let start = lines.next().expect("request line");
    let mut parts = start.splitn(3, ' ');
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let headers: Vec<(String, String)> = lines
        .map(|l| {
            let (n, v) = l.split_once(':').expect("header line");
            (n.to_ascii_lowercase(), v.trim().to_string())
        })
        .collect();
    let length = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .map_or(0, |(_, v)| v.parse::<usize>().expect("content-length"));
    buf.drain(..end + 4);
    while buf.len() < length {
        match tls.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let body: Vec<u8> = if keep_alive {
        buf.drain(..length).collect()
    } else {
        std::mem::take(buf)
    };
    Some(Recorded {
        conn,
        head,
        method,
        target,
        headers,
        body,
    })
}
