//! The `store` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, DumpParams, LoadParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = store_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Write this whole project to one dump file: every object with its hash, the landings, tags, views, proposals, reviews and inboxes. rdy_load rebuilds the project from it. It never overwrites a file."
    )]
    async fn rdy_dump(&self, Parameters(p): Parameters<DumpParams>) -> CallToolResult {
        self.run("rdy_dump", p, tools::rdy_dump).await
    }

    #[tool(
        description = "Rebuild a project from a dump file in dir, a directory with no rdy store. Every hash, landing, tag and view is kept; the load builds the index and runs fsck. To move this project across a store format change, give the person these steps, in this order: close every session on this project; with rdy 0.3.0 or a later rdy that reads store format <n>, run rdy dump --out <file>, then rdy serve --stop; move .rdy/store aside; then run rdy load <file> . with this rdy."
    )]
    async fn rdy_load(&self, Parameters(p): Parameters<LoadParams>) -> CallToolResult {
        self.run("rdy_load", p, tools::rdy_load).await
    }
}
