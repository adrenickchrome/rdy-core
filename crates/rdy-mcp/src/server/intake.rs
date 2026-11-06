//! The `intake` family's tool registration, and the two protocol flows of an elicited answer
//! (D-6M0AF, TASK-982 O4 and O5).
//!
//! On 2026-07-28 the first call returns `input_required` with a form and an opaque handle, and the
//! client retries the call with the person's response. On 2025-11-25 and 2025-06-18 the call sends
//! `elicitation/create` itself and waits for the reply, holding no state lock, registry lock or
//! engine writer. Both flows keep the card waiting in the hub's registry, consume it once, and
//! reach the writer only with a validated choice, through the engine's presented answer.

use super::CALL;
use crate::claims;
use crate::elicitation::{self, ANSWER_KEY, Bound, EXPIRY, Route, Terminal};
use crate::params::{Parameters, parse_params};
use crate::server::RdyServer;
use crate::tools::{self, AnswerParams, McpState, PendingParams, session};
use rdy_engine::RdyError;
use rdy_engine::answer::{Presentation, PresentedAnswer, presented_item, run_presented_answer};
use rdy_engine::pending::Item;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, CancelledNotificationParam,
    ClientResult, ElicitRequest, InputRequest, InputRequiredResult, JsonObject, ServerRequest,
};
use rmcp::service::{PeerRequestOptions, RequestContext, ServiceError};
use rmcp::{RoleServer, tool, tool_router};
use serde_json::Value;
use std::sync::{Arc, Mutex};

const TOOL: &str = "rdy_answer";

/// Where an elicited call stands: asking for the person's input, or its final value.
enum Step {
    Input(InputRequiredResult),
    Done(Value),
}

/// What a valid terminal response leads to.
enum Settled {
    /// The reply of a form that writes nothing.
    Reply(Value),
    /// The person's validated choice.
    Chosen(String),
}

#[tool_router(router = intake_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Read what waits for the person: {items, not_ready, snoozed, omitted}. Each item has a ref, kind (approve, confirm, answer, resolve, defer, reaffirm), options, card and card_hash. Each option has an id, a label and its consequences. The recommended option carries recommended: true and a reason of at most 200 characters; no other option carries either. A question block's own recommended field is different: it is the exact text of one option.\n\nShow the card to the person word for word. Keep its options, recommendation and consequences unchanged. A batch (approve) stands first, its exceptions (confirm) beside it. Pass session_ref (the chat session) so Later hides a card only there; max_tokens cuts each card. A defer card and a reaffirm card need the person's own choice and carry no recommended option; show both options and wait for the person. not_ready lists the versions that need work from you first."
    )]
    async fn rdy_pending(&self, Parameters(p): Parameters<PendingParams>) -> CallToolResult {
        self.run("rdy_pending", p, tools::rdy_pending).await
    }

    #[tool(
        description = "Relay the person's answer to one card: ref, option (an option id of the card), card_hash (of the card you showed) and the person's words as evidence. It records the person as the one who answered and you as the relay. A card or version that changed is refused with the fresh card in corrected; show it and ask again. An approval waits until every exception beside the batch has a confirm answer. An approving answer to a version that is not ready is refused with E_PROPOSAL_NOT_READY, and issues lists why. Pass all_recommended: [{ref, card_hash}] to answer each item with its recommended option. A defer card, a reaffirm card, and an approve card showing a concern or fail check verdict that no person accepted have no recommended option. all_recommended refuses them with E_BAD_INPUT (<ref> has no recommended option); answer each of them on its own. kind is card (default) or text. brief is for a question under stable content. With signing on, the act and its signatures share one batch. The result adds signatures. Under signing.sign auto, a signing failure saves the act unsigned and adds W_SIGNING_FAILED. Under always, the act is refused. With elicit: true, pass ref and card_hash, and brief for a question under stable content. The client shows rdy's card and returns the person's choice straight to rdy. If elicitation is unavailable, show the card in chat and relay the answer with option and evidence."
    )]
    async fn rdy_answer(&self, Parameters(p): Parameters<AnswerParams>) -> CallToolResult {
        self.run("rdy_answer", p, tools::rdy_answer).await
    }
}

impl RdyServer {
    /// Whether this call is an elicited answer: a first ask, or anything that carries a
    /// continuation. The relay path never reads a handle, so a stray one is refused there.
    pub(super) fn elicits(request: &CallToolRequestParams) -> bool {
        request.name == TOOL
            && (request.request_state.is_some()
                || request.input_responses.is_some()
                || request
                    .arguments
                    .as_ref()
                    .and_then(|a| a.get("elicit"))
                    .is_some_and(|e| *e == Value::Bool(true)))
    }

    /// The elicited answer's reply: a refusal is the tool's own error reply.
    pub(super) async fn answer_elicited(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> CallToolResponse {
        let out = match self.elicited(request, context).await {
            // not a tool result: the exchange's final result carries any warning
            Ok(Step::Input(asked)) => return CallToolResponse::InputRequired(asked),
            Ok(Step::Done(v)) => Ok(v),
            Err(e) => Err(e),
        };
        let out = self.warned(out).await;
        self.reply(out).into()
    }

    /// `out`, the final result of an elicited answer, with the build warning it is owed.
    async fn warned(&self, out: Result<Value, RdyError>) -> Result<Value, RdyError> {
        self.blocking(TOOL, false, move |st| {
            claims::take_build_warning(st);
            claims::deliver_build_warning(st, out)
        })
        .await
    }

    async fn elicited(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<Step, RdyError> {
        let args = request.arguments.unwrap_or_default();
        if request.request_state.is_some() || request.input_responses.is_some() {
            let terminal = elicitation::parse_responses(request.input_responses.as_ref());
            let v = self
                .continued(args, request.request_state, terminal)
                .await?;
            return Ok(Step::Done(v));
        }
        let route = elicitation::route(
            context.protocol_version().as_ref(),
            context.client_capabilities().as_ref(),
        );
        let (bound, item, handle) = self
            .opened(move |st| {
                let (ask, s) = asked(st, args)?;
                if route == Route::Unavailable {
                    return Err(elicitation::unavailable());
                }
                let person = rdy_engine::human_actor_for_env(&st.engine.root())?;
                let state = st.engine.read();
                state.ensure_current_epoch()?;
                let item = presented_item(&state, &person, &ask.item, Some(&ask.card_hash))?;
                let bound = ask.bound(s.actor, s.run);
                let handle = st.hub.elicitation().insert(bound.clone())?;
                Ok((bound, item, handle))
            })
            .await?;
        match route {
            Route::InCall { enum_names } => {
                let v = self
                    .in_call(bound, &item, handle, enum_names, context)
                    .await?;
                Ok(Step::Done(v))
            }
            _ => {
                let request =
                    InputRequest::Elicitation(ElicitRequest::new(elicitation::form(&item, false)));
                Ok(Step::Input(InputRequiredResult::new(
                    Some([(ANSWER_KEY.to_string(), request)].into_iter().collect()),
                    Some(handle),
                )))
            }
        }
    }

    /// The retry of a call that returned `input_required`.
    async fn continued(
        &self,
        args: JsonObject,
        handle: Option<String>,
        terminal: Option<Terminal>,
    ) -> Result<Value, RdyError> {
        let (bound, settled) = self
            .opened(move |st| {
                let (ask, s) = asked(st, args)?;
                let handle = handle.ok_or_else(elicitation::gone)?;
                let bound = ask.bound(s.actor, s.run);
                let settled = settle(st, &handle, &bound, terminal)?;
                Ok((bound, settled))
            })
            .await?;
        self.finish(bound, settled).await
    }

    /// The reply of a settled card: a choice goes to the engine's bound answer, as the person,
    /// with no note. Signing uses the call's own agent and cancel.
    async fn finish(&self, bound: Bound, settled: Settled) -> Result<Value, RdyError> {
        let choice = match settled {
            Settled::Reply(v) => return Ok(v),
            Settled::Chosen(choice) => choice,
        };
        let claim = CALL.try_with(|c| c.claim.clone()).ok().flatten();
        self.blocking(TOOL, true, move |st| {
            claims::call(st, claim.as_ref(), TOOL, |st| {
                let caller = session(st)?;
                let person = rdy_engine::human_actor_for_env(&st.engine.root())?;
                run_presented_answer(
                    &*st.engine,
                    &*st.agent,
                    &caller,
                    &person,
                    Presentation::Elicited,
                    PresentedAnswer {
                        reference: bound.item,
                        card_hash: bound.card_hash,
                        option: choice,
                        note: None,
                        brief: bound.brief,
                    },
                )
                .map_err(elicitation::elicited_refusal)
            })
        })
        .await
    }

    /// `elicitation/create` inside the call. The wait ends at the client's reply, the expiry, the
    /// call's cancel or the session's end. Only the reply can answer.
    async fn in_call(
        &self,
        bound: Bound,
        item: &Item,
        handle: String,
        enum_names: bool,
        context: RequestContext<RoleServer>,
    ) -> Result<Value, RdyError> {
        let registry = Arc::clone(&self.hub);
        let peer = context.peer.clone();
        let mut options = PeerRequestOptions::no_options();
        options.timeout = Some(EXPIRY);
        let ask =
            ServerRequest::ElicitRequest(ElicitRequest::new(elicitation::form(item, enum_names)));
        let sent = match peer.send_request_with_option(ask, options).await {
            Ok(sent) => sent,
            Err(_) => {
                registry.elicitation().discard(&handle);
                return Ok(elicitation::cancelled());
            }
        };
        let id = sent.id.clone();
        let reply = tokio::select! {
            biased;
            () = context.ct.cancelled() => None,
            reply = sent.await_response() => Some(reply),
        };
        let terminal = match reply {
            None => {
                registry.elicitation().discard(&handle);
                // the person's form closes too; a closed session has nobody to tell
                let note = CancelledNotificationParam::new(Some(id), None);
                let _ = peer.notify_cancelled(note).await;
                return Ok(elicitation::cancelled());
            }
            Some(Err(ServiceError::Timeout { .. })) => {
                registry.elicitation().discard(&handle);
                return Err(elicitation::gone());
            }
            Some(Err(ServiceError::Cancelled { .. } | ServiceError::TransportClosed)) => {
                registry.elicitation().discard(&handle);
                return Ok(elicitation::cancelled());
            }
            Some(Ok(ClientResult::ElicitResult(r))) => serde_json::to_value(&r)
                .ok()
                .and_then(|v| elicitation::parse_result(&v)),
            Some(_) => None,
        };
        let (bound, settled) = self
            .opened(move |st| {
                let settled = settle(st, &handle, &bound, terminal)?;
                Ok((bound, settled))
            })
            .await?;
        self.finish(bound, settled).await
    }

    /// One claimed step on the blocking pool, with no pack notice: `f` runs under the state lock
    /// and its value comes back as it is.
    async fn opened<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut McpState) -> Result<T, RdyError> + Send + 'static,
    ) -> Result<T, RdyError> {
        let claim = CALL.try_with(|c| c.claim.clone()).ok().flatten();
        let check = CALL.try_with(|c| c.kungfu.clone()).ok();
        let slot = Arc::new(Mutex::new(None));
        let put = Arc::clone(&slot);
        self.blocking(TOOL, false, move |st| {
            claims::call(st, claim.as_ref(), TOOL, |st| {
                // the first claimed step of a request counts it; a later step finds it counted
                if let Some(check) = &check {
                    crate::kungfu::evaluate(st, check);
                }
                let value = f(st)?;
                *put.lock().unwrap_or_else(|p| p.into_inner()) = Some(value);
                Ok(Value::Null)
            })
        })
        .await?;
        let value = slot.lock().unwrap_or_else(|p| p.into_inner()).take();
        Ok(value.expect("a step that succeeded left its value"))
    }
}

/// The closed input of a call, under its claim.
fn asked(
    st: &mut McpState,
    args: JsonObject,
) -> Result<(elicitation::Ask, rdy_engine::Session), RdyError> {
    let p = parse_params::<AnswerParams>(TOOL, args.clone())?;
    if p.elicit != Some(true) {
        // a handle or a response on a call that is no elicited answer
        return Err(elicitation::gone());
    }
    let ask = elicitation::ask_of(&p, Some(&args))?;
    Ok((ask, session(st)?))
}

/// Consumes the card, then reads what the person did. A malformed response has consumed it; a
/// choice is checked against the card as it stands now.
fn settle(
    st: &mut McpState,
    handle: &str,
    bound: &Bound,
    terminal: Option<Terminal>,
) -> Result<Settled, RdyError> {
    st.hub.elicitation().take(handle, bound)?;
    let terminal = terminal.ok_or_else(elicitation::invalid_answer)?;
    if let Some(state) = terminal.state() {
        return Ok(Settled::Reply(state));
    }
    let Terminal::Accept(choice) = terminal else {
        return Err(elicitation::invalid_answer());
    };
    let person = rdy_engine::human_actor_for_env(&st.engine.root())?;
    let state = st.engine.read();
    state.ensure_current_epoch()?;
    let item = presented_item(&state, &person, &bound.item, Some(&bound.card_hash))?;
    if !item.options.iter().any(|o| o.id == choice) {
        return Err(elicitation::invalid_answer());
    }
    Ok(Settled::Chosen(choice))
}
