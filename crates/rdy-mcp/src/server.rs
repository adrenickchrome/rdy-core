mod alt;
mod asset;
mod auth;
mod brief;
mod coverage;
mod decide;
mod entity;
mod graph;
mod inbox;
mod instructions;
mod intake;
mod intent;
mod propose;
mod read;
mod remote;
mod resources;
mod review;
mod schema;
mod search;
mod session;
mod signing;
mod site;
mod store;
mod tension;
mod views;
mod write;
pub use instructions::{INSTRUCTION_SENTENCES, INSTRUCTIONS};
pub use resources::{RESOURCES, TEMPLATES};

use crate::notify::{NotifyHub, Sink};
use crate::order::{Order, Ordered, Stamped};
use crate::tools::{self, *};
use rdy_engine::engine::{Engine, Local};
use rdy_engine::signing_crypto::{CallCancel, SigningAgent, SigningCall};
use rdy_engine::{E_INTERNAL, E_LIMIT, RdyError};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        ListResourceTemplatesResult, ListResourcesResult, PaginatedRequestParams,
        ReadResourceRequestParams, ReadResourceResponse, ServerCapabilities, ServerConfig,
        SubscribeRequestParams, SubscriptionFilter, UnsubscribeRequestParams,
    },
    service::{RequestContext, SubscriptionContext},
    tool_handler,
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
    /// The sessions connected to this server (ib10); shared with `state` for `rdy_session`.
    hub: Arc<NotifyHub>,
    /// This session's id in `hub`: the daemon's session id, else the process-unique owner.
    hub_sid: u64,
    /// The engine `state` holds, for the `subscriptions/listen` filter, which resolves doc URIs
    /// without the state lock.
    engine: Arc<dyn Engine>,
}

tokio::task_local! {
    /// The tool call this task runs, set by `call_tool` for the call's own task.
    static CALL: ToolCall;
}

/// Whether rmcp has cancelled a tool call's request token.
type RequestCancelled = Arc<dyn Fn() -> bool + Send + Sync>;

/// One tool call, as its body sees it.
#[derive(Clone)]
struct ToolCall {
    /// The call's request id and its cancel, which `call_tool` sets when rmcp cancels the
    /// request (the client's `notifications/cancelled` for it, or the session's end).
    signing: SigningCall,
    /// Reads rmcp's request token itself, so a cancel that came before an ask stops that ask
    /// however late the call's task is polled.
    cancelled: RequestCancelled,
}

/// The signer of one tool call: the session's agent serving that call
/// ([`SigningAgent::for_call`]), which names the call's request in each ask and stops an ask in
/// progress at the call's cancel. After the cancel it asks no more, and an answer that comes
/// after the cancel is dropped, both as `E_NO_AGENT` `request cancelled`.
struct CallAgent {
    inner: Arc<dyn SigningAgent>,
    cancel: Arc<CallCancel>,
    request_cancelled: RequestCancelled,
}

impl CallAgent {
    fn new(session: &Arc<dyn SigningAgent>, call: &ToolCall) -> CallAgent {
        CallAgent {
            inner: session
                .for_call(&call.signing)
                .unwrap_or_else(|| Arc::clone(session)),
            cancel: Arc::clone(&call.signing.cancel),
            request_cancelled: Arc::clone(&call.cancelled),
        }
    }

    /// Refuses once the call is cancelled. A request token rmcp has cancelled latches the
    /// call's cancel here, before the ask, if `call_tool` has not latched it yet.
    fn check(&self) -> Result<(), RdyError> {
        if (self.request_cancelled)() {
            self.cancel.cancel();
        }
        if self.cancel.is_cancelled() {
            return Err(rdy_engine::no_agent(rdy_engine::NoAgentCause::Cancelled));
        }
        Ok(())
    }
}

impl SigningAgent for CallAgent {
    fn identities(&self) -> Result<Vec<rdy_engine::signing_crypto::AgentIdentity>, RdyError> {
        self.check()?;
        let ids = self.inner.identities()?;
        self.check()?;
        Ok(ids)
    }

    fn sign(&self, public_key: &[u8], payload: &[u8]) -> Result<Vec<u8>, RdyError> {
        self.check()?;
        let sig = self.inner.sign(public_key, payload)?;
        self.check()?;
        Ok(sig)
    }
}

fn result(r: Result<serde_json::Value, RdyError>) -> CallToolResult {
    match r {
        Ok(v) => CallToolResult::success(vec![ContentBlock::text(v.to_string())]),
        Err(e) => CallToolResult::error(vec![ContentBlock::text(error_json(&e).to_string())]),
    }
}

impl RdyServer {
    pub fn new(engine: Arc<dyn Engine>, sid: Option<u64>, hub: Arc<NotifyHub>) -> Self {
        let mut st = McpState::new(Arc::clone(&engine));
        st.sid = sid;
        if let Some(sid) = sid {
            st.lease_owner = sid;
        }
        st.hub = Arc::clone(&hub);
        let hub_sid = st.hub_sid();
        RdyServer {
            hub,
            hub_sid,
            engine,
            state: Arc::new(Mutex::new(st)),
            tool_router: Self::family_routers()
                .into_iter()
                .fold(ToolRouter::new(), |a, b| a + b),
        }
    }

    /// This server with `agent` as its session's signer: the caller's own SSH agent.
    pub fn with_agent(self, agent: Arc<dyn SigningAgent>) -> Self {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).agent = agent;
        self
    }

    /// The session's signer.
    pub fn agent(&self) -> Arc<dyn SigningAgent> {
        Arc::clone(&self.state.lock().unwrap_or_else(|p| p.into_inner()).agent)
    }

    /// The tool router of each family (`server/<family>.rs`), once. `new` registers their sum;
    /// a new family adds one line here.
    fn family_routers() -> [ToolRouter<Self>; 24] {
        [
            Self::session_router(),
            Self::read_router(),
            Self::graph_router(),
            Self::schema_router(),
            Self::brief_router(),
            Self::search_router(),
            Self::write_router(),
            Self::decide_router(),
            Self::propose_router(),
            Self::asset_router(),
            Self::views_router(),
            Self::entity_router(),
            Self::intent_router(),
            Self::alt_router(),
            Self::tension_router(),
            Self::coverage_router(),
            Self::review_router(),
            Self::inbox_router(),
            Self::intake_router(),
            Self::site_router(),
            Self::store_router(),
            Self::signing_router(),
            Self::auth_router(),
            Self::remote_router(),
        ]
    }

    /// Runs one tool on the blocking pool, so no tool blocks the executor. A panic inside it
    /// becomes an `E_INTERNAL` reply (so the request is still answered) and republishes a clean
    /// state, which the panic may have left half-updated. Parameters that did not parse
    /// (`Err`, see [`crate::params`]) are answered at once, as an ordinary tool error.
    ///
    /// Order is not kept here: a session served through [`serve`] runs one stamped call at a
    /// time, in arrival order, and writes the replies in that order (see [`crate::order`]).
    pub(crate) async fn run<P: Send + 'static>(
        &self,
        tool: &'static str,
        p: Result<P, RdyError>,
        f: fn(&mut McpState, P) -> Result<serde_json::Value, RdyError>,
    ) -> CallToolResult {
        let p = match p {
            Ok(p) => p,
            Err(e) => return result(Err(e)),
        };
        result(self.blocking(tool, true, move |st| f(st, p)).await)
    }

    /// `run` for a tool whose reply is a closed, size-checked object: no pack notice rides on
    /// it, and the notice stays pending for the next ordinary tool reply (the CLI prints it on
    /// stderr, apart from the report).
    pub(crate) async fn run_closed<P: Send + 'static>(
        &self,
        tool: &'static str,
        p: Result<P, RdyError>,
        f: fn(&mut McpState, P) -> Result<serde_json::Value, RdyError>,
    ) -> CallToolResult {
        let p = match p {
            Ok(p) => p,
            Err(e) => return result(Err(e)),
        };
        result(self.blocking(tool, false, move |st| f(st, p)).await)
    }

    /// The blocking body shared by tool runs and resource reads: `f` runs on the blocking pool
    /// under the state lock. Inside a tool call, `f` sees the call's own signer ([`CallAgent`]),
    /// whose ask in progress ends at the call's cancel. A panic in `f` becomes an `E_INTERNAL` error and republishes a clean
    /// state (the invalidate runs on the writer, where `Rdy` is); if that fails, the error says
    /// so. With `notice` set, a pack notice rides on an object result; a resource read leaves
    /// the notice for the next tool reply.
    pub(crate) async fn blocking<F>(
        &self,
        tool: &'static str,
        notice: bool,
        f: F,
    ) -> Result<serde_json::Value, RdyError>
    where
        F: FnOnce(&mut McpState) -> Result<serde_json::Value, RdyError> + Send + 'static,
    {
        let state = Arc::clone(&self.state);
        let call = CALL.try_with(ToolCall::clone).ok();
        let joined = tokio::task::spawn_blocking(move || {
            let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
            let session_agent = call.map(|call| {
                let agent = Arc::new(CallAgent::new(&st.agent, &call));
                std::mem::replace(&mut st.agent, agent)
            });
            let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut st)));
            if let Some(agent) = session_agent {
                st.agent = agent;
            }
            match out {
                Ok(mut r) => {
                    if notice && let Ok(serde_json::Value::Object(m)) = &mut r {
                        let generation = st.engine.pack_generation();
                        let error = st.engine.pack_error();
                        if let Some(n) =
                            rdy_engine::engine::notice(&mut st.seen_gen, generation, error.as_deref())
                        {
                            m.insert("notice".into(), serde_json::Value::String(n));
                        }
                    }
                    r
                }
                Err(panic) => {
                    let republished = st.engine.write(Box::new(|r| {
                        r.invalidate();
                        Ok(serde_json::Value::Null)
                    }));
                    let msg = panic
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".into());
                    let mut message = format!("internal error in {tool}: {msg}");
                    if let Err(e) = republished {
                        rdy_engine::notice(&format!(
                            "rdy mcp: after a panic in {tool} the state was not republished ({}: {})",
                            e.code, e.message
                        ));
                        message.push_str(&format!(
                            "; the state was not republished ({}: {})",
                            e.code, e.message
                        ));
                    }
                    Err(RdyError::new(
                        E_INTERNAL,
                        message,
                        "this is a bug in rdy; the request may or may not have been applied: re-read before retrying",
                    ))
                }
            }
        })
        .await;
        joined.unwrap_or_else(|_| {
            Err(RdyError::new(
                E_INTERNAL,
                format!("internal error in {tool}: the call was cancelled"),
                "this is a bug in rdy; the request may or may not have been applied: re-read before retrying",
            ))
        })
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for RdyServer {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        // every tool call is a new response: a state read before a content removal refuses it
        // before routing (plan Redaction: one response-check layer, no name allowlist); the tool
        // may read a newer state
        let state = self.engine.read();
        if let Err(e) = state.ensure_current_epoch() {
            return Ok(result(Err(e)).into());
        }
        drop(state);
        if request.name == REQUEST_TOO_LARGE {
            let mb = tools::MAX_REQUEST_BYTES / (1024 * 1024);
            return Ok(result(Err(RdyError::new(
                E_LIMIT,
                format!("{REQUEST_TOO_LARGE}: the request line was over {mb} MiB and was not read"),
                "send smaller requests: split writes into batches and upload assets one at a time",
            )))
            .into());
        }
        // the call's body signs through a signer that names this request and stops at its
        // cancel: the client's `notifications/cancelled` for it, or the session's end (rmcp
        // cancels every request's token then), reaches an ask of the agent in progress
        let ct = context.ct.clone();
        let cancel = Arc::new(CallCancel::new());
        // a call cancelled before it got here (while it waited for its turn) is cancelled
        // before its body is first polled, so the body never asks the agent
        if ct.is_cancelled() {
            cancel.cancel();
        }
        let call = ToolCall {
            signing: SigningCall {
                request: serde_json::to_value(&context.id).unwrap_or(serde_json::Value::Null),
                cancel: Arc::clone(&cancel),
            },
            cancelled: {
                let ct = ct.clone();
                Arc::new(move || ct.is_cancelled())
            },
        };
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let run = CALL.scope(call, self.tool_router.call(tcc));
        tokio::pin!(run);
        // the cancel goes first when both are ready; the body is then awaited, never dropped,
        // so its worker on the blocking pool is joined
        tokio::select! {
            biased;
            () = ct.cancelled() => {
                cancel.cancel();
                run.await
            }
            out = &mut run => out,
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<rmcp::RoleServer>,
    ) -> Result<ListResourcesResult, rmcp::ErrorData> {
        Ok(Self::resources_list())
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<rmcp::RoleServer>,
    ) -> Result<ListResourceTemplatesResult, rmcp::ErrorData> {
        Ok(Self::resource_templates_list())
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<rmcp::RoleServer>,
    ) -> Result<ReadResourceResponse, rmcp::ErrorData> {
        // as `call_tool`: the state the engine serves is checked before the read routes
        let state = self.engine.read();
        state.ensure_current_epoch().map_err(resources::refuse)?;
        drop(state);
        Ok(self.resources_read(request.uri).await?.into())
    }

    #[allow(deprecated)]
    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<rmcp::RoleServer>,
    ) -> Result<(), rmcp::ErrorData> {
        self.resources_subscribe(&request.uri, context.peer).await
    }

    #[allow(deprecated)]
    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<rmcp::RoleServer>,
    ) -> Result<(), rmcp::ErrorData> {
        self.resources_unsubscribe(&request.uri)
    }

    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        // read once, and only when a doc URI asks
        let read = std::cell::OnceCell::new();
        Some(Self::resources_filter(requested, |text| {
            resources::followed_block(read.get_or_init(|| self.engine.read()), text).is_some()
        }))
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), rmcp::ErrorData> {
        self.resources_listen(context).await;
        Ok(())
    }

    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_resources_subscribe()
                .build(),
        )
        .with_server_info(Implementation::new("rdy", env!("CARGO_PKG_VERSION")))
        .with_instructions(INSTRUCTIONS)
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
struct IdScanner {
    /// The longest id value captured; a longer one counts as no id.
    id_limit: usize,
    depth: usize,
    top_is_object: bool,
    in_string: bool,
    escaped: bool,
    /// At depth 1 of the top-level object, the next string is a key.
    expect_key: bool,
    /// Bytes of the depth-1 key being read (capped: only `id`, `result` and `error` matter).
    key: Option<Vec<u8>>,
    /// A depth-1 key `result` or `error` was seen: the line is a response.
    response: bool,
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

impl Default for IdScanner {
    fn default() -> IdScanner {
        IdScanner::with_id_limit(MAX_ID_BYTES)
    }
}

impl IdScanner {
    fn with_id_limit(id_limit: usize) -> IdScanner {
        IdScanner {
            id_limit,
            depth: 0,
            top_is_object: false,
            in_string: false,
            escaped: false,
            expect_key: false,
            key: None,
            response: false,
            key_was_id: None,
            value_is_id: false,
            capture: None,
            capture_is_string: false,
            id_over_limit: false,
            id: None,
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.byte(b);
        }
    }

    fn byte(&mut self, b: u8) {
        if self.in_string {
            // keep up to 8 bytes: `result` plus its closing quote is 7, so 8 tells `result`
            // from `resultx` (and `id` from `idx`)
            if let Some(k) = &mut self.key
                && k.len() < 8
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
                    if k == b"result\"" || k == b"error\"" {
                        self.response = true;
                    }
                }
                if self.capture_is_string {
                    self.finish_capture();
                }
            }
            if self
                .capture
                .as_ref()
                .is_some_and(|c| c.len() > self.id_limit)
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
                if c.len() > self.id_limit {
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

/// What [`LineScan`] found in one JSON line.
#[derive(Debug, Clone, PartialEq)]
pub struct Scanned {
    /// The top-level `"id"`, when it is a string or a number.
    pub id: Option<serde_json::Value>,
    /// The top-level object has a `"result"` or an `"error"` key: the line is a response.
    pub response: bool,
}

/// Scans one JSON line as it streams, never holding the line: its top-level `"id"` and whether
/// it is a response. It holds no more than the id, up to its id limit (an id over the limit
/// counts as none). It is the scanner [`pump_lines`] reads an oversized request's id with, so a
/// line of any length can be followed by its id.
#[derive(Default)]
pub struct LineScan(IdScanner);

impl LineScan {
    /// A scanner that keeps an id of up to 1024 bytes, as [`pump_lines`] does.
    pub fn new() -> LineScan {
        LineScan::default()
    }

    /// A scanner that keeps an id of up to `id_limit` bytes. A reader that must match a reply
    /// to any request it forwarded sets the request lines' own bound: an id is never longer than
    /// the line that carried it, and a reply writes the id no longer than the request did.
    pub fn with_id_limit(id_limit: usize) -> LineScan {
        LineScan(IdScanner::with_id_limit(id_limit))
    }

    /// The next bytes of the line, its newline excluded.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.0.feed(bytes);
    }

    /// What the line held, once all of it has been fed.
    pub fn finish(mut self) -> Scanned {
        self.0.finish_capture();
        Scanned {
            id: self
                .0
                .id
                .as_deref()
                .and_then(|id| serde_json::from_str(id).ok()),
            response: self.0.response,
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
    on_line: &mut (dyn FnMut(&[u8]) + Send),
) -> std::io::Result<()> {
    match scan.finish() {
        Ok(id) => {
            let call = format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"tools/call\",\"params\":{{\"name\":\"{REQUEST_TOO_LARGE}\",\"arguments\":{{}}}}}}\n"
            );
            on_line(call.as_bytes());
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
    w: W,
    max: usize,
    on_drop: &mut (dyn FnMut(usize, Dropped) + Send),
) -> std::io::Result<()> {
    pump_lines_with(r, w, max, on_drop, &mut |_| {}).await
}

/// [`pump_lines`] with `on_line(line)` called for each line it forwards (a replacement call for
/// an over-long line included), before writing it. The line is as written, newline and all.
pub async fn pump_lines_with<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    r: R,
    mut w: W,
    max: usize,
    on_drop: &mut (dyn FnMut(usize, Dropped) + Send),
    on_line: &mut (dyn FnMut(&[u8]) + Send),
) -> std::io::Result<()> {
    let mut r = BufReader::new(r);
    let mut line: Vec<u8> = Vec::new();
    // `Some` while draining an oversized line, with its length so far
    let mut over: Option<(IdScanner, usize)> = None;
    loop {
        let buf = r.fill_buf().await?;
        if buf.is_empty() {
            match over.take() {
                Some((scan, len)) => answer_oversized(&mut w, scan, len, on_drop, on_line).await?,
                None if !line.is_empty() => {
                    on_line(&line);
                    w.write_all(&line).await?
                }
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
                    answer_oversized(&mut w, scan, len - 1, on_drop, on_line).await?
                }
                None => {
                    on_line(&line);
                    w.write_all(&line).await?
                }
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
/// The service runs on its own single-threaded runtime, and tools run on the blocking pool
/// (`spawn_blocking`) holding the state lock for the call. Writes apply, and replies leave, in
/// request order because the calls are stamped at the transport ([`serve`]), not because of the
/// runtime.
///
/// `agent` is the caller's own SSH agent, which this process made from its own environment: the
/// session signs only through it.
pub async fn serve_stdio(project: PathBuf, agent: Arc<dyn SigningAgent>) -> anyhow::Result<()> {
    serve_stdio_with(project, agent, |_| {}).await
}

/// [`serve_stdio`], calling `inspect` with the session's server once it is made, before it
/// serves: a test seam that shows which signer the session got.
#[doc(hidden)]
pub async fn serve_stdio_with(
    project: PathBuf,
    agent: Arc<dyn SigningAgent>,
    inspect: impl FnOnce(&RdyServer) + Send + 'static,
) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    tokio::task::spawn_blocking(move || rt.block_on(serve_stdio_inner(project, agent, inspect)))
        .await?
}

async fn serve_stdio_inner(
    project: PathBuf,
    agent: Arc<dyn SigningAgent>,
    inspect: impl FnOnce(&RdyServer),
) -> anyhow::Result<()> {
    let local = Arc::new(Local::open(&project)?);
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
    let server = RdyServer::new(
        Arc::clone(&local) as Arc<dyn Engine>,
        None,
        NotifyHub::new(),
    )
    .with_agent(agent);
    inspect(&server);
    let ticker = {
        let local = Arc::clone(&local);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(CHECKPOINT_TICK);
            let log = Arc::new(Mutex::new(IdleCheckpointLog::default()));
            loop {
                tick.tick().await;
                let (local, log) = (Arc::clone(&local), Arc::clone(&log));
                let _ = tokio::task::spawn_blocking(move || {
                    let r = local.checkpoint_if_idle();
                    let mut log = log.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(line) = log.note(&r) {
                        eprintln!("{line}");
                    }
                })
                .await;
            }
        })
    };
    // `serve` consumes the server (and with it the state's handle on the engine)
    let quit = serve(server, from_pump, tokio::io::stdout()).await;
    ticker.abort();
    let _ = ticker.await;
    let _ = tokio::task::spawn_blocking(move || {
        // the clean close: checkpoint, close the index, remove the sentinel; a tool call still
        // holding the engine (none should, once the service has ended) leaves the close to the
        // last drop, which reports a failure the same way
        let closed = match Arc::try_unwrap(local) {
            Ok(local) => local.close(),
            Err(local) => local
                .write(Box::new(|r| {
                    r.checkpoint()?;
                    Ok(serde_json::Value::Null)
                }))
                .map(|_| ()),
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

/// Serves `server` over `(r, w)` until the peer closes, with the session's tool calls stamped
/// at the transport, run one at a time in arrival order and answered in that order (see
/// [`crate::order`]), on whatever runtime polls it. Once the session is up, its peer is the
/// session's sink in the server's hub (ib11), so resource notifications can reach it.
pub async fn serve<R, W>(server: RdyServer, r: R, w: W) -> anyhow::Result<()>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let (hub, sid) = (Arc::clone(&server.hub), server.hub_sid);
    let order = Arc::new(Order::default());
    let transport = Stamped::new(
        rmcp::transport::async_rw::AsyncRwTransport::new_server(r, w),
        Arc::clone(&order),
    );
    // the session is known to the hub before its first request (`NotifyHub::connect`)
    hub.connect(sid, Sink::None);
    let service = Ordered::new(server, order).serve(transport).await?;
    hub.set_sink(sid, Sink::Legacy(service.peer().clone()));
    service.waiting().await?;
    Ok(())
}

/// Serves one MCP session over `(r, w)` against `engine` until the peer closes. The daemon owns
/// the checkpoint tick and the close, so neither runs here. `sid` is the daemon's session id,
/// and `hub` the daemon's hub of connected sessions (ib11): the session joins it under `sid`,
/// and the daemon removes it when the session ends. `agent` is the signer of the process at the
/// other end of the session's connection.
/// It runs on the daemon's runtime: order is explicit (see [`serve`]), not a property of the
/// runtime. Dropping the returned future ends the session: the service is cancelled and the
/// stream closed; a tool call already on the blocking pool finishes first.
pub async fn serve_on<R, W>(
    engine: Arc<dyn Engine>,
    sid: u64,
    hub: Arc<NotifyHub>,
    agent: Arc<dyn SigningAgent>,
    r: R,
    w: W,
) -> anyhow::Result<()>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let (to_server, from_pump) = tokio::io::duplex(1 << 16);
    // a daemon log line (`<unix ms> rdy serve: <text>`); a failed write loses the line, never
    // the session
    let mut dropped = |n: usize, why: Dropped| {
        use std::io::Write as _;
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let _ = writeln!(
            std::io::stderr().lock(),
            "{ms} rdy serve: mcp session {sid} dropped a {n}-byte request line: {why}"
        );
    };
    let pump = pump_lines(r, to_server, tools::MAX_REQUEST_BYTES, &mut dropped);
    let served = serve(
        RdyServer::new(engine, Some(sid), hub).with_agent(agent),
        from_pump,
        w,
    );
    tokio::pin!(served);
    // the session ends when `serve` does (the peer closed, or the session failed to start);
    // when the pump ends first (the peer closed its side), `serve` reads EOF and ends after it
    tokio::select! {
        out = &mut served => out,
        _ = pump => served.await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailingWrite(Local);
    impl Engine for FailingWrite {
        fn read(&self) -> Arc<rdy_engine::ReadState> {
            self.0.read()
        }
        fn write(&self, _f: rdy_engine::WriteFn) -> Result<serde_json::Value, RdyError> {
            Err(RdyError::new(
                "E_DAEMON_GONE",
                "the daemon's writer has stopped",
                "restart the daemon",
            ))
        }
        fn open_report(&self) -> rdy_engine::OpenReport {
            self.0.open_report()
        }
        fn root(&self) -> PathBuf {
            self.0.root()
        }
        fn briefs(&self) -> Arc<rdy_engine::BriefRegistry> {
            self.0.briefs()
        }
        fn leases(&self) -> Arc<rdy_engine::semantic::lease::LeaseRegistry> {
            Default::default()
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn msw9_a_failed_republish_after_a_tool_panic_is_reported() {
        let notes = rdy_engine::capture_notices_for_tests(&["after a panic in rdy_session "]);
        let dir = tempfile::tempdir().unwrap();
        let rdy = rdy_engine::Rdy::init(dir.path(), "rdy-standard").unwrap();
        let server = RdyServer::new(
            Arc::new(FailingWrite(Local::new(rdy))),
            None,
            NotifyHub::new(),
        );
        let p = SessionParams {
            actor: "__rdy_test_panic__".into(),
            run: None,
            role: "writer".into(),
            view: None,
            areas: None,
        };
        let out = serde_json::to_value(server.run("rdy_session", Ok(p), tools::rdy_session).await)
            .unwrap()
            .to_string();
        assert!(
            out.contains("E_INTERNAL")
                && out.contains("not republished")
                && out.contains("E_DAEMON_GONE"),
            "{out}"
        );
        // the server notes it too, where the person running it sees it
        assert_eq!(
            notes.notices(),
            [
                "rdy mcp: after a panic in rdy_session the state was not republished (E_DAEMON_GONE: the daemon's writer has stopped)"
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ib10_a_panic_in_a_blocking_body_republishes_or_says_it_did_not() {
        let notes = rdy_engine::capture_notices_for_tests(&["after a panic in a_read "]);
        let dir = tempfile::tempdir().unwrap();
        let rdy = rdy_engine::Rdy::init(dir.path(), "rdy-standard").unwrap();
        let server = RdyServer::new(
            Arc::new(FailingWrite(Local::new(rdy))),
            None,
            NotifyHub::new(),
        );
        let e = server
            .blocking("a_read", false, |_| panic!("boom"))
            .await
            .unwrap_err();
        assert_eq!(e.code, E_INTERNAL, "{e:?}");
        assert!(
            e.message.contains("boom")
                && e.message.contains("not republished")
                && e.message.contains("E_DAEMON_GONE"),
            "{e:?}"
        );
        // a body that does not panic passes its value through, with no notice added
        let ok = server
            .blocking("a_read", false, |_| Ok(serde_json::json!({"a": 1})))
            .await
            .unwrap();
        assert_eq!(ok, serde_json::json!({"a": 1}));
        assert_eq!(
            notes.notices(),
            [
                "rdy mcp: after a panic in a_read the state was not republished (E_DAEMON_GONE: the daemon's writer has stopped)"
            ]
        );
    }

    /// An agent that counts its calls, records the requests it serves, and can cancel a call
    /// while it answers.
    #[derive(Default)]
    struct Counting {
        calls: std::sync::atomic::AtomicUsize,
        flip: Mutex<Option<Arc<CallCancel>>>,
        served: Mutex<Vec<serde_json::Value>>,
    }

    impl SigningAgent for Counting {
        fn identities(&self) -> Result<Vec<rdy_engine::signing_crypto::AgentIdentity>, RdyError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(cancel) = &*self.flip.lock().unwrap() {
                cancel.cancel();
            }
            Ok(Vec::new())
        }

        fn sign(&self, _key: &[u8], _payload: &[u8]) -> Result<Vec<u8>, RdyError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![1])
        }

        fn for_call(&self, call: &SigningCall) -> Option<Arc<dyn SigningAgent>> {
            self.served.lock().unwrap().push(call.request.clone());
            None
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sig5_a_cancelled_tool_call_asks_its_agent_no_more() {
        use std::sync::atomic::Ordering;
        let dir = tempfile::tempdir().unwrap();
        let rdy = rdy_engine::Rdy::init(dir.path(), "rdy-standard").unwrap();
        let agent = Arc::new(Counting::default());
        let server = RdyServer::new(Arc::new(Local::new(rdy)), None, NotifyHub::new())
            .with_agent(agent.clone());
        // a call whose request token rmcp never cancels: only its own cancel stops it
        let call = |id: u64| ToolCall {
            signing: SigningCall {
                request: serde_json::json!(id),
                cancel: Arc::new(CallCancel::new()),
            },
            cancelled: Arc::new(|| false),
        };
        let keys = |st: &mut McpState| st.agent.identities().map(|k| serde_json::json!(k.len()));
        let sign = |st: &mut McpState| st.agent.sign(&[1], &[2]).map(|_| serde_json::Value::Null);
        let cause = |r: Result<serde_json::Value, RdyError>| {
            let e = r.expect_err("a cancelled call");
            assert_eq!(e.code, rdy_engine::E_NO_AGENT, "{e:?}");
            e.message
        };
        let cancelled_text = "the SSH agent could not sign: request cancelled";
        // a call its client has not cancelled asks the session's agent, for its own request
        let r = CALL.scope(call(1), server.blocking("t", false, keys)).await;
        assert_eq!(r.unwrap(), serde_json::json!(0));
        assert_eq!(agent.calls.load(Ordering::SeqCst), 1);
        // an answer that comes after the cancel is dropped
        let two = call(2);
        *agent.flip.lock().unwrap() = Some(Arc::clone(&two.signing.cancel));
        let r = CALL
            .scope(two.clone(), server.blocking("t", false, keys))
            .await;
        assert_eq!(cause(r), cancelled_text);
        assert_eq!(agent.calls.load(Ordering::SeqCst), 2);
        // once cancelled, the call asks no more
        let r = CALL.scope(two, server.blocking("t", false, sign)).await;
        assert_eq!(cause(r), cancelled_text);
        assert_eq!(agent.calls.load(Ordering::SeqCst), 2);
        // the session's signer is unchanged for the next call, and outside a tool call
        *agent.flip.lock().unwrap() = None;
        let r = CALL.scope(call(3), server.blocking("t", false, sign)).await;
        assert_eq!(r.unwrap(), serde_json::Value::Null);
        assert_eq!(server.agent().identities().unwrap().len(), 0);
        assert_eq!(agent.calls.load(Ordering::SeqCst), 4);
        assert_eq!(
            *agent.served.lock().unwrap(),
            [1, 2, 2, 3].map(|n| serde_json::json!(n)),
            "each call is served for its own request"
        );
    }

    /// How long a test waits for a step it signals; a deadline, never a measured time.
    const DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

    /// A held step: it reports on the sender, then waits for the receiver.
    type Hold = (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>);

    /// An agent that records each call it is bound to (with whether that call's cancel had
    /// already come), counts its identities asks, and can hold its first bind or first ask.
    #[derive(Default)]
    struct Gated {
        asks: std::sync::atomic::AtomicUsize,
        bound: Mutex<Vec<(serde_json::Value, bool)>>,
        hold_bind: Mutex<Option<Hold>>,
        hold_ask: Mutex<Option<Hold>>,
    }

    fn hold(h: &Mutex<Option<Hold>>) {
        let held = h.lock().unwrap().take();
        if let Some((reached, go)) = held {
            reached.send(()).unwrap();
            go.recv_timeout(DEADLINE)
                .expect("the test lets the step go");
        }
    }

    impl SigningAgent for Gated {
        fn identities(&self) -> Result<Vec<rdy_engine::signing_crypto::AgentIdentity>, RdyError> {
            self.asks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            hold(&self.hold_ask);
            Ok(Vec::new())
        }

        fn sign(&self, _key: &[u8], _payload: &[u8]) -> Result<Vec<u8>, RdyError> {
            panic!("key show never signs");
        }

        fn for_call(&self, call: &SigningCall) -> Option<Arc<dyn SigningAgent>> {
            self.bound
                .lock()
                .unwrap()
                .push((call.request.clone(), call.cancel.is_cancelled()));
            hold(&self.hold_bind);
            None
        }
    }

    /// A server on a new project whose person key is set (so `rdy_key_show` asks the agent),
    /// with a reader session and `agent` as its signer.
    async fn key_show_server(agent: Arc<Gated>) -> (tempfile::TempDir, RdyServer) {
        let dir = tempfile::tempdir().unwrap();
        let rdy = rdy_engine::Rdy::init(dir.path(), "rdy-standard").unwrap();
        let fp = rdy_engine::signing_crypto::blob_fingerprint(b"a key the agent does not hold");
        std::fs::write(
            dir.path().join(".rdy/local.yaml"),
            format!("signing:\n  key: {fp:?}\n"),
        )
        .unwrap();
        let server =
            RdyServer::new(Arc::new(Local::new(rdy)), None, NotifyHub::new()).with_agent(agent);
        let p = SessionParams {
            actor: "agent:a".into(),
            run: Some("r1".into()),
            role: "reader".into(),
            view: None,
            areas: None,
        };
        let out = serde_json::to_value(server.run("rdy_session", Ok(p), tools::rdy_session).await)
            .unwrap();
        assert_ne!(out["isError"], true, "{out}");
        (dir, server)
    }

    /// A session of `server` over an in-memory stream, after the handshake, for the peer a tool
    /// call's context names. The client end is returned so that the session stays up.
    async fn session_peer(
        server: RdyServer,
    ) -> (
        rmcp::service::RunningService<rmcp::RoleServer, RdyServer>,
        tokio::io::DuplexStream,
    ) {
        let (mut client, end) = tokio::io::duplex(1 << 16);
        for line in [
            serde_json::json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": {"name": "sig5", "version": "0"}}}),
            serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        ] {
            client
                .write_all(format!("{line}\n").as_bytes())
                .await
                .unwrap();
        }
        let (r, w) = tokio::io::split(end);
        let session = server
            .serve(rmcp::transport::async_rw::AsyncRwTransport::new_server(
                r, w,
            ))
            .await
            .unwrap();
        (session, client)
    }

    fn key_show() -> CallToolRequestParams {
        CallToolRequestParams::new("rdy_key_show").with_arguments(serde_json::Map::new())
    }

    /// The text of a tool reply.
    fn reply_text(out: Result<CallToolResponse, rmcp::ErrorData>) -> String {
        let CallToolResponse::Complete(r) = out.expect("a tool reply") else {
            panic!("a complete tool reply");
        };
        serde_json::to_value(&r).unwrap().to_string()
    }

    fn assert_cancelled(text: &str) {
        assert!(
            text.contains("E_NO_AGENT") && text.contains("request cancelled"),
            "the call ends as cancelled: {text}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sig5_a_call_cancelled_before_call_tool_never_asks_its_agent() {
        use std::sync::atomic::Ordering;
        let agent = Arc::new(Gated::default());
        let (_dir, server) = key_show_server(Arc::clone(&agent)).await;
        let (session, _client) = session_peer(server.clone()).await;
        let ctx = RequestContext::new(
            rmcp::model::NumberOrString::Number(7),
            session.peer().clone(),
        );
        // the client's cancel came while the call waited (rmcp has already cancelled its token)
        ctx.ct.cancel();
        let text = reply_text(ServerHandler::call_tool(&server, key_show(), ctx).await);
        assert_cancelled(&text);
        assert_eq!(agent.asks.load(Ordering::SeqCst), 0, "the agent was asked");
        assert_eq!(
            *agent.bound.lock().unwrap(),
            [(serde_json::json!(7), true)],
            "the body saw its call's cancel before it ran"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sig5_a_cancel_before_the_ask_stops_it_however_the_server_is_polled() {
        use std::sync::atomic::Ordering;
        let agent = Arc::new(Gated::default());
        let (bound_tx, bound) = std::sync::mpsc::channel();
        let (go, go_rx) = std::sync::mpsc::channel();
        *agent.hold_bind.lock().unwrap() = Some((bound_tx, go_rx));
        let (_dir, server) = key_show_server(Arc::clone(&agent)).await;
        let (session, _client) = session_peer(server.clone()).await;
        let ctx = RequestContext::new(
            rmcp::model::NumberOrString::Number(8),
            session.peer().clone(),
        );
        let ct = ctx.ct.clone();
        let call = ServerHandler::call_tool(&server, key_show(), ctx);
        tokio::pin!(call);
        // one poll: the body starts on the blocking pool and binds its call's signer
        let first = std::future::poll_fn(|cx| std::task::Poll::Ready(call.as_mut().poll(cx))).await;
        assert!(first.is_pending(), "the body runs on the blocking pool");
        bound
            .recv_timeout(DEADLINE)
            .expect("the body binds its signer");
        // the client cancels before the body asks; the call's task is not polled in between
        ct.cancel();
        go.send(()).unwrap();
        // the body holds the state lock until it ends: it ends before the call is polled again
        drop(server.state.lock().unwrap_or_else(|p| p.into_inner()));
        assert_eq!(agent.asks.load(Ordering::SeqCst), 0, "the agent was asked");
        assert_cancelled(&reply_text(call.await));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sig5_a_queued_call_cancelled_before_its_turn_never_asks_its_agent() {
        use std::sync::atomic::Ordering;
        let agent = Arc::new(Gated::default());
        let (asked_tx, asked) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        *agent.hold_ask.lock().unwrap() = Some((asked_tx, release_rx));
        let (_dir, server) = key_show_server(Arc::clone(&agent)).await;
        let (client, end) = tokio::io::duplex(1 << 16);
        let (sr, sw) = tokio::io::split(end);
        let served = tokio::spawn(serve(server, sr, sw));
        let (cr, mut cw) = tokio::io::split(client);
        let mut cr = BufReader::new(cr);
        let call = |id: u64| {
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": {"name": "rdy_key_show", "arguments": {}}})
        };
        async fn line(w: &mut (impl AsyncWriteExt + Unpin), v: serde_json::Value) {
            w.write_all(format!("{v}\n").as_bytes()).await.unwrap();
        }
        async fn recv(r: &mut (impl AsyncBufReadExt + Unpin)) -> serde_json::Value {
            let mut line = String::new();
            tokio::time::timeout(DEADLINE, r.read_line(&mut line))
                .await
                .expect("a reply before the deadline")
                .unwrap();
            serde_json::from_str(&line).unwrap()
        }
        line(
            &mut cw,
            serde_json::json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "sig5", "version": "0"}}}),
        )
        .await;
        assert_eq!(recv(&mut cr).await["id"], 0);
        line(
            &mut cw,
            serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        .await;
        // call 2 holds the agent, so call 3 waits for its turn
        line(&mut cw, call(2)).await;
        asked.recv_timeout(DEADLINE).expect("call 2 asks the agent");
        line(&mut cw, call(3)).await;
        line(
            &mut cw,
            serde_json::json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
            "params": {"requestId": 3, "reason": "the person went away"}}),
        )
        .await;
        // the session reads in order: once the ping is answered, call 3's cancel has come
        line(
            &mut cw,
            serde_json::json!({"jsonrpc": "2.0", "id": 4, "method": "ping"}),
        )
        .await;
        assert_eq!(recv(&mut cr).await["id"], 4);
        release.send(()).unwrap();
        line(&mut cw, call(5)).await;
        // call 3's reply is dropped; call 5 runs after call 3's turn has ended
        assert_eq!(recv(&mut cr).await["id"], 2);
        let five = recv(&mut cr).await;
        assert_eq!(five["id"], 5, "{five}");
        assert_eq!(
            agent.asks.load(Ordering::SeqCst),
            2,
            "calls 2 and 5 ask the agent, the cancelled call 3 does not"
        );
        assert_eq!(
            *agent.bound.lock().unwrap(),
            [
                (serde_json::json!(2), false),
                (serde_json::json!(3), true),
                (serde_json::json!(5), false)
            ],
            "call 3 saw its cancel before its body ran"
        );
        drop((cr, cw));
        let _ = tokio::time::timeout(DEADLINE, served).await;
    }

    #[test]
    fn mrt1_every_tool_is_in_exactly_one_family_router() {
        let families = RdyServer::family_routers();
        // sig9: the signing family follows the store family; hsa9: it holds the three hub key
        // tools too
        let mut signing: Vec<String> = families[21]
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        signing.sort();
        assert_eq!(
            signing,
            [
                "rdy_key_bind",
                "rdy_key_retire",
                "rdy_key_revoke",
                "rdy_key_show",
                "rdy_sign"
            ]
        );
        assert!(
            families[20]
                .list_all()
                .iter()
                .any(|t| t.name == "rdy_clone"),
            "the store family precedes the signing family"
        );
        // hsa9: the remote family is the last, after auth; hpb15: it holds rdy_publish too
        let mut remote: Vec<String> = families[23]
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        remote.sort();
        assert_eq!(remote, ["rdy_publish", "rdy_remote"]);
        let mut names: Vec<String> = families
            .iter()
            .flat_map(|r| r.list_all().into_iter().map(|t| t.name.to_string()))
            .collect();
        let all = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), all, "a tool is in two families: {names:?}");
        let combined = families.into_iter().fold(
            rmcp::handler::server::router::tool::ToolRouter::new(),
            |a, b| a + b,
        );
        assert_eq!(combined.list_all().len(), all);
        // The server registers every tool of every family: a family left out of `new` would
        // leave its tools silently unregistered.
        let dir = tempfile::tempdir().unwrap();
        let rdy = rdy_engine::Rdy::init(dir.path(), "rdy-standard").unwrap();
        let server = RdyServer::new(Arc::new(Local::new(rdy)), None, NotifyHub::new());
        let registered: Vec<String> = server
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        assert_eq!(registered, names);
    }

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

    #[tokio::test(flavor = "current_thread")]
    async fn hook1_the_panic_hook_exists_only_under_cfg_test() {
        let dir = tempfile::tempdir().unwrap();
        let rdy = rdy_engine::Rdy::init(dir.path(), "rdy-standard").unwrap();
        let server = RdyServer::new(Arc::new(Local::new(rdy)), None, NotifyHub::new());
        let p = SessionParams {
            actor: "__rdy_test_panic__".into(),
            run: None,
            role: "writer".into(),
            view: None,
            areas: None,
        };
        let out = serde_json::to_value(server.run("rdy_session", Ok(p), tools::rdy_session).await)
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
            areas: None,
        };
        let out = serde_json::to_value(server.run("rdy_session", Ok(p), tools::rdy_session).await)
            .unwrap();
        assert_ne!(out["isError"], true, "{out}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn port1_a_tool_panic_on_the_blocking_pool_still_answers_and_the_server_stays_usable() {
        let dir = tempfile::tempdir().unwrap();
        let server = RdyServer::new(
            Arc::new(Local::new(
                rdy_engine::Rdy::init(dir.path(), "rdy-standard").unwrap(),
            )),
            None,
            NotifyHub::new(),
        );
        let bad = SessionParams {
            actor: "__rdy_test_panic__".into(),
            run: Some("x".into()),
            role: "writer".into(),
            view: None,
            areas: None,
        };
        let out =
            serde_json::to_value(server.run("rdy_session", Ok(bad), tools::rdy_session).await)
                .unwrap()
                .to_string();
        assert!(
            out.contains("E_INTERNAL") && out.contains("deliberate panic"),
            "{out}"
        );
        let ok = SessionParams {
            actor: "agent:a".into(),
            run: Some("x".into()),
            role: "writer".into(),
            view: None,
            areas: None,
        };
        let out = serde_json::to_value(server.run("rdy_session", Ok(ok), tools::rdy_session).await)
            .unwrap();
        assert_ne!(out["isError"], true, "{out}");
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let o = serde_json::to_value(
                server
                    .run(
                        "rdy_overview",
                        Ok(ScopeParams::default()),
                        tools::rdy_overview,
                    )
                    .await,
            )
            .unwrap();
            assert_ne!(o["isError"], true, "{o}");
            assert!(
                t.elapsed() < std::time::Duration::from_secs(2),
                "{:?}",
                t.elapsed()
            );
        }
    }

    #[test]
    fn mcp2_the_stdio_asset_limit_fits_one_request() {
        assert_eq!(tools::MAX_ASSET_BYTES_STDIO, 12_579_840);
        let b64 = tools::MAX_ASSET_BYTES_STDIO.div_ceil(3) * 4;
        assert!(b64 + 4096 <= tools::MAX_REQUEST_BYTES);
        let next = (tools::MAX_ASSET_BYTES_STDIO + 3).div_ceil(3) * 4;
        assert!(next + 4096 > tools::MAX_REQUEST_BYTES);
    }

    #[test]
    fn sig5_line_scan_finds_a_response_id_in_constant_memory() {
        let scan = |parts: &[&[u8]]| {
            let mut s = LineScan::new();
            for p in parts {
                s.feed(p);
            }
            s.finish()
        };
        let big = vec![b'y'; 1 << 20];
        let r = scan(&[
            br#"{"jsonrpc":"2.0","result":{"big":""#,
            &big,
            br#""},"id":7}"#,
        ]);
        assert_eq!(r.id, Some(serde_json::json!(7)));
        assert!(r.response);
        let r = scan(&[br#"{"id":"a","error":{"code":1}}"#]);
        assert_eq!(r.id, Some(serde_json::json!("a")));
        assert!(r.response);
        // a request or a notification is not a response; nested keys do not count
        let r = scan(&[br#"{"id":3,"method":"ping","params":{"result":1,"error":2}}"#]);
        assert_eq!(r.id, Some(serde_json::json!(3)));
        assert!(!r.response);
        let r = scan(&[br#"{"method":"notifications/message","params":{"id":1}}"#]);
        assert_eq!(r.id, None);
        assert!(!r.response);
        // a longer key that starts like one is not it
        let r = scan(&[br#"{"id":4,"resultx":1,"errors":2}"#]);
        assert!(!r.response);
        // a callback body with junk after its id still yields the id
        let r = scan(&[br#"{"id":12,"op":"identities","pad":""#, &big]);
        assert_eq!(r.id, Some(serde_json::json!(12)));
    }

    #[test]
    fn sig5_line_scan_keeps_an_id_up_to_its_limit() {
        let long = serde_json::Value::from("i".repeat(4000));
        let line = serde_json::json!({"jsonrpc": "2.0", "id": long, "result": {}}).to_string();
        let scan = |mut s: LineScan| {
            s.feed(line.as_bytes());
            s.finish()
        };
        // the default keeps 1024 bytes, as pump_lines does
        assert_eq!(scan(LineScan::new()).id, None);
        let r = scan(LineScan::with_id_limit(8000));
        assert_eq!(r.id, Some(long));
        assert!(r.response);
        assert_eq!(scan(LineScan::with_id_limit(2000)).id, None);
    }
}
