//! The projectless MCP server (ain4, D-8X04F, D-YQPB6): `rdy mcp` with no project and no meta
//! project to serve. It answers the session claim and `rdy_instructions` only, with the same
//! input, the same reply and the same creation hint as the project server; every other tool is
//! the existing `E_NO_PROJECT`. It opens no store and makes no project: there is no engine
//! behind it, only the host's reader of the person layer. Once the person creates a meta
//! project, a new connection gets the ordinary project server.
//!
//! Who acts follows D-4PHYQ as on the project server: the first valid explicit claim, by
//! `rdy_session` or by `_meta`, binds the connection, and the session settings (with the
//! instructions read bit) live in the server's claims registry.

use crate::claims::{self, Asked, Key};
use crate::notify::NotifyHub;
use crate::order::{Order, Ordered, Stamped};
use crate::params::parse_params;
use crate::server::{INSTRUCTIONS, RdyServer, pump_lines};
use crate::tools::{self, InstructionsParams, SessionParams};
use rdy_engine::instructions::{Caller, PersonHost};
use rdy_engine::{E_INTERNAL, RdyError};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ServerHandler, ServiceExt};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncWrite};

/// The two tools this server answers.
const SESSION_TOOL: &str = "rdy_session";
const INSTRUCTIONS_TOOL: &str = "rdy_instructions";

/// The connection's state: where it runs, the reader of the person layer, the claims registry
/// and the connection's bound claim.
struct Projectless {
    dir: PathBuf,
    host: Arc<dyn PersonHost>,
    hub: Arc<NotifyHub>,
    bound: Option<Key>,
}

/// The projectless server of one connection.
#[derive(Clone)]
pub struct ProjectlessServer {
    state: Arc<Mutex<Projectless>>,
    tools: Arc<Vec<Tool>>,
}

impl ProjectlessServer {
    /// A server for a connection started in `dir`, outside any project, that reads the person
    /// layer through `host`. `hub` holds its claims registry.
    pub fn new(dir: &Path, host: Arc<dyn PersonHost>, hub: Arc<NotifyHub>) -> ProjectlessServer {
        // the project server's own listing of the two tools: the same names, descriptions and
        // closed input schemas
        let tools: Vec<Tool> = RdyServer::session_router()
            .list_all()
            .into_iter()
            .chain(RdyServer::agent_instructions_router().list_all())
            .filter(|t| t.name == SESSION_TOOL || t.name == INSTRUCTIONS_TOOL)
            .collect();
        ProjectlessServer {
            state: Arc::new(Mutex::new(Projectless {
                dir: dir.to_path_buf(),
                host,
                hub,
                bound: None,
            })),
            tools: Arc::new(tools),
        }
    }

    /// The tools this server lists.
    pub fn tools(&self) -> Vec<Tool> {
        self.tools.as_ref().clone()
    }

    /// One call of `name` with `args`, as the claim `asked` states. Runs on the blocking pool
    /// under the state lock: reading the person layer may wait for its daemon.
    async fn call(
        &self,
        name: String,
        args: rmcp::model::JsonObject,
        asked: Option<Asked>,
    ) -> Result<Value, RdyError> {
        let state = Arc::clone(&self.state);
        tokio::task::spawn_blocking(move || {
            let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
            match name.as_str() {
                SESSION_TOOL => session(&mut st, asked.as_ref(), parse_params(&name, args)?),
                INSTRUCTIONS_TOOL => {
                    instructions(&mut st, asked.as_ref(), parse_params(&name, args)?)
                }
                _ => Err(rdy_engine::config_levels::missing_project(&st.dir)),
            }
        })
        .await
        .unwrap_or_else(|e| {
            Err(RdyError::new(
                E_INTERNAL,
                format!("internal error in the projectless server: {e}"),
                "this is a bug in rdy; re-read before retrying",
            ))
        })
    }
}

/// `rdy_session` outside a project: the project server's checks, then the settings saved and
/// the connection bound. Areas need a project's inbox, so naming any is `E_NO_PROJECT`.
fn session(
    st: &mut Projectless,
    asked: Option<&Asked>,
    p: SessionParams,
) -> Result<Value, RdyError> {
    let (session, role, view) = tools::checked_session(&p, st.bound.as_ref(), asked)?;
    if p.areas.is_some() {
        return Err(rdy_engine::config_levels::missing_project(&st.dir));
    }
    let key = (session.actor.clone(), session.run.clone());
    st.hub.claims().check_room(&key)?;
    let dedupe = asked.and_then(Asked::dedupe);
    st.hub.claims().save(&key, view.clone(), role, dedupe)?;
    st.bound.get_or_insert(key);
    Ok(json!({"view": view, "role": p.role}))
}

/// `rdy_instructions` outside a project: the person layer alone, or an empty reply and the
/// creation hint. The first explicit claim binds the connection, as on the project server.
fn instructions(
    st: &mut Projectless,
    asked: Option<&Asked>,
    p: InstructionsParams,
) -> Result<Value, RdyError> {
    let key = claims::claimed_key(asked, st.bound.as_ref())?;
    st.hub.claims().touch(&key);
    st.bound.get_or_insert(key.clone());
    let caller = Caller {
        actor: key.0.clone(),
        run: key.1.clone(),
    };
    let out = tools::instructions_reply(None, Some(st.host.as_ref()), &caller, p.load)?;
    st.hub.claims().mark_instructions_read(&key);
    Ok(out)
}

fn result(r: Result<Value, RdyError>) -> CallToolResult {
    match r {
        Ok(v) => CallToolResult::success(vec![ContentBlock::text(v.to_string())]),
        Err(e) => {
            CallToolResult::error(vec![ContentBlock::text(tools::error_json(&e).to_string())])
        }
    }
}

impl ServerHandler for ProjectlessServer {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        // D-4PHYQ: who acts, as the request's `_meta` states it; a bad claim is the reply
        let asked = match claims::parse(Some(&context.meta.0.0)) {
            Ok(a) => a,
            Err(e) => return Ok(result(Err(e)).into()),
        };
        let args = request.arguments.unwrap_or_default();
        Ok(result(self.call(request.name.to_string(), args, asked).await).into())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<rmcp::RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools()))
    }

    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("rdy", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

/// Serves `server` over `(r, w)` until the peer closes, one call at a time in arrival order, as
/// the project server's [`crate::server::serve`] does. Only protocol lines reach `w`.
pub async fn serve_projectless<R, W>(server: ProjectlessServer, r: R, w: W) -> anyhow::Result<()>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let order = Arc::new(Order::default());
    let transport = Stamped::new(
        rmcp::transport::async_rw::AsyncRwTransport::new_server(r, w),
        Arc::clone(&order),
    );
    let service = Ordered::new(server, order).serve(transport).await?;
    service.waiting().await?;
    Ok(())
}

/// Serves the projectless server over stdin and stdout until stdin closes, for a connection
/// started in `dir`. Request lines pass the same 16 MiB bound as the project server's; a
/// dropped line is said on stderr, never on stdout.
pub async fn serve_projectless_stdio(
    dir: PathBuf,
    host: Arc<dyn PersonHost>,
) -> anyhow::Result<()> {
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
    let server = ProjectlessServer::new(&dir, host, NotifyHub::new());
    serve_projectless(server, from_pump, tokio::io::stdout()).await
}
