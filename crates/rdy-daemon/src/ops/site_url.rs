//! The `site_url` op: the link to the live site, the twin of `rdy_site` (D-N5NSN, D-FV6G9).
use super::*;
use rdy_engine::RdyError;
use rdy_engine::engine::site_reply;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SiteUrlArgs {
    scope: Option<String>,
}

pub(super) fn site_url(cx: &OpContext, a: SiteUrlArgs) -> Result<Value, RdyError> {
    site_reply(cx.engine, a.scope.as_deref())
}
