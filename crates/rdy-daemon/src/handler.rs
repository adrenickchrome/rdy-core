//! What the host plugs into the transport.

use crate::ctl::CtlRequest;
use crate::error::ErrorReply;
use crate::session::SessionInfo;
use rdy_engine::signing_crypto::SigningAgent;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// A byte stream an MCP session runs on: the daemon's socket today, any transport later.
pub trait DuplexStream:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static
{
}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static> DuplexStream for T {}

/// The stream `Handler::mcp` takes.
pub type McpStream = Box<dyn DuplexStream>;

pub trait Handler: Send + Sync + 'static {
    /// One ctl request. Called on the blocking pool, one at a time per session, in request order.
    /// `agent` is the signer of the process at the other end of this connection (the daemon has
    /// none of its own); a call to it waits for that process, never for the writer.
    fn ctl(
        &self,
        session: &SessionInfo,
        request: CtlRequest,
        agent: Arc<dyn SigningAgent>,
    ) -> Result<serde_json::Value, ErrorReply>;

    /// An `mcp` connection after its hello, its signing channel already taken out of `stream`;
    /// `agent` is the signer of the process at the other end. The default closes the stream (M2b
    /// serves rmcp on it).
    fn mcp(
        &self,
        session: SessionInfo,
        stream: McpStream,
        agent: Arc<dyn SigningAgent>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        drop((session, stream, agent));
        Box::pin(async {})
    }

    /// A session was opened (its hello passed, it is in the table); its `ready` reply waits for
    /// this future. The default does nothing (the host reloads a changed pack here).
    fn session_started(&self, _session: &SessionInfo) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async {})
    }

    /// The notice to carry on the session's next ctl reply, if any. Called for every reply, so
    /// it must be cheap.
    fn notice(&self, _session: &SessionInfo) -> Option<String> {
        None
    }

    /// The session's connection has closed. Called exactly once per session, after it is removed
    /// from the table.
    fn session_ended(&self, _session: &SessionInfo) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Proto, SessionId};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// Echoes the first line it reads back on the same stream.
    struct EchoLine;

    impl Handler for EchoLine {
        fn ctl(
            &self,
            _: &SessionInfo,
            _: CtlRequest,
            _: Arc<dyn SigningAgent>,
        ) -> Result<serde_json::Value, ErrorReply> {
            unreachable!()
        }

        fn mcp(
            &self,
            _session: SessionInfo,
            stream: McpStream,
            _agent: Arc<dyn SigningAgent>,
        ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            Box::pin(async move {
                let mut r = BufReader::new(stream);
                let mut line = String::new();
                r.read_line(&mut line).await.unwrap();
                r.get_mut().write_all(line.as_bytes()).await.unwrap();
                r.get_mut().flush().await.unwrap();
            })
        }
    }

    #[tokio::test]
    async fn ps7_the_mcp_hook_takes_any_duplex_stream() {
        let (a, b) = tokio::io::duplex(1024);
        let session = SessionInfo {
            id: SessionId(1),
            proto: Proto::Mcp,
            version: "1".into(),
            cwd: "/".into(),
            peer_uid: 0,
            connected_at_ms: 0,
        };
        let h = EchoLine;
        let task = tokio::spawn(h.mcp(
            session,
            Box::new(a),
            Arc::new(rdy_engine::signing_crypto::UnavailableAgent),
        ));
        let mut b = BufReader::new(b);
        b.get_mut().write_all(b"one line\n").await.unwrap();
        let mut echo = String::new();
        b.read_line(&mut echo).await.unwrap();
        assert_eq!(echo, "one line\n");
        task.await.unwrap();
    }
}
