//! The onboarding path (FW-CO4, D-3GF54): four phases, each aimed at coverage dimensions. A phase
//! scores the weighted mean of its dimensions' corpus scores and is done at the project's target
//! coverage. The next phase is the first one that is not done. Computed from the current state,
//! never stored; progress over time is rdyhub's (D-WDKWQ).
use super::model::{CoverageIssue, Dimension, IssueCode};
use super::score::{Rollup, rollup};
use crate::{RdyError, ReadState, Scope};
use serde::Serialize;
use std::collections::BTreeMap;

/// One phase of the onboarding path, in the order the work goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Structure,
    Entities,
    Decisions,
    Consistency,
}

impl Phase {
    pub const ALL: [Phase; 4] = [
        Phase::Structure,
        Phase::Entities,
        Phase::Decisions,
        Phase::Consistency,
    ];

    /// The dimensions the phase works on.
    pub fn dimensions(&self) -> &'static [Dimension] {
        match self {
            Phase::Structure => &[Dimension::Connected, Dimension::Complete],
            Phase::Entities => &[Dimension::Grounded],
            Phase::Decisions => &[Dimension::Governed],
            Phase::Consistency => &[Dimension::Consistent, Dimension::Current],
        }
    }

    /// The wire name.
    pub fn name(&self) -> &'static str {
        match self {
            Phase::Structure => "structure",
            Phase::Entities => "entities",
            Phase::Decisions => "decisions",
            Phase::Consistency => "consistency",
        }
    }
}

/// The state of one phase: its score, its open issues (dismissed ones left out), the open
/// issues per dimension that has any, and whether it reached the target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PhaseState {
    pub phase: Phase,
    pub dimensions: Vec<Dimension>,
    pub score: u32,
    pub issues: usize,
    pub dimension_issues: BTreeMap<Dimension, usize>,
    pub done: bool,
}

/// The onboarding path of a scope: the corpus score against the target, the four phases and the
/// next one to work (none when onboarding is done).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Onboarding {
    pub target: u32,
    pub score: u32,
    pub done: bool,
    pub next: Option<Phase>,
    pub phases: Vec<PhaseState>,
}

/// The onboarding path from a corpus `rollup`, its `issues` and the pack's dimension `weight`.
pub fn onboarding_of(
    target: u32,
    weight: impl Fn(Dimension) -> u32,
    corpus: &Rollup,
    issues: &[CoverageIssue],
) -> Onboarding {
    let phases: Vec<PhaseState> = Phase::ALL
        .iter()
        .map(|phase| {
            let (mut weighed, mut total) = (0u64, 0u64);
            let mut dimension_issues = BTreeMap::new();
            for d in phase.dimensions() {
                if let Some(s) = corpus.dimensions.get(d) {
                    let w = u64::from(weight(*d));
                    weighed += w * u64::from(*s);
                    total += w;
                }
                let n = issues
                    .iter()
                    .filter(|i| i.dimension == *d && !dismissed(i))
                    .count();
                if n > 0 {
                    dimension_issues.insert(*d, n);
                }
            }
            // no dimension present, or all weighted 0: nothing to fix
            let score = weighed.checked_div(total).map_or(100, |s| s as u32);
            PhaseState {
                phase: *phase,
                dimensions: phase.dimensions().to_vec(),
                score,
                issues: dimension_issues.values().sum(),
                dimension_issues,
                done: score >= target,
            }
        })
        .collect();
    let next = phases.iter().find(|p| !p.done).map(|p| p.phase);
    Onboarding {
        target,
        score: corpus.score,
        done: corpus.score >= target,
        next: if corpus.score >= target { None } else { next },
        phases,
    }
}

fn dismissed(i: &CoverageIssue) -> bool {
    i.dismissed.is_some() || i.code == IssueCode::DismissedTension
}

impl ReadState {
    /// The onboarding path of `scope` against the project's target. `E_BAD_INPUT` for a past
    /// landing (`coverage_scope`).
    pub fn onboarding(&self, scope: &Scope) -> Result<Onboarding, RdyError> {
        let found = self.coverage_issues(scope)?;
        let corpus = rollup(&self.pack, found.blocks.values());
        let issues: Vec<CoverageIssue> = found
            .blocks
            .values()
            .flat_map(|e| e.issues.iter().cloned())
            .collect();
        Ok(onboarding_of(
            self.coverage_target,
            |d| self.pack.coverage_weight(d),
            &corpus,
            &issues,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rollup_of(score: u32, dims: &[(Dimension, u32)]) -> Rollup {
        Rollup {
            score,
            dimensions: dims.iter().copied().collect(),
        }
    }

    fn issue(d: Dimension, code: IssueCode) -> CoverageIssue {
        let mut i = CoverageIssue::new(
            &rdy_model::BlockId::parse("c-ABCDE").unwrap(),
            code,
            None,
            "fix".into(),
        );
        i.dimension = d;
        i
    }

    #[test]
    fn cc17_the_next_phase_is_the_first_not_done() {
        let r = rollup_of(
            70,
            &[
                (Dimension::Connected, 100),
                (Dimension::Complete, 90),
                (Dimension::Grounded, 60),
                (Dimension::Governed, 10),
                (Dimension::Consistent, 0),
            ],
        );
        let o = onboarding_of(80, |_| 1, &r, &[]);
        assert_eq!(o.next, Some(Phase::Entities));
        assert!(!o.done);
        let done: Vec<bool> = o.phases.iter().map(|p| p.done).collect();
        assert_eq!(done, [true, false, false, false]);
        assert_eq!(o.phases[0].score, 95);
    }

    #[test]
    fn cc17_onboarding_is_done_at_the_target() {
        let r = rollup_of(80, &[(Dimension::Grounded, 10)]);
        let o = onboarding_of(80, |_| 1, &r, &[]);
        assert!(o.done);
        assert_eq!(o.next, None);
        let o = onboarding_of(81, |_| 1, &r, &[]);
        assert!(!o.done);
        assert_eq!(o.next, Some(Phase::Entities));
    }

    #[test]
    fn cc17_a_phase_with_no_present_dimension_is_100() {
        let r = rollup_of(50, &[(Dimension::Governed, 50)]);
        let o = onboarding_of(80, |_| 1, &r, &[]);
        assert_eq!(o.phases[0].score, 100);
        assert_eq!(o.phases[1].score, 100);
        assert_eq!(o.phases[3].score, 100);
        assert_eq!(o.next, Some(Phase::Decisions));
    }

    #[test]
    fn cc17_a_phase_scores_the_weighted_mean_and_a_zero_weight_leaves_a_dimension_out() {
        let r = rollup_of(0, &[(Dimension::Connected, 100), (Dimension::Complete, 40)]);
        let w = |d: Dimension| if d == Dimension::Connected { 3 } else { 1 };
        assert_eq!(onboarding_of(80, w, &r, &[]).phases[0].score, 85);
        let zero = |d: Dimension| u32::from(d != Dimension::Complete);
        assert_eq!(onboarding_of(80, zero, &r, &[]).phases[0].score, 100);
        assert_eq!(onboarding_of(80, |_| 0, &r, &[]).phases[0].score, 100);
    }

    #[test]
    fn cc17_issues_count_per_phase_and_leave_dismissed_ones_out() {
        let issues = [
            issue(Dimension::Grounded, IssueCode::UnlinkedMention),
            issue(Dimension::Grounded, IssueCode::UnlinkedMention),
            issue(Dimension::Consistent, IssueCode::OpenTension),
            issue(Dimension::Consistent, IssueCode::DismissedTension),
            issue(Dimension::Current, IssueCode::OpenSuspect),
        ];
        let o = onboarding_of(80, |_| 1, &rollup_of(100, &[]), &issues);
        let n: Vec<usize> = o.phases.iter().map(|p| p.issues).collect();
        assert_eq!(n, [0, 2, 0, 2]);
        assert_eq!(o.phases[1].dimension_issues[&Dimension::Grounded], 2);
    }
}
