//! The answers to a re-affirmation card (D-CQMA4, D-EJBPD, D-KQ4CA; p-WQB0V, p-D3P76). The card
//! stands in for a briefing: its answer is the approval of its own isolated landing, and nothing
//! else rides in that landing.
//!
//! | option | act                                                                        |
//! |--------|----------------------------------------------------------------------------|
//! | `o2`   | one `ANSWER` `keep_suspect`: the card's hash, the person's note, and every member's BLOCK version as `targets` |
//! | `o1`   | the isolated landing of the members' re-affirmation (`Rdy::prepare_reaffirm_landing`); no `ANSWER` |
//!
//! # Keep them suspect
//!
//! The note is the reason, 1 to 2000 characters after trimming. The `ANSWER` binds the card's
//! complete hash, the person (local) or the relay with the current card evidence rules, and the
//! sorted member versions, so the store's deferral table reads one accepted deferral per member
//! and the card's cause from the answer alone. No plan, change or landing is written; the
//! suspects stay, with their deferral.
//!
//! # They still hold
//!
//! The note is the reason, or `They still hold` when none is given; a note that is given must
//! hold 1 to 2000 characters, as the public reaffirm op's reason. The landing writes only the
//! members' pins and re-affirmation metadata for the card's cause, records the answering person
//! as its approver with the card as its evidence (both labels, and the chosen one; a relay with
//! text evidence is refused), and asks for no second approval and no one-way exception: the
//! person answered this exact card.
use super::{
    AnswerInfo, AnswerStep, Answerer, Bound, PreparedAnswer, assurance, labels, put_alone,
};
use crate::Rdy;
use crate::error::{E_BAD_INPUT, E_INTERNAL, E_PERMISSION, RdyError};
use crate::gate::ReaffirmLanding;
use crate::pending::PlanCardBinding;
use crate::pending::reaffirm::{HOLD_LABEL, KEEP_LABEL};
use crate::reaffirm::Reason;
use rdy_model::engine_fields::MAX_REASON_CHARS;
use rdy_model::{Answer, AnswerAct, ApprovalEvidence, EvidenceKind, Hash};

/// Runs the act of the option `b` chose on its re-affirmation card (the table in the module
/// docs). The landing of `o1` is only prepared: [`super::Rdy::commit_answer`] lands it.
pub(crate) fn act(
    r: &mut Rdy,
    by: &Answerer,
    b: Bound,
    note: Option<String>,
) -> Result<AnswerStep, RdyError> {
    match b.option.id.as_str() {
        "o1" => hold(r, by, b, note).map(|a| AnswerStep::Approval(Box::new(a))),
        "o2" => keep(r, by, b, note).map(|i| AnswerStep::Done(Box::new(i))),
        other => Err(RdyError::new(
            E_BAD_INPUT,
            format!("{other} is not an option of {}", b.item.r#ref),
            "answer with one of: o1, o2",
        )),
    }
}

/// The card's complete hash, as the queue computed it.
fn card_hash(b: &Bound) -> Result<Hash, RdyError> {
    b.item.card_hash.parse().map_err(|_| {
        RdyError::new(
            E_INTERNAL,
            format!("{} has no card hash", b.item.r#ref),
            "report this as a bug in rdy",
        )
    })
}

/// `o2`: one `KeepSuspect` ANSWER (see the module docs).
fn keep(
    r: &mut Rdy,
    by: &Answerer,
    b: Bound,
    note: Option<String>,
) -> Result<AnswerInfo, RdyError> {
    let note = note
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty() && n.chars().count() <= MAX_REASON_CHARS)
        .ok_or_else(|| {
            RdyError::new(
                E_BAD_INPUT,
                "Keep them suspect needs a note",
                "pass note with a reason of 1 to 2000 characters",
            )
        })?;
    let Some(PlanCardBinding::Reaffirm { members, .. }) = &b.item.plan_binding else {
        return Err(RdyError::new(
            E_INTERNAL,
            format!("{} binds no re-affirmation card", b.item.r#ref),
            "report this as a bug in rdy",
        ));
    };
    // the item ref names the cause's version; the targets name each member's
    let mut targets: Vec<Hash> = members.iter().map(|m| m.version).collect();
    targets.sort();
    targets.dedup();
    let relay = by.relay.clone();
    let a = Answer {
        item: b.item.r#ref.clone(),
        act: AnswerAct::KeepSuspect,
        option: Some(b.option.id.clone()),
        label: Some(KEEP_LABEL.to_string()),
        options: labels(&b.item),
        card_hash: Some(card_hash(&b)?),
        targets,
        person: by.who.provenance(),
        relayed_by: relay.as_ref().map(|r| r.by.clone()),
        relay_evidence: relay.as_ref().map(|r| r.evidence.clone()),
        evidence_kind: relay.as_ref().and(by.kind),
        note: Some(note),
    };
    let h = put_alone(r, &a)?;
    Ok(AnswerInfo {
        r#ref: b.item.r#ref,
        act: AnswerAct::KeepSuspect.as_str().into(),
        option: b.option.id,
        label: b.option.label,
        assurance: assurance(&relay),
        answer: Some(h.to_string()),
        landing: None,
        relayed_by: relay.map(|r| r.by),
        next: None,
    })
}

/// `o1`: the card's isolated landing, prepared under the writer (see the module docs).
fn hold(
    r: &mut Rdy,
    by: &Answerer,
    b: Bound,
    note: Option<String>,
) -> Result<PreparedAnswer, RdyError> {
    if !by.who.role.can_land() {
        return Err(RdyError::new(
            E_PERMISSION,
            format!("role {:?} cannot land proposals", by.who.role),
            "ask an approver to land it",
        ));
    }
    // the landing's evidence is the card the person answered, local or relayed (D-CQMA4): a
    // relay that calls its evidence the person's own words is refused before anything is written
    if by.kind.is_some_and(|k| k != EvidenceKind::Card) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "a re-affirmation card's landing records card evidence, not text",
            "relay the answer with kind card, or leave kind out",
        ));
    }
    let reason = Reason::parse(note.as_deref().unwrap_or(HOLD_LABEL))?;
    let evidence = ApprovalEvidence {
        kind: EvidenceKind::Card,
        options: labels(&b.item),
        choice: Some(HOLD_LABEL.to_string()),
        card_hash: Some(card_hash(&b)?),
    };
    let relay = by.relay.clone();
    let landing = r.prepare_reaffirm_landing(ReaffirmLanding {
        card: &b.item,
        approver: by.who.clone(),
        relay: relay.clone(),
        evidence,
        reason,
    })?;
    Ok(PreparedAnswer {
        landing,
        info: AnswerInfo {
            r#ref: b.item.r#ref,
            act: "reaffirm".into(),
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
