//! The review op (D-JQD5X "Surfaces"): list the checks, run one, request an independent review,
//! or list requests and verdicts. Its input is the strict op union of
//! `rdy_engine::check::api::ReviewRequest`, the one `rdy_review` builds. A command line that
//! relays a person's request passes `relayed_by` on `request` only; the engine takes it
//! separately, so every other op refuses it as an unknown key. The queue ops
//! (`rdy_review_next`, `rdy_review_submit`) are tools only and have no ctl op.
use super::*;
use rdy_engine::check::api::{ReviewRequest, run_review};

pub(super) fn review(cx: &OpContext, args: Value) -> Result<Value, RdyError> {
    let mut args = if args.is_null() { json!({}) } else { args };
    let mut relayed_by = None;
    if args.get("op").and_then(Value::as_str) == Some("request")
        && let Some(v) = args.as_object_mut().and_then(|m| m.remove("relayed_by"))
    {
        relayed_by = match v {
            Value::Null => None,
            Value::String(by) => Some(by),
            _ => {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    "rdy_review: relayed_by must be a string",
                    "pass the relaying actor as a string, or drop relayed_by",
                ));
            }
        };
    }
    let req = ReviewRequest::from_value(args)?;
    run_review(cx.engine, cx.session, cx.human, false, relayed_by, req)
}
