//! The projectless MCP server (ain4, D-8X04F, D-YQPB6): `rdy mcp` with no project and no meta
//! project to serve. It answers the session claim and `rdy_instructions` only, with the same
//! input, the same reply and the same creation hint as the project server; every other tool is
//! the existing `E_NO_PROJECT`. It opens no store and makes no project: there is no engine
//! behind it, only the host's reader of the person layer. Once the person creates a meta
//! project, a new connection gets the ordinary project server.
//!
//! One more tool joins the two while a meta project exists (FW-MH.rules, D-ZSRBN): `rdy_rule`,
//! for the person's own rules. The listing is rebuilt on every `tools/list`, so a connection that
//! began before `rdy setup` gains it, and loses it again if the meta project goes. The call
//! checks again before it runs. It holds no lock across the host's work.
//!
//! Who acts follows D-4PHYQ as on the project server: the first valid explicit claim, by
//! `rdy_session` or by `_meta`, binds the connection, and the session settings (with the
//! instructions read bit) live in the server's claims registry.

use crate::claims::{self, Asked, Key};
use crate::kungfu::{self, Check};
use crate::notify::NotifyHub;
use crate::order::{Order, Ordered, Stamped};
use crate::params::parse_params;
use crate::server::{INSTRUCTIONS, RdyServer, pump_lines};
use crate::tools::{self, InstructionsParams, RuleParams, SessionParams};
use rdy_engine::instructions::{Caller, PersonHost};
use rdy_engine::instructions_config::PersonPath;
use rdy_engine::kungfu_counts::Recorder;
use rdy_engine::signing_crypto::{SigningAgent, UnavailableAgent};
use rdy_engine::{E_INTERNAL, RdyError};
use rdy_model::Role;
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

/// The tools this server answers: the first two always, the third while a meta project exists.
const SESSION_TOOL: &str = "rdy_session";
const INSTRUCTIONS_TOOL: &str = "rdy_instructions";
const RULE_TOOL: &str = "rdy_rule";

/// The connection's state: where it runs, the reader of the person layer, the claims registry
/// and the connection's bound claim.
struct Projectless {
    dir: PathBuf,
    host: Arc<dyn PersonHost>,
    hub: Arc<NotifyHub>,
    bound: Option<Key>,
    /// The count recorder of the meta project last counted against, with its path.
    recorder: Option<(PathBuf, Recorder)>,
}

/// The projectless server of one connection.
#[derive(Clone)]
pub struct ProjectlessServer {
    state: Arc<Mutex<Projectless>>,
    /// The two tools every connection lists, and the person's rule tool, which joins them while
    /// a meta project exists.
    tools: Arc<Vec<Tool>>,
    rule: Arc<Tool>,
    /// Signs for a rule that needs a signature; none unless [`ProjectlessServer::with_agent`].
    agent: Arc<dyn SigningAgent>,
}

impl ProjectlessServer {
    /// A server for a connection started in `dir`, outside any project, that reads the person
    /// layer through `host`. `hub` holds its claims registry.
    pub fn new(dir: &Path, host: Arc<dyn PersonHost>, hub: Arc<NotifyHub>) -> ProjectlessServer {
        // the kungfu values are ready before the first call
        rdy_engine::kungfu::owners();
        // the project server's own listing of the two tools: the same names, descriptions and
        // closed input schemas
        let tools: Vec<Tool> = RdyServer::session_router()
            .list_all()
            .into_iter()
            .chain(RdyServer::agent_instructions_router().list_all())
            .filter(|t| t.name == SESSION_TOOL || t.name == INSTRUCTIONS_TOOL)
            .collect();
        // the project router's own entry: the same name, description and closed input schema
        let rule = RdyServer::rule_router()
            .list_all()
            .into_iter()
            .find(|t| t.name == RULE_TOOL)
            .expect("the project server lists rdy_rule");
        ProjectlessServer {
            state: Arc::new(Mutex::new(Projectless {
                dir: dir.to_path_buf(),
                host,
                hub,
                bound: None,
                recorder: None,
            })),
            tools: Arc::new(tools),
            rule: Arc::new(rule),
            agent: Arc::new(UnavailableAgent),
        }
    }

    /// This server with `agent` as the signer of the rules its connection writes: the caller's
    /// own SSH agent.
    pub fn with_agent(mut self, agent: Arc<dyn SigningAgent>) -> ProjectlessServer {
        self.agent = agent;
        self
    }

    /// The tools this server lists now: `rdy_rule` is among them while a meta project exists.
    pub fn tools(&self) -> Vec<Tool> {
        let mut tools = self.tools.as_ref().clone();
        if self.meta_exists() {
            tools.push(self.rule.as_ref().clone());
        }
        tools
    }

    /// Whether the person's meta project exists now: the host's place for it holds a project.
    /// A config the host cannot read means none.
    fn meta_exists(&self) -> bool {
        let host = Arc::clone(&self.state.lock().unwrap_or_else(|p| p.into_inner()).host);
        meta_exists(host.as_ref())
    }

    /// One call of `name` with `args`, as the claim `asked` states. Runs on the blocking pool
    /// under the state lock: reading the person layer may wait for its daemon.
    async fn call(
        &self,
        name: String,
        args: rmcp::model::JsonObject,
        asked: Option<Asked>,
        check: Check,
    ) -> Result<Value, RdyError> {
        let state = Arc::clone(&self.state);
        let agent = Arc::clone(&self.agent);
        tokio::task::spawn_blocking(move || {
            if name == RULE_TOOL {
                // the state lock is held for the claim alone, never across the host's work
                return rule(&state, agent, asked.as_ref(), &check, args);
            }
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
    let out = tools::instructions_reply(None, Some(st.host.as_ref()), &caller, p.load, p.full)?;
    crate::kungfu::note_delivered(st.hub.claims(), &key, &out);
    Ok(out)
}

/// Whether the place the host names for the meta project holds a project.
fn meta_exists(host: &dyn PersonHost) -> bool {
    host.locate().is_ok_and(|source| {
        matches!(
            rdy_engine::instructions::place_person(None, &source.path),
            Ok(rdy_engine::instructions::PersonPlace::Elsewhere(_))
        )
    })
}

/// `rdy_rule` outside a project: the person's rule, written to the meta project through the host
/// ([`tools::projectless_rule`]). The claim is read under the state lock, which is released before
/// the request is checked and before the host does anything.
fn rule(
    state: &Mutex<Projectless>,
    agent: Arc<dyn SigningAgent>,
    asked: Option<&Asked>,
    check: &Check,
    args: rmcp::model::JsonObject,
) -> Result<Value, RdyError> {
    let (key, role, hub, host) = {
        let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
        let key = claims::claimed_key(asked, st.bound.as_ref())?;
        st.hub.claims().touch(&key);
        st.bound.get_or_insert(key.clone());
        let saved = st.hub.claims().get(&key);
        let role = asked
            .and_then(Asked::role)
            .or(saved.and_then(|s| s.role))
            .unwrap_or(Role::Writer);
        (key, role, Arc::clone(&st.hub), Arc::clone(&st.host))
    };
    let session = claims::agent_session(key.0.clone(), key.1.clone(), role)?;
    // the one count of this request, at the meta project's recorder: outside its store
    kungfu::evaluate_for(
        hub.claims(),
        &key,
        || meta_recorder(state, host.as_ref()),
        check,
    );
    let request = tools::checked_request(parse_params::<RuleParams>(RULE_TOOL, args)?)?;
    tools::projectless_rule(host.as_ref(), &session, &request, agent)
}

/// The count recorder of the meta project, kept for the connection. It opens the project's `.rdy`
/// directory and nothing else; a place that has none counts nothing.
fn meta_recorder(state: &Mutex<Projectless>, host: &dyn PersonHost) -> Option<Recorder> {
    let path = match host.locate().ok()?.path {
        PersonPath::Override(p) | PersonPath::Default(p) => p,
        PersonPath::Unavailable => return None,
    };
    {
        let st = state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((at, rec)) = &st.recorder
            && *at == path
        {
            return Some(rec.clone());
        }
    }
    let rec = Recorder::for_project(&path.join(".rdy")).ok()?;
    state.lock().unwrap_or_else(|p| p.into_inner()).recorder = Some((path, rec.clone()));
    Some(rec)
}

fn result(check: &Check, claims: &claims::Registry, r: Result<Value, RdyError>) -> CallToolResult {
    match kungfu::finish(claims, Some(check), r) {
        Ok(v) => CallToolResult::success(vec![ContentBlock::text(v.to_string())]),
        Err(v) => CallToolResult::error(vec![ContentBlock::text(v.to_string())]),
    }
}

/// A refusal before any check was made.
fn refusal(e: RdyError) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(tools::error_json(&e).to_string())])
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
            Err(e) => return Ok(refusal(e).into()),
        };
        let name = request.name.to_string();
        // D-Y18E1: the value leaves the arguments here, once; only `rdy_rule` has an owner
        let mut args = request.arguments;
        let check = Check::of(&name, &mut args);
        let claims_hub = {
            let st = self.state.lock().unwrap_or_else(|p| p.into_inner());
            Arc::clone(&st.hub)
        };
        let out = self
            .call(name, args.unwrap_or_default(), asked, check.clone())
            .await;
        Ok(result(&check, claims_hub.claims(), out).into())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<rmcp::RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        // the host may wait on the file system: not on the executor
        let server = self.clone();
        let tools = tokio::task::spawn_blocking(move || server.tools())
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;
        Ok(ListToolsResult::with_all_items(tools))
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
    agent: Arc<dyn SigningAgent>,
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
    let server = ProjectlessServer::new(&dir, host, NotifyHub::new()).with_agent(agent);
    serve_projectless(server, from_pump, tokio::io::stdout()).await
}
