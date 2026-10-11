//! The `coverage` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, CoverageParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = coverage_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "The coverage report for a scope (default your view) or a path: the score and its six dimensions, the lowest pages, the areas, and each coverage issue with its fix."
    )]
    async fn rdy_coverage(&self, Parameters(p): Parameters<CoverageParams>) -> CallToolResult {
        self.run("rdy_coverage", p, tools::rdy_coverage).await
    }
}
