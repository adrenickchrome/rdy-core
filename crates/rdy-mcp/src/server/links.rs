//! The `links` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, LinksParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = links_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Read the repo links of this project. op list (default): every link with its alias, repo, state (none, delivered or revoked), last delivery and the pins that are behind, and the delivered repos no link names. op show with alias: what that link delivers, each block with its pin state, and the instructions it has not adopted. op refs, with or without alias: the pins of canon to cited content, by repo. A cited block reads as rdy_read {id: \"<alias>:<id>\"}. It writes nothing; a delivery moves no pin, and an adopt card in rdy_pending asks the person."
    )]
    async fn rdy_links(&self, Parameters(p): Parameters<LinksParams>) -> CallToolResult {
        self.run("rdy_links", p, tools::rdy_links).await
    }
}
