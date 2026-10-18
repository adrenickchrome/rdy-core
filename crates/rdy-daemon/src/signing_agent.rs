//! The calling process's own SSH agent, spoken to over its socket (the agent protocol's
//! identities and sign requests only).
//!
//! A [`CallerAgent`] belongs to one calling process: the CLI or the MCP proxy makes it from its
//! own environment with [`CallerAgent::from_env`]. The daemon never reads `SSH_AUTH_SOCK` and
//! never holds a process-wide signer. A `CallerAgent` holds no private material.
//!
//! Each call opens its own connection, sends one request, reads one bounded reply and closes the
//! connection; no connection outlives its call. Calls block, so they run on a request's blocking
//! worker. Every failure is `E_NO_AGENT` with one of the closed causes, never the agent's own
//! text.
//!
//! A call can be cancelled at any point. It is registered before its connection is set up. Its
//! connect, request and reply all run on the call's own thread: a cancel ends a pending connect
//! through the sys seam (which closes that socket), and closes a connection in use. A call made
//! with a [`CallCancel`] also ends at once, before it connects, when that cancel came before the
//! call started (for example while its worker waited for a thread).

use crate::sys::{self, AgentCancel, AgentStream};
use rdy_engine::signing_crypto::{
    AGENT_PACKET_MAX, AgentIdentity, COMMENT_MAX, IDENTITIES_MAX, PUBLIC_KEY_MAX, SSHSIG_MAX,
    STATEMENT_MAX, SigningAgent,
};
use rdy_engine::{NoAgentCause, RdyError, no_agent, signing_limit};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// The agent's failure reply.
pub const SSH_AGENT_FAILURE: u8 = 5;
/// The client's identities request.
pub const SSH2_AGENTC_REQUEST_IDENTITIES: u8 = 11;
/// The agent's identities answer.
pub const SSH2_AGENT_IDENTITIES_ANSWER: u8 = 12;
/// The client's sign request.
pub const SSH2_AGENTC_SIGN_REQUEST: u8 = 13;
/// The agent's sign response.
pub const SSH2_AGENT_SIGN_RESPONSE: u8 = 14;
/// A failure reply some agents send.
pub const SSH_COM_AGENT2_FAILURE: u8 = 30;
/// A failure reply some agents send.
pub const SSH2_AGENT_FAILURE: u8 = 102;

/// The reply types that refuse a request.
const REFUSALS: [u8; 3] = [
    SSH_AGENT_FAILURE,
    SSH_COM_AGENT2_FAILURE,
    SSH2_AGENT_FAILURE,
];

/// The environment variable that names the caller's agent socket.
const AUTH_SOCK: &str = "SSH_AUTH_SOCK";

/// The SSH agent of the calling process.
pub struct CallerAgent {
    /// The agent socket; `None` when the caller has no agent.
    socket: Option<PathBuf>,
    /// How the socket is reached: the daemon's sys seam (an in-memory one in unit tests).
    transport: Arc<dyn Transport>,
    /// The connections of the calls in flight, so [`CallerAgent::cancel`] and a call's
    /// [`CallCancel`] can close them.
    active: Arc<Mutex<Active>>,
}

/// The cancel of one call, latched: a cancel that comes before the call starts is kept, and the
/// call then ends at once as `E_NO_AGENT` `request cancelled`, without reaching the agent.
#[derive(Default)]
pub struct CallCancel {
    state: Mutex<CancelState>,
}

/// What runs at a cancel.
type Hook = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct CancelState {
    cancelled: bool,
    next: u64,
    hooks: Vec<(u64, Hook)>,
}

impl CallCancel {
    pub fn new() -> CallCancel {
        CallCancel::default()
    }

    fn lock(&self) -> MutexGuard<'_, CancelState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Cancels the call, now or when it starts. A second cancel does nothing.
    pub fn cancel(&self) {
        let hooks = {
            let mut st = self.lock();
            if st.cancelled {
                return;
            }
            st.cancelled = true;
            std::mem::take(&mut st.hooks)
        };
        for (_, hook) in hooks {
            hook();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.lock().cancelled
    }

    /// Runs `hook` once at the cancel, unless [`CallCancel::forget`] comes first. `None`, and
    /// the hook is dropped unrun, when the cancel has already come.
    pub fn on_cancel(&self, hook: Hook) -> Option<u64> {
        let mut st = self.lock();
        if st.cancelled {
            return None;
        }
        let id = st.next;
        st.next = st.next.wrapping_add(1);
        st.hooks.push((id, hook));
        Some(id)
    }

    /// Drops the hook `id` unrun. A hook already taken by a cancel may still run: hooks are
    /// harmless once their call has ended.
    pub fn forget(&self, id: u64) {
        self.lock().hooks.retain(|(h, _)| *h != id);
    }
}

#[derive(Default)]
struct Active {
    next: u64,
    calls: Vec<Call>,
}

/// One call in flight. It is registered before its connection is set up, so a cancel at any
/// point is latched.
struct Call {
    id: u64,
    /// Set by [`CallerAgent::cancel`]; the call then ends as `request cancelled`.
    cancelled: bool,
    /// Ends the call's connect while it is pending.
    setup: Arc<AgentCancel>,
    /// Closes the call's connection once it is in use.
    closer: Option<Closer>,
}

/// Closes a connection from another thread: a read or write that waits on it ends.
type Closer = Box<dyn Fn() + Send>;

/// How a [`CallerAgent`] reaches its agent socket.
trait Transport: Send + Sync {
    /// A new connection to the agent socket at `path`, made on the calling thread. It may wait;
    /// once `cancel` is set it ends, its socket closed, before it returns.
    fn connect(&self, path: &Path, cancel: &AgentCancel) -> io::Result<Box<dyn Conn>>;
}

/// One connection to an agent.
trait Conn: Read + Write + Send {
    /// A closer for this connection.
    fn closer(&self) -> io::Result<Closer>;
}

/// The daemon's sys seam: the platform socket.
struct SysTransport;

impl Transport for SysTransport {
    fn connect(&self, path: &Path, cancel: &AgentCancel) -> io::Result<Box<dyn Conn>> {
        Ok(Box::new(sys::agent_connect(path, cancel)?))
    }
}

impl Conn for AgentStream {
    fn closer(&self) -> io::Result<Closer> {
        let handle = sys::agent_handle(self)?;
        Ok(Box::new(move || sys::agent_close(&handle)))
    }
}

impl CallerAgent {
    /// The agent `SSH_AUTH_SOCK` names in this process's environment; an unset or empty value
    /// is no agent. Only the calling process (the CLI, the MCP proxy) calls this.
    pub fn from_env() -> CallerAgent {
        let socket = std::env::var_os(AUTH_SOCK)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        CallerAgent::new(socket)
    }

    /// The agent at the socket `path`, whatever the environment says.
    pub fn with_socket(path: impl Into<PathBuf>) -> CallerAgent {
        CallerAgent::new(Some(path.into()))
    }

    fn new(socket: Option<PathBuf>) -> CallerAgent {
        CallerAgent {
            socket,
            transport: Arc::new(SysTransport),
            active: Arc::new(Mutex::new(Active::default())),
        }
    }

    /// The agent at `path`, reached through `transport`.
    #[cfg(test)]
    fn with_transport(path: impl Into<PathBuf>, transport: Arc<dyn Transport>) -> CallerAgent {
        CallerAgent {
            transport,
            ..CallerAgent::with_socket(path)
        }
    }

    /// Cancels the calls in flight, a call still setting up its connection included: their
    /// connections close and each ends as `E_NO_AGENT` `request cancelled`, without sending its
    /// request if it had not yet. A later call is not affected. There is no timeout: a request
    /// waits as long as the agent (or a security key) does, until it answers or is cancelled.
    pub fn cancel(&self) {
        let mut active = self.lock();
        for call in &mut active.calls {
            call.cancel();
        }
    }

    /// The agent's identities, as [`SigningAgent::identities`], with `cancel` ending the call
    /// whenever it comes, before the call starts included.
    pub fn identities_until(&self, cancel: &CallCancel) -> Result<Vec<AgentIdentity>, RdyError> {
        self.identities_with(Some(cancel))
    }

    /// A signature, as [`SigningAgent::sign`], with `cancel` ending the call whenever it comes,
    /// before the call starts included.
    pub fn sign_until(
        &self,
        public_key: &[u8],
        payload: &[u8],
        cancel: &CallCancel,
    ) -> Result<Vec<u8>, RdyError> {
        self.sign_with(public_key, payload, Some(cancel))
    }

    fn lock(&self) -> MutexGuard<'_, Active> {
        lock_active(&self.active)
    }

    /// Sends `request` (a message type and its body) on a new connection and returns the reply
    /// body when the reply's type is `expected`. A call bound to `cancel` ends as `request
    /// cancelled` when that cancel came first, without connecting.
    fn call(
        &self,
        request: &[u8],
        expected: u8,
        cancel: Option<&CallCancel>,
    ) -> Result<Vec<u8>, RdyError> {
        let socket = self
            .socket
            .as_ref()
            .ok_or_else(|| no_agent(NoAgentCause::Unset))?;
        // Registered (and so cancellable) before any connection setup.
        let setup =
            Arc::new(AgentCancel::new().map_err(|_| no_agent(NoAgentCause::ConnectionFailed))?);
        let id = {
            let mut active = self.lock();
            let id = active.next;
            active.next = active.next.wrapping_add(1);
            active.calls.push(Call {
                id,
                cancelled: false,
                setup: Arc::clone(&setup),
                closer: None,
            });
            id
        };
        // The cancel's hook is set after the registration, so a cancel at any point reaches
        // the call: before the hook, it is seen here; after it, the hook cancels the call.
        let hook = match cancel {
            None => None,
            Some(cancel) => {
                let active = Arc::clone(&self.active);
                let hook = cancel.on_cancel(Box::new(move || {
                    let mut active = lock_active(&active);
                    if let Some(call) = active.calls.iter_mut().find(|c| c.id == id) {
                        call.cancel();
                    }
                }));
                if hook.is_none() {
                    let mut active = self.lock();
                    active.calls.retain(|c| c.id != id);
                    return Err(no_agent(NoAgentCause::Cancelled));
                }
                hook
            }
        };
        let result = self.connect_and_exchange(id, socket, &setup, request, expected);
        if let (Some(cancel), Some(hook)) = (cancel, hook) {
            cancel.forget(hook);
        }
        let cancelled = {
            let mut active = self.lock();
            let at = active.calls.iter().position(|c| c.id == id);
            at.is_some_and(|at| active.calls.swap_remove(at).cancelled)
        };
        if cancelled {
            return Err(no_agent(NoAgentCause::Cancelled));
        }
        result.map_err(no_agent)
    }

    /// The connection setup and the exchange of the registered call `id`, both on this thread.
    /// A cancel latched at any point before the exchange stops the call before it sends
    /// anything; a cancel during the connect ends it through `setup`.
    fn connect_and_exchange(
        &self,
        id: u64,
        socket: &Path,
        setup: &AgentCancel,
        request: &[u8],
        expected: u8,
    ) -> Result<Vec<u8>, NoAgentCause> {
        // The active lock is never held across the connect, so a cancel can reach it.
        let mut stream = self
            .transport
            .connect(socket, setup)
            .map_err(|_| NoAgentCause::ConnectionFailed)?;
        let closer = stream
            .closer()
            .map_err(|_| NoAgentCause::ConnectionFailed)?;
        {
            let mut active = self.lock();
            let call = active.calls.iter_mut().find(|c| c.id == id);
            match call {
                Some(call) if !call.cancelled => call.closer = Some(closer),
                // Cancelled while connecting: the connection closes here, unused.
                _ => return Err(NoAgentCause::Cancelled),
            }
        }
        // The connection closes when this returns, whatever the reply was.
        exchange(&mut *stream, request, expected)
    }
}

fn lock_active(active: &Mutex<Active>) -> MutexGuard<'_, Active> {
    active.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Call {
    /// Marks the call cancelled, ends its connect and closes its connection.
    fn cancel(&mut self) {
        self.cancelled = true;
        self.setup.cancel();
        if let Some(close) = &self.closer {
            close();
        }
    }
}

impl CallerAgent {
    fn identities_with(&self, cancel: Option<&CallCancel>) -> Result<Vec<AgentIdentity>, RdyError> {
        let body = self.call(
            &[SSH2_AGENTC_REQUEST_IDENTITIES],
            SSH2_AGENT_IDENTITIES_ANSWER,
            cancel,
        )?;
        parse_identities(&body).ok_or_else(|| no_agent(NoAgentCause::InvalidReply))
    }

    fn sign_with(
        &self,
        public_key: &[u8],
        payload: &[u8],
        cancel: Option<&CallCancel>,
    ) -> Result<Vec<u8>, RdyError> {
        if public_key.len() > PUBLIC_KEY_MAX {
            return Err(signing_limit(&format!("{PUBLIC_KEY_MAX} bytes")));
        }
        if payload.len() > STATEMENT_MAX {
            return Err(signing_limit(&format!("{STATEMENT_MAX} bytes")));
        }
        let mut request = Vec::with_capacity(1 + 4 + public_key.len() + 4 + payload.len() + 4);
        request.push(SSH2_AGENTC_SIGN_REQUEST);
        put_string(&mut request, public_key);
        put_string(&mut request, payload);
        request.extend(0u32.to_be_bytes()); // flags
        let body = self.call(&request, SSH2_AGENT_SIGN_RESPONSE, cancel)?;
        parse_sign_response(&body).ok_or_else(|| no_agent(NoAgentCause::InvalidReply))
    }
}

impl SigningAgent for CallerAgent {
    fn identities(&self) -> Result<Vec<AgentIdentity>, RdyError> {
        self.identities_with(None)
    }

    fn sign(&self, public_key: &[u8], payload: &[u8]) -> Result<Vec<u8>, RdyError> {
        self.sign_with(public_key, payload, None)
    }
}

/// One request and its reply on `stream`: the reply body when its type is `expected`.
fn exchange(stream: &mut dyn Conn, request: &[u8], expected: u8) -> Result<Vec<u8>, NoAgentCause> {
    let len = u32::try_from(request.len()).map_err(|_| NoAgentCause::InvalidReply)?;
    let mut packet = Vec::with_capacity(4 + request.len());
    packet.extend(len.to_be_bytes());
    packet.extend(request);
    stream
        .write_all(&packet)
        .map_err(|_| NoAgentCause::ConnectionFailed)?;
    let mut prefix = [0u8; 4];
    stream
        .read_exact(&mut prefix)
        .map_err(|_| NoAgentCause::ConnectionFailed)?;
    // The cap comes before any read or allocation of the body.
    let len =
        usize::try_from(u32::from_be_bytes(prefix)).map_err(|_| NoAgentCause::ReplyTooLarge)?;
    if len > AGENT_PACKET_MAX {
        return Err(NoAgentCause::ReplyTooLarge);
    }
    if len == 0 {
        return Err(NoAgentCause::InvalidReply);
    }
    let mut reply = vec![0u8; len];
    stream
        .read_exact(&mut reply)
        .map_err(|_| NoAgentCause::ConnectionFailed)?;
    match reply[0] {
        // A refusal is the bare type; anything after it makes the reply malformed.
        t if REFUSALS.contains(&t) && len == 1 => Err(NoAgentCause::Refused),
        t if REFUSALS.contains(&t) => Err(NoAgentCause::InvalidReply),
        t if t == expected => {
            reply.remove(0);
            Ok(reply)
        }
        _ => Err(NoAgentCause::InvalidReply),
    }
}

/// The identities of an identities answer's body, every count and length bounded; comments are
/// read past and dropped.
fn parse_identities(body: &[u8]) -> Option<Vec<AgentIdentity>> {
    let mut r = Wire(body);
    let count = usize::try_from(r.u32()?).ok()?;
    // Each identity takes at least 8 bytes, so the count is bounded by the body too.
    if count > IDENTITIES_MAX || count > body.len() / 8 {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let key = r.string(PUBLIC_KEY_MAX)?;
        r.string(COMMENT_MAX)?;
        out.push(AgentIdentity {
            public_key: key.to_vec(),
        });
    }
    r.0.is_empty().then_some(out)
}

/// The signature blob of a sign response's body.
fn parse_sign_response(body: &[u8]) -> Option<Vec<u8>> {
    let mut r = Wire(body);
    let sig = r.string(SSHSIG_MAX)?;
    r.0.is_empty().then(|| sig.to_vec())
}

/// Appends `bytes` as an SSH string; callers bound `bytes` far below 2^32.
fn put_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend(u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend(bytes);
}

/// A reader over agent wire data that never reads past its end.
struct Wire<'a>(&'a [u8]);

impl<'a> Wire<'a> {
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        if n > self.0.len() {
            return None;
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Some(head)
    }

    fn u32(&mut self) -> Option<u32> {
        let b = self.bytes(4)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// A length-prefixed string of at most `max` bytes.
    fn string(&mut self, max: usize) -> Option<&'a [u8]> {
        let n = usize::try_from(self.u32()?).ok()?;
        if n > max {
            return None;
        }
        self.bytes(n)
    }
}

#[cfg(test)]
mod tests {
    //! Cancellation while a call is still setting up its connection, through an in-memory
    //! transport (no socket): the call is registered, and can be cancelled, before it connects,
    //! and its connect runs on the call's own thread.

    use super::{CallCancel, CallerAgent, Closer, Conn, Transport};
    use crate::sys::AgentCancel;
    use rdy_engine::signing_crypto::SigningAgent;
    use rdy_engine::{E_NO_AGENT, RdyError};
    use std::io::{self, Cursor, Read, Write};
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle, ThreadId};

    /// An in-memory connection: it records what the caller writes, answers with an empty
    /// identities answer, and reports when it is dropped (closed).
    struct MemConn {
        written: Arc<Mutex<Vec<u8>>>,
        reply: Cursor<Vec<u8>>,
        dropped: Sender<()>,
    }

    impl Read for MemConn {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.reply.read(buf)
        }
    }

    impl Write for MemConn {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.written.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Conn for MemConn {
        fn closer(&self) -> io::Result<Closer> {
            Ok(Box::new(|| {}))
        }
    }

    impl Drop for MemConn {
        fn drop(&mut self) {
            let _ = self.dropped.send(());
        }
    }

    /// A transport whose connect reports that it began, then waits for the test's release.
    struct PausedConnect {
        entered: Mutex<Sender<()>>,
        release: Mutex<Receiver<()>>,
        written: Arc<Mutex<Vec<u8>>>,
        dropped: Mutex<Sender<()>>,
    }

    impl Transport for PausedConnect {
        fn connect(&self, _path: &Path, _cancel: &AgentCancel) -> io::Result<Box<dyn Conn>> {
            let _ = self.entered.lock().unwrap().send(());
            let _ = self.release.lock().unwrap().recv();
            // the empty identities answer: type 12, count 0
            let reply = vec![0, 0, 0, 5, 12, 0, 0, 0, 0];
            Ok(Box::new(MemConn {
                written: Arc::clone(&self.written),
                reply: Cursor::new(reply),
                dropped: self.dropped.lock().unwrap().clone(),
            }))
        }
    }

    struct Harness {
        agent: Arc<CallerAgent>,
        entered: Receiver<()>,
        release: Sender<()>,
        written: Arc<Mutex<Vec<u8>>>,
        dropped: Receiver<()>,
    }

    fn harness() -> Harness {
        let (entered_tx, entered) = channel();
        let (release, release_rx) = channel();
        let (dropped_tx, dropped) = channel();
        let written = Arc::new(Mutex::new(Vec::new()));
        let transport = PausedConnect {
            entered: Mutex::new(entered_tx),
            release: Mutex::new(release_rx),
            written: Arc::clone(&written),
            dropped: Mutex::new(dropped_tx),
        };
        Harness {
            agent: Arc::new(CallerAgent::with_transport(
                "agent.sock",
                Arc::new(transport),
            )),
            entered,
            release,
            written,
            dropped,
        }
    }

    /// Starts an identities call on its own thread; its result arrives on the receiver.
    fn start(agent: &Arc<CallerAgent>) -> (JoinHandle<()>, Receiver<Result<usize, RdyError>>) {
        let (tx, rx) = channel();
        let agent = Arc::clone(agent);
        let worker = thread::spawn(move || {
            let _ = tx.send(agent.identities().map(|ids| ids.len()));
        });
        (worker, rx)
    }

    /// Starts an identities call bound to `cancel` on its own thread.
    fn start_until(
        agent: &Arc<CallerAgent>,
        cancel: &Arc<CallCancel>,
    ) -> (JoinHandle<()>, Receiver<Result<usize, RdyError>>) {
        let (tx, rx) = channel();
        let (agent, cancel) = (Arc::clone(agent), Arc::clone(cancel));
        let worker = thread::spawn(move || {
            let _ = tx.send(agent.identities_until(&cancel).map(|ids| ids.len()));
        });
        (worker, rx)
    }

    fn assert_cancelled(r: Result<usize, RdyError>) {
        let e = r.expect_err("the cancelled call must fail");
        assert_eq!(e.code, E_NO_AGENT);
        assert_eq!(e.message, "the SSH agent could not sign: request cancelled");
    }

    #[test]
    fn sig3_cancel_during_connect_sends_nothing() {
        let h = harness();
        let (_worker, done) = start(&h.agent);
        // Connection setup has begun and is paused: cancel, then let the connect finish.
        h.entered.recv().unwrap();
        h.agent.cancel();
        h.release.send(()).unwrap();
        assert_cancelled(done.recv().unwrap());
        // The connection that was set up closes unused: no request reached the agent.
        h.dropped.recv().unwrap();
        assert!(
            h.written.lock().unwrap().is_empty(),
            "no exchange after cancel"
        );

        // The cancel applied to that call only: the next call is answered.
        h.release.send(()).unwrap();
        assert_eq!(h.agent.identities().unwrap(), Vec::new());
        assert_eq!(*h.written.lock().unwrap(), vec![0, 0, 0, 1, 11]);
    }

    /// A socket a connect has opened but not yet connected; it reports when it is dropped.
    struct PendingSocket(Arc<AtomicUsize>);

    impl Drop for PendingSocket {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// A transport whose connect never completes on its own: it opens a socket, reports the
    /// thread it runs on, and waits until its setup is cancelled. Nothing else releases it.
    struct BlockedConnect {
        entered: Mutex<Sender<ThreadId>>,
        /// The sockets of the connects that have not ended.
        open: Arc<AtomicUsize>,
    }

    impl Transport for BlockedConnect {
        fn connect(&self, _path: &Path, cancel: &AgentCancel) -> io::Result<Box<dyn Conn>> {
            self.open.fetch_add(1, Ordering::SeqCst);
            let _socket = PendingSocket(Arc::clone(&self.open));
            let _ = self.entered.lock().unwrap().send(thread::current().id());
            cancel.wait();
            Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
        }
    }

    #[test]
    fn sig3_cancel_ends_a_blocked_connect_and_frees_its_socket_and_worker() {
        let (entered_tx, entered) = channel();
        let open = Arc::new(AtomicUsize::new(0));
        let transport = BlockedConnect {
            entered: Mutex::new(entered_tx),
            open: Arc::clone(&open),
        };
        let agent = Arc::new(CallerAgent::with_transport(
            "agent.sock",
            Arc::new(transport),
        ));
        // Repeated cancelled attempts leave nothing behind: each connect is never released by
        // the test, only by the cancel.
        for attempt in 0..20 {
            let (worker, done) = start(&agent);
            // The connect runs on the call's own worker, not on a helper thread.
            assert_eq!(
                entered.recv().unwrap(),
                worker.thread().id(),
                "attempt {attempt}"
            );
            agent.cancel();
            assert_cancelled(done.recv().unwrap());
            // The worker ends, and the connect's socket is closed by then.
            worker.join().unwrap();
            assert_eq!(open.load(Ordering::SeqCst), 0, "attempt {attempt}");
        }
    }

    #[test]
    fn sig5_a_cancel_before_the_call_starts_is_kept() {
        let h = harness();
        let cancel = Arc::new(CallCancel::new());
        // cancelled while the call waits to be run (a queued worker)
        cancel.cancel();
        let (worker, done) = start_until(&h.agent, &cancel);
        assert_cancelled(done.recv().unwrap());
        worker.join().unwrap();
        assert!(h.entered.try_recv().is_err(), "the call never connects");
        assert!(h.written.lock().unwrap().is_empty());

        // a cancel during the connect ends the call too, and only that call
        let cancel = Arc::new(CallCancel::new());
        let (_worker, done) = start_until(&h.agent, &cancel);
        h.entered.recv().unwrap();
        cancel.cancel();
        h.release.send(()).unwrap();
        assert_cancelled(done.recv().unwrap());
        h.dropped.recv().unwrap();
        assert!(
            h.written.lock().unwrap().is_empty(),
            "no exchange after cancel"
        );
        let fresh = Arc::new(CallCancel::new());
        let (_worker, done) = start_until(&h.agent, &fresh);
        h.release.send(()).unwrap();
        assert_eq!(done.recv().unwrap().unwrap(), 0);
    }
}
