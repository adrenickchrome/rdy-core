//! The answer to an open question (tb-Q970N, F7): the person's ruling, recorded as a proposed
//! decision that `answers` the question, then an `ANSWER` that names the decision's change.
//!
//! # The decision (closed)
//!
//! - Its `statement` is the chosen option's label (`o1` to `o4`), or, for a free answer (`o0`),
//!   the person's note. A free answer needs a note.
//! - It `answers` the question, with `authority_basis: direct_ruling`.
//! - `attested_by` is `<person> (relayed by <agent>: "<quote>")` for a relay, else `<person>`.
//! - It is written by the session that gave the answer, in that session's own view: the relaying
//!   agent's view, or the person's `work` at the command line. No role is needed to answer (O12):
//!   a reader session writes it as a writer.
//! - Its parent is the doc `decisions.home` names when the project sets one, else the
//!   question's parent.
//!
//! The decision is `proposed`; it lands with the view's next proposal. A write the view refuses
//! refuses the answer, and nothing is written.
//!
//! # Checks before the write (closed)
//!
//! - The view holds the version of the question the person answered. The card shows canon's
//!   version (the ref's version short); a view that rewrote the question, or whose base is
//!   older, holds another one, and landing the view would close a question the person never saw.
//!   `E_ITEM_MOVED`, with the card in `corrected` (O21), and nothing is written. A view that
//!   does not exist yet starts from canon.
//! - A decision under stable content cites a briefing (bwr1), as any write there does: the
//!   answer's `brief`. Without one, or with a stale one, the refusal's fix names the retry:
//!   brief the blocks the refusal names, then answer again with the token.
//!
//! # Two batches (O14)
//!
//! The decision's commit already names the relay in `attested_by`. Its `ANSWER` (act
//! `answer_question`, `targets` = the decision's change) follows in a batch of its own. A crash
//! between them leaves the question open, so the agent asks again.
use super::{AnswerInfo, Answerer, Bound, assurance, card_of, labels, put_alone};
use crate::brief::{E_BRIEF_REQUIRED, E_BRIEF_STALE};
use crate::error::{E_BAD_INPUT, E_ITEM_MOVED, RdyError};
use crate::pending::ItemOption;
use crate::{DecisionInput, Rdy, Scope, Session};
use rdy_model::{Answer, AnswerAct, BlockId, Hash, Role};

/// The option id of a free answer: the person's own words, in the note.
pub const FREE: &str = "o0";

/// The option a free answer binds to. The card does not show it, so it has no label.
pub(crate) fn free() -> ItemOption {
    ItemOption {
        id: FREE.to_string(),
        label: String::new(),
        consequences: vec![],
        recommended: None,
    }
}

/// answer · `o1`…`o4` or `o0`: the decision, then the `ANSWER` (see the module docs).
pub(crate) fn act(
    r: &mut Rdy,
    by: &Answerer,
    person: &str,
    b: Bound,
    note: Option<String>,
) -> Result<AnswerInfo, RdyError> {
    let free = b.option.id == FREE;
    let statement = if free {
        note.clone().ok_or_else(|| {
            RdyError::new(
                E_BAD_INPUT,
                "a free answer (o0) needs a note: the person's own answer",
                "pass the person's answer as note, or choose one of the card's options",
            )
        })?
    } else {
        b.option.label.clone()
    };
    let (qid, shown) = b
        .item
        .r#ref
        .split_once('@')
        .and_then(|(id, v)| Some((BlockId::parse(id).ok()?, v.to_string())))
        .ok_or_else(|| {
            RdyError::new(
                E_BAD_INPUT,
                format!("{} does not name a question", b.item.r#ref),
                "pass the question's ref as rdy_pending gives it",
            )
        })?;
    // no role is needed to answer (O12): a reader writes its answer's decision as a writer
    let writer = Session {
        role: if by.from.role.can_write() {
            by.from.role
        } else {
            Role::Writer
        },
        ..by.from.clone()
    };
    let view = writer.default_view();
    let st = r.read();
    if st.has_view(&view) {
        let held = st
            .block(&Scope::View(view.clone()), &qid)
            .ok()
            .map(|q| q.version_hash().short());
        if held.as_deref() != Some(shown.as_str()) {
            return Err(RdyError::new(
                E_ITEM_MOVED,
                format!(
                    "{qid} in view {view} is not the version the person answered ({})",
                    b.item.r#ref
                ),
                format!(
                    "land or drop view {view}'s edit of {qid}, or rebase the view, then show the person the card again"
                ),
            )
            .with_corrected(card_of(&b.item)));
        }
    }
    let given = match st.decisions_home() {
        Some(_) => None,
        None => st.block(&Scope::Canon, &qid)?.parent.map(|p| p.to_string()),
    };
    drop(st);
    let parent = r.decision_parent(&view, given.as_deref())?;
    let attested_by = match &by.relay {
        Some(relay) => {
            let agent = crate::identity::actor_run_text(&relay.by.actor, relay.by.run.as_deref());
            format!(
                "{person} (relayed by {agent}: \"{}\")",
                relay.evidence.quoted
            )
        }
        None => person.to_string(),
    };
    let w = r
        .decide(
            &writer,
            &view,
            DecisionInput {
                parent: parent.clone(),
                statement: statement.clone(),
                answers: vec![qid.to_string()],
                authority_basis: Some("direct_ruling".into()),
                attested_by: Some(attested_by),
                brief: by.brief.clone(),
                ..Default::default()
            },
        )
        .map_err(|e| briefing_retry(e, &parent))?;
    let change: Hash = w.change.parse().map_err(|_| {
        RdyError::new(
            crate::error::E_INTERNAL,
            format!("the decision's change {} is not a hash", w.change),
            "report this as a bug in rdy",
        )
    })?;
    let a = Answer {
        item: b.item.r#ref.clone(),
        act: AnswerAct::AnswerQuestion,
        option: Some(b.option.id.clone()),
        // a free answer has no label on the card; the note holds the words
        label: (!free).then(|| b.option.label.clone()),
        options: labels(&b.item),
        card_hash: b.card_hash,
        targets: vec![change],
        person: by.who.provenance(),
        relayed_by: by.relay.as_ref().map(|r| r.by.clone()),
        relay_evidence: by.relay.as_ref().map(|r| r.evidence.clone()),
        evidence_kind: by.relay.as_ref().and(by.kind),
        note,
    };
    let h = put_alone(r, &a)?;
    Ok(AnswerInfo {
        r#ref: b.item.r#ref,
        act: AnswerAct::AnswerQuestion.as_str().into(),
        option: b.option.id,
        label: if free { statement } else { b.option.label },
        assurance: assurance(&by.relay),
        answer: Some(h.to_string()),
        landing: None,
        relayed_by: by.relay.as_ref().map(|r| r.by.clone()),
        next: None,
    })
}

/// A briefing refusal of the answer's write, with the fix that names the answer's retry: brief
/// the blocks the refusal names (`briefing.ids`; the parent when it names none), then answer
/// again with the token. Any other error comes back unchanged.
fn briefing_retry(mut e: RdyError, parent: &str) -> RdyError {
    if e.code != E_BRIEF_REQUIRED && e.code != E_BRIEF_STALE {
        return e;
    }
    let ids: Vec<&str> = e
        .briefing
        .as_ref()
        .and_then(|b| b.get("ids"))
        .and_then(|ids| ids.as_array())
        .map(|ids| ids.iter().filter_map(|i| i.as_str()).collect())
        .unwrap_or_default();
    let on = if ids.is_empty() {
        parent.to_string()
    } else {
        ids.join(", ")
    };
    e.fix = format!("call rdy_brief on {on}, then rdy_answer again with brief: <token>");
    e
}
