//! The tension set of a published state: the canon part (memoised per canon build for the
//! state's own detectors) and the in-flight part, built on first read.
use super::index::{DetectCx, Writes, canon_index_of};
use super::{DetectScope, Detector, SideRef, Tension, tension_from, tension_label};
use crate::error::*;
use crate::views::clip;
use crate::{RdyError, ReadState};
use imbl::ordmap::DiffItem;
use imbl::{OrdMap, OrdSet};
use rdy_model::{Block, BlockId, Hash, Op, decision};
use std::collections::BTreeSet;
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
fn seeds_of(st: &ReadState, change: &Hash) -> Result<BTreeSet<BlockId>, RdyError> {
    let c = st.reader().change(change)?;
    let mut seeds = BTreeSet::new();
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
    Ok(seeds)
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
