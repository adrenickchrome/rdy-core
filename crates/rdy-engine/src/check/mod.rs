//! Checks (D-JQD5X). Agent checks (D-MQR17): the fixed catalogue of eight authoring checks and the
//! case each one gives a reviewer. A case is read data: this module writes no record, dispatches
//! no agent and runs nothing a decision or a file says. Agent checks belong to no stage (FW-G7b).
//! Deterministic checks: the stage registry ([`stages`]), pack rules ([`rules`]) and the landing
//! projection they judge ([`options`]). Verdicts reach a version only as live lines on its
//! approval card and as the holds a person accepts there ([`verdicts`]).
pub mod api;
pub mod case;
pub mod catalogue;
pub mod code;
pub mod options;
mod recipes;
pub mod requests;
pub mod rules;
pub mod stages;
pub mod submit;
pub mod verdicts;

pub use case::{BuiltCase, CaseInputs, build_case, contradiction_cases};
pub use catalogue::{AGENT_CHECKS, AgentCheck, CheckSpec};
pub use code::{parse_code_range, resolve_code_range};
pub use options::{ConsideredOptions, LandingProjection, considered_options, landing_projection};
pub use rules::{RuleIssue, RuleScan, evaluate_rules};
pub use stages::{DeterministicCheck, STAGE_CHECKS, Stage, StageCheck};
pub use verdicts::{
    MatchingVerdict, accepted_verdict_issues, any_check_request_for_tests, matching_verdicts,
};
