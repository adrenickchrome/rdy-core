//! The `dispute` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, DISPUTE_DOC, DisputeParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = dispute_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(description = DISPUTE_DOC)]
    async fn rdy_dispute(&self, Parameters(p): Parameters<DisputeParams>) -> CallToolResult {
        self.run("rdy_dispute", p, tools::rdy_dispute).await
    }
}
