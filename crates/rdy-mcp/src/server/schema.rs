//! The `schema` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, SchemaParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = schema_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "How to write block types: fields, required values, allowed children and an example op. Omit block_type to list all types and document skeletons."
    )]
    async fn rdy_schema(&self, Parameters(p): Parameters<SchemaParams>) -> CallToolResult {
        self.run("rdy_schema", p, tools::rdy_schema).await
    }
}
