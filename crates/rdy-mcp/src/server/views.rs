//! The `views` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::view_tools;
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = views_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Create, fork, include, exclude, compare, list, show, close or undo views. Include, exclude, close and undo need ownership of the target view. Include, exclude and undo return the op's consequences, with conflict entries to resolve."
    )]
    async fn rdy_view(&self, Parameters(p): Parameters<view_tools::ViewParams>) -> CallToolResult {
        self.run("rdy_view", p, view_tools::rdy_view).await
    }

    #[tool(
        description = "Canon's landings, newest first: who approved which proposal version, how (assurance, relay, evidence), and the tags on each. Each row names the proposer (proposed_by) and the approver. limit and since page it."
    )]
    async fn rdy_log(&self, Parameters(p): Parameters<view_tools::LogParams>) -> CallToolResult {
        self.run("rdy_log", p, view_tools::rdy_log).await
    }
}
