//! The `site` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, SiteParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = site_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "The link to this project's live docs site, for the person to open in a browser. Pages update by themselves as the scope changes. Pass scope for a view or proposal version: it renders in review mode with Show changes. Relay warning and each entry of warnings to the person. Read and write through the tools yourself, not the site."
    )]
    async fn rdy_site(&self, Parameters(p): Parameters<SiteParams>) -> CallToolResult {
        self.run("rdy_site", p, tools::rdy_site).await
    }
}
