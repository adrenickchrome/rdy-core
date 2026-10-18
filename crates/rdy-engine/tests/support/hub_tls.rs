//! A loopback TLS hub for the transport tests: a generated CA, a listener on a generated
//! loopback address, a log of the raw requests it read and the replies a test queues for it.
//!
//! Nothing here sleeps: a test queues every reply before it calls the transport, the fixture
//! thread serves exactly the connections `accept_n` names, and `finish` joins it and proves that
//! no further connection is waiting.
#![allow(dead_code)]

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, mpsc};
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

    /// A hub whose certificate, signed by its own CA, names only `names`.
    pub fn with_names(names: &[&str]) -> Server {
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

    /// Serves the next `n` connections on a fixture thread, one request each. A connection whose
    /// handshake or request fails is counted and logs nothing.
    pub fn accept_n(&mut self, n: usize) {
        let listener = Arc::clone(&self.listener);
        let shared = Arc::clone(&self.shared);
        let first = self.next_conn;
        self.next_conn += n;
        self.threads.push(std::thread::spawn(move || {
            for conn in first..first + n {
                let (stream, _) = listener.accept().expect("accept");
                serve(conn, stream, &shared);
            }
        }));
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
        return;
    };
    let mut tls = StreamOwned::new(server, stream);
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i;
        }
        match tls.read(&mut chunk) {
            Ok(0) | Err(_) => return,
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
    let mut body = buf[end + 4..].to_vec();
    while body.len() < length {
        match tls.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    shared.log.lock().expect("log").push(Recorded {
        conn,
        head,
        method,
        target,
        headers,
        body,
    });
    // Every reply is queued before the call it answers; a missing one closes the connection.
    let reply = shared.replies.lock().expect("replies").try_recv();
    match reply {
        Ok(Reply::Raw(bytes)) => {
            let _ = tls.write_all(&bytes);
            let _ = tls.flush();
            tls.conn.send_close_notify();
            let _ = tls.conn.complete_io(&mut tls.sock);
            let _ = tls.sock.shutdown(Shutdown::Both);
        }
        Ok(Reply::Close) | Err(_) => {
            let _ = tls.sock.shutdown(Shutdown::Both);
        }
    }
}
