//! rdy_pending and rdy_answer: the chat path to the "needs you" queue (D-KQ4CA, D-JV6Z8,
//! D-ST423). `rdy_pending` lists what waits for the person, each item with its card. `rdy_answer`
//! relays the person's answer to one card (or each of several, with its recommended option),
//! bound to the card they were shown. An MCP session is an agent session, so an answer always
//! relays the person's words; the session is the relay (the relay table, D-HA7YZ). Elicited
//! answers wait for M3 (F5).

use super::*;
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::pending::{CARD_DEFAULT_TOKENS, PendingQuery};
use rdy_engine::signing_act::{SigningAct, run_signed_act};
use rdy_engine::*;
use rdy_model::EvidenceKind;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PendingParams {
    /// The chat session you speak with the person in. Later hides a card only in the session
    /// that said it, so pass the same session_ref you pass to rdy_answer.
    pub session_ref: Option<String>,
    /// The token budget of each card (default 2000); a longer card loses its last facts. The
    /// card_hash does not change with the budget.
    pub max_tokens: Option<usize>,
}

/// One answer of `all_recommended`: the card's ref and the hash of the card the person was shown.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecommendedParams {
    /// The item ref, as rdy_pending gives it.
    pub r#ref: String,
    /// The card_hash rdy_pending gave with the card.
    pub card_hash: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnswerParams {
    /// The item ref, as rdy_pending gives it. Leave out with all_recommended.
    pub r#ref: Option<String>,
    /// The id of the option the person chose, as the card lists it (`approve`, `reject`,
    /// `confirm`, `later`, `o1`, ...). Leave out with all_recommended.
    pub option: Option<String>,
    /// The card_hash rdy_pending gave with the card the person was shown. A card that changed
    /// since is refused, with the fresh card in corrected.
    pub card_hash: Option<String>,
    /// The person's note, quoted if they gave one (at most 2000 characters).
    pub note: Option<String>,
    /// A briefing token from rdy_brief. Only the answer to a question whose decision goes under
    /// stable content takes one.
    pub brief: Option<String>,
    /// The person's words, quoted (1 to 2000 characters). Without them nothing is recorded.
    pub evidence: String,
    /// The question you asked the person.
    pub question: Option<String>,
    /// The chat session the person spoke in (default: mcp).
    pub session_ref: Option<String>,
    /// How you got the answer: `card` (the person saw the card; default) or `text` (their own
    /// words).
    pub kind: Option<String>,
    /// Answers each entry with its recommended option instead of one item; leave out ref, option
    /// and card_hash. Each entry is its own answer, in the order given; it stops at the first
    /// refusal.
    pub all_recommended: Option<Vec<RecommendedParams>>,
}

fn bad(what: impl Into<String>, fix: impl Into<String>) -> RdyError {
    RdyError::new(E_BAD_INPUT, what, fix)
}

/// The evidence kind a caller names: `card` or `text`, else none. `elicited` is read too, so the
/// engine refuses it in its own words (F5).
pub fn evidence_kind(kind: Option<&str>) -> Result<Option<EvidenceKind>, RdyError> {
    match kind {
        None => Ok(None),
        Some("card") => Ok(Some(EvidenceKind::Card)),
        Some("text") => Ok(Some(EvidenceKind::Text)),
        Some("elicited") => Ok(Some(EvidenceKind::Elicited)),
        Some(other) => Err(bad(
            format!("kind {other} is not card or text"),
            "pass kind card for an answer on the card, or text for the person's own words",
        )),
    }
}

/// `rdy_pending`: the person's queue, each item with its card and the card's hash. A session is
/// required; no role.
pub fn rdy_pending(st: &mut McpState, p: PendingParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    let person = human_actor_for_env(&st.engine.root());
    let q = st.engine.read().pending(&PendingQuery {
        person,
        session_ref: p.session_ref,
        max_tokens: p.max_tokens.unwrap_or(CARD_DEFAULT_TOKENS),
    })?;
    Ok(serde_json::to_value(q).expect("the queue serializes"))
}

/// `rdy_answer`: the person's answer to one card, or each of several with its recommended option,
/// relayed by this agent session. The person is the project's human actor (O18).
pub fn rdy_answer(st: &mut McpState, p: AnswerParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let person = human_actor_for_env(&st.engine.root());
    let relay = RelayInput {
        relayed_by: None,
        evidence: Some(p.evidence),
        question: p.question,
        session_ref: p.session_ref,
        kind: evidence_kind(p.kind.as_deref())?,
    };
    if let Some(entries) = p.all_recommended {
        if p.r#ref.is_some() || p.option.is_some() || p.card_hash.is_some() {
            return Err(bad(
                "all_recommended goes without ref, option and card_hash",
                "name each item in all_recommended, or answer one item with ref, option and card_hash",
            ));
        }
        if p.note.is_some() || p.brief.is_some() {
            return Err(bad(
                "all_recommended takes no note and no brief",
                "answer the item on its own to add a note or a brief",
            ));
        }
        if entries.is_empty() {
            return Err(bad(
                "all_recommended is empty",
                "name the items to answer, each with the card_hash of its card",
            ));
        }
        let (refs, hashes): (Vec<String>, Vec<String>) =
            entries.into_iter().map(|e| (e.r#ref, e.card_hash)).unzip();
        // each approval of the list is signed in its own batch (D-1Z951)
        let request = SigningAct::Recommended {
            refs,
            card_hashes: hashes,
            relay,
        };
        return run_signed_act(&*st.engine, &*st.agent, &s, &person, request);
    }
    let (Some(r#ref), Some(option)) = (p.r#ref, p.option) else {
        return Err(bad(
            "an answer names its item and its option",
            "pass ref and option, or all_recommended",
        ));
    };
    let req = AnswerRequest {
        r#ref,
        option,
        card_hash: p.card_hash,
        note: p.note,
        relay,
        brief: p.brief,
    };
    run_signed_act(
        &*st.engine,
        &*st.agent,
        &s,
        &person,
        SigningAct::Answer(req),
    )
}
