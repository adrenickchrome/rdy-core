//! The `land` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, LAND_DOC, LandParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = land_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(description = LAND_DOC)]
    async fn rdy_land(&self, Parameters(p): Parameters<LandParams>) -> CallToolResult {
        self.run("rdy_land", p, tools::rdy_land).await
    }
}
