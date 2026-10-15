//! The acts on a batch and on the items beside it (D-YFDG3, F1, F2, F4). The dispatch is closed:
//!
//! | item      | option    | act                                                     |
//! |-----------|-----------|---------------------------------------------------------|
//! | `approve` | `approve` | lands the version (`land_with`); no `ANSWER`            |
//! | `approve` | `reject`  | `ANSWER` `reject_proposal`: the version leaves the queue |
//! | any       | `later`   | `ANSWER` `later` under `pending/`; needs a relay (F4)   |
//! | `confirm` | `confirm` | `ANSWER` `confirm`                                      |
//! | `confirm` | `reject`  | `ANSWER` `reject_decision`: the batch is not ready (F1) |
//!
//! Any other pair is refused with `E_BAD_INPUT`. An `ANSWER` is written alone in one batch with
//! `Store::put_answer`, and its publish is `Delta::Seq` (O14). Canon never changes but by a
//! landing.
//!
//! # The exception check (F2)
//!
//! An approval is refused with `E_EXCEPTION_OPEN` while an exception of the version has no
//! `confirm` answer bound to its decision's current version (`open_exceptions`). The refusal
//! names each one and carries its card in `corrected.exceptions` (O21). It runs before
//! `land_with`, so nothing is written.
use super::{AnswerInfo, Bound, card_of};
use crate::error::{E_BAD_INPUT, E_EXCEPTION_OPEN, E_INTERNAL, RdyError};
use crate::gate::{LandOptions, resolve_in};
use crate::pending::queue::open_exceptions;
use crate::pending::{Item, ItemKind};
use crate::{Delta, Rdy, Session};
use rdy_model::{Answer, AnswerAct, ApprovalEvidence, EvidenceKind};
use rdy_store::{Relay, StoreError};
use serde_json::json;

/// Runs the act the item's kind and the chosen option name (the table in the module docs).
pub(crate) fn act(
    r: &mut Rdy,
    who: &Session,
    relay: Option<Relay>,
    kind: Option<EvidenceKind>,
    b: Bound,
    note: Option<String>,
) -> Result<AnswerInfo, RdyError> {
    let act = match (b.item.kind, b.option.id.as_str()) {
        (_, "later") => {
            if relay.is_none() {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    "Later needs a chat session; the CLI has no Later",
                    "answer with another option, or let the agent relay Later from chat",
                ));
            }
            AnswerAct::Later
        }
        (ItemKind::Approve, "approve") => return approve(r, who, relay, kind, b, note),
        (ItemKind::Approve, "reject") => AnswerAct::RejectProposal,
        (ItemKind::Confirm, "confirm") => AnswerAct::Confirm,
        (ItemKind::Confirm, "reject") => AnswerAct::RejectDecision,
        (k, option) => {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!(
                    "{}: option {option} of a {} item has no answer path yet",
                    b.item.r#ref,
                    k.word()
                ),
                "use rdy resolve for tensions, or rdy decide for a question",
            ));
        }
    };
    record(r, who, relay, kind, b, note, act)
}

/// The labels the card shows, in order.
fn labels(i: &Item) -> Vec<String> {
    i.options.iter().map(|o| o.label.clone()).collect()
}

fn assurance(relay: &Option<Relay>) -> &'static str {
    if relay.is_some() { "relayed" } else { "local" }
}

/// approve · `approve`: the exception check (F2), then the landing, with the card as evidence
/// for a relay.
fn approve(
    r: &mut Rdy,
    who: &Session,
    relay: Option<Relay>,
    kind: Option<EvidenceKind>,
    b: Bound,
    note: Option<String>,
) -> Result<AnswerInfo, RdyError> {
    if note.is_some() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "an approval keeps no note",
            "drop the note; the landing records the approval",
        ));
    }
    let st = r.read();
    let pref = resolve_in(&st, &b.item.r#ref)?;
    let open = open_exceptions(&st, &pref)?;
    drop(st);
    if !open.is_empty() {
        let refs: Vec<&str> = open.iter().map(|i| i.r#ref.as_str()).collect();
        let cards: Vec<_> = open.iter().map(card_of).collect();
        return Err(RdyError::new(
            E_EXCEPTION_OPEN,
            format!("{} exceptions of {pref} have no answer", open.len()),
            format!("answer each exception card first: {}", refs.join(", ")),
        )
        .with_corrected(json!({ "exceptions": cards })));
    }
    let evidence = relay.as_ref().map(|_| ApprovalEvidence {
        kind: kind.unwrap_or(EvidenceKind::Card),
        options: labels(&b.item),
        choice: Some(b.option.label.clone()),
        card_hash: b.card_hash,
    });
    let landing = r.land_with(
        who,
        &b.item.r#ref,
        LandOptions {
            strict: false,
            relay: relay.clone(),
            evidence,
        },
    )?;
    Ok(AnswerInfo {
        r#ref: b.item.r#ref,
        act: "approve".into(),
        option: b.option.id,
        label: b.option.label,
        assurance: assurance(&relay),
        answer: None,
        landing: Some(landing),
        relayed_by: relay.map(|r| r.by),
        next: None,
    })
}

/// Writes the `ANSWER` of `act` in a batch of its own and publishes it.
fn record(
    r: &mut Rdy,
    who: &Session,
    relay: Option<Relay>,
    kind: Option<EvidenceKind>,
    b: Bound,
    note: Option<String>,
    act: AnswerAct,
) -> Result<AnswerInfo, RdyError> {
    let a = Answer {
        item: b.item.r#ref.clone(),
        act,
        option: Some(b.option.id.clone()),
        label: Some(b.option.label.clone()),
        options: labels(&b.item),
        card_hash: b.card_hash,
        targets: vec![],
        person: who.provenance(),
        relayed_by: relay.as_ref().map(|r| r.by.clone()),
        relay_evidence: relay.as_ref().map(|r| r.evidence.clone()),
        evidence_kind: relay.as_ref().and(kind),
        note,
    };
    // a refusal means rdy built an answer the store's rules reject: a bug, not bad input
    let h = r.store_mut().put_answer(&a).map_err(|e| match e {
        StoreError::Refused(message) => {
            RdyError::new(E_INTERNAL, message, "report this as a bug in rdy")
        }
        e => e.into(),
    })?;
    r.publish(Delta::Seq)?;
    Ok(AnswerInfo {
        r#ref: b.item.r#ref,
        act: act.as_str().into(),
        option: b.option.id,
        label: b.option.label,
        assurance: assurance(&relay),
        answer: Some(h.to_string()),
        landing: None,
        relayed_by: relay.map(|r| r.by),
        next: None,
    })
}
