//! The transport on Windows: a byte-mode named pipe with one instance for each connection.
//!
//! - **The name stays owned.** The first instance is made with `FILE_FLAG_FIRST_PIPE_INSTANCE`, so
//!   a name another process holds is refused. One instance always waits for the next client, and
//!   its replacement exists before it is handed to [`Listener::accept`]'s caller.
//! - **Every instance is private.** Each one has this account's protected descriptor and refuses
//!   remote clients.
//! - **A half-close.** A pipe has no half-close: a handle closes both ways, and a null write
//!   reaches no reader. The protocols need one: an MCP client's half-close ends the session, and
//!   the session's last bytes still come back. So a [`Stream`] carries it in the byte stream. The
//!   byte `0xFF` is the escape, and never occurs in UTF-8 text: `0xFF 0x00` is a data byte `0xFF`,
//!   and `0xFF 0x01` is the end of the sender's bytes. Everything else passes unchanged. Both ends
//!   of a connection are this type, and the layer adds no framing and no buffer of its own, so
//!   the hello's, ctl's and MCP's bounds and the pipe's backpressure hold as they are.

use rdy_store::pio::{ObjectKind, PrivateDescriptor};
use std::io;
use std::os::windows::io::{AsRawHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, PipeMode, ServerOptions,
};
use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY};

/// The escape byte. It is no byte of UTF-8 text, so ordinary traffic carries no escapes.
const ESCAPE: u8 = 0xFF;
/// After an escape: a data byte [`ESCAPE`].
const CODE_DATA: u8 = 0x00;
/// After an escape: the sender has shut down its writing.
const CODE_END: u8 = 0x01;
/// A data byte [`ESCAPE`], as sent.
const ESCAPED_DATA: &[u8] = &[ESCAPE, CODE_DATA];
/// The end of the sender's bytes, as sent.
const END_MARK: &[u8] = &[ESCAPE, CODE_END];

/// How long a connect waits before it tries a busy pipe again. A pipe is busy between a client
/// taking the waiting instance and the listener making its replacement.
const BUSY_RETRY: Duration = Duration::from_millis(20);
/// How long a connect goes on trying a busy pipe. Then it fails as a full socket queue does.
const BUSY_LIMIT: Duration = Duration::from_secs(5);

/// The daemon's listener: the pipe's instance that waits for the next client.
pub struct Listener {
    name: PathBuf,
    descriptor: PrivateDescriptor,
    waiting: NamedPipeServer,
}

impl std::fmt::Debug for Listener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Listener")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// One new instance of the pipe `name`, in byte mode, private to this account, refusing remote
/// clients. Only the listener's first instance claims the name.
#[allow(unsafe_code)]
fn instance(
    name: &Path,
    descriptor: &PrivateDescriptor,
    first: bool,
) -> io::Result<NamedPipeServer> {
    let mut attributes = descriptor.attributes();
    let mut options = ServerOptions::new();
    options
        .access_inbound(true)
        .access_outbound(true)
        .pipe_mode(PipeMode::Byte)
        .first_pipe_instance(first)
        .reject_remote_clients(true);
    // SAFETY: `attributes` is a valid SECURITY_ATTRIBUTES whose descriptor `descriptor` keeps
    // alive, and both outlive the call, which only reads them.
    let made =
        unsafe { options.create_with_security_attributes_raw(name, (&raw mut attributes).cast()) };
    made.map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("could not create named pipe {}: {e}", name.display()),
        )
    })
}

/// Claims the pipe `path` and makes its first waiting instance. A name that any process holds is
/// refused, as `PermissionDenied`.
pub fn bind(path: &Path) -> io::Result<Listener> {
    let descriptor = PrivateDescriptor::of_this_account(ObjectKind::File)?;
    let waiting = instance(path, &descriptor, true)?;
    Ok(Listener {
        name: path.to_path_buf(),
        descriptor,
        waiting,
    })
}

impl Listener {
    /// The next connection. Cancel safe: a future dropped before it ends loses no connection.
    ///
    /// When the replacement instance cannot be made, the connected instance stays in place and
    /// the error goes to the caller. The name stays owned, and the next call tries again and
    /// hands that connection over.
    pub async fn accept(&mut self) -> io::Result<Stream> {
        self.waiting.connect().await?;
        let next = instance(&self.name, &self.descriptor, false)?;
        let connected = std::mem::replace(&mut self.waiting, next);
        Ok(Stream::new(End::Server(connected)))
    }
}

/// Connects to the pipe `path`. A busy pipe is tried again for up to [`BUSY_LIMIT`]. A connect
/// holds no handle between its tries, so a dropped future leaves nothing behind.
pub async fn connect(path: &Path) -> io::Result<Stream> {
    let limit = tokio::time::Instant::now() + BUSY_LIMIT;
    loop {
        match ClientOptions::new().open(path) {
            Ok(client) => return Ok(Stream::new(End::Client(client))),
            Err(e) if is_os_error(&e, ERROR_PIPE_BUSY) && tokio::time::Instant::now() < limit => {
                tokio::time::sleep(BUSY_RETRY).await;
            }
            Err(e) => return Err(e),
        }
    }
}

fn is_os_error(e: &io::Error, code: u32) -> bool {
    e.raw_os_error().and_then(|n| u32::try_from(n).ok()) == Some(code)
}

/// What one connect says of the pipe `path`. No instance of that name is gone: no process listens.
/// A connect (it closes at once, and the daemon reads an end of file where a hello would be), a
/// busy pipe and access denied are live. Anything else proves neither. This connects and never
/// looks for a name: a pipe is no file system object.
pub async fn endpoint_liveness(path: &Path) -> super::super::Liveness {
    use super::super::Liveness;
    match ClientOptions::new().open(path) {
        Ok(_) => Liveness::Live,
        Err(e) if is_os_error(&e, ERROR_FILE_NOT_FOUND) => Liveness::Gone,
        Err(e) if is_os_error(&e, ERROR_PIPE_BUSY) || is_os_error(&e, ERROR_ACCESS_DENIED) => {
            Liveness::Live
        }
        Err(_) => Liveness::Ambiguous,
    }
}

/// The handle of a stream's pipe end.
#[derive(Debug)]
enum End {
    Server(NamedPipeServer),
    Client(NamedPipeClient),
}

impl End {
    fn poll_read(&mut self, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self {
            End::Server(end) => Pin::new(end).poll_read(cx, buf),
            End::Client(end) => Pin::new(end).poll_read(cx, buf),
        }
    }

    fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match self {
            End::Server(end) => Pin::new(end).poll_write(cx, buf),
            End::Client(end) => Pin::new(end).poll_write(cx, buf),
        }
    }

    fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            End::Server(end) => end.try_read(buf),
            End::Client(end) => end.try_read(buf),
        }
    }
}

/// What a stream has read of the sender's escapes.
#[derive(Debug, Default)]
struct Decoder {
    /// The last byte read was an escape, and its code has not come yet.
    in_escape: bool,
    /// The end mark, or the end of the pipe, has been read.
    ended: bool,
}

impl Decoder {
    /// Turns `bytes`, as read, into data in place, and returns the length of the data. Whatever
    /// follows an end mark is dropped. An unknown code is `InvalidData`.
    fn decode(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let mut kept = 0;
        for i in 0..bytes.len() {
            let byte = bytes[i];
            if self.in_escape {
                self.in_escape = false;
                match byte {
                    CODE_DATA => {
                        bytes[kept] = ESCAPE;
                        kept += 1;
                    }
                    CODE_END => {
                        self.ended = true;
                        return Ok(kept);
                    }
                    code => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("an unknown escape code {code} on a named pipe"),
                        ));
                    }
                }
            } else if byte == ESCAPE {
                self.in_escape = true;
            } else {
                bytes[kept] = byte;
                kept += 1;
            }
        }
        Ok(kept)
    }
}

/// `result`, with a peer that has gone counted as done. A flush or a shutdown has nothing left to
/// wait for then. The peer may have answered before it left, and its answer is still there to read.
fn peer_gone_is_done(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => other,
    }
}

/// One connection to or from the daemon: either end of a pipe instance.
#[derive(Debug)]
pub struct Stream {
    end: End,
    decoder: Decoder,
    /// Bytes of an escape that a short write left. They go out before anything else.
    unsent: &'static [u8],
    /// The end mark has been queued: further writes fail.
    write_ended: bool,
}

impl Stream {
    fn new(end: End) -> Stream {
        Stream {
            end,
            decoder: Decoder::default(),
            unsent: &[],
            write_ended: false,
        }
    }

    /// Reads what is there now, without waiting. `WouldBlock` when nothing is; 0 at the end.
    pub fn try_read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        while !self.decoder.ended {
            let n = self.end.try_read(buf)?;
            if n == 0 {
                self.decoder.ended = true;
                break;
            }
            let kept = self.decoder.decode(&mut buf[..n])?;
            if kept > 0 {
                return Ok(kept);
            }
        }
        Ok(0)
    }

    /// Sends what an earlier short write left of an escape.
    fn poll_unsent(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while !self.unsent.is_empty() {
            let n = ready!(self.end.poll_write(cx, self.unsent))?;
            if n == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }
            self.unsent = &self.unsent[n..];
        }
        Poll::Ready(Ok(()))
    }

    /// Queues the end mark behind every byte written, once, and waits until the pipe has taken
    /// them.
    fn poll_end_mark(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if !self.write_ended {
            ready!(self.poll_unsent(cx))?;
            let n = ready!(self.end.poll_write(cx, END_MARK))?;
            if n == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }
            self.unsent = &END_MARK[n..];
            self.write_ended = true;
        }
        self.poll_taken(cx)
    }

    /// Ends when the pipe has taken every byte written so far. Tokio's pipe write copies the bytes
    /// and returns, and only one write is in progress at a time, so a null write waits its turn
    /// behind the others. No reader sees it.
    fn poll_taken(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        ready!(self.poll_unsent(cx))?;
        ready!(self.end.poll_write(cx, &[]))?;
        Poll::Ready(Ok(()))
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let me = self.get_mut();
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let start = buf.filled().len();
        while !me.decoder.ended {
            ready!(me.end.poll_read(cx, buf))?;
            if buf.filled().len() == start {
                me.decoder.ended = true;
                break;
            }
            let kept = me.decoder.decode(&mut buf.filled_mut()[start..])?;
            buf.set_filled(start + kept);
            if kept > 0 {
                return Poll::Ready(Ok(()));
            }
        }
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let me = self.get_mut();
        if me.write_ended {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        ready!(me.poll_unsent(cx))?;
        if buf[0] == ESCAPE {
            let n = ready!(me.end.poll_write(cx, ESCAPED_DATA))?;
            if n == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }
            // the data byte is taken once the escape's first byte is: the rest goes first next
            me.unsent = &ESCAPED_DATA[n..];
            return Poll::Ready(Ok(1));
        }
        let run = buf.iter().position(|b| *b == ESCAPE).unwrap_or(buf.len());
        me.end.poll_write(cx, &buf[..run])
    }

    /// A socket's flush does nothing, so a caller reads the reply or the end of the stream next, on
    /// every platform.
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let taken = ready!(self.get_mut().poll_taken(cx));
        Poll::Ready(peer_gone_is_done(taken))
    }

    /// Queues the end mark behind every byte written, and waits until the pipe has taken them. The
    /// peer's reads end after those bytes; this stream goes on reading what the peer sends.
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let me = self.get_mut();
        let shut = ready!(me.poll_end_mark(cx));
        Poll::Ready(peer_gone_is_done(shut))
    }
}

impl AsRawHandle for Stream {
    fn as_raw_handle(&self) -> RawHandle {
        match &self.end {
            End::Server(end) => end.as_raw_handle(),
            End::Client(end) => end.as_raw_handle(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::Liveness;
    use super::*;
    use rdy_store::pio::{self, Ace, current_account};
    use std::fs::File;
    use std::mem::{ManuallyDrop, size_of};
    use std::os::windows::io::FromRawHandle;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::windows::named_pipe::PipeEnd;
    use windows_sys::Win32::Security::{
        ACL, ACL_REVISION, InitializeAcl, InitializeSecurityDescriptor, SECURITY_ATTRIBUTES,
        SECURITY_DESCRIPTOR, SetSecurityDescriptorDacl,
    };
    use windows_sys::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn name() -> PathBuf {
        PathBuf::from(format!(
            r"\\.\pipe\rdy-wx13-unit-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ))
    }

    /// A connected pair: (client, server).
    async fn pair(listener: &mut Listener) -> (Stream, Stream) {
        let client = connect(&listener.name).await.unwrap();
        let server = listener.accept().await.unwrap();
        (client, server)
    }

    /// `handle`'s owner and protected DACL, read through a file that does not own the handle.
    #[allow(unsafe_code)]
    fn security(handle: RawHandle) -> pio::Security {
        // SAFETY: the caller's handle stays open for the call, and `ManuallyDrop` keeps this file
        // from closing it.
        let file = ManuallyDrop::new(unsafe { File::from_raw_handle(handle) });
        pio::security_of(&file).unwrap()
    }

    /// A client writes a request and is held up before its flush. In that time the daemon answers
    /// and exits. The flush must not hide the reply: on a socket a flush does nothing, and a
    /// reply is read before the end of the stream.
    #[tokio::test]
    async fn wx25b_a_flush_after_the_peer_answered_and_left_hides_no_reply() {
        let mut listener = bind(&name()).unwrap();
        let (mut client, mut server) = pair(&mut listener).await;
        client.write_all(b"request\n").await.unwrap();
        let mut request = [0u8; 8];
        server.read_exact(&mut request).await.unwrap();
        server.write_all(b"reply\n").await.unwrap();
        server.flush().await.unwrap();
        drop(server);
        // the handle closes with the drop, and the close has no await to wait on
        tokio::task::yield_now().await;
        client.flush().await.expect("a flush is no read");
        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert_eq!(reply, "reply\n");
        client.shutdown().await.expect("a shutdown is no read");
    }

    #[tokio::test]
    async fn wx13_native_every_instance_has_the_private_descriptor() {
        let me = current_account().unwrap();
        let mut listener = bind(&name()).unwrap();
        let (first, second) = (pair(&mut listener).await, pair(&mut listener).await);
        for handle in [
            listener.waiting.as_raw_handle(),
            first.1.as_raw_handle(),
            second.1.as_raw_handle(),
        ] {
            let security = security(handle);
            assert_eq!(security.owner, Some(me));
            assert!(security.protected);
            assert_eq!(
                security.dacl,
                Some(vec![Ace::private(me, ObjectKind::File)])
            );
        }
    }

    #[tokio::test]
    async fn wx13_native_every_instance_is_a_byte_mode_pipe() {
        let mut listener = bind(&name()).unwrap();
        let (client, server) = pair(&mut listener).await;
        for info in [
            listener.waiting.info().unwrap(),
            match &server.end {
                End::Server(end) => end.info().unwrap(),
                End::Client(_) => panic!("accept gave a client end"),
            },
        ] {
            assert_eq!(info.mode, PipeMode::Byte);
            assert_eq!(info.end, PipeEnd::Server);
        }
        match &client.end {
            End::Client(end) => assert_eq!(end.info().unwrap().mode, PipeMode::Byte),
            End::Server(_) => panic!("connect gave a server end"),
        }
    }

    #[tokio::test]
    async fn wx13_native_a_failed_replacement_keeps_the_connection() {
        let good = name();
        let mut listener = bind(&good).unwrap();
        let mut client = connect(&good).await.unwrap();
        // the next instance cannot be made
        listener.name = PathBuf::from(r"\\.\pipe\");
        let e = listener.accept().await.unwrap_err();
        assert!(
            e.to_string()
                .starts_with(r"could not create named pipe \\.\pipe\: "),
            "{e}"
        );
        // the name is still held, by the instance the client has
        assert!(bind(&good).is_err());
        listener.name = good.clone();
        let mut server = listener.accept().await.unwrap();
        client.write_all(b"kept").await.unwrap();
        client.flush().await.unwrap();
        let mut got = [0u8; 4];
        server.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"kept");
    }

    /// A pipe that denies every account, this one included: an empty DACL, and no owner given.
    #[allow(unsafe_code)]
    fn pipe_denying_everyone(name: &Path) -> NamedPipeServer {
        let mut acl: ACL = unsafe { std::mem::zeroed() };
        let mut descriptor: SECURITY_DESCRIPTOR = unsafe { std::mem::zeroed() };
        // SAFETY: `acl` and `descriptor` are live, writable and of the sizes given. The descriptor
        // points at `acl`, and both outlive the create call that reads them.
        unsafe {
            assert_ne!(
                InitializeAcl(&raw mut acl, size_of::<ACL>() as u32, ACL_REVISION),
                0
            );
            assert_ne!(
                InitializeSecurityDescriptor(
                    (&raw mut descriptor).cast(),
                    SECURITY_DESCRIPTOR_REVISION
                ),
                0
            );
            assert_ne!(
                SetSecurityDescriptorDacl((&raw mut descriptor).cast(), 1, &raw const acl, 0),
                0
            );
            let mut attributes = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: (&raw mut descriptor).cast(),
                bInheritHandle: 0,
            };
            ServerOptions::new()
                .first_pipe_instance(true)
                .create_with_security_attributes_raw(name, (&raw mut attributes).cast())
                .unwrap()
        }
    }

    /// Waits until the pipe has no instance. A closed instance with a read in progress lets go of
    /// its handle on the runtime's next turn, when the cancelled read has come back.
    async fn wait_until_gone(path: &Path) {
        while endpoint_liveness(path).await != Liveness::Gone {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn wx13_native_endpoint_liveness_proves_only_an_end() {
        let path = name();
        // no instance of that name
        assert_eq!(endpoint_liveness(&path).await, Liveness::Gone);
        let mut listener = bind(&path).unwrap();
        // an instance answers; the probe has taken it, so the pipe is now busy, which is live too
        assert_eq!(endpoint_liveness(&path).await, Liveness::Live);
        assert_eq!(endpoint_liveness(&path).await, Liveness::Live);
        drop(listener.accept().await.unwrap());
        drop(listener);
        wait_until_gone(&path).await;

        // a pipe this account may not open is there all the same, and a bind cannot claim it
        let denied = name();
        let squatter = pipe_denying_everyone(&denied);
        assert_eq!(endpoint_liveness(&denied).await, Liveness::Live);
        assert_eq!(
            bind(&denied).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        drop(squatter);
        wait_until_gone(&denied).await;

        // no pipe name at all proves neither
        assert_eq!(
            endpoint_liveness(Path::new(r"C:\no-such-directory-of-rdy\x")).await,
            Liveness::Ambiguous
        );
    }

    #[test]
    fn wx13_native_escapes_are_undone_in_any_chunks() {
        // every byte value, and the escape next to itself and to the codes
        let mut data: Vec<u8> = (0..=u8::MAX).collect();
        data.extend_from_slice(&[0xff, 0xff, 0x00, 0xff, 0x01, 0xff]);
        let mut sent = Vec::new();
        for byte in &data {
            match *byte {
                ESCAPE => sent.extend_from_slice(ESCAPED_DATA),
                other => sent.push(other),
            }
        }
        sent.extend_from_slice(END_MARK);
        sent.extend_from_slice(b"after the end");
        for chunk in 1..=sent.len() {
            let mut decoder = Decoder::default();
            let mut got = Vec::new();
            for part in sent.chunks(chunk) {
                if decoder.ended {
                    break;
                }
                let mut part = part.to_vec();
                let kept = decoder.decode(&mut part).unwrap();
                got.extend_from_slice(&part[..kept]);
            }
            assert!(decoder.ended, "chunks of {chunk}");
            assert_eq!(got, data, "chunks of {chunk}");
        }
    }

    #[test]
    fn wx13_native_an_unknown_escape_is_an_error() {
        for code in [0x02, 0x7f, 0xff] {
            let e = Decoder::default().decode(&mut [ESCAPE, code]).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidData);
            assert_eq!(
                e.to_string(),
                format!("an unknown escape code {code} on a named pipe")
            );
        }
    }

    #[tokio::test]
    async fn wx13_native_a_half_close_ends_one_direction_only() {
        let mut listener = bind(&name()).unwrap();
        let (mut client, mut server) = pair(&mut listener).await;
        client.write_all(&[1, 0xff, 2]).await.unwrap();
        client.shutdown().await.unwrap();
        // a second shutdown is no error, and a write after it fails
        client.shutdown().await.unwrap();
        assert_eq!(
            client.write_all(b"x").await.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        let mut got = Vec::new();
        server.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, [1, 0xff, 2]);
        // the end stays: a further read is 0 too
        assert_eq!(server.read(&mut [0u8; 4]).await.unwrap(), 0);
        // the other direction still runs, after the half-close and to its own end
        server.write_all(b"after").await.unwrap();
        server.flush().await.unwrap();
        drop(server);
        got.clear();
        client.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, b"after");
    }

    #[tokio::test]
    async fn wx13_native_one_byte_reads_and_try_read_undo_escapes() {
        let mut listener = bind(&name()).unwrap();
        let (mut client, mut server) = pair(&mut listener).await;
        client.write_all(&[0xff, 0x41, 0xff]).await.unwrap();
        client.flush().await.unwrap();
        // the hello is read one byte at a time
        assert_eq!(server.read_u8().await.unwrap(), 0xff);
        assert_eq!(server.read_u8().await.unwrap(), 0x41);
        assert_eq!(server.read_u8().await.unwrap(), 0xff);
        // nothing there yet
        assert_eq!(
            server.try_read(&mut [0u8; 8]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        client.write_all(&[0xff, 0x42]).await.unwrap();
        client.shutdown().await.unwrap();
        // what `close_after_reply` does: reads off what is there, waiting for nothing. Here it
        // waits until the bytes have arrived, on the stream's own readiness.
        let mut got = Vec::new();
        let mut buf = [0u8; 8];
        loop {
            match server.try_read(&mut buf) {
                Ok(0) => break,
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    tokio::task::yield_now().await;
                }
                Err(e) => panic!("{e}"),
            }
        }
        assert_eq!(got, [0xff, 0x42]);
    }

    #[tokio::test]
    async fn wx13_native_a_writer_waits_for_its_reader() {
        // Larger than every buffer between the two ends.
        const TOTAL: usize = 8 * 1024 * 1024;
        const PIPE_BUFFER: usize = 65_536;
        let mut listener = bind(&name()).unwrap();
        let (mut client, mut server) = pair(&mut listener).await;
        let chunk = vec![0x41u8; 4096];

        // Nobody reads: writes go on until the pipe's buffer and the one write in progress are
        // full. Each round lets the runtime finish what it can, so no clock is involved.
        let mut taken = 0;
        for _ in 0..50_000 {
            let write = std::future::poll_fn(|cx| {
                Poll::Ready(Pin::new(&mut client).poll_write(cx, &chunk))
            });
            if let Poll::Ready(n) = write.await {
                taken += n.unwrap();
            }
            tokio::task::yield_now().await;
        }
        assert!(
            taken <= PIPE_BUFFER + 2 * chunk.len(),
            "{taken} bytes taken with no reader"
        );
        // A flush waits with the writes.
        let flush = std::future::poll_fn(|cx| {
            Poll::Ready(Pin::new(&mut client).poll_flush(cx).is_pending())
        });
        assert!(flush.await, "a flush ended while the pipe was full");

        // The reader starts: every byte arrives, and the writer finishes.
        let rest = TOTAL - taken;
        let write = async {
            client.write_all(&vec![0x41u8; rest]).await.unwrap();
            client.shutdown().await.unwrap();
        };
        let mut got = Vec::new();
        let (read, ()) = tokio::join!(server.read_to_end(&mut got), write);
        assert_eq!(read.unwrap(), TOTAL);
        assert!(got.iter().all(|b| *b == 0x41));
    }
}
