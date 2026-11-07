//! rdy_rule (FW-MH.rules, D-ZSRBN): one rule of the person's, or of this project, added, edited or
//! removed from the person's words, or the answer to a card this tool returned. The engine checks
//! the request ([`rdy_engine::rule_types::parse_request`]), routes it, and runs it through the
//! ordinary gate; this file only publishes the closed input schema and passes the arguments on.
//! The project server and the projectless server share both.

use super::*;
use rdy_engine::rule_types::{RuleRequest, parse_request};
use rdy_engine::rules::RuleOrigin;
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::sync::Arc;

/// The tool's description, on every server that lists it.
pub const RULE_DOC: &str = "Change one rule from the person's own words, in one call: add, edit or remove an instruction (op), or answer a card this tool returned (answer). scope person writes to the person's meta project, from any project or none; scope project writes to this project. Pass the person's words as evidence and never your own; ask the person first. A subagent never relays. The reply is the landed rule, or a card for the person (state card) or a list of issues (state not_ready). Show a card to the person as it is, then call again with answer: ref, option, card_hash and the person's words as evidence. Content that is stable is refused: use the normal write, propose and answer path. Only a writer or reviewer session can call it.";

/// rdy_rule's input, as the schema shows it: one closed object with four forms. Add needs scope,
/// op, text, why and evidence. Edit also needs id. Remove needs scope, op, id, why and evidence.
/// Answer needs scope and answer alone.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuleArgs {
    /// Where the rule lives: person (the person's meta project) or project (this project). No default.
    #[schemars(extend("enum" = ["person", "project"]))]
    pub scope: String,
    /// The change: add, edit or remove. Leave it out only to answer a card.
    #[schemars(extend("enum" = ["add", "edit", "remove"]))]
    pub op: Option<String>,
    /// The rule in full, at most 65536 bytes (add and edit). It is stored as given.
    pub text: Option<String>,
    /// The id of the instruction to change (edit and remove), at most 96 bytes.
    #[schemars(length(max = 96))]
    pub id: Option<String>,
    /// The ruleset that holds the rule: a doc id or path in the selected scope, or its person: or project: handle (add and edit). Default: the scope's default ruleset.
    #[schemars(length(max = 1024))]
    pub ruleset: Option<String>,
    /// When the rule applies, 1 to 1024 selectors of at most 256 bytes each (add and edit). Default for add: all.
    #[schemars(length(min = 1, max = 1024))]
    pub applies_to: Option<Vec<String>>,
    /// Why, in one line of at most 2000 characters. It is stored as the reason of the rule's decision.
    #[schemars(length(max = 2000))]
    pub why: Option<String>,
    /// The person's own words that ask for this change, at most 2000 characters. Required for add, edit and remove.
    #[schemars(length(max = 2000))]
    pub evidence: Option<String>,
    /// The question you asked the person, at most 2000 characters.
    #[schemars(length(max = 2000))]
    pub question: Option<String>,
    /// A short id for the conversation, at most 256 characters. A repeat of the same request in the same session reads the same receipt; use a new one for a new request.
    #[schemars(length(max = 256))]
    pub session_ref: Option<String>,
    /// The person's answer to a card this tool returned. Pass it alone with scope.
    pub answer: Option<RuleAnswerArgs>,
}

/// The answer to a card of this tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuleAnswerArgs {
    /// The item the card named: its ref, at most 128 bytes.
    #[serde(rename = "ref")]
    #[schemars(length(max = 128))]
    pub item_ref: String,
    /// An option id of that card, at most 144 bytes.
    #[schemars(length(max = 144))]
    pub option: String,
    /// The card_hash of the card the person saw: 64 lowercase hexadecimal characters.
    #[schemars(length(min = 64, max = 64), regex(pattern = "^[0-9a-f]{64}$"))]
    pub card_hash: String,
    /// The person's own words, at most 2000 characters.
    #[schemars(length(max = 2000))]
    pub evidence: String,
    /// The question you asked the person, at most 2000 characters.
    #[schemars(length(max = 2000), extend("type" = "string"))]
    pub question: Option<String>,
}

/// rdy_rule's arguments as sent. The input schema is [`RuleArgs`]'s; [`checked_request`] reads
/// them, so the engine's one parser gives every refusal, on the server and in the daemon alike.
#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct RuleParams(pub Map<String, Value>);

impl JsonSchema for RuleParams {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        RuleArgs::schema_name()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = RuleArgs::json_schema(generator);
        // the parser refuses a null for every key, so the schema never offers one
        if let Some(root) = schema.as_object_mut() {
            close_nulls(root);
        }
        schema
    }
}

/// Removes `null` from the type of every property of `schema`. The answer is a definition that the
/// generator adds after this runs, so its one optional key says its type itself.
fn close_nulls(schema: &mut Map<String, Value>) {
    if let Some(Value::Object(props)) = schema.get_mut("properties") {
        for prop in props.values_mut().filter_map(Value::as_object_mut) {
            no_null(prop);
        }
    }
}

/// `prop` without a null: a type list loses its `null`, and an `anyOf` of one reference and `null`
/// becomes that reference, with the property's own description kept.
fn no_null(prop: &mut Map<String, Value>) {
    if let Some(Value::Array(types)) = prop.get_mut("type") {
        types.retain(|t| t != "null");
        if types.len() == 1 {
            let only = types.remove(0);
            prop.insert("type".into(), only);
        }
    }
    if let Some(Value::Array(any)) = prop.get_mut("anyOf") {
        any.retain(|b| b.get("type") != Some(&Value::String("null".into())));
        if any.len() == 1
            && let Value::Object(only) = any.remove(0)
        {
            prop.remove("anyOf");
            prop.extend(only);
        }
    }
}

/// The checked request in `p`, before any session, writer or host is touched.
pub fn checked_request(p: RuleParams) -> Result<RuleRequest, RdyError> {
    parse_request(&Value::Object(p.0))
}

/// `rdy_rule` on the project server. The project is this server's; a person rule goes to the meta
/// project through the host, on this call's own signer.
pub fn rdy_rule(st: &mut McpState, p: RuleParams) -> Result<Value, RdyError> {
    let request = checked_request(p)?;
    let s = session(st)?;
    let host = st.engine.person_host();
    let origin = RuleOrigin {
        session: &s,
        relayed_by: None,
    };
    rdy_engine::rules::route_rule(
        Some(&*st.engine),
        host.as_deref(),
        &origin,
        &request,
        Arc::clone(&st.agent),
    )
}

/// `rdy_rule` outside any project: the person's rule, through `host`. The caller checked the
/// session; nothing here holds a lock.
pub(crate) fn projectless_rule(
    host: &dyn rdy_engine::instructions::PersonHost,
    session: &Session,
    request: &RuleRequest,
    agent: Arc<dyn rdy_engine::signing_crypto::SigningAgent>,
) -> Result<Value, RdyError> {
    let origin = RuleOrigin {
        session,
        relayed_by: None,
    };
    rdy_engine::rules::route_rule(None, Some(host), &origin, request, agent)
}
