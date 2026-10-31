//! The stage registry (D-JQD5X): one list of the deterministic checks, each tagged with the
//! stages it runs at. The registry names and routes checks that already exist; it runs none of
//! them itself, so nothing here can run a check twice.
//!
//! - `change`, on every write and view op: the tension delta and the rule checks on the changed
//!   blocks and the blocks one link away. It never refuses.
//! - `propose`, when a version is made: readiness (D-ME2HD), with blocking rule checks in its
//!   item 6. Structural validation, justification, plans, tensions and coverage are inside it.
//! - `land`, after a landing: suspects, events and rebases. It never refuses.
//!
//! Agent checks are in no stage (FW-G7b). [`DeterministicCheck`] is a closed enum with no
//! variant for an agent check and no conversion from [`crate::check::AgentCheck`], so no caller
//! can register or dispatch one through a stage.
use serde::Serialize;

/// When a deterministic check runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Change,
    Propose,
    Land,
}

impl Stage {
    /// The stage's one name.
    pub fn name(self) -> &'static str {
        match self {
            Stage::Change => "change",
            Stage::Propose => "propose",
            Stage::Land => "land",
        }
    }
}

/// The deterministic checks, in registry order. No variant is an agent check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeterministicCheck {
    TensionDelta,
    PackRules,
    Readiness,
    Suspects,
    Events,
    Rebases,
}

/// One registry row: the check's name and its stages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct StageCheck {
    pub name: &'static str,
    pub stages: &'static [Stage],
}

/// The registry, in [`DeterministicCheck::ALL`] order.
pub const STAGE_CHECKS: [StageCheck; 6] = [
    StageCheck {
        name: "tension_delta",
        stages: &[Stage::Change],
    },
    StageCheck {
        name: "pack_rules",
        stages: &[Stage::Change, Stage::Propose],
    },
    StageCheck {
        name: "readiness",
        stages: &[Stage::Propose],
    },
    StageCheck {
        name: "suspects",
        stages: &[Stage::Land],
    },
    StageCheck {
        name: "events",
        stages: &[Stage::Land],
    },
    StageCheck {
        name: "rebases",
        stages: &[Stage::Land],
    },
];

impl DeterministicCheck {
    /// Every deterministic check, in registry order.
    pub const ALL: [DeterministicCheck; 6] = [
        DeterministicCheck::TensionDelta,
        DeterministicCheck::PackRules,
        DeterministicCheck::Readiness,
        DeterministicCheck::Suspects,
        DeterministicCheck::Events,
        DeterministicCheck::Rebases,
    ];

    /// The check's registry row.
    pub fn row(self) -> &'static StageCheck {
        let at = match self {
            DeterministicCheck::TensionDelta => 0,
            DeterministicCheck::PackRules => 1,
            DeterministicCheck::Readiness => 2,
            DeterministicCheck::Suspects => 3,
            DeterministicCheck::Events => 4,
            DeterministicCheck::Rebases => 5,
        };
        &STAGE_CHECKS[at]
    }

    /// The checks that run at `stage`, in registry order.
    pub fn at(stage: Stage) -> impl Iterator<Item = DeterministicCheck> {
        DeterministicCheck::ALL
            .into_iter()
            .filter(move |c| c.row().stages.contains(&stage))
    }
}
