//! The `brief` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, BriefParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = brief_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Brief before editing: for blocks (ids) or a doc or folder (path), the decisions that govern them, what depends on them, their lineage (the decisions they refine, supersede or revoke, and the ones that do so to them), what will go stale if they change, their open suspects, work in flight (other views and open proposals that change them), and their current versions with the briefing token. Cite the token as brief on your next write; writes to stable content need it. To create a block, brief its parent. A cut briefing carries next: pass page: <next> alone (same scope) to continue it. For a code file, pass code_path alone: the blocks its commits implement (Implements: trailers) and the blocks whose reflects globs match it, plus a code section (provisional when a cited decision is still proposed, rework when it was rejected). A briefing also gives coverage (score, dimensions, pages, issues, confidence and reasons) for what it covers (coverage is for the current state only: at a past landing it leaves coverage out, and so in a proposal version), for a new doc the blocks that mention it, and for a decision its related decisions."
    )]
    async fn rdy_brief(&self, Parameters(p): Parameters<BriefParams>) -> CallToolResult {
        self.run("rdy_brief", p, tools::rdy_brief).await
    }
}
