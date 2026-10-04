use crate::tools::{self, *};
use rdy_engine::{E_INTERNAL, E_LIMIT, Rdy, RdyError};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        ServerCapabilities, ServerConfig,
    },
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// The tool name an over-long request line is rewritten to call (see [`pump_lines`]). It is not
/// listed by `tools/list`; calling it always answers `E_LIMIT`.
pub const REQUEST_TOO_LARGE: &str = "rdy_request_too_large";

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
        RdyServer {
            state: Arc::new(Mutex::new(McpState::new(rdy))),
            tool_router: Self::tool_router(),
        }
    }

    /// Runs one tool. A panic inside it becomes an `E_INTERNAL` reply (so the request is still
    /// answered) and drops the engine's caches, which the panic may have left half-updated.
    fn run<P>(
        &self,
        tool: &str,
        p: P,
        f: fn(&mut McpState, P) -> Result<serde_json::Value, RdyError>,
    ) -> CallToolResult {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut st, p)));
        match out {
            Ok(r) => result(r),
            Err(panic) => {
                st.rdy.invalidate();
                let msg = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".into());
                result(Err(RdyError::new(
                    E_INTERNAL,
                    format!("internal error in {tool}: {msg}"),
                    "this is a bug in rdy; the request may or may not have been applied: re-read before retrying",
                )))
            }
        }
    }

    #[tool(
        description = "Start a session: who you are (actor), your run, and role (reader, writer, reviewer). Required before other tools. Your writes go to a view named after your run."
    )]
    fn rdy_session(&self, Parameters(p): Parameters<SessionParams>) -> CallToolResult {
        self.run("rdy_session", p, tools::rdy_session)
    }

    #[tool(
        description = "Project overview: pack, the folder and doc tree, views, canon history and the version position."
    )]
    fn rdy_overview(&self, Parameters(p): Parameters<ScopeParams>) -> CallToolResult {
        self.run("rdy_overview", p, tools::rdy_overview)
    }

    #[tool(
        description = "Versions (frozen snapshots of canon, e.g. v1.2.0), oldest first, and the current version. Read one with scope \"tag:<version>\"."
    )]
    fn rdy_tags(&self, Parameters(p): Parameters<TagsParams>) -> CallToolResult {
        self.run("rdy_tags", p, tools::rdy_tags)
    }

    #[tool(
        description = "Read a document, folder or subtree by id or path (e.g. wallets/transactions) as compact lines (default), markdown or yaml. scope is 'canon', a view name, 'tag:<version>' or 'landing:<id>'."
    )]
    fn rdy_read(&self, Parameters(p): Parameters<IdParams>) -> CallToolResult {
        self.run("rdy_read", p, tools::rdy_read)
    }

    #[tool(
        description = "By id or path: everything about one block: fields, body, version (use as base for updates), doc, links in and out, decision status, governing decisions, suspects."
    )]
    fn rdy_context(&self, Parameters(p): Parameters<IdParams>) -> CallToolResult {
        self.run("rdy_context", p, tools::rdy_context)
    }

    #[tool(
        description = "How to write block types: fields, required values, allowed children and an example op. Omit block_type to list all types and document skeletons."
    )]
    fn rdy_schema(&self, Parameters(p): Parameters<SchemaParams>) -> CallToolResult {
        self.run("rdy_schema", p, tools::rdy_schema)
    }

    #[tool(
        description = "By id or path: blast radius: blocks that depend on, reference or describe the given block, grouped by depth."
    )]
    fn rdy_impact(&self, Parameters(p): Parameters<IdParams>) -> CallToolResult {
        self.run("rdy_impact", p, tools::rdy_impact)
    }

    #[tool(
        description = "References whose target changed, or whose decision was superseded or revoked."
    )]
    fn rdy_suspects(&self, Parameters(p): Parameters<ScopeParams>) -> CallToolResult {
        self.run("rdy_suspects", p, tools::rdy_suspects)
    }

    #[tool(
        description = "Atomic batch write of create/update/move/delete ops. Use id \"$name\" placeholders to refer to blocks created earlier in the same batch, including {{$name}} inside text. Reference blocks in text as {{id}}."
    )]
    fn rdy_write(&self, Parameters(p): Parameters<WriteParams>) -> CallToolResult {
        self.run("rdy_write", p, tools::rdy_write)
    }

    #[tool(description = "Propose a decision (status proposed; accepted when its proposal lands).")]
    fn rdy_decide(&self, Parameters(p): Parameters<DecideParams>) -> CallToolResult {
        self.run("rdy_decide", p, tools::rdy_decide)
    }

    #[tool(
        description = "Propose a decision that supersedes an existing one, wholly or a named part."
    )]
    fn rdy_supersede(&self, Parameters(p): Parameters<SupersedeParams>) -> CallToolResult {
        self.run("rdy_supersede", p, tools::rdy_supersede)
    }

    #[tool(description = "Propose revoking an existing decision.")]
    fn rdy_revoke(&self, Parameters(p): Parameters<RevokeParams>) -> CallToolResult {
        self.run("rdy_revoke", p, tools::rdy_revoke)
    }

    #[tool(
        description = "Propose your view's changes for canon. A human approves it with rdy approve."
    )]
    fn rdy_propose(&self, Parameters(p): Parameters<ProposeParams>) -> CallToolResult {
        self.run("rdy_propose", p, tools::rdy_propose)
    }

    #[tool(
        description = "Upload a binary asset (base64); returns the hash to put in a figure block's blob field."
    )]
    fn rdy_asset(&self, Parameters(p): Parameters<AssetParams>) -> CallToolResult {
        self.run("rdy_asset", p, tools::rdy_asset)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for RdyServer {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        if request.name == REQUEST_TOO_LARGE {
            let mb = tools::MAX_REQUEST_BYTES / (1024 * 1024);
            return Ok(result(Err(RdyError::new(
                E_LIMIT,
                format!("{REQUEST_TOO_LARGE}: the request line was over {mb} MiB and was not read"),
                "send smaller requests: split writes into batches and upload assets one at a time",
            )))
            .into());
        }
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        self.tool_router.call(tcc).await
    }

    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("rdy", env!("CARGO_PKG_VERSION")))
            .with_instructions("rdy: version control for design docs and decisions. Call rdy_session first, then rdy_overview. Write with rdy_write; propose with rdy_propose.")
    }
}

/// The JSON-RPC id in the first bytes of a request line, as raw JSON (a number or a string).
fn request_id(prefix: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(prefix);
    let at = s.find("\"id\"")? + 4;
    let rest = s[at..].trim_start().strip_prefix(':')?.trim_start();
    if let Some(q) = rest.strip_prefix('"') {
        let end = q.find('"')?;
        return Some(format!("\"{}\"", &q[..end]));
    }
    let n: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    (!n.is_empty()).then_some(n)
}

/// Copies `r` to `w` line by line. A line longer than `max` bytes is never buffered whole: if a
/// JSON-RPC `"id"` can be read from its first 4 KiB it is replaced by a call to the unlisted
/// [`REQUEST_TOO_LARGE`] tool (so the client gets an error reply for its id); otherwise dropped.
pub async fn pump_lines<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    r: R,
    mut w: W,
    max: usize,
) -> std::io::Result<()> {
    const HEAD: usize = 4096;
    let mut r = BufReader::new(r);
    let mut line: Vec<u8> = Vec::new();
    let mut over = false;
    let mut head: Vec<u8> = Vec::new();
    loop {
        let buf = r.fill_buf().await?;
        if buf.is_empty() {
            if !over && !line.is_empty() {
                w.write_all(&line).await?;
                w.flush().await?;
            }
            return Ok(());
        }
        let (chunk, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (&buf[..=i], true),
            None => (buf, false),
        };
        let n = chunk.len();
        if !over {
            line.extend_from_slice(chunk);
            // `max` counts the line without its newline
            if line.len() > max + 1 || (!done && line.len() > max) {
                over = true;
                head = line[..line.len().min(HEAD)].to_vec();
                line = Vec::new();
            }
        } else if head.len() < HEAD {
            head.extend_from_slice(&chunk[..chunk.len().min(HEAD - head.len())]);
        }
        r.consume(n);
        if done {
            if over {
                if let Some(id) = request_id(&head) {
                    let call = format!(
                        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"tools/call\",\"params\":{{\"name\":\"{REQUEST_TOO_LARGE}\",\"arguments\":{{}}}}}}\n"
                    );
                    w.write_all(call.as_bytes()).await?;
                }
            } else {
                w.write_all(&line).await?;
            }
            w.flush().await?;
            line.clear();
            head.clear();
            over = false;
        }
    }
}

pub async fn serve_stdio(project: PathBuf) -> anyhow::Result<()> {
    let rdy = Rdy::open(&project)?;
    let (to_server, from_pump) = tokio::io::duplex(1 << 16);
    tokio::spawn(async move {
        let _ = pump_lines(tokio::io::stdin(), to_server, tools::MAX_REQUEST_BYTES).await;
    });
    let service = RdyServer::new(rdy)
        .serve((from_pump, tokio::io::stdout()))
        .await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pump(input: &str, max: usize) -> String {
        let (mut w, r) = (Vec::new(), input.as_bytes());
        pump_lines(r, &mut w, max).await.unwrap();
        String::from_utf8(w).unwrap()
    }

    #[tokio::test]
    async fn short_lines_pass_and_long_ones_become_an_error_call() {
        let long = format!(
            r#"{{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{{"x":"{}"}}}}"#,
            "y".repeat(100)
        );
        let out = pump(&format!("{{\"a\":1}}\n{long}\n{{\"b\":2}}\n"), 64).await;
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "{\"a\":1}");
        assert!(
            lines[1].contains("\"id\":7") && lines[1].contains("rdy_request_too_large"),
            "{}",
            lines[1]
        );
        assert_eq!(lines[2], "{\"b\":2}");
        // no id to answer: dropped
        let out = pump(&format!("{}\n{{\"c\":3}}\n", "z".repeat(100)), 64).await;
        assert_eq!(out.lines().collect::<Vec<_>>(), vec!["{\"c\":3}"]);
    }
}
