//! Presented answers (D-6M0AF, D-WPE4R, D-QVH4N; TASK-982 §1, O2, O9, O11, C7): a card the
//! person answers in the local approver UI ([`Presentation::Ui`]), or one an MCP client elicited
//! from them ([`Presentation::Elicited`]).
//!
//! Both go through the answer engine's own binding and dispatch, as the person: the answer runs
//! as the person's local approver session, with no relay, and records evidence `card` (the UI) or
//! `elicited`, with the hash of the card shown. The binding checks the item, its required role,
//! the option and the full card hash, and each act keeps its holds and refusals. The session that
//! presented the card is kept apart: it asks for signatures, gets the consequences, and names the
//! chat session of an elicited Later.
//!
//! None of these types reads input: no CLI flag, ctl argument or MCP input can name a
//! presentation. Only the UI and elicitation build one.
//!
//! - A Later in the UI is answered after a fresh binding, and stores nothing (F2): the reply is
//!   `{"state": "later"}`. An elicited Later is a snooze of the caller's run.
//! - An advice acceptance keeps its plain record (F3): the person's tension versions, no
//!   `ANSWER`, and `local` in the reply.
//! - An answer to a question cites the briefing the presentation showed
//!   ([`presented_question_brief`]): rdy never fetches a token during the answer.
use super::{
    AnswerStep, Answerer, Located, Pick, locate, next_of, note_of, question, session_ref_of,
};
use crate::brief::{BriefRegistry, Briefing};
use crate::engine::Engine;
use crate::error::RdyError;
use crate::pending::reaffirm::REAFFIRM_REF_PREFIX;
use crate::pending::{Item, ItemKind};
use crate::policy::roles::{holds, outside_role};
use crate::signing_crypto::SigningAgent;
use crate::{Rdy, ReadState, Scope, Session};
use rdy_model::{EvidenceKind, Role};
use std::collections::BTreeSet;

/// Where the person was shown the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presentation {
    /// The local approver UI: evidence `card`.
    Ui,
    /// An MCP client's elicitation: evidence `elicited`.
    Elicited,
}

impl Presentation {
    /// The evidence kind the records of this source name.
    pub(crate) fn evidence(self) -> EvidenceKind {
        match self {
            Presentation::Ui => EvidenceKind::Card,
            Presentation::Elicited => EvidenceKind::Elicited,
        }
    }
}

/// The person's answer to a presented card: the item ref and the full hash of the card shown,
/// the option chosen, the person's note, and the briefing token the presentation showed (only an
/// answer to a question under stable content reads it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresentedAnswer {
    pub reference: String,
    pub card_hash: String,
    pub option: String,
    pub note: Option<String>,
    pub brief: Option<String>,
}

/// The source of a presented answer and the session that presented the card.
#[derive(Clone)]
pub(crate) struct Presenter {
    pub source: Presentation,
    pub caller: Session,
}

/// What the first half of a presented answer did.
pub(crate) enum PresentedStep {
    /// A Later in the UI, bound to the card shown: nothing is stored.
    Later,
    /// Any other answer, as [`AnswerStep`] gives it.
    Step(AnswerStep),
}

impl Rdy {
    /// A presented answer up to its write (see the module docs): every act but an approval is
    /// written (with its `next`); an approval is prepared and not landed.
    pub(crate) fn presented_step(
        &mut self,
        caller: &Session,
        person: &str,
        source: Presentation,
        req: PresentedAnswer,
    ) -> Result<PresentedStep, RdyError> {
        // the person is resolved before anything else
        let who = Session::human(person.to_string(), Role::Approver)?;
        let by = Answerer {
            who: who.clone(),
            relay: None,
            kind: Some(source.evidence()),
            from: who,
            brief: req.brief,
            presented: Some(Presenter {
                source,
                caller: caller.clone(),
            }),
        };
        // a re-affirmation card checks its own note: a reason for each option (D-CQMA4)
        let note = if req.reference.starts_with(REAFFIRM_REF_PREFIX) {
            req.note
        } else {
            note_of(req.note)?
        };
        let bound = self.bound(
            &by,
            person,
            &req.reference,
            Pick::Id(&req.option),
            Some(&req.card_hash),
        )?;
        // F2: a Later in the UI leaves the item open and stores nothing
        if source == Presentation::Ui && bound.option.id == "later" {
            return Ok(PresentedStep::Later);
        }
        Ok(PresentedStep::Step(
            match self.dispatch(&by, person, bound, note)? {
                AnswerStep::Done(mut info) => {
                    info.next = next_of(&self.read(), person, &by);
                    AnswerStep::Done(info)
                }
                AnswerStep::Approval(mut prepared) => {
                    prepared.next_session = Some(session_ref_of(&by));
                    AnswerStep::Approval(prepared)
                }
            },
        ))
    }
}

/// The item `reference` names in `person`'s complete eligible queue, read before the display
/// cap and never by title (O11). With `shown`, the card of the ref with that full hash. It keeps
/// the refusals of an answer's binding: a ref outside the grammar, an item that is not in the
/// queue (`E_ITEM_MOVED`, `E_PROPOSAL_MOVED`, `E_PROPOSAL_NOT_READY` or `E_VIEW_CLOSED`, with the
/// fresh card), a hash that is no card of the ref, and a person outside the item's required
/// policy role (`E_PERMISSION`). It reads only.
pub fn presented_item(
    st: &ReadState,
    person: &str,
    reference: &str,
    shown: Option<&str>,
) -> Result<Item, RdyError> {
    match locate(st, person, reference, shown, false, false)? {
        Located::Found(found) => {
            if let Some(role) = &found.role
                && !holds(role, person)
            {
                return Err(outside_role(person, role, &found.item.r#ref));
            }
            Ok(found.item)
        }
        Located::ShownMoved(e) => Err(e),
    }
}

/// The briefing a presentation shows with a question's card before the person answers
/// (TASK-982 §1): the briefing of the decision's destination, by the rules the answer writes it
/// under (`decisions.home`, else the question's parent in canon), in `person`'s answer view, or
/// canon when that view does not exist yet. The briefing is registered in `registry`, so its
/// token is citable. `None` for an item that is not a question, and for a destination at the
/// project root. The item, its card and its hash are not changed.
pub fn presented_question_brief(
    st: &ReadState,
    registry: &BriefRegistry,
    person: &Session,
    item: &Item,
) -> Result<Option<Briefing>, RdyError> {
    if item.kind != ItemKind::Answer {
        return Ok(None);
    }
    let Some((qid, _)) = question::question_ref(&item.r#ref) else {
        return Ok(None);
    };
    let Some(destination) = question::destination(st, person, &qid)? else {
        return Ok(None);
    };
    let view = person.default_view();
    let scope = if st.has_view(&view) {
        Scope::View(view)
    } else {
        Scope::Canon
    };
    let id = st.resolve_id(&scope, &destination)?;
    let briefing = st.brief_ids(&scope, &BTreeSet::from([id]), None)?;
    briefing.register(registry);
    Ok(Some(briefing))
}

/// Answers a presented card for `person` (the project's human actor), as `caller` presented it
/// from `source` (see the module docs), through the signed-act protocol: an approval signs as
/// the settings say, with the person's key for an elicited answer and never through an agent in
/// the UI. The reply is the answer's, with `signatures` on an approval and the consequences
/// attached; a UI Later's is `{"state": "later"}`.
pub fn run_presented_answer(
    engine: &dyn Engine,
    agent: &dyn SigningAgent,
    caller: &Session,
    person: &str,
    source: Presentation,
    request: PresentedAnswer,
) -> Result<serde_json::Value, RdyError> {
    crate::signing_act::run_presented(engine, agent, caller, person, source, request)
}
