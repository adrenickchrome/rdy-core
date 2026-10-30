//! Agent checks (D-JQD5X, D-MQR17): the fixed catalogue of eight authoring checks and the case
//! each one gives a reviewer. A case is read data: this module writes no record, dispatches no
//! agent and runs nothing a decision or a file says. Agent checks belong to no stage (FW-G7b).
pub mod case;
pub mod catalogue;
pub mod code;
mod recipes;

pub use case::{BuiltCase, CaseInputs, build_case, contradiction_cases};
pub use catalogue::{AGENT_CHECKS, AgentCheck, CheckSpec};
pub use code::{parse_code_range, resolve_code_range};
