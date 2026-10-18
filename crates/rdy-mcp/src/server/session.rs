//! The `session` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, ScopeParams, SessionParams, TagParams, TagsParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = session_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Start a session: who you are (actor), your run, and role (reader, writer, reviewer). Required before other tools. Your writes go to a view named after your run, or work-<actor> without one; a reader may pass view \"canon\" to read canon by default. areas: the pack areas whose landings reach your inbox (rdy_inbox); [] clears them."
    )]
    async fn rdy_session(&self, Parameters(p): Parameters<SessionParams>) -> CallToolResult {
        self.run("rdy_session", p, tools::rdy_session).await
    }

    #[tool(
        description = "Project overview: pack, the folder and doc tree, views, canon history and the version position. Also coverage (the current score, its dimensions and the issue count) and the entity kinds. Coverage is for the current state only: at a past landing the overview leaves coverage out. The same holds for a proposal version."
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

    #[tool(
        description = "Name a landing (default: canon's head) as a version, e.g. v1.2.0, on the person's word. A tag is the person's act: ask them, then pass their words as evidence and your question. The tag records the person, and an answer records your relay. Every tag rule holds: the version is new and above the current one. With signing on, the act and its signatures share one batch. The result adds signatures. Under signing.sign auto, a signing failure saves the act unsigned and adds W_SIGNING_FAILED. Under always, the act is refused."
    )]
    async fn rdy_tag(&self, Parameters(p): Parameters<TagParams>) -> CallToolResult {
        self.run("rdy_tag", p, tools::rdy_tag).await
    }
}
