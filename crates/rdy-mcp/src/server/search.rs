//! The `search` family's tool registration.

use crate::params::Parameters;
use crate::search_tools::{self, SearchParams};
use crate::server::RdyServer;
use crate::tools;
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = search_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Find text in blocks: a literal (or, with regex: true, a pattern) in every block's body and string fields, code included, within scope (default: your session's view). The result starts with totals (hits per doc path and per block type). Each entry gives the block's id, type, path and matching lines (field, line number, text); blocks with identical matching lines collapse into one entry whose also lists the others. A literal of several words finds the exact phrase first, then blocks holding all the words (all_words: true), each in reading order. Filters combine with AND: types, traits, path, lifecycle, changed_since, exclude_history, governed_by, describes, linked_to, suspect, tension. A match inside a superseded, rejected or revoked decision carries history: true. brief: true adds a registered briefing over the page's hits: cite its base.token to write them. Deterministic, no model. A cut result carries next: pass it as page with the same query and filters to continue. References are stored as ids, so search a block's id to find text that references it. A literal query that names an entity (its key, name or alias, any case) also returns its fact sheet in entities on the first page: definition, value, facts, governing decisions, dependents. Such a literal matches text in any case and spacing (the result says ignore_case: true), so every unlinked mention comes back. A match drops out only when a returned sheet lists it and it is tied to that entity (a filtered search keeps every match); each other match that names the entity carries a suggestion to link it. Each hit carries its coverage, its open coverage issues and a confidence level; coverage_below keeps hits under a score; sort: coverage lists the lowest first. Coverage is for the current state only: at a past landing hits carry no coverage, and coverage_below or sort is refused. The same holds for a proposal version."
    )]
    async fn rdy_search(&self, Parameters(p): Parameters<SearchParams>) -> CallToolResult {
        self.run("rdy_search", p, search_tools::rdy_search).await
    }

    #[tool(
        description = "Filter blocks with an expression, such as type = decision and status = live. Returns matches {id, block_type, path, summary} in reading order, and total. scope defaults to your session's view. max_tokens defaults to 4000; a cut result carries next: pass it as page with the same filter and scope. rdy_schema's filter gives the grammar. A parse error names the column. rdy_search finds text; rdy_query selects by fields and links."
    )]
    async fn rdy_query(&self, Parameters(p): Parameters<tools::QueryParams>) -> CallToolResult {
        self.run("rdy_query", p, tools::rdy_query).await
    }
}
