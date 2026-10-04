use crate::Block;

pub const DECISION: &str = "decision";
pub const QUESTION: &str = "question";

pub const SUPERSEDES: &str = "supersedes";
pub const REFINES: &str = "refines";
pub const DEPENDS_ON: &str = "depends_on";
pub const CONDITIONAL_ON: &str = "conditional_on";
pub const REVOKES: &str = "revokes";
pub const ANSWERS: &str = "answers";
pub const DECIDED_BY: &str = "decided_by";
pub const IMPLEMENTS: &str = "implements";
pub const DESCRIBES: &str = "describes";
pub const AFFECTS: &str = "affects";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionStatus {
    Proposed,
    Accepted,
    Rejected,
}

pub fn status(b: &Block) -> Option<DecisionStatus> {
    if b.block_type != DECISION {
        return None;
    }
    match b.field_str("status")? {
        "proposed" => Some(DecisionStatus::Proposed),
        "accepted" => Some(DecisionStatus::Accepted),
        "rejected" => Some(DecisionStatus::Rejected),
        _ => None,
    }
}

pub fn is_frozen(b: &Block) -> bool {
    matches!(status(b), Some(DecisionStatus::Accepted | DecisionStatus::Rejected))
}
