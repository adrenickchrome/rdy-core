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
    /// The person disputes a landing (appended: index 14). Item `dispute:<landing hex>`; its
    /// one target is the DISPUTE. Carried only: no writer yet.
    Dispute,
    /// The person resolves a dispute (index 15). Item `resolve:<dispute hex>`; its one target
    /// is the DISPUTE_RESOLUTION. Carried only: no writer yet.
    ResolveDispute,
    /// The person accepts a concern or fail check verdict (index 16). Item
    /// `verdict:<review hex>`; no targets. Carried only: no writer yet.
    AcceptVerdict,
    /// The person adopts a delivered change of cited content (repo links, index 17). Item
    /// `adopt:<repo>@<entry>`. Its answer takes ANSWER encoding 1
    /// ([`crate::encoding::ANSWER_FOREIGN_ENCODING`]). The adopt landing writes it in its batch,
    /// with the landing as its one target, or alone when the card moves no pin.
    Adopt,
    /// The person keeps the approved pins of an adopt item (index 18), with a reason. ANSWER
    /// encoding 1, as [`AnswerAct::Adopt`]. Written alone, with the reason as its note.
    KeepPin,
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
            AnswerAct::Dispute => "dispute",
            AnswerAct::ResolveDispute => "resolve_dispute",
            AnswerAct::AcceptVerdict => "accept_verdict",
            AnswerAct::Adopt => "adopt",
            AnswerAct::KeepPin => "keep_pin",
        }
    }

    /// Whether the act is one of repo links' acts ([`AnswerAct::Adopt`], [`AnswerAct::KeepPin`]):
    /// an answer with one takes ANSWER encoding 1, which ANSWER encoding 0 has no form for.
    pub fn is_foreign(self) -> bool {
        matches!(self, AnswerAct::Adopt | AnswerAct::KeepPin)
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
    /// How the answer arrived: a relay records `card` or `text`; a local answer records none,
    /// `card` (the approver UI) or `elicited` (an MCP client asked the person).
    pub evidence_kind: Option<EvidenceKind>,
    /// The person's note: 1 to 2000 characters when present.
    pub note: Option<String>,
}

impl Answer {
    /// The answer's identity: the hash of its stored frame in the encoding it is written in
    /// ([`Answer::encoding`]). For an answer of encoding 0 that is `Hash::of` over its body, as
    /// before encodings; an Adopt or KeepPin answer's hash covers the version prefix too.
    pub fn id(&self) -> Hash {
        match self.encoding() {
            0 => hash_of(self),
            v => crate::encoding::encoded_hash(
                v,
                &postcard::to_allocvec(self)
                    .expect("postcard encoding of an in-memory value cannot fail"),
            ),
        }
    }

    /// The encoding the answer is written in: [`crate::encoding::ANSWER_FOREIGN_ENCODING`] for a
    /// repo-links act ([`AnswerAct::is_foreign`]), else 0.
    pub fn encoding(&self) -> u16 {
        if self.act.is_foreign() {
            crate::encoding::ANSWER_FOREIGN_ENCODING
        } else {
            0
        }
    }
}
