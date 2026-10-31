//! The review dispatchers MCP shares (rdy_review_next, rdy_review_submit) and the reviews paragraph of rdy_schema.
use crate::check::api::parse_check;
use crate::check::catalogue::AgentCheck;
use crate::check::requests::{durable_label, next_durable};
use crate::engine::write_typed;
use crate::semantic::candidates::request_label;
use crate::semantic::case::case_hash;
use crate::semantic::submit::{CheckedSubmit, ReviewSubmitRequest};
use crate::{E_PERMISSION, Engine, RdyError, Session, now_ms, rfc3339};
use rdy_model::Role;
use serde::Deserialize;
use serde_json::{Value, json};

/// `rdy_schema`'s reviews paragraph.
pub const REVIEWS_DOC: &str = "rdy_review_next {kind: tension} and rdy_review_submit {request, case_hash, verdict, citations?, severity?, reason?} are the review queue. For each in-flight change, rdy pairs each block it writes with other blocks, at most 20 pairs per change, closest first: blocks linked as premises, blocks that describe the same target, blocks whose texts name the same entity, blocks in the same section, then blocks in the same area; inside each, the same doc first. Normative and content blocks take part; deprecated blocks never do. A request ends when its last in-flight side lands, leaves every view or changes. rdy_review_next needs the reviewer role. It leases the request to the session until it submits, disconnects or reviews.lease_ttl passes, and never offers a leased request, a settled pair or a pair whose block your actor wrote. Its reply holds request (rq-<12 hex>), case (both blocks with their headings, text and governing decisions, the entities both name, the rule and the verdict schema) and case_hash. rdy_review_submit takes verdict tension (1 to 20 citations {block, note}, each on one of the two blocks) or no_tension (0 to 20 citations). A tension verdict opens an advisory semantic tension (kind semantic). With severity blocking and a reason, it advises a person to make the tension block; the advice gates nothing. The propose report and rdy_read P-x@n show advised tensions with their citations. A person accepts the advice once for a proposal version, and you relay it with their words: rdy_resolve {accept: {proposal, advised_digest, evidence, question?, session_ref?, tensions?}}. An accepted tension blocks until a side's content changes. A no_tension verdict settles the pair until a side's content changes. The first verdict settles a pair, but a tension after a no_tension opens the tension; any other later verdict is E_REVIEW_STALE, and so is a case_hash that no longer matches. The reviewer's actor must differ from the author of both blocks (E_SELF_REVIEW). Pending requests never block a landing.";

/// How many requests one `rdy_review_next` tries to claim before it answers that none is free.
const MAX_CLAIM_TRIES: usize = 8;

/// `rdy_review_next`'s input.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewNextRequest {
    /// The review kind: `tension`, a check's name, or `any`.
    pub kind: String,
}

/// What a `rdy_review_next` kind selects.
enum Kind {
    /// The derived tension requests, as before.
    Tension,
    /// The independent durable requests of one check.
    Check(AgentCheck),
    /// The independent durable requests, then the tension queue.
    Any,
}

/// Leases the next review request session `s` may take to `owner` and returns its case:
/// `{request, kind, case_hash, case, lease_expires_at}` (`lease_expires_at` is `null` when
/// leases are off), or `{request: null, leased}` (the live leases) when none is free. `kind`
/// `tension` keeps the derived queue; a check's name offers its independent durable requests in
/// full hash order; `any` offers those of every check, then the tension queue.
pub fn run_review_next(
    engine: &dyn Engine,
    s: &Session,
    owner: u64,
    req: ReviewNextRequest,
) -> Result<Value, RdyError> {
    let kind = match req.kind.as_str() {
        "tension" => Kind::Tension,
        "any" => Kind::Any,
        name => Kind::Check(parse_check(name)?),
    };
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
    let ttl = st.lease_ttl_ms();
    let only = match kind {
        Kind::Tension => None,
        Kind::Check(c) => Some(Some(c)),
        Kind::Any => Some(None),
    };
    if let Some(only) = only {
        let mut leased = leases.leased(now);
        for _ in 0..MAX_CLAIM_TRIES {
            let Some((key, r, case)) = next_durable(&st, &s.actor, &leased, only)? else {
                break;
            };
            if !leases.claim(key, vec![], owner, now, ttl) {
                // another session took it first: the next one
                leased.insert(key);
                continue;
            }
            let expires = (ttl > 0).then(|| rfc3339(now.saturating_add(ttl)));
            return Ok(json!({
                "request": durable_label(&key),
                "kind": r.check,
                "case_hash": case.hash.to_string(),
                "case": case.case,
                "lease_expires_at": expires,
            }));
        }
        if let Kind::Check(_) = kind {
            return Ok(json!({"request": null, "leased": leases.leased(now).len()}));
        }
    }
    let mut leased = leases.leased(now);
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

/// Records session `s`'s verdict and returns its `SubmitInfo`: `{request, review, verdict}`,
/// with `tension`, `severity` and `advice` for a `tension`, and `suppressed: true` for a
/// `no_tension`. The request is resolved on a read, outside the writer (a lease-less submit walks
/// the queue, candidate requests are never computed on the writer, and a durable request's case
/// is rebuilt there); the writer runs only `Rdy::submit_resolved`, which re-proves that the
/// request is current.
pub fn run_review_submit(
    engine: &dyn Engine,
    s: &Session,
    req: ReviewSubmitRequest,
) -> Result<Value, RdyError> {
    let c = CheckedSubmit::new(s, req)?;
    let found = engine
        .read()
        .resolve_submit(&engine.leases(), &c, now_ms())?;
    write_typed(engine, move |r| r.submit_resolved(c, found))
}
