//! acc1: `rdy_accept`, the bulk way to take the suggestions a write or search response lists
//! (FW-EN3). The accepting itself is `Rdy::accept`.
use crate::tools::{McpState, check_tokens, report_once, session};
use rdy_engine::accept::AcceptItem;
use rdy_engine::consequence::write_and_report;
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AcceptParams {
    /// Suggestions to accept, unchanged from a write or search response (each an object with kind "link" or "entity"). An entity suggestion may add name, type (default entity), entity_kind, value, unit and parent.
    pub suggestions: Vec<Value>,
    /// Where new entities go (a doc or section: id, key or path), unless a suggestion names its own.
    pub parent: Option<String>,
    /// The view to write (default: your session's).
    pub view: Option<String>,
    /// The briefing token, for stable content.
    pub brief: Option<String>,
    /// The budget for the consequences.
    pub max_tokens: Option<usize>,
    /// The change message.
    pub message: Option<String>,
}

pub fn rdy_accept(st: &mut McpState, p: AcceptParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    let s = session(st)?;
    let items: Vec<AcceptItem> = p
        .suggestions
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            serde_json::from_value(v).map_err(|e| {
                RdyError::new(
                    E_BAD_INPUT,
                    format!("suggestion {i}: {e}"),
                    "pass the suggestion objects unchanged from a response's suggestions",
                )
            })
        })
        .collect::<Result<_, _>>()?;
    let view = p.view.unwrap_or_else(|| st.view.clone());
    let (parent, brief, message, max_tokens) = (p.parent, p.brief, p.message, p.max_tokens);
    let v = view.clone();
    write_and_report(&*st.engine, &v, max_tokens, move |r| {
        r.accept(&s, &view, items, parent.as_deref(), brief, message)
    })
    .map(|v| report_once(st, v))
}
