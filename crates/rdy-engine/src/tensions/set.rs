//! The tension set of a published state: the canon part (memoised per canon build for the
//! state's own detectors) and the in-flight part, built on first read.
use super::index::{DetectCx, Writes, canon_index_of};
use super::{DetectScope, Detector, SideRef, Tension, TensionStatus, tension_from, tension_label};
use crate::error::*;
use crate::gate::applied_ops;
use crate::views::clip;
use crate::{InflightSet, RdyError, ReadState, Scope};
use imbl::ordmap::DiffItem;
use imbl::{OrdMap, OrdSet};
use rdy_model::{Block, BlockId, Change, Hash, Op, decision};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

/// The canon part of a tension set (FW-CH6): the tensions whose sides are all canon, by id,
/// with their index by block. Memoised per canon build and shared by every set built on it, so
/// a build does not copy it and a diff between two such sets skips it.
#[derive(Clone, Debug, Default)]
pub struct CanonPart {
    tensions: OrdMap<Hash, Arc<Tension>>,
    by_block: OrdMap<BlockId, OrdSet<Hash>>,
}

impl CanonPart {
    /// The part of `tensions` (a repeated id keeps the first).
    fn of(tensions: impl IntoIterator<Item = Arc<Tension>>) -> CanonPart {
        let mut part = CanonPart::default();
        for t in tensions {
            if part.tensions.contains_key(&t.id) {
                continue;
            }
            for b in t.blocks() {
                part.by_block.entry(b).or_default().insert(t.id);
            }
            part.tensions.insert(t.id, t);
        }
        part
    }
}

/// The event-derived tensions of one state, by id: the canon part (shared) and the in-flight
/// part, with indexes by block and by change. The two parts never share an id: an in-flight
/// tension has a change side, a canon one none.
#[derive(Clone, Debug, Default)]
pub struct TensionSet {
    canon: Arc<CanonPart>,
    /// The in-flight part.
    tensions: OrdMap<Hash, Arc<Tension>>,
    by_block: OrdMap<BlockId, OrdSet<Hash>>,
    by_change: OrdMap<Hash, OrdSet<Hash>>,
    /// The in-flight writes the set was built from.
    writes: Arc<Writes>,
}

/// The entries of two maps with disjoint keys, by key.
fn merged<'a>(
    a: &'a OrdMap<Hash, Arc<Tension>>,
    b: &'a OrdMap<Hash, Arc<Tension>>,
) -> impl Iterator<Item = &'a Arc<Tension>> {
    let (mut a, mut b) = (a.iter().peekable(), b.iter().peekable());
    std::iter::from_fn(move || match (a.peek(), b.peek()) {
        (Some((x, _)), Some((y, _))) if x <= y => a.next().map(|(_, t)| t),
        (Some(_), Some(_)) | (None, Some(_)) => b.next().map(|(_, t)| t),
        (Some(_), None) => a.next().map(|(_, t)| t),
        (None, None) => None,
    })
}

impl TensionSet {
    /// The set of the canon part `canon` and the in-flight tensions `inflight`, built from
    /// `writes`: a repeated id keeps the first.
    fn of(
        canon: Arc<CanonPart>,
        inflight: impl IntoIterator<Item = Arc<Tension>>,
        writes: Arc<Writes>,
    ) -> TensionSet {
        let mut set = TensionSet {
            canon,
            writes,
            ..TensionSet::default()
        };
        for t in inflight {
            set.insert(t);
        }
        set
    }

    /// Adds in-flight tension `t` unless its id is in the set already; true if it was added.
    fn insert(&mut self, t: Arc<Tension>) -> bool {
        if self.tensions.contains_key(&t.id) || self.canon.tensions.contains_key(&t.id) {
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

    /// Removes in-flight tension `id` and its index entries; true if it was in the set.
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
        true
    }

    pub fn len(&self) -> usize {
        self.canon.tensions.len() + self.tensions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every tension, by id.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<Tension>> {
        merged(&self.canon.tensions, &self.tensions)
    }

    pub fn get(&self, id: &Hash) -> Option<&Arc<Tension>> {
        self.tensions
            .get(id)
            .or_else(|| self.canon.tensions.get(id))
    }

    /// The in-flight writes the set was built from.
    pub fn writes(&self) -> &Arc<Writes> {
        &self.writes
    }

    /// Whether tension `id` came from the canon part (every side canon).
    pub fn from_canon(&self, id: &Hash) -> bool {
        self.canon.tensions.contains_key(id)
    }

    /// Whether this set and `other` share their canon part (built on one canon build).
    pub(crate) fn shares_canon(&self, other: &TensionSet) -> bool {
        Arc::ptr_eq(&self.canon, &other.canon)
    }

    /// The tensions with an id in `ids`, by id.
    fn by_ids<'a>(&self, ids: impl IntoIterator<Item = &'a Hash>) -> Vec<Arc<Tension>> {
        let ids: BTreeSet<&Hash> = ids.into_iter().collect();
        ids.into_iter()
            .filter_map(|id| self.get(id).cloned())
            .collect()
    }

    /// The tensions that involve block `b`, by id.
    pub fn involving_block(&self, b: &BlockId) -> Vec<Arc<Tension>> {
        self.by_ids(
            self.canon
                .by_block
                .get(b)
                .into_iter()
                .flatten()
                .chain(self.by_block.get(b).into_iter().flatten()),
        )
    }

    /// The tensions with change `c` as a side, by id (in-flight ones only).
    pub fn involving_change(&self, c: &Hash) -> Vec<Arc<Tension>> {
        self.by_ids(self.by_change.get(c).into_iter().flatten())
    }

    /// The tension `text` names: `tn-` and a hex prefix, or the prefix alone (at least 6 hex
    /// digits), matching exactly one tension.
    pub fn resolve(&self, text: &str) -> Result<Arc<Tension>, RdyError> {
        resolve_among(self.iter(), text)
    }

    /// The tensions `newer` has that this set has not (opened), and those this set has that
    /// `newer` has not (resolved), each by id. The structure a carry shares with the set it
    /// started from is skipped, and so is a canon part both sets share, so the cost follows
    /// what changed, not the size of the sets. Also how many entries the diff examined.
    pub fn diff(&self, newer: &TensionSet) -> (Vec<Arc<Tension>>, Vec<Arc<Tension>>) {
        let d = self.diff_counted(newer);
        (d.opened, d.resolved)
    }

    /// `diff`, with the number of entries it examined.
    fn diff_counted(&self, newer: &TensionSet) -> TensionDelta {
        let mut d = TensionDelta::default();
        let mut parts = vec![(&self.tensions, &newer.tensions)];
        d.examined = self.tensions.len() + newer.tensions.len();
        if !self.shares_canon(newer) {
            parts.push((&self.canon.tensions, &newer.canon.tensions));
            d.examined += self.canon.tensions.len() + newer.canon.tensions.len();
        }
        for (old, new) in parts {
            for item in old.diff(new) {
                match item {
                    DiffItem::Add(_, t) => d.opened.push(Arc::clone(t)),
                    DiffItem::Remove(_, t) => d.resolved.push(Arc::clone(t)),
                    // one id is one tension: same kind, sides and involved items
                    DiffItem::Update { .. } => {}
                }
            }
        }
        d.opened.sort_by_key(|t| t.id);
        d.resolved.sort_by_key(|t| t.id);
        d
    }

    /// The same opened and resolved lists as `diff`, found from what a step can touch: the
    /// tensions with one of `changes` as a side and those that involve one of `blocks`, in
    /// either set, each checked against the other. Exact only when every tension the step
    /// opened or resolved involves one of them: `newer` is this set carried across the one
    /// change in `changes`, and `blocks` are the blocks the carry re-detected around (its seeds
    /// and every detector's reach: a carry only adds or removes tensions around them). `tension_delta` checks that before it calls this. Its cost follows
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
                candidates.extend(set.canon.by_block.get(b).into_iter().flatten().copied());
            }
        }
        let mut delta = TensionDelta {
            examined: candidates.len(),
            ..TensionDelta::default()
        };
        for id in &candidates {
            match (self.get(id), newer.get(id)) {
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
/// and the blocks the carry re-detected around read (`diff_scoped`). Every other step diffs the two sets whole: a commit that
/// does not carry (a delete, a structural update, a move, a lifecycle change) and a view op
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
            let blocks = match after.tensions.around.get() {
                Some(b) => b.clone(),
                None => seeds_of(after, change)?,
            };
            Ok(old.diff_scoped(&new, &[*change], &blocks))
        }
        _ => Ok(old.diff_counted(&new)),
    }
}

/// The one tension of `tensions` that `text` names: `tn-` and a hex prefix, or the prefix alone
/// (at least 6 hex digits).
pub(crate) fn resolve_among<'a>(
    tensions: impl IntoIterator<Item = &'a Arc<Tension>>,
    text: &str,
) -> Result<Arc<Tension>, RdyError> {
    find_among(tensions, text)?.ok_or_else(|| no_tension(text))
}

/// `E_NOT_FOUND` for a tension `text` names that no list holds.
pub(crate) fn no_tension(text: &str) -> RdyError {
    RdyError::new(
        E_NOT_FOUND,
        format!("no tension {}", clip(text)),
        "rdy_tensions lists them; a tension whose side changed has a new id",
    )
}

/// The lowercase hex of a tension id `text` (`tn-` and 6 to 64 hex digits, or the hex alone),
/// or `None` when `text` is no tension id.
pub(crate) fn tension_hex(text: &str) -> Option<String> {
    let hex = text
        .strip_prefix("tn-")
        .unwrap_or(text)
        .to_ascii_lowercase();
    ((6..=64).contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hex)
}

/// The tensions of `tensions` whose id starts with `hex`.
pub(crate) fn matching<'a>(
    tensions: impl IntoIterator<Item = &'a Arc<Tension>>,
    hex: &str,
) -> impl Iterator<Item = &'a Arc<Tension>> {
    tensions
        .into_iter()
        .filter(move |t| t.id.to_string().starts_with(hex))
}

/// The tension of `tensions` that `text` names, or `None` when none matches (or `text` is no
/// tension id); `E_NOT_FOUND` when several match.
pub(crate) fn find_among<'a>(
    tensions: impl IntoIterator<Item = &'a Arc<Tension>>,
    text: &str,
) -> Result<Option<Arc<Tension>>, RdyError> {
    let Some(hex) = tension_hex(text) else {
        return Ok(None);
    };
    let found: Vec<&Arc<Tension>> = matching(tensions, &hex).collect();
    match found.as_slice() {
        [] => Ok(None),
        [t] => Ok(Some(Arc::clone(t))),
        many => Err(ambiguous(text, many.iter().copied())),
    }
}

/// `E_NOT_FOUND` for a tension `text` names that several tensions (`many`) match.
pub(crate) fn ambiguous<'a>(
    text: &str,
    many: impl ExactSizeIterator<Item = &'a Arc<Tension>>,
) -> RdyError {
    let n = many.len();
    RdyError::new(
        E_NOT_FOUND,
        format!("tension {} is ambiguous ({n} tensions)", clip(text)),
        format!(
            "use more characters: it matches {}",
            many.take(5)
                .map(|t| tension_label(&t.id))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
}

thread_local! {
    /// This thread's count of full tension-set builds (`ReadState::tensions` with no carry).
    static FULL_BUILDS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn count_full_build() {
    FULL_BUILDS.with(|c| c.set(c.get() + 1));
}

/// This thread's count of full tension-set builds so far (a state's set built without a
/// carry). A test reads the difference over a call to check that the call built no whole set.
#[doc(hidden)]
pub fn full_builds() -> u64 {
    FULL_BUILDS.with(std::cell::Cell::get)
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
    /// The blocks a carry re-detected around (its seeds and every detector's reach); set
    /// before `carried` is.
    around: OnceLock<BTreeSet<BlockId>>,
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
/// tension that involves a seed, and less each kind's tensions that involve a block of its
/// [`Detector::reach`]; and what the state's detectors find in flight around the seeds and the
/// reach that involves one of them. With the blocks it re-detected around (seeds and every
/// reach). `None` when the state's in-flight set is not `prev`'s plus `change`. It starts from a
/// clone of `prev` (structure shared), so its cost follows the seeds' tensions, not the size of
/// the set.
fn carry(
    st: &ReadState,
    prev: &TensionSet,
    change: Hash,
    view: &str,
) -> Result<Option<(TensionSet, BTreeSet<BlockId>)>, RdyError> {
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
    // the canon part stays: canon did not move
    for seed in &seeds {
        for id in prev.by_block.get(seed).into_iter().flatten() {
            next.remove(id);
        }
    }
    let cx = DetectCx::new(st, &writes, canon_index_of(st)?, DetectScope::Inflight);
    let mut around_all = seeds.clone();
    for d in st.detectors {
        let reach = d.reach(&cx, &seeds)?;
        for b in reach.difference(&seeds) {
            for id in prev.by_block.get(b).into_iter().flatten() {
                if prev.tensions.get(id).is_some_and(|t| t.kind == d.kind()) {
                    next.remove(id);
                }
            }
        }
        let mut around = seeds.clone();
        around.extend(reach);
        let mut found = vec![];
        d.detect(&cx, Some(&around), &mut found)?;
        for t in found
            .into_iter()
            .filter_map(|f| tension_from(st.pack(), f, false, DetectScope::Inflight))
            .filter(|t| t.involved.iter().any(|i| around.contains(&i.block)))
        {
            next.insert(Arc::new(t));
        }
        around_all.extend(around);
    }
    Ok(Some((next, around_all)))
}

/// The canon part for `detectors`: each run in the canon scope (always a full run), kept when
/// every side is canon, by id (a repeated id keeps the first).
fn canon_part(st: &ReadState, detectors: &[&dyn Detector]) -> Result<CanonPart, RdyError> {
    let writes = Writes::empty();
    let cx = DetectCx::new(st, &writes, canon_index_of(st)?, DetectScope::Canon);
    let mut out = vec![];
    for d in detectors {
        let mut found = vec![];
        d.detect(&cx, None, &mut found)?;
        out.extend(
            found
                .into_iter()
                .filter_map(|f| tension_from(st.pack(), f, false, DetectScope::Canon))
                .map(Arc::new),
        );
    }
    Ok(CanonPart::of(out))
}

/// The full build for `detectors`: the canon part (canon's memo when `memo`, shared, not
/// copied) and the in-flight part.
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
    Ok(TensionSet::of(canon, inflight, writes))
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
        let applied = self.landing_order(&read);
        self.landing_tensions_applied(view, changes, &read, &applied)
    }

    /// FW-JU1 in a view listing: the tensions of the kinds judged on the net change
    /// ([`super::judged_on_the_net_change`]) in their landing form over every change view `view`
    /// includes, which is what proposing the whole view lists. By id; none for a view with no
    /// changes. A listing scoped to the view shows these in place of the in-flight forms of
    /// those kinds, so it never shows another view's unjustified change that the view's own
    /// proposal does not judge.
    pub fn view_net_tensions(&self, view: &str) -> Result<Vec<Arc<Tension>>, RdyError> {
        let included = self.included(view);
        if included.is_empty() {
            return Ok(vec![]);
        }
        Ok(self
            .landing_tensions(view, &included)?
            .into_iter()
            .filter(|t| super::judged_on_the_net_change(t.kind))
            .collect())
    }

    /// The order the landing of `read` applies its changes in: the merge's onto canon, or the
    /// list order when the merge refuses (the gate reports that refusal itself).
    pub(crate) fn landing_order(&self, read: &[Change]) -> Vec<usize> {
        self.canon_snapshot()
            .and_then(|canon| Ok(self.reader().merge_checked(&canon, read)?))
            .map_or_else(|_| (0..read.len()).collect(), |(_, at)| at)
    }

    /// [`ReadState::landing_tensions`] for `changes`, read (`read`) and applied in the order
    /// `applied` ([`ReadState::landing_order`]).
    pub(crate) fn landing_tensions_applied(
        &self,
        view: &str,
        changes: &[Hash],
        read: &[Change],
        applied: &[usize],
    ) -> Result<Vec<Arc<Tension>>, RdyError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        // the canon the landing would give: the ops in the order the merge applies the changes
        let ops = applied_ops(read, applied);
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
        for c in read {
            seeds_into(self, c, &mut seeds)?;
        }
        let cx = DetectCx::landing(
            self,
            &writes,
            canon_index_of(self)?,
            Arc::new(m),
            Arc::new(g),
            touched.into_iter().cloned().collect(),
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
                    && let Ok(Some((set, around))) = carry(self, prev, *change, view)
                {
                    let _ = cell.around.set(around);
                    let _ = cell.carried.set(true);
                    return Ok(Arc::new(set));
                }
                let _ = cell.carried.set(false);
                count_full_build();
                build(self, self.detectors, true).map(Arc::new)
            })
            .clone()
    }

    /// The state's tension set when it is built, or when its carry runs (the cost follows the
    /// committed change); `None` when only a full build would give it. Never builds the whole
    /// set: a carry that cannot run leaves the cell unbuilt.
    pub(crate) fn tensions_without_build(&self) -> Option<Arc<TensionSet>> {
        if self.error.is_some() {
            return None;
        }
        let cell = &self.tensions;
        if let Some(r) = cell.set.get() {
            return r.as_ref().ok().cloned();
        }
        let (prev, change, view) = cell.carry.as_ref()?;
        let (set, around) = carry(self, prev, *change, view).ok()??;
        // another reader may have built the cell meanwhile: its set is the same
        cell.set
            .get_or_init(|| {
                let _ = cell.around.set(around);
                let _ = cell.carried.set(true);
                Ok(Arc::new(set))
            })
            .as_ref()
            .ok()
            .cloned()
    }

    /// The in-flight tensions with one of `changes` as a side, by id: what
    /// `tensions()?.involving_change(c)` gives for each, once. Read from the set when it is
    /// built or carried; otherwise the state's detectors run seeded with the blocks the changes
    /// write (and the targets their decisions supersede or revoke) and each detector's reach,
    /// over every in-flight write, as a carry re-detects around them. Every tension with a
    /// change side involves a block that change writes, so the seeded run finds them all. It
    /// never builds the whole set, and never the canon part (a canon tension has no change
    /// side).
    pub fn tensions_of_changes(&self, changes: &[Hash]) -> Result<Vec<Arc<Tension>>, RdyError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        match self.tensions_without_build() {
            Some(set) => {
                let mut out: BTreeMap<Hash, Arc<Tension>> = BTreeMap::new();
                for c in changes {
                    for t in set.involving_change(c) {
                        out.entry(t.id).or_insert(t);
                    }
                }
                Ok(out.into_values().collect())
            }
            None => self.tensions_of_changes_seeded(changes),
        }
    }

    /// Test hook: `tensions_of_changes` by the seeded run, whatever the cell holds.
    #[doc(hidden)]
    pub fn tensions_of_changes_seeded(
        &self,
        changes: &[Hash],
    ) -> Result<Vec<Arc<Tension>>, RdyError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        let reader = self.reader();
        let mut seeds = BTreeSet::new();
        for c in changes {
            seeds_into(self, &reader.change(c)?, &mut seeds)?;
        }
        let wanted: BTreeSet<&Hash> = changes.iter().collect();
        let writes = Writes::build(self, &*self.inflight_set()?)?;
        let cx = DetectCx::new(self, &writes, canon_index_of(self)?, DetectScope::Inflight);
        let mut out: BTreeMap<Hash, Arc<Tension>> = BTreeMap::new();
        for d in self.detectors {
            let mut around = seeds.clone();
            around.extend(d.reach(&cx, &seeds)?);
            let mut found = vec![];
            d.detect(&cx, Some(&around), &mut found)?;
            for t in found
                .into_iter()
                .filter_map(|f| tension_from(self.pack(), f, false, DetectScope::Inflight))
                .filter(|t| {
                    t.sides
                        .iter()
                        .any(|s| matches!(s, SideRef::Change(c) if wanted.contains(c)))
                })
            {
                out.entry(t.id).or_insert_with(|| Arc::new(t));
            }
        }
        Ok(out.into_values().collect())
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
        Ok(canon_part(self, self.detectors)?
            .tensions
            .into_iter()
            .map(|(_, t)| t)
            .collect())
    }

    /// Every block involved in an open tension: the event-derived set (all-canon tensions
    /// included) and the read-time area overlaps at `now_ms`, leaving out the dismissed.
    pub fn open_tension_blocks(&self, now_ms: i64) -> Result<BTreeSet<BlockId>, RdyError> {
        let mut out = BTreeSet::new();
        for t in self.tensions()?.iter() {
            if self.tension_status(t) == TensionStatus::Open {
                out.extend(t.blocks());
            }
        }
        for t in self.intent_tensions(now_ms)? {
            if self.tension_status(&t) == TensionStatus::Open {
                out.extend(t.blocks());
            }
        }
        Ok(out)
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
