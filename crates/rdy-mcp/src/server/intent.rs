//! The `intent` family's tool registration.

use crate::intent_tools;
use crate::params::Parameters;
use crate::server::RdyServer;
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = intent_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Post, renew, withdraw or list intents: advisory notes on the blocks or area you are about to change, with an expiry (default 2h, at most 7d). An intent never locks anything."
    )]
    async fn rdy_intent(
        &self,
        Parameters(p): Parameters<intent_tools::IntentParams>,
    ) -> CallToolResult {
        self.run("rdy_intent", p, intent_tools::rdy_intent).await
    }
}
