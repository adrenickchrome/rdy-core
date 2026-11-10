//! The shared foreign readiness facts (repo links, D-Y49PX, D-8VJAZ): what readiness and the
//! adapter's `foreign_tension_gate` read of the tensions that have a side cited from a linked
//! project.
//!
//! [`foreign_tension_gate`] is the one pure collector. It reads tension rows already built and
//! the author's answers, and nothing else: no store, no clock. A fact is one foreign side of one
//! open tension: the tension, its kind and severity, the cited repo, the FOREIGN entry that
//! delivered the side, the cited block with the version the side holds, the local blocks of the
//! tension, and the author's answer to its plan item. A dismissed tension and a tension a live
//! deviation resolves give no fact. A cited block is no local entity: no fact counts toward
//! coverage and none enters a local reach.
//!
//! [`assessment_foreign`] is the foreign part an assessment records (ASSESSMENT encoding 2): the
//! held version of each pin a written block holds, the cited side of each foreign fact, and the
//! foreign sides of the version's tension rows. Each list is sorted, distinct and within the
//! row bound.
use crate::ReadState;
use crate::error::RdyError;
use crate::materialize::Materialized;
use crate::readiness::Candidate;
use crate::tensions::{SideRef, Tension, TensionKind, TensionSeverity, TensionStatus};
use rdy_model::assessment::{
    Assessment, AssessmentForeign, AssessmentForeignReach, AssessmentForeignTension,
    AssessmentPlanAnswerRef, MAX_ROWS,
};
use rdy_model::foreign::ForeignId;
use rdy_model::{BlockId, Hash, RepoId};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// One foreign side of one open tension, as the gate reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignGateFact {
    /// The tension's full id.
    pub tension: Hash,
    pub kind: TensionKind,
    pub severity: TensionSeverity,
    /// The cited repo of the side.
    pub repo: RepoId,
    /// The FOREIGN entry that delivered the side's version.
    pub entry: Hash,
    /// The cited block the side involves.
    pub target: ForeignId,
    /// The BLOCK version the side holds; `None` when the cited block is absent from the entry.
    pub version: Option<Hash>,
    /// The local blocks of the tension, sorted and distinct.
    pub blocks: Vec<BlockId>,
    /// The author's answer to the tension's plan item, when it has one.
    pub answer: Option<AssessmentPlanAnswerRef>,
}

/// The foreign gate facts of `rows`, each a tension with its status: one fact per foreign side
/// item of each open tension, `answers` giving the author's answer by tension id. Sorted by
/// tension, repo, entry and cited block; the same row twice gives its facts once.
pub fn foreign_tension_gate(
    rows: &[(Arc<Tension>, TensionStatus)],
    answers: &BTreeMap<Hash, AssessmentPlanAnswerRef>,
) -> Vec<ForeignGateFact> {
    let mut facts: BTreeMap<(Hash, RepoId, Hash, BlockId), ForeignGateFact> = BTreeMap::new();
    for (t, status) in rows {
        if *status != TensionStatus::Open {
            continue;
        }
        let blocks = t.blocks();
        for item in &t.involved {
            let SideRef::Foreign { repo, entry } = &item.side else {
                continue;
            };
            let fact = ForeignGateFact {
                tension: t.id,
                kind: t.kind,
                severity: t.severity,
                repo: *repo,
                entry: *entry,
                target: ForeignId::new(*repo, item.block.clone()),
                version: item.version,
                blocks: blocks.clone(),
                answer: answers.get(&t.id).cloned(),
            };
            facts.insert((t.id, *repo, *entry, item.block.clone()), fact);
        }
    }
    facts.into_values().collect()
}

/// The foreign part of the assessment `a` of candidate `c`: see the module docs. `rows` are the
/// tension rows the version lists, each with its status, as the local record was built from
/// them; a row the local record does not list is left out, so the two parts agree.
pub fn assessment_foreign(
    st: &ReadState,
    c: &Candidate,
    a: &Assessment,
    rows: &[(Arc<Tension>, TensionStatus)],
) -> Result<AssessmentForeign, RdyError> {
    let listed: BTreeSet<Hash> = a.tensions.iter().map(|t| t.id).collect();
    let answers: BTreeMap<Hash, AssessmentPlanAnswerRef> = a
        .tensions
        .iter()
        .filter_map(|t| Some((t.id, t.answer.clone()?)))
        .collect();
    let rows: Vec<(Arc<Tension>, TensionStatus)> = rows
        .iter()
        .filter(|(t, _)| listed.contains(&t.id))
        .cloned()
        .collect();
    let mut reach: BTreeSet<AssessmentForeignReach> = BTreeSet::new();
    let mut tensions: BTreeSet<AssessmentForeignTension> = BTreeSet::new();
    for f in foreign_tension_gate(&rows, &answers) {
        tensions.insert(AssessmentForeignTension {
            tension: f.tension,
            repo: f.repo,
            entry: f.entry,
        });
        if let Some(version) = f.version {
            reach.insert(AssessmentForeignReach {
                target: f.target,
                version,
            });
        }
    }
    // the pins of every block in the local reach, as the candidate projects it (canon at the
    // version's base plus its changes in order): written blocks and unchanged ones alike
    let reader = st.reader();
    let mut m = match crate::readiness::canon_scope(st, c.p.canon_at) {
        Some(scope) => (*st.materialized(&scope)?).clone(),
        None => Materialized::default(),
    };
    for h in &c.p.changes {
        m = m.with(&reader.change(h)?.ops);
    }
    for id in &a.reach {
        let Some(block) = m.get(id) else { continue };
        for (target, pin) in &block.foreign.pins {
            if let Some(version) = st.foreign_held_pin(target, pin)? {
                reach.insert(AssessmentForeignReach {
                    target: target.clone(),
                    version,
                });
            }
        }
    }
    Ok(AssessmentForeign {
        reach: reach.into_iter().take(MAX_ROWS).collect(),
        tensions: tensions.into_iter().take(MAX_ROWS).collect(),
    })
}
