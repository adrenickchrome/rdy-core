//! Answers (plan Decision intake, O1, D-JV6Z8, D-DGCH7): the record of one human act that no
//! other record holds. A person answers an item of the "needs you" queue, or an agent relays the
//! person's words for an act the agent may not take alone (a tag, a dismissal, a view act). The
//! store keeps it as an object of kind `ANSWER`; the refs `answer/<item>` and
//! `pending/<person>/<item>` name the latest one (`rdy_store::answer`).
use crate::{ActorRun, EvidenceKind, Hash, Provenance, RelayEvidence, hash_of};
use serde::{Deserialize, Serialize};

/// The act an answer records (O1, a closed list). The declaration order is the wire format
/// (postcard's variant index): a new act goes at the end, never between two others.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerAct {
    /// The person returns a proposal version to its view.
    RejectProposal,
    /// The person hides an item until the chat session changes (a snooze, F4).
    Later,
    /// The person confirms one exception of a batch.
    Confirm,
    /// The person rejects one decision of a batch (F1).
    RejectDecision,
    /// The person answers an open question.
    AnswerQuestion,
    /// The person dismisses tensions.
    Dismiss,
    /// The person tags a landing (a relayed `rdy tag`, D-DGCH7).
    Tag,
    /// The person takes a view over.
    ViewOwn,
    /// The person closes a stale view.
    CloseStale,
    /// The person proposes a view.
    Propose,
    /// The person answers a suspect with keep.
    KeepSuspect,
    /// The person accepts a change plan's deferrals.
    AcceptDeferrals,
    /// The person refuses a change plan's deferrals.
    RefuseDeferrals,
    /// The person rejects a re-affirmation.
    RejectReaffirm,
}

impl AnswerAct {
    /// The act's name, as JSON spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            AnswerAct::RejectProposal => "reject_proposal",
            AnswerAct::Later => "later",
            AnswerAct::Confirm => "confirm",
            AnswerAct::RejectDecision => "reject_decision",
            AnswerAct::AnswerQuestion => "answer_question",
            AnswerAct::Dismiss => "dismiss",
            AnswerAct::Tag => "tag",
            AnswerAct::ViewOwn => "view_own",
            AnswerAct::CloseStale => "close_stale",
            AnswerAct::Propose => "propose",
            AnswerAct::KeepSuspect => "keep_suspect",
            AnswerAct::AcceptDeferrals => "accept_deferrals",
            AnswerAct::RefuseDeferrals => "refuse_deferrals",
            AnswerAct::RejectReaffirm => "reject_reaffirm",
        }
    }
}

/// One answer (kind `ANSWER`). The field order is the wire format (postcard): never add, remove
/// or reorder a field (D-F0904).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    /// The item ref (O3), such as `P-x@n`, `D-x@v`, `Q-x@v`, `advice:P-x@n` or `tag:<name>`.
    pub item: String,
    /// What the person did.
    pub act: AnswerAct,
    /// The id of the option chosen (O4), when the item offered options.
    pub option: Option<String>,
    /// The label of the chosen option, from rdy's own card (never the agent's words).
    pub label: Option<String>,
    /// The labels of the options shown, in order.
    pub options: Vec<String>,
    /// The hash of the card the person answered (O7, F6).
    pub card_hash: Option<Hash>,
    /// The objects the act wrote.
    pub targets: Vec<Hash>,
    /// The person, as the actor of the act.
    pub person: Provenance,
    /// The agent that relayed the person's words, if any.
    pub relayed_by: Option<ActorRun>,
    /// The person's words and where they came from, with a relay.
    pub relay_evidence: Option<RelayEvidence>,
    /// How the relay got the answer: `card` or `text`; None for a local answer.
    pub evidence_kind: Option<EvidenceKind>,
    /// The person's note: 1 to 2000 characters when present.
    pub note: Option<String>,
}

impl Answer {
    /// `Hash::of` over the answer's encoding.
    pub fn id(&self) -> Hash {
        hash_of(self)
    }
}
