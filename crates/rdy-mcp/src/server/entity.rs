//! The `entity` family's tool registration.

use crate::entity_tools::{self, AcceptParams};
use crate::params::Parameters;
use crate::server::RdyServer;
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = entity_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Accept suggestions in bulk: pass suggestion objects from a write or search response unchanged (an entity suggestion may add name, type, entity_kind, value, unit, body (a term needs one), parent; a link's target must be an entity). Links are added, never replaced; a link already there is skipped. New entities go under parent. One write: cite brief: <token> from rdy_brief for stable content; the response lists the write's consequences and suggestions."
    )]
    async fn rdy_accept(&self, Parameters(p): Parameters<AcceptParams>) -> CallToolResult {
        self.run("rdy_accept", p, entity_tools::rdy_accept).await
    }
}
