//! The `read` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, ReadParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = read_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Read a document, folder or subtree by id or path (e.g. wallets/transactions) as compact lines (default: each block's id, type and its whole statement, prompt, name or title, else its body's first sentence; references as bare ids), markdown or yaml. max_tokens pages long output by whole blocks: a cut read returns truncated, omitted (the blocks left) and next; pass next as page to continue. A block larger than max_tokens is shown alone and cut, and cut: {id, omitted_bytes} names it. scope is 'canon', an existing view's name (or your session's), 'tag:<version>' or 'landing:<id>'; an unknown view is E_NOT_FOUND. A compact line ends with [implements <decision>] or [editorial <class>] when the change that last wrote the block carried one; when the store cannot say which change that was, the markers are left out and warnings says why. `id` may be `id@version` or a ref line `[id@version] …`, read under that version. id may also be a proposal version, P-<id>@<n> or P-<id> (its latest): its title, proposer, changes with block counts, the conflicts and other reasons that block approving it, and its version chain. pattern keeps only matching blocks (a substring, or /regex/); <doc>#<heading-slug> reads one section."
    )]
    async fn rdy_read(&self, Parameters(p): Parameters<ReadParams>) -> CallToolResult {
        self.run("rdy_read", p, tools::rdy_read).await
    }
}
