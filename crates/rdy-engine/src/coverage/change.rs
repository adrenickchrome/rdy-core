//! The coverage change a write or a proposal makes (FW-CH3, D-C6HHV): the corpus score of the
//! scope before and after, and the score of each page the change touched. Read from two
//! memoised rollups and the pages' own blocks: it never builds a full report, counts no entity
//! kinds and stores nothing (D-NMGD2).
use super::model::CoverageIssues;
use super::score::{PAGES_CAP, rollups_of};
use super::state::CandidateCoverage;
use crate::gate::{applied_ops, changes_of};
use crate::read_state::Built;
use crate::{Materialized, RdyError, ReadState, Scope};
use rdy_model::{BlockId, Change, Proposal};
use rdy_schema::Pack;
use serde::Serialize;
use std::collections::BTreeSet;

/// One page the change touched: its score before and after. A page that did not exist before
/// scores 100, as a page with nothing to check does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PageChange {
    pub doc: String,
    pub path: String,
    pub before: u32,
    pub after: u32,
}

/// The coverage change of a write or a proposal: the corpus scores of the scope and the pages the
/// change touched, the ones whose score moved first, then by path, at most [`PAGES_CAP`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CoverageChange {
    pub before: u32,
    pub after: u32,
    pub pages: Vec<PageChange>,
    #[serde(skip_serializing_if = "is_zero")]
    pub pages_omitted: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// The coverage change from `before` in `before_scope` to `after` in `after_scope`, for a change
/// that wrote the blocks `touched`. A page is listed when it holds a touched block in either
/// state and still exists after the change. `E_BAD_INPUT` for a past landing.
pub fn coverage_change(
    before: &ReadState,
    before_scope: &Scope,
    after: &ReadState,
    after_scope: &Scope,
    touched: &BTreeSet<BlockId>,
) -> Result<CoverageChange, RdyError> {
    let b_score = before.scope_rollup(before_scope)?.score;
    let a_score = after.scope_rollup(after_scope)?.score;
    let b_issues = before.coverage_issues(before_scope)?;
    let a_issues = after.coverage_issues(after_scope)?;
    let bm = before.materialized(before_scope)?;
    let am = after.materialized(after_scope)?;
    Ok(change_between(
        after.pack(),
        Side {
            score: b_score,
            issues: &b_issues,
            m: &bm,
        },
        Side {
            score: a_score,
            issues: &a_issues,
            m: &am,
        },
        touched,
    ))
}

/// The coverage change that landing a proposal version would make (ruling final, FW-CV2
/// D-XZXCE, FW-G7c D-4PG9Y): canon's against the proposal candidate's, the canon with exactly
/// the version's changes (`built`, whose coverage is `candidate`), for the blocks the version
/// writes (`touched`). The batch card's Coverage fact and the propose reply's `coverage` are
/// both this, so the two cannot differ.
pub(crate) fn candidate_change(
    st: &ReadState,
    built: &Built,
    candidate: &CandidateCoverage,
    touched: &BTreeSet<BlockId>,
) -> Result<CoverageChange, RdyError> {
    let canon = Scope::Canon;
    let b_score = st.scope_rollup(&canon)?.score;
    let b_issues = st.coverage_issues(&canon)?;
    let bm = st.materialized(&canon)?;
    Ok(change_between(
        st.pack(),
        Side {
            score: b_score,
            issues: &b_issues,
            m: &bm,
        },
        Side {
            score: candidate.score,
            issues: &candidate.issues,
            m: &built.m,
        },
        touched,
    ))
}

/// The canon that landing a version with changes `changes` would give: canon's build with their
/// ops applied in the order the merge applies them. Canon's graph is built first, so the
/// candidate carries it instead of building another.
pub(crate) fn candidate_build(st: &ReadState, changes: &[Change]) -> Result<Built, RdyError> {
    st.graph(&Scope::Canon)?;
    let applied = st.landing_order(changes);
    Ok(st
        .built(&Scope::Canon)?
        .after(&applied_ops(changes, &applied), st.pack()))
}

impl ReadState {
    /// The coverage change of landing proposal version `p` ([`candidate_change`]): what the
    /// propose reply gives.
    pub fn proposal_coverage(&self, p: &Proposal) -> Result<CoverageChange, RdyError> {
        let changes = changes_of(self, &p.changes)?;
        let touched: BTreeSet<BlockId> = changes
            .iter()
            .flat_map(|c| &c.ops)
            .map(|o| o.block_id().clone())
            .collect();
        // canon's coverage and lexicon first, as the queue reads them: the candidate's build
        // carries its structural layer from them
        let _ = self.scope_rollup(&Scope::Canon);
        let _ = self.lexicon(&Scope::Canon);
        let built = candidate_build(self, &changes)?;
        let candidate = self.candidate_coverage(&built, &p.changes)?;
        candidate_change(self, &built, &candidate, &touched)
    }
}

/// One side of a coverage change: its corpus score, its coverage issues and its blocks.
struct Side<'a> {
    score: u32,
    issues: &'a CoverageIssues,
    m: &'a Materialized,
}

/// The coverage change from `before` to `after` for a change that wrote the blocks `touched`
/// (see [`coverage_change`]).
fn change_between(
    pack: &Pack,
    before: Side<'_>,
    after: Side<'_>,
    touched: &BTreeSet<BlockId>,
) -> CoverageChange {
    let mut docs: BTreeSet<&BlockId> = BTreeSet::new();
    for id in touched {
        for issues in [after.issues, before.issues] {
            if let Some(d) = issues.blocks.get(id).and_then(|e| e.doc.as_ref()) {
                docs.insert(d);
            }
        }
    }
    let was = rollups_of(pack, before.m, before.issues, &docs);
    let now = rollups_of(pack, after.m, after.issues, &docs);
    let mut pages: Vec<PageChange> = now
        .iter()
        .map(|(doc, r)| PageChange {
            doc: doc.to_string(),
            path: after.m.path_of(doc).unwrap_or_default(),
            before: was.get(doc).map_or(100, |w| w.score),
            after: r.score,
        })
        .collect();
    pages.sort_by(|a, b| {
        (a.before == a.after, &a.path, &a.doc).cmp(&(b.before == b.after, &b.path, &b.doc))
    });
    let pages_omitted = pages.len().saturating_sub(PAGES_CAP);
    pages.truncate(PAGES_CAP);
    CoverageChange {
        before: before.score,
        after: after.score,
        pages,
        pages_omitted,
    }
}
