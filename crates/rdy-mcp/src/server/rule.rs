//! The `rule` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, RULE_DOC, RuleParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = rule_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(description = RULE_DOC)]
    async fn rdy_rule(&self, Parameters(p): Parameters<RuleParams>) -> CallToolResult {
        self.run("rdy_rule", p, tools::rdy_rule).await
    }
}
