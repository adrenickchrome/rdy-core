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
use crate::format::StoreFeatures;
use crate::kinds;
use crate::object_refs::ObjectKey;
use crate::stream_error::{LIMIT_HISTORY_DEPTH, StreamError};
use crate::sync_proof::ObjectSource;
use rdy_model::{
    BlockId, Hash, LandingRecord, SnapshotDelta, SnapshotDeltaError, StateSnapshot,
    fold_snapshot_delta, snapshot_map_id,
};
use std::cell::{Cell, RefCell};
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
    /// Delta `delta`'s `set` entry for block `id` names `version`, a version of another block
    /// (rule 6).
    WrongBlock {
        delta: Hash,
        id: BlockId,
        version: Hash,
    },
    /// The SNAPSHOT `state` of `landing` is not stored as the canonical encoding of its map.
    NotCanonicalSnapshot { landing: Hash, state: Hash },
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
            FoldRefusal::WrongBlock { delta, id, version } => {
                format!(
                    "snapshot delta {delta}: block {id} names version {version} of another block"
                )
            }
            FoldRefusal::NotCanonicalSnapshot { landing, state } => {
                format!("landing {landing}: snapshot {state} is not canonical")
            }
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
            FoldRefusal::NotCanonicalSnapshot { state, .. } => (kinds::SNAPSHOT, *state),
            FoldRefusal::WrongBlock { delta, .. } => (kinds::SNAPSHOT_DELTA, *delta),
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

    /// Whether the held SNAPSHOT `state` is stored as the canonical encoding of its map; None
    /// when the source holds no SNAPSHOT of that hash.
    fn held_snapshot_canonical(&mut self, state: &Hash) -> Result<Option<bool>, Self::Error>;

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

/// The entries of one B-tree node of a [`BlockMap`] (std's `BTreeMap`: `2 * B - 1`, `B = 6`).
const NODE_ENTRIES: usize = 11;

/// The bytes of one leaf node of a [`BlockMap`]: its parent link, its index in the parent and
/// its length, then its keys and values, rounded up to the link's alignment.
fn leaf_node_bytes() -> usize {
    let link = std::mem::size_of::<usize>();
    let raw = link
        + 2 * std::mem::size_of::<u16>()
        + NODE_ENTRIES * (std::mem::size_of::<BlockId>() + std::mem::size_of::<Hash>());
    raw.div_ceil(link) * link
}

/// The bytes of one internal node: a leaf node and its edges.
fn internal_node_bytes() -> usize {
    leaf_node_bytes() + (NODE_ENTRIES + 1) * std::mem::size_of::<usize>()
}

/// The leaf and internal nodes of a map of `n` entries built from sorted entries, as
/// `BTreeMap`'s bulk build lays them out: each node fills before the next opens, and the entry
/// after a full node goes up into the lowest ancestor with room (a new root when none has).
fn bulk_nodes(n: usize) -> (usize, usize) {
    if n == 0 {
        return (0, 0);
    }
    // the length of the rightmost node at each height, the leaf first
    let mut open = vec![0usize];
    let (mut leaves, mut internals) = (1usize, 0usize);
    for _ in 0..n {
        if open[0] < NODE_ENTRIES {
            open[0] += 1;
            continue;
        }
        let h = match open.iter().position(|&len| len < NODE_ENTRIES) {
            Some(h) => h,
            None => {
                open.push(0);
                internals += 1;
                open.len() - 1
            }
        };
        open[h] += 1;
        // a fresh right subtree below the entry: one node at each lower height
        for (level, len) in open.iter_mut().enumerate().take(h) {
            *len = 0;
            if level == 0 {
                leaves += 1;
            } else {
                internals += 1;
            }
        }
    }
    (leaves, internals)
}

/// `map` laid out as the cache keeps it: built again from its sorted entries, so its nodes are
/// full and each id's text takes exactly its length. [`entry_bytes`] measures this layout.
fn compact(map: &BlockMap) -> BlockMap {
    map.iter().map(|(id, v)| (id.clone(), *v)).collect()
}

/// The bytes one cached map holds, measured on the layout [`compact`] gives it: its B-tree nodes,
/// each id's text, and its slot: the shared pointer's counts and map, the table entry (key,
/// pointer, bytes) and the order entry.
fn entry_bytes(map: &BlockMap) -> usize {
    const SLOT: usize = 2 * std::mem::size_of::<usize>()
        + std::mem::size_of::<BlockMap>()
        + std::mem::size_of::<Hash>()
        + 2 * std::mem::size_of::<usize>()
        + std::mem::size_of::<Hash>();
    let (leaves, internals) = bulk_nodes(map.len());
    let text = map
        .keys()
        .fold(0usize, |n, id| n.saturating_add(id.as_str().len()));
    SLOT + leaves * leaf_node_bytes() + internals * internal_node_bytes() + text
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
    /// The cache keeps its own compact copy of `map` ([`compact`]), whose bytes it measures and
    /// charges; `map` itself is not kept.
    pub fn insert(&self, tag: CacheTag, full: Hash, map: Arc<BlockMap>) {
        if self.get(tag, &full).is_some() {
            return;
        }
        // the copy is made outside the lock
        let map = Arc::new(compact(&map));
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

    /// The record source the snapshots are read through.
    pub fn source(&mut self) -> &mut (dyn ObjectSource + 's) {
        &mut *self.source
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

    fn held_snapshot_canonical(&mut self, state: &Hash) -> Result<Option<bool>, StreamError> {
        let key = ObjectKey {
            kind: kinds::SNAPSHOT,
            hash: *state,
        };
        match self.source.record(key)? {
            Some(r @ crate::sync_stream::StreamRecord::Present { .. }) => {
                let crate::sync_stream::StreamRecord::Present { payload, .. } = &r else {
                    unreachable!("a present record")
                };
                Ok(Some(canonical_snapshot(r.raw_kind(), payload)))
            }
            _ => Ok(None),
        }
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

    fn held_snapshot_canonical(&mut self, state: &Hash) -> Result<Option<bool>, crate::StoreError> {
        match self.reader.get_kind_raw(kinds::SNAPSHOT, state) {
            Ok((raw, payload)) => Ok(Some(canonical_snapshot(raw, &payload))),
            Err(crate::StoreError::NotFound(h)) if h == *state => Ok(None),
            Err(e) => Err(e),
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

// ---------------------------------------------------------------------------------------------
// the chain checks (D-7K4Z0): a project's fsck, the holder check, a take and a load

/// The detail of a SNAPSHOT whose bytes are not the canonical encoding of its map, met before a
/// landing that names it is resolved (a batch record, a dump frame).
pub fn noncanonical_snapshot(state: &Hash) -> String {
    format!("snapshot {state}: bytes are not canonical")
}

/// Whether `payload`, a SNAPSHOT frame of raw kind `raw`, is the canonical encoding of the map it
/// decodes to (`crate::codec::decode_canonical`). Bytes that do not decode are not.
pub(crate) fn canonical_snapshot(raw: u8, payload: &[u8]) -> bool {
    crate::codec::decode_canonical::<StateSnapshot>(raw, payload).is_ok()
}

/// The features a held history's links follow (`crate::object_refs`): the store's
/// `meta/store_features` manifest, or no feature for a log that holds none yet.
pub fn held_features(reader: &crate::StoreReader) -> Result<StoreFeatures, crate::StoreError> {
    match reader.get_ref(crate::format::REF_STORE_FEATURES)? {
        Some(v) => crate::format::decode_features(&v),
        None => Ok(StoreFeatures::of(vec![])),
    }
}

thread_local! {
    /// The deltas [`check_snapshot_chains`] applied on this thread, while a test records them.
    static CHAIN_APPLIED: RefCell<Option<Vec<Hash>>> = const { RefCell::new(None) };
}

/// While held, this thread records each delta [`check_snapshot_chains`] applies (a test hook:
/// a check applies each landing's delta once).
#[doc(hidden)]
#[must_use]
pub struct ChainApplications(std::marker::PhantomData<*const ()>);

impl Drop for ChainApplications {
    fn drop(&mut self) {
        CHAIN_APPLIED.with(|c| *c.borrow_mut() = None);
    }
}

impl ChainApplications {
    /// The deltas applied since the last call, in the order applied.
    pub fn take(&self) -> Vec<Hash> {
        CHAIN_APPLIED.with(|c| {
            c.borrow_mut()
                .as_mut()
                .map(std::mem::take)
                .unwrap_or_default()
        })
    }
}

/// Test hook: records each delta [`check_snapshot_chains`] applies on this thread until the
/// guard drops.
#[doc(hidden)]
pub fn record_chain_applications_for_tests() -> ChainApplications {
    CHAIN_APPLIED.with(|c| *c.borrow_mut() = Some(Vec::new()));
    ChainApplications(std::marker::PhantomData)
}

/// How a chain check reads rule 6: the block a set entry's version belongs to (a removed version
/// through its stub), or None when no record describes it (nothing to check).
pub type BlockOf<'a, S, E> = dyn FnMut(&mut S, &Hash) -> Result<Option<BlockId>, E> + 'a;

/// What a chain check hands its caller for each landing it checked: the landing, its record and
/// its full map, in chain order.
pub type VisitMap<'a, S, E> =
    dyn FnMut(&mut S, &Hash, &LandingRecord, &BlockMap) -> Result<(), E> + 'a;

/// One edit a fold made, to undo it at a fork: the block id and the version it held before.
type Edit = (BlockId, Option<Hash>);

/// Checks the snapshots of `landings`, read through `source`, and hands each landing's full map
/// to `visit` once it passes. Every boundary that reads a landing shares it: a project's fsck,
/// the holder check, a take and a load.
///
/// With `deltas` (the history lists `snapshot_delta`), each landing keeps the six rules:
///
/// 1. it has exactly one SNAPSHOT_DELTA;
/// 2. the delta names it, and its `full` is the landing's `state`;
/// 3. its `base` is the previous landing's delta, absent exactly at the first landing;
/// 4. its sorted, disjoint `set` and `drop` hold no redundant operation;
/// 5. its folded map hashes to `full` (the canonical SNAPSHOT bytes);
/// 6. each `set` version belongs to its block (`block_of`; a removed version by its stub).
///
/// The chains are walked oldest first, from each landing whose `prev` is not among `landings`
/// (its map read by [`read_snapshot`], the empty map at a first landing), with one running map
/// per walk. Each landing is checked once, forks and repeated full hashes included; a fork's
/// branches undo their own edits to give back the map of the fork, which is not checked again.
/// Only the `set` entries are read for rule 6: an entry a landing kept was checked where it was
/// set. The walk is iterative, and a walk's map is released when it ends. A landing that no walk
/// reaches is on a cycle (`dependency cycle through {l}`), and a walk deeper than
/// [`MAX_FOLD_DEPTH`] is refused.
///
/// Without `deltas`, each landing's held SNAPSHOT is its map, and it must be stored as the
/// canonical encoding of that map (`landing {l}: snapshot {s} is not canonical`).
pub fn check_snapshot_chains<S, E>(
    source: &mut S,
    deltas: bool,
    landings: &[(Hash, &LandingRecord)],
    block_of: &mut BlockOf<'_, S, E>,
    visit: &mut VisitMap<'_, S, E>,
) -> Result<(), E>
where
    S: SnapshotSource + ?Sized,
    E: From<S::Error>,
{
    let mut order: Vec<(Hash, &LandingRecord)> = landings.to_vec();
    order.sort_unstable_by_key(|(h, _)| *h);
    order.dedup_by_key(|(h, _)| *h);
    if !deltas {
        for (l, rec) in &order {
            let map = legacy_map(source, l, rec)?;
            visit(source, l, rec, &map)?;
        }
        return Ok(());
    }
    let pos: HashMap<Hash, usize> = order
        .iter()
        .enumerate()
        .map(|(i, (h, _))| (*h, i))
        .collect();
    let mut kids: Vec<Vec<usize>> = vec![Vec::new(); order.len()];
    let mut roots: Vec<usize> = Vec::new();
    for (i, (_, rec)) in order.iter().enumerate() {
        match rec.prev.and_then(|p| pos.get(&p).copied()) {
            // a landing that is its own `prev` is on a cycle: no walk reaches it
            Some(j) if j == i => {}
            Some(j) => kids[j].push(i),
            None => roots.push(i),
        }
    }
    drop(pos);
    let mut visited = 0usize;
    let mut reached = vec![false; order.len()];
    for r in roots {
        let (l, rec) = order[r];
        // the map and delta of the landing before the walk: none at a first landing, else the
        // source's own read of a landing outside the set
        let (mut map, before) = match rec.prev {
            None => (BlockMap::new(), None),
            Some(p) => {
                let m = read_snapshot(source, None, &p)?.blocks;
                (m, source.delta_of(&p)?)
            }
        };
        let mut undo: Vec<Edit> = Vec::new();
        let dh = check_landing(source, &l, rec, before, &mut map, None, block_of)?;
        reached[r] = true;
        visited += 1;
        visit(source, &l, rec, &map)?;
        // each frame: the landing, its delta, its next child and the undo mark of a fork
        let mut stack: Vec<(usize, Hash, usize, Option<usize>)> = vec![(r, dh, 0, None)];
        let mut marks = 0usize;
        while let Some(top) = stack.last_mut() {
            let (at, delta, next, mark) = (top.0, top.1, top.2, top.3);
            let Some(&c) = kids[at].get(next) else {
                stack.pop();
                // back at a fork: its map again, from the branch's own edits
                if let Some(&(_, _, _, Some(m))) = stack.last() {
                    revert(&mut map, &mut undo, m);
                }
                continue;
            };
            top.2 += 1;
            // a fork with branches left keeps a mark to come back to
            if next + 1 < kids[at].len() {
                if mark.is_none() {
                    marks += 1;
                }
                top.3 = Some(undo.len());
            } else if mark.is_some() {
                marks -= 1;
                top.3 = None;
            }
            let (cl, crec) = order[c];
            if stack.len() >= MAX_FOLD_DEPTH {
                return Err(source.refused(FoldRefusal::TooDeep(cl)).into());
            }
            let edits = (marks > 0).then_some(&mut undo);
            let cdh = check_landing(source, &cl, crec, Some(delta), &mut map, edits, block_of)?;
            reached[c] = true;
            visited += 1;
            visit(source, &cl, crec, &map)?;
            stack.push((c, cdh, 0, None));
        }
    }
    if visited < order.len() {
        let i = reached
            .iter()
            .position(|r| !r)
            .expect("an unreached landing");
        return Err(source.refused(FoldRefusal::Cycle(order[i].0)).into());
    }
    Ok(())
}

/// Undoes the edits after `mark`, newest first.
fn revert(map: &mut BlockMap, undo: &mut Vec<Edit>, mark: usize) {
    while undo.len() > mark {
        let (id, old) = undo.pop().expect("an edit");
        match old {
            Some(v) => map.insert(id, v),
            None => map.remove(&id),
        };
    }
}

/// The six rules of landing `landing` (record `rec`), whose previous landing's delta is
/// `before`, on `map`, the previous landing's map, which it then folds to this landing's. With
/// `edits`, the fold notes what it changes so a fork can undo it. Returns the landing's delta.
fn check_landing<S, E>(
    source: &mut S,
    landing: &Hash,
    rec: &LandingRecord,
    before: Option<Hash>,
    map: &mut BlockMap,
    edits: Option<&mut Vec<Edit>>,
    block_of: &mut BlockOf<'_, S, E>,
) -> Result<Hash, E>
where
    S: SnapshotSource + ?Sized,
    E: From<S::Error>,
{
    // 1. exactly one delta (the source refuses a second distinct one when it associates them)
    let Some(dh) = source.delta_of(landing)? else {
        return Err(source
            .refused(FoldRefusal::MissingDelta { landing: *landing })
            .into());
    };
    let delta = source.delta(&dh)?;
    // 2. its landing and full
    if delta.landing != *landing || delta.full != rec.state {
        return Err(source.refused(FoldRefusal::Identity { delta: dh }).into());
    }
    // 3. its base: the previous landing's delta, absent exactly at the first landing
    let base_ok = match rec.prev {
        None => delta.base.is_none(),
        Some(_) => delta.base.is_some() && delta.base == before,
    };
    if !base_ok {
        return Err(source.refused(FoldRefusal::Base { delta: dh }).into());
    }
    let grows = delta
        .set
        .iter()
        .filter(|(k, _)| !map.contains_key(k))
        .count();
    if map.len().saturating_sub(delta.drop.len()) + grows > MAX_ELEMENTS {
        return Err(source.refused(FoldRefusal::TooLarge(dh)).into());
    }
    // 4. canonical against the previous map; the fold checks the form before it changes a key
    let olds: Vec<Edit> = match &edits {
        Some(_) => delta
            .drop
            .iter()
            .chain(delta.set.iter().map(|(k, _)| k))
            .map(|k| (k.clone(), map.get(k).copied()))
            .collect(),
        None => Vec::new(),
    };
    if let Err(error) = fold_snapshot_delta(map, &delta) {
        return Err(source
            .refused(FoldRefusal::NotCanonical { delta: dh, error })
            .into());
    }
    if let Some(undo) = edits {
        undo.extend(olds);
    }
    // 5. the folded map is `full`
    if snapshot_map_id(map) != rec.state {
        return Err(source.refused(FoldRefusal::Mismatch { delta: dh }).into());
    }
    // 6. each set version is a version of its block
    for (id, version) in &delta.set {
        if let Some(of) = block_of(source, version)?
            && of != *id
        {
            return Err(source
                .refused(FoldRefusal::WrongBlock {
                    delta: dh,
                    id: id.clone(),
                    version: *version,
                })
                .into());
        }
    }
    CHAIN_APPLIED.with(|c| {
        if let Some(v) = c.borrow_mut().as_mut() {
            v.push(dh);
        }
    });
    Ok(dh)
}

/// A landing's map without the feature: its held SNAPSHOT, which must be stored canonically.
fn legacy_map<S, E>(source: &mut S, landing: &Hash, rec: &LandingRecord) -> Result<BlockMap, E>
where
    S: SnapshotSource + ?Sized,
    E: From<S::Error>,
{
    let missing = FoldRefusal::MissingMap {
        landing: *landing,
        state: rec.state,
    };
    let Some(s) = source.held_snapshot(landing, &rec.state)? else {
        return Err(source.refused(missing).into());
    };
    match source.held_snapshot_canonical(&rec.state)? {
        Some(true) => Ok(s.blocks),
        None => Err(source.refused(missing).into()),
        Some(false) => Err(source
            .refused(FoldRefusal::NotCanonicalSnapshot {
                landing: *landing,
                state: rec.state,
            })
            .into()),
    }
}

impl crate::StoreReader {
    /// The chain checks of `landings` ([`check_snapshot_chains`]) over this store, with its
    /// features: every landing a project holds (off canon too) for its fsck, or every landing
    /// a holder check read. A version a removal erased reads as its stub; one no residue
    /// describes, or one the store does not hold, has nothing to check. A refusal is `StoreError::Decode` with the rule's text.
    pub fn check_snapshot_chains(
        &self,
        landings: &[(Hash, &LandingRecord)],
    ) -> Result<(), crate::StoreError> {
        let deltas = held_features(self)?.has(crate::format::FEATURE_SNAPSHOT_DELTA);
        let mut source = StoreSnapshots { reader: self };
        check_snapshot_chains(
            &mut source,
            deltas,
            landings,
            &mut |s: &mut StoreSnapshots<'_>, v: &Hash| match s.reader.get_block(v) {
                Ok(b) => Ok(Some(b.id)),
                // a version a removal erased with no residue, or one the store does not hold
                // (its own check refuses a missing object), has nothing to check
                Err(crate::StoreError::Redacted { .. }) => Ok(None),
                Err(crate::StoreError::NotFound(h)) if h == *v => Ok(None),
                Err(e) => Err(e),
            },
            &mut |_, _, _, _| Ok(()),
        )
    }
}
