//! The `session` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, ScopeParams, SessionParams, TagsParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = session_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Start a session: who you are (actor), your run, and role (reader, writer, reviewer). Required before other tools. Your writes go to a view named after your run, or work-<actor> without one; a reader may pass view \"canon\" to read canon by default."
    )]
    async fn rdy_session(&self, Parameters(p): Parameters<SessionParams>) -> CallToolResult {
        self.run("rdy_session", p, tools::rdy_session).await
    }

    #[tool(
        description = "Project overview: pack, the folder and doc tree, views, canon history and the version position. Also coverage (the current score, its dimensions and the issue count) and the entity kinds. Coverage is for the current state only, so at a past landing the overview is refused as a whole: read canon or a view."
    )]
    async fn rdy_overview(&self, Parameters(p): Parameters<ScopeParams>) -> CallToolResult {
        self.run("rdy_overview", p, tools::rdy_overview).await
    }

    #[tool(
        description = "Versions (frozen snapshots of canon, e.g. v1.2.0), oldest first, and the current version. Read one with scope \"tag:<version>\"."
    )]
    async fn rdy_tags(&self, Parameters(p): Parameters<TagsParams>) -> CallToolResult {
        self.run("rdy_tags", p, tools::rdy_tags).await
    }
}
