//! The `policy` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, POLICY_DOC, PolicyParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = policy_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(description = POLICY_DOC)]
    async fn rdy_policy(&self, Parameters(p): Parameters<PolicyParams>) -> CallToolResult {
        self.run("rdy_policy", p, tools::rdy_policy).await
    }
}
