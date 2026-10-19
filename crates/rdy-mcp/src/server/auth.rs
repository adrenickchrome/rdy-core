//! The `auth` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, AuthParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = auth_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Start hub sign-in, show its state, list saved sign-ins, or sign out. Start returns a confirmation card. Show the card to the person, then send its card_hash, their answer and their words as evidence. Show the returned user code and code-free verification link. Relay warnings. Never ask for a token."
    )]
    async fn rdy_auth(&self, Parameters(p): Parameters<AuthParams>) -> CallToolResult {
        self.run("rdy_auth", p, tools::rdy_auth).await
    }
}
