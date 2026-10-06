//! vapi1: the one view request dispatcher behind MCP's `rdy_view` and the ctl `view` op. Writes
//! go through `write_typed` (they run on the engine's writer); reads use one `engine.read()`
//! snapshot.
use crate::engine::{Engine, write_typed};
use crate::error::*;
use crate::{Session, now_ms};
use serde::Deserialize;
use serde_json::{Value, json};

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
    pub owner: Option<String>,
    pub none: Option<bool>,
    pub target: Option<String>,
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
/// `view` (or `name`, for close, own and show), else `default_view`.
pub fn run_view(
    engine: &dyn Engine,
    s: &Session,
    default_view: &str,
    req: ViewRequest,
) -> Result<Value, RdyError> {
    let s = s.clone();
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
            write_typed(engine, move |r| r.view_include(&s, &view, &changes))
        }
        "exclude" => {
            let changes = req.changes.clone().ok_or_else(|| {
                needs(
                    "exclude needs changes",
                    "pass changes: the change ids to exclude",
                )
            })?;
            let (view, cascade) = (view(), req.cascade.unwrap_or(false));
            write_typed(engine, move |r| {
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
            let closed = match (names, req.stale) {
                (None, Some(days)) => {
                    write_typed(engine, move |r| r.view_close_stale(&s, days, now_ms()))?
                }
                (Some(names), _) => {
                    let reason = req.reason.clone().unwrap_or_default();
                    write_typed(engine, move |r| r.view_close(&s, &names, &reason))?
                }
                (None, None) => {
                    return Err(needs(
                        "close needs a view, views or stale",
                        "pass view, views, or stale: <days> to close the idle views",
                    ));
                }
            };
            Ok(json!({ "closed": closed }))
        }
        "own" => {
            let view = named()
                .ok_or_else(|| needs("own needs a view", "pass view: the view to reassign"))?;
            let owner = match (req.owner.clone(), req.none.unwrap_or(false)) {
                (Some(_), true) => {
                    return Err(needs(
                        "own takes an owner or none, not both",
                        "pass owner: <actor>[/<run>], or none: true to leave it unowned",
                    ));
                }
                (None, false) => {
                    return Err(needs(
                        "own needs an owner or none",
                        "pass owner: <actor>[/<run>], or none: true to leave it unowned",
                    ));
                }
                (owner, _) => owner,
            };
            write_typed(engine, move |r| r.view_own(&s, &view, owner.as_deref()))
        }
        "undo" => {
            let (view, target, cascade) =
                (view(), req.target.clone(), req.cascade.unwrap_or(false));
            write_typed(engine, move |r| {
                r.undo(&s, &view, target.as_deref(), cascade)
            })
        }
        op => Err(RdyError::new(
            E_BAD_INPUT,
            format!("unknown view op {op:?}"),
            "use create, fork, include, exclude, compare, list, show, close, own or undo",
        )),
    }
}
