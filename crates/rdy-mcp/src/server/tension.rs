//! The `tension` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, ResolveParams, TensionsParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = tension_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Tensions between in-flight changes, between a change and canon, or inside canon: kind, severity, sides and status (open or dismissed). Filter by scope (inflight, canon or a view), kind, severity, status, view, area or involving (a block or a change). A blocking tension refuses a landing only when a side is in it and every side would be in canon."
    )]
    async fn rdy_tensions(&self, Parameters(p): Parameters<TensionsParams>) -> CallToolResult {
        self.run("rdy_tensions", p, tools::rdy_tensions).await
    }

    #[tool(
        description = "Dismiss a tension with a reason (tension, dismiss), dismiss or restore a passing entity mention (mention: {block, entity}, dismiss or restore), or relay a person's acceptance of the reviewers' advice to block on a proposal's semantic tensions (accept: {proposal: P-x@n, advised_digest, evidence, question?, session_ref?, tensions?}; advised_digest is the one rdy_propose or rdy_read P-x@n gave with the tensions the person was shown). The tension form is {tension, dismiss: {reason, evidence?, question?, session_ref?}}, with no scope, view, id or act. The mention form is {mention, dismiss or restore: {reason}, scope?}. Example: {\"tension\": \"tn-0123456789ab\", \"dismiss\": {\"reason\": \"<why it needs no action>\"}}. A writer cannot dismiss a blocking tension: relay the person's dismissal with their words in dismiss.evidence. A tension is otherwise resolved by a change."
    )]
    async fn rdy_resolve(&self, Parameters(p): Parameters<ResolveParams>) -> CallToolResult {
        self.run("rdy_resolve", p, tools::rdy_resolve).await
    }
}
