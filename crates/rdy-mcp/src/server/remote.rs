//! The `remote` family's tool registration: the remote edits, and `rdy_publish`, which uploads to
//! a configured publish remote.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, PublishParams, RemoteParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = remote_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "List, add or remove a remote, or change its mode. A remote has a name, URL and mode, share or publish. Remote edits are offline. Credentials stay in a private file at the user level. Relay warnings."
    )]
    async fn rdy_remote(&self, Parameters(p): Parameters<RemoteParams>) -> CallToolResult {
        self.run("rdy_remote", p, tools::rdy_remote).await
    }

    #[tool(
        description = "Upload a tagged publication to a publish remote and create a hub candidate. The site selects the profile. Relay warnings, the candidate link and the disclosure review to the person. The hub authorizes publication."
    )]
    async fn rdy_publish(&self, Parameters(p): Parameters<PublishParams>) -> CallToolResult {
        // a closed, size-checked result: no pack notice rides on it
        self.run_closed("rdy_publish", p, tools::rdy_publish).await
    }
}
