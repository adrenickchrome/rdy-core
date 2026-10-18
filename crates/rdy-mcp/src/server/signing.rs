//! The `signing` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, KeyShowParams, SignParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = signing_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Sign an existing landing or tag through the caller's SSH agent. Pass target and optional kind: approval, relay or tag. A human signs their own act; an agent signs a relay or the person's relayed act. The result reports validity, coverage, local trust, key type and presence. Presence is a signature flag, not proof that the person read the act. Explicit signing works when signing.sign is never."
    )]
    async fn rdy_sign(&self, Parameters(p): Parameters<SignParams>) -> CallToolResult {
        self.run("rdy_sign", p, tools::rdy_sign).await
    }

    #[tool(
        description = "Show the configured person and machine agent keys, their types and whether the caller's SSH agent holds them. The result is {key, agent_key, sign}. Unset or ignored keys are null. This call needs no hub."
    )]
    async fn rdy_key_show(&self, Parameters(p): Parameters<KeyShowParams>) -> CallToolResult {
        self.run("rdy_key_show", p, tools::rdy_key_show).await
    }
}
