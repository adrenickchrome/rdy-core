//! The tension set of a published state: the canon part (memoised per canon build for the
//! state's own detectors) and the in-flight part, built on first read.
use super::index::{DetectCx, Writes, canon_index_of};
use super::{DetectScope, Detector, SideRef, Tension, tension_from, tension_label};
use crate::error::*;
use crate::gate::applied_ops;
use crate::views::clip;
use crate::{InflightSet, RdyError, ReadState, Scope};
use imbl::ordmap::DiffItem;
use imbl::{OrdMap, OrdSet};
use rdy_model::{Block, BlockId, Change, Hash, Op, decision};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

/// The event-derived tensions of one state, by id, with indexes by block and by change.
#[derive(Clone, Debug, Default)]
pub struct TensionSet {
    tensions: OrdMap<Hash, Arc<Tension>>,
    by_block: OrdMap<BlockId, OrdSet<Hash>>,
    by_change: OrdMap<Hash, OrdSet<Hash>>,
    /// The ids that came from the canon part (every side canon).
    canon: OrdSet<Hash>,
    /// The in-flight writes the set was built from.
    writes: Arc<Writes>,
}

impl TensionSet {
    /// The set of `canon` (the canon part) and `inflight` (the in-flight part) built from
    /// `writes`, inserted by id: a repeated id keeps the first.
    fn of(
        canon: &[Arc<Tension>],
        inflight: impl IntoIterator<Item = Arc<Tension>>,
        writes: Arc<Writes>,
    ) -> TensionSet {
        let mut set = TensionSet {
            writes,
            ..TensionSet::default()
        };
        for t in canon {
            if set.insert(Arc::clone(t)) {
                set.canon.insert(t.id);
            }
        }
        for t in inflight {
            set.insert(t);
        }
        set
    }

    /// Adds `t` unless its id is in the set already; true if it was added.
    fn insert(&mut self, t: Arc<Tension>) -> bool {
        if self.tensions.contains_key(&t.id) {
            return false;
        }
        for b in t.blocks() {
            self.by_block.entry(b).or_default().insert(t.id);
        }
        for s in &t.sides {
            if let SideRef::Change(c) = s {
                self.by_change.entry(*c).or_default().insert(t.id);
            }
        }
        self.tensions.insert(t.id, t);
        true
    }

    /// Removes tension `id` and its index entries; true if it was in the set.
    fn remove(&mut self, id: &Hash) -> bool {
        let Some(t) = self.tensions.remove(id) else {
            return false;
        };
        for b in t.blocks() {
            if let Some(ids) = self.by_block.get_mut(&b) {
                ids.remove(id);
                if ids.is_empty() {
                    self.by_block.remove(&b);
                }
            }
        }
        for side in &t.sides {
            if let SideRef::Change(c) = side
                && let Some(ids) = self.by_change.get_mut(c)
            {
                ids.remove(id);
                if ids.is_empty() {
                    self.by_change.remove(c);
                }
            }
        }
        self.canon.remove(id);
        true
    }

    pub fn len(&self) -> usize {
        self.tensions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tensions.is_empty()
    }

    /// Every tension, by id.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<Tension>> {
        self.tensions.values()
    }

    pub fn get(&self, id: &Hash) -> Option<&Arc<Tension>> {
        self.tensions.get(id)
    }

    /// The in-flight writes the set was built from.
    pub fn writes(&self) -> &Arc<Writes> {
        &self.writes
    }

    /// Whether tension `id` came from the canon part (every side canon).
    pub fn from_canon(&self, id: &Hash) -> bool {
        self.canon.contains(id)
    }

    fn by_ids(&self, ids: Option<&OrdSet<Hash>>) -> Vec<Arc<Tension>> {
        ids.into_iter()
            .flatten()
            .filter_map(|id| self.tensions.get(id).cloned())
            .collect()
    }

    /// The tensions that involve block `b`, by id.
    pub fn involving_block(&self, b: &BlockId) -> Vec<Arc<Tension>> {
        self.by_ids(self.by_block.get(b))
    }

    /// The tensions with change `c` as a side, by id.
    pub fn involving_change(&self, c: &Hash) -> Vec<Arc<Tension>> {
        self.by_ids(self.by_change.get(c))
    }

    /// The tension `text` names: `tn-` and a hex prefix, or the prefix alone (at least 6 hex
    /// digits), matching exactly one tension.
    pub fn resolve(&self, text: &str) -> Result<Arc<Tension>, RdyError> {
        resolve_among(self.tensions.values(), text)
    }

    /// The tensions `newer` has that this set has not (opened), and those this set has that
    /// `newer` has not (resolved), each by id. The structure a carry shares with the set it
    /// started from is skipped, so the cost follows what changed, not the size of the sets.
    pub fn diff(&self, newer: &TensionSet) -> (Vec<Arc<Tension>>, Vec<Arc<Tension>>) {
        let (mut opened, mut resolved) = (vec![], vec![]);
        for item in self.tensions.diff(&newer.tensions) {
            match item {
                DiffItem::Add(_, t) => opened.push(Arc::clone(t)),
                DiffItem::Remove(_, t) => resolved.push(Arc::clone(t)),
                // one id is one tension: same kind, sides and involved items
                DiffItem::Update { .. } => {}
            }
        }
        (opened, resolved)
    }

    /// The same opened and resolved lists as `diff`, found from what a step can touch: the
    /// tensions with one of `changes` as a side and those that involve one of `blocks`, in
    /// either set, each checked against the other. Exact only when every tension the step
    /// opened or resolved involves one of them: `newer` is this set carried across the one
    /// change in `changes`, and `blocks` are its seeds (a carry only adds or removes tensions
    /// around its seeds). `tension_delta` checks that before it calls this. Its cost follows
    /// those tensions, not the size of the sets.
    pub fn diff_scoped(
        &self,
        newer: &TensionSet,
        changes: &[Hash],
        blocks: &BTreeSet<BlockId>,
    ) -> TensionDelta {
        let mut candidates: BTreeSet<Hash> = BTreeSet::new();
        for set in [self, newer] {
            for c in changes {
                candidates.extend(set.by_change.get(c).into_iter().flatten().copied());
            }
            for b in blocks {
                candidates.extend(set.by_block.get(b).into_iter().flatten().copied());
            }
        }
        let mut delta = TensionDelta {
            examined: candidates.len(),
            ..TensionDelta::default()
        };
        for id in &candidates {
            match (self.tensions.get(id), newer.tensions.get(id)) {
                (None, Some(t)) => delta.opened.push(Arc::clone(t)),
                (Some(t), None) => delta.resolved.push(Arc::clone(t)),
                _ => {}
            }
        }
        delta
    }
}

/// What a step did to the tension set: the tensions it opened and those it resolved, by id.
#[derive(Clone, Debug, Default)]
pub struct TensionDelta {
    pub opened: Vec<Arc<Tension>>,
    pub resolved: Vec<Arc<Tension>>,
    /// How many tensions were looked at to find them (every tension of both sets, for a full
    /// diff).
    pub examined: usize,
}

/// What the step from `before` to `after` did to the tension set. `changes` are the changes
/// the step added or removed (`None` for a rebase or a moved base). Only when `after`'s set was
/// carried from `before`'s across the one change in `changes` are just the tensions around it
/// and its seeds read (`diff_scoped`). Every other step diffs the two sets whole: a commit that
/// does not carry (a delete, a structural write, a move, a lifecycle change) and a view op
/// (its state builds the set again) can open or resolve tensions away from the change and its
/// blocks, for example an area overlap that a deleted `affects` target gave.
pub fn tension_delta(
    before: &ReadState,
    after: &ReadState,
    changes: Option<&[Hash]>,
) -> Result<TensionDelta, RdyError> {
    let (old, new) = (before.tensions()?, after.tensions()?);
    match changes {
        Some([change]) if after.tensions_carried_from(&old, change) => {
            let blocks = seeds_of(after, change)?;
            Ok(old.diff_scoped(&new, &[*change], &blocks))
        }
        _ => {
            let (opened, resolved) = old.diff(&new);
            Ok(TensionDelta {
                opened,
                resolved,
                examined: old.len() + new.len(),
            })
        }
    }
}

/// The one tension of `tensions` that `text` names: `tn-` and a hex prefix, or the prefix alone
/// (at least 6 hex digits).
pub(crate) fn resolve_among<'a>(
    tensions: impl IntoIterator<Item = &'a Arc<Tension>>,
    text: &str,
) -> Result<Arc<Tension>, RdyError> {
    let hex = text
        .strip_prefix("tn-")
        .unwrap_or(text)
        .to_ascii_lowercase();
    let not_found = || {
        RdyError::new(
            E_NOT_FOUND,
            format!("no tension {}", clip(text)),
            "rdy_tensions lists them; a tension whose side changed has a new id",
        )
    };
    if !(6..=64).contains(&hex.len()) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(not_found());
    }
    let found: Vec<&Arc<Tension>> = tensions
        .into_iter()
        .filter(|t| t.id.to_string().starts_with(&hex))
        .collect();
    match found.as_slice() {
        [] => Err(not_found()),
        [t] => Ok(Arc::clone(t)),
        many => Err(RdyError::new(
            E_NOT_FOUND,
            format!(
                "tension {} is ambiguous ({} tensions)",
                clip(text),
                many.len()
            ),
            format!(
                "use more characters: it matches {}",
                many.iter()
                    .take(5)
                    .map(|t| tension_label(&t.id))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

/// A state's tension set, built on first read: carried from the state before when `carry` is
/// set (a commit), else a full build.
#[derive(Default)]
pub(crate) struct TensionCell {
    pub(crate) set: OnceLock<Result<Arc<TensionSet>, RdyError>>,
    /// The state before's set, the change committed and its view: the first read carries.
    pub(crate) carry: Option<(Arc<TensionSet>, Hash, String)>,
    /// Whether the set was carried; set before `set` is.
    carried: OnceLock<bool>,
}

impl TensionCell {
    /// An unbuilt cell whose first read carries `prev` across `change`, committed to `view`.
    pub(crate) fn carrying(prev: Arc<TensionSet>, change: Hash, view: String) -> TensionCell {
        TensionCell {
            carry: Some((prev, change, view)),
            ..TensionCell::default()
        }
    }
}

/// The blocks a carry re-detects around for `change`: the blocks it writes, and the
/// `supersedes` and `revokes` targets of the decisions it writes (as written, and as they were).
pub(crate) fn seeds_of(st: &ReadState, change: &Hash) -> Result<BTreeSet<BlockId>, RdyError> {
    let mut seeds = BTreeSet::new();
    seeds_into(st, &st.reader().change(change)?, &mut seeds)?;
    Ok(seeds)
}

/// Adds to `seeds` the blocks [`seeds_of`] names for change `c`.
fn seeds_into(st: &ReadState, c: &Change, seeds: &mut BTreeSet<BlockId>) -> Result<(), RdyError> {
    let mut targets = |b: &Block| {
        if b.block_type == decision::DECISION {
            for name in [decision::SUPERSEDES, decision::REVOKES] {
                for l in b.links.get(name).into_iter().flatten() {
                    seeds.insert(l.target.clone());
                }
            }
        }
    };
    for op in &c.ops {
        if let Op::Create { block } | Op::Update { block, .. } = op {
            targets(block);
        }
        if let Some(base) = op.base() {
            targets(&st.reader().get_block(&base)?);
        }
    }
    for op in &c.ops {
        seeds.insert(op.block_id().clone());
    }
    Ok(())
}

/// The carry (tcar1): `prev`'s canon part (canon did not move); its in-flight part less every
/// tension that involves a seed; and what the state's detectors find in flight around the seeds
/// that involves one. `None` when the state's in-flight set is not `prev`'s plus `change`. It
/// starts from a clone of `prev` (structure shared), so its cost follows the seeds' tensions, not
/// the size of the set.
fn carry(
    st: &ReadState,
    prev: &TensionSet,
    change: Hash,
    view: &str,
) -> Result<Option<TensionSet>, RdyError> {
    if let Some(e) = &st.error {
        return Err(e.clone());
    }
    let set = st.inflight_set()?;
    if !prev.writes.extends_to(&set, &change) {
        return Ok(None);
    }
    let writes = Arc::new(prev.writes.with_change(st, change, view)?);
    let seeds = seeds_of(st, &change)?;
    let mut next = TensionSet {
        writes: Arc::clone(&writes),
        ..prev.clone()
    };
    for seed in &seeds {
        for id in prev.by_block.get(seed).into_iter().flatten() {
            // a canon-part tension stays: canon did not move
            if !prev.from_canon(id) {
                next.remove(id);
            }
        }
    }
    let involves_seed = |t: &Tension| t.involved.iter().any(|i| seeds.contains(&i.block));
    let cx = DetectCx::new(st, &writes, canon_index_of(st)?, DetectScope::Inflight);
    for d in st.detectors {
        let mut found = vec![];
        d.detect(&cx, Some(&seeds), &mut found)?;
        for t in found
            .into_iter()
            .filter_map(|f| tension_from(st.pack(), f, false, DetectScope::Inflight))
            .filter(|t| involves_seed(t))
        {
            next.insert(Arc::new(t));
        }
    }
    Ok(Some(next))
}

/// The canon part for `detectors`: each run in the canon scope, kept when every side is canon,
/// by id (a repeated id keeps the first).
fn canon_part(st: &ReadState, detectors: &[&dyn Detector]) -> Result<Vec<Arc<Tension>>, RdyError> {
    let writes = Writes::empty();
    let cx = DetectCx::new(st, &writes, canon_index_of(st)?, DetectScope::Canon);
    let mut out = OrdMap::new();
    for d in detectors {
        let mut found = vec![];
        d.detect(&cx, None, &mut found)?;
        for f in found {
            if let Some(t) = tension_from(st.pack(), f, false, DetectScope::Canon) {
                out.entry(t.id).or_insert_with(|| Arc::new(t));
            }
        }
    }
    Ok(out.into_iter().map(|(_, t)| t).collect())
}

/// The full build for `detectors`: the canon part (canon's memo when `memo`) and the in-flight
/// part.
fn build(st: &ReadState, detectors: &[&dyn Detector], memo: bool) -> Result<TensionSet, RdyError> {
    if let Some(e) = &st.error {
        return Err(e.clone());
    }
    let canon = if memo {
        let built = st.built(&crate::Scope::Canon)?;
        built
            .tension_canon
            .get_or_init(|| canon_part(st, detectors).map(Arc::new))
            .clone()?
    } else {
        Arc::new(canon_part(st, detectors)?)
    };
    let set = st.inflight_set()?;
    let writes = Arc::new(Writes::build(st, &set)?);
    let cx = DetectCx::new(st, &writes, canon_index_of(st)?, DetectScope::Inflight);
    let mut inflight = vec![];
    for d in detectors {
        let mut found = vec![];
        d.detect(&cx, None, &mut found)?;
        inflight.extend(
            found
                .into_iter()
                .filter_map(|f| tension_from(st.pack(), f, false, DetectScope::Inflight))
                .map(Arc::new),
        );
    }
    Ok(TensionSet::of(&canon, inflight, writes))
}

impl ReadState {
    /// tgat1 (the gate judges what lands): the tensions around what landing `changes` (one
    /// version's changes, proposed from view `view`) would write. The state's detectors run
    /// seeded with the blocks the changes write (and the targets their decisions supersede or
    /// revoke), in the in-flight scope, over a context whose in-flight writes are exactly these
    /// changes, as one view (`view`) merges them, and whose in-flight state is the canon their
    /// landing would give; canon is as is. Each tension involves a seed and has a change side; a
    /// change in flight elsewhere, or a later change of the view, is not a side. By id. The cost
    /// follows the proposal, not the in-flight set.
    pub fn landing_tensions(
        &self,
        view: &str,
        changes: &[Hash],
    ) -> Result<Vec<Arc<Tension>>, RdyError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        let reader = self.reader();
        let read: Vec<Change> = changes
            .iter()
            .map(|c| reader.change(c))
            .collect::<Result<_, _>>()?;
        // the canon the landing would give: the ops in the order the merge applies the changes
        // (list order when the merge refuses: the gate reports that refusal itself)
        let applied = reader
            .state_of(self.canon_head())
            .and_then(|canon| reader.merge_checked(&canon, &read))
            .map_or_else(|_| (0..read.len()).collect(), |(_, at)| at);
        let ops = applied_ops(&read, &applied);
        let canon_m = self.materialized(&Scope::Canon)?;
        let m = canon_m.with(&ops);
        let touched: BTreeSet<&BlockId> = ops.iter().map(Op::block_id).collect();
        let g = self.graph(&Scope::Canon)?.with_changes(
            touched.iter().filter_map(|b| canon_m.get(b)),
            touched.iter().filter_map(|b| m.get(b)),
        );
        let set = InflightSet {
            changes: changes.to_vec(),
            views_of: changes
                .iter()
                .map(|c| (*c, vec![view.to_string()]))
                .collect(),
        };
        let writes = Writes::build(self, &set)?;
        let mut seeds = BTreeSet::new();
        for c in &read {
            seeds_into(self, c, &mut seeds)?;
        }
        let cx = DetectCx::landing(
            self,
            &writes,
            canon_index_of(self)?,
            Arc::new(m),
            Arc::new(g),
        );
        let mut out = BTreeMap::new();
        for d in self.detectors {
            let mut found = vec![];
            d.detect(&cx, Some(&seeds), &mut found)?;
            for t in found
                .into_iter()
                .filter_map(|f| tension_from(self.pack(), f, false, DetectScope::Inflight))
                .filter(|t| t.involved.iter().any(|i| seeds.contains(&i.block)))
            {
                out.entry(t.id).or_insert_with(|| Arc::new(t));
            }
        }
        Ok(out.into_values().collect())
    }

    /// The event-derived tensions of this state (the state's detectors over canon and every
    /// in-flight change), built on first read.
    pub fn tensions(&self) -> Result<Arc<TensionSet>, RdyError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        let cell = &self.tensions;
        cell.set
            .get_or_init(|| {
                // a carry that cannot run (another in-flight set) or fails falls back to a build
                if let Some((prev, change, view)) = &cell.carry
                    && let Ok(Some(set)) = carry(self, prev, *change, view)
                {
                    let _ = cell.carried.set(true);
                    return Ok(Arc::new(set));
                }
                let _ = cell.carried.set(false);
                build(self, self.detectors, true).map(Arc::new)
            })
            .clone()
    }

    /// Test hook: the tension set `d` finds in this state, built fresh.
    #[doc(hidden)]
    pub fn tensions_with(&self, d: &[&dyn Detector]) -> Result<TensionSet, RdyError> {
        build(self, d, false)
    }

    /// Test hook: the state's tension set built fresh (nothing memoised is read).
    #[doc(hidden)]
    pub fn tensions_rebuilt(&self) -> Result<TensionSet, RdyError> {
        build(self, self.detectors, false)
    }

    /// Test hook: the canon part of the state's tension set, built fresh.
    #[doc(hidden)]
    pub fn canon_tensions_rebuilt(&self) -> Result<Vec<Arc<Tension>>, RdyError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        canon_part(self, self.detectors)
    }

    /// Test hook: whether this state's tension set is built. Never builds.
    #[doc(hidden)]
    pub fn tensions_built(&self) -> bool {
        self.tensions.set.get().is_some()
    }

    /// Whether this state's tension set was carried from exactly `prev` across `change` (and
    /// nothing else). Never builds.
    pub(crate) fn tensions_carried_from(&self, prev: &Arc<TensionSet>, change: &Hash) -> bool {
        self.tensions_carried() == Some(true)
            && matches!(&self.tensions.carry, Some((p, c, _)) if Arc::ptr_eq(p, prev) && c == change)
    }

    /// Test hook: whether this state's tension set was carried from the state before (`None`
    /// while unbuilt).
    #[doc(hidden)]
    pub fn tensions_carried(&self) -> Option<bool> {
        self.tensions
            .set
            .get()
            .map(|_| self.tensions.carried.get().copied().unwrap_or(false))
    }
}
