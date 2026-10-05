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

/// The decision field naming when a decision takes effect (RFC 3339 UTC, like `recorded_at`).
pub const EFFECTIVE_FROM: &str = "effective_from";

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
    matches!(
        status(b),
        Some(DecisionStatus::Accepted | DecisionStatus::Rejected)
    )
}

/// When a decision takes effect: its `effective_from`, otherwise when it was recorded. `None` for
/// a block that is not a decision.
pub fn effective_from(b: &Block) -> Option<&str> {
    if b.block_type != DECISION {
        return None;
    }
    b.field_str(EFFECTIVE_FROM).or(b.field_str("recorded_at"))
}
