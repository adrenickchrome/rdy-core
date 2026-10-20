//! `rdy_publish` (D-Q0AVZ, D-KQ4CA, D-50JXR; hub i-6ZXW0, i-F19YZ, r-K71GM): one tag's
//! publication uploaded to a publish remote's hub as a candidate, through `Engine::publish`
//! (this crate has no edge to the renderer). The daemon runs the one publish coordinator; an
//! engine without a daemon refuses with its own fallback text.
//!
//! - **Closed input.** The tool takes exactly `{tag, to, site, keep_latest}`. No caller names a
//!   profile or an audience: the site selects the profile, and the audience is the tagged
//!   profile's own. `keep_latest` reaches the hub unchanged beside the record; absent and null
//!   are both "not given".
//! - **Any session.** A session is required and every role may publish, as for the hub key
//!   commands: the hub credential, not the session role, grants upload authority.
//! - **The result unchanged.** The reply is the coordinator's publish result. It says
//!   `state: candidate` and `review: hub`: the hub's own review is authoritative, and nothing
//!   here opens, approves or authorizes the candidate.
use super::*;
use rdy_engine::RdyError;
use rdy_engine::hub_publish::PublishRequest;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PublishParams {
    /// The tag to publish: a version such as v1.2.0.
    pub tag: String,
    /// The publish remote to upload to, by its name.
    pub to: String,
    /// The hub site that receives the candidate. The site selects the publication profile.
    pub site: String,
    /// Optional: true or false. rdy sends it to the hub beside the record. Omit it to use the hub's default.
    pub keep_latest: Option<bool>,
}

/// `rdy_publish`: the publish result of the coordinator, for the session's own claim. Any session
/// may call it.
pub fn rdy_publish(st: &mut McpState, p: PublishParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let PublishParams {
        tag,
        to,
        site,
        keep_latest,
    } = p;
    st.engine.publish(
        PublishRequest {
            tag,
            to,
            site,
            keep_latest,
        },
        &s,
    )
}
