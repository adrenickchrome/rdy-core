//! The tension set of a published state: the canon part (memoised per canon build for the
//! state's own detectors) and the in-flight part, built on first read.
use super::index::{DetectCx, Write, Writes, canon_index_of};
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

    /// This part with `tensions` added (a repeated id keeps the first; structure shared).
    fn with(&self, tensions: impl IntoIterator<Item = Arc<Tension>>) -> CanonPart {
        let mut part = self.clone();
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

    /// This part less the tensions with an id in `ids` (structure shared).
    fn without(&self, ids: &BTreeSet<Hash>) -> CanonPart {
        let mut part = self.clone();
        for id in ids {
            let Some(t) = part.tensions.remove(id) else {
                continue;
            };
            for b in t.blocks() {
                if let Some(set) = part.by_block.get_mut(&b) {
                    set.remove(id);
                    if set.is_empty() {
                        part.by_block.remove(&b);
                    }
                }
            }
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

    /// The tensions that involve any of `blocks`, each once, by id.
    pub(crate) fn involving_blocks<'a>(
        &self,
        blocks: impl IntoIterator<Item = &'a BlockId>,
    ) -> Vec<Arc<Tension>> {
        let mut ids: BTreeSet<&Hash> = BTreeSet::new();
        for b in blocks {
            ids.extend(self.canon.by_block.get(b).into_iter().flatten());
            ids.extend(self.by_block.get(b).into_iter().flatten());
        }
        ids.into_iter()
            .filter_map(|id| self.get(id).cloned())
            .collect()
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

    /// The tensions this set and `newer` differ in, as (here, in `newer`) pairs: one opened
    /// (`None`, `Some`), one resolved (`Some`, `None`), or one id whose tension differs (both).
    /// As in `diff`, the structure the two sets share is skipped, and so is a canon part they
    /// share (perf4: a carry of the coverage issues reads it).
    pub(crate) fn changed<'a>(&'a self, newer: &'a TensionSet) -> Vec<TensionChange<'a>> {
        let mut parts = vec![(&self.tensions, &newer.tensions)];
        if !self.shares_canon(newer) {
            parts.push((&self.canon.tensions, &newer.canon.tensions));
        }
        let mut out = vec![];
        for (old, new) in parts {
            for item in old.diff(new) {
                match item {
                    DiffItem::Add(_, t) => out.push((None, Some(t))),
                    DiffItem::Remove(_, t) => out.push((Some(t), None)),
                    DiffItem::Update { old, new } => {
                        if !Arc::ptr_eq(old.1, new.1) && old.1 != new.1 {
                            out.push((Some(old.1), Some(new.1)));
                        }
                    }
                }
            }
        }
        out
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

/// One tension two sets differ in: (in the older set, in the newer one).
pub(crate) type TensionChange<'a> = (Option<&'a Arc<Tension>>, Option<&'a Arc<Tension>>);

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

thread_local! {
    /// This thread's count of full canon-part runs (every non-stored kind over all of canon).
    static CANON_RUNS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// This thread's count of full canon-part runs so far (a canon build's part found by running
/// every non-stored kind over all of canon, not carried across a landing). A test reads the
/// difference over a call.
#[doc(hidden)]
pub fn canon_part_runs() -> u64 {
    CANON_RUNS.with(std::cell::Cell::get)
}

/// This thread's count of full tension-set builds so far (a state's set built without a
/// carry). A test reads the difference over a call to check that the call built no whole set.
#[doc(hidden)]
pub fn full_builds() -> u64 {
    FULL_BUILDS.with(std::cell::Cell::get)
}

/// A state's tension set, built on first read: carried from the state before when `carry` is
/// set (a commit) or `verdict` is (a verdict or an acceptance), else a full build.
#[derive(Default)]
pub(crate) struct TensionCell {
    pub(crate) set: OnceLock<Result<Arc<TensionSet>, RdyError>>,
    /// The state before's set, the change committed and its view: the first read carries.
    pub(crate) carry: Option<(Arc<TensionSet>, Hash, String)>,
    /// The state before's set and the blocks of the pairs a verdict or an acceptance wrote:
    /// the first read re-detects the stored kinds around them ([`verdict_carry`]).
    verdict: Option<(Arc<TensionSet>, Vec<BlockId>)>,
    /// The state before's set, after a view op (or a landing, with `landed`): the first read
    /// re-detects around the changes whose membership or views moved ([`step_carry`]).
    views: Option<Arc<TensionSet>>,
    /// The ops a landing applied to canon, with `views`: the first read also re-detects around
    /// the blocks they write and builds the canon part of the new canon ([`step_carry`]).
    landed: Option<Vec<Op>>,
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

    /// An unbuilt cell whose first read carries `prev` across a verdict or an acceptance on
    /// pairs of `blocks` ([`verdict_carry`]).
    pub(crate) fn after_verdict(prev: Arc<TensionSet>, blocks: Vec<BlockId>) -> TensionCell {
        TensionCell {
            verdict: Some((prev, blocks)),
            ..TensionCell::default()
        }
    }

    /// An unbuilt cell whose first read carries `prev` across a view op on the same canon:
    /// changes that entered or left flight, or moved between views ([`step_carry`]).
    pub(crate) fn after_views(prev: Arc<TensionSet>) -> TensionCell {
        TensionCell {
            views: Some(prev),
            ..TensionCell::default()
        }
    }

    /// An unbuilt cell whose first read carries `prev` across a landing that applied `ops` to
    /// canon (and took its changes out of flight) ([`step_carry`]).
    pub(crate) fn after_landing(prev: Arc<TensionSet>, ops: Vec<Op>) -> TensionCell {
        TensionCell {
            views: Some(prev),
            landed: Some(ops),
            ..TensionCell::default()
        }
    }

    /// The set this cell's first read carries to, or `None` when it has no carry or its carry
    /// cannot run (another in-flight set) or fails; with the blocks a commit's carry re-detected
    /// around (none for a verdict's or a view op's).
    fn carried(&self, st: &ReadState) -> Option<(TensionSet, Option<BTreeSet<BlockId>>)> {
        if let Some((prev, change, view)) = &self.carry {
            let (set, around) = carry(st, prev, *change, view).ok()??;
            return Some((set, Some(around)));
        }
        if let Some(prev) = &self.views {
            return Some((step_carry(st, prev, self.landed.as_deref()).ok()??, None));
        }
        let (prev, blocks) = self.verdict.as_ref()?;
        Some((verdict_carry(st, prev, blocks).ok()??, None))
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
    seeds_of_ops(st, &c.ops, seeds)
}

/// Adds to `seeds` the blocks `ops` write and the `supersedes` and `revokes` targets of the
/// decisions they write (as written, and as they were).
fn seeds_of_ops(st: &ReadState, ops: &[Op], seeds: &mut BTreeSet<BlockId>) -> Result<(), RdyError> {
    let mut targets = |b: &Block| {
        if b.block_type == decision::DECISION {
            for name in [decision::SUPERSEDES, decision::REVOKES] {
                for l in b.links.get(name).into_iter().flatten() {
                    seeds.insert(l.target.clone());
                }
            }
        }
    };
    for op in ops {
        if let Op::Create { block } | Op::Update { block, .. } = op {
            targets(block);
        }
        if let Some(base) = op.base() {
            targets(&st.reader().get_block(&base)?);
        }
    }
    for op in ops {
        seeds.insert(op.block_id().clone());
    }
    Ok(())
}

/// The carry (tcar1): `prev`'s canon part (canon did not move); its in-flight part less every
/// tension that involves a seed, and less each kind's tensions that involve a block of its
/// [`Detector::reach`]; and what the state's detectors find in flight around the seeds and the
/// reach that involves one of them. With the blocks it re-detected around (seeds and every
/// reach). `None` when the state's in-flight set is not `prev`'s plus `change`, or when the
/// change writes more blocks than were in flight ([`too_many`]: a full build costs less). It starts from a
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
    let seeds = seeds_of(st, &change)?;
    // a change that writes more blocks than were in flight (an import into an empty flight)
    // re-detects nearly everything, and a seeded run over many seeds costs more than one full
    // run: build it in full
    if too_many(&seeds, &prev.writes) {
        return Ok(None);
    }
    let writes = Arc::new(prev.writes.with_change(st, change, view)?);
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
        for t in detect_kept(st, &cx, *d, Some(&around))? {
            next.insert(t);
        }
        around_all.extend(around);
    }
    Ok(Some((next, around_all)))
}

/// Whether a step that commits `ops` (or takes them out of flight) may carry the tension set:
/// no op is a delete, no update writes a structural block (a rename or retitle moves every
/// descendant's path, and so its areas), and no update moves a block or changes its `lifecycle`
/// against its base (which every descendant's effective lifecycle reads). An unreadable base
/// never carries. See `Rdy::carries_tensions`.
pub(crate) fn ops_carry(
    pack: &rdy_schema::Pack,
    reader: &rdy_store::StoreReader,
    ops: &[Op],
) -> bool {
    let structural = |ty: Option<&str>| {
        ty.and_then(|t| pack.block_types.get(t))
            .is_some_and(|d| d.traits.contains(&rdy_schema::Trait::Structural))
    };
    let lifecycle = |b: &Block| {
        b.fields
            .get("lifecycle")
            .filter(|v| **v != rdy_model::FieldValue::Null)
            .cloned()
    };
    ops.iter().all(|op| match op {
        Op::Create { .. } => true,
        Op::Update { base, block } => {
            !structural(Some(&block.block_type))
                && reader.get_block(base).is_ok_and(|old| {
                    old.parent == block.parent && lifecycle(&old) == lifecycle(block)
                })
        }
        // a delete takes a block out of other blocks' reads that the seeds do not cover (an
        // `affects` target's path gives its deciders their areas), so it never carries
        Op::Delete { .. } => false,
    })
}

/// Whether the view-op step `changed` leaves every block outside its seeds as their
/// re-detection sees it. A change that only moved between views keeps the in-flight
/// materialization (it merges the distinct in-flight changes, whatever their views), so it
/// needs nothing more. A change that entered or left flight must commit ops that carry
/// ([`ops_carry`]), write no block with an in-flight delete, and create only fresh blocks: canon
/// does not hold them, no change ever deleted them, no other in-flight change writes them or
/// writes a block under them. Otherwise the step moves paths, areas or lifecycles of blocks no
/// seed covers, and the set is built in full. A failed read never carries.
fn views_step_carries(
    st: &ReadState,
    before: &Writes,
    after: &Writes,
    set: &InflightSet,
    changed: &BTreeSet<Hash>,
    landed: &BTreeSet<BlockId>,
) -> Result<bool, RdyError> {
    let moved: Vec<&Hash> = changed
        .iter()
        .filter(|c| before.change(c).is_some() != set.views_of.contains_key(*c))
        .collect();
    if moved.is_empty() {
        return Ok(true);
    }
    let reader = st.reader();
    let canon = st.materialized(&crate::Scope::Canon)?;
    let others = |w: &Write| !changed.contains(&w.change);
    let mut created: BTreeSet<BlockId> = BTreeSet::new();
    for c in moved {
        let change = reader.change(c)?;
        if !ops_carry(st.pack(), reader, &change.ops) {
            return Ok(false);
        }
        for op in &change.ops {
            let b = op.block_id();
            let on = before.on(b).iter().chain(after.on(b));
            let mut on = on.peekable();
            if on.peek().is_some() {
                let writes: Vec<&Arc<Write>> = on.collect();
                if writes.iter().any(|w| w.new.is_none()) {
                    return Ok(false);
                }
                if matches!(op, Op::Create { .. }) && writes.iter().any(|w| others(w)) {
                    return Ok(false);
                }
            }
            if let Op::Create { block } = op {
                // a landing's own creates are canon's now
                if canon.get(&block.id).is_some() && !landed.contains(&block.id) {
                    return Ok(false);
                }
                created.insert(block.id.clone());
            }
        }
    }
    if created.is_empty() {
        return Ok(true);
    }
    if reader.ever_deleted(created.iter())? {
        return Ok(false);
    }
    // a block under a created one, written by a change outside the step: its path moves
    let under = |w: &Arc<Write>| {
        others(w)
            && w.new
                .as_ref()
                .and_then(|b| b.parent.as_ref())
                .is_some_and(|p| created.contains(p))
    };
    Ok(!before.all().any(under) && !after.all().any(under))
}

/// Below this many seeds a step always carries: a seeded run that small costs little, whatever
/// the size of the set.
pub const CARRY_SEEDS_FLOOR: usize = 1_000;

/// Whether a step's `seeds` are more than [`CARRY_SEEDS_FLOOR`] and outnumber the blocks the
/// writes before it write (an import, a bulk include into a near-empty flight): re-detecting
/// around them reads nearly everything, and one full build costs less than the carry.
fn too_many(seeds: &BTreeSet<BlockId>, before: &Writes) -> bool {
    seeds.len() > CARRY_SEEDS_FLOOR && seeds.len() > before.blocks().count()
}

/// The step carry (plan M2e2: a batch's moves are computed from its delta), after a view op or a
/// landing. `prev` was built on the state before, with the same coordination refs and the same
/// verdicts. A view op changed only which changes are in flight and in which views; a landing
/// (`landed`: the ops it applied to canon) also moved canon. The changes whose membership or
/// views differ, and the blocks the landed ops write, are the step; their blocks (and their
/// decisions' targets) are the seeds, as for a commit. The writes move only on the changes'
/// blocks ([`Writes::with_changes`]). The in-flight part drops every tension on a seed and each
/// kind's tensions on a block of its reach, read before and after the step (so a change that
/// left flight still names the groups its writes were in), and re-detects around the seeds,
/// the reach, and every block of a tension it dropped (a group that loses a member keeps the
/// rest). A landing's canon part is the new canon's, built as a full build builds it; a view
/// op's stays. Its in-flight cost follows the step, not the size of the set; when nothing
/// moved, it is `prev`. `None` when the step does not carry ([`views_step_carries`]; a
/// landing's ops as a commit's, [`ops_carry`]) or is too large ([`too_many`]): the set is
/// built in full.
fn step_carry(
    st: &ReadState,
    prev: &TensionSet,
    landed: Option<&[Op]>,
) -> Result<Option<TensionSet>, RdyError> {
    if let Some(e) = &st.error {
        return Err(e.clone());
    }
    let reader = st.reader();
    let set = st.inflight_set()?;
    let mut changed: BTreeSet<Hash> = prev
        .writes
        .changes()
        .filter(|f| set.views_of.get(&f.id) != Some(&f.views))
        .map(|f| f.id)
        .collect();
    changed.extend(
        set.changes
            .iter()
            .filter(|c| prev.writes.change(c).is_none()),
    );
    let mut seeds = BTreeSet::new();
    let mut landed_blocks = BTreeSet::new();
    if let Some(ops) = landed {
        if !ops_carry(st.pack(), reader, ops) {
            return Ok(None);
        }
        let created: Vec<&BlockId> = ops
            .iter()
            .filter_map(|op| match op {
                Op::Create { block } => Some(&block.id),
                _ => None,
            })
            .collect();
        if reader.ever_deleted(created)? {
            return Ok(None);
        }
        landed_blocks.extend(ops.iter().map(|op| op.block_id().clone()));
        seeds_of_ops(st, ops, &mut seeds)?;
    } else if changed.is_empty() {
        return Ok(Some(prev.clone()));
    }
    for c in &changed {
        seeds_into(st, &reader.change(c)?, &mut seeds)?;
    }
    // a step that touches more blocks than were in flight (an import, a bulk include into an
    // empty flight) re-detects nearly everything around its seeds, and a seeded run over many
    // seeds costs more than one full run: build it in full
    if too_many(&seeds, &prev.writes) {
        return Ok(None);
    }
    let writes = if changed.is_empty() {
        Arc::clone(&prev.writes)
    } else {
        Arc::new(prev.writes.with_changes(st, &set, &changed)?)
    };
    if !views_step_carries(st, &prev.writes, &writes, &set, &changed, &landed_blocks)? {
        return Ok(None);
    }
    let canon = match landed {
        Some(_) => canon_of_landing(st, &prev.canon, &seeds)?,
        None => Arc::clone(&prev.canon),
    };
    let mut next = TensionSet {
        canon,
        writes: Arc::clone(&writes),
        ..prev.clone()
    };
    let mut dropped: BTreeSet<BlockId> = BTreeSet::new();
    let mut drop_id = |next: &mut TensionSet, id: &Hash| {
        if let Some(t) = prev.tensions.get(id)
            && next.remove(id)
        {
            dropped.extend(t.blocks());
        }
    };
    for seed in &seeds {
        for id in prev.by_block.get(seed).into_iter().flatten() {
            drop_id(&mut next, id);
        }
    }
    let index = canon_index_of(st)?;
    let cx = DetectCx::new(st, &writes, Arc::clone(&index), DetectScope::Inflight);
    let before = DetectCx::new(st, &prev.writes, index, DetectScope::Inflight);
    let mut reaches = Vec::with_capacity(st.detectors.len());
    for d in st.detectors {
        let mut reach = d.reach(&cx, &seeds)?;
        reach.extend(d.reach(&before, &seeds)?);
        for b in reach.difference(&seeds) {
            for id in prev.by_block.get(b).into_iter().flatten() {
                if prev.tensions.get(id).is_some_and(|t| t.kind == d.kind()) {
                    drop_id(&mut next, id);
                }
            }
        }
        reaches.push(reach);
    }
    for (d, reach) in st.detectors.iter().zip(reaches) {
        let mut around = seeds.clone();
        around.extend(reach);
        around.extend(dropped.iter().cloned());
        for t in detect_kept(st, &cx, *d, Some(&around))? {
            next.insert(t);
        }
    }
    Ok(Some(next))
}

/// The tensions `d` finds in the scope `cx` reads (with `seeds`, those that involve a seed), as
/// the core keeps them in that scope.
fn detect_kept(
    st: &ReadState,
    cx: &DetectCx<'_>,
    d: &dyn Detector,
    seeds: Option<&BTreeSet<BlockId>>,
) -> Result<Vec<Arc<Tension>>, RdyError> {
    let mut found = vec![];
    d.detect(cx, seeds, &mut found)?;
    Ok(found
        .into_iter()
        .filter_map(|f| tension_from(st.pack(), d.own_severity(cx, &f), f, false, cx.scope()))
        .filter(|t| seeds.is_none_or(|s| t.involved.iter().any(|i| s.contains(&i.block))))
        .map(Arc::new)
        .collect())
}

/// Whether `writes` were built from exactly the in-flight set `set`: the same changes, each in
/// the same views.
fn built_from(writes: &Writes, set: &crate::InflightSet) -> bool {
    set.changes.len() == writes.changes().count()
        && writes
            .changes()
            .all(|f| set.views_of.get(&f.id) == Some(&f.views))
}

/// The verdict carry (plan M2e1): after a verdict or an acceptance on pairs of `blocks`,
/// `prev` (whose in-flight writes must be the state's, else `None`: a full build) less the
/// tensions of the stored kinds ([`Detector::stored`]) that involve one of `blocks`, in both
/// parts, plus what each stored detector finds around them in each scope. The canon part stays
/// shared when no all-canon tension of a stored kind involves those blocks, before or after.
/// Its cost follows the records on those blocks, not the size of the set.
fn verdict_carry(
    st: &ReadState,
    prev: &TensionSet,
    blocks: &[BlockId],
) -> Result<Option<TensionSet>, RdyError> {
    if let Some(e) = &st.error {
        return Err(e.clone());
    }
    if !built_from(&prev.writes, &*st.inflight_set()?) {
        return Ok(None);
    }
    let stored: Vec<&dyn Detector> = st
        .detectors
        .iter()
        .copied()
        .filter(|d| d.stored())
        .collect();
    let mut next = prev.clone();
    if stored.is_empty() || blocks.is_empty() {
        return Ok(Some(next));
    }
    let kinds: BTreeSet<_> = stored.iter().map(|d| d.kind()).collect();
    let seeds: BTreeSet<BlockId> = blocks.iter().cloned().collect();
    let of_stored = |t: Option<&Arc<Tension>>| t.is_some_and(|t| kinds.contains(&t.kind));
    let mut canon_gone = BTreeSet::new();
    for b in &seeds {
        for id in prev.by_block.get(b).into_iter().flatten() {
            if of_stored(prev.tensions.get(id)) {
                next.remove(id);
            }
        }
        for id in prev.canon.by_block.get(b).into_iter().flatten() {
            if of_stored(prev.canon.tensions.get(id)) {
                canon_gone.insert(*id);
            }
        }
    }
    let index = canon_index_of(st)?;
    let cx = DetectCx::new(st, &prev.writes, Arc::clone(&index), DetectScope::Inflight);
    let empty = Writes::empty();
    let canon_cx = DetectCx::new(st, &empty, index, DetectScope::Canon);
    let mut canon_found = vec![];
    for d in &stored {
        for t in detect_kept(st, &cx, *d, Some(&seeds))? {
            next.insert(t);
        }
        canon_found.extend(detect_kept(st, &canon_cx, *d, Some(&seeds))?);
    }
    if !canon_gone.is_empty() || !canon_found.is_empty() {
        next.canon = Arc::new(prev.canon.without(&canon_gone).with(canon_found));
    }
    Ok(Some(next))
}

/// The canon part for the detectors of `detectors` that are not stored (each run in the canon
/// scope, always a full run), kept when every side is canon, by id (a repeated id keeps the
/// first). What canon's memo holds: a stored kind's all-canon tensions follow its records, not
/// canon, so [`stored_canon`] gives them per build.
fn canon_part(st: &ReadState, detectors: &[&dyn Detector]) -> Result<CanonPart, RdyError> {
    CANON_RUNS.with(|c| c.set(c.get() + 1));
    let writes = Writes::empty();
    let cx = DetectCx::new(st, &writes, canon_index_of(st)?, DetectScope::Canon);
    let mut out = vec![];
    for d in detectors.iter().filter(|d| !d.stored()) {
        out.extend(detect_kept(st, &cx, *d, None)?);
    }
    Ok(CanonPart::of(out))
}

/// The all-canon tensions of the stored detectors of `detectors` (D-VA8DA: a semantic tension
/// stays open in canon), each run in the canon scope; none, at no cost, when no such detector
/// has a record.
fn stored_canon(
    st: &ReadState,
    detectors: &[&dyn Detector],
) -> Result<Vec<Arc<Tension>>, RdyError> {
    let mut stored = detectors.iter().filter(|d| d.stored()).peekable();
    if stored.peek().is_none() {
        return Ok(vec![]);
    }
    let writes = Writes::empty();
    let cx = DetectCx::new(st, &writes, canon_index_of(st)?, DetectScope::Canon);
    let mut out = vec![];
    for d in stored {
        out.extend(detect_kept(st, &cx, *d, None)?);
    }
    Ok(out)
}

/// The canon part of `st`'s set after a landing whose blocks (and their decisions' targets) are
/// `seeds`, carried from `prev` (the state before's canon part): canon's memo, filled by the
/// carry when nothing built it yet, joined with the stored kinds' all-canon tensions as
/// [`canon_of`] joins them. A kind whose canon form runs around seeds
/// ([`Detector::canon_seeded`]) drops its tensions on a seed, on its reach and on every block
/// of a tension it drops, and re-detects around all of them; every other kind runs in full.
fn canon_of_landing(
    st: &ReadState,
    prev: &CanonPart,
    seeds: &BTreeSet<BlockId>,
) -> Result<Arc<CanonPart>, RdyError> {
    let built = st.built(&crate::Scope::Canon)?;
    let part = built
        .tension_canon
        .get_or_init(|| canon_carry(st, prev, seeds).map(Arc::new))
        .clone()?;
    let stored = stored_canon(st, st.detectors)?;
    Ok(if stored.is_empty() {
        part
    } else {
        Arc::new(part.with(stored))
    })
}

/// The non-stored canon part of `st` carried from `prev` across a landing on `seeds` (see
/// [`canon_of_landing`]).
fn canon_carry(
    st: &ReadState,
    prev: &CanonPart,
    seeds: &BTreeSet<BlockId>,
) -> Result<CanonPart, RdyError> {
    let writes = Writes::empty();
    let cx = DetectCx::new(st, &writes, canon_index_of(st)?, DetectScope::Canon);
    // the stored kinds' all-canon tensions follow their records, not canon: they are joined
    // per build, never carried in the memo
    let stored: BTreeSet<_> = st
        .detectors
        .iter()
        .filter(|d| d.stored())
        .map(|d| d.kind())
        .collect();
    let mut gone: BTreeSet<Hash> = prev
        .tensions
        .values()
        .filter(|t| stored.contains(&t.kind))
        .map(|t| t.id)
        .collect();
    let mut found = vec![];
    for d in st.detectors.iter().filter(|d| !d.stored()) {
        let of_kind = |id: &Hash| prev.tensions.get(id).is_some_and(|t| t.kind == d.kind());
        if !d.canon_seeded() {
            gone.extend(
                prev.tensions
                    .values()
                    .filter(|t| t.kind == d.kind())
                    .map(|t| t.id),
            );
            found.extend(detect_kept(st, &cx, *d, None)?);
            continue;
        }
        let mut around = seeds.clone();
        around.extend(d.reach(&cx, seeds)?);
        let mut dropped: BTreeSet<BlockId> = BTreeSet::new();
        for b in &around {
            for id in prev.by_block.get(b).into_iter().flatten() {
                if of_kind(id) && gone.insert(*id) {
                    dropped.extend(prev.tensions[id].blocks());
                }
            }
        }
        around.extend(dropped);
        found.extend(detect_kept(st, &cx, *d, Some(&around))?);
    }
    Ok(prev.without(&gone).with(found))
}

/// The canon part of a state's set for `detectors`: canon's memo when `memo` (shared, not
/// copied), joined with the stored kinds' all-canon tensions when there are any.
fn canon_of(
    st: &ReadState,
    detectors: &[&dyn Detector],
    memo: bool,
) -> Result<Arc<CanonPart>, RdyError> {
    let canon = if memo {
        let built = st.built(&crate::Scope::Canon)?;
        built
            .tension_canon
            .get_or_init(|| canon_part(st, detectors).map(Arc::new))
            .clone()?
    } else {
        Arc::new(canon_part(st, detectors)?)
    };
    let stored = stored_canon(st, detectors)?;
    Ok(if stored.is_empty() {
        canon
    } else {
        Arc::new(canon.with(stored))
    })
}

/// The full build for `detectors`: the canon part (canon's memo when `memo`, shared, not
/// copied, joined with the stored kinds' all-canon tensions when there are any) and the
/// in-flight part.
fn build(st: &ReadState, detectors: &[&dyn Detector], memo: bool) -> Result<TensionSet, RdyError> {
    if let Some(e) = &st.error {
        return Err(e.clone());
    }
    let canon = canon_of(st, detectors, memo)?;
    let set = st.inflight_set()?;
    let writes = Arc::new(Writes::build(st, &set)?);
    let cx = DetectCx::new(st, &writes, canon_index_of(st)?, DetectScope::Inflight);
    let mut inflight = vec![];
    for d in detectors {
        inflight.extend(detect_kept(st, &cx, *d, None)?);
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

    /// The landing form ([`ReadState::landing_tensions`]) of the stored kinds' tensions
    /// ([`Detector::stored`]) only: what the gate judges of the semantic tensions, at the cost of
    /// the records on the proposal's blocks (final ruling 2).
    pub(crate) fn landing_stored_tensions(
        &self,
        view: &str,
        changes: &[Hash],
    ) -> Result<Vec<Arc<Tension>>, RdyError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        let stored: Vec<&dyn Detector> = self
            .detectors
            .iter()
            .copied()
            .filter(|d| d.stored())
            .collect();
        if stored.is_empty() {
            return Ok(vec![]);
        }
        let reader = self.reader();
        let read: Vec<Change> = changes
            .iter()
            .map(|c| reader.change(c))
            .collect::<Result<_, _>>()?;
        let applied = self.landing_order(&read);
        self.landing_detect(view, changes, &read, &applied, &stored)
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
        self.landing_detect(view, changes, read, applied, self.detectors)
    }

    /// The landing run of `detectors` (see [`ReadState::landing_tensions_applied`]).
    fn landing_detect(
        &self,
        view: &str,
        changes: &[Hash],
        read: &[Change],
        applied: &[usize],
        detectors: &[&dyn Detector],
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
        for d in detectors {
            for t in detect_kept(self, &cx, *d, Some(&seeds))? {
                out.entry(t.id).or_insert(t);
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
                if let Some((set, around)) = cell.carried(self) {
                    if let Some(around) = around {
                        let _ = cell.around.set(around);
                    }
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
        let (set, around) = cell.carried(self)?;
        // another reader may have built the cell meanwhile: its set is the same
        cell.set
            .get_or_init(|| {
                if let Some(around) = around {
                    let _ = cell.around.set(around);
                }
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
            for t in detect_kept(self, &cx, *d, Some(&around))?
                .into_iter()
                .filter(|t| {
                    t.sides
                        .iter()
                        .any(|s| matches!(s, SideRef::Change(c) if wanted.contains(c)))
                })
            {
                out.entry(t.id).or_insert(t);
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
        let part = canon_part(self, self.detectors)?;
        let part = part.with(stored_canon(self, self.detectors)?);
        Ok(part.tensions.into_iter().map(|(_, t)| t).collect())
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
