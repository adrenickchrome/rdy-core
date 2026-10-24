//! vapi1: the one view request dispatcher behind MCP's `rdy_view` and the ctl `view` op. Writes
//! go through `write_typed` (they run on the engine's writer), include, exclude and undo through
//! `view_op_and_report` (with their consequences); reads use one `engine.read()` snapshot.
use crate::answer::relay::RelayInput;
use crate::consequence::{op_effect, undo_effect, view_op_and_report};
use crate::engine::{Engine, write_typed};
use crate::error::*;
use crate::{Session, now_ms};
use serde::Deserialize;
use serde_json::{Value, json};

/// A new owner as `own` takes it (D-PWB82): the actor, and the run in its own field. An input
/// only, never stored: `own` checks both and copies them into the stored `Owner`. An omitted or
/// null run is no run; an empty one is refused.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerInput {
    pub actor: String,
    pub run: Option<String>,
}

/// One view request: `op` and the fields that op takes.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewRequest {
    pub op: String,
    pub name: Option<String>,
    pub from: Option<String>,
    pub base: Option<String>,
    pub pin: Option<bool>,
    pub view: Option<String>,
    pub views: Option<Vec<String>>,
    pub changes: Option<Vec<String>>,
    pub cascade: Option<bool>,
    pub a: Option<String>,
    pub b: Option<String>,
    pub all: Option<bool>,
    pub stale: Option<u32>,
    pub reason: Option<String>,
    pub owner: Option<OwnerInput>,
    pub none: Option<bool>,
    pub target: Option<String>,
    /// resolve: the conflicted block (id, key or path).
    pub block: Option<String>,
    /// resolve: take the landed side (cf1, D-X4GMH); required, the one side resolve takes.
    pub landed: Option<bool>,
    /// include, exclude and undo: cut the consequences to about this many tokens.
    pub max_tokens: Option<usize>,
    /// own and close with stale: the person's words, quoted, when an agent relays the act.
    pub evidence: Option<String>,
    /// own and close with stale: the question the person was asked.
    pub question: Option<String>,
    /// own and close with stale: where the person said it (default: `mcp` over MCP, `cli` at
    /// the command line).
    pub session_ref: Option<String>,
    /// own and close with stale, at the command line: the agent that relays, its actor alone
    /// (the relay records the session's run).
    pub relayed_by: Option<String>,
}

impl ViewRequest {
    fn relay(&self) -> RelayInput {
        RelayInput {
            relayed_by: self.relayed_by.clone(),
            evidence: self.evidence.clone(),
            question: self.question.clone(),
            session_ref: self.session_ref.clone(),
            kind: None,
        }
    }
}

fn needs(message: &str, fix: &str) -> RdyError {
    RdyError::new(E_BAD_INPUT, message, fix)
}

fn to_json<T: serde::Serialize>(v: T) -> Result<Value, RdyError> {
    serde_json::to_value(v).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("a view read did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })
}

/// Runs `req` for session `s`, whose own view is `default_view`: the view an op acts on is
/// `view` (or `name`, for close, own and show), else `default_view`. `person` is the project's
/// human actor, from the caller (O18): `own` and `close` with `stale` are a person's acts, and
/// an agent relays them with the person's words, so the record names `person` as the actor.
pub fn run_view(
    engine: &dyn Engine,
    s: &Session,
    person: &str,
    default_view: &str,
    req: ViewRequest,
) -> Result<Value, RdyError> {
    let relays = req.relay() != RelayInput::default();
    let person_act = req.op == "own" || (req.op == "close" && req.stale.is_some());
    if relays && !person_act {
        return Err(needs(
            "evidence, question, session_ref and relayed_by apply to own and close with stale",
            "drop them, or relay a person's own or close with stale",
        ));
    }
    let (s, person) = (s.clone(), person.to_string());
    let named = || req.view.clone().or_else(|| req.name.clone());
    let view = || req.view.clone().unwrap_or_else(|| default_view.to_string());
    match req.op.as_str() {
        "create" => {
            let name = req
                .name
                .clone()
                .ok_or_else(|| needs("create needs name", "pass name: the new view's name"))?;
            let (base, pin) = (req.base.clone(), req.pin.unwrap_or(false));
            write_typed(engine, move |r| {
                r.view_create(&s, &name, base.as_deref(), pin)
            })
        }
        "fork" => {
            let (Some(from), Some(name)) = (req.from.clone(), req.name.clone()) else {
                return Err(needs(
                    "fork needs from and name",
                    "pass from: the view to fork, and name: the new view's name",
                ));
            };
            write_typed(engine, move |r| r.view_fork(&s, &from, &name))
        }
        "include" => {
            let changes = req.changes.clone().ok_or_else(|| {
                needs(
                    "include needs changes",
                    "pass changes: the change ids to include",
                )
            })?;
            let view = view();
            let v = view.clone();
            view_op_and_report(engine, &v, req.max_tokens, op_effect, move |r| {
                r.view_include(&s, &view, &changes)
            })
        }
        "exclude" => {
            let changes = req.changes.clone().ok_or_else(|| {
                needs(
                    "exclude needs changes",
                    "pass changes: the change ids to exclude",
                )
            })?;
            let (view, cascade) = (view(), req.cascade.unwrap_or(false));
            let v = view.clone();
            view_op_and_report(engine, &v, req.max_tokens, op_effect, move |r| {
                r.view_exclude(&s, &view, &changes, cascade)
            })
        }
        "compare" => {
            let st = engine.read();
            let a = req.a.as_deref().unwrap_or(default_view);
            let b = req.b.as_deref().unwrap_or("canon");
            let a = st.resolve_read_scope(a, Some(default_view))?;
            let b = st.resolve_read_scope(b, Some(default_view))?;
            to_json(st.compare(&a, &b)?)
        }
        "list" => {
            let st = engine.read();
            let days = req.stale.unwrap_or_else(|| st.idle_days());
            let views = st.view_list(req.all.unwrap_or(false), days, now_ms())?;
            Ok(json!({ "views": to_json(views)? }))
        }
        "show" => {
            let st = engine.read();
            let view = named().unwrap_or_else(|| default_view.to_string());
            to_json(st.view_show(&view)?)
        }
        "close" => {
            let names = match (req.views.clone(), named()) {
                (Some(views), _) => Some(views),
                (None, Some(v)) => Some(vec![v]),
                (None, None) => None,
            };
            match (names, req.stale) {
                (None, Some(days)) => {
                    // `kept`: the session's own default view, idle but never closed this way
                    let relay = req.relay();
                    write_typed(engine, move |r| {
                        r.view_close_stale_as(&s, &person, &relay, days, now_ms())
                    })
                }
                (Some(names), _) => {
                    if relays {
                        return Err(needs(
                            "a relay applies to close with stale, not to named views",
                            "close the views yourself, or pass stale with the person's words",
                        ));
                    }
                    let reason = req.reason.clone().unwrap_or_default();
                    let closed = write_typed(engine, move |r| r.view_close(&s, &names, &reason))?;
                    Ok(json!({ "closed": closed }))
                }
                (None, None) => Err(needs(
                    "close needs a view, views or stale",
                    "pass view, views, or stale: <days> to close the idle views",
                )),
            }
        }
        "own" => {
            let view = named()
                .ok_or_else(|| needs("own needs a view", "pass view: the view to reassign"))?;
            let owner = match (req.owner.clone(), req.none.unwrap_or(false)) {
                (Some(_), true) => {
                    return Err(needs(
                        "own takes an owner or none, not both",
                        "pass owner: {actor, run?}, or none: true to leave it unowned",
                    ));
                }
                (None, false) => {
                    return Err(needs(
                        "own needs an owner or none",
                        "pass owner: {actor, run?}, or none: true to leave it unowned",
                    ));
                }
                (owner, _) => owner,
            };
            let relay = req.relay();
            write_typed(engine, move |r| {
                r.view_own_as(&s, &person, &relay, &view, owner.as_ref())
            })
        }
        "resolve" => {
            let block = req.block.clone().ok_or_else(|| {
                needs(
                    "resolve needs block",
                    "pass block: the conflicted block, and landed: true",
                )
            })?;
            if req.landed != Some(true) {
                return Err(needs(
                    "resolve needs landed: true",
                    "pass landed: true to take the landed side of a conflict on an accepted or rejected decision",
                ));
            }
            let view = view();
            write_typed(engine, move |r| r.view_resolve_landed(&s, &view, &block))
        }
        "undo" => {
            let (view, target, cascade) =
                (view(), req.target.clone(), req.cascade.unwrap_or(false));
            let v = view.clone();
            view_op_and_report(engine, &v, req.max_tokens, undo_effect, move |r| {
                r.undo(&s, &view, target.as_deref(), cascade)
            })
        }
        op => Err(RdyError::new(
            E_BAD_INPUT,
            format!("unknown view op {op:?}"),
            "use create, fork, include, exclude, compare, list, show, close, own, resolve or undo",
        )),
    }
}
