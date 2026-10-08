//! The `decide` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, DecideParams, RevokeParams, SupersedeParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = decide_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Propose a decision (status proposed; accepted when its proposal lands). Omit parent to record it in the project's decision home. Cite brief: <token> from rdy_brief for stable content; the response lists the write's consequences."
    )]
    async fn rdy_decide(&self, Parameters(p): Parameters<DecideParams>) -> CallToolResult {
        self.run("rdy_decide", p, tools::rdy_decide).await
    }

    #[tool(
        description = "Propose a decision that supersedes an existing one. When the change retires only some of the old decision's rules, pass part (the words of the rules it retires) and the rest stays in force; omit part to supersede all of it. It inherits the old decision's affects, depends_on, conditional_on and refines links unless you give that kind (an empty list drops it); the result's inherited lists what was copied. Cite brief: <token> from rdy_brief for stable content; the response lists the write's consequences."
    )]
    async fn rdy_supersede(&self, Parameters(p): Parameters<SupersedeParams>) -> CallToolResult {
        self.run("rdy_supersede", p, tools::rdy_supersede).await
    }

    #[tool(
        description = "Propose revoking an existing decision. Omit parent to record it in the project's decision home. Cite brief: <token> from rdy_brief for stable content; the response lists the write's consequences."
    )]
    async fn rdy_revoke(&self, Parameters(p): Parameters<RevokeParams>) -> CallToolResult {
        self.run("rdy_revoke", p, tools::rdy_revoke).await
    }
}
