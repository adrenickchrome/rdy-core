//! rdy_tag: the person's tag, relayed from chat (D-KQ4CA, D-DGCH7). The tag records the person;
//! an `ANSWER` in the same batch holds the agent's relay of their words.

use super::*;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::engine::write_typed;
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TagParams {
    /// The new version, e.g. "v1.2.0": above every version, on the current version's landing or a later one.
    pub version: String,
    /// The landing to name (a hash or a prefix of 6 or more); default: canon's head.
    pub landing: Option<String>,
    /// The version's message.
    pub message: Option<String>,
    /// The person's words that ask for the tag, quoted (1 to 2000 characters).
    pub evidence: String,
    /// The question you asked the person.
    pub question: Option<String>,
    /// The chat session the person spoke in (default: mcp).
    pub session_ref: Option<String>,
}

/// `rdy_tag`: the person's tag, relayed by this agent session (the relay table, case 4).
pub fn rdy_tag(st: &mut McpState, p: TagParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let person = human_actor_for_env(&st.engine.root());
    let relay = RelayInput {
        relayed_by: None,
        evidence: Some(p.evidence),
        question: p.question,
        session_ref: p.session_ref,
        kind: None,
    };
    write_typed(&*st.engine, move |r| {
        r.tag_as(
            &s,
            &person,
            &relay,
            true,
            &p.version,
            p.landing.as_deref(),
            p.message,
        )
    })
}
