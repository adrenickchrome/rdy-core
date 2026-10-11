//! The state layer of coverage (FW-CH1, FW-CV4, FW-CH6, FW-CH7): the structural layer with
//! the dismissed mentions settled, plus the Consistent issues of the tensions the scope counts
//! and the Current issues of its suspects.
use super::model::*;
use super::structural::structural_of;
use crate::error::E_BAD_INPUT;
use crate::tensions::{SideRef, Tension, TensionStatus, tension_label};
use crate::{RdyError, ReadState, Scope};
use rdy_model::{BlockId, Hash};
use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

/// The most scopes a state memoises coverage issues for; the oldest is dropped first.
const MEMO_SCOPES: usize = 8;

/// The coverage issues a state memoised, by scope label, oldest first.
pub(crate) type CoverageMemo = Mutex<VecDeque<(String, Arc<CoverageIssues>)>>;

/// The memo key of `scope` (one `coverage_scope` gave: never a past landing).
pub(super) fn memo_label(scope: &Scope) -> String {
    match scope {
        Scope::Canon | Scope::At(_) => "canon".to_string(),
        Scope::View(v) => format!("view/{v}"),
        Scope::Inflight => "inflight".to_string(),
    }
}

/// Which tensions a scope counts (ledger Q5): canon counts those whose sides are all canon; a
/// view counts those too, plus those with a change side in its set; the in-flight scope counts
/// every tension. Intent and read-time tensions are never counted.
enum Counted {
    Canon,
    View(BTreeSet<Hash>),
    All,
}

impl Counted {
    /// The tensions `scope` counts; `scope` is one `coverage_scope` gave, so a landing in it is
    /// canon's head.
    fn of(st: &ReadState, scope: &Scope) -> Counted {
        match scope {
            Scope::Canon | Scope::At(_) => Counted::Canon,
            Scope::View(v) => Counted::View(st.included(v).iter().copied().collect()),
            Scope::Inflight => Counted::All,
        }
    }

    fn counts(&self, from_canon: bool, t: &Tension) -> bool {
        if t.read_time || t.sides.iter().any(|s| matches!(s, SideRef::Intent { .. })) {
            return false;
        }
        match self {
            Counted::Canon => from_canon,
            Counted::View(set) => {
                from_canon
                    || t.sides
                        .iter()
                        .any(|s| matches!(s, SideRef::Change(c) if set.contains(c)))
            }
            Counted::All => true,
        }
    }
}

impl ReadState {
    /// True when `scope` is a landing other than canon's head: a past state, which coverage is
    /// not computed for (FW-H7).
    pub(crate) fn is_past_landing(&self, scope: &Scope) -> bool {
        matches!(scope, Scope::At(h) if Some(*h) != self.canon_head())
    }

    /// The scope coverage is computed in for `scope`: canon's head is canon. `E_BAD_INPUT` for a
    /// past landing: coverage describes the current state only, and coverage over time belongs
    /// to rdyhub (FW-H7).
    pub fn coverage_scope(&self, scope: &Scope) -> Result<Scope, RdyError> {
        match scope {
            Scope::At(h) if self.is_past_landing(scope) => Err(RdyError::new(
                E_BAD_INPUT,
                format!("coverage is computed for the current state only, not at landing:{h}"),
                "ask for canon or a view; coverage over time is not part of rdy core",
            )),
            Scope::At(_) => Ok(Scope::Canon),
            other => Ok(other.clone()),
        }
    }

    /// The coverage issues of `scope` in all six dimensions, per scored block, each block's
    /// issues in the closed order. Memoised per state and scope. Canon's head is canon;
    /// `E_BAD_INPUT` for a past landing (`coverage_scope`).
    pub fn coverage_issues(&self, scope: &Scope) -> Result<Arc<CoverageIssues>, RdyError> {
        let scope = &self.coverage_scope(scope)?;
        let label = memo_label(scope);
        {
            let memo = self.coverage_memo.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, hit)) = memo.iter().find(|(l, _)| *l == label) {
                return Ok(Arc::clone(hit));
            }
        }
        let issues = Arc::new(self.coverage_issues_of(scope)?);
        let mut memo = self.coverage_memo.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, hit)) = memo.iter().find(|(l, _)| *l == label) {
            return Ok(Arc::clone(hit));
        }
        memo.push_back((label, Arc::clone(&issues)));
        while memo.len() > MEMO_SCOPES {
            memo.pop_front();
        }
        Ok(issues)
    }

    /// The structural layer of `scope`, memoised on its build.
    pub(crate) fn structural(&self, scope: &Scope) -> Result<Arc<Structural>, RdyError> {
        let built = self.built(scope)?;
        if let Some(s) = built.structural.get() {
            return Ok(Arc::clone(s));
        }
        let s = Arc::new(structural_of(self, scope)?);
        Ok(Arc::clone(built.structural.get_or_init(|| s)))
    }

    fn coverage_issues_of(&self, scope: &Scope) -> Result<CoverageIssues, RdyError> {
        let structural = self.structural(scope)?;
        let mut blocks = structural.blocks.clone();
        let mut touched: BTreeSet<BlockId> = BTreeSet::new();

        // a dismissed mention is settled: its check stays, its issue goes (D-E6JD1)
        for ((block, entity), _) in self.mention_dismissals().iter() {
            if let Some(e) = blocks.get_mut(block) {
                let subject = entity.to_string();
                e.issues.retain(|i| {
                    !(i.code == IssueCode::UnlinkedMention
                        && i.subject.as_deref() == Some(subject.as_str()))
                });
            }
        }

        // Consistent: each counted tension, once per block it involves
        let counted = Counted::of(self, scope);
        let set = self.tensions()?;
        for t in set.iter() {
            if !counted.counts(set.from_canon(&t.id), t) {
                continue;
            }
            let label = tension_label(&t.id);
            let (code, fix, dismissed) = match self.tension_status(t) {
                TensionStatus::Open => (
                    IssueCode::OpenTension,
                    fix_open_tension(&label, t.kind.name()),
                    None,
                ),
                TensionStatus::Dismissed(review) => (
                    IssueCode::DismissedTension,
                    fix_dismissed_tension(&label),
                    Some(match self.dismissal_info(&review) {
                        Some(d) => Dismissal {
                            review: d.review,
                            actor: d.actor,
                            reason: d.reason,
                        },
                        None => Dismissal {
                            review: review.to_string(),
                            actor: String::new(),
                            reason: String::new(),
                        },
                    }),
                ),
            };
            for b in t.blocks() {
                if let Some(e) = blocks.get_mut(&b) {
                    let mut issue = CoverageIssue::new(&b, code, Some(label.clone()), fix.clone());
                    issue.dismissed = dismissed.clone();
                    e.issues.push(issue);
                    touched.insert(b);
                }
            }
        }

        // Current: each suspect on its block, presentational sources left out
        let m = self.materialized(scope)?;
        let g = self.graph(scope)?;
        for s in g.suspects() {
            if self.is_presentational_source(&m, &s) {
                continue;
            }
            if let Some(e) = blocks.get_mut(&s.block) {
                e.issues.push(CoverageIssue::new(
                    &s.block,
                    IssueCode::OpenSuspect,
                    Some(s.target.to_string()),
                    fix_open_suspect(&s.block, &s.target),
                ));
                touched.insert(s.block);
            }
        }

        let no_paths = Default::default();
        for b in touched {
            if let Some(e) = blocks.get_mut(&b) {
                sort_issues(&mut e.issues, &no_paths);
            }
        }
        Ok(CoverageIssues {
            blocks,
            mentions: structural.mentions.clone(),
        })
    }
}
