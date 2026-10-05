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
        description = "Start a session: who you are (actor), your run, and role (reader, writer, reviewer). Required before other tools. Your writes go to a view named after your run; a reader may pass view \"canon\" to read canon by default."
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
        description = "Read a document, folder or subtree by id or path (e.g. wallets/transactions) as compact lines (default: each block's id, type and its whole statement, prompt, name or title, else its body's first sentence; references as bare ids), markdown or yaml. max_tokens cuts long output. scope is 'canon', an existing view's name (or your session's), 'tag:<version>' or 'landing:<id>'; an unknown view is E_NOT_FOUND. `id` may be `id@version` or a ref line `[id@version] …`, read under that version."
    )]
    fn rdy_read(&self, Parameters(p): Parameters<IdParams>) -> CallToolResult {
        self.run("rdy_read", p, tools::rdy_read)
    }

    #[tool(
        description = "By id or path: everything about one block: fields, body, version (use as base for updates), doc, links in and out, decision status, governing decisions, suspects. `id` may be `id@version` or a ref line `[id@version] …`, read under that version."
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
        description = "By id or path: blast radius: blocks that depend on the given block through references or dependency links (depends_on, conditional_on, implements, decided_by, describes, affects), by depth. Decisions that refine or supersede it are listed at depth 1 and not followed. Each item's via says which edge reached it: via.kind (references or a link name) to via.from."
    )]
    fn rdy_impact(&self, Parameters(p): Parameters<IdParams>) -> CallToolResult {
        self.run("rdy_impact", p, tools::rdy_impact)
    }

    #[tool(
        description = "References and premises (depends_on, conditional_on, implements, decided_by) whose target changed or was deleted since the block pinned it, or links of any kind to a decision that was superseded or revoked. After checking a \"changed\" suspect, clear it with a repin op in rdy_write."
    )]
    fn rdy_suspects(&self, Parameters(p): Parameters<ScopeParams>) -> CallToolResult {
        self.run("rdy_suspects", p, tools::rdy_suspects)
    }

    #[tool(
        description = "Atomic batch write of ops (create, update, move, delete, rebalance, repin; rdy_schema lists them under \"ops\"). Place a block with after or before a sibling, or neither to append. Use id \"$name\" placeholders to refer to blocks created earlier in the same batch, including {{$name}} inside text. Reference blocks in text as {{id}}, or {{id|your words}} to choose the link text; a block's key (fields.key, e.g. REQ-12) works wherever an id does and is stored as the id. Bodies take a small Markdown subset (rdy_schema \"text\"). links: {\"<kind>\": [{\"target\": \"<id or path>\"}]}. An update replaces only the link kinds it names; \"<kind>\": [] removes that kind."
    )]
    fn rdy_write(&self, Parameters(p): Parameters<WriteParams>) -> CallToolResult {
        self.run("rdy_write", p, tools::rdy_write)
    }

    #[tool(description = "Propose a decision (status proposed; accepted when its proposal lands).")]
    fn rdy_decide(&self, Parameters(p): Parameters<DecideParams>) -> CallToolResult {
        self.run("rdy_decide", p, tools::rdy_decide)
    }

    #[tool(
        description = "Propose a decision that supersedes an existing one, wholly or a named part. It inherits the old decision's affects, depends_on, conditional_on and refines links unless you give that kind (an empty list drops it); the result's inherited lists what was copied."
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

/// The longest id value [`IdScanner`] will capture; a longer one counts as no id.
const MAX_ID_BYTES: usize = 1024;

/// Why [`pump_lines`] dropped an over-long line instead of answering it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dropped {
    /// A JSON object, but no valid top-level `"id"` (a string or a number).
    NoTopLevelId,
    /// A top-level `"id"` longer than [`MAX_ID_BYTES`] bytes (and no valid one besides).
    IdOverLimit,
    /// The line does not start a JSON object (an array, a scalar or junk).
    NotAnObject,
}

impl std::fmt::Display for Dropped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Dropped::NoTopLevelId => f.write_str("no top-level JSON-RPC id"),
            Dropped::IdOverLimit => write!(f, "id longer than {MAX_ID_BYTES} bytes"),
            Dropped::NotAnObject => f.write_str("the line is not a JSON object"),
        }
    }
}

/// Finds the JSON-RPC `"id"` of a request line in one pass, in constant memory, without
/// buffering the line: it tracks string and escape state and nesting depth, and captures the
/// value of the `"id"` key of the top-level object wherever it occurs (the TypeScript SDK
/// writes it after `params`). Keys and values at deeper levels are ignored.
#[derive(Default)]
struct IdScanner {
    depth: usize,
    top_is_object: bool,
    in_string: bool,
    escaped: bool,
    /// At depth 1 of the top-level object, the next string is a key.
    expect_key: bool,
    /// Bytes of the depth-1 key being read (capped: only `id` matters).
    key: Option<Vec<u8>>,
    /// The depth-1 key just closed was `id`; `Some` until its `:`.
    key_was_id: Option<bool>,
    /// After `"id":`, waiting for the value's first byte.
    value_is_id: bool,
    /// The raw bytes of the id value being captured.
    capture: Option<Vec<u8>>,
    capture_is_string: bool,
    /// A top-level id was seen but was longer than `MAX_ID_BYTES`.
    id_over_limit: bool,
    /// The first valid top-level id, as JSON.
    id: Option<String>,
}

impl IdScanner {
    fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.byte(b);
        }
    }

    fn byte(&mut self, b: u8) {
        if self.in_string {
            // keep up to 4 bytes: `id` plus its closing quote is 3, so 4 tells `id` from `idx`
            if let Some(k) = &mut self.key
                && k.len() < 4
            {
                k.push(b);
            }
            if self.capture_is_string
                && let Some(c) = &mut self.capture
            {
                c.push(b);
            }
            if self.escaped {
                self.escaped = false;
            } else if b == b'\\' {
                self.escaped = true;
            } else if b == b'"' {
                self.in_string = false;
                if let Some(k) = self.key.take() {
                    self.key_was_id = Some(k == b"id\"");
                }
                if self.capture_is_string {
                    self.finish_capture();
                }
            }
            if self
                .capture
                .as_ref()
                .is_some_and(|c| c.len() > MAX_ID_BYTES)
            {
                self.capture = None;
                self.capture_is_string = false;
                self.id_over_limit = true;
            }
            return;
        }
        if let Some(c) = &mut self.capture {
            if matches!(b, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                c.push(b);
                if c.len() > MAX_ID_BYTES {
                    self.capture = None;
                    self.id_over_limit = true;
                }
                return;
            }
            self.finish_capture();
        }
        if b.is_ascii_whitespace() {
            return;
        }
        let at_top = self.depth == 1 && self.top_is_object;
        if at_top && self.value_is_id {
            self.value_is_id = false;
            if b == b'"' {
                self.capture = Some(vec![b]);
                self.capture_is_string = true;
            } else if b == b'-' || b.is_ascii_digit() {
                self.capture = Some(vec![b]);
                return;
            }
        }
        match b {
            b'"' => {
                self.in_string = true;
                if at_top && self.expect_key {
                    self.expect_key = false;
                    self.key = Some(Vec::new());
                }
            }
            b'{' | b'[' => {
                self.depth += 1;
                if self.depth == 1 {
                    self.top_is_object = b == b'{';
                    self.expect_key = self.top_is_object;
                }
            }
            b'}' | b']' => self.depth = self.depth.saturating_sub(1),
            b':' if at_top => self.value_is_id = self.key_was_id.take().unwrap_or(false),
            b',' if at_top => self.expect_key = true,
            _ => {}
        }
    }

    fn finish_capture(&mut self) {
        self.capture_is_string = false;
        let Some(raw) = self.capture.take() else {
            return;
        };
        if self.id.is_some() {
            return;
        }
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw)
            && (v.is_string() || v.is_number())
        {
            self.id = Some(v.to_string());
        }
    }

    /// The id found, once the whole line has been fed; else why there is none.
    fn finish(mut self) -> Result<String, Dropped> {
        self.finish_capture();
        match self.id {
            Some(id) => Ok(id),
            None if !self.top_is_object => Err(Dropped::NotAnObject),
            None if self.id_over_limit => Err(Dropped::IdOverLimit),
            None => Err(Dropped::NoTopLevelId),
        }
    }
}

/// The reply-producing call for an oversized line of `len` bytes whose id the scanner found; a
/// line with no usable id is reported to `on_drop` instead.
async fn answer_oversized<W: AsyncWrite + Unpin>(
    w: &mut W,
    scan: IdScanner,
    len: usize,
    on_drop: &mut (dyn FnMut(usize, Dropped) + Send),
) -> std::io::Result<()> {
    match scan.finish() {
        Ok(id) => {
            let call = format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"tools/call\",\"params\":{{\"name\":\"{REQUEST_TOO_LARGE}\",\"arguments\":{{}}}}}}\n"
            );
            w.write_all(call.as_bytes()).await?;
        }
        Err(why) => on_drop(len, why),
    }
    Ok(())
}

/// Copies `r` to `w` line by line. A line longer than `max` bytes is never buffered whole: its
/// bytes are fed to an [`IdScanner`] and drained. If the line has a top-level JSON-RPC `"id"`
/// it is replaced by a call to the unlisted [`REQUEST_TOO_LARGE`] tool (so the client gets an
/// error reply for its id); otherwise it is dropped, and `on_drop` gets its length in bytes
/// (without the newline) and the [`Dropped`] reason, once per dropped line.
pub async fn pump_lines<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    r: R,
    mut w: W,
    max: usize,
    on_drop: &mut (dyn FnMut(usize, Dropped) + Send),
) -> std::io::Result<()> {
    let mut r = BufReader::new(r);
    let mut line: Vec<u8> = Vec::new();
    // `Some` while draining an oversized line, with its length so far
    let mut over: Option<(IdScanner, usize)> = None;
    loop {
        let buf = r.fill_buf().await?;
        if buf.is_empty() {
            match over.take() {
                Some((scan, len)) => answer_oversized(&mut w, scan, len, on_drop).await?,
                None if !line.is_empty() => w.write_all(&line).await?,
                None => {}
            }
            w.flush().await?;
            return Ok(());
        }
        let (chunk, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (&buf[..=i], true),
            None => (buf, false),
        };
        let n = chunk.len();
        match &mut over {
            Some((scan, len)) => {
                scan.feed(chunk);
                *len += n;
            }
            None => {
                line.extend_from_slice(chunk);
                // `max` counts the line without its newline
                if line.len() > max + 1 || (!done && line.len() > max) {
                    let mut scan = IdScanner::default();
                    scan.feed(&line);
                    over = Some((scan, line.len()));
                    line = Vec::new();
                }
            }
        }
        r.consume(n);
        if done {
            match over.take() {
                Some((scan, len)) => {
                    // `len` counts the newline that ended the line
                    answer_oversized(&mut w, scan, len - 1, on_drop).await?
                }
                None => w.write_all(&line).await?,
            }
            w.flush().await?;
            line.clear();
        }
    }
}

/// How often `serve_stdio` looks for an idle store to checkpoint.
const CHECKPOINT_TICK: std::time::Duration = std::time::Duration::from_millis(500);

/// What the idle ticker tells the operator: the first failed checkpoint, once, until one succeeds.
/// The store records the failure too (`Store::last_checkpoint_error`); every write is still
/// durable in the log, so a failing checkpoint only lets the index lag.
#[derive(Default)]
struct IdleCheckpointLog {
    failing: bool,
}

impl IdleCheckpointLog {
    /// The line to write to stderr for this tick's result, if any.
    fn note(&mut self, r: &Result<bool, RdyError>) -> Option<String> {
        match r {
            Err(e) if !self.failing => {
                self.failing = true;
                Some(format!(
                    "rdy mcp: the idle index checkpoint failed ({e}); writes are still durable in the log, and it is retried every tick"
                ))
            }
            Err(_) | Ok(false) => None,
            Ok(true) => {
                self.failing = false;
                None
            }
        }
    }
}

/// Serves MCP over stdin and stdout until stdin closes.
///
/// The service runs on its own single-threaded runtime. rmcp runs every request in its own task
/// and a tool body never yields, so on one thread requests run to completion in arrival order and
/// replies leave in that order; on a worker pool they would race for the state lock and a client
/// could see its replies shuffled. The tools are serialised by that lock anyway, so nothing is
/// lost.
pub async fn serve_stdio(project: PathBuf) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    tokio::task::spawn_blocking(move || rt.block_on(serve_stdio_inner(project))).await?
}

async fn serve_stdio_inner(project: PathBuf) -> anyhow::Result<()> {
    let rdy = Rdy::open(&project)?;
    let (to_server, from_pump) = tokio::io::duplex(1 << 16);
    tokio::spawn(async move {
        let _ = pump_lines(
            tokio::io::stdin(),
            to_server,
            tools::MAX_REQUEST_BYTES,
            &mut |n, why| eprintln!("rdy mcp: dropped a {n}-byte request line: {why}"),
        )
        .await;
    });
    let server = RdyServer::new(rdy);
    // `serve` consumes the server: keep the state for the idle ticker and the close at exit
    let state = Arc::clone(&server.state);
    let ticker = {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(CHECKPOINT_TICK);
            let log = Arc::new(Mutex::new(IdleCheckpointLog::default()));
            loop {
                tick.tick().await;
                let (state, log) = (Arc::clone(&state), Arc::clone(&log));
                let _ = tokio::task::spawn_blocking(move || {
                    let r = {
                        let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
                        st.rdy.checkpoint_if_idle()
                    };
                    let mut log = log.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(line) = log.note(&r) {
                        eprintln!("{line}");
                    }
                })
                .await;
            }
        })
    };
    let service = server.serve((from_pump, tokio::io::stdout())).await?;
    let quit = service.waiting().await;
    ticker.abort();
    let _ = ticker.await;
    let _ = tokio::task::spawn_blocking(move || {
        // the clean close: checkpoint, close the index, remove the sentinel; a tool call still
        // holding the state (none should, once the service has ended) leaves the close to the
        // last drop, which reports a failure the same way
        let closed = match Arc::try_unwrap(state) {
            Ok(st) => st.into_inner().unwrap_or_else(|p| p.into_inner()).rdy.close(),
            Err(state) => state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .rdy
                .checkpoint(),
        };
        if let Err(e) = closed {
            eprintln!(
                "rdy mcp: the store did not close cleanly ({e}); the next open catches its index up from the log"
            );
        }
    })
    .await;
    quit?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pump(input: &str, max: usize) -> String {
        let (mut w, r) = (Vec::new(), input.as_bytes());
        pump_lines(r, &mut w, max, &mut |_, _| {}).await.unwrap();
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

    /// Final review (Important 3): the idle ticker reports the first failed checkpoint once, stays
    /// quiet while it keeps failing, and reports the next failure again after a success.
    #[test]
    fn ckpt1_the_idle_ticker_reports_a_failing_checkpoint_once_until_one_succeeds() {
        let fail = || -> Result<bool, RdyError> {
            Err(RdyError::new("E_IO", "disk refused", "free space"))
        };
        let mut log = IdleCheckpointLog::default();
        assert_eq!(log.note(&Ok(false)), None);
        let first = log.note(&fail()).expect("the first failure is reported");
        assert!(first.contains("disk refused"), "{first}");
        assert_eq!(log.note(&fail()), None, "a repeat is not");
        assert_eq!(log.note(&Ok(false)), None, "an idle tick changes nothing");
        assert_eq!(log.note(&fail()), None);
        assert_eq!(log.note(&Ok(true)), None);
        assert!(
            log.note(&fail()).is_some(),
            "reported again after a success"
        );
    }
}

#[cfg(test)]
mod mcp1_id_tests {
    use super::*;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::ReadBuf;

    /// Yields its data `n` bytes per read, so a line arrives in many chunks.
    struct Chunked {
        data: Vec<u8>,
        pos: usize,
        n: usize,
    }

    impl AsyncRead for Chunked {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let end = (self.pos + self.n)
                .min(self.data.len())
                .min(self.pos + buf.remaining());
            let (pos, data) = (self.pos, std::mem::take(&mut self.data));
            buf.put_slice(&data[pos..end]);
            self.data = data;
            self.pos = end;
            Poll::Ready(Ok(()))
        }
    }

    async fn pump_with(r: impl AsyncRead + Unpin, max: usize) -> Vec<serde_json::Value> {
        let mut w = Vec::new();
        pump_lines(r, &mut w, max, &mut |_, _| {}).await.unwrap();
        String::from_utf8(w)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
            .collect()
    }

    fn too_large(v: &serde_json::Value) -> bool {
        v["params"]["name"] == REQUEST_TOO_LARGE
    }

    fn big() -> String {
        "y".repeat(10_000)
    }

    #[tokio::test]
    async fn mcp1_the_id_after_a_large_params_gets_the_reply() {
        let line = format!(
            r#"{{"jsonrpc":"2.0","method":"tools/call","params":{{"name":"rdy_asset","arguments":{{"data_base64":"{}"}}}},"id":42}}"#,
            big()
        );
        let out = pump_with(format!("{line}\n{{\"b\":2}}\n").as_bytes(), 64).await;
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(too_large(&out[0]));
        assert_eq!(out[0]["id"], 42);
        assert_eq!(out[1]["b"], 2);
    }

    #[tokio::test]
    async fn mcp1_an_id_inside_params_does_not_take_the_reply() {
        let line = format!(
            r#"{{"jsonrpc":"2.0","method":"tools/call","params":{{"name":"rdy_write","arguments":{{"ops":[{{"op":"edit","id":"c-ABCDE","body":"{}"}}],"id":5}}}},"id":"req-9"}}"#,
            big()
        );
        let out = pump_with(format!("{line}\n").as_bytes(), 64).await;
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(too_large(&out[0]));
        assert_eq!(out[0]["id"], "req-9");
    }

    #[tokio::test]
    async fn mcp1_a_string_id_with_an_escaped_quote_is_answered_verbatim() {
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":"a\"b\\","method":"tools/call","params":{{"x":"{}"}}}}"#,
            big()
        );
        let out = pump_with(format!("{line}\n").as_bytes(), 64).await;
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0]["id"], "a\"b\\");
    }

    #[tokio::test]
    async fn mcp1_a_line_in_small_chunks_keeps_its_id() {
        let line = format!(
            r#"{{"jsonrpc":"2.0","method":"tools/call","params":{{"x":"{}","id":1}},"id":-17}}"#,
            big()
        );
        let data = format!("{{\"a\":1}}\n{line}\n{{\"b\":2}}\n").into_bytes();
        let out = pump_with(Chunked { data, pos: 0, n: 5 }, 64).await;
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(out[0]["a"], 1);
        assert_eq!(out[1]["id"], -17);
        assert!(too_large(&out[1]));
        assert_eq!(out[2]["b"], 2);
    }

    #[tokio::test]
    async fn mcp1_a_crlf_line_keeps_its_id() {
        let line = format!(
            r#"{{"jsonrpc":"2.0","method":"tools/call","params":{{"x":"{}"}},"id":3}}"#,
            big()
        );
        let out = pump_with(format!("{line}\r\n{{\"b\":2}}\r\n").as_bytes(), 64).await;
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0]["id"], 3);
        assert_eq!(out[1]["b"], 2);
    }

    #[tokio::test]
    async fn mcp1_an_oversized_last_line_without_a_newline_is_answered() {
        let line = format!(
            r#"{{"jsonrpc":"2.0","method":"tools/call","params":{{"x":"{}"}},"id":11}}"#,
            big()
        );
        let out = pump_with(line.as_bytes(), 64).await;
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0]["id"], 11);
    }

    #[tokio::test]
    async fn mcp1_no_valid_top_level_id_is_dropped() {
        for line in [
            format!(r#"{{"params":{{"id":1,"x":"{}"}}}}"#, big()),
            format!(r#"{{"id":true,"x":"{}"}}"#, big()),
            format!(r#"[{{"id":1,"x":"{}"}}]"#, big()),
            format!(r#"{{"idx":1,"ids":2,"i":3,"x":"{}"}}"#, big()),
        ] {
            let out = pump_with(format!("{line}\n{{\"c\":3}}\n").as_bytes(), 64).await;
            assert_eq!(out.len(), 1, "{line:.40}: {out:?}");
            assert_eq!(out[0]["c"], 3);
        }
    }

    #[tokio::test]
    async fn mcp3_a_dropped_line_reports_its_length_and_reason() {
        let mut seen: Vec<(usize, Dropped)> = vec![];
        let data = format!(
            "{{\"params\":{{\"id\":1,\"x\":\"{}\"}}}}\n[{{\"id\":1,\"x\":\"{}\"}}]\n{{\"id\":\"{}\",\"x\":\"{}\"}}\n",
            big(),
            big(),
            "i".repeat(1100),
            big()
        );
        let mut w = Vec::new();
        pump_lines(data.as_bytes(), &mut w, 64, &mut |n, why| {
            seen.push((n, why))
        })
        .await
        .unwrap();
        assert!(w.is_empty(), "{}", String::from_utf8_lossy(&w));
        assert_eq!(
            seen.iter().map(|(_, d)| *d).collect::<Vec<_>>(),
            vec![
                Dropped::NoTopLevelId,
                Dropped::NotAnObject,
                Dropped::IdOverLimit
            ]
        );
        assert!(seen.iter().all(|(n, _)| *n > 10_000), "{seen:?}");
        assert_eq!(
            Dropped::NoTopLevelId.to_string(),
            "no top-level JSON-RPC id"
        );
    }

    #[tokio::test]
    async fn mcp3_a_line_of_exactly_the_limit_passes_and_one_more_byte_does_not() {
        let max = 64;
        let exact = format!("{{\"id\":1,\"p\":\"{}\"}}", "a".repeat(max - 15)); // 15 bytes of envelope
        assert_eq!(exact.len(), max);
        let out = pump_with(format!("{exact}\n").as_bytes(), max).await;
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0]["p"].as_str().unwrap().len(),
            max - 15,
            "passed through verbatim"
        );
        let over = format!("{{\"id\":2,\"p\":\"{}\"}}", "a".repeat(max - 14));
        assert_eq!(over.len(), max + 1);
        let out = pump_with(format!("{over}\n").as_bytes(), max).await;
        assert_eq!(out.len(), 1);
        assert!(too_large(&out[0]), "{out:?}");
        assert_eq!(out[0]["id"], 2);
        let out = pump_with(exact.as_bytes(), max).await; // exactly the limit, no newline, at EOF
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], 1);
    }

    #[test]
    fn hook1_the_panic_hook_exists_only_under_cfg_test() {
        let dir = tempfile::tempdir().unwrap();
        let rdy = Rdy::init(dir.path(), "rdy-standard").unwrap();
        let server = RdyServer::new(rdy);
        let p = SessionParams {
            actor: "__rdy_test_panic__".into(),
            run: None,
            role: "writer".into(),
            view: None,
        };
        let out = serde_json::to_value(server.run("rdy_session", p, tools::rdy_session))
            .unwrap()
            .to_string();
        assert!(
            out.contains("E_INTERNAL") && out.contains("deliberate panic"),
            "{out}"
        );
        // and the server lives: a normal session opens afterwards
        let p = SessionParams {
            actor: "agent:a".into(),
            run: Some("r".into()),
            role: "writer".into(),
            view: None,
        };
        let out = serde_json::to_value(server.run("rdy_session", p, tools::rdy_session)).unwrap();
        assert_ne!(out["isError"], true, "{out}");
    }

    #[test]
    fn mcp2_the_stdio_asset_limit_fits_one_request() {
        assert_eq!(tools::MAX_ASSET_BYTES_STDIO, 12_579_840);
        let b64 = tools::MAX_ASSET_BYTES_STDIO.div_ceil(3) * 4;
        assert!(b64 + 4096 <= tools::MAX_REQUEST_BYTES);
        let next = (tools::MAX_ASSET_BYTES_STDIO + 3).div_ceil(3) * 4;
        assert!(next + 4096 > tools::MAX_REQUEST_BYTES);
    }
}
