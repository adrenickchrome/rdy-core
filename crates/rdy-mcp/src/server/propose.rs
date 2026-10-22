//! The `propose` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, PlanParams, ProposeParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = propose_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Propose your view's changes for canon. A human approves it with rdy approve. The result lists the consequences for canon: what landing it would make suspect, with follow-ups (advice). plan_answers records answers in the same batch as the proposal. The reply includes the change plan. Open non-code items warn; deferred non-code items need the person's answer before landing."
    )]
    async fn rdy_propose(&self, Parameters(p): Parameters<ProposeParams>) -> CallToolResult {
        self.run("rdy_propose", p, tools::rdy_propose).await
    }

    #[tool(
        description = "Read a change plan, or record answers in the session's view. Read a proposal, landing or decision with its selector. Page with next. Confirm and defer need a reason."
    )]
    async fn rdy_plan(&self, Parameters(p): Parameters<PlanParams>) -> CallToolResult {
        self.run("rdy_plan", p, tools::rdy_plan).await
    }
}
