//! The coverage change a write or a proposal makes (FW-CH3, D-C6HHV): the corpus score of the
//! scope before and after, and the score of each page the change touched. Read from two
//! memoised summaries and the pages' own blocks: it never builds a full report and stores
//! nothing (D-NMGD2).
use super::score::PAGES_CAP;
use crate::{RdyError, ReadState, Scope};
use rdy_model::BlockId;
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
    let b_score = before.coverage_summary(before_scope)?.score;
    let a_score = after.coverage_summary(after_scope)?.score;
    let b_issues = before.coverage_issues(before_scope)?;
    let a_issues = after.coverage_issues(after_scope)?;
    let mut docs: BTreeSet<&BlockId> = BTreeSet::new();
    for id in touched {
        for issues in [&a_issues, &b_issues] {
            if let Some(d) = issues.blocks.get(id).and_then(|e| e.doc.as_ref()) {
                docs.insert(d);
            }
        }
    }
    let was = before.doc_rollups(before_scope, &docs)?;
    let now = after.doc_rollups(after_scope, &docs)?;
    let m = after.materialized(after_scope)?;
    let mut pages: Vec<PageChange> = now
        .iter()
        .map(|(doc, r)| PageChange {
            doc: doc.to_string(),
            path: m.path_of(doc).unwrap_or_default(),
            before: was.get(doc).map_or(100, |w| w.score),
            after: r.score,
        })
        .collect();
    pages.sort_by(|a, b| {
        (a.before == a.after, &a.path, &a.doc).cmp(&(b.before == b.after, &b.path, &b.doc))
    });
    let pages_omitted = pages.len().saturating_sub(PAGES_CAP);
    pages.truncate(PAGES_CAP);
    Ok(CoverageChange {
        before: b_score,
        after: a_score,
        pages,
        pages_omitted,
    })
}
