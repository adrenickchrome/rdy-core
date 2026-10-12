//! The `review` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, ReviewNextParams, ReviewSubmitParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = review_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Take the next review request of kind tension: two blocks that may disagree, with their case file and case_hash, leased to this session for reviews.lease_ttl minutes (default 10). Needs the reviewer role. Answer with rdy_review_submit."
    )]
    async fn rdy_review_next(&self, Parameters(p): Parameters<ReviewNextParams>) -> CallToolResult {
        self.run("rdy_review_next", p, tools::rdy_review_next).await
    }

    #[tool(
        description = "Answer a review request: verdict tension, with findings that cite the blocks, or no_tension; pass the case_hash you read. A tension verdict opens an advisory tension; severity blocking with a reason advises a person to make it block, and gates nothing until the person accepts. Needs the reviewer role and an actor other than the blocks' authors."
    )]
    async fn rdy_review_submit(
        &self,
        Parameters(p): Parameters<ReviewSubmitParams>,
    ) -> CallToolResult {
        self.run("rdy_review_submit", p, tools::rdy_review_submit)
            .await
    }
}
