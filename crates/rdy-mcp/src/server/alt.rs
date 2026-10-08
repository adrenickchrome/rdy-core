//! The `alt` family's tool registration.

use crate::alt_tools;
use crate::params::Parameters;
use crate::server::RdyServer;
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = alt_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Create, change, list, compare or abandon an alternative set: member views that answer one question. A proposal from a member view names its set and its own member."
    )]
    async fn rdy_alternatives(
        &self,
        Parameters(p): Parameters<alt_tools::AltParams>,
    ) -> CallToolResult {
        self.run("rdy_alternatives", p, alt_tools::rdy_alternatives)
            .await
    }
}
