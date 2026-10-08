//! The `asset` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, AssetParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = asset_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Upload a binary asset (base64); returns the hash to put in a figure block's blob field."
    )]
    async fn rdy_asset(&self, Parameters(p): Parameters<AssetParams>) -> CallToolResult {
        self.run("rdy_asset", p, tools::rdy_asset).await
    }
}
