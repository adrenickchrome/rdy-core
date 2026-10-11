//! The state layer of coverage (FW-CH1, FW-CV4, FW-CH6, FW-CH7): the structural layer with
//! the dismissed mentions settled, plus the Consistent issues of the tensions the scope counts
//! and the Current issues of its suspects.
use super::kinds::{KindCount, kinds_of};
use super::model::*;
use super::score::rollup;
use super::structural::structural_of;
use crate::error::E_BAD_INPUT;
use crate::tensions::set::TensionSet;
use crate::tensions::{SideRef, Tension, TensionStatus, tension_label};
use crate::{Materialized, RdyError, ReadState, Scope};
use rdy_graph::Suspect;
use rdy_model::{BlockId, Hash};
use serde::Serialize;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

thread_local! {
    static STATE_LAYER_BUILDS: Cell<u64> = const { Cell::new(0) };
}

/// Test hook: this thread's count of full builds of the state layer (every counted tension and
/// every suspect applied to the whole structural layer). A carried read leaves it unchanged.
#[doc(hidden)]
pub fn state_layer_builds() -> u64 {
    STATE_LAYER_BUILDS.with(Cell::get)
}

/// Test hook: the coverage issues of `scope` built in full now, past the memo and any carry.
#[doc(hidden)]
pub fn issues_rebuilt_for_tests(st: &ReadState, scope: &Scope) -> Result<CoverageIssues, RdyError> {
    let scope = st.coverage_scope(scope)?;
    let inputs = Inputs::of(st, &scope)?;
    Ok(st.full_issues(&inputs))
}

/// The most scopes a state memoises coverage issues for; the oldest is dropped first.
const MEMO_SCOPES: usize = 8;

/// The coverage a state memoised, by scope label, oldest first, and the latest entry of each
/// label in the states before it, which a first read may carry from (fix round 2, FW-PF2).
#[derive(Default)]
pub(crate) struct CoverageMemo {
    entries: Mutex<VecDeque<(String, Arc<Memoised>)>>,
    prev: Vec<(String, Arc<Memoised>)>,
}

impl CoverageMemo {
    /// An empty memo that may carry from `prev`'s entries, and, for a label `prev` did not read,
    /// from the entry `prev` could carry from (a propose publishes before its landing, and
    /// nothing reads in between). A carry checks every input against the entry, so an older
    /// entry is safe. At most [`MEMO_SCOPES`] labels, the latest kept.
    pub(crate) fn after(prev: &CoverageMemo) -> CoverageMemo {
        let mut out: VecDeque<(String, Arc<Memoised>)> = prev.prev.iter().cloned().collect();
        for (label, m) in prev
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
        {
            out.retain(|(l, _)| l != label);
            out.push_back((label.clone(), Arc::clone(m)));
        }
        while out.len() > MEMO_SCOPES {
            out.pop_front();
        }
        CoverageMemo {
            entries: Mutex::default(),
            prev: out.into(),
        }
    }
}

/// One scope's memoised coverage: its issues, the summary of them once read, and what the
/// issues were computed from.
pub(crate) struct Memoised {
    issues: Arc<CoverageIssues>,
    summary: OnceLock<Arc<CoverageSummary>>,
    inputs: Inputs,
}

/// What a scope's coverage issues are a function of (with the pack): its structural layer, its
/// build, the tension set and the tensions the scope counts, the dismissed tensions and
/// mentions, and its suspects with presentational sources left out.
struct Inputs {
    structural: Arc<Structural>,
    m: Arc<Materialized>,
    tensions: Arc<TensionSet>,
    counted: Counted,
    dismissals: Arc<imbl::OrdMap<Hash, Hash>>,
    mention_dismissals: Arc<imbl::OrdMap<(BlockId, BlockId), Hash>>,
    suspects: Vec<Suspect>,
}

impl Inputs {
    /// `scope`'s inputs in `st`; `scope` is one `coverage_scope` gave.
    fn of(st: &ReadState, scope: &Scope) -> Result<Inputs, RdyError> {
        let structural = st.structural(scope)?;
        let tensions = st.tensions()?;
        let m = st.materialized(scope)?;
        let g = st.graph(scope)?;
        let suspects = g
            .suspects()
            .into_iter()
            .filter(|s| !st.is_presentational_source(&m, s))
            .collect();
        Ok(Inputs {
            structural,
            m,
            tensions,
            counted: Counted::of(st, scope),
            dismissals: Arc::clone(&st.dismissals),
            mention_dismissals: Arc::clone(&st.mention_dismissals),
            suspects,
        })
    }

    /// The tensions these inputs count, by id, in the set's order.
    fn counted(&self) -> BTreeMap<&Hash, &Arc<Tension>> {
        self.tensions
            .iter()
            .filter(|t| self.counted.counts(self.tensions.from_canon(&t.id), t))
            .map(|t| (&t.id, t))
            .collect()
    }
}

/// The summary of a scope's coverage that `rdy status` and `rdy_overview` show: the score, its
/// dimensions, the issue count and the open entity kinds. The same numbers as the scope's
/// [`super::CoverageReport`], without its pages, areas and issue list.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CoverageSummary {
    pub score: u32,
    pub dimensions: BTreeMap<Dimension, u32>,
    pub issues_total: usize,
    pub kinds: Vec<KindCount>,
    pub similar_kinds: Vec<Vec<String>>,
}

/// A coverage read error as a reply's `coverage_error` gives it: `<code>: <message>`. A
/// coverage read error never fails a reply; the coverage is left out and this says why.
pub fn coverage_error_text(e: &RdyError) -> String {
    format!("{}: {}", e.code, e.message)
}

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
#[derive(PartialEq)]
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
        Ok(Arc::clone(&self.memoised(scope)?.issues))
    }

    /// The summary of `scope`'s coverage: what its report gives for the score, the dimensions,
    /// the issue count and the kinds, read from the memoised issues without cloning, placing or
    /// sorting any of them. Memoised per state and scope. `E_BAD_INPUT` for a past landing.
    pub fn coverage_summary(&self, scope: &Scope) -> Result<Arc<CoverageSummary>, RdyError> {
        let scope = &self.coverage_scope(scope)?;
        let memo = self.memoised(scope)?;
        if let Some(s) = memo.summary.get() {
            return Ok(Arc::clone(s));
        }
        let whole = rollup(&self.pack, memo.issues.blocks.values());
        let issues_total = memo.issues.blocks.values().map(|e| e.issues.len()).sum();
        let m = self.materialized(scope)?;
        let (kinds, similar_kinds) = kinds_of(&m);
        let s = Arc::new(CoverageSummary {
            score: whole.score,
            dimensions: whole.dimensions,
            issues_total,
            kinds,
            similar_kinds,
        });
        Ok(Arc::clone(memo.summary.get_or_init(|| s)))
    }

    /// The memo entry of `scope`, built on first read.
    fn memoised(&self, scope: &Scope) -> Result<Arc<Memoised>, RdyError> {
        let scope = &self.coverage_scope(scope)?;
        let label = memo_label(scope);
        {
            let memo = self
                .coverage_memo
                .entries
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some((_, hit)) = memo.iter().find(|(l, _)| *l == label) {
                return Ok(Arc::clone(hit));
            }
        }
        let inputs = Inputs::of(self, scope)?;
        let prev = self
            .coverage_memo
            .prev
            .iter()
            .find(|(l, _)| *l == label)
            .map(|(_, p)| p);
        let built = match prev.and_then(|p| self.carried_issues(scope, p, &inputs)) {
            Some(Carried::Same) => Arc::clone(prev.expect("a carry needs the entry before")),
            Some(Carried::Issues(issues)) => Arc::new(Memoised {
                issues: Arc::new(issues),
                summary: OnceLock::new(),
                inputs,
            }),
            None => Arc::new(Memoised {
                issues: Arc::new(self.full_issues(&inputs)),
                summary: OnceLock::new(),
                inputs,
            }),
        };
        let mut memo = self
            .coverage_memo
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some((_, hit)) = memo.iter().find(|(l, _)| *l == label) {
            return Ok(Arc::clone(hit));
        }
        memo.push_back((label, Arc::clone(&built)));
        while memo.len() > MEMO_SCOPES {
            memo.pop_front();
        }
        Ok(built)
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

    /// The coverage issues of `inputs` built in full: the structural layer with the dismissed
    /// mentions settled, then every counted tension and every suspect applied. Counted by
    /// [`state_layer_builds`].
    fn full_issues(&self, inputs: &Inputs) -> CoverageIssues {
        STATE_LAYER_BUILDS.with(|c| c.set(c.get() + 1));
        let mut blocks = inputs.structural.blocks.clone();
        self.apply_state(&mut blocks, inputs, None);
        CoverageIssues {
            blocks,
            mentions: inputs.structural.mentions.clone(),
        }
    }

    /// Fix round 2 (FW-PF2): `prev`, the issues of the same scope in the state this one was
    /// published after, carried to `inputs`: equal to [`ReadState::full_issues`]. The blocks
    /// whose entries can differ are those the structural carry recomputed (or none, when the
    /// layer is the same), those of each counted tension added, removed or changed, and those
    /// of each suspect when the suspects changed; only their entries are rebuilt. `None` (a
    /// full build follows) when the dismissed tensions or mentions changed, or the structural
    /// layer is neither `prev`'s nor carried from it.
    fn carried_issues(&self, scope: &Scope, prev: &Memoised, inputs: &Inputs) -> Option<Carried> {
        let was = &prev.inputs;
        if !Arc::ptr_eq(&was.dismissals, &inputs.dismissals)
            || !Arc::ptr_eq(&was.mention_dismissals, &inputs.mention_dismissals)
        {
            return None;
        }
        let mut dirty: BTreeSet<BlockId> = if Arc::ptr_eq(&was.structural, &inputs.structural) {
            BTreeSet::new()
        } else {
            let built = self.built(scope).ok()?;
            let (from, dirty) = built.structural_from.get()?;
            if !Arc::ptr_eq(from, &was.structural) {
                return None;
            }
            (**dirty).clone()
        };
        if !(Arc::ptr_eq(&was.tensions, &inputs.tensions) && was.counted == inputs.counted) {
            let (before, after) = (was.counted(), inputs.counted());
            for (id, t) in &before {
                match after.get(id) {
                    Some(u) if Arc::ptr_eq(u, t) || u == t => {}
                    Some(u) => {
                        dirty.extend(t.blocks());
                        dirty.extend(u.blocks());
                    }
                    None => dirty.extend(t.blocks()),
                }
            }
            for (id, t) in &after {
                if !before.contains_key(id) {
                    dirty.extend(t.blocks());
                }
            }
        }
        if was.suspects != inputs.suspects {
            dirty.extend(was.suspects.iter().map(|s| s.block.clone()));
            dirty.extend(inputs.suspects.iter().map(|s| s.block.clone()));
        }
        if dirty.is_empty() {
            return Some(if Arc::ptr_eq(&was.m, &inputs.m) {
                Carried::Same
            } else {
                Carried::Issues((*prev.issues).clone())
            });
        }
        let mut blocks = prev.issues.blocks.clone();
        for id in &dirty {
            match inputs.structural.blocks.get(id) {
                Some(e) => {
                    blocks.insert(id.clone(), e.clone());
                }
                None => {
                    blocks.remove(id);
                }
            }
        }
        self.apply_state(&mut blocks, inputs, Some(&dirty));
        Some(Carried::Issues(CoverageIssues {
            blocks,
            mentions: inputs.structural.mentions.clone(),
        }))
    }

    /// Applies the state layer of `inputs` to `blocks` (structural entries): the dismissed
    /// mentions settled, the Consistent issue of each counted tension on each block it
    /// involves, and the Current issue of each suspect on its block; only to the blocks in
    /// `only`, when given.
    fn apply_state(
        &self,
        blocks: &mut imbl::OrdMap<BlockId, BlockEntry>,
        inputs: &Inputs,
        only: Option<&BTreeSet<BlockId>>,
    ) {
        let wanted = |b: &BlockId| only.is_none_or(|o| o.contains(b));
        let mut touched: BTreeSet<BlockId> = BTreeSet::new();

        // a dismissed mention is settled: its check stays, its issue goes (D-E6JD1)
        for ((block, entity), _) in inputs.mention_dismissals.iter() {
            if !wanted(block) {
                continue;
            }
            if let Some(e) = blocks.get_mut(block) {
                let subject = entity.to_string();
                e.issues.retain(|i| {
                    !(i.code == IssueCode::UnlinkedMention
                        && i.subject.as_deref() == Some(subject.as_str()))
                });
            }
        }

        // Consistent: each counted tension, once per block it involves
        let set = &inputs.tensions;
        for t in set.iter() {
            if !inputs.counted.counts(set.from_canon(&t.id), t) {
                continue;
            }
            if only.is_some() && !t.involved.iter().any(|i| wanted(&i.block)) {
                continue;
            }
            let mine: Vec<BlockId> = t.blocks().into_iter().filter(|b| wanted(b)).collect();
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
            for b in mine {
                if let Some(e) = blocks.get_mut(&b) {
                    let mut issue = CoverageIssue::new(&b, code, Some(label.clone()), fix.clone());
                    issue.dismissed = dismissed.clone();
                    e.issues.push(issue);
                    touched.insert(b);
                }
            }
        }

        // Current: each suspect on its block, presentational sources left out
        for s in &inputs.suspects {
            if !wanted(&s.block) {
                continue;
            }
            if let Some(e) = blocks.get_mut(&s.block) {
                e.issues.push(CoverageIssue::new(
                    &s.block,
                    IssueCode::OpenSuspect,
                    Some(s.target.to_string()),
                    fix_open_suspect(&s.block, &s.target),
                ));
                touched.insert(s.block.clone());
            }
        }

        let no_paths = Default::default();
        for b in touched {
            if let Some(e) = blocks.get_mut(&b) {
                sort_issues(&mut e.issues, &no_paths);
            }
        }
    }
}

/// What a carry gives: the entry before, whole (nothing it read changed), or new issues.
enum Carried {
    Same,
    Issues(CoverageIssues),
}
