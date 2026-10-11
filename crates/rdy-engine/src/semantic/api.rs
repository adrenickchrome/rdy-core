//! The review dispatchers MCP shares (rdy_review_next, rdy_review_submit) and the reviews paragraph of rdy_schema.
use crate::engine::write_typed;
use crate::semantic::candidates::request_label;
use crate::semantic::case::case_hash;
use crate::semantic::submit::ReviewSubmitRequest;
use crate::{E_BAD_INPUT, E_PERMISSION, Engine, RdyError, Session, now_ms, rfc3339};
use rdy_model::Role;
use serde::Deserialize;
use serde_json::{Value, json};

/// How many requests one `rdy_review_next` tries to claim before it answers that none is free.
const MAX_CLAIM_TRIES: usize = 8;

/// `rdy_review_next`'s input.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewNextRequest {
    /// The review kind: only `tension`.
    pub kind: String,
}

/// Leases the next review request session `s` may take to `owner` and returns its case:
/// `{request, kind, case_hash, case, lease_expires_at}` (`lease_expires_at` is `null` when
/// leases are off), or `{request: null, leased}` (the live leases) when none is free.
pub fn run_review_next(
    engine: &dyn Engine,
    s: &Session,
    owner: u64,
    req: ReviewNextRequest,
) -> Result<Value, RdyError> {
    if req.kind != "tension" {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!(
                "kind {:?} is not a review kind; the only kind is tension",
                req.kind
            ),
            "pass kind: tension",
        ));
    }
    if s.role != Role::Reviewer {
        return Err(RdyError::new(
            E_PERMISSION,
            "rdy_review_next needs the reviewer role",
            "start a session with role reviewer",
        ));
    }
    let st = engine.read();
    let now = now_ms();
    let leases = engine.leases();
    let mut leased = leases.leased(now);
    let ttl = st.lease_ttl_ms();
    for _ in 0..MAX_CLAIM_TRIES {
        let Some(q) = st.next_request(&s.actor, &leased)? else {
            break;
        };
        let case = st.case_of(&q.key, &q.items)?;
        let hash = case_hash(&case);
        let pair = q
            .items
            .iter()
            .map(|i| (i.block.clone(), i.content))
            .collect();
        if !leases.claim(q.key, pair, owner, now, ttl) {
            // another session took it first: the next one
            leased.insert(q.key);
            continue;
        }
        let expires = (ttl > 0).then(|| rfc3339(now.saturating_add(ttl)));
        return Ok(json!({
            "request": request_label(&q.key),
            "kind": "tension",
            "case_hash": hash.to_string(),
            "case": case,
            "lease_expires_at": expires,
        }));
    }
    Ok(json!({"request": null, "leased": leases.leased(now).len()}))
}

/// Records session `s`'s verdict (`Rdy::submit_verdict`) on the writer and returns its
/// `SubmitInfo`: `{request, review, verdict}`, with `tension`, `severity` and `advice` for a
/// `tension`, and `suppressed: true` for a `no_tension`.
pub fn run_review_submit(
    engine: &dyn Engine,
    s: &Session,
    req: ReviewSubmitRequest,
) -> Result<Value, RdyError> {
    let s = s.clone();
    write_typed(engine, move |r| r.submit_verdict(&s, req))
}
