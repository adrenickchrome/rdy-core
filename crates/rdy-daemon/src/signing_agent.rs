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

use crate::sys::{self, AgentStream};
use rdy_engine::signing_crypto::{
    AGENT_PACKET_MAX, AgentIdentity, COMMENT_MAX, IDENTITIES_MAX, PUBLIC_KEY_MAX, SSHSIG_MAX,
    STATEMENT_MAX, SigningAgent,
};
use rdy_engine::{NoAgentCause, RdyError, no_agent, signing_limit};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

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
    /// The connections of the calls in flight, so [`CallerAgent::cancel`] can close them.
    active: Mutex<Active>,
}

#[derive(Default)]
struct Active {
    next: u64,
    calls: Vec<Call>,
}

struct Call {
    id: u64,
    handle: AgentStream,
    cancelled: bool,
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
            active: Mutex::new(Active::default()),
        }
    }

    /// Cancels the calls in flight: their connections close and each ends as `E_NO_AGENT`
    /// `request cancelled`. A later call is not affected. There is no timeout: a request waits
    /// as long as the agent (or a security key) does, until it answers or is cancelled.
    pub fn cancel(&self) {
        let mut active = self.lock();
        for call in &mut active.calls {
            call.cancelled = true;
            sys::agent_close(&call.handle);
        }
    }

    fn lock(&self) -> MutexGuard<'_, Active> {
        self.active.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sends `request` (a message type and its body) on a new connection and returns the reply
    /// body when the reply's type is `expected`.
    fn call(&self, request: &[u8], expected: u8) -> Result<Vec<u8>, RdyError> {
        let socket = self
            .socket
            .as_ref()
            .ok_or_else(|| no_agent(NoAgentCause::Unset))?;
        let stream =
            sys::agent_connect(socket).map_err(|_| no_agent(NoAgentCause::ConnectionFailed))?;
        let handle =
            sys::agent_handle(&stream).map_err(|_| no_agent(NoAgentCause::ConnectionFailed))?;
        let id = {
            let mut active = self.lock();
            let id = active.next;
            active.next = active.next.wrapping_add(1);
            active.calls.push(Call {
                id,
                handle,
                cancelled: false,
            });
            id
        };
        let result = exchange(&stream, request, expected);
        let cancelled = {
            let mut active = self.lock();
            let at = active.calls.iter().position(|c| c.id == id);
            at.is_some_and(|at| active.calls.swap_remove(at).cancelled)
        };
        // The connection closes here, whatever the reply was.
        drop(stream);
        if cancelled {
            return Err(no_agent(NoAgentCause::Cancelled));
        }
        result.map_err(no_agent)
    }
}

impl SigningAgent for CallerAgent {
    fn identities(&self) -> Result<Vec<AgentIdentity>, RdyError> {
        let body = self.call(
            &[SSH2_AGENTC_REQUEST_IDENTITIES],
            SSH2_AGENT_IDENTITIES_ANSWER,
        )?;
        parse_identities(&body).ok_or_else(|| no_agent(NoAgentCause::InvalidReply))
    }

    fn sign(&self, public_key: &[u8], payload: &[u8]) -> Result<Vec<u8>, RdyError> {
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
        let body = self.call(&request, SSH2_AGENT_SIGN_RESPONSE)?;
        parse_sign_response(&body).ok_or_else(|| no_agent(NoAgentCause::InvalidReply))
    }
}

/// One request and its reply on `stream`: the reply body when its type is `expected`.
fn exchange(stream: &AgentStream, request: &[u8], expected: u8) -> Result<Vec<u8>, NoAgentCause> {
    let mut stream = stream;
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
        t if REFUSALS.contains(&t) => Err(NoAgentCause::Refused),
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
