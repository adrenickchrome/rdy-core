//! The acts on a batch and on the items beside it (D-YFDG3, F1, F2, F4). The dispatch is closed:
//!
//! | item      | option    | act                                                     |
//! |-----------|-----------|---------------------------------------------------------|
//! | `approve` | `approve` | lands the version (`land_with`); no `ANSWER`            |
//! | `approve` | `reject`  | `ANSWER` `reject_proposal`: the version leaves the queue |
//! | any       | `later`   | `ANSWER` `later` under `pending/`; a relay or elicited (F4) |
//! | `confirm` | `confirm` | `ANSWER` `confirm`                                      |
//! | `confirm` | `reject`  | `ANSWER` `reject_decision`: the batch is not ready (F1) |
//! | `confirm` (`reaffirm_one_way`) | `reject` | `ANSWER` `reject_reaffirm`: not ready |
//! | `defer`   | `o1`      | `ANSWER` `accept_deferrals`, with the card's full hash  |
//! | `defer`   | `o2`      | `ANSWER` `refuse_deferrals`: the batch is not ready     |
//!
//! An `approve` item's `accept_verdict:<review>:<version>` option never reaches this table:
//! `answer_one` dispatches it first ([`super::verdict`], D-JQD5X O14).
//!
//! An elicited Later (D-WPE4R, F2) is the person's own snooze of the chat session the caller's
//! run names: its `ANSWER` records that run as `person.run`, evidence `elicited` and no relay. A
//! caller with no run is refused. A Later in the UI never reaches this table: it stores nothing
//! ([`super::presented`]).
//!
//! Any other pair is refused with `E_BAD_INPUT`. An `ANSWER` is written alone in one batch with
//! `Store::put_answer`, and its publish is `Delta::Seq` (O14). Canon never changes but by a
//! landing.
//!
//! A rule's own confirmation ([`super::RuleContext::Text`]) records the person's words and no
//! card: no option, no label, no options and no hash, with evidence `text`.
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
use super::presented::Presentation;
use super::{AnswerInfo, AnswerStep, Answerer, Bound, PreparedAnswer, RuleContext};
use crate::error::{E_BAD_INPUT, E_INTERNAL, RdyError};
use crate::gate::LandOptions;
use crate::pending::{ExceptionKind, Item, ItemKind};
use crate::{Delta, Rdy, Session};
use rdy_model::{Answer, AnswerAct, ApprovalEvidence, EvidenceKind, Provenance};
use rdy_store::{Relay, StoreError};

/// Runs the act the item's kind and the chosen option name (the table in the module docs). An
/// approval is only prepared: [`super::Rdy::commit_answer`] lands it. Every other act is written
/// here.
pub(crate) fn act(
    r: &mut Rdy,
    by: &Answerer,
    b: Bound,
    note: Option<String>,
) -> Result<AnswerStep, RdyError> {
    let who = &by.who;
    let (relay, kind) = (by.relay.clone(), by.kind);
    let mut person = who.provenance();
    let act = match (b.item.kind, b.option.id.as_str()) {
        (_, "later") => {
            if relay.is_none() {
                person.run = Some(elicited_run(by)?);
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
    record(r, by, person, b, note, act).map(|i| AnswerStep::Done(Box::new(i)))
}

/// The chat session of a Later with no relay: the run of the session that elicited it (O9). A
/// caller with no run, and any other answer with no relay (the CLI's), is refused.
fn elicited_run(by: &Answerer) -> Result<String, RdyError> {
    match &by.presented {
        Some(p) if p.source == Presentation::Elicited => p.caller.run.clone().ok_or_else(|| {
            RdyError::new(
                E_BAD_INPUT,
                "Later needs a run to name the chat session",
                "ask again from a session with a run, or show the card in chat and relay the answer",
            )
        }),
        _ => Err(RdyError::new(
            E_BAD_INPUT,
            "Later needs a chat session; the CLI has no Later",
            "answer with another option, or let the agent relay Later from chat",
        )),
    }
}

/// The labels the card shows, in order.
fn labels(i: &Item) -> Vec<String> {
    i.options.iter().map(|o| o.label.clone()).collect()
}

fn assurance(relay: &Option<Relay>) -> &'static str {
    if relay.is_some() { "relayed" } else { "local" }
}

/// approve · `approve`: the landing (which holds the exception check, F2), with the card as
/// evidence for a relay or a presented answer, prepared under the writer and not yet written.
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
    let evidence = (relay.is_some() || kind.is_some()).then(|| ApprovalEvidence {
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

/// Writes the `ANSWER` of `act` by `person` in a batch of its own and publishes it.
fn record(
    r: &mut Rdy,
    by: &Answerer,
    person: Provenance,
    b: Bound,
    note: Option<String>,
    act: AnswerAct,
) -> Result<AnswerInfo, RdyError> {
    let (relay, kind) = (by.relay.clone(), by.kind);
    // the person's words alone: no card was shown, so nothing claims one
    let shown = !by.rule.as_ref().is_some_and(RuleContext::no_card);
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
        option: shown.then(|| b.option.id.clone()),
        label: shown.then(|| b.option.label.clone()),
        options: if shown { labels(&b.item) } else { vec![] },
        card_hash: card_hash.filter(|_| shown),
        targets: vec![],
        person,
        relayed_by: relay.as_ref().map(|r| r.by.clone()),
        relay_evidence: relay.as_ref().map(|r| r.evidence.clone()),
        evidence_kind: kind,
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
