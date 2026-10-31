//! A person accepts a check verdict as blocking (D-JQD5X QD2, p-8WADR; O14): the option
//! `accept_verdict:<review>:<version>` of a version's approve card. It runs after the card is
//! bound ([`super::bind`]): the card shown, its relay and evidence, and the approve item's
//! required policy role are checked there, as for every answer but an approval.
//!
//! The option is checked against the fresh card: it names a `concern` or `fail` row the card
//! shows now (so its case matches the version now), and the version the card stands for. rdy
//! writes one `ANSWER` alone in its batch: act `accept_verdict`, item `verdict:<review>`, the full
//! option and its label, the card's labels and hash, no targets, and the person, relay, evidence
//! kind and note as every answer records them. It writes no proposal, assessment or landing. The
//! next queue read lists the version as not ready ([`crate::check::verdicts`]).
use super::{AnswerInfo, Answerer, Bound, assurance, labels, put_alone};
use crate::Rdy;
use crate::check::verdicts::{VERDICT_ITEM, parse_accept_option};
use crate::error::{E_BAD_INPUT, E_INTERNAL, RdyError};
use crate::gate::resolve_in;
use rdy_model::{Answer, AnswerAct, Hash};

/// Whether option id `id` is an acceptance option.
pub(crate) fn is_accept(id: &str) -> bool {
    id.starts_with(crate::check::verdicts::ACCEPT_PREFIX)
}

/// Writes the acceptance `b` binds (module docs).
pub(crate) fn act(
    r: &mut Rdy,
    by: &Answerer,
    b: Bound,
    note: Option<String>,
) -> Result<AnswerInfo, RdyError> {
    let id = b.option.id.clone();
    let not_option = || {
        RdyError::new(
            E_BAD_INPUT,
            format!("{id} is not an option of {}", b.item.r#ref),
            "pass an option the card shows",
        )
    };
    let (review, version) = parse_accept_option(&id).ok_or_else(not_option)?;
    let pref = resolve_in(&r.read(), &b.item.r#ref)?;
    let shown = b
        .item
        .check_verdicts
        .iter()
        .any(|v| v.review == review && v.acceptable());
    if version != pref.version || !shown {
        return Err(not_option());
    }
    // the card the person was shown: the hash they passed, else the one bound now
    let card_hash = match b.card_hash {
        Some(h) => h,
        None => b.item.card_hash.parse::<Hash>().map_err(|_| {
            RdyError::new(
                E_INTERNAL,
                format!("{} has no card hash", b.item.r#ref),
                "report this as a bug in rdy",
            )
        })?,
    };
    let a = Answer {
        item: format!("{VERDICT_ITEM}{review}"),
        act: AnswerAct::AcceptVerdict,
        option: Some(id.clone()),
        label: Some(b.option.label.clone()),
        options: labels(&b.item),
        card_hash: Some(card_hash),
        targets: vec![],
        person: by.who.provenance(),
        relayed_by: by.relay.as_ref().map(|x| x.by.clone()),
        relay_evidence: by.relay.as_ref().map(|x| x.evidence.clone()),
        evidence_kind: by.relay.as_ref().and(by.kind),
        note,
    };
    let h = put_alone(r, &a)?;
    Ok(AnswerInfo {
        r#ref: b.item.r#ref,
        act: AnswerAct::AcceptVerdict.as_str().into(),
        option: id,
        label: b.option.label,
        assurance: assurance(&by.relay),
        answer: Some(h.to_string()),
        landing: None,
        relayed_by: by.relay.as_ref().map(|x| x.by.clone()),
        next: None,
    })
}
