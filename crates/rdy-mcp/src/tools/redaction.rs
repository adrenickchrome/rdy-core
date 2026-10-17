//! rdy_redact: the redaction dry run and request (plan Redaction; D-6B1W1: a report "Before a
//! redaction is proposed"; D-KQ4CA: the chat path first). The engine computes the report and
//! records the request; this tool parses its arguments, chooses the view and replies with the
//! complete result plus the shared text (`report_text`). It approves nothing: a person approves
//! the proposal that carries the request.

use super::*;
use rdy_engine::consequence::write_and_report;
use rdy_engine::redaction::presentation::{DEFAULT_TOKENS, check_budget, check_recorded_fits};
use rdy_engine::redaction::{RedactInput, RedactResult, check_recorder, dry_run, report_text};
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::sync::{Arc, Mutex};

/// The name an unknown key of a redaction request is reported under, on every surface.
pub const REDACT_CONTEXT: &str = "redact";

/// rdy_redact's arguments, strictly: the keys in the order the interface lists them, which is
/// the order an unknown key's fix names them.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RedactArgs {
    /// Selectors: a block id (every stored version), id@<64-hex version hash> (one version), or blob:<64-hex manifest hash>. 1 to 256, each at most 256 bytes.
    pub targets: Vec<String>,
    /// Why the content goes: secret, personal, legal or retraction.
    #[schemars(extend("enum" = ["secret", "personal", "legal", "retraction"]))]
    pub reason_class: String,
    /// The reason in plain words, 1 to 4096 UTF-8 bytes. It is kept as audit text.
    pub reason: String,
    /// true reports the blast radius and records nothing (any role may run it). Default false: record the request in the view.
    #[schemars(extend("default" = false))]
    pub dry_run: Option<bool>,
    /// The view the request is recorded in (default: your session's). It changes where the request lives, not what the report scans.
    pub view: Option<String>,
    /// The text report's budget, 256 to 20000 (default 4000). A smaller budget omits the row list, never a count or a warning; the structured report stays complete.
    #[schemars(range(min = 256, max = 20000), extend("default" = 4000))]
    pub max_tokens: Option<usize>,
}

/// rdy_redact's arguments as sent. Its input schema is [`RedactArgs`]'s; [`parse_redact`] reads
/// it, so an unknown key is refused with the did-you-mean the daemon's `redact` op gives.
#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct RedactParams(pub Map<String, Value>);

impl JsonSchema for RedactParams {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        RedactArgs::schema_name()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        RedactArgs::json_schema(generator)
    }
}

/// Parses rdy_redact's arguments. The first unknown key is `E_BAD_INPUT` from the shared
/// did-you-mean helper with context `redact` and the interface's key order (the daemon op's own
/// text); any other fault is the tool parameter refusal every MCP tool gives.
pub fn parse_redact(args: Map<String, Value>) -> Result<RedactArgs, RdyError> {
    if let Some(key) = args.keys().find(|k| !is_redact_key(k)) {
        // serde names an unknown key before it reads its value, whatever the value is
        let probe = json!({ key.as_str(): null });
        if let Err(e) = RedactArgs::deserialize(&probe)
            && let Some(err) = rdy_engine::did_you_mean::unknown_field_error_in(
                REDACT_CONTEXT,
                &e.to_string(),
                &Value::Object(args.clone()),
            )
        {
            return Err(err);
        }
    }
    crate::params::parse_params::<RedactArgs>("rdy_redact", args)
}

/// Whether `key` is one of [`RedactArgs`]'s keys.
fn is_redact_key(key: &str) -> bool {
    let probe = json!({ key: null });
    match RedactArgs::deserialize(&probe) {
        Ok(_) => true,
        Err(e) => !e.to_string().starts_with("unknown field"),
    }
}

/// `v` with the shared text of `result` within `max_tokens`, and `truncated: true` when its row
/// list was omitted.
fn with_text(mut v: Value, result: &RedactResult, max_tokens: usize) -> Result<Value, RdyError> {
    let t = report_text(result, max_tokens)?;
    v["text"] = json!(t.text);
    if t.truncated {
        v["truncated"] = json!(true);
    }
    Ok(v)
}

pub fn rdy_redact(st: &mut McpState, p: RedactParams) -> Result<Value, RdyError> {
    let a = parse_redact(p.0)?;
    let s = session(st)?;
    let max_tokens = a.max_tokens.unwrap_or(DEFAULT_TOKENS);
    let input = RedactInput {
        targets: a.targets,
        reason_class: a.reason_class,
        reason: a.reason,
        dry_run: a.dry_run.unwrap_or(false),
    };
    if input.dry_run {
        // any role: the report reads one published state and records nothing
        check_budget(max_tokens)?;
        let result = dry_run(&st.engine.read(), &input)?;
        let v = serde_json::to_value(&result).map_err(|e| {
            RdyError::new(
                E_INTERNAL,
                format!("a redaction result did not serialize: {e}"),
                "report this as a bug in rdy",
            )
        })?;
        return with_text(v, &result, max_tokens);
    }
    // the view as every write tool names it: `view:<name>` or `<name>`, else the session's
    let view = a
        .view
        .map(|v| v.strip_prefix("view:").map(String::from).unwrap_or(v))
        .unwrap_or_else(|| st.view.clone());
    let slot: Arc<Mutex<Option<RedactResult>>> = Arc::default();
    let inner = Arc::clone(&slot);
    let v = view.clone();
    let out = write_and_report(&*st.engine, &view, None, move |r| {
        // the permission and view ownership first, then the budget: its range, and the text of
        // the report about to be recorded, so a budget refusal never follows a write
        check_recorder(&r.read(), &s, &v)?;
        check_budget(max_tokens)?;
        let result = r.redact_checked(&s, &v, input, &mut |probe| {
            check_recorded_fits(probe, max_tokens)
        })?;
        *inner.lock().unwrap_or_else(|p| p.into_inner()) = Some(result.clone());
        Ok(result)
    })?;
    let result = slot
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
        .ok_or_else(|| {
            RdyError::new(
                E_INTERNAL,
                "a redaction request wrote no result",
                "report this as a bug in rdy",
            )
        })?;
    with_text(out, &result, max_tokens)
}
