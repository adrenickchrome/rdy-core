//! The policy op (D-ZWP7Q, O20): `init` starts the authority policy. The request runs through
//! `rdy_engine::policy::init::run_policy`, the runner `rdy_policy` shares.
use super::*;
use rdy_engine::policy::init::{PolicyRequest, run_policy};

pub(super) fn policy(cx: &OpContext, req: PolicyRequest) -> Result<Value, RdyError> {
    run_policy(cx.engine, cx.session, req)
}
