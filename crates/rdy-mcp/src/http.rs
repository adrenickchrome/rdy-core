//! mht3 (D-6M0AF, D-4PHYQ, D-44NVC, D-QG54Z): MCP over HTTP, bound to the shared claim state,
//! before any socket exists.
//!
//! [`HttpMcp::handle`] is the admission layer around rmcp's
//! `StreamableHttpService<RdyServer, NeverSessionManager>`. It reads the body under
//! [`tools::MAX_REQUEST_BYTES`], parses it with rmcp's request model, and reads the request's
//! claim once. A claim selects the state of its actor and run ([`Slot`]), held in the hub's claim
//! registry under one capacity with the saved settings. Every request gets a fresh transport
//! service and a fresh [`RdyServer`] over its own [`McpState`]; the claim's saved state (read
//! pages, reported warnings, the pack generation it heard of) is lent to that state for the
//! claim's turn ([`Turn`]), so the slot holds no engine or hub. The parsed claim, the claim's
//! queue place and a listen's reservation ride the request's extensions into the server, so
//! rmcp's protocol checks and errors still come before a claim's refusal, and the tool path is
//! the one stdio uses.
//!
//! A listen is refused before rmcp acknowledges it: the request passes rmcp's protocol checks,
//! and the server's filter check ends it ([`ListenGate`]). The layer then answers with the
//! refusal. The `Authorization` and `Cookie` headers never reach rmcp, which copies every header
//! into the handler's extensions.
//!
//! mht6: the layer accepts exactly the three HTTP revisions of [`HTTP_REVISIONS`] on every
//! request; rmcp's own fallback for a missing header (`2025-03-26`) is refused here. The serving
//! daemon's activity permit rides the request ([`Activity`]) and stays with its worker: a call's
//! until its handler returns, a listen's until the listen ends.
use crate::claims::{self, Asked, Key};
use crate::notify::NotifyHub;
use crate::read_page::ReadRegistry;
use crate::server::RdyServer;
use crate::tools::DaemonBuildWarning;
use crate::tools::{self, McpState};
use bytes::{Buf, Bytes};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use rdy_engine::engine::Engine;
use rdy_engine::semantic::lease::LeaseRegistry;
use rdy_engine::{E_LIMIT, RdyError};
use rdy_model::Role;
use rmcp::model::{ClientJsonRpcMessage, ClientRequest, GetMeta, JsonObject};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// An HTTP response, as rmcp's transport gives it.
pub type Response = http::Response<BoxBody<Bytes, Infallible>>;

/// The calls of one HTTP claim that may wait for its turn, beside the one that runs.
pub const MAX_WAITING_CALLS: usize = 8;

/// mht4: the listen requests one daemon keeps open over HTTP.
pub const MAX_LISTENS: usize = 32;

/// mht4: the resource URIs one listen request may name.
pub const MAX_LISTEN_URIS: usize = 128;

/// mht4: the longest resource URI a listen request may name, in bytes.
pub const MAX_LISTEN_URI_BYTES: usize = 4096;

/// mht6: the MCP revisions served over HTTP, exactly these (D-6M0AF). Stdio keeps its own.
pub const HTTP_REVISIONS: [&str; 3] = ["2025-06-18", "2025-11-25", "2026-07-28"];

/// The revision rmcp assumes for a request with no `MCP-Protocol-Version` header; HTTP refuses it.
const SDK_FALLBACK_REVISION: &str = "2025-03-26";

/// mht6: what the serving daemon counts one request as, put in the request's extensions by the
/// transport. The admission layer keeps the held permit with the request's worker: a call's (or
/// any other request's) until its handler returns and the request drops, an admitted listen's
/// with its reservation until the listen ends. Before that it asks `listen` once to count an
/// admitted listen as a stream; when that refuses, the held permit stays.
#[derive(Clone)]
pub struct Activity(Arc<Mutex<Option<ActivityParts>>>);

/// A held activity permit, as the daemon gives it.
pub type ActivityPermit = Box<dyn std::any::Any + Send + Sync>;

/// Counts an admitted listen as a stream: its own permit, or `None` to keep the held one.
pub type ToListen = Box<dyn FnOnce() -> Option<ActivityPermit> + Send + Sync>;

struct ActivityParts {
    held: ActivityPermit,
    listen: Option<ToListen>,
}

impl Activity {
    /// A request counted by `held`; an admitted listen is counted by what `listen` gives.
    pub fn new(held: ActivityPermit, listen: Option<ToListen>) -> Activity {
        Activity(Arc::new(Mutex::new(Some(ActivityParts { held, listen }))))
    }

    fn take(&self) -> Option<ActivityParts> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).take()
    }
}

/// mht6: the transport services of one [`HttpMcp`]: how many live now and how many were made
/// (each of rmcp's lookups of a tool makes one, so a cache shared across requests would show as
/// fewer). Every request makes its own and drops it when its dispatch returns.
#[derive(Clone, Default)]
pub struct TransportStats {
    live: Arc<AtomicUsize>,
    made: Arc<AtomicUsize>,
}

impl TransportStats {
    /// The transport services alive now.
    pub fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    /// The services the requests' transports have asked for so far.
    pub fn made(&self) -> usize {
        self.made.load(Ordering::SeqCst)
    }
}

/// One live transport service, counted until its factory drops with it.
struct LiveService(Arc<AtomicUsize>);

impl Drop for LiveService {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// mht6: the W_DAEMON_BUILD each HTTP request's replies may owe (the daemon's latched build).
pub type BuildWarningSource = Arc<dyn Fn() -> Option<DaemonBuildWarning> + Send + Sync>;

/// The wall clock lease times are read on, in milliseconds (a test sets it).
pub type LeaseClock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Reserves the capacity of one listen stream, or refuses it. By default it is the daemon's
/// [`MAX_LISTENS`] (mht4); a test injects its own.
pub type ListenCapacity = Arc<dyn Fn() -> Result<ListenReservation, RdyError> + Send + Sync>;

/// One listen stream's reserved capacity. It is held from admission through rmcp's dispatch to
/// the accepted listen, and released when the last clone drops: on a refusal, on rmcp's refusal,
/// on cancellation, or when the listen ends.
#[derive(Clone)]
pub struct ListenReservation(#[allow(dead_code)] Arc<dyn std::any::Any + Send + Sync>);

impl ListenReservation {
    /// A reservation that holds `held` until its last clone drops.
    pub fn new<T: Send + Sync + 'static>(held: T) -> ListenReservation {
        ListenReservation(Arc::new(held))
    }
}

/// The state one HTTP claim keeps between its requests: who holds its review leases, its turn,
/// its queue, and the state a request borrows during its turn. It holds no engine or hub, so the
/// hub's registry, which holds it, makes no cycle.
pub struct Slot {
    lease_owner: u64,
    pub(crate) turn: Arc<tokio::sync::Mutex<()>>,
    /// Admitted calls: the one that runs and the ones that wait.
    queued: AtomicUsize,
    /// The settings expired while a lease kept the owner: the next turn starts from fresh state.
    reset_due: AtomicBool,
    saved: Mutex<Saved>,
    leases: Arc<LeaseRegistry>,
    lease_clock: LeaseClock,
}

/// What a claim's requests share through their turns.
#[derive(Default)]
struct Saved {
    reads: ReadRegistry,
    reported: BTreeSet<(String, String, String)>,
    seen_gen: u64,
}

impl Slot {
    fn saved(&self) -> MutexGuard<'_, Saved> {
        self.saved.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The claim's place in its queue, or `None` when [`MAX_WAITING_CALLS`] calls wait already.
    fn enqueue(self: &Arc<Slot>) -> Option<Queued> {
        let admitted = self
            .queued
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n <= MAX_WAITING_CALLS).then_some(n + 1)
            })
            .is_ok();
        admitted.then(|| Queued(Arc::clone(self)))
    }

    /// The registry's question when the claim's settings expired: keep the state? A call in the
    /// queue keeps it whole. A live lease keeps only its owner: the rest starts again.
    pub(crate) fn expire(&self) -> bool {
        if self.queued.load(Ordering::SeqCst) > 0 {
            return true;
        }
        let held = self
            .leases
            .owner_expiry(self.lease_owner, (self.lease_clock)())
            .is_some();
        if held {
            self.reset_due.store(true, Ordering::SeqCst);
        }
        held
    }
}

/// A call's place in its claim's queue, released when the call's request is dropped.
struct Queued(Arc<Slot>);

impl Drop for Queued {
    fn drop(&mut self) {
        self.0.queued.fetch_sub(1, Ordering::SeqCst);
    }
}

/// An HTTP claim's turn: one call at a time per actor and run. While it lasts, the request's
/// state holds the claim's saved state; the end of the turn gives it back.
pub(crate) struct Turn {
    _turn: tokio::sync::OwnedMutexGuard<()>,
    slot: Arc<Slot>,
    state: Arc<Mutex<McpState>>,
}

impl Turn {
    /// Waits for `slot`'s turn and lends its saved state to `state`.
    pub(crate) async fn take(slot: &Arc<Slot>, state: &Arc<Mutex<McpState>>) -> Turn {
        let turn = Arc::clone(&slot.turn).lock_owned().await;
        {
            let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
            let mut saved = slot.saved();
            if slot.reset_due.swap(false, Ordering::SeqCst) {
                *saved = Saved {
                    seen_gen: st.seen_gen,
                    ..Saved::default()
                };
            }
            st.reads = std::mem::take(&mut saved.reads);
            st.reported = std::mem::take(&mut saved.reported);
            st.seen_gen = saved.seen_gen;
        }
        Turn {
            _turn: turn,
            slot: Arc::clone(slot),
            state: Arc::clone(state),
        }
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let mut saved = self.slot.saved();
        saved.reads = std::mem::take(&mut st.reads);
        saved.reported = std::mem::take(&mut st.reported);
        saved.seen_gen = st.seen_gen;
    }
}

/// A listen the admission layer refused: the server's filter check marks that rmcp reached it,
/// before any acknowledgement, and ends the request there.
pub(crate) struct ListenGate {
    reached: AtomicBool,
}

impl ListenGate {
    pub(crate) fn reach(&self) {
        self.reached.store(true, Ordering::SeqCst);
    }
}

/// What the admission layer settled for one request, in its extensions.
struct Admitted {
    /// The request's claim: its `_meta`, parsed once, or the refusal of its claim (its form,
    /// its capacity, its queue).
    claim: Result<Option<Asked>, RdyError>,
    _queued: Option<Queued>,
    /// An admitted listen's reservation, until the listen takes it ([`listen_of`]); a refused
    /// or never-established listen drops it with its request.
    listen: Mutex<Option<ListenReservation>>,
    /// mht6: the daemon's activity permit of this request, held with it.
    _activity: Option<ActivityPermit>,
    #[cfg(test)]
    seen: Arc<Mutex<Vec<http::HeaderMap>>>,
}

/// A request's admission, held by its handler: the request's place in its claim's queue and its
/// listen reservation last while it lasts. rmcp can drop the request's context before the
/// handler's body ends, so the handler keeps this until it returns.
pub(crate) struct Admission {
    _admitted: Arc<Admitted>,
}

/// The claim the admission layer settled for this request, when it came over HTTP, with the
/// admission the handler holds until it returns.
pub(crate) fn claim_of(
    extensions: &rmcp::model::Extensions,
) -> Option<(Result<Option<Asked>, RdyError>, Admission)> {
    let parts = extensions.get::<http::request::Parts>()?;
    let admitted = parts.extensions.get::<Arc<Admitted>>()?;
    #[cfg(test)]
    admitted
        .seen
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(parts.headers.clone());
    Some((
        admitted.claim.clone(),
        Admission {
            _admitted: Arc::clone(admitted),
        },
    ))
}

/// mht4: an admitted HTTP listen, as its handler takes it: the claim it acts for and its
/// reservation, which the listen holds until it ends. `None` on stdio.
pub(crate) fn listen_of(
    extensions: &rmcp::model::Extensions,
) -> Option<(Key, Option<ListenReservation>)> {
    let parts = extensions.get::<http::request::Parts>()?;
    let admitted = parts.extensions.get::<Arc<Admitted>>()?;
    let key = admitted.claim.as_ref().ok()?.as_ref()?.identity()?;
    let reservation = admitted
        .listen
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
    Some((key, reservation))
}

/// The refusal of a listen when [`MAX_LISTENS`] listens are open.
fn listens_full() -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("MCP over HTTP has {MAX_LISTENS} open listen requests"),
        "cancel an older subscriptions/listen request",
    )
}

/// A listen filter inside its bounds, checked before anything copies it: at most
/// [`MAX_LISTEN_URIS`] URIs of at most [`MAX_LISTEN_URI_BYTES`] bytes each.
fn filter_bounds(filter: &rmcp::model::SubscriptionFilter) -> Result<(), RdyError> {
    let uris = filter.resource_subscriptions.as_deref().unwrap_or_default();
    if uris.len() > MAX_LISTEN_URIS {
        return Err(RdyError::new(
            E_LIMIT,
            format!("the listen filter has over {MAX_LISTEN_URIS} resource URIs"),
            format!("use at most {MAX_LISTEN_URIS} resource URIs in one listen request"),
        ));
    }
    if uris.iter().any(|u| u.len() > MAX_LISTEN_URI_BYTES) {
        return Err(RdyError::new(
            E_LIMIT,
            format!("a listen resource URI is over {MAX_LISTEN_URI_BYTES} bytes"),
            format!("use a resource URI of at most {MAX_LISTEN_URI_BYTES} bytes"),
        ));
    }
    Ok(())
}

/// One of the daemon's [`MAX_LISTENS`] listens, given back when it drops.
struct OpenListen(Arc<AtomicUsize>);

impl Drop for OpenListen {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Reserves one of the [`MAX_LISTENS`] listens `open` counts, or refuses.
fn reserve_listen(open: &Arc<AtomicUsize>) -> Result<ListenReservation, RdyError> {
    open.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
        (n < MAX_LISTENS).then_some(n + 1)
    })
    .map_err(|_| listens_full())?;
    Ok(ListenReservation::new(OpenListen(Arc::clone(open))))
}

/// The refusal of a call whose claim already has [`MAX_WAITING_CALLS`] calls waiting.
fn queue_full() -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("this HTTP claim has {MAX_WAITING_CALLS} calls waiting"),
        "wait for a call with this actor and run to finish",
    )
}

/// What a request asks, as the admission layer reads it.
enum Asks {
    /// `tools/call` of `name`.
    Call {
        name: String,
        arguments: Option<JsonObject>,
    },
    /// `resources/read`.
    Read,
    /// `subscriptions/listen`, with its filter's bounds checked.
    Listen(Result<(), RdyError>),
    /// Anything else: discovery, listing, notifications, or a body rmcp refuses.
    Other,
}

/// The MCP-over-HTTP state of one project: the shared engine, hub and tools, and the claims'
/// state in the hub's registry. One per daemon; every request gets its own transport service.
pub struct HttpMcp {
    proto: RdyServer,
    engine: Arc<dyn Engine>,
    hub: Arc<NotifyHub>,
    kungfu_counts: Option<rdy_engine::kungfu_counts::Recorder>,
    leases: Arc<LeaseRegistry>,
    lease_clock: LeaseClock,
    listen: ListenCapacity,
    /// The listens the default capacity holds open.
    listens: Arc<AtomicUsize>,
    /// mht6: the port of the loopback listener this serves, for rmcp's own Host allowlist.
    port: Option<u16>,
    /// mht6: the daemon's W_DAEMON_BUILD for each request, when one is owed.
    build_warning: Option<BuildWarningSource>,
    /// mht6: the requests' transport services.
    transports: TransportStats,
    #[cfg(test)]
    seen: Arc<Mutex<Vec<http::HeaderMap>>>,
}

impl HttpMcp {
    /// The HTTP side of the project `engine`, sharing `hub` (its claim registry too) with the
    /// project's other MCP sessions.
    pub fn new(engine: Arc<dyn Engine>, hub: Arc<NotifyHub>) -> HttpMcp {
        let proto = RdyServer::new(Arc::clone(&engine), None, Arc::clone(&hub));
        let listens = Arc::new(AtomicUsize::new(0));
        let open = Arc::clone(&listens);
        HttpMcp {
            kungfu_counts: proto.kungfu_counts(),
            leases: engine.leases(),
            proto,
            engine,
            hub,
            lease_clock: Arc::new(rdy_engine::now_ms),
            listen: Arc::new(move || reserve_listen(&open)),
            listens,
            port: None,
            build_warning: None,
            transports: TransportStats::default(),
            #[cfg(test)]
            seen: Default::default(),
        }
    }

    /// This state with `clock` as the time lease expiry is read at.
    pub fn with_lease_clock(mut self, clock: LeaseClock) -> HttpMcp {
        self.lease_clock = clock;
        self
    }

    /// This state with `capacity` reserving each listen before rmcp sees it.
    pub fn with_listen_capacity(mut self, capacity: ListenCapacity) -> HttpMcp {
        self.listen = capacity;
        self
    }

    /// mht6: this state serving the loopback listener of `port`: rmcp's own Host check then
    /// accepts exactly the loopback names with that port, as the site's guard does.
    pub fn with_port(mut self, port: u16) -> HttpMcp {
        self.port = Some(port);
        self
    }

    /// mht6: this state with `source` giving each request the build warning it may owe.
    pub fn with_build_warning(mut self, source: BuildWarningSource) -> HttpMcp {
        self.build_warning = Some(source);
        self
    }

    /// The transport configuration: stateless (no session, no event store), JSON replies where
    /// rmcp can give one, and rdy's request bound.
    fn transport_config() -> StreamableHttpServerConfig {
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_max_request_body_bytes(tools::MAX_REQUEST_BYTES)
    }

    /// mht6: [`Self::transport_config`] with rmcp's Host and Origin policy matching the outer
    /// guard: every Origin refused (an empty allowlist alone refuses none), and, on a bound
    /// port, only its loopback names.
    fn request_config(&self) -> StreamableHttpServerConfig {
        let config = Self::transport_config().enforce_origin_validation();
        match self.port {
            Some(port) => config.with_allowed_hosts([
                format!("localhost:{port}"),
                format!("127.0.0.1:{port}"),
                format!("[::1]:{port}"),
            ]),
            None => config,
        }
    }

    /// A request state that serves MCP over HTTP: bound to `key` when the request acts for a
    /// claim, holding leases as `lease_owner`.
    fn request_state(&self, key: Option<Key>, lease_owner: u64) -> McpState {
        let mut st = McpState::new(Arc::clone(&self.engine));
        st.hub = Arc::clone(&self.hub);
        st.lease_owner = lease_owner;
        st.kungfu_counts = self.kungfu_counts.clone();
        st.bound = key;
        st.http = true;
        st.build_warning = self.build_warning.as_ref().and_then(|w| w());
        st
    }

    /// The state of claim `key`, made when it has none, under the registry's capacity.
    fn slot(&self, key: &Key) -> Result<Arc<Slot>, RdyError> {
        self.hub.claims().http_slot(key, || Slot {
            lease_owner: LeaseRegistry::local_owner(),
            turn: Default::default(),
            queued: AtomicUsize::new(0),
            reset_due: AtomicBool::new(false),
            saved: Mutex::new(Saved {
                seen_gen: self.engine.pack_generation(),
                ..Saved::default()
            }),
            leases: Arc::clone(&self.leases),
            lease_clock: Arc::clone(&self.lease_clock),
        })
    }

    /// Who holds the review leases of claim `key`, while its state is held.
    pub fn lease_owner(&self, key: &Key) -> Option<u64> {
        self.hub.claims().http_slot_of(key).map(|s| s.lease_owner)
    }

    /// The calls of claim `key` admitted now: the one that runs and the ones that wait.
    pub fn queued(&self, key: &Key) -> usize {
        self.hub
            .claims()
            .http_slot_of(key)
            .map_or(0, |s| s.queued.load(Ordering::SeqCst))
    }

    /// mht6: the requests' transport services, for their lifetime checks.
    #[doc(hidden)]
    pub fn transport_stats(&self) -> TransportStats {
        self.transports.clone()
    }

    /// The listens the default capacity holds now: admitted, open, or still ending.
    pub fn listen_streams(&self) -> usize {
        self.listens.load(Ordering::SeqCst)
    }

    /// Holds claim `key`'s turn, so its calls wait (a test seam for the queue bound). `None`
    /// when the claim has no state.
    #[doc(hidden)]
    pub async fn hold_turn(&self, key: &Key) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        let slot = self.hub.claims().http_slot_of(key)?;
        Some(Arc::clone(&slot.turn).lock_owned().await)
    }

    /// Admits one HTTP request to the MCP endpoint and answers it. Host, Origin, bearer and
    /// method checks come before this layer (the site's guard); this layer reads the body, the
    /// claim and its capacity, and rmcp does the rest.
    pub async fn handle<B>(&self, request: http::Request<B>) -> Response
    where
        B: BodyExt + Send + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let (mut parts, body) = request.into_parts();
        let activity = parts.extensions.remove::<Activity>().and_then(|a| a.take());
        let bytes = match read_body(body, tools::MAX_REQUEST_BYTES).await {
            Ok(b) => b,
            Err(response) => return response,
        };
        // the bearer and cookies end here: rmcp copies every header into the handler's view
        parts.headers.remove(http::header::AUTHORIZATION);
        parts.headers.remove(http::header::COOKIE);
        let message = serde_json::from_slice::<ClientJsonRpcMessage>(&bytes).ok();
        if let Err(refused) = revision(&parts.headers, message.as_ref()) {
            return refused;
        }
        let (id, asks, meta) = match &message {
            Some(ClientJsonRpcMessage::Request(r)) => {
                let asks = match &r.request {
                    ClientRequest::CallToolRequest(c) => Asks::Call {
                        name: c.params.name.to_string(),
                        arguments: c.params.arguments.clone(),
                    },
                    ClientRequest::ReadResourceRequest(_) => Asks::Read,
                    ClientRequest::SubscriptionsListenRequest(l) => {
                        Asks::Listen(filter_bounds(&l.params.notifications))
                    }
                    _ => Asks::Other,
                };
                let id = serde_json::to_value(&r.id).unwrap_or(Value::Null);
                (id, asks, r.request.get_meta().0.0.clone())
            }
            _ => (Value::Null, Asks::Other, JsonObject::new()),
        };
        let mut admitted = Admitted {
            claim: Ok(None),
            _queued: None,
            listen: Mutex::new(None),
            _activity: None,
            #[cfg(test)]
            seen: Arc::clone(&self.seen),
        };
        let mut slot: Option<(Key, Arc<Slot>)> = None;
        let mut gate = None;
        let listens = matches!(asks, Asks::Listen(_));
        match asks {
            Asks::Other => {}
            Asks::Call { .. } | Asks::Read => {
                let parsed = claims::parse(Some(&meta));
                let stated = parsed.as_ref().ok().and_then(|a| a.as_ref()?.identity());
                // `rdy_session` may state its claim in its own arguments (D-4PHYQ); only the
                // actor and run select the state, and the tool checks the rest in stdio's order
                let peeked = match (&asks, &parsed, &stated) {
                    (Asks::Call { name, arguments }, Ok(_), None) if name == "rdy_session" => {
                        session_key(arguments.as_ref())
                    }
                    _ => None,
                };
                admitted.claim = parsed;
                if let Some(key) = stated.clone().or(peeked) {
                    match self.slot(&key) {
                        Ok(s) => match s.enqueue() {
                            Some(q) => {
                                admitted._queued = Some(q);
                                slot = Some((key, s));
                            }
                            None => admitted.claim = Err(queue_full()),
                        },
                        // a session's own checks come first and refuse a full registry alike
                        Err(_) if stated.is_none() => {}
                        Err(e) => admitted.claim = Err(e),
                    }
                }
            }
            Asks::Listen(bounds) => {
                // the claim, then the filter's bounds, then the capacity (mht4)
                let parsed = claims::parse(Some(&meta)).and_then(|a| {
                    let asked = a.ok_or_else(tools::no_session)?;
                    let key = asked.identity().ok_or_else(tools::no_session)?;
                    Ok((asked, key))
                });
                let parsed = parsed.and_then(|(asked, key)| {
                    bounds?;
                    let s = self.slot(&key)?;
                    admitted.claim = Ok(Some(asked));
                    Ok((key, s))
                });
                let refusal = match parsed {
                    Ok((key, s)) => match (self.listen)() {
                        Ok(r) => {
                            *admitted.listen.lock().unwrap_or_else(|p| p.into_inner()) = Some(r);
                            slot = Some((key, s));
                            None
                        }
                        Err(e) => Some(e),
                    },
                    Err(e) => Some(e),
                };
                if let Some(e) = refusal {
                    gate = Some((
                        Arc::new(ListenGate {
                            reached: AtomicBool::new(false),
                        }),
                        e,
                    ));
                }
            }
        }
        // the activity permit stays with the worker: an admitted listen's with its reservation
        if let Some(mut activity) = activity {
            let mut reservation = admitted.listen.lock().unwrap_or_else(|p| p.into_inner());
            match reservation.take() {
                Some(r) => {
                    if let Some(p) = activity.listen.take().and_then(|f| f()) {
                        activity.held = p;
                    }
                    *reservation = Some(ListenReservation::new((r, activity.held)));
                }
                None => {
                    drop(reservation);
                    admitted._activity = Some(activity.held);
                }
            }
        }
        let (key, slot) = slot.unzip();
        let lease_owner = slot
            .as_ref()
            .map_or_else(LeaseRegistry::local_owner, |s| s.lease_owner);
        let state = Arc::new(Mutex::new(self.request_state(key, lease_owner)));
        // each listen is a session of its own in the hub, so two listens of one claim never
        // share a registration (mht4)
        let hub_sid = if listens {
            LeaseRegistry::local_owner()
        } else {
            lease_owner
        };
        let server = self.proto.for_http(
            state,
            hub_sid,
            slot,
            gate.as_ref().map(|(g, _)| Arc::clone(g)),
        );
        parts.extensions.insert(Arc::new(admitted));
        self.transports.live.fetch_add(1, Ordering::SeqCst);
        let live = LiveService(Arc::clone(&self.transports.live));
        let made = Arc::clone(&self.transports.made);
        let service: StreamableHttpService<RdyServer, NeverSessionManager> =
            StreamableHttpService::new(
                move || {
                    let _live = &live;
                    made.fetch_add(1, Ordering::SeqCst);
                    Ok(server.clone())
                },
                Arc::new(NeverSessionManager::default()),
                self.request_config(),
            );
        let response = service
            .handle(http::Request::from_parts(parts, Full::new(bytes)))
            .await;
        // the request's transport service ends with its dispatch; the response keeps none
        drop(service);
        match gate {
            Some((g, e)) if g.reached.load(Ordering::SeqCst) => listen_refusal(id, e),
            _ => response,
        }
    }

    /// One in-memory request to [`HttpMcp::handle`], with its whole response body: the MCP test
    /// harness's client, which needs no socket.
    #[doc(hidden)]
    pub async fn post(&self, headers: &[(&str, &str)], body: Vec<u8>) -> Posted {
        let mut request = http::Request::builder().method("POST").uri("/~mcp");
        for (k, v) in headers {
            request = request.header(*k, *v);
        }
        let request = request
            .body(Full::new(Bytes::from(body)))
            .expect("a valid test request");
        let response = self.handle(request).await;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect();
        let body = match response.into_body().collect().await {
            Ok(c) => c.to_bytes().to_vec(),
            Err(never) => match never {},
        };
        Posted {
            status,
            headers,
            body,
        }
    }
}

impl HttpMcp {
    /// One in-memory request to [`HttpMcp::handle`], with its response open: the MCP test
    /// harness's client for a stream it reads one message at a time. Dropping it is the client
    /// going away.
    #[doc(hidden)]
    pub async fn open(&self, headers: &[(&str, &str)], body: Vec<u8>) -> Opened {
        let mut request = http::Request::builder().method("POST").uri("/~mcp");
        for (k, v) in headers {
            request = request.header(*k, *v);
        }
        let request = request
            .body(Full::new(Bytes::from(body)))
            .expect("a valid test request");
        let response = self.handle(request).await;
        let status = response.status().as_u16();
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect();
        let sse = headers
            .iter()
            .any(|(k, v)| k == "content-type" && v.starts_with("text/event-stream"));
        Opened {
            status,
            headers,
            sse,
            body: response.into_body(),
            text: Vec::new(),
            ready: std::collections::VecDeque::new(),
            ended: false,
        }
    }
}

/// One in-memory response, read as it comes: a JSON reply, or an SSE stream's messages.
#[doc(hidden)]
pub struct Opened {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    sse: bool,
    body: BoxBody<Bytes, Infallible>,
    text: Vec<u8>,
    ready: std::collections::VecDeque<Value>,
    ended: bool,
}

impl Opened {
    /// The value of header `name` (lowercase), if any.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// The next JSON-RPC message of the response, or `None` once it ended. An SSE event with
    /// no JSON data (a keep-alive, a priming event) is skipped.
    pub async fn next(&mut self) -> Option<Value> {
        loop {
            if let Some(v) = self.ready.pop_front() {
                return Some(v);
            }
            if self.ended {
                return None;
            }
            match self.body.frame().await {
                None => {
                    self.ended = true;
                    let rest = std::mem::take(&mut self.text);
                    if self.sse {
                        self.events(&rest);
                    } else if let Ok(v) = serde_json::from_slice(&rest) {
                        self.ready.push_back(v);
                    }
                }
                Some(Ok(frame)) => {
                    let Ok(data) = frame.into_data() else {
                        continue;
                    };
                    self.text.extend_from_slice(&data);
                    if self.sse {
                        while let Some(end) = self.text.windows(2).position(|w| w == b"\n\n") {
                            let event: Vec<u8> = self.text.drain(..end + 2).collect();
                            self.events(&event);
                        }
                    }
                }
                Some(Err(never)) => match never {},
            }
        }
    }

    /// The JSON data of one SSE event, queued.
    fn events(&mut self, event: &[u8]) {
        let text = String::from_utf8_lossy(event);
        let data: String = text
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .collect();
        if let Ok(v) = serde_json::from_str(&data) {
            self.ready.push_back(v);
        }
    }
}

/// The actor and run an `rdy_session` call's arguments name, when both have a valid agent form.
/// Nothing else of the arguments is read: the tool checks them as stdio does.
fn session_key(arguments: Option<&JsonObject>) -> Option<Key> {
    let args = arguments?;
    let actor = args.get("actor")?.as_str()?;
    let run = match args.get("run") {
        None | Some(Value::Null) => None,
        Some(Value::String(r)) => Some(r.clone()),
        Some(_) => return None,
    };
    let s = claims::agent_session(actor.to_string(), run, Role::Writer).ok()?;
    Some((s.actor, s.run))
}

/// The body, read under `max` decoded bytes: over it is 413 with D-QG54Z's text.
async fn read_body<B>(body: B, max: usize) -> Result<Bytes, Response>
where
    B: BodyExt + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let mut body = std::pin::pin!(body);
    let mut collected = bytes::BytesMut::new();
    let over = || {
        plain(
            http::StatusCode::PAYLOAD_TOO_LARGE,
            format!("the MCP request body is over {max} bytes"),
        )
    };
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| {
            let e: Box<dyn std::error::Error + Send + Sync> = e.into();
            // a transport's own bound on the body (mht6: the daemon's `Limited`) is this bound
            if e.is::<http_body_util::LengthLimitError>() {
                return over();
            }
            plain(
                http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to read request body: {e}"),
            )
        })?;
        let Ok(mut data) = frame.into_data() else {
            continue;
        };
        if data.remaining() > max.saturating_sub(collected.len()) {
            return Err(over());
        }
        while data.has_remaining() {
            let chunk = data.chunk();
            let n = chunk.len();
            collected.extend_from_slice(chunk);
            data.advance(n);
        }
    }
    Ok(collected.freeze())
}

/// mht6: the HTTP revision rule, on every request: the `MCP-Protocol-Version` header names one
/// of [`HTTP_REVISIONS`], once. Only `initialize` may omit it, and then its body names one. The
/// refusal is rmcp's own for a revision it does not serve; a missing header is refused as the
/// revision rmcp would assume for it. rmcp checks the header against the body's metadata after.
fn revision(
    headers: &http::HeaderMap,
    message: Option<&ClientJsonRpcMessage>,
) -> Result<(), Response> {
    let unsupported = |v: &str| {
        plain(
            http::StatusCode::BAD_REQUEST,
            format!("Bad Request: Unsupported MCP-Protocol-Version: {v}"),
        )
    };
    let mut values = headers.get_all("mcp-protocol-version").iter();
    match (values.next(), values.next()) {
        (Some(_), Some(_)) => Err(plain(
            http::StatusCode::BAD_REQUEST,
            "Bad Request: Invalid MCP-Protocol-Version header encoding".to_string(),
        )),
        (Some(v), None) => match v.to_str() {
            Ok(v) if HTTP_REVISIONS.contains(&v) => Ok(()),
            Ok(v) => Err(unsupported(v)),
            Err(_) => Err(plain(
                http::StatusCode::BAD_REQUEST,
                "Bad Request: Invalid MCP-Protocol-Version header encoding".to_string(),
            )),
        },
        (None, _) => match message {
            Some(ClientJsonRpcMessage::Request(r)) => match &r.request {
                ClientRequest::InitializeRequest(init) => {
                    let asked = init.params.protocol_version.as_str();
                    if HTTP_REVISIONS.contains(&asked) {
                        Ok(())
                    } else {
                        Err(unsupported(asked))
                    }
                }
                _ => Err(unsupported(SDK_FALLBACK_REVISION)),
            },
            // a body rmcp cannot read gets rmcp's own refusal
            None => Ok(()),
            Some(_) => Err(unsupported(SDK_FALLBACK_REVISION)),
        },
    }
}

/// A plain-text response.
fn plain(status: http::StatusCode, text: String) -> Response {
    http::Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from(text)).boxed())
        .expect("a valid response")
}

/// The JSON-RPC refusal of a listen, as a resource read's refusal is enveloped: the listen's id
/// and the error, with no stream and no acknowledgement.
fn listen_refusal(id: Value, e: RdyError) -> Response {
    let error = crate::server::refuse_resource(e);
    let body = json!({"jsonrpc": "2.0", "id": id, "error": error});
    http::Response::builder()
        .status(http::StatusCode::OK)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body.to_string())).boxed())
        .expect("a valid response")
}

/// One in-memory response, read whole.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct Posted {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Posted {
    /// The value of header `name` (lowercase), if any.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// The JSON-RPC messages of the body: one JSON reply, or each SSE `data:` event's.
    pub fn messages(&self) -> Vec<Value> {
        let text = String::from_utf8_lossy(&self.body);
        if self
            .header("content-type")
            .is_some_and(|t| t.starts_with("text/event-stream"))
        {
            return text
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .filter_map(|d| serde_json::from_str(d.trim()).ok())
                .collect();
        }
        serde_json::from_str(&text).into_iter().collect()
    }

    /// The last JSON-RPC message: the reply to the request.
    pub fn reply(&self) -> Value {
        self.messages().pop().unwrap_or(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_engine::engine::Local;
    use std::sync::atomic::AtomicUsize;

    const LEGACY: &str = "2025-11-25";
    const MODERN: &str = "2026-07-28";

    fn project() -> (tempfile::TempDir, Arc<dyn Engine>) {
        let dir = tempfile::tempdir().unwrap();
        let rdy = rdy_engine::Rdy::init(dir.path(), "rdy-standard").unwrap();
        (dir, Arc::new(Local::new(rdy)))
    }

    fn headers(revision: &str) -> Vec<(&'static str, String)> {
        vec![
            ("host", "localhost".into()),
            ("accept", "application/json, text/event-stream".into()),
            ("content-type", "application/json".into()),
            ("mcp-protocol-version", revision.into()),
        ]
    }

    async fn post(mcp: &HttpMcp, h: &[(&str, String)], body: Value) -> Posted {
        let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
        mcp.post(&h, body.to_string().into_bytes()).await
    }

    fn call(id: u64, name: &str, args: Value, meta: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": args, "_meta": meta}})
    }

    fn claim(actor: &str, run: &str) -> Value {
        json!({"rdy/actor": actor, "rdy/run": run})
    }

    /// The modern per-request metadata rmcp requires, with `extra`.
    fn modern_meta(extra: Value) -> Value {
        let mut m = json!({
            "io.modelcontextprotocol/protocolVersion": MODERN,
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": {"name": "mht3", "version": "0"},
        });
        for (k, v) in extra.as_object().unwrap() {
            m[k] = v.clone();
        }
        m
    }

    fn modern_headers(method: &str, name: Option<&str>) -> Vec<(&'static str, String)> {
        let mut h = headers(MODERN);
        h.push(("mcp-method", method.into()));
        if let Some(n) = name {
            h.push(("mcp-name", n.into()));
        }
        h
    }

    /// The tool reply's rdy body, and whether it is an error.
    fn tool(p: &Posted) -> (bool, Value) {
        let r = p.reply();
        let text = r["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no tool reply: {r}"));
        (
            r["result"]["isError"] == json!(true),
            serde_json::from_str(text).unwrap(),
        )
    }

    /// Waits until claim `key` has no admitted call: rmcp dropped each request, which releases
    /// its place. A place that is never released fails at the hang guard.
    async fn settled(mcp: &HttpMcp, key: &Key) {
        let wait = async {
            while mcp.queued(key) > 0 {
                tokio::task::yield_now().await;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(60), wait)
            .await
            .expect("every place in the queue is released");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mht3_bridge_compiles_with_streamable_http_service() {
        let (_d, engine) = project();
        let mcp = HttpMcp::new(engine, NotifyHub::new());
        // the bridge is the SDK's own service type, made through the production config
        let svc: StreamableHttpService<RdyServer, NeverSessionManager> = StreamableHttpService::new(
            {
                let s = mcp.proto.clone();
                move || Ok(s.clone())
            },
            Arc::new(NeverSessionManager::default()),
            HttpMcp::transport_config(),
        );
        assert!(!svc.config.legacy_session_mode && svc.config.json_response);
        assert_eq!(svc.config.max_request_body_bytes, tools::MAX_REQUEST_BYTES);
        drop(svc);
        // discovery needs no claim
        let list = post(
            &mcp,
            &headers(LEGACY),
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        )
        .await;
        assert_eq!(list.status, 200, "{list:?}");
        assert!(
            list.reply()["result"]["tools"]
                .as_array()
                .is_some_and(|t| t.iter().any(|t| t["name"] == "rdy_session")),
            "{list:?}"
        );
        // a claimed call on an older revision and on the modern one, through the tool router
        let key = ("agent:b".to_string(), Some("b1".to_string()));
        let ov = post(
            &mcp,
            &headers(LEGACY),
            call(2, "rdy_overview", json!({}), claim("agent:b", "b1")),
        )
        .await;
        let (err, v) = tool(&ov);
        assert!(!err, "{v}");
        assert_eq!(v["session"], json!({"actor": "agent:b", "view": "b1"}));
        let ov = post(
            &mcp,
            &modern_headers("tools/call", Some("rdy_overview")),
            call(
                3,
                "rdy_overview",
                json!({}),
                modern_meta(claim("agent:b", "b1")),
            ),
        )
        .await;
        let (err, v) = tool(&ov);
        assert!(!err, "{ov:?}");
        assert_eq!(v["session"]["actor"], "agent:b");
        settled(&mcp, &key).await;
        // protocol before claim: rmcp's refusal of a bad Accept, not E_NO_SESSION
        let mut bad = headers(LEGACY);
        bad[1].1 = "application/json".into();
        let r = post(&mcp, &bad, call(4, "rdy_overview", json!({}), json!({}))).await;
        assert_eq!(r.status, 406, "{r:?}");
        // and a claimed call rmcp refuses releases its place in the claim's queue
        let r = post(
            &mcp,
            &bad,
            call(5, "rdy_overview", json!({}), claim("agent:b", "b1")),
        )
        .await;
        assert_eq!(r.status, 406, "{r:?}");
        settled(&mcp, &key).await;
        // a modern header that contradicts the body is rmcp's 400, ahead of the claim
        let r = post(
            &mcp,
            &modern_headers("tools/list", None),
            call(6, "rdy_overview", json!({}), modern_meta(json!({}))),
        )
        .await;
        assert_eq!(r.status, 400, "{r:?}");
        // the claim's own refusal is the tool's, in stdio's words
        let r = post(
            &mcp,
            &headers(LEGACY),
            call(7, "rdy_overview", json!({}), json!({})),
        )
        .await;
        let (err, v) = tool(&r);
        assert!(err);
        assert_eq!(
            (v["code"].as_str(), v["message"].as_str()),
            (Some("E_NO_SESSION"), Some("no session"))
        );
        // a body over the bound is 413 with D-QG54Z's text, before anything is parsed
        let h = headers(LEGACY);
        let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let r = mcp.post(&h, vec![b' '; tools::MAX_REQUEST_BYTES + 1]).await;
        assert_eq!(r.status, 413);
        assert_eq!(
            String::from_utf8_lossy(&r.body),
            "the MCP request body is over 16777216 bytes"
        );
    }

    /// A listen capacity that counts its reservations and their releases.
    struct Counted {
        taken: AtomicUsize,
        released: Arc<AtomicUsize>,
        refuse: AtomicBool,
        on_release: tokio::sync::mpsc::UnboundedSender<()>,
    }

    struct Held(Arc<AtomicUsize>, tokio::sync::mpsc::UnboundedSender<()>);
    impl Drop for Held {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
            let _ = self.1.send(());
        }
    }

    fn listen(id: u64, meta: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": "subscriptions/listen",
            "params": {"_meta": meta, "notifications": {"resourceSubscriptions": ["rdy://project"]}}})
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mht3_bridge_refuses_listen_before_sdk_ack() {
        let (_d, engine) = project();
        let (tx, mut released_rx) = tokio::sync::mpsc::unbounded_channel();
        let counted = Arc::new(Counted {
            taken: AtomicUsize::new(0),
            released: Default::default(),
            refuse: AtomicBool::new(false),
            on_release: tx,
        });
        let c = Arc::clone(&counted);
        let capacity: ListenCapacity = Arc::new(move || {
            if c.refuse.load(Ordering::SeqCst) {
                return Err(RdyError::new(E_LIMIT, "listens are full", "cancel one"));
            }
            c.taken.fetch_add(1, Ordering::SeqCst);
            Ok(ListenReservation::new(Held(
                Arc::clone(&c.released),
                c.on_release.clone(),
            )))
        });
        let mcp = HttpMcp::new(engine, NotifyHub::new()).with_listen_capacity(capacity);
        let h = modern_headers("subscriptions/listen", None);
        // a refused listen answers at once; an acknowledged one would stream until cancelled,
        // so a hang guard turns that into a failure
        let post = |mcp: &'static HttpMcp, h: Vec<(&'static str, String)>, body: Value| async move {
            tokio::time::timeout(std::time::Duration::from_secs(60), post(mcp, &h, body))
                .await
                .expect("a refused listen answers without a stream")
        };
        let mcp: &'static HttpMcp = Box::leak(Box::new(mcp));
        let h2 = h.clone();
        let h = &h2;
        let refused = |p: &Posted| {
            assert_eq!(p.status, 200, "{p:?}");
            assert_eq!(p.header("content-type"), Some("application/json"), "{p:?}");
            let msgs = p.messages();
            assert_eq!(msgs.len(), 1, "one reply and no acknowledgement: {msgs:?}");
            msgs[0]["error"]["data"].clone()
        };
        // no claim: E_NO_SESSION, no reservation, no acknowledgement
        let p = post(mcp, h.clone(), listen(1, modern_meta(json!({})))).await;
        let data = refused(&p);
        assert_eq!(data["code"], "E_NO_SESSION", "{p:?}");
        assert_eq!(p.reply()["id"], 1);
        // an options-only claim is no identity either
        let p = post(
            mcp,
            h.clone(),
            listen(2, modern_meta(json!({"rdy/role": "reader"}))),
        )
        .await;
        assert_eq!(refused(&p)["code"], "E_NO_SESSION");
        // a malformed claim keeps its parse error
        let p = post(
            mcp,
            h.clone(),
            listen(3, modern_meta(json!({"rdy/actr": "x"}))),
        )
        .await;
        assert_eq!(
            refused(&p)["message"],
            "_meta: unknown parameter \"rdy/actr\""
        );
        assert_eq!(counted.taken.load(Ordering::SeqCst), 0);
        // a full capacity: its refusal, before rmcp acknowledges anything
        counted.refuse.store(true, Ordering::SeqCst);
        let p = post(
            mcp,
            h.clone(),
            listen(4, modern_meta(claim("agent:l", "l1"))),
        )
        .await;
        let data = refused(&p);
        assert_eq!(
            (data["code"].as_str(), data["message"].as_str()),
            (Some("E_LIMIT"), Some("listens are full"))
        );
        // protocol first: an older revision's listen is rmcp's refusal, not the claim's
        let p = post(mcp, headers(LEGACY), listen(5, json!({}))).await;
        assert!(
            p.reply()["error"]["data"].get("code").is_none(),
            "rmcp's own refusal: {p:?}"
        );
        // admitted: the reservation rides to the listen, which rmcp acknowledges on a stream
        counted.refuse.store(false, Ordering::SeqCst);
        let mut request = http::Request::builder().method("POST").uri("/~mcp");
        for (k, v) in h {
            request = request.header(*k, v.as_str());
        }
        let body = listen(6, modern_meta(claim("agent:l", "l1"))).to_string();
        let response = mcp
            .handle(request.body(Full::new(Bytes::from(body))).unwrap())
            .await;
        assert_eq!(
            response.headers()[http::header::CONTENT_TYPE],
            "text/event-stream"
        );
        let mut body = response.into_body();
        let mut seen = String::new();
        while !seen.contains("acknowledged") {
            let frame = body.frame().await.expect("a frame").unwrap();
            if let Ok(d) = frame.into_data() {
                seen.push_str(&String::from_utf8_lossy(&d));
            }
        }
        assert_eq!(counted.taken.load(Ordering::SeqCst), 1);
        assert_eq!(
            counted.released.load(Ordering::SeqCst),
            0,
            "the listen holds it"
        );
        // the client goes: the stream's cancellation releases the reservation
        drop(body);
        released_rx
            .recv()
            .await
            .expect("the reservation is released");
        assert_eq!(counted.released.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mht3_admission_strips_the_bearer_before_the_sdk() {
        let (_d, engine) = project();
        let mcp = HttpMcp::new(engine, NotifyHub::new());
        let mut h = headers(LEGACY);
        h.push(("authorization", "Bearer SECRETSECRETSECRET".into()));
        h.push(("cookie", "rdy=SECRETCOOKIE".into()));
        h.push(("x-mht3-probe", "1".into()));
        let r = post(
            &mcp,
            &h,
            call(1, "rdy_overview", json!({}), claim("agent:s", "s1")),
        )
        .await;
        assert!(!tool(&r).0, "{r:?}");
        let seen = mcp.seen.lock().unwrap().clone();
        let parts = seen
            .iter()
            .find(|h| h.contains_key("x-mht3-probe"))
            .expect("the handler saw the request's parts");
        assert!(
            parts.get(http::header::AUTHORIZATION).is_none(),
            "{parts:?}"
        );
        assert!(parts.get(http::header::COOKIE).is_none(), "{parts:?}");
        assert!(
            !format!("{parts:?}").contains("SECRET"),
            "no secret in a Debug print of what the SDK holds"
        );
    }

    // lsc1 (D-52A2G): a call that has not taken its claim's turn is withdrawn when its client
    // closes its response; a call that has started runs to its end, once.

    /// A hang guard for what an lsc1 test waits on: a failure message, never a measure.
    const LSC1_GUARD: std::time::Duration = std::time::Duration::from_secs(30);

    /// An engine whose every write stops at a gate: it says that a write came, and the write
    /// goes on only when the test releases it. `landed` counts the writes that landed.
    struct GatedWrites {
        inner: Local,
        entered: tokio::sync::mpsc::UnboundedSender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
        landed: AtomicUsize,
    }

    impl Engine for GatedWrites {
        fn read(&self) -> Arc<rdy_engine::ReadState> {
            self.inner.read()
        }
        fn read_serialized(&self, f: rdy_engine::engine::ReadFn) -> Result<Value, RdyError> {
            self.inner.read_serialized(f)
        }
        fn republish(&self) -> Result<(), RdyError> {
            self.inner.republish()
        }
        fn write(&self, f: rdy_engine::engine::WriteFn) -> Result<Value, RdyError> {
            let _ = self.entered.send(());
            self.release
                .lock()
                .unwrap()
                .recv_timeout(LSC1_GUARD)
                .expect("the test releases the write");
            let out = self.inner.write(f);
            if out.is_ok() {
                self.landed.fetch_add(1, Ordering::SeqCst);
            }
            out
        }
        fn open_report(&self) -> rdy_engine::OpenReport {
            self.inner.open_report()
        }
        fn root(&self) -> std::path::PathBuf {
            self.inner.root()
        }
        fn briefs(&self) -> Arc<rdy_engine::BriefRegistry> {
            self.inner.briefs()
        }
        fn leases(&self) -> Arc<LeaseRegistry> {
            self.inner.leases()
        }
    }

    /// The test's side of a [`GatedWrites`] engine served over HTTP.
    struct Gate {
        _dir: tempfile::TempDir,
        mcp: Arc<HttpMcp>,
        engine: Arc<GatedWrites>,
        entered: tokio::sync::mpsc::UnboundedReceiver<()>,
        release: std::sync::mpsc::Sender<()>,
    }

    impl Gate {
        fn new() -> Gate {
            let dir = tempfile::tempdir().unwrap();
            let rdy = rdy_engine::Rdy::init(dir.path(), "rdy-standard").unwrap();
            let (entered_tx, entered) = tokio::sync::mpsc::unbounded_channel();
            let (release, release_rx) = std::sync::mpsc::channel();
            let engine = Arc::new(GatedWrites {
                inner: Local::new(rdy),
                entered: entered_tx,
                release: Mutex::new(release_rx),
                landed: AtomicUsize::new(0),
            });
            let mcp = Arc::new(HttpMcp::new(
                Arc::clone(&engine) as Arc<dyn Engine>,
                NotifyHub::new(),
            ));
            Gate {
                _dir: dir,
                mcp,
                engine,
                entered,
                release,
            }
        }

        /// Waits for the next write at the gate.
        async fn write_comes(&mut self, what: &str) {
            lsc1_within(what, self.entered.recv())
                .await
                .expect("the engine is alive");
        }

        /// Lets the write at the gate go on.
        fn release(&self) {
            self.release.send(()).unwrap();
        }

        fn landed(&self) -> usize {
            self.engine.landed.load(Ordering::SeqCst)
        }

        /// How many calls reached their claim's turn: the handler reads its admission just
        /// before it waits for the turn, with no await between.
        fn reached(&self) -> usize {
            self.mcp.seen.lock().unwrap().len()
        }
    }

    async fn lsc1_within<T>(what: &str, f: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(LSC1_GUARD, f)
            .await
            .unwrap_or_else(|_| panic!("{what}"))
    }

    async fn lsc1_until(what: &str, mut f: impl FnMut() -> bool) {
        lsc1_within(what, async {
            while !f() {
                tokio::task::yield_now().await;
            }
        })
        .await
    }

    fn lsc1_key() -> Key {
        ("agent:q".to_string(), Some("q1".to_string()))
    }

    /// An `rdy_write` of claim agent:q/q1 that makes one folder, `name`.
    fn lsc1_write(id: u64, name: &str) -> Value {
        let mut args = json!({"ops": [{"op": "create", "type": "folder", "id": "$f",
            "fields": {"name": name, "title": name}}]});
        let owner = rdy_engine::kungfu::policy("rdy_write")
            .and_then(|p| p.ruleset)
            .and_then(|r| r.strip_prefix("rdy:"))
            .and_then(rdy_engine::kungfu::owner);
        if let Some(o) = owner {
            args["kungfu"] = json!(o.value);
        }
        call(id, "rdy_write", args, claim("agent:q", "q1"))
    }

    /// One request through the admission layer, as `post` sends it, counted by `activity`
    /// when given; its task is the client, and aborting it closes the response.
    fn lsc1_spawn(
        mcp: &Arc<HttpMcp>,
        body: Value,
        activity: Option<Activity>,
    ) -> tokio::task::JoinHandle<Posted> {
        let mcp = Arc::clone(mcp);
        tokio::spawn(async move {
            let mut request = http::Request::builder().method("POST").uri("/~mcp");
            for (k, v) in headers(LEGACY) {
                request = request.header(k, v);
            }
            let mut request = request
                .body(Full::new(Bytes::from(body.to_string().into_bytes())))
                .unwrap();
            if let Some(a) = activity {
                request.extensions_mut().insert(a);
            }
            let response = mcp.handle(request).await;
            let status = response.status().as_u16();
            let body = match response.into_body().collect().await {
                Ok(c) => c.to_bytes().to_vec(),
                Err(never) => match never {},
            };
            Posted {
                status,
                headers: Vec::new(),
                body,
            }
        })
    }

    /// The client closes `call`'s response, and its task is gone.
    async fn lsc1_close(call: tokio::task::JoinHandle<Posted>) {
        call.abort();
        let gone = lsc1_within("the client's task ends", call).await;
        assert!(gone.unwrap_err().is_cancelled(), "the client closed first");
    }

    /// A daemon activity permit that says when it is released.
    struct Permit(tokio::sync::mpsc::UnboundedSender<()>);
    impl Drop for Permit {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    // a single thread: once the test runs again, a call that reached its turn waits in it
    #[tokio::test(flavor = "current_thread")]
    async fn lsc1_a_cancelled_waiting_call_is_withdrawn_and_frees_its_admission() {
        let mut g = Gate::new();
        let key = lsc1_key();
        // call 1 takes the turn and stops in its write
        let one = lsc1_spawn(&g.mcp, lsc1_write(1, "one"), None);
        g.write_comes("call 1 writes").await;
        // call 2 waits for the turn, counted by the daemon
        let (permit_tx, mut permit_released) = tokio::sync::mpsc::unbounded_channel();
        let activity = Activity::new(Box::new(Permit(permit_tx)), None);
        let two = lsc1_spawn(&g.mcp, lsc1_write(2, "two"), Some(activity));
        lsc1_until("call 2 waits for its turn", || g.reached() == 2).await;
        assert_eq!(g.mcp.queued(&key), 2);
        // its client closes the response while call 1 still holds the turn
        lsc1_close(two).await;
        lsc1_until("the closed call leaves the queue at once", || {
            g.mcp.queued(&key) == 1
        })
        .await;
        lsc1_within(
            "the closed call releases its activity permit",
            permit_released.recv(),
        )
        .await;
        // its place is free: eight new calls are admitted beside call 1, and none is E_LIMIT
        let mut waiting = Vec::new();
        for i in 0..MAX_WAITING_CALLS as u64 {
            waiting.push(lsc1_spawn(
                &g.mcp,
                lsc1_write(10 + i, &format!("w{i}")),
                None,
            ));
            lsc1_until("the new call waits for its turn", || {
                g.reached() == 3 + i as usize
            })
            .await;
        }
        assert_eq!(g.mcp.queued(&key), MAX_WAITING_CALLS + 1);
        assert!(waiting.iter().all(|w| !w.is_finished()), "none was refused");
        // every admitted call writes once; the closed call never runs
        for n in 0..=MAX_WAITING_CALLS {
            if n > 0 {
                g.write_comes("the next waiting call writes").await;
            }
            g.release();
        }
        let (err, v) = tool(&lsc1_within("call 1 ends", one).await.unwrap());
        assert!(!err, "{v}");
        for w in waiting {
            let (err, v) = tool(&lsc1_within("a waiting call ends", w).await.unwrap());
            assert!(!err, "{v}");
        }
        settled(&g.mcp, &key).await;
        assert_eq!(g.landed(), MAX_WAITING_CALLS + 1, "call 2 wrote nothing");
        assert!(
            g.entered.try_recv().is_err(),
            "no write came after the last"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lsc1_a_cancelled_started_call_completes_its_write_once() {
        let mut g = Gate::new();
        let key = lsc1_key();
        // call 1 holds the turn and stops in its write; call 2 waits behind it
        let one = lsc1_spawn(&g.mcp, lsc1_write(1, "one"), None);
        g.write_comes("call 1 writes").await;
        let two = lsc1_spawn(&g.mcp, lsc1_write(2, "two"), None);
        lsc1_until("call 2 waits for its turn", || g.reached() == 2).await;
        // both clients close their responses: the started call first
        lsc1_close(one).await;
        lsc1_close(two).await;
        // call 1's write goes on and lands; call 2 never starts
        g.release();
        let ended = async {
            while g.mcp.queued(&key) > 0 {
                tokio::task::yield_now().await;
            }
        };
        tokio::select! {
            () = lsc1_within("every call ends", ended) => {}
            _ = g.entered.recv() => panic!("the closed waiting call ran a write"),
        }
        assert_eq!(
            g.landed(),
            1,
            "the started call's write landed exactly once"
        );
        assert!(g.entered.try_recv().is_err(), "rdy never ran it again");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lsc1_waiting_calls_keep_their_order_past_a_withdrawn_call() {
        let mut g = Gate::new();
        let key = lsc1_key();
        // call 1 holds the turn; calls 2, 3 and 4 wait in that order
        let one = lsc1_spawn(&g.mcp, lsc1_write(1, "one"), None);
        g.write_comes("call 1 writes").await;
        let two = lsc1_spawn(&g.mcp, lsc1_write(2, "two"), None);
        lsc1_until("call 2 waits", || g.reached() == 2).await;
        let three = lsc1_spawn(&g.mcp, lsc1_write(3, "three"), None);
        lsc1_until("call 3 waits", || g.reached() == 3).await;
        let four = lsc1_spawn(&g.mcp, lsc1_write(4, "four"), None);
        lsc1_until("call 4 waits", || g.reached() == 4).await;
        // call 3's client goes
        lsc1_close(three).await;
        // call 1 ends, then call 2 has the turn
        g.release();
        assert!(!tool(&lsc1_within("call 1 ends", one).await.unwrap()).0);
        g.write_comes("call 2 writes").await;
        assert!(!four.is_finished());
        g.release();
        assert!(!tool(&lsc1_within("call 2 ends", two).await.unwrap()).0);
        // the next write is call 4's: call 3 was withdrawn, and the queue goes on
        g.write_comes("the next waiting call writes").await;
        g.release();
        tokio::select! {
            r = lsc1_within("call 4 ends", four) => assert!(!tool(&r.unwrap()).0),
            _ = g.entered.recv() => panic!("the withdrawn call 3 took a turn before call 4"),
        }
        settled(&g.mcp, &key).await;
        assert_eq!(g.landed(), 3);
        assert!(
            g.entered.try_recv().is_err(),
            "no write came after call 4's"
        );
    }
}
