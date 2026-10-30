//! The dispute op (D-ZWP7Q "Disputes"): open, resolve and list disputes of landings. Open and
//! resolve are a person's acts: an agent session relays the person's words with `relayed_by`
//! (its own actor) and `evidence`, as `rdy view own` does. The request runs through
//! `rdy_engine::dispute::run_dispute`, the runner `rdy_dispute` shares.
use super::*;
use rdy_engine::dispute::{DisputeRequest, run_dispute};

pub(super) fn dispute(cx: &OpContext, req: DisputeRequest) -> Result<Value, RdyError> {
    run_dispute(cx.engine, cx.session, cx.human, req)
}
