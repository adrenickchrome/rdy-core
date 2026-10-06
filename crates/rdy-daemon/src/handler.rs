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
