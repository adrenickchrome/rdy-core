//! se9: `rdy_review_next` and `rdy_review_submit`; ck8: `rdy_review`. The requests are run by
//! `rdy_engine::semantic::api::{run_review_next, run_review_submit}` and
//! `rdy_engine::check::api::run_review`, the dispatchers the daemon's review op shares;
//! `rdy_schema`'s paragraph is `REVIEWS_DOC`.
use super::*;
use rdy_engine::check::api::{ReviewRequest, run_review};
use rdy_engine::semantic::api::{ReviewNextRequest, run_review_next, run_review_submit};
use rdy_engine::semantic::submit::{CitationRequest, ReviewSubmitRequest};
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// The `subject` of `rdy_review`: one block, change or P-x@n; two block ids; or {decision, code}.
fn subject_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "anyOf": [
            {"type": "string"},
            {"type": "array", "items": {"type": "string"}, "minItems": 2, "maxItems": 2},
            {
                "type": "object",
                "properties": {"decision": {"type": "string"}, "code": {"type": "string"}},
                "required": ["decision", "code"],
                "additionalProperties": false
            },
            {"type": "null"}
        ]
    })
}

/// `rdy_review`: one flat input; `op` picks the engine's request and the other keys belong to it.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewParams {
    /// checks, run, request or list. checks lists the catalogue; run writes an author-run request and returns its case; request asks for an independent review; list shows requests and verdicts.
    pub op: String,
    /// run and request: the check's name, as checks lists it.
    pub check: Option<String>,
    /// run and request need it; list filters by it. One block id, key or path, a change hash or P-x@n; a pair of two block ids; or {decision, code} for diff_contradicts, with code <repo>@<from>..<to>.
    #[serde(default)]
    #[schemars(schema_with = "subject_schema")]
    pub subject: Option<Value>,
    /// request: why the review is wanted, 1 to 2000 characters.
    pub reason: Option<String>,
    /// request: the person's words, quoted. Pass them when you request a review for a person.
    pub evidence: Option<String>,
    /// request: the question the person answered.
    pub question: Option<String>,
    /// request: the chat session the person spoke in (default mcp).
    pub session_ref: Option<String>,
    /// list: only the open requests (default false).
    pub open: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewNextParams {
    /// The review kind: tension, a check's name (rdy_review {op: "checks"} lists them), or any.
    pub kind: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewSubmitParams {
    /// The request from rdy_review_next: rq-<12 hex>.
    pub request: String,
    /// case_hash from rdy_review_next: the case you read.
    pub case_hash: String,
    /// tension or no_tension for a contradiction; pass, concern or fail for another check.
    pub verdict: String,
    /// What disagrees: {block, note} per block that takes part; a tension, a concern and a fail need 1 to 20.
    pub citations: Option<Vec<CitationParams>>,
    /// advisory (default) or blocking: on a tension, advise a person to make it block. Advice gates nothing until a person accepts it.
    pub severity: Option<String>,
    /// With severity blocking: why the tension should block. A concern or fail: what the check found. 1 to 2000 characters.
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CitationParams {
    /// One of the block ids of the case.
    pub block: String,
    /// What in the block disagrees, 1 to 2000 characters.
    pub note: String,
}

/// `rdy_review`: lists the checks, runs one on the session's own work, requests an independent
/// review, or lists requests and verdicts. The flat input becomes the engine's op-tagged
/// request, which refuses the keys of the other ops; the session is the relay of a person's
/// request (`implicit_relay`), so the tool has no `relayed_by`.
pub fn rdy_review(st: &mut McpState, p: ReviewParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let person = human_actor_for_env(&st.engine.root())?;
    let mut args = serde_json::Map::new();
    args.insert("op".into(), Value::String(p.op));
    let given = [
        ("check", p.check.map(Value::String)),
        ("subject", p.subject),
        ("reason", p.reason.map(Value::String)),
        ("evidence", p.evidence.map(Value::String)),
        ("question", p.question.map(Value::String)),
        ("session_ref", p.session_ref.map(Value::String)),
        ("open", p.open.map(Value::Bool)),
    ];
    for (key, value) in given {
        // an omitted key and a null are the same; the engine refuses a key its op does not take
        if let Some(v) = value.filter(|v| !v.is_null()) {
            args.insert(key.into(), v);
        }
    }
    let req = ReviewRequest::from_value(Value::Object(args))?;
    run_review(&*st.engine, &s, &person, true, None, req)
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
