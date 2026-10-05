//! What the host plugs into the transport.

use crate::ctl::CtlRequest;
use crate::error::ErrorReply;
use crate::session::SessionInfo;
use std::future::Future;
use std::pin::Pin;

pub trait Handler: Send + Sync + 'static {
    /// One ctl request. Called on the blocking pool, one at a time per session, in request order.
    fn ctl(
        &self,
        session: &SessionInfo,
        request: CtlRequest,
    ) -> Result<serde_json::Value, ErrorReply>;

    /// An `mcp` connection after its hello. The default closes the stream (M2b serves rmcp on it).
    fn mcp(
        &self,
        session: SessionInfo,
        stream: tokio::net::UnixStream,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        drop((session, stream));
        Box::pin(async {})
    }

    /// The session's connection has closed. Called exactly once per session, after it is removed
    /// from the table.
    fn session_ended(&self, _session: &SessionInfo) {}
}
