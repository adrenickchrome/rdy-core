//! The `store` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, CloneParams, DumpParams, LoadParams, StreamCheckParams, UpgradeParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = store_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Write this whole project to one dump file: every object with its hash, the landings, tags, views, proposals, reviews and inboxes. rdy_load rebuilds the project from it. It never overwrites a file. Give after, an earlier dump of this project, to write only the batches since its end; when nothing is newer, no file is written and out is null."
    )]
    async fn rdy_dump(&self, Parameters(p): Parameters<DumpParams>) -> CallToolResult {
        self.run("rdy_dump", p, tools::rdy_dump).await
    }

    #[tool(
        description = "Rebuild a project from a dump file in dir, a directory with no rdy store. Give then, the increments of the dump in chain order, to load the whole chain, and stop_at (landing:<hash>, tag:<name> or pos:<segment>.<offset>) to stop at the end of one batch; a stop before a content removal returns W_STOP_BEFORE_REDACTION warnings. Every hash, landing, tag and view is kept; the load builds the index and runs fsck. To move this project across a store format change, give the person these steps, in this order: close every session on this project; with rdy 0.3.0 or a later rdy that reads store format <n>, run rdy dump --out <file>, then rdy serve --stop; move .rdy/store aside; then run rdy load <file> . with this rdy."
    )]
    async fn rdy_load(&self, Parameters(p): Parameters<LoadParams>) -> CallToolResult {
        self.run("rdy_load", p, tools::rdy_load).await
    }

    #[tool(
        description = "Clone this project, or the dump file named by file, into dir, a directory with no rdy store. The clone keeps every hash, landing, tag and view, keeps the repo id and records its origin; fork gives it a new repo id. To clone another project, call rdy_dump on its own server, then pass the file here."
    )]
    async fn rdy_clone(&self, Parameters(p): Parameters<CloneParams>) -> CallToolResult {
        self.run("rdy_clone", p, tools::rdy_clone).await
    }

    #[tool(
        description = "Inspect this project's store upgrade and show the last upgrade receipt. Set dry_run to true to inspect without writes."
    )]
    async fn rdy_upgrade(&self, Parameters(p): Parameters<UpgradeParams>) -> CallToolResult {
        self.run("rdy_upgrade", p, tools::rdy_upgrade).await
    }

    #[tool(
        description = "Check one upload batch without opening a store. Return its record keys in order. Complete upload checks run when all batches are taken."
    )]
    async fn rdy_stream_check(
        &self,
        Parameters(p): Parameters<StreamCheckParams>,
    ) -> CallToolResult {
        // a closed report: no pack notice rides on it (the notice stays for the next reply)
        self.run_closed("rdy_stream_check", p, tools::rdy_stream_check)
            .await
    }
}
