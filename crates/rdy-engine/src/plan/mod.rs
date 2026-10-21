//! Change plans (D-4JMPH): the items a decision change owes, computed from a version's final
//! state against its canon baseline. Nothing here writes a plan; the store keeps plan records.
pub mod compute;

pub use compute::{ComputedPlan, PlanCandidate, allowed_acts};
