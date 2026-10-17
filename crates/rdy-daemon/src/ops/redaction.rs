//! The redact op: a redaction dry run or request (plan Redaction). A dry run reads one snapshot
//! with any role; a request runs `Rdy::redact_checked` on the writer, which computes the report
//! once, and replies through the mutation envelope. Both replies carry the shared text
//! (`report_text`) and, when its rows were omitted, `truncated: true`.
use super::*;
use rdy_engine::consequence::write_and_report;
use rdy_engine::redaction::presentation::{DEFAULT_TOKENS, check_budget, check_recorded_fits};
use rdy_engine::redaction::{RedactInput, RedactResult, check_recorder, dry_run, report_text};
use std::sync::{Arc, Mutex};

/// The redact op's args, in the order an unknown key's fix lists them.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RedactArgs {
    targets: Vec<String>,
    reason_class: String,
    reason: String,
    dry_run: Option<bool>,
    view: Option<String>,
    max_tokens: Option<usize>,
}

/// `v` with the text of `result` within `max_tokens`, and `truncated: true` when its rows were
/// omitted.
fn with_text(mut v: Value, result: &RedactResult, max_tokens: usize) -> Result<Value, RdyError> {
    let t = report_text(result, max_tokens)?;
    v["text"] = json!(t.text);
    if t.truncated {
        v["truncated"] = json!(true);
    }
    Ok(v)
}

pub(super) fn redact(cx: &OpContext, a: RedactArgs) -> Result<Value, RdyError> {
    let max_tokens = a.max_tokens.unwrap_or(DEFAULT_TOKENS);
    let input = RedactInput {
        targets: a.targets,
        reason_class: a.reason_class,
        reason: a.reason,
        dry_run: a.dry_run.unwrap_or(false),
    };
    if input.dry_run {
        check_budget(max_tokens)?;
        let result = dry_run(&cx.engine.read(), &input)?;
        return with_text(to_json(&result), &result, max_tokens);
    }
    let s = cx.session.clone();
    let view = view_of(&s, a.view);
    let slot: Arc<Mutex<Option<RedactResult>>> = Arc::default();
    let inner = Arc::clone(&slot);
    let v = view.clone();
    let out = write_and_report(cx.engine, &view, None, move |r| {
        // the permission first, then the budget: its range, and the text of the report about to
        // be recorded, so a budget refusal never follows a write; the report is computed once
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
        .expect("a successful redact write stores its result");
    with_text(out, &result, max_tokens)
}
