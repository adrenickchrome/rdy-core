//! The queue's ops: `pending` and `answer` (D-KQ4CA, D-JV6Z8, D-ST423). They are the CLI's
//! twins of `rdy_pending` and `rdy_answer`. The person is the project's human actor
//! (`cx.human`, O18). A human session answers as itself (`local`); an agent session only by
//! relaying the person's words, as the relay table says (D-HA7YZ). Every argument struct refuses
//! unknown fields (FW-HD1).
use super::*;
use rdy_engine::answer::AnswerRequest;
use rdy_engine::pending::{CARD_DEFAULT_TOKENS, PendingQuery};
use rdy_engine::signing_act::{SigningAct, run_signed_act};
use rdy_mcp::tools::evidence_kind;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PendingArgs {
    max_tokens: Option<usize>,
    session_ref: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecommendedArgs {
    r#ref: String,
    card_hash: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AnswerArgs {
    r#ref: Option<String>,
    option: Option<String>,
    card_hash: Option<String>,
    note: Option<String>,
    brief: Option<String>,
    relayed_by: Option<String>,
    evidence: Option<String>,
    question: Option<String>,
    session_ref: Option<String>,
    kind: Option<String>,
    all_recommended: Option<Vec<RecommendedArgs>>,
}

/// The person's queue, as `rdy_pending` gives it.
pub(super) fn pending(cx: &OpContext, a: PendingArgs) -> Result<Value, RdyError> {
    check_max_tokens(a.max_tokens)?;
    let q = cx.engine.read().pending(&PendingQuery {
        person: cx.human.to_string(),
        session_ref: a.session_ref,
        max_tokens: a.max_tokens.unwrap_or(CARD_DEFAULT_TOKENS),
    })?;
    Ok(to_json(&q))
}

/// The refusal of Later on the ctl surface, which the CLI is (F4).
const NO_LATER: &str = "the CLI has no Later";
const NO_LATER_FIX: &str =
    "answer with another option; Later is a chat act, relayed with rdy_answer";

fn bad(what: impl Into<String>, fix: impl Into<String>) -> RdyError {
    RdyError::new(E_BAD_INPUT, what, fix)
}

/// One answer, or each of `all_recommended` with its recommended option.
pub(super) fn answer(cx: &OpContext, a: AnswerArgs) -> Result<Value, RdyError> {
    // Later is a chat act, relayed with rdy_answer; no ctl caller gets it, relay or not (F4)
    if a.option.as_deref() == Some("later") {
        return Err(bad(NO_LATER, NO_LATER_FIX));
    }
    // an agent's relay is `relayed_by` with `evidence`: half of it never reaches the engine
    if !cx.session.is_human() && a.relayed_by.is_some() != a.evidence.is_some() {
        return Err(RdyError::new(E_BAD_INPUT, RELAY_PAIR, RELAY_PAIR_FIX));
    }
    let relay = RelayInput {
        relayed_by: a.relayed_by,
        evidence: a.evidence,
        question: a.question,
        session_ref: a.session_ref,
        kind: evidence_kind(a.kind.as_deref())?,
    };
    if let Some(entries) = a.all_recommended {
        if a.r#ref.is_some() || a.option.is_some() || a.card_hash.is_some() {
            return Err(bad(
                "all_recommended goes without ref, option and card_hash",
                "name each item in all_recommended, or answer one item with ref and option",
            ));
        }
        if a.note.is_some() || a.brief.is_some() {
            return Err(bad(
                "all_recommended takes no note and no brief",
                "answer the item on its own to add a note or a brief",
            ));
        }
        if entries.is_empty() {
            return Err(bad("all_recommended is empty", "name the items to answer"));
        }
        // a card hash for every item or for none: a local answer may bind to the version only
        let hashes: Vec<String> = entries.iter().filter_map(|e| e.card_hash.clone()).collect();
        if !hashes.is_empty() && hashes.len() != entries.len() {
            return Err(bad(
                "some items of all_recommended name a card_hash and some do not",
                "pass the card_hash of every item, or of none",
            ));
        }
        let refs: Vec<String> = entries.into_iter().map(|e| e.r#ref).collect();
        // each approval of the list is signed in its own batch (D-1Z951)
        let request = SigningAct::Recommended {
            refs,
            card_hashes: hashes,
            relay,
        };
        return run_signed_act(cx.engine, cx.agent, cx.session, cx.human, request);
    }
    let (Some(r#ref), Some(option)) = (a.r#ref, a.option) else {
        return Err(bad(
            "an answer names its item and its option",
            "pass ref and option, or all_recommended",
        ));
    };
    let req = AnswerRequest {
        r#ref,
        option,
        card_hash: a.card_hash,
        note: a.note,
        relay,
        brief: a.brief,
    };
    run_signed_act(
        cx.engine,
        cx.agent,
        cx.session,
        cx.human,
        SigningAct::Answer(req),
    )
}
