//! se9: `rdy_review_next` and `rdy_review_submit`. The requests are run by
//! `rdy_engine::semantic::api::{run_review_next, run_review_submit}`, the dispatchers the
//! daemon's review ops share; `rdy_schema`'s paragraph is `REVIEWS_DOC`.
use super::*;
use rdy_engine::semantic::api::{ReviewNextRequest, run_review_next, run_review_submit};
use rdy_engine::semantic::submit::{CitationRequest, ReviewSubmitRequest};
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewNextParams {
    /// The review kind: tension (the only kind until M3).
    pub kind: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewSubmitParams {
    /// The request from rdy_review_next: rq-<12 hex>.
    pub request: String,
    /// case_hash from rdy_review_next: the case you read.
    pub case_hash: String,
    /// tension or no_tension.
    pub verdict: String,
    /// What disagrees: {block, note} per block that takes part; a tension needs 1 to 20.
    pub citations: Option<Vec<CitationParams>>,
    /// advisory (default) or blocking: on a tension, advise a person to make it block. Advice gates nothing until a person accepts it.
    pub severity: Option<String>,
    /// With severity blocking: why the tension should block, 1 to 2000 characters.
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CitationParams {
    /// One of the two block ids of the case.
    pub block: String,
    /// What in the block disagrees, 1 to 2000 characters.
    pub note: String,
}

/// `rdy_review_next`: leases the next review request to this session and returns its case.
pub fn rdy_review_next(st: &mut McpState, p: ReviewNextParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    run_review_next(
        &*st.engine,
        &s,
        st.lease_owner,
        ReviewNextRequest { kind: p.kind },
    )
}

/// `rdy_review_submit`: records this session's verdict on a request.
pub fn rdy_review_submit(st: &mut McpState, p: ReviewSubmitParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    run_review_submit(
        &*st.engine,
        &s,
        ReviewSubmitRequest {
            request: p.request,
            case_hash: p.case_hash,
            verdict: p.verdict,
            citations: p
                .citations
                .unwrap_or_default()
                .into_iter()
                .map(|f| CitationRequest {
                    block: f.block,
                    note: f.note,
                })
                .collect(),
            severity: p.severity,
            reason: p.reason,
        },
    )
}
