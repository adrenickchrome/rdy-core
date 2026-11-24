//! The `propose` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, PlanParams, ProposeParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = propose_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Propose your view's changes for canon. A human approves it with rdy approve. The result lists the consequences for canon: what landing it would make suspect, with follow-ups (advice). plan_answers records answers in the same batch as the proposal. The reply includes the change plan. Open non-code items warn; deferred non-code items need the person's answer before landing. title is required; summary is optional, and the person who approves reads it. Example: {\"title\": \"Guest mode\", \"summary\": \"Lets a visitor read a shared doc without an account.\"}."
    )]
    async fn rdy_propose(&self, Parameters(p): Parameters<ProposeParams>) -> CallToolResult {
        self.run("rdy_propose", p, tools::rdy_propose).await
    }

    #[tool(
        description = "Read a change plan, or record answers in the session's view. Read a proposal, landing or decision with its selector. Page with next. answers is [{item, act, reason?}] or [{group: {kind?, follow_up?, doc?, edge?}, act, reason?}]; act is confirm, defer, done or dismiss. Update, rejustify and reaffirm items take defer; confirm items take confirm or defer; update_implementation items take done or defer; tension items take confirm or dismiss. Confirm, defer and dismiss need a reason. An update item also closes when you edit its block, and a reaffirm item with an rdy_write reaffirm op. One answer that matches no open item refuses the whole call. A write can close items: read the plan again after a write, check counts.open, and read every page before you build groups. rdy_propose plan_answers take item, not group."
    )]
    async fn rdy_plan(&self, Parameters(p): Parameters<PlanParams>) -> CallToolResult {
        self.run("rdy_plan", p, tools::rdy_plan).await
    }
}
