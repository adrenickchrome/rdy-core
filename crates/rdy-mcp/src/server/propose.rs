//! The `propose` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, ProposeParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = propose_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Propose your view's changes for canon. A human approves it with rdy approve. The result lists the consequences for canon: what landing it would make suspect, with follow-ups (advice)."
    )]
    async fn rdy_propose(&self, Parameters(p): Parameters<ProposeParams>) -> CallToolResult {
        self.run("rdy_propose", p, tools::rdy_propose).await
    }
}
