//! Scores and rollups (FW-CH1, FW-CH7, D-NMGD2, D-TQD4D): integer arithmetic only. A dimension
//! scores the checks that pass over the checks that apply; an open issue costs one check and a
//! dismissed tension costs the pack's `dismissed_weight` percent of one. The overall score is
//! the weighted mean of the dimensions present. Nothing is stored.
use super::kinds::{KindCount, kinds_of};
use super::model::*;
use super::state::memo_label;
use crate::error::E_NOT_FOUND;
use crate::paths::resolve_in;
use crate::{RdyError, ReadState, Scope};
use rdy_model::{BlockId, tree};
use rdy_schema::Pack;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// The most pages a report lists, lowest score first; the rest are counted.
pub const PAGES_CAP: usize = 20;

thread_local! {
    static REPORT_BUILDS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Test hook: this thread's count of full coverage reports built (`coverage_report` calls that
/// got past the scope check). A summary reader must leave it unchanged.
#[doc(hidden)]
pub fn report_builds() -> u64 {
    REPORT_BUILDS.with(std::cell::Cell::get)
}

/// The score of a set of blocks: overall, and per dimension. A dimension with no check is
/// absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Rollup {
    pub score: u32,
    pub dimensions: BTreeMap<Dimension, u32>,
}

/// The score of one page (a doc and the blocks under it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PageScore {
    pub doc: String,
    pub path: String,
    pub score: u32,
}

/// The score of one area.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AreaScore {
    pub area: String,
    pub score: u32,
}

/// The coverage of a scope: its score and dimensions, its lowest pages, its areas, its issues in
/// the closed order, and the open entity kinds. With a path, the score, pages, areas and issues
/// are those of the docs at or under that path; the kinds are those of the whole scope.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CoverageReport {
    pub scope: String,
    pub score: u32,
    pub dimensions: BTreeMap<Dimension, u32>,
    pub pages: Vec<PageScore>,
    pub pages_omitted: usize,
    pub areas: Vec<AreaScore>,
    pub issues: Vec<CoverageIssue>,
    pub issues_total: usize,
    pub kinds: Vec<KindCount>,
    pub similar_kinds: Vec<Vec<String>>,
}

/// The rollup of each of `docs` that has blocks in `m`, scored by `issues`: what
/// `ReadState::doc_rollups` gives a scope, and the coverage change gives a candidate canon.
pub(crate) fn rollups_of(
    pack: &Pack,
    m: &crate::Materialized,
    issues: &CoverageIssues,
    docs: &BTreeSet<&BlockId>,
) -> BTreeMap<BlockId, Rollup> {
    let mut out = BTreeMap::new();
    for d in docs {
        let entries: Vec<&BlockEntry> = page_blocks(m, d)
            .iter()
            .filter_map(|id| issues.blocks.get(id))
            .collect();
        if !entries.is_empty() {
            out.insert((*d).clone(), rollup(pack, entries.into_iter()));
        }
    }
    out
}

/// The blocks of page `doc` in `m`: the doc and the blocks under it, not crossing into a doc
/// under it (a doc under a doc is its own page).
pub(crate) fn page_blocks(m: &crate::Materialized, doc: &BlockId) -> Vec<BlockId> {
    let mut out = vec![];
    let mut stack = vec![doc.clone()];
    while let Some(id) = stack.pop() {
        // a damaged store may loop: never walk more blocks than there are
        if out.len() > m.len() {
            break;
        }
        stack.extend(
            m.children(Some(&id))
                .into_iter()
                .filter(|b| b.block_type != tree::DOC)
                .map(|b| b.id.clone()),
        );
        out.push(id);
    }
    out
}

/// The rollup of `entries` under `pack`'s weights, in hundredths of a check.
pub fn rollup<'a>(pack: &Pack, entries: impl Iterator<Item = &'a BlockEntry>) -> Rollup {
    let mut sums = RollupSums::new(pack);
    for e in entries {
        sums.add(e);
    }
    sums.rollup(pack)
}

/// What a rollup adds up, per dimension: the checks and their cost in hundredths of a check.
/// Each entry adds its own part, so a carry of the coverage issues takes the parts of the
/// entries it replaced out and puts the new ones in, instead of adding up every entry again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RollupSums {
    /// The pack's weight of a dismissed issue, in hundredths, that the costs were added under.
    dismissed: u64,
    checks: [u64; 6],
    cost: [u64; 6],
}

impl RollupSums {
    /// No entry, under `pack`'s weight of a dismissed issue.
    pub(crate) fn new(pack: &Pack) -> RollupSums {
        RollupSums {
            dismissed: u64::from(pack.dismissed_weight().min(100)),
            checks: [0; 6],
            cost: [0; 6],
        }
    }

    /// Whether these sums were added under `pack`'s weight of a dismissed issue.
    pub(crate) fn fits(&self, pack: &Pack) -> bool {
        self.dismissed == u64::from(pack.dismissed_weight().min(100))
    }

    /// The checks and the cost `e` adds, per dimension.
    fn part(&self, e: &BlockEntry) -> ([u64; 6], [u64; 6]) {
        let (mut checks, mut cost) = ([0u64; 6], [0u64; 6]);
        for (i, d) in Dimension::ALL.iter().enumerate() {
            let n = u64::from(e.checks[i]);
            let c: u64 = e
                .issues
                .iter()
                .filter(|x| x.dimension == *d)
                .map(|x| {
                    if x.dismissed.is_some() || x.code == IssueCode::DismissedTension {
                        self.dismissed
                    } else {
                        100
                    }
                })
                .sum();
            // one check at most in every dimension but Grounded, which has one per item
            let cap = if *d == Dimension::Grounded {
                100 * n
            } else {
                100u64.min(100 * n)
            };
            checks[i] = n;
            cost[i] = c.min(cap);
        }
        (checks, cost)
    }

    /// Adds entry `e`.
    pub(crate) fn add(&mut self, e: &BlockEntry) {
        let (checks, cost) = self.part(e);
        for i in 0..6 {
            self.checks[i] += checks[i];
            self.cost[i] += cost[i];
        }
    }

    /// Takes entry `e` out again (it was added before).
    pub(crate) fn sub(&mut self, e: &BlockEntry) {
        let (checks, cost) = self.part(e);
        for i in 0..6 {
            self.checks[i] -= checks[i];
            self.cost[i] -= cost[i];
        }
    }

    /// The rollup these sums give under `pack`'s dimension weights.
    pub(crate) fn rollup(&self, pack: &Pack) -> Rollup {
        let mut dimensions = BTreeMap::new();
        let (mut weighed, mut total) = (0u64, 0u64);
        for (i, d) in Dimension::ALL.iter().enumerate() {
            if self.checks[i] == 0 {
                continue;
            }
            let whole = 100 * self.checks[i];
            let s = (whole - self.cost[i]) * 100 / whole;
            dimensions.insert(*d, s as u32);
            let w = u64::from(pack.coverage_weight(*d));
            weighed += w * s;
            total += w;
        }
        // no dimension present, or all weighted 0: nothing to fix
        let score = weighed.checked_div(total).map_or(100, |s| s as u32);
        Rollup { score, dimensions }
    }
}

impl ReadState {
    /// The rollup of the blocks `ids` in `scope` (a briefing's covered ids); an id that is not
    /// scored adds nothing. `E_BAD_INPUT` for a past landing.
    pub fn coverage_rollup(
        &self,
        scope: &Scope,
        ids: &BTreeSet<BlockId>,
    ) -> Result<Rollup, RdyError> {
        let issues = self.coverage_issues(scope)?;
        Ok(rollup(
            &self.pack,
            ids.iter().filter_map(|id| issues.blocks.get(id)),
        ))
    }

    /// The rollup of every page in `scope`, by doc: the doc and the blocks whose doc it is.
    pub fn page_rollups(&self, scope: &Scope) -> Result<BTreeMap<BlockId, Rollup>, RdyError> {
        let issues = self.coverage_issues(scope)?;
        let mut by_doc: BTreeMap<&BlockId, Vec<&BlockEntry>> = BTreeMap::new();
        for e in issues.blocks.values() {
            if let Some(d) = &e.doc {
                by_doc.entry(d).or_default().push(e);
            }
        }
        Ok(by_doc
            .into_iter()
            .map(|(d, es)| (d.clone(), rollup(&self.pack, es.into_iter())))
            .collect())
    }

    /// The rollup of each of `docs` in `scope`, as `page_rollups` gives it, found by walking
    /// the docs' own blocks: the cost follows the docs asked for, not the project. A doc that
    /// is not scored is absent.
    pub fn doc_rollups(
        &self,
        scope: &Scope,
        docs: &BTreeSet<&BlockId>,
    ) -> Result<BTreeMap<BlockId, Rollup>, RdyError> {
        let issues = self.coverage_issues(scope)?;
        let m = self.materialized(scope)?;
        Ok(rollups_of(&self.pack, &m, &issues, docs))
    }

    /// The coverage report of `scope`, or of the docs at or under `path`. Pages come lowest
    /// score first, then by path; at most [`PAGES_CAP`] are listed. Areas come by name. Issues
    /// come in the closed order; `issues_total` counts them all. An unknown path is
    /// `E_NOT_FOUND`.
    pub fn coverage_report(
        &self,
        scope: &Scope,
        path: Option<&str>,
    ) -> Result<CoverageReport, RdyError> {
        let scope = self.coverage_scope(scope)?;
        REPORT_BUILDS.with(|c| c.set(c.get() + 1));
        let issues = self.coverage_issues(&scope)?;
        let m = self.materialized(&scope)?;
        let under = match path {
            None => None,
            Some(p) => {
                let id =
                    resolve_in(&m, p).map_err(|e| RdyError::new(E_NOT_FOUND, e.message, e.fix))?;
                Some(m.path_of(&id).unwrap_or_default())
            }
        };
        let doc_path = |doc: &BlockId| m.path_of(doc).unwrap_or_default();
        // D-P3YD5: a block is under `u` by its own path, which is its doc's path, or the folder's
        // for a block outside a doc; a block with no path is kept only with no filter. A folder
        // is no content: it stays out of a filtered report, as before.
        let kept = |id: &BlockId, e: &BlockEntry| match &under {
            None => true,
            Some(u) => {
                let folder =
                    e.doc.is_none() && m.get(id).is_some_and(|b| b.block_type == tree::FOLDER);
                !folder
                    && m.path_of(id)
                        .is_some_and(|p| p == *u || p.starts_with(&format!("{u}/")))
            }
        };
        let entries: Vec<(&BlockId, &BlockEntry)> =
            issues.blocks.iter().filter(|(id, e)| kept(id, e)).collect();

        let whole = rollup(&self.pack, entries.iter().map(|(_, e)| *e));

        let mut by_doc: BTreeMap<&BlockId, Vec<&BlockEntry>> = BTreeMap::new();
        let mut by_area: BTreeMap<String, Vec<&BlockEntry>> = BTreeMap::new();
        let mut all: Vec<CoverageIssue> = vec![];
        for (id, e) in &entries {
            if let Some(d) = &e.doc {
                by_doc.entry(d).or_default().push(e);
            }
            if let Some(b) = m.get(id) {
                for a in self.areas_in(&m, b) {
                    by_area.entry(a).or_default().push(e);
                }
            }
            all.extend(e.issues.iter().cloned());
        }
        let paths = issue_paths(&m, entries.iter().copied());
        sort_issues(&mut all, &paths);

        let mut pages: Vec<PageScore> = by_doc
            .into_iter()
            .filter(|(d, _)| m.get(d).is_some_and(|b| b.block_type == tree::DOC))
            .map(|(d, es)| PageScore {
                doc: d.to_string(),
                path: doc_path(d),
                score: rollup(&self.pack, es.into_iter()).score,
            })
            .collect();
        pages.sort_by(|a, b| (a.score, &a.path, &a.doc).cmp(&(b.score, &b.path, &b.doc)));
        let pages_omitted = pages.len().saturating_sub(PAGES_CAP);
        pages.truncate(PAGES_CAP);

        let areas = by_area
            .into_iter()
            .map(|(area, es)| AreaScore {
                area,
                score: rollup(&self.pack, es.into_iter()).score,
            })
            .collect();

        let (kinds, similar_kinds) = kinds_of(&m);
        Ok(CoverageReport {
            scope: memo_label(&scope),
            score: whole.score,
            dimensions: whole.dimensions,
            pages,
            pages_omitted,
            areas,
            issues_total: all.len(),
            issues: all,
            kinds,
            similar_kinds,
        })
    }
}
