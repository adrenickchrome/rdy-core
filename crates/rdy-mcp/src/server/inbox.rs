//! The `inbox` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, InboxParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = inbox_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Read your inbox: the events that concern this session since your cursor, at most 200 compact entries {event, ids, line}, then the new cursor. Pass ack: true to move the cursor past them. Read it at session start and after an rdy://inbox notification. Pass person: true to read the person's inbox and show it to them in chat."
    )]
    async fn rdy_inbox(&self, Parameters(p): Parameters<InboxParams>) -> CallToolResult {
        self.run("rdy_inbox", p, tools::rdy_inbox).await
    }
}
