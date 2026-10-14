//! The state layer of coverage (FW-CH1, FW-CV4, FW-CH6, FW-CH7): the structural layer with
//! the dismissed mentions settled, plus the Consistent issues of the tensions the scope counts
//! and the Current issues of its suspects.
use super::kinds::{KindCount, kinds_of};
use super::model::*;
use super::score::{Rollup, RollupSums};
use super::structural::{CxMemo, EntryCx, entry_of, structural_of};
use crate::error::E_BAD_INPUT;
use crate::tensions::set::TensionSet;
use crate::tensions::{SideRef, Tension, TensionStatus, tension_label};
use crate::{Materialized, RdyError, ReadState, Scope};
use rdy_graph::{Graph, Suspect};
use rdy_model::{BlockId, Hash};
use serde::Serialize;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

thread_local! {
    static STATE_LAYER_BUILDS: Cell<u64> = const { Cell::new(0) };
    static ROLLUP_WALKS: Cell<u64> = const { Cell::new(0) };
    static SUSPECT_WALKS: Cell<u64> = const { Cell::new(0) };
    static DISMISSALS_VISITED: Cell<u64> = const { Cell::new(0) };
}

/// Test hook: this thread's count of dismissed mentions the state layer visited. A layer
/// applied to some blocks visits only the dismissals of those blocks.
#[doc(hidden)]
pub fn mention_dismissals_visited() -> u64 {
    DISMISSALS_VISITED.with(Cell::get)
}

/// Test hook: this thread's count of coverage inputs that read the suspects of every edge of a
/// scope's graph. Inputs carried across a write read only the blocks whose suspects can differ
/// and leave it unchanged.
#[doc(hidden)]
pub fn suspect_walks() -> u64 {
    SUSPECT_WALKS.with(Cell::get)
}

/// Test hook: this thread's count of scope rollups added up over every entry of a scope. A
/// rollup carried across a write takes out and puts in only the entries the carry rebuilt,
/// and leaves it unchanged.
#[doc(hidden)]
pub fn rollup_walks() -> u64 {
    ROLLUP_WALKS.with(Cell::get)
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
    let inputs = Inputs::of(st, &scope, None)?;
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
    /// perf4: the block entries briefings of this state worked out on their own, by scope
    /// label (at most [`LOCAL_CAP`] a label), so a second briefing of the state reads them.
    /// Never carried to the next state.
    local: Mutex<BTreeMap<String, LocalEntries>>,
}

/// Block entries worked out one by one; `None` for a block that is not scored.
type LocalEntries = BTreeMap<BlockId, Option<Arc<BlockEntry>>>;

/// The most block entries a state keeps per scope for briefings that work out their own.
const LOCAL_CAP: usize = 20_000;

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
            local: Mutex::default(),
        }
    }
}

/// One scope's memoised coverage: its issues, their rollup and summary once read, and what
/// the issues were computed from. `sums` are what the rollup adds up: carried from the entry
/// before when it had them, else added up on the first rollup read.
pub(crate) struct Memoised {
    issues: Arc<CoverageIssues>,
    sums: OnceLock<RollupSums>,
    rollup: OnceLock<Arc<Rollup>>,
    summary: OnceLock<Arc<CoverageSummary>>,
    inputs: Inputs,
}

/// What a scope's coverage issues are a function of (with the pack): its structural layer, its
/// build, the tension set and the tensions the scope counts, the dismissed tensions and
/// mentions, and its suspects with presentational sources and absent targets left out.
struct Inputs {
    structural: Arc<Structural>,
    m: Arc<Materialized>,
    g: Arc<Graph>,
    tensions: Arc<TensionSet>,
    counted: Counted,
    dismissals: Arc<imbl::OrdMap<Hash, Hash>>,
    mention_dismissals: Arc<imbl::OrdMap<(BlockId, BlockId), Hash>>,
    suspects: Vec<Suspect>,
}

impl Inputs {
    /// `scope`'s inputs in `st`; `scope` is one `coverage_scope` gave.
    /// `scope`'s inputs in `st`; `scope` is one `coverage_scope` gave. With `prev`, the inputs
    /// of the entry a carry starts from, the suspects are carried from its: only the blocks
    /// whose suspects can differ between the two graphs are read again (perf4: not a walk of
    /// every edge).
    fn of(st: &ReadState, scope: &Scope, prev: Option<&Inputs>) -> Result<Inputs, RdyError> {
        let structural = st.structural(scope)?;
        let tensions = st.tensions()?;
        let m = st.materialized(scope)?;
        let g = st.graph(scope)?;
        // a suspect whose target the scope does not hold is its citer's dangling reference
        // (Connected), not a pin to confirm (final review M1)
        let kept = |s: &Suspect| !st.is_presentational_source(&m, s) && m.get(&s.target).is_some();
        let suspects: Vec<Suspect> = match prev {
            Some(p) if Arc::ptr_eq(&p.g, &g) => p.suspects.clone(),
            Some(p) => {
                let cand = g.suspect_candidates(&p.g);
                let mut out: Vec<Suspect> = p
                    .suspects
                    .iter()
                    .filter(|s| !cand.contains(&s.block))
                    .cloned()
                    .collect();
                for b in &cand {
                    out.extend(g.suspects_of(b).into_iter().filter(|s| kept(s)));
                }
                // the order `Graph::suspects` gives
                out.sort_by(|a, b| (&a.block, &a.target).cmp(&(&b.block, &b.target)));
                out
            }
            None => {
                SUSPECT_WALKS.with(|c| c.set(c.get() + 1));
                g.suspects().into_iter().filter(|s| kept(s)).collect()
            }
        };
        Ok(Inputs {
            structural,
            m,
            g,
            tensions,
            counted: Counted::of(st, scope),
            dismissals: Arc::clone(&st.dismissals),
            mention_dismissals: Arc::clone(&st.mention_dismissals),
            suspects,
        })
    }

    /// What the state layer reads of these inputs.
    fn layer(&self) -> Layer<'_> {
        Layer {
            tensions: &self.tensions,
            counted: &self.counted,
            mention_dismissals: &self.mention_dismissals,
            suspects: &self.suspects,
        }
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

/// What the state layer applies to structural entries: the tension set and the tensions the
/// scope counts, the dismissed mentions, and the suspects (of the whole scope, or of the blocks
/// a briefing works out).
struct Layer<'a> {
    tensions: &'a TensionSet,
    counted: &'a Counted,
    mention_dismissals: &'a imbl::OrdMap<(BlockId, BlockId), Hash>,
    suspects: &'a [Suspect],
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

/// The memo key of `scope` (one `coverage_scope` gave: never a past landing or a proposal).
pub(super) fn memo_label(scope: &Scope) -> String {
    match scope {
        Scope::Canon | Scope::At(_) | Scope::Proposal(_) => "canon".to_string(),
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
            Scope::Canon | Scope::At(_) | Scope::Proposal(_) => Counted::Canon,
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
    pub fn is_past_landing(&self, scope: &Scope) -> bool {
        matches!(scope, Scope::At(h) if Some(*h) != self.canon_head())
    }

    /// The scope coverage is computed in for `scope`: canon's head is canon. `E_BAD_INPUT` for a
    /// past landing: coverage describes the current state only, and coverage over time belongs
    /// to rdyhub (FW-H7). `E_BAD_INPUT` for a proposal version (ls8): its view, or canon, has
    /// the coverage.
    pub fn coverage_scope(&self, scope: &Scope) -> Result<Scope, RdyError> {
        match scope {
            Scope::Proposal(v) => Err(RdyError::new(
                E_BAD_INPUT,
                format!(
                    "coverage is computed for canon and views, not for proposal:{}",
                    v.short()
                ),
                "ask for the proposal's view, or canon",
            )),
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

    /// The score and dimensions of `scope`'s coverage, read from the memoised issues: what a
    /// write reply's coverage change reads (no entity kinds, fix round 3). Memoised per state
    /// and scope. `E_BAD_INPUT` for a past landing.
    pub fn scope_rollup(&self, scope: &Scope) -> Result<Arc<Rollup>, RdyError> {
        let memo = self.memoised(scope)?;
        if let Some(r) = memo.rollup.get() {
            return Ok(Arc::clone(r));
        }
        let sums = memo.sums.get_or_init(|| {
            ROLLUP_WALKS.with(|c| c.set(c.get() + 1));
            let mut sums = RollupSums::new(&self.pack);
            for e in memo.issues.blocks.values() {
                sums.add(e);
            }
            sums
        });
        let r = Arc::new(sums.rollup(&self.pack));
        Ok(Arc::clone(memo.rollup.get_or_init(|| r)))
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
        let whole = self.scope_rollup(scope)?;
        let issues_total = memo.issues.blocks.values().map(|e| e.issues.len()).sum();
        let m = self.materialized(scope)?;
        let (kinds, similar_kinds) = kinds_of(&m);
        let s = Arc::new(CoverageSummary {
            score: whole.score,
            dimensions: whole.dimensions.clone(),
            issues_total,
            kinds,
            similar_kinds,
        });
        Ok(Arc::clone(memo.summary.get_or_init(|| s)))
    }

    /// The memo entry of `scope`, built on first read.
    fn memoised(&self, scope: &Scope) -> Result<Arc<Memoised>, RdyError> {
        Ok(self
            .memo_entry(scope, true)?
            .expect("a full memo read always gives an entry"))
    }

    /// The memoised coverage issues of `scope` when they are memoised already or a carry from
    /// the entry before gives them; `None` when only a full build of the whole scope would
    /// (perf4: a briefing then works out its own blocks' entries instead). `E_BAD_INPUT` for a
    /// past landing.
    pub(crate) fn coverage_issues_if_cheap(
        &self,
        scope: &Scope,
    ) -> Result<Option<Arc<CoverageIssues>>, RdyError> {
        Ok(self
            .memo_entry(scope, false)?
            .map(|m| Arc::clone(&m.issues)))
    }

    /// The memo entry of `scope`: memoised, or carried from the entry before, or (when `full`)
    /// built in full; `None` when none of these is allowed.
    fn memo_entry(&self, scope: &Scope, full: bool) -> Result<Option<Arc<Memoised>>, RdyError> {
        let scope = &self.coverage_scope(scope)?;
        let label = memo_label(scope);
        {
            let memo = self
                .coverage_memo
                .entries
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some((_, hit)) = memo.iter().find(|(l, _)| *l == label) {
                return Ok(Some(Arc::clone(hit)));
            }
        }
        let prev = self
            .coverage_memo
            .prev
            .iter()
            .find(|(l, _)| *l == label)
            .map(|(_, p)| p);
        // perf4: when only a full build would give the entry, a caller that does not want one
        // pays for no inputs (the suspects are a walk of the whole graph)
        if !full && !prev.is_some_and(|p| self.may_carry(scope, p)) {
            return Ok(None);
        }
        let inputs = Inputs::of(self, scope, prev.map(|p| &p.inputs))?;
        let built = match prev.and_then(|p| self.carried_issues(scope, p, &inputs)) {
            Some(Carried::Same) => Arc::clone(prev.expect("a carry needs the entry before")),
            Some(Carried::Issues(issues, sums)) => Arc::new(Memoised {
                issues: Arc::new(issues),
                sums: sums.map(OnceLock::from).unwrap_or_default(),
                rollup: OnceLock::new(),
                summary: OnceLock::new(),
                inputs,
            }),
            None if !full => return Ok(None),
            None => Arc::new(Memoised {
                issues: Arc::new(self.full_issues(&inputs)),
                sums: OnceLock::new(),
                rollup: OnceLock::new(),
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
            return Ok(Some(Arc::clone(hit)));
        }
        memo.push_back((label, Arc::clone(&built)));
        while memo.len() > MEMO_SCOPES {
            memo.pop_front();
        }
        Ok(Some(built))
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
        self.apply_state(&mut blocks, &inputs.layer(), None);
        CoverageIssues {
            blocks,
            mentions: inputs.structural.mentions.clone(),
        }
    }

    /// Whether a carry from `prev` can give the coverage issues of `scope` (what
    /// [`ReadState::carried_issues`] checks first, read without building anything): the
    /// dismissed tensions and mentions are `prev`'s, and the structural layer is `prev`'s or
    /// carried from it.
    fn may_carry(&self, scope: &Scope, prev: &Memoised) -> bool {
        let was = &prev.inputs;
        if !Arc::ptr_eq(&was.dismissals, &self.dismissals)
            || !Arc::ptr_eq(&was.mention_dismissals, &self.mention_dismissals)
        {
            return false;
        }
        let Ok(built) = self.built(scope) else {
            return false;
        };
        match (built.structural.get(), built.structural_from.get()) {
            (Some(s), _) if Arc::ptr_eq(s, &was.structural) => true,
            (Some(_), Some((from, _))) => Arc::ptr_eq(from, &was.structural),
            _ => false,
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
        let same_counted = was.counted == inputs.counted;
        if !same_counted
            && let (Counted::View(a), Counted::View(b)) = (&was.counted, &inputs.counted)
        {
            // perf4: a view's write adds a change to its set; a tension's count can change only
            // when one of its sides is a change that came or went
            for c in a.symmetric_difference(b) {
                for set in [&was.tensions, &inputs.tensions] {
                    for t in set.involving_change(c) {
                        dirty.extend(t.blocks());
                    }
                }
            }
        }
        if same_counted
            || matches!(
                (&was.counted, &inputs.counted),
                (Counted::View(_), Counted::View(_))
            )
        {
            // perf4: the tensions the two sets differ in, the structure they share skipped,
            // instead of a walk of every tension of both
            if !Arc::ptr_eq(&was.tensions, &inputs.tensions) {
                for (old, new) in was.tensions.changed(&inputs.tensions) {
                    if let Some(t) = old
                        && was.counted.counts(was.tensions.from_canon(&t.id), t)
                    {
                        dirty.extend(t.blocks());
                    }
                    if let Some(u) = new
                        && inputs.counted.counts(inputs.tensions.from_canon(&u.id), u)
                    {
                        dirty.extend(u.blocks());
                    }
                }
            }
        } else {
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
            // the blocks of the suspects that came, went or changed (perf4: not of every one)
            let key = |s: &'_ Suspect| (s.block.clone(), s.target.clone(), s.reason.clone());
            let a: BTreeSet<_> = was.suspects.iter().map(key).collect();
            let b: BTreeSet<_> = inputs.suspects.iter().map(key).collect();
            dirty.extend(
                a.symmetric_difference(&b)
                    .map(|(block, _, _)| block.clone()),
            );
        }
        let sums = prev.sums.get().filter(|s| s.fits(&self.pack)).copied();
        if dirty.is_empty() {
            return Some(if Arc::ptr_eq(&was.m, &inputs.m) {
                Carried::Same
            } else {
                Carried::Issues((*prev.issues).clone(), sums)
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
        self.apply_state(&mut blocks, &inputs.layer(), Some(&dirty));
        // the rollup's sums: the old entries of the rebuilt blocks out, their new ones in
        let sums = sums.map(|mut s| {
            for id in &dirty {
                if let Some(e) = prev.issues.blocks.get(id) {
                    s.sub(e);
                }
                if let Some(e) = blocks.get(id) {
                    s.add(e);
                }
            }
            s
        });
        Some(Carried::Issues(
            CoverageIssues {
                blocks,
                mentions: inputs.structural.mentions.clone(),
            },
            sums,
        ))
    }

    /// Applies the state layer of `inputs` to `blocks` (structural entries): the dismissed
    /// mentions settled, the Consistent issue of each counted tension on each block it
    /// involves, and the Current issue of each suspect on its block; only to the blocks in
    /// `only`, when given.
    fn apply_state(
        &self,
        blocks: &mut imbl::OrdMap<BlockId, BlockEntry>,
        inputs: &Layer<'_>,
        only: Option<&BTreeSet<BlockId>>,
    ) {
        let wanted = |b: &BlockId| only.is_none_or(|o| o.contains(b));
        let mut touched: BTreeSet<BlockId> = BTreeSet::new();

        // a dismissed mention is settled: its check stays, its issue goes (D-E6JD1); for some
        // blocks, only their own dismissals, read by range (the map is keyed by block first)
        let dismissed: Box<dyn Iterator<Item = &(BlockId, BlockId)>> = match only {
            Some(o) => Box::new(o.iter().flat_map(|b| {
                inputs
                    .mention_dismissals
                    .range((b.clone(), BlockId::MIN)..)
                    .map(|(k, _)| k)
                    .take_while(move |(x, _)| x == b)
            })),
            None => Box::new(inputs.mention_dismissals.keys()),
        };
        for (block, entity) in dismissed {
            DISMISSALS_VISITED.with(|c| c.set(c.get() + 1));
            if let Some(e) = blocks.get_mut(block) {
                let subject = entity.to_string();
                e.issues.retain(|i| {
                    !(i.code == IssueCode::UnlinkedMention
                        && i.subject.as_deref() == Some(subject.as_str()))
                });
            }
        }

        // Consistent: each counted tension, once per block it involves; for some blocks, the
        // tensions that involve them, by id as the set lists them (perf4: not every tension)
        let set = inputs.tensions;
        let involving: Option<Vec<Arc<Tension>>> = only.map(|o| set.involving_blocks(o));
        let tensions: Box<dyn Iterator<Item = &Arc<Tension>>> = match &involving {
            Some(found) => Box::new(found.iter()),
            None => Box::new(set.iter()),
        };
        for t in tensions {
            if !inputs.counted.counts(set.from_canon(&t.id), t) {
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
        for s in inputs.suspects {
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

thread_local! {
    static LOCAL_ENTRIES: Cell<u64> = const { Cell::new(0) };
}

/// Test hook: this thread's count of block entries briefings worked out on their own, with no
/// memoised coverage to read (perf4).
#[doc(hidden)]
pub fn local_entries() -> u64 {
    LOCAL_ENTRIES.with(Cell::get)
}

/// The coverage entries a briefing reads (perf4): the scope's memoised issues when they are
/// memoised or a carry gives them, else the entries of the blocks the briefing asks for,
/// worked out one by one from the state. Either way an entry equals the one a full build of
/// the scope gives; a block that is not scored has none.
pub(crate) struct ScopedEntries {
    memo: Option<Arc<CoverageIssues>>,
    local: LocalEntries,
    /// What the batches worked out so far share, for the scope (by memo label) they read:
    /// the tensions it counts and the entry context's memo, built once per briefing.
    shared: Option<(String, Arc<Counted>, CxMemo)>,
}

impl ScopedEntries {
    /// The entry of block `id`: memoised, or worked out by [`ReadState::fill_entries`].
    pub(crate) fn get(&self, id: &BlockId) -> Option<&BlockEntry> {
        match &self.memo {
            Some(m) => m.blocks.get(id),
            None => self.local.get(id).and_then(Option::as_deref),
        }
    }
}

impl ReadState {
    /// The coverage entries of `scope` a briefing reads: memoised ones when they are cheap to
    /// have (`coverage_issues_if_cheap`), else none yet ([`ReadState::fill_entries`] works
    /// them out). It never builds the coverage of the whole scope. `E_BAD_INPUT` for a past
    /// landing.
    pub(crate) fn scoped_entries(&self, scope: &Scope) -> Result<ScopedEntries, RdyError> {
        Ok(ScopedEntries {
            memo: self.coverage_issues_if_cheap(scope)?,
            local: BTreeMap::new(),
            shared: None,
        })
    }

    /// Works out the entries of `ids` that `entries` does not hold yet, when it reads no memo:
    /// each block's structural entry ([`entry_of`], local queries only), then the state layer
    /// on those blocks alone: the dismissed mentions, the counted tensions that involve them and
    /// their own suspects. The cost follows `ids`, not the scope.
    pub(crate) fn fill_entries<'a>(
        &self,
        scope: &Scope,
        entries: &mut ScopedEntries,
        ids: impl IntoIterator<Item = &'a BlockId>,
    ) -> Result<(), RdyError> {
        if entries.memo.is_some() {
            return Ok(());
        }
        let scope = &self.coverage_scope(scope)?;
        let label = memo_label(scope);
        let mut new: BTreeSet<BlockId> = BTreeSet::new();
        {
            let cache = self
                .coverage_memo
                .local
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let cached = cache.get(&label);
            for id in ids {
                if entries.local.contains_key(id) {
                    continue;
                }
                match cached.and_then(|c| c.get(id)) {
                    Some(e) => {
                        entries.local.insert(id.clone(), e.clone());
                    }
                    None => {
                        new.insert(id.clone());
                    }
                }
            }
        }
        if new.is_empty() {
            return Ok(());
        }
        let m = self.materialized(scope)?;
        let g = self.graph(scope)?;
        let lex = self.lexicon(scope)?;
        let tensions = self.tensions()?;
        let (counted, memo) = match entries.shared.take() {
            Some((l, counted, memo)) if l == label => (counted, memo),
            _ => (Arc::new(Counted::of(self, scope)), CxMemo::default()),
        };
        let cx = EntryCx::resume(&m, &g, &lex, self.pack(), memo);
        let mut blocks: imbl::OrdMap<BlockId, BlockEntry> = imbl::OrdMap::new();
        let mut suspects: Vec<Suspect> = vec![];
        for id in &new {
            LOCAL_ENTRIES.with(|c| c.set(c.get() + 1));
            if let Some((entry, _)) = entry_of(&cx, id) {
                blocks.insert(id.clone(), entry);
                // as `Inputs::of` keeps them: no presentational source, no absent target
                suspects.extend(g.suspects_of(id).into_iter().filter(|s| {
                    !self.is_presentational_source(&m, s) && m.get(&s.target).is_some()
                }));
            }
        }
        entries.shared = Some((label.clone(), Arc::clone(&counted), cx.into_memo()));
        let layer = Layer {
            tensions: &tensions,
            counted: &counted,
            mention_dismissals: &self.mention_dismissals,
            suspects: &suspects,
        };
        self.apply_state(&mut blocks, &layer, Some(&new));
        let mut worked: LocalEntries = new.into_iter().map(|id| (id, None)).collect();
        for (id, e) in blocks {
            worked.insert(id, Some(Arc::new(e)));
        }
        let mut cache = self
            .coverage_memo
            .local
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let kept = cache.entry(label).or_default();
        for (id, e) in worked {
            if kept.len() < LOCAL_CAP {
                kept.insert(id.clone(), e.clone());
            }
            entries.local.insert(id, e);
        }
        Ok(())
    }
}

/// What a carry gives: the entry before, whole (nothing it read changed), or new issues, with
/// their rollup's sums when the entry before had them.
enum Carried {
    Same,
    Issues(CoverageIssues, Option<RollupSums>),
}
