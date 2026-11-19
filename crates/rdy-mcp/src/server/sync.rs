//! The `sync` family's tool registration: `rdy_push` and `rdy_pull`.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, PullParams, PushParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = sync_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Push canon, and the views you name, to a share remote's hub repo, or restore a dump into it (restore takes no views and no force). A push that sends the force move, and a restore into a hub repo that holds canon, return a card first (status confirmation_required, with card_hash, the remote, the tips before and after and each landing that leaves canon) and write nothing. Show the card to the person, then call again with the same inputs plus card_hash, their answer and their words as evidence. A card that changed since is refused: show the new one. force sends the force move only when local canon does not hold the hub tip; else the reply says force not needed and no card is returned. A push that may have reached the hub with no reply is not repeated: run rdy_push or rdy_pull again, and rdy finds its outcome first. Relay each hub refusal with its fix, the held request, the warnings, and for a restore the tags sent without render inputs and the hub views left as they are. Needs the writer role."
    )]
    async fn rdy_push(&self, Parameters(p): Parameters<PushParams>) -> CallToolResult {
        // a closed, size-checked result: no pack notice rides on it
        self.run_closed("rdy_push", p, tools::rdy_push).await
    }

    #[tool(
        description = "Pull canon, and the views you name, from a share remote's hub repo. Without take, local landings replay on hub canon in a pull view; strict stops at the first conflict. With take and no views, local canon moves to hub canon with no replay: a card comes first (status confirmation_required, with card_hash, the tips before and after and each landing that leaves canon); show it to the person, then call again with the same inputs plus card_hash, their answer and their words as evidence. With take and views, only those views are taken and canon pulls as usual, with no card. A view the hub lacks is refused. A pull that may have reached the hub with no reply is not repeated: run it again, and rdy finds the outcome first. Relay the result in full: views taken or refused, tags taken and kept, conflicts, the held request, warnings and each hub refusal with its fix. Needs the writer role."
    )]
    async fn rdy_pull(&self, Parameters(p): Parameters<PullParams>) -> CallToolResult {
        self.run_closed("rdy_pull", p, tools::rdy_pull).await
    }
}
