//! The `remote` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, RemoteParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = remote_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "List, add or remove a remote, or change its mode. A remote has a name, URL and mode, share or publish. Remote edits are offline. Credentials stay in a private file at the user level. Relay warnings."
    )]
    async fn rdy_remote(&self, Parameters(p): Parameters<RemoteParams>) -> CallToolResult {
        self.run("rdy_remote", p, tools::rdy_remote).await
    }
}
