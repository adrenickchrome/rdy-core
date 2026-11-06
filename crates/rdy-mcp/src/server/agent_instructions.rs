//! The `agent_instructions` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, InstructionsParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = agent_instructions_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Your rules for this task: call at the start of every task. Returns every required instruction in full, with its reasons, and the menu of rulesets (id, name, when, layer, required, estimated tokens, hints) from this project and your person layer (the meta project). Pass full: true to get every built-in ruleset in full. Pass load with menu ids to get those rulesets in full as well. Hints never select rules: you choose what to load. Reads canon only."
    )]
    async fn rdy_instructions(
        &self,
        Parameters(p): Parameters<InstructionsParams>,
    ) -> CallToolResult {
        self.run_closed("rdy_instructions", p, tools::rdy_instructions)
            .await
    }
}
