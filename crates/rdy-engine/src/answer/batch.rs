//! The acts on a batch and on the items beside it (D-YFDG3, F1, F2, F4). The dispatch is closed:
//!
//! | item      | option    | act                                                     |
//! |-----------|-----------|---------------------------------------------------------|
//! | `approve` | `approve` | lands the version (`land_with`); no `ANSWER`            |
//! | `approve` | `reject`  | `ANSWER` `reject_proposal`: the version leaves the queue |
//! | any       | `later`   | `ANSWER` `later` under `pending/`; needs a relay (F4)   |
//! | `confirm` | `confirm` | `ANSWER` `confirm`                                      |
//! | `confirm` | `reject`  | `ANSWER` `reject_decision`: the batch is not ready (F1) |
//! | `confirm` (`reaffirm_one_way`) | `reject` | `ANSWER` `reject_reaffirm`: not ready |
//! | `defer`   | `o1`      | `ANSWER` `accept_deferrals`, with the card's full hash  |
//! | `defer`   | `o2`      | `ANSWER` `refuse_deferrals`: the batch is not ready     |
//!
//! An `approve` item's `accept_verdict:<review>:<version>` option never reaches this table:
//! `answer_one` dispatches it first ([`super::verdict`], D-JQD5X O14).
//!
//! Any other pair is refused with `E_BAD_INPUT`. An `ANSWER` is written alone in one batch with
//! `Store::put_answer`, and its publish is `Delta::Seq` (O14). Canon never changes but by a
//! landing.
//!
//! # The exception check (F2)
//!
//! An approval is refused with `E_EXCEPTION_OPEN` while an exception of the version has no
//! `confirm` answer bound to its decision's current version (`approval_holds`). The refusal
//! names each one and carries its card in `corrected.exceptions` (O21). `land_with` holds the
//! check for every approval path (Task 17), so an answer's approval gets it there, and nothing
//! is written. A decision the person rejected never lands, exception or not (F1): `land_with`
//! refuses it with the issue the queue's readiness gives.
//!
//! # Content removal (D-CC90E)
//!
//! An `approve` item that carries removal requests is answered here like any other, one item at
//! a time, as the person (local) or by a relay of the person's words: its card hash is the
//! removal digest, so an approval binds each request and the hash of its complete report. A batch
//! of recommended answers never reaches it (`Rdy::answer_all_recommended` refuses the whole call).
use super::{AnswerInfo, AnswerStep, Bound, PreparedAnswer};
use crate::error::{E_BAD_INPUT, E_INTERNAL, RdyError};
use crate::gate::LandOptions;
use crate::pending::{ExceptionKind, Item, ItemKind};
use crate::{Delta, Rdy, Session};
use rdy_model::{Answer, AnswerAct, ApprovalEvidence, EvidenceKind};
use rdy_store::{Relay, StoreError};

/// Runs the act the item's kind and the chosen option name (the table in the module docs). An
/// approval is only prepared: [`super::Rdy::commit_answer`] lands it. Every other act is written
/// here.
pub(crate) fn act(
    r: &mut Rdy,
    who: &Session,
    relay: Option<Relay>,
    kind: Option<EvidenceKind>,
    b: Bound,
    note: Option<String>,
) -> Result<AnswerStep, RdyError> {
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
        (ItemKind::Approve, "approve") => {
            return approve(r, who, relay, kind, b, note)
                .map(|a| AnswerStep::Approval(Box::new(a)));
        }
        (ItemKind::Approve, "reject") => AnswerAct::RejectProposal,
        (ItemKind::Confirm, "confirm") => AnswerAct::Confirm,
        // D-GV465: a rejected re-affirmation, never a rejected decision
        (ItemKind::Confirm, "reject")
            if b.item.exception == Some(ExceptionKind::ReaffirmOneWay) =>
        {
            AnswerAct::RejectReaffirm
        }
        (ItemKind::Confirm, "reject") => AnswerAct::RejectDecision,
        (ItemKind::Defer, "o1") => AnswerAct::AcceptDeferrals,
        (ItemKind::Defer, "o2") => AnswerAct::RefuseDeferrals,
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
    record(r, who, relay, kind, b, note, act).map(|i| AnswerStep::Done(Box::new(i)))
}

/// The labels the card shows, in order.
fn labels(i: &Item) -> Vec<String> {
    i.options.iter().map(|o| o.label.clone()).collect()
}

fn assurance(relay: &Option<Relay>) -> &'static str {
    if relay.is_some() { "relayed" } else { "local" }
}

/// approve · `approve`: the landing (which holds the exception check, F2), with the card as
/// evidence for a relay, prepared under the writer and not yet written.
fn approve(
    r: &mut Rdy,
    who: &Session,
    relay: Option<Relay>,
    kind: Option<EvidenceKind>,
    b: Bound,
    note: Option<String>,
) -> Result<PreparedAnswer, RdyError> {
    if note.is_some() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "an approval keeps no note",
            "drop the note; the landing records the approval",
        ));
    }
    let evidence = relay.as_ref().map(|_| ApprovalEvidence {
        kind: kind.unwrap_or(EvidenceKind::Card),
        options: labels(&b.item),
        choice: Some(b.option.label.clone()),
        card_hash: b.card_hash,
    });
    let landing = r.prepare_landing(
        who,
        &b.item.r#ref,
        LandOptions {
            strict: false,
            relay: relay.clone(),
            evidence,
        },
    )?;
    Ok(PreparedAnswer {
        landing,
        info: AnswerInfo {
            r#ref: b.item.r#ref,
            act: "approve".into(),
            option: b.option.id,
            label: b.option.label,
            assurance: assurance(&relay),
            answer: None,
            landing: None,
            relayed_by: relay.map(|r| r.by),
            next: None,
        },
        next_session: None,
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
    // D-AAQ9Z: a deferral answer binds the card's complete hash, local or relayed, so the
    // landing can check it names the card the version has now
    let card_hash = match b.item.kind {
        ItemKind::Defer => Some(b.item.card_hash.parse().map_err(|_| {
            RdyError::new(
                E_INTERNAL,
                format!("{} has no card hash", b.item.r#ref),
                "report this as a bug in rdy",
            )
        })?),
        _ => b.card_hash,
    };
    let a = Answer {
        item: b.item.r#ref.clone(),
        act,
        option: Some(b.option.id.clone()),
        label: Some(b.option.label.clone()),
        options: labels(&b.item),
        card_hash,
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
