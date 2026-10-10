//! The `write` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, WriteParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = write_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Atomic batch write of ops (create, update, move, delete, rebalance, repin; rdy_schema lists them under \"ops\"). Place a block with after or before a sibling, or neither to append. Use id \"$name\" placeholders to refer to blocks created earlier in the same batch, including {{$name}} inside text. Reference blocks in text as {{id}}, or {{id|your words}} to choose the link text; a block's key (fields.key, e.g. REQ-12) works wherever an id does; a bare {{KEY}} is stored as {{id|@key}} and {{KEY|words}} as {{id|words}}. Bodies take a small Markdown subset (rdy_schema \"text\"). links: {\"<kind>\": [{\"target\": \"<id or path>\"}]}. An update replaces only the link kinds it names; \"<kind>\": [] removes that kind. Cite brief: <token> from rdy_brief for stable content; the response lists the write's consequences."
    )]
    async fn rdy_write(&self, Parameters(p): Parameters<WriteParams>) -> CallToolResult {
        self.run("rdy_write", p, tools::rdy_write).await
    }
}
