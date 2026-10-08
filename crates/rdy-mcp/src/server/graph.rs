//! The `graph` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, IdParams, ScopeParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = graph_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "By id or path: everything about one block: fields, body, version (use as base for updates), doc, links in and out, decision status, governing decisions, suspects. Also the block's history (newest 20 versions: change, author, justification, landing and approver) and the justification of the change that last wrote it. history_more says the block has older versions than those 20; truncated and omitted say max_tokens cut entries from the reply. `id` may be `id@version` or a ref line `[id@version] …`, read under that version."
    )]
    async fn rdy_context(&self, Parameters(p): Parameters<IdParams>) -> CallToolResult {
        self.run("rdy_context", p, tools::rdy_context).await
    }

    #[tool(
        description = "By id or path: blast radius: blocks that depend on the given block through references or dependency links (depends_on, conditional_on, implements, decided_by, describes, affects), by depth. Decisions that refine or supersede it are listed at depth 1 and not followed. Each item's via says which edge reached it: via.kind (references or a link name) to via.from."
    )]
    async fn rdy_impact(&self, Parameters(p): Parameters<IdParams>) -> CallToolResult {
        self.run("rdy_impact", p, tools::rdy_impact).await
    }

    #[tool(
        description = "References and premises (depends_on, conditional_on, implements, decided_by) whose target changed or was deleted since the block pinned it, or links of any kind to a decision that was superseded or revoked. After checking a \"changed\" suspect, clear it with a repin op in rdy_write."
    )]
    async fn rdy_suspects(&self, Parameters(p): Parameters<ScopeParams>) -> CallToolResult {
        self.run("rdy_suspects", p, tools::rdy_suspects).await
    }
}
