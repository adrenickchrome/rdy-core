//! The catalogue of agent checks: a fixed list of data, in a fixed order. It holds no stage and
//! no callback, so no registry can run an agent check by itself (FW-G7b).
use crate::semantic::case::CASE_QUESTION;

/// The eight agent checks, in catalogue order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AgentCheck {
    Contradiction,
    MeaningPreservation,
    ImplementationFidelity,
    DecisionReview,
    EntityDivergence,
    ProseDrift,
    DiffContradicts,
    Confirmation,
}

/// What the catalogue says about one check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckSpec {
    /// The check's one name: names parse by exact equality.
    pub name: &'static str,
    /// The subject kind, as a refusal words it.
    pub subject: &'static str,
    pub question: &'static str,
    pub verdicts: &'static [&'static str],
}

const TENSION_VERDICTS: &[&str] = &["tension", "no_tension"];
const CHECK_VERDICTS: &[&str] = &["pass", "concern", "fail"];

/// The catalogue, in [`AgentCheck::ALL`] order.
pub const AGENT_CHECKS: [CheckSpec; 8] = [
    CheckSpec {
        name: "contradiction",
        subject: "two blocks or a decision",
        question: CASE_QUESTION,
        verdicts: TENSION_VERDICTS,
    },
    CheckSpec {
        name: "meaning_preservation",
        subject: "a change or proposal version with editorial blocks",
        question: "Does each edit keep the meaning of the earlier text? Cite each changed meaning.",
        verdicts: CHECK_VERDICTS,
    },
    CheckSpec {
        name: "implementation_fidelity",
        subject: "a change or proposal version with implements",
        question: "Does each changed block follow the decision it implements? Cite each difference.",
        verdicts: CHECK_VERDICTS,
    },
    CheckSpec {
        name: "decision_review",
        subject: "a proposed decision",
        question: "Do the decision, its options, basis and premises support one another? Cite each concern.",
        verdicts: CHECK_VERDICTS,
    },
    CheckSpec {
        name: "entity_divergence",
        subject: "an entity",
        question: "Do these blocks describe the same entity consistently? Cite each difference.",
        verdicts: CHECK_VERDICTS,
    },
    CheckSpec {
        name: "prose_drift",
        subject: "a decision or proposal version that supersedes, revokes or refines one",
        question: "Does the unchanged prose still follow the decision? Cite each stale statement.",
        verdicts: CHECK_VERDICTS,
    },
    CheckSpec {
        name: "diff_contradicts",
        subject: "a decision and a code range",
        question: "Does this code diff contradict the decision? Cite the decision and name each file and line in the note.",
        verdicts: CHECK_VERDICTS,
    },
    CheckSpec {
        name: "confirmation",
        subject: "a decision with confirmation text",
        question: "Do the shown implementation and files meet the decision's confirmation text? Cite each unmet part.",
        verdicts: CHECK_VERDICTS,
    },
];

impl AgentCheck {
    /// Every check, in catalogue order.
    pub const ALL: [AgentCheck; 8] = [
        AgentCheck::Contradiction,
        AgentCheck::MeaningPreservation,
        AgentCheck::ImplementationFidelity,
        AgentCheck::DecisionReview,
        AgentCheck::EntityDivergence,
        AgentCheck::ProseDrift,
        AgentCheck::DiffContradicts,
        AgentCheck::Confirmation,
    ];

    /// The check's catalogue row.
    pub fn spec(self) -> &'static CheckSpec {
        let at = match self {
            AgentCheck::Contradiction => 0,
            AgentCheck::MeaningPreservation => 1,
            AgentCheck::ImplementationFidelity => 2,
            AgentCheck::DecisionReview => 3,
            AgentCheck::EntityDivergence => 4,
            AgentCheck::ProseDrift => 5,
            AgentCheck::DiffContradicts => 6,
            AgentCheck::Confirmation => 7,
        };
        &AGENT_CHECKS[at]
    }

    /// The check named exactly `name`. No display name is an alias.
    pub fn parse(name: &str) -> Option<AgentCheck> {
        AgentCheck::ALL.into_iter().find(|c| c.spec().name == name)
    }
}
