//! The publication ops: `publication_check`, the reference check and disclosure comparison of a
//! publication profile (D-0VCQ5, D-KDFZ3). A read op: one snapshot, every session role, no
//! write and no authorization. It calls the same engine function `rdy_publication_check` does.
use super::*;
use rdy_engine::RdyError;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckArgs {
    profile: String,
    scope: Option<String>,
    against: Option<String>,
}

pub(super) fn publication_check(cx: &OpContext, a: CheckArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    rdy_engine::publication::check(&st, &a.profile, a.scope.as_deref(), a.against.as_deref())
}
