//! The `propose` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, PlanParams, ProposeParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = propose_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Propose your view's changes for canon. A human approves it with rdy approve. The result lists the consequences for canon: what landing it would make suspect, with follow-ups (advice)."
    )]
    async fn rdy_propose(&self, Parameters(p): Parameters<ProposeParams>) -> CallToolResult {
        self.run("rdy_propose", p, tools::rdy_propose).await
    }

    #[tool(
        description = "Read a change plan, or answer its items. The plan lists what a change to decisions owes: items to update, rejustify, re-affirm, confirm or change in code, each with its subject, cause and state (open, closed or deferred). With no selector it reads your view's plan; view, proposal (P-<id>@<n>), landing (the full hash) and decision read a view's, a proposal version's, a landing's sealed plan and the seals that name a decision. answers: [{item, act, reason?}] records answers in your view's plan: confirm and defer need a reason, done keeps none, and which acts an item takes depends on its kind. All the answers are checked first, and either all are recorded or none; the reply is the plan after them. A cut plan carries next: pass page: <next> alone to continue it. Counts cover every item, not only the page."
    )]
    async fn rdy_plan(&self, Parameters(p): Parameters<PlanParams>) -> CallToolResult {
        self.run("rdy_plan", p, tools::rdy_plan).await
    }
}
