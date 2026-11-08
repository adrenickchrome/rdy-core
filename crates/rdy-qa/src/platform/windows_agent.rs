//! The fake SSH agents on Windows: each one serves a named pipe, which a child finds through
//! `SSH_AUTH_SOCK`, as a real agent's pipe is found. The pipe carries raw agent packets, with no
//! layer of the daemon's own.
//!
//! - **A pipe name always exists.** A pipe with no free instance is no pipe to a caller, so the next
//!   instance is made before a connection is served. A caller that arrives meanwhile waits for it.
//! - **One connection at a time**, as on Unix, so the count of requests has one order.
//! - **Dropping the agent ends it**, whatever a caller does: a wait for a connection, a read and a
//!   write are all cancelled. Only a signer that blocks holds the drop, so a test releases such a
//!   signer first.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::runtime::{Builder, Runtime};
use tokio::sync::watch;
use windows_sys::Win32::Foundation::ERROR_NO_DATA;

/// Names this process's agents apart. The process id names it apart from others.
static NEXT_AGENT: AtomicU32 = AtomicU32::new(0);

/// An agent's running pipe: its name, the connections so far, and its thread.
struct Pipe {
    name: PathBuf,
    connections: Arc<AtomicUsize>,
    stop: watch::Sender<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Pipe {
    /// Makes a new pipe, which a caller can connect to at once, and hands each connection to `serve`
    /// on a thread of its own.
    fn start<F>(kind: &str, serve: F) -> io::Result<Pipe>
    where
        F: AsyncFnMut(NamedPipeServer) + Send + 'static,
    {
        let name = PathBuf::from(format!(
            r"\\.\pipe\rdy-qa-{kind}-{}-{}",
            std::process::id(),
            NEXT_AGENT.fetch_add(1, Ordering::SeqCst)
        ));
        let runtime = Builder::new_current_thread().enable_io().build()?;
        let first = {
            let _inside = runtime.enter();
            instance(&name, true)?
        };
        let connections = Arc::new(AtomicUsize::new(0));
        let (stop, stopped) = watch::channel(false);
        let thread = {
            let (name, connections) = (name.clone(), connections.clone());
            std::thread::spawn(move || {
                run(runtime, first, &name, &connections, stopped, serve);
            })
        };
        Ok(Pipe {
            name,
            connections,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A new instance of the pipe `name`. It answers callers on this machine only.
fn instance(name: &Path, first: bool) -> io::Result<NamedPipeServer> {
    ServerOptions::new()
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .create(name)
}

/// Ends when the agent is dropped.
async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|stopped| *stopped).await;
}

/// The agent's thread: accepts and serves until it is stopped, or until the pipe fails.
fn run<F>(
    runtime: Runtime,
    mut server: NamedPipeServer,
    name: &Path,
    connections: &AtomicUsize,
    mut stop: watch::Receiver<bool>,
    mut serve: F,
) where
    F: AsyncFnMut(NamedPipeServer),
{
    runtime.block_on(async {
        loop {
            let connected = tokio::select! {
                () = stopped(&mut stop) => return,
                connected = server.connect() => connected,
            };
            // a caller that has already gone was a connection too, with nothing to serve
            let gone = connected
                .as_ref()
                .is_err_and(|e| e.raw_os_error() == i32::try_from(ERROR_NO_DATA).ok());
            if connected.is_err() && !gone {
                return;
            }
            connections.fetch_add(1, Ordering::SeqCst);
            let Ok(next) = instance(name, false) else {
                return;
            };
            let current = std::mem::replace(&mut server, next);
            if gone {
                continue;
            }
            tokio::select! {
                () = stopped(&mut stop) => return,
                () = serve(current) => {}
            }
        }
    });
}

/// A fake SSH agent that records connections: a named pipe (set its name as `SSH_AUTH_SOCK` for a
/// child) that counts each connection and closes it at once, so a client that waits for a reply
/// gets end of file instead of hanging.
pub struct RecordingAgent {
    pipe: Pipe,
}

impl RecordingAgent {
    /// Makes a new pipe and starts accepting.
    pub fn start() -> io::Result<Self> {
        Ok(Self {
            pipe: Pipe::start("agent", async |_connection| {})?,
        })
    }

    /// The pipe's name, for `SSH_AUTH_SOCK`.
    pub fn socket_path(&self) -> &Path {
        &self.pipe.name
    }

    /// The number of connections so far. A caller that has seen its connection end is counted.
    pub fn connections(&self) -> usize {
        self.pipe.connections.load(Ordering::SeqCst)
    }
}

/// A test SSH agent on a named pipe (set its name as `SSH_AUTH_SOCK` for a child): it serves the
/// agent protocol's identities and sign requests from an in-memory [`SigningAgent`], one request
/// per connection, as rdy's caller agent sends them, and counts the requests it served. Any other
/// request, or a signer error, is answered with the bare agent failure.
///
/// [`SigningAgent`]: rdy_engine::signing_crypto::SigningAgent
pub struct SocketAgent {
    pipe: Pipe,
    served: Arc<AtomicUsize>,
}

impl SocketAgent {
    /// Makes a new pipe and serves `signer`.
    pub fn start(signer: Arc<dyn rdy_engine::signing_crypto::SigningAgent>) -> io::Result<Self> {
        let served = Arc::new(AtomicUsize::new(0));
        let counted = served.clone();
        let pipe = Pipe::start("sagent", async move |mut connection| {
            let Some(body) = read_request(&mut connection).await else {
                return;
            };
            let packet = super::super::agent_wire::reply_packet(&body, &*signer);
            // counted before the reply is written, so a caller that has read a reply sees it
            // counted
            counted.fetch_add(1, Ordering::SeqCst);
            let _ = connection.write_all(&packet).await;
        })?;
        Ok(Self { pipe, served })
    }

    /// The pipe's name, for `SSH_AUTH_SOCK`.
    pub fn socket_path(&self) -> &Path {
        &self.pipe.name
    }

    /// The requests answered so far (a failure reply counts too); a request is counted before
    /// its reply is written, so a caller that has read a reply sees it counted.
    pub fn requests(&self) -> usize {
        self.served.load(Ordering::SeqCst)
    }
}

/// One request's body from `connection`; `None` when no whole request within the bound came.
async fn read_request(connection: &mut NamedPipeServer) -> Option<Vec<u8>> {
    let mut len = [0u8; 4];
    connection.read_exact(&mut len).await.ok()?;
    let mut body = vec![0u8; super::super::agent_wire::request_len(len)?];
    connection.read_exact(&mut body).await.ok()?;
    Some(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signing::{QaAgent, TestKey};
    use rdy_daemon::sys::{AgentCancel, agent_connect};
    use std::io::{Read, Write};
    use std::sync::mpsc;
    use std::time::Duration;

    /// A hung test fails here. It asserts no timing.
    const HANG_LIMIT: Duration = Duration::from_secs(60);

    fn connect(agent: &Path) -> rdy_daemon::sys::AgentStream {
        agent_connect(agent, &AgentCancel::new().unwrap()).expect("connect to the agent's pipe")
    }

    /// One request on a connection of its own: the reply's type byte and body.
    fn ask(agent: &Path, request: &[u8]) -> Vec<u8> {
        let mut stream = connect(agent);
        let mut packet = u32::try_from(request.len()).unwrap().to_be_bytes().to_vec();
        packet.extend(request);
        stream.write_all(&packet).unwrap();
        let mut len = [0u8; 4];
        stream.read_exact(&mut len).unwrap();
        let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
        stream.read_exact(&mut body).unwrap();
        body
    }

    fn string(bytes: &[u8]) -> Vec<u8> {
        let mut out = u32::try_from(bytes.len()).unwrap().to_be_bytes().to_vec();
        out.extend(bytes);
        out
    }

    /// `f` on a thread of its own, which must end before the hang limit.
    fn must_end<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(HANG_LIMIT).expect("the fixture hung")
    }

    #[test]
    fn wx31a_the_pipe_agent_answers_as_the_socket_agent_does() {
        let agent = SocketAgent::start(Arc::new(QaAgent::new())).unwrap();
        let path = agent.socket_path().to_path_buf();
        // identities: the count, then each key blob with an empty comment
        let ids = ask(&path, &[11]);
        assert_eq!(ids[0], 12);
        assert_eq!(ids[1..5], 2u32.to_be_bytes());
        let person = QaAgent::blob(TestKey::Person);
        assert_eq!(ids[5..9 + person.len()], string(&person)[..]);
        assert_eq!(agent.requests(), 1, "counted before the reply");
        // a sign request of a held key
        let mut sign = vec![13u8];
        sign.extend(string(&person));
        sign.extend(string(b"a payload"));
        sign.extend(0u32.to_be_bytes());
        assert_eq!(ask(&path, &sign)[0], 14);
        // a key it does not hold, and a request it does not know: the bare failure
        let mut unheld = vec![13u8];
        unheld.extend(string(b"no such key"));
        unheld.extend(string(b"a payload"));
        assert_eq!(ask(&path, &unheld), [5]);
        assert_eq!(ask(&path, &[99]), [5]);
        assert_eq!(agent.requests(), 4);
    }

    #[test]
    fn wx31a_a_request_past_the_bound_gets_no_reply() {
        let agent = SocketAgent::start(Arc::new(QaAgent::new())).unwrap();
        let mut stream = connect(agent.socket_path());
        stream.write_all(&u32::MAX.to_be_bytes()).unwrap();
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty(), "{rest:?}");
        assert_eq!(agent.requests(), 0);
        // the next caller is served
        assert_eq!(ask(agent.socket_path(), &[11])[0], 12);
    }

    #[test]
    fn wx31a_a_recording_agent_counts_each_caller_and_ends_its_connection() {
        let agent = RecordingAgent::start().unwrap();
        assert_eq!(agent.connections(), 0);
        for n in 1..=50 {
            let mut stream = connect(agent.socket_path());
            let mut rest = Vec::new();
            stream.read_to_end(&mut rest).unwrap();
            assert!(rest.is_empty(), "{rest:?}");
            assert_eq!(agent.connections(), n);
        }
    }

    #[test]
    fn wx31a_dropping_an_agent_cancels_its_wait_and_a_stalled_caller() {
        // no caller at all: the wait for a connection ends
        let agent = SocketAgent::start(Arc::new(QaAgent::new())).unwrap();
        must_end(move || drop(agent));
        // a caller that sends half a request and waits: the read ends, and the caller sees the end
        let agent = SocketAgent::start(Arc::new(QaAgent::new())).unwrap();
        let mut stalled = connect(agent.socket_path());
        stalled.write_all(&[0, 0]).unwrap();
        must_end(move || drop(agent));
        let mut rest = Vec::new();
        stalled.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty(), "{rest:?}");
    }

    #[test]
    fn wx31a_the_pipe_is_gone_after_the_drop() {
        let agent = RecordingAgent::start().unwrap();
        let path = agent.socket_path().to_path_buf();
        drop(agent);
        let refused = agent_connect(&path, &AgentCancel::new().unwrap());
        assert!(refused.is_err(), "no agent is left on the pipe");
    }

    #[test]
    fn wx31a_a_cancelled_caller_leaves_the_agent_serving_the_next() {
        let agent = SocketAgent::start(Arc::new(QaAgent::new())).unwrap();
        // a caller that goes away before it asks anything
        drop(connect(agent.socket_path()));
        assert_eq!(ask(agent.socket_path(), &[11])[0], 12);
        // a caller whose cancel is set never connects
        let cancel = AgentCancel::new().unwrap();
        cancel.cancel();
        let refused = agent_connect(agent.socket_path(), &cancel);
        assert_eq!(
            refused.map(|_| ()).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(ask(agent.socket_path(), &[11])[0], 12);
    }
}
