//! rdy_instructions (ain4, D-8X04F, D-YQPB6, D-A8N0E): the required instructions in full, the
//! rulesets a call loads, and the menu of every ruleset, from the project and the person layer
//! in one reply. The reminder that overview and briefings carry until such a reply goes out.

use super::*;
use rdy_engine::instructions::{self, Caller, LoadRequest, REMINDER, REMINDER_KEY, Reply};
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstructionsParams {
    /// Set true to get every built-in ruleset in full. Your project's and person layer's required rules arrive either way; pass load for their optional rulesets. Default false.
    #[serde(default)]
    pub full: bool,
    /// Rulesets to load, by their menu ids (project:<doc-id> or person:<doc-id>); a bare doc id loads when only one layer holds it. Required rules arrive without asking.
    pub load: Option<Vec<String>>,
}

/// The tokens the reminder takes in a reply, with its key.
pub(crate) const REMINDER_TOKENS: usize =
    (REMINDER.len() + REMINDER_KEY.len() + r#","":"""#.len()).div_ceil(4);

/// Whether this call's reply owes the reminder: no complete instructions reply went out for the
/// session's actor and run while its settings live.
pub(crate) fn reminder_due(st: &McpState, s: &Session) -> bool {
    !st.hub
        .claims()
        .instructions_read(&(s.actor.clone(), s.run.clone()))
}

/// Puts the reminder on an object reply; it goes last.
pub(crate) fn add_reminder(out: &mut Value) {
    if let Some(m) = out.as_object_mut() {
        m.insert(REMINDER_KEY.into(), Value::String(REMINDER.into()));
    }
}

/// The JSON of a bounded reply: counted before it is written ([`Reply::to_json`]).
pub(crate) fn reply_value(reply: &Reply) -> Result<Value, RdyError> {
    let bytes = reply.to_json()?;
    serde_json::from_slice(&bytes).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("an instructions reply did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })
}

/// One instructions call as `caller`: the load ids are checked first, then the project's
/// captured state and the person layer are read and merged ([`instructions::deliver`]). `None`
/// for `project` is a call outside any project.
pub(crate) fn instructions_reply(
    project: Option<(&ReadState, &std::path::Path)>,
    host: Option<&dyn instructions::PersonHost>,
    caller: &Caller,
    load: Option<Vec<String>>,
    full: bool,
) -> Result<Value, RdyError> {
    let load = LoadRequest::parse(&load.unwrap_or_default())?.with_full(full);
    let reply = instructions::deliver(project, &load, caller, host)?;
    reply_value(&reply)
}

pub fn rdy_instructions(st: &mut McpState, p: InstructionsParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let caller = Caller {
        actor: s.actor.clone(),
        run: s.run.clone(),
    };
    let state = st.engine.read();
    let root = st.engine.root();
    let host = st.engine.person_host();
    let out = instructions_reply(
        Some((&state, &root)),
        host.as_deref(),
        &caller,
        p.load,
        p.full,
    )?;
    // only a complete reply clears the reminder and marks what it holds; a refusal leaves both
    crate::kungfu::note_delivered(st.hub.claims(), &(s.actor, s.run), &out);
    Ok(out)
}
