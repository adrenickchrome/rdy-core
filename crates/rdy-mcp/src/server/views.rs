//! The `views` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::view_tools;
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = views_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Create, fork, include, exclude, replay, compare, list, show, close, own, resolve or undo views. op is required: create {name, base?, pin?}; fork {from, name}; include {changes, view?}; exclude {changes, cascade?, view?}; undo {view?, target?, cascade?}; close {name, reason?}; resolve {block, landed: true, view?}; list {all?, stale?}; show {view?}; compare {a?, b?}; rdy_schema \"views\" gives own and replay. To drop one change, exclude it or undo its op. A closed view comes back with an undo of its close op, or a fork. Include, exclude, replay, close and undo need ownership of the target view: the actor and run that made it. Include, exclude, replay and undo return the op's consequences, with conflict entries to resolve. Replay with from_file ports landed changes from a dump of another project, with brief as the target briefing."
    )]
    async fn rdy_view(&self, Parameters(p): Parameters<view_tools::ViewParams>) -> CallToolResult {
        self.run("rdy_view", p, view_tools::rdy_view).await
    }

    #[tool(
        description = "Canon's landings, newest first: who approved which proposal version, how (assurance, relay, evidence), and the tags on each. Each row names the proposer (proposed_by) and the approver. limit and since page it. Pass verify: true to check signatures offline. Each row adds signatures and tag_signatures with validity, coverage, local trust, key type and presence. Empty lists mean unsigned. Verification needs no SSH agent or hub. Pass landing (a landing id of at least 6 hex characters, landing:<id> or tag:<version>) to read one landing: its row and its change list, a compact diff from its previous landing. max_tokens cuts the change list, and page continues it."
    )]
    async fn rdy_log(&self, Parameters(p): Parameters<view_tools::LogParams>) -> CallToolResult {
        self.run("rdy_log", p, view_tools::rdy_log).await
    }
}
