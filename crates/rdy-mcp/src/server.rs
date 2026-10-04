use crate::tools::{self, *};
use rdy_engine::{Rdy, RdyError};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router, ServerHandler, ServiceExt,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct RdyServer {
    state: Arc<Mutex<McpState>>,
    tool_router: ToolRouter<Self>,
}

fn result(r: Result<serde_json::Value, RdyError>) -> CallToolResult {
    match r {
        Ok(v) => CallToolResult::success(vec![ContentBlock::text(v.to_string())]),
        Err(e) => CallToolResult::error(vec![ContentBlock::text(error_json(&e).to_string())]),
    }
}

#[tool_router]
impl RdyServer {
    pub fn new(rdy: Rdy) -> Self {
        RdyServer { state: Arc::new(Mutex::new(McpState::new(rdy))), tool_router: Self::tool_router() }
    }

    fn run<P>(&self, p: P, f: fn(&mut McpState, P) -> Result<serde_json::Value, RdyError>) -> CallToolResult {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        result(f(&mut st, p))
    }

    #[tool(description = "Start a session: who you are (actor), your run, and role (reader, writer, reviewer). Required before other tools. Your writes go to a view named after your run.")]
    fn rdy_session(&self, Parameters(p): Parameters<SessionParams>) -> CallToolResult { self.run(p, tools::rdy_session) }

    #[tool(description = "Project overview: pack, docs, views and canon history.")]
    fn rdy_overview(&self, Parameters(p): Parameters<ScopeParams>) -> CallToolResult { self.run(p, tools::rdy_overview) }

    #[tool(description = "Read a document or subtree as compact lines (default), markdown or yaml. scope is 'canon' or a view name.")]
    fn rdy_read(&self, Parameters(p): Parameters<IdParams>) -> CallToolResult { self.run(p, tools::rdy_read) }

    #[tool(description = "Everything about one block: fields, body, version (use as base for updates), doc, links in and out, decision status, governing decisions, suspects.")]
    fn rdy_context(&self, Parameters(p): Parameters<IdParams>) -> CallToolResult { self.run(p, tools::rdy_context) }

    #[tool(description = "How to write block types: fields, required values, allowed children and an example op. Omit block_type to list all types and document skeletons.")]
    fn rdy_schema(&self, Parameters(p): Parameters<SchemaParams>) -> CallToolResult { self.run(p, tools::rdy_schema) }

    #[tool(description = "Blast radius: blocks that depend on, reference or describe the given block, grouped by depth.")]
    fn rdy_impact(&self, Parameters(p): Parameters<IdParams>) -> CallToolResult { self.run(p, tools::rdy_impact) }

    #[tool(description = "References whose target changed, or whose decision was superseded or revoked.")]
    fn rdy_suspects(&self, Parameters(p): Parameters<ScopeParams>) -> CallToolResult { self.run(p, tools::rdy_suspects) }

    #[tool(description = "Atomic batch write of create/update/move/delete ops. Use id \"$name\" placeholders to refer to blocks created earlier in the same batch, including {{$name}} inside text. Reference blocks in text as {{id}}.")]
    fn rdy_write(&self, Parameters(p): Parameters<WriteParams>) -> CallToolResult { self.run(p, tools::rdy_write) }

    #[tool(description = "Propose a decision (status proposed; accepted when its proposal lands).")]
    fn rdy_decide(&self, Parameters(p): Parameters<DecideParams>) -> CallToolResult { self.run(p, tools::rdy_decide) }

    #[tool(description = "Propose a decision that supersedes an existing one, wholly or a named part.")]
    fn rdy_supersede(&self, Parameters(p): Parameters<SupersedeParams>) -> CallToolResult { self.run(p, tools::rdy_supersede) }

    #[tool(description = "Propose revoking an existing decision.")]
    fn rdy_revoke(&self, Parameters(p): Parameters<RevokeParams>) -> CallToolResult { self.run(p, tools::rdy_revoke) }

    #[tool(description = "Propose your view's changes for canon. A human approves it with rdy approve.")]
    fn rdy_propose(&self, Parameters(p): Parameters<ProposeParams>) -> CallToolResult { self.run(p, tools::rdy_propose) }

    #[tool(description = "Upload a binary asset (base64); returns the hash to put in a figure block's blob field.")]
    fn rdy_asset(&self, Parameters(p): Parameters<AssetParams>) -> CallToolResult { self.run(p, tools::rdy_asset) }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for RdyServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("rdy", env!("CARGO_PKG_VERSION")))
            .with_instructions("rdy: version control for design docs and decisions. Call rdy_session first, then rdy_overview. Write with rdy_write; propose with rdy_propose.")
    }
}

pub async fn serve_stdio(project: PathBuf) -> anyhow::Result<()> {
    let rdy = Rdy::open(&project)?;
    let service = RdyServer::new(rdy).serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
