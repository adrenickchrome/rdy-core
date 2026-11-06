//! A fake SSH agent on the native endpoint: a raw named pipe on Windows, as a real agent's is, and
//! a socket elsewhere. It does not speak the daemon's own stream layer. Every connection carries
//! one request, which the fake reports and answers by its [`Act`].
#![allow(dead_code)]
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::runtime::Runtime;
use tokio::sync::Notify;

/// The raw bytes an agent writes for one request (its type and body), length prefix included.
/// An empty reply closes the connection with nothing written.
pub type Script = Arc<dyn Fn(u8, &[u8]) -> Vec<u8> + Send + Sync>;

/// What a fake agent does with each connection.
#[derive(Clone)]
pub enum Act {
    /// Reads the request, writes the script's reply and closes.
    Reply(Script),
    /// Reads the request, then answers nothing. It reports the client's close, and closes the
    /// connection itself at `release`.
    Hold { release: Arc<Notify> },
}

/// What a fake agent saw.
#[derive(Debug, PartialEq, Eq)]
pub enum Seen {
    /// One whole request, length prefix included.
    Request(Vec<u8>),
    /// The client closed its connection: its handle is gone.
    Closed,
}

/// One endpoint name, unused by any other test or process.
pub struct Endpoint {
    pub path: PathBuf,
    /// Holds a socket's directory.
    _dir: Option<tempfile::TempDir>,
}

static NEXT_ENDPOINT: AtomicUsize = AtomicUsize::new(0);

#[cfg(windows)]
pub fn endpoint() -> Endpoint {
    let n = NEXT_ENDPOINT.fetch_add(1, Ordering::SeqCst);
    Endpoint {
        path: PathBuf::from(format!(
            r"\\.\pipe\rdy-wx21d-test-{}-{n}",
            std::process::id()
        )),
        _dir: None,
    }
}

#[cfg(not(windows))]
pub fn endpoint() -> Endpoint {
    NEXT_ENDPOINT.fetch_add(1, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    Endpoint {
        path: dir.path().join("a"),
        _dir: Some(dir),
    }
}

/// The fake's listener: a raw pipe with one waiting instance on Windows, as an agent's is.
#[cfg(windows)]
mod raw {
    use std::io;
    use std::path::{Path, PathBuf};
    use tokio::net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions};

    pub struct Listener {
        path: PathBuf,
        waiting: NamedPipeServer,
    }

    pub type Conn = NamedPipeServer;

    fn instance(path: &Path, first: bool) -> io::Result<NamedPipeServer> {
        ServerOptions::new()
            .first_pipe_instance(first)
            .pipe_mode(PipeMode::Byte)
            .create(path)
    }

    pub fn bind(path: &Path) -> Listener {
        Listener {
            path: path.to_path_buf(),
            waiting: instance(path, true).unwrap(),
        }
    }

    impl Listener {
        pub async fn accept(&mut self) -> io::Result<Conn> {
            self.waiting.connect().await?;
            let next = instance(&self.path, false)?;
            Ok(std::mem::replace(&mut self.waiting, next))
        }
    }
}

#[cfg(not(windows))]
mod raw {
    use rdy_daemon::sys;
    use std::path::Path;

    pub type Listener = sys::Listener;
    pub type Conn = sys::Stream;

    pub fn bind(path: &Path) -> Listener {
        sys::bind(path).unwrap()
    }
}

/// A running fake agent. It stops when dropped.
pub struct Fake {
    pub path: PathBuf,
    _endpoint: Endpoint,
    runtime: Option<Runtime>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        // Not a plain drop: a test may drop its fake inside its own runtime.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// Starts a fake agent that does `act` on each connection and tells `report` what it saw.
pub fn start(act: Act, report: impl Fn(Seen) + Send + Sync + 'static) -> Fake {
    let endpoint = endpoint();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let report = Arc::new(report);
    // `enter`, not `block_on`: a test may start its fake from inside its own runtime.
    let mut listener = {
        let _in = runtime.enter();
        raw::bind(&endpoint.path)
    };
    runtime.spawn(async move {
        while let Ok(conn) = listener.accept().await {
            tokio::spawn(serve(conn, act.clone(), Arc::clone(&report)));
        }
    });
    Fake {
        path: endpoint.path.clone(),
        _endpoint: endpoint,
        runtime: Some(runtime),
    }
}

/// A name no process serves.
pub fn no_endpoint() -> Endpoint {
    endpoint()
}

/// Reads to the end, then reports the client's close.
async fn report_close(conn: &mut raw::Conn, report: &Arc<impl Fn(Seen)>) {
    let mut buf = vec![0u8; 64 * 1024];
    while matches!(conn.read(&mut buf).await, Ok(n) if n > 0) {}
    report(Seen::Closed);
}

async fn serve(mut conn: raw::Conn, act: Act, report: Arc<impl Fn(Seen) + Send + Sync + 'static>) {
    let mut prefix = [0u8; 4];
    if conn.read_exact(&mut prefix).await.is_err() {
        return;
    }
    let mut request = vec![0u8; u32::from_be_bytes(prefix) as usize];
    if conn.read_exact(&mut request).await.is_err() {
        return;
    }
    report(Seen::Request([&prefix[..], &request].concat()));
    match act {
        Act::Reply(script) => {
            // The script may block (a test holds the reply): a blocking thread runs it.
            let (ty, body) = (
                request.first().copied().unwrap_or(0),
                request.get(1..).unwrap_or(&[]).to_vec(),
            );
            let Ok(reply) = tokio::task::spawn_blocking(move || script(ty, &body)).await else {
                return;
            };
            let _ = conn.write_all(&reply).await;
            // A pipe write copies the bytes and returns: this waits until the pipe has them all.
            let _ = conn.write(&[]).await;
        }
        Act::Hold { release } => {
            tokio::select! {
                () = report_close(&mut conn, &report) => {}
                () = release.notified() => {}
            }
        }
    }
}

/// The path as a `&Path`, for callers that hold a `Fake`.
impl Fake {
    pub fn path(&self) -> &Path {
        &self.path
    }
}
