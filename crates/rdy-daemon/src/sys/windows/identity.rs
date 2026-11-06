//! Account identity on Windows: who is at the other end of a named pipe.
//!
//! - **An account is a full SID.** Two accounts are the same only when every byte of their SIDs
//!   is, and a session shows the SID's whole standard string (D-FARN9). No uid, hash or user name
//!   stands in for it.
//! - **Only the system's word counts.** The pipe API gives the peer's pid. The process's handle
//!   gives its creation time and its token's account. A hello's fields play no part.
//! - **A peer that cannot be read is refused.** Each failed call refuses the connection, with the
//!   call's own error. So does a peer that has exited.
//! - **The handle proves the pid.** An open handle pins a pid, but a process that has ended lets
//!   its pid go before the open. So after the open, and only then, the pipe must still have its
//!   other end: a peer gone by then means the handle may hold another process (a reused pid), and
//!   the connection is refused. A peer that left after that point has lost only its connection.
//!   Bytes left in the pipe by a peer that has gone can hide its leaving. Then the peer is read
//!   as whatever now has its pid, which is refused by its SID, or has this account and reads an
//!   end of file.
//!
//! Each side checks before a hello byte moves: [`check_client`] in the daemon, and
//! [`check_daemon`] in a client.

use super::Stream;
use super::process::{open_process, wait_exit};
use rdy_store::pio::{self, Sid};
use serde::{Serialize, Serializer};
use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle, OwnedHandle, RawHandle};
use std::path::Path;
use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED, FILETIME, HANDLE,
};
use windows_sys::Win32::System::Pipes::{
    GetNamedPipeClientProcessId, GetNamedPipeServerProcessId, PeekNamedPipe,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};

/// An account, as this platform names one: a SID.
pub type Account = Sid;

/// This process's account.
pub fn own_account() -> io::Result<Account> {
    pio::current_account()
}

/// A session's peer account, under its field on this platform (D-FARN9): the SID's standard
/// string form, never cut or hashed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionPeer {
    #[serde(serialize_with = "whole_sid")]
    peer_sid: Account,
}

fn whole_sid<S: Serializer>(sid: &Account, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(sid)
}

impl From<Account> for SessionPeer {
    fn from(peer_sid: Account) -> SessionPeer {
        SessionPeer { peer_sid }
    }
}

/// The process at the other end of a pipe, held open: its pid stays its own while this lives.
#[derive(Debug)]
pub struct PeerProcess<P = OwnedHandle> {
    pid: u32,
    created: u64,
    account: Account,
    handle: P,
}

impl<P> PeerProcess<P> {
    /// Its account.
    pub fn account(&self) -> Account {
        self.account
    }

    /// Its pid, from the pipe. The system always says on this platform; a Unix peer may not.
    pub fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }

    /// Its creation time, as the system's `FILETIME`: 100 ns units since 1601.
    pub fn created(&self) -> u64 {
        self.created
    }

    /// Whether this is the process with that pid and that creation time. A reused pid has another
    /// creation time.
    pub fn is_process(&self, pid: u32, created: u64) -> bool {
        self.pid == pid && self.created == created
    }

    /// The process's handle, held since the check. It has `PROCESS_QUERY_LIMITED_INFORMATION` and
    /// `SYNCHRONIZE`.
    pub fn handle(&self) -> &P {
        &self.handle
    }
}

/// The creation time of the process `process` is open on, as a `FILETIME` in one number: 100 ns
/// units since 1601.
#[allow(unsafe_code)]
pub(super) fn creation_time(process: RawHandle) -> io::Result<u64> {
    let mut times = [FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    }; 4];
    let [created, exited, kernel, user] = &mut times;
    // SAFETY: the caller keeps `process` open for the call, and all four times are writable.
    if unsafe { GetProcessTimes(process, created, exited, kernel, user) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(u64::from(created.dwHighDateTime) << 32 | u64::from(created.dwLowDateTime))
}

/// Test hook: the system calls of one peer check, in the order [`read_peer`] makes them.
#[doc(hidden)]
pub trait ProcessQuery {
    /// An open process. It pins the pid until it is dropped.
    type Process;
    /// The pid the pipe gives for its other end.
    fn peer_pid(&self) -> io::Result<u32>;
    fn open(&self, pid: u32) -> io::Result<Self::Process>;
    /// The creation time, as a `FILETIME` in one number.
    fn created(&self, process: &Self::Process) -> io::Result<u64>;
    /// The user SID of the process's token.
    fn account(&self, process: &Self::Process) -> io::Result<Account>;
    fn has_exited(&self, process: &Self::Process) -> io::Result<bool>;
    /// Whether the pipe still has its other end.
    fn connected(&self) -> io::Result<bool>;
}

/// Test hook: reads the process at the other end of a pipe. Any failed call is the error. A peer
/// that has gone is `ERROR_BROKEN_PIPE`.
#[doc(hidden)]
pub fn read_peer<Q: ProcessQuery>(query: &Q) -> io::Result<PeerProcess<Q::Process>> {
    let pid = query.peer_pid()?;
    let handle = query.open(pid)?;
    let created = query.created(&handle)?;
    let account = query.account(&handle)?;
    // after the open: only a handle pins the pid (the module's notes)
    if query.has_exited(&handle)? || !query.connected()? {
        return Err(io::Error::from_raw_os_error(ERROR_BROKEN_PIPE as i32));
    }
    Ok(PeerProcess {
        pid,
        created,
        account,
        handle,
    })
}

/// Test hook: [`check_client`] over any [`ProcessQuery`].
#[doc(hidden)]
pub fn check_client_with<Q: ProcessQuery>(
    query: &Q,
    expected: Account,
) -> Result<PeerProcess<Q::Process>, String> {
    let process =
        read_peer(query).map_err(|e| format!("could not read a connection's account SID: {e}"))?;
    if process.account != expected {
        return Err(format!(
            "refused a connection from account {}",
            process.account
        ));
    }
    Ok(process)
}

/// Test hook: [`check_daemon`] over any [`ProcessQuery`].
#[doc(hidden)]
pub fn check_daemon_with<Q: ProcessQuery>(
    query: &Q,
    pipe: &Path,
    expected: Account,
) -> io::Result<PeerProcess<Q::Process>> {
    let process = read_peer(query).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("could not read the daemon's account SID: {e}"),
        )
    })?;
    if process.account != expected {
        return Err(io::Error::other(format!(
            "the daemon on {} belongs to another account ({})",
            pipe.display(),
            process.account
        )));
    }
    Ok(process)
}

/// The daemon's check of a new connection, before it reads a byte: the peer runs as `expected`.
/// The error is the daemon's log line; the daemon then closes the connection without a reply.
pub fn check_client(stream: &Stream, expected: Account) -> Result<PeerProcess, String> {
    check_client_with(&PipeQuery::of(stream, PipePeer::Client), expected)
}

/// A client's check of a new connection, before it sends its hello: the peer runs as `expected`.
pub fn check_daemon(stream: &Stream, pipe: &Path, expected: Account) -> io::Result<PeerProcess> {
    check_daemon_with(&PipeQuery::of(stream, PipePeer::Server), pipe, expected)
}

/// Which end of the pipe the peer holds.
#[derive(Clone, Copy)]
enum PipePeer {
    Client,
    Server,
}

/// The real system calls, for one end of a pipe.
struct PipeQuery<'a> {
    stream: &'a Stream,
    peer: PipePeer,
}

impl<'a> PipeQuery<'a> {
    fn of(stream: &'a Stream, peer: PipePeer) -> PipeQuery<'a> {
        PipeQuery { stream, peer }
    }

    fn pipe(&self) -> HANDLE {
        self.stream.as_raw_handle()
    }
}

#[allow(unsafe_code)]
impl ProcessQuery for PipeQuery<'_> {
    type Process = OwnedHandle;

    fn peer_pid(&self) -> io::Result<u32> {
        let mut pid = 0u32;
        // SAFETY: `stream` holds the handle open for the call, and `pid` is writable.
        let ok = unsafe {
            match self.peer {
                PipePeer::Client => GetNamedPipeClientProcessId(self.pipe(), &mut pid),
                PipePeer::Server => GetNamedPipeServerProcessId(self.pipe(), &mut pid),
            }
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(pid)
    }

    fn open(&self, pid: u32) -> io::Result<OwnedHandle> {
        open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE)
    }

    fn created(&self, process: &OwnedHandle) -> io::Result<u64> {
        creation_time(process.as_raw_handle())
    }

    fn account(&self, process: &OwnedHandle) -> io::Result<Account> {
        pio::account_of_process(process.as_handle())
    }

    fn has_exited(&self, process: &OwnedHandle) -> io::Result<bool> {
        wait_exit(process.as_raw_handle(), 0)
    }

    fn connected(&self) -> io::Result<bool> {
        let none = std::ptr::null_mut();
        // SAFETY: `stream` holds the handle open for the call. With no buffer and null out
        // pointers the call writes nothing and takes no bytes.
        if unsafe { PeekNamedPipe(self.pipe(), none, 0, none.cast(), none.cast(), none.cast()) }
            != 0
        {
            return Ok(true);
        }
        let e = io::Error::last_os_error();
        match e.raw_os_error().and_then(|n| u32::try_from(n).ok()) {
            Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED) => Ok(false),
            _ => Err(e),
        }
    }
}
