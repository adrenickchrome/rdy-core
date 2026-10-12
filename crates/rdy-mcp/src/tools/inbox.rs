//! ib7: `rdy_inbox`. The request is run by `rdy_engine::inbox::api::run_inbox`, the dispatcher the
//! daemon's `inbox` op shares; `rdy_schema`'s paragraph is `INBOX_DOC`.
use super::*;
use rdy_engine::inbox::api::{InboxRequest, run_inbox};
use rdy_engine::*;
use rdy_model::Role;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

/// The MCP-only clause `rdy_schema` adds after the engine's `INBOX_DOC`: the daemon's request has
/// no `person` field, so the engine's paragraph cannot name it.
pub const INBOX_PERSON_DOC: &str = "rdy_inbox {person: true} reads the person's inbox (the human actor) so you can show it in chat; ack only when they say they have seen it.";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxParams {
    /// A cursor from an earlier rdy_inbox reply; default: the saved cursor.
    pub since: Option<String>,
    /// true moves the saved cursor past the entries returned.
    pub ack: Option<bool>,
    /// true reads the person's inbox (the human actor, run cli) so you can show it in chat; ack only when they say they have seen it.
    pub person: Option<bool>,
    /// The reply's token budget (default 4000).
    pub max_tokens: Option<usize>,
}

/// `rdy_inbox`: the events that concern this session (or, with `person: true`, the person) since
/// the saved cursor. A session is required either way.
pub fn rdy_inbox(st: &mut McpState, p: InboxParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    let own = session(st)?;
    let req = InboxRequest {
        since: p.since,
        ack: p.ack.unwrap_or(false),
        max_tokens: p.max_tokens,
    };
    if p.person == Some(true) {
        let person = human_actor_for_env(&st.engine.root());
        let target = Session::human(person.clone(), Role::Writer);
        let mut v = run_inbox(&*st.engine, &target, req)?;
        v["person"] = json!(person);
        Ok(v)
    } else {
        run_inbox(&*st.engine, &own, req)
    }
}
