//! The `intake` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, AnswerParams, PendingParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = intake_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Read what waits for the person: {items, not_ready, snoozed, omitted}. Each item has a ref, kind (approve, confirm, answer, resolve, defer, reaffirm), options, card and card_hash. Each option has an id, a label and its consequences. The recommended option carries recommended: true and a reason of at most 200 characters; no other option carries either. A question block's own recommended field is different: it is the exact text of one option.\n\nShow the card to the person word for word. Keep its options, recommendation and consequences unchanged. A batch (approve) stands first, its exceptions (confirm) beside it. Pass session_ref (the chat session) so Later hides a card only there; max_tokens cuts each card. A defer card and a reaffirm card need the person's own choice and carry no recommended option; show both options and wait for the person. not_ready lists the versions that need work from you first."
    )]
    async fn rdy_pending(&self, Parameters(p): Parameters<PendingParams>) -> CallToolResult {
        self.run("rdy_pending", p, tools::rdy_pending).await
    }

    #[tool(
        description = "Relay the person's answer to one card: ref, option (an option id of the card), card_hash (of the card you showed) and the person's words as evidence. It records the person as the one who answered and you as the relay. A card or version that changed is refused with the fresh card in corrected; show it and ask again. An approval waits until every exception beside the batch has a confirm answer. Pass all_recommended: [{ref, card_hash}] to answer each item with its recommended option. all_recommended refuses a defer or a reaffirm card with E_BAD_INPUT (<ref> has no recommended option); answer each of them on its own. kind is card (default) or text. brief is for a question under stable content. With signing on, the act and its signatures share one batch. The result adds signatures. Under signing.sign auto, a signing failure saves the act unsigned and adds W_SIGNING_FAILED. Under always, the act is refused."
    )]
    async fn rdy_answer(&self, Parameters(p): Parameters<AnswerParams>) -> CallToolResult {
        self.run("rdy_answer", p, tools::rdy_answer).await
    }
}
