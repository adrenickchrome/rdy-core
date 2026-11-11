//! The `hook` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, HOOK_DOC, HookParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = hook_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(description = HOOK_DOC)]
    async fn rdy_hook(&self, Parameters(p): Parameters<HookParams>) -> CallToolResult {
        self.run("rdy_hook", p, tools::rdy_hook).await
    }
}
