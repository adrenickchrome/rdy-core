//! The `review` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, ReviewNextParams, ReviewParams, ReviewSubmitParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = review_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "List the checks, run one on your work, request an independent review, or list requests and verdicts. A run records an author-run request and returns its case. When you request a review for a person, pass their evidence. Checks inform the approval card; only a person's acceptance can make a check verdict block a version."
    )]
    async fn rdy_review(&self, Parameters(p): Parameters<ReviewParams>) -> CallToolResult {
        self.run("rdy_review", p, tools::rdy_review).await
    }

    #[tool(
        description = "Take the next independent review request of kind tension, a named check, or any. The reply gives its case and case_hash. The request is leased to this session under reviews.lease_ttl. Needs the reviewer role. Answer with rdy_review_submit."
    )]
    async fn rdy_review_next(&self, Parameters(p): Parameters<ReviewNextParams>) -> CallToolResult {
        self.run("rdy_review_next", p, tools::rdy_review_next).await
    }

    #[tool(
        description = "Answer the case you read. A contradiction takes tension or no_tension. Other checks take pass, concern or fail. Concern and fail need citations and a reason. Independent reviews need another actor with the reviewer role. Only the requesting actor answers an author-run check."
    )]
    async fn rdy_review_submit(
        &self,
        Parameters(p): Parameters<ReviewSubmitParams>,
    ) -> CallToolResult {
        self.run("rdy_review_submit", p, tools::rdy_review_submit)
            .await
    }
}
