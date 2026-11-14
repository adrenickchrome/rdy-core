//! The one read of a landing's full map (SD1, D-RP6SM): every state read goes through
//! [`read_snapshot`], over a [`SnapshotSource`] (a captured store reader, or the records of a
//! checked stream or upload), with an optional bounded [`MapCache`].
//!
//! The read of landing `L`, whose `state` is `S`:
//!
//! 1. A cached map of `S` answers at once.
//! 2. A held typed SNAPSHOT under `S` answers next.
//! 3. Otherwise the read walks back along `prev`, one landing at a time, to the nearest landing
//!    whose map is cached or held, or to the first landing (the empty map before it). Each
//!    landing it leaves on the way needs its SNAPSHOT_DELTA. Then it folds the deltas forward,
//!    in chain order, and compares the folded map's id with `S`.
//!
//! The walk is a loop, never a recursion. It refuses a landing it meets twice, a landing deeper
//! than [`MAX_FOLD_DEPTH`], a delta that names another landing or another `full`, a `base` that
//! is not the previous landing's delta, a delta that is not canonical against the map it folds
//! onto, a map that would grow past [`MAX_ELEMENTS`](crate::bounded::MAX_ELEMENTS) entries, and
//! a folded map whose id is not `S`. Each check runs before the map grows.
//!
//! A cached map holds block ids and version hashes only: a block body is read through the
//! current removal-aware source each time, so a cache hit never serves removed content.
use crate::bounded::{COLLECTION_ELEMENTS, MAX_ELEMENTS, RecordError};
use crate::kinds;
use crate::object_refs::ObjectKey;
use crate::stream_error::{LIMIT_HISTORY_DEPTH, StreamError};
use crate::sync_proof::ObjectSource;
use rdy_model::{
    BlockId, Hash, LandingRecord, SnapshotDelta, SnapshotDeltaError, StateSnapshot,
    fold_snapshot_delta, snapshot_map_id,
};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

/// The most landings one fold walks back: the holder check's landing depth.
pub const MAX_FOLD_DEPTH: usize = crate::check_holder::MAX_LANDING_DEPTH;

/// The most bytes of retained entries a [`MapCache`] keeps by default (64 MiB). The current
/// captured canon map (the engine's `ReadState`) is kept beside it, outside this budget.
pub const MAP_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// One map: block ids to version hashes.
pub type BlockMap = BTreeMap<BlockId, Hash>;

thread_local! {
    /// Deltas folded by [`read_snapshot`] on this thread (a test hook).
    static FOLDED: Cell<u64> = const { Cell::new(0) };
}

/// The snapshot deltas [`read_snapshot`] has folded on this thread so far (a test hook: a warm
/// read folds none).
#[doc(hidden)]
pub fn folded_deltas_for_tests() -> u64 {
    FOLDED.with(Cell::get)
}

/// Why a map read refused. The detail texts are the plan's closed list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FoldRefusal {
    /// The source does not hold landing `.0`.
    MissingLanding(Hash),
    /// The source holds neither the SNAPSHOT `state` of `landing` nor a delta of it, and the
    /// source does not require deltas: the old missing-object answer.
    MissingMap { landing: Hash, state: Hash },
    /// A landing the read needs has no delta (the history requires one, or the walk left it).
    MissingDelta { landing: Hash },
    /// Delta `delta` names another landing or another full hash than the landing it records.
    Identity { delta: Hash },
    /// Delta `delta`'s `base` is not the previous landing's delta.
    Base { delta: Hash },
    /// Delta `delta` is not canonical against the map it folds onto.
    NotCanonical {
        delta: Hash,
        error: SnapshotDeltaError,
    },
    /// The map folded through delta `delta` is not its `full`.
    Mismatch { delta: Hash },
    /// The walk met landing `.0` twice.
    Cycle(Hash),
    /// The walk from landing `.0` passed [`MAX_FOLD_DEPTH`] landings.
    TooDeep(Hash),
    /// Delta `.0` would grow the map past [`MAX_ELEMENTS`] entries.
    TooLarge(Hash),
}

impl FoldRefusal {
    /// The refusal's detail text.
    pub fn detail(&self) -> String {
        match self {
            FoldRefusal::MissingLanding(l) => format!("object {l} not found"),
            FoldRefusal::MissingMap { state, .. } => format!("object {state} not found"),
            FoldRefusal::MissingDelta { landing } => {
                format!("landing {landing}: expected exactly one snapshot delta")
            }
            FoldRefusal::Identity { delta } => {
                format!("snapshot delta {delta}: landing or full hash does not match")
            }
            FoldRefusal::Base { delta } => {
                format!("snapshot delta {delta}: base does not match the previous landing")
            }
            FoldRefusal::NotCanonical { delta, .. } => {
                format!("snapshot delta {delta}: set or drop is not canonical")
            }
            FoldRefusal::Mismatch { delta } => {
                format!("snapshot delta {delta}: folded map does not match full")
            }
            FoldRefusal::Cycle(l) => format!("dependency cycle through {l}"),
            FoldRefusal::TooDeep(l) => format!("landing {l}: exceeds {LIMIT_HISTORY_DEPTH}"),
            FoldRefusal::TooLarge(d) => format!(
                "snapshot delta {d}: exceeds {}",
                RecordError::limit(COLLECTION_ELEMENTS, MAX_ELEMENTS)
            ),
        }
    }

    /// The object the refusal is about: the delta, or the landing.
    pub fn object(&self) -> ObjectKey {
        let (kind, hash) = match self {
            FoldRefusal::MissingLanding(l) | FoldRefusal::Cycle(l) | FoldRefusal::TooDeep(l) => {
                (kinds::LANDING, *l)
            }
            FoldRefusal::MissingMap { state, .. } => (kinds::SNAPSHOT, *state),
            FoldRefusal::MissingDelta { landing } => (kinds::LANDING, *landing),
            FoldRefusal::Identity { delta }
            | FoldRefusal::Base { delta }
            | FoldRefusal::NotCanonical { delta, .. }
            | FoldRefusal::Mismatch { delta }
            | FoldRefusal::TooLarge(delta) => (kinds::SNAPSHOT_DELTA, *delta),
        };
        ObjectKey { kind, hash }
    }
}

/// What a map read needs from where the history is held: typed landings, held SNAPSHOTs, the
/// landing-to-delta association, typed deltas and whether the history requires deltas (the
/// `snapshot_delta` feature). Each answer of None means "not held", never an error.
pub trait SnapshotSource {
    type Error;

    /// Landing `landing`, decoded; None when the source does not hold it.
    fn landing(&mut self, landing: &Hash) -> Result<Option<LandingRecord>, Self::Error>;

    /// The typed SNAPSHOT `state`; None when the source holds no SNAPSHOT of that hash.
    fn held_snapshot(
        &mut self,
        landing: &Hash,
        state: &Hash,
    ) -> Result<Option<StateSnapshot>, Self::Error>;

    /// The hash of the delta that records landing `landing`; None when the source holds none.
    fn delta_of(&mut self, landing: &Hash) -> Result<Option<Hash>, Self::Error>;

    /// The typed delta `delta`, which [`SnapshotSource::delta_of`] named.
    fn delta(&mut self, delta: &Hash) -> Result<SnapshotDelta, Self::Error>;

    /// Whether the history lists the `snapshot_delta` feature, so a landing with no map needs a
    /// delta. Read only on a refusal.
    fn requires_delta(&mut self) -> Result<bool, Self::Error> {
        Ok(false)
    }

    /// The source's error for `refusal`.
    fn refused(&mut self, refusal: FoldRefusal) -> Self::Error;
}

/// The identity of the source a cache entry was built from: entries of another source, or of an
/// older removal epoch, are never served.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheTag {
    pub source: u64,
    pub epoch: u64,
}

/// A bounded in-memory cache of full maps by `full` hash, for one source's lifetime. Entries are
/// kept within a byte budget ([`MAP_CACHE_BYTES`] by default) that counts each key, each map
/// entry and its allocation; the oldest entry goes first, and an entry larger than the whole
/// budget is not kept. A miss reads as a cold read does.
pub struct MapCache {
    inner: Mutex<CacheInner>,
}

struct CacheInner {
    budget: usize,
    used: usize,
    tag: Option<CacheTag>,
    order: VecDeque<Hash>,
    maps: HashMap<Hash, (Arc<BlockMap>, usize)>,
}

impl Default for MapCache {
    fn default() -> Self {
        MapCache::with_budget(MAP_CACHE_BYTES)
    }
}

impl std::fmt::Debug for MapCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (len, used) = self.stats();
        f.debug_struct("MapCache")
            .field("entries", &len)
            .field("bytes", &used)
            .finish()
    }
}

/// The bytes one cached map is charged: its key, its slot and each entry with its id's text.
fn entry_bytes(map: &BlockMap) -> usize {
    const SLOT: usize = 32 + 3 * std::mem::size_of::<usize>() + 64;
    // a B-tree entry: the id, its text, the hash and a share of its node
    let per = std::mem::size_of::<BlockId>() + std::mem::size_of::<Hash>() + 16;
    map.keys()
        .fold(SLOT, |n, id| n.saturating_add(per + id.as_str().len()))
}

impl MapCache {
    pub fn new() -> MapCache {
        MapCache::default()
    }

    /// A cache that keeps at most `budget` bytes of entries.
    pub fn with_budget(budget: usize) -> MapCache {
        MapCache {
            inner: Mutex::new(CacheInner {
                budget,
                used: 0,
                tag: None,
                order: VecDeque::new(),
                maps: HashMap::new(),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sets the byte budget, evicting the oldest entries past it.
    pub fn set_budget(&self, budget: usize) {
        let mut c = self.lock();
        c.budget = budget;
        c.evict_to(budget);
    }

    /// Drops every entry.
    pub fn clear(&self) {
        let mut c = self.lock();
        c.clear();
    }

    /// The entries held and the bytes charged.
    pub fn stats(&self) -> (usize, usize) {
        let c = self.lock();
        (c.maps.len(), c.used)
    }

    /// The map of `full` built from the source `tag` names; None on a miss.
    pub fn get(&self, tag: CacheTag, full: &Hash) -> Option<Arc<BlockMap>> {
        let c = self.lock();
        if c.tag != Some(tag) {
            return None;
        }
        c.maps.get(full).map(|(m, _)| Arc::clone(m))
    }

    /// Keeps `map` as the map of `full`, built from the source `tag` names. Entries of another
    /// source, or of an older epoch, are dropped first; a reader of an older epoch than the
    /// entries' keeps nothing.
    pub fn insert(&self, tag: CacheTag, full: Hash, map: Arc<BlockMap>) {
        let mut c = self.lock();
        match c.tag {
            Some(t) if t == tag => {}
            Some(t) if tag.epoch < t.epoch => return,
            _ => {
                c.clear();
                c.tag = Some(tag);
            }
        }
        if c.maps.contains_key(&full) {
            return;
        }
        let bytes = entry_bytes(&map);
        if bytes > c.budget {
            return;
        }
        let keep = c.budget - bytes;
        c.evict_to(keep);
        c.used += bytes;
        c.order.push_back(full);
        c.maps.insert(full, (map, bytes));
    }
}

impl CacheInner {
    fn clear(&mut self) {
        self.used = 0;
        self.order.clear();
        self.maps.clear();
    }

    /// Evicts the oldest entries until at most `limit` bytes are charged.
    fn evict_to(&mut self, limit: usize) {
        while self.used > limit {
            let Some(oldest) = self.order.pop_front() else {
                self.used = 0;
                return;
            };
            if let Some((_, b)) = self.maps.remove(&oldest) {
                self.used -= b;
            }
        }
    }
}

/// The full map of landing `landing`, read from `source` (see the module docs), through `cache`
/// when one is given with its source's tag.
pub fn read_snapshot<S: SnapshotSource + ?Sized>(
    source: &mut S,
    cache: Option<(&MapCache, CacheTag)>,
    landing: &Hash,
) -> Result<StateSnapshot, S::Error> {
    let Some(record) = source.landing(landing)? else {
        return Err(source.refused(FoldRefusal::MissingLanding(*landing)));
    };
    read_landing_snapshot(source, cache, landing, &record)
}

/// [`read_snapshot`] of landing `landing`, whose record `record` the caller has read already.
pub fn read_landing_snapshot<S: SnapshotSource + ?Sized>(
    source: &mut S,
    cache: Option<(&MapCache, CacheTag)>,
    landing: &Hash,
    record: &LandingRecord,
) -> Result<StateSnapshot, S::Error> {
    if let Some(map) = known(source, cache, landing, &record.state)? {
        return Ok(StateSnapshot {
            blocks: Arc::unwrap_or_clone(map),
        });
    }
    // walk back: each step is a landing's delta hash and that delta's base. Only hashes are
    // kept, so the walk holds 64 bytes a landing; the fold reads each delta again.
    let mut steps: Vec<(Hash, Option<Hash>)> = Vec::new();
    let mut seen: HashSet<Hash> = HashSet::new();
    let mut at = (*landing, record.clone());
    let base: Arc<BlockMap> = loop {
        let (here, rec) = at;
        if !seen.insert(here) {
            return Err(source.refused(FoldRefusal::Cycle(here)));
        }
        if steps.len() >= MAX_FOLD_DEPTH {
            return Err(source.refused(FoldRefusal::TooDeep(*landing)));
        }
        let Some(dh) = source.delta_of(&here)? else {
            let refusal = if steps.is_empty() && !source.requires_delta()? {
                FoldRefusal::MissingMap {
                    landing: here,
                    state: rec.state,
                }
            } else {
                FoldRefusal::MissingDelta { landing: here }
            };
            return Err(source.refused(refusal));
        };
        let delta = source.delta(&dh)?;
        if delta.landing != here || delta.full != rec.state {
            return Err(source.refused(FoldRefusal::Identity { delta: dh }));
        }
        // the later step's base names this delta
        if let Some(&(later, base)) = steps.last()
            && base != Some(dh)
        {
            return Err(source.refused(FoldRefusal::Base { delta: later }));
        }
        let Some(prev) = rec.prev else {
            if delta.base.is_some() {
                return Err(source.refused(FoldRefusal::Base { delta: dh }));
            }
            steps.push((dh, delta.base));
            break Arc::new(BlockMap::new());
        };
        if delta.base.is_none() {
            return Err(source.refused(FoldRefusal::Base { delta: dh }));
        }
        steps.push((dh, delta.base));
        drop(delta);
        let Some(prev_rec) = source.landing(&prev)? else {
            return Err(source.refused(FoldRefusal::MissingLanding(prev)));
        };
        if let Some(map) = known(source, cache, &prev, &prev_rec.state)? {
            // the base must name the previous landing's delta when the source holds one
            if let Some(pd) = source.delta_of(&prev)? {
                let &(last, base) = steps.last().expect("one step");
                if base != Some(pd) {
                    return Err(source.refused(FoldRefusal::Base { delta: last }));
                }
            }
            break map;
        }
        at = (prev, prev_rec);
    };
    // fold forward, in chain order
    let mut map = Arc::unwrap_or_clone(base);
    for &(dh, _) in steps.iter().rev() {
        let delta = source.delta(&dh)?;
        let grows = delta
            .set
            .iter()
            .filter(|(k, _)| !map.contains_key(k))
            .count();
        // the fold drops first, then sets: this is the most entries the map would hold
        if map.len().saturating_sub(delta.drop.len()) + grows > MAX_ELEMENTS {
            return Err(source.refused(FoldRefusal::TooLarge(dh)));
        }
        if let Err(error) = fold_snapshot_delta(&mut map, &delta) {
            return Err(source.refused(FoldRefusal::NotCanonical { delta: dh, error }));
        }
        FOLDED.with(|c| c.set(c.get().wrapping_add(1)));
    }
    let &(target, _) = steps.first().expect("one step");
    if snapshot_map_id(&map) != record.state {
        return Err(source.refused(FoldRefusal::Mismatch { delta: target }));
    }
    let map = Arc::new(map);
    if let Some((c, tag)) = cache {
        c.insert(tag, record.state, Arc::clone(&map));
    }
    Ok(StateSnapshot {
        blocks: Arc::unwrap_or_clone(map),
    })
}

/// The map of `state` (landing `landing`'s) without a fold: a cached one, else a held SNAPSHOT
/// (which the cache then keeps).
fn known<S: SnapshotSource + ?Sized>(
    source: &mut S,
    cache: Option<(&MapCache, CacheTag)>,
    landing: &Hash,
    state: &Hash,
) -> Result<Option<Arc<BlockMap>>, S::Error> {
    if let Some((c, tag)) = cache
        && let Some(m) = c.get(tag, state)
    {
        return Ok(Some(m));
    }
    let Some(snap) = source.held_snapshot(landing, state)? else {
        return Ok(None);
    };
    let map = Arc::new(snap.blocks);
    if let Some((c, tag)) = cache {
        c.insert(tag, *state, Arc::clone(&map));
    }
    Ok(Some(map))
}

// ---------------------------------------------------------------------------------------------
// the records of a stream or an upload as a source

/// The records of an [`ObjectSource`] (a checked stream, a take's upload over its holder, or
/// records by key) as a map source. The landing-to-delta association is the source's own
/// ([`ObjectSource::snapshot_delta_of`]): a stream builds it from the typed delta records it
/// holds, never from an index it does not have. A removed (dropped) record reads as not held.
pub struct RecordSnapshots<'a, 's> {
    source: &'a mut (dyn ObjectSource + 's),
}

impl<'a, 's> RecordSnapshots<'a, 's> {
    pub fn new(source: &'a mut (dyn ObjectSource + 's)) -> RecordSnapshots<'a, 's> {
        RecordSnapshots { source }
    }
}

impl SnapshotSource for RecordSnapshots<'_, '_> {
    type Error = StreamError;

    fn landing(&mut self, landing: &Hash) -> Result<Option<LandingRecord>, StreamError> {
        crate::stream_source::typed(
            self.source,
            ObjectKey {
                kind: kinds::LANDING,
                hash: *landing,
            },
        )
    }

    fn held_snapshot(
        &mut self,
        _: &Hash,
        state: &Hash,
    ) -> Result<Option<StateSnapshot>, StreamError> {
        crate::stream_source::typed(
            self.source,
            ObjectKey {
                kind: kinds::SNAPSHOT,
                hash: *state,
            },
        )
    }

    fn delta_of(&mut self, landing: &Hash) -> Result<Option<Hash>, StreamError> {
        self.source.snapshot_delta_of(landing)
    }

    fn delta(&mut self, delta: &Hash) -> Result<SnapshotDelta, StreamError> {
        let key = ObjectKey {
            kind: kinds::SNAPSHOT_DELTA,
            hash: *delta,
        };
        crate::stream_source::typed(self.source, key)?
            .ok_or_else(|| StreamError::unresolved(0, 0, key))
    }

    fn refused(&mut self, refusal: FoldRefusal) -> StreamError {
        stream_refusal(refusal)
    }
}

/// A map read's refusal over stream records: a missing landing or SNAPSHOT is the stream's
/// unresolved reference (as before deltas), a bound is E_LIMIT and every other refusal is
/// E_BAD_RECORD with its detail and the take's fix.
pub fn stream_refusal(refusal: FoldRefusal) -> StreamError {
    let object = refusal.object();
    match refusal {
        FoldRefusal::MissingLanding(_) | FoldRefusal::MissingMap { .. } => {
            StreamError::unresolved(0, 0, object)
        }
        FoldRefusal::TooDeep(_) => {
            StreamError::limit(None, 0, LIMIT_HISTORY_DEPTH).with_object(Some(object))
        }
        FoldRefusal::TooLarge(_) => {
            StreamError::from_record(0, 0, &RecordError::limit(COLLECTION_ELEMENTS, MAX_ELEMENTS))
                .with_object(Some(object))
        }
        r => StreamError {
            code: "E_BAD_RECORD",
            message: r.detail(),
            fix: "upload the history as its source project exported it".into(),
            record: None,
            offset: 0,
            object: Some(object),
        },
    }
}

/// The landing-to-delta association of `records`, built from the typed delta records among
/// them (`keys`, each read through `read`): each landing to its one delta. Two distinct deltas
/// of one landing are E_BAD_RECORD.
pub(crate) fn associate_deltas<'k>(
    keys: impl IntoIterator<Item = &'k ObjectKey>,
    mut read: impl FnMut(ObjectKey) -> Result<Option<SnapshotDelta>, StreamError>,
) -> Result<BTreeMap<Hash, Hash>, StreamError> {
    let mut out: BTreeMap<Hash, Hash> = BTreeMap::new();
    for key in keys {
        if key.kind != kinds::SNAPSHOT_DELTA {
            continue;
        }
        let Some(delta) = read(*key)? else {
            continue;
        };
        match out.get(&delta.landing) {
            Some(h) if *h != key.hash => {
                return Err(stream_refusal(FoldRefusal::MissingDelta {
                    landing: delta.landing,
                })
                .with_object(Some(*key)));
            }
            _ => {
                out.insert(delta.landing, key.hash);
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// a captured store reader as a source

/// A captured [`StoreReader`](crate::StoreReader) as a map source: typed landings and SNAPSHOTs
/// from its index, and each landing's delta from the derived `snapshot_deltas` table.
pub(crate) struct StoreSnapshots<'a> {
    pub(crate) reader: &'a crate::StoreReader,
}

impl SnapshotSource for StoreSnapshots<'_> {
    type Error = crate::StoreError;

    fn landing(&mut self, landing: &Hash) -> Result<Option<LandingRecord>, crate::StoreError> {
        match self.reader.landing(landing) {
            Err(crate::StoreError::NotFound(h)) if h == *landing => Ok(None),
            r => r.map(Some),
        }
    }

    fn held_snapshot(
        &mut self,
        landing: &Hash,
        state: &Hash,
    ) -> Result<Option<StateSnapshot>, crate::StoreError> {
        if self.reader.folds_held_snapshots() && self.reader.snapshot_delta_of(landing)?.is_some() {
            return Ok(None);
        }
        match self.reader.get_typed(kinds::SNAPSHOT, state) {
            Err(crate::StoreError::NotFound(h)) if h == *state => {
                // a state hash that names an object of another kind is no map
                if self.reader.has(state)? {
                    let (k, _) = self.reader.get_raw(state)?;
                    return Err(crate::StoreError::Decode(format!(
                        "{state} is not a snapshot (object kind {k})"
                    )));
                }
                Ok(None)
            }
            r => r.map(Some),
        }
    }

    fn delta_of(&mut self, landing: &Hash) -> Result<Option<Hash>, crate::StoreError> {
        Ok(self.reader.snapshot_delta_of(landing)?.map(|(h, _)| h))
    }

    fn delta(&mut self, delta: &Hash) -> Result<SnapshotDelta, crate::StoreError> {
        self.reader.get_typed(kinds::SNAPSHOT_DELTA, delta)
    }

    fn requires_delta(&mut self) -> Result<bool, crate::StoreError> {
        Ok(self
            .reader
            .index()
            .get_ref(crate::format::REF_STORE_FEATURES)?
            .map(|v| crate::format::decode_features(&v))
            .transpose()?
            .is_some_and(|f| f.has(crate::format::FEATURE_SNAPSHOT_DELTA)))
    }

    fn refused(&mut self, refusal: FoldRefusal) -> crate::StoreError {
        match refusal {
            FoldRefusal::MissingLanding(h) => crate::StoreError::NotFound(h),
            FoldRefusal::MissingMap { state, .. } => crate::StoreError::NotFound(state),
            r => crate::StoreError::Decode(r.detail()),
        }
    }
}
