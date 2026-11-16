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
    /// The file of a portable baseline, in place of `against`.
    against_baseline: Option<String>,
    release: Option<String>,
}

pub(super) fn publication_check(cx: &OpContext, a: CheckArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let file = a.against_baseline.as_ref().map(|f| resolve_path(cx.cwd, f));
    rdy_engine::publication::check_with_inputs(
        &st,
        &a.profile,
        a.scope.as_deref(),
        a.against.as_deref(),
        file.as_deref(),
        a.release.as_deref(),
        0,
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BaselineArgs {
    profile: String,
    scope: String,
    out: Option<String>,
}

/// The `publication_baseline` op: the portable baseline of a profile at a tag, the twin of
/// `rdy_publication_baseline`. A read op that writes at most the new file `out`.
pub(super) fn publication_baseline(cx: &OpContext, a: BaselineArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let out = a.out.as_ref().map(|o| resolve_path(cx.cwd, o));
    rdy_engine::publication::baseline_reply(&st, &a.profile, Some(&a.scope), out.as_deref())
}
