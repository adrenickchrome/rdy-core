//! The answers to tensions (D-WMCFW, D-562QR, D-67ZN7): the `resolve` items of the queue. The
//! dispatch is closed:
//!
//! | item                  | option    | act                                                     |
//! |-----------------------|-----------|---------------------------------------------------------|
//! | `advice:P-x@n`        | `accept`  | `accept_advice` with the card's advised digest; no `ANSWER` |
//! | `advice:P-x@n`        | `dismiss` | one dismissal per vkey of the advised tensions, and one `ANSWER` |
//! | a blocking tension id | `dismiss` | one dismissal, and one `ANSWER`                         |
//!
//! Later is in [`super::batch`]. Any other pair is refused with `E_BAD_INPUT`.
//!
//! An acceptance writes the semantic tension versions, which record the person and the relay;
//! it keeps no note. A dismissal needs the person's reason as the note. It runs as the person
//! (D-562QR: a relay with the person's words counts as the person), so the reviewer-or-human rule
//! holds and the self-review rule checks the person against every side's author. Every tension
//! passes those checks before anything is written. A dismissal binds to a vkey (FW-CH7), so
//! advised tensions that share one (two changes that write the same content) get one review.
//! The reviews and the `ANSWER` (act `dismiss`, `targets` = the reviews) are one batch (O14).
use super::{AnswerInfo, Answerer, Bound, assurance, labels};
use crate::error::{E_BAD_INPUT, RdyError};
use crate::gate::resolve_in;
use crate::pending::Item;
use crate::session::now_ms;
use crate::tensions::Tension;
use crate::tensions::api::AcceptRequest;
use crate::{Rdy, ReadState};
use rdy_model::{Answer, AnswerAct};
use std::sync::Arc;

/// The prefix of an advice item's ref.
const ADVICE: &str = "advice:";

/// resolve · `accept` or `dismiss` (see the module docs).
pub(crate) fn act(
    r: &mut Rdy,
    by: &Answerer,
    b: Bound,
    note: Option<String>,
) -> Result<AnswerInfo, RdyError> {
    let advice = b.item.r#ref.strip_prefix(ADVICE).map(str::to_string);
    match (advice, b.option.id.as_str()) {
        (Some(pref), "accept") => accept(r, by, b, &pref, note),
        (advice, "dismiss") => dismiss(r, by, b, advice.as_deref(), note),
        (_, option) => Err(RdyError::new(
            E_BAD_INPUT,
            format!("{option} is not an option of {}", b.item.r#ref),
            "answer with one of the card's options",
        )),
    }
}

/// The value of `item`'s fact `name`.
fn fact<'a>(item: &'a Item, name: &str) -> Option<&'a str> {
    item.facts
        .iter()
        .find(|f| f.name == name)
        .map(|f| f.value.as_str())
}

/// advice · `accept`: the acceptance, bound to the advised set the card showed.
fn accept(
    r: &mut Rdy,
    by: &Answerer,
    b: Bound,
    pref: &str,
    note: Option<String>,
) -> Result<AnswerInfo, RdyError> {
    if note.is_some() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "an acceptance keeps no note",
            "drop the note; the tension versions record the acceptance",
        ));
    }
    let digest = fact(&b.item, "Advised digest").unwrap_or_default();
    r.accept_as(
        &by.who,
        by.relay.clone(),
        AcceptRequest {
            proposal: pref.to_string(),
            advised_digest: digest.to_string(),
            ..Default::default()
        },
    )?;
    Ok(AnswerInfo {
        r#ref: b.item.r#ref,
        act: "accept".into(),
        option: b.option.id,
        label: b.option.label,
        assurance: assurance(&by.relay),
        answer: None,
        landing: None,
        relayed_by: by.relay.as_ref().map(|r| r.by.clone()),
        next: None,
    })
}

/// The tensions a dismissal of `r#ref` covers: every advised tension of the version `pref` that
/// no one accepted (the set the advice card lists), by id; or the one tension `r#ref` names.
fn covered(st: &ReadState, r#ref: &str, pref: Option<&str>) -> Result<Vec<Arc<Tension>>, RdyError> {
    match pref {
        Some(pref) => {
            let pref = resolve_in(st, pref)?;
            let p = st.reader().proposal(&pref.version)?;
            let mut out: Vec<Arc<Tension>> = st
                .advised_of(&p.view, &p.changes)?
                .into_iter()
                .filter(|a| !a.open.record.blocking)
                .map(|a| a.tension)
                .collect();
            out.sort_by_key(|t| t.id);
            Ok(out)
        }
        None => Ok(vec![st.resolve_tension(r#ref, now_ms())?]),
    }
}

/// `dismiss`: the dismissals and the `ANSWER`, in one batch, as the person.
fn dismiss(
    r: &mut Rdy,
    by: &Answerer,
    b: Bound,
    pref: Option<&str>,
    note: Option<String>,
) -> Result<AnswerInfo, RdyError> {
    let Some(reason) = note else {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "a dismissal needs a note: the person's reason",
            "ask the person why, then pass their words as note",
        ));
    };
    let tensions = covered(&r.read(), &b.item.r#ref, pref)?;
    let a = Answer {
        item: b.item.r#ref.clone(),
        act: AnswerAct::Dismiss,
        option: Some(b.option.id.clone()),
        label: Some(b.option.label.clone()),
        options: labels(&b.item),
        card_hash: b.card_hash,
        targets: vec![],
        person: by.who.provenance(),
        relayed_by: by.relay.as_ref().map(|r| r.by.clone()),
        relay_evidence: by.relay.as_ref().map(|r| r.evidence.clone()),
        evidence_kind: by.relay.as_ref().and(by.kind),
        note: Some(reason.clone()),
    };
    let (_, h) = r.dismiss_answered(&by.who, &tensions, &reason, a)?;
    Ok(AnswerInfo {
        r#ref: b.item.r#ref,
        act: AnswerAct::Dismiss.as_str().into(),
        option: b.option.id,
        label: b.option.label,
        assurance: assurance(&by.relay),
        answer: Some(h.to_string()),
        landing: None,
        relayed_by: by.relay.as_ref().map(|r| r.by.clone()),
        next: None,
    })
}
