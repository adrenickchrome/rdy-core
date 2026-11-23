//! vapi1: the one view request dispatcher behind MCP's `rdy_view` and the ctl `view` op. Writes
//! go through `write_typed` (they run on the engine's writer), include, exclude and undo through
//! `view_op_and_report` (with their consequences); reads use one `engine.read()` snapshot. A
//! replay with `from` or `from_file` is a local or dump port (D-G4PZX): its source is read and
//! closed before the writer runs, and `brief` is its target briefing.
use crate::answer::relay::RelayInput;
use crate::consequence::{op_effect, replay_effect, undo_effect, view_op_and_report};
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
    /// fork: the view to fork. replay: a local port's source (D-G4PZX), a project directory or
    /// a dump file.
    pub from: Option<String>,
    /// replay: a local port's source as a dump file (D-G4PZX).
    pub from_file: Option<String>,
    /// replay with a source: the target briefing token every ported change records.
    pub brief: Option<String>,
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
    /// include, exclude, undo and replay: cut the consequences to about this many tokens.
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
    /// The first field, in declaration order, that is set although replay does not take it.
    /// The source fields (`from`, `from_file`, `brief`) are replay's own.
    fn replay_forbidden(&self) -> Option<&'static str> {
        let set = [
            ("name", self.name.is_some()),
            ("base", self.base.is_some()),
            ("pin", self.pin.is_some()),
            ("views", self.views.is_some()),
            ("cascade", self.cascade.is_some()),
            ("a", self.a.is_some()),
            ("b", self.b.is_some()),
            ("all", self.all.is_some()),
            ("stale", self.stale.is_some()),
            ("reason", self.reason.is_some()),
            ("owner", self.owner.is_some()),
            ("none", self.none.is_some()),
            ("target", self.target.is_some()),
            ("block", self.block.is_some()),
            ("landed", self.landed.is_some()),
            ("evidence", self.evidence.is_some()),
            ("question", self.question.is_some()),
            ("session_ref", self.session_ref.is_some()),
            ("relayed_by", self.relayed_by.is_some()),
        ];
        set.into_iter().find(|(_, on)| *on).map(|(name, _)| name)
    }

    /// The first source field, in declaration order, that is set on op `op`, which is not
    /// replay. Fork's `from` is fork's own.
    fn source_field_on(&self, op: &str) -> Option<&'static str> {
        let set = [
            ("from", self.from.is_some() && op != "fork"),
            ("from_file", self.from_file.is_some()),
            ("brief", self.brief.is_some()),
        ];
        set.into_iter().find(|(_, on)| *on).map(|(name, _)| name)
    }

    /// The port source a replay names: `from` or `from_file`, never both. `brief` needs one.
    fn port_source(&self) -> Result<Option<&str>, RdyError> {
        match (self.from.as_deref(), self.from_file.as_deref()) {
            (Some(_), Some(_)) => Err(needs(
                "replay takes only one source",
                "pass from or from_file, not both",
            )),
            (Some(p), None) | (None, Some(p)) => Ok(Some(p)),
            (None, None) if self.brief.is_some() => Err(needs(
                "replay without a source does not take brief",
                "remove brief, or name a port source",
            )),
            (None, None) => Ok(None),
        }
    }

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

/// The view ops `run_view` knows.
const VIEW_OPS: [&str; 12] = [
    "create", "fork", "include", "exclude", "replay", "compare", "list", "show", "close", "own",
    "resolve", "undo",
];

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
    if req.op == "replay"
        && let Some(field) = req.replay_forbidden()
    {
        return Err(needs(
            &format!("replay does not take {field}"),
            &format!("drop {field}, or use the view operation that takes it"),
        ));
    }
    if VIEW_OPS.contains(&req.op.as_str())
        && req.op != "replay"
        && let Some(field) = req.source_field_on(&req.op)
    {
        return Err(needs(
            &format!("{} does not take {field}", req.op),
            "use the source field only with replay",
        ));
    }
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
        "replay" => {
            // an omitted or empty `changes` gets the engine's one refusal text
            let changes = req.changes.clone().unwrap_or_default();
            let view = view();
            let v = view.clone();
            match req.port_source()? {
                None => view_op_and_report(engine, &v, req.max_tokens, replay_effect, move |r| {
                    r.view_replay(&s, &view, &changes)
                }),
                // a local or dump port (D-G4PZX): the source is read and closed before the
                // target's writer; a relative path names one under the project
                Some(path) => {
                    let path = engine.root().join(path);
                    let source = crate::views::port::read_source(&path, &changes)?;
                    let brief = req.brief.clone();
                    view_op_and_report(engine, &v, req.max_tokens, replay_effect, move |r| {
                        r.view_port(&s, &view, &source, brief.as_deref())
                    })
                }
            }
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
            "use create, fork, include, exclude, replay, compare, list, show, close, own, resolve or undo",
        )),
    }
}
