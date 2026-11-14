//! The snapshot-delta conversion (D-918YB, D-8T7PA): the one action that makes a legacy history
//! hold a SNAPSHOT_DELTA for each landing. The store upgrade (`crate::upgrade`), a load's tail
//! batch (`crate::ingest`) and a take of a legacy upload into a holder that holds the feature
//! (`crate::take_batch`) run it.
//!
//! The action reads each landing's own legacy SNAPSHOT, whose bytes must be the canonical
//! encoding of its map: that hash is the delta's `full`, and no changed object is ever stored
//! under an old hash. The landings are ordered by `prev`, oldest first, ties by landing hash,
//! roots and forks alike. Each delta is the difference between the previous landing's map (the
//! empty map at a first landing) and the landing's map, with `base` the previous landing's
//! delta. A removed block version keeps its hash in a map, as it does in the SNAPSHOT; the store
//! checks set versions against their stubs. At most two full maps are held at a time: the map
//! of the landing visited last, which the next landing in a chain reuses, and the one read now.
//! The deltas are small; the sealed log, not this preparation, is the authority.
use crate::object_refs::ObjectKey;
use crate::segment::{Location, SegmentLog};
use crate::snapshot::{BlockMap, FoldRefusal, canonical_snapshot};
use crate::stream_error::StreamError;
use crate::sync_proof::ObjectSource;
use crate::upgrade::SNAPSHOT_DELTA_STEP;
use crate::{StoreError, StoreReader, codec, kinds};
use rdy_model::{Hash, LandingRecord, SnapshotDelta, StateSnapshot, diff_snapshots};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// The detail of an upgrade batch that holds anything but its registered step objects and its
/// two refs.
pub const UPGRADE_BATCH: &str =
    "the upgrade batch holds only its registered step objects and the features and receipt refs";

/// One landing a delta is prepared for: its `prev` and its `state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Need {
    pub(crate) prev: Option<Hash>,
    pub(crate) state: Hash,
}

impl Need {
    pub(crate) fn of(rec: &LandingRecord) -> Need {
        Need {
            prev: rec.prev,
            state: rec.state,
        }
    }
}

/// What precedes a landing whose `prev` is not among the landings a delta is prepared for.
pub(crate) enum Outside {
    /// The previous landing's held delta and its map.
    Base { delta: Hash, map: BlockMap },
    /// No delta can be based on it: the landing, and every landing after it, gets none.
    Skip,
}

/// The landings of `need` in chain order: a landing after its `prev`, the ready ones by hash.
/// A landing on a `prev` cycle is never ready, and is left out.
pub(crate) fn chain_order(need: &BTreeMap<Hash, Need>) -> Vec<Hash> {
    let mut children: BTreeMap<Hash, Vec<Hash>> = BTreeMap::new();
    let mut ready: BTreeSet<Hash> = BTreeSet::new();
    for (l, n) in need {
        match n.prev.filter(|p| need.contains_key(p)) {
            Some(p) => children.entry(p).or_default().push(*l),
            None => {
                ready.insert(*l);
            }
        }
    }
    let mut out = Vec::with_capacity(need.len());
    while let Some(l) = ready.pop_first() {
        out.push(l);
        if let Some(c) = children.remove(&l) {
            ready.extend(c);
        }
    }
    out
}

/// Builds the delta of each landing of `need`, in [`chain_order`], and hands each to `emit`.
/// `outside` answers for a `prev` outside `need`; `read_map` reads a landing's full map by its
/// `state`; `not_canonical` is the refusal of a landing whose map's hash is not its `state` (its
/// SNAPSHOT is not stored canonically).
pub(crate) fn build<E>(
    need: &BTreeMap<Hash, Need>,
    outside: &mut dyn FnMut(&Hash) -> Result<Outside, E>,
    read_map: &mut dyn FnMut(&Hash, &Hash) -> Result<BlockMap, E>,
    not_canonical: &dyn Fn(&Hash, &Hash) -> E,
    emit: &mut dyn FnMut(SnapshotDelta) -> Result<(), E>,
) -> Result<(), E> {
    let mut made: HashMap<Hash, Hash> = HashMap::new();
    let mut skipped: HashSet<Hash> = HashSet::new();
    // the map of the landing visited last: the next landing of a chain starts from it
    let mut last: Option<(Hash, BlockMap)> = None;
    for l in chain_order(need) {
        let n = need[&l];
        let (base, before) = match n.prev {
            None => (None, BlockMap::new()),
            Some(p) if need.contains_key(&p) => {
                if skipped.contains(&p) {
                    skipped.insert(l);
                    continue;
                }
                let before = match last.take() {
                    Some((h, m)) if h == p => m,
                    _ => read_map(&p, &need[&p].state)?,
                };
                (made.get(&p).copied(), before)
            }
            Some(p) => match outside(&p)? {
                Outside::Base { delta, map } => (Some(delta), map),
                Outside::Skip => {
                    skipped.insert(l);
                    continue;
                }
            },
        };
        let next = read_map(&l, &n.state)?;
        let d = diff_snapshots(l, base, &before, &next);
        // the delta's `full` is the landing's own state: never another hash for its map
        if d.full != n.state {
            return Err(not_canonical(&l, &n.state));
        }
        made.insert(l, d.id());
        emit(d)?;
        last = Some((l, next));
    }
    Ok(())
}

/// The held landings of the store that have no delta yet: every landing of the project (off
/// canon too, and every landing a holder holds), less the records of verified foreign-removal
/// proofs (`StoreReader::project_landings`).
pub(crate) fn needs(reader: &StoreReader) -> Result<BTreeMap<Hash, Need>, StoreError> {
    let mut out = BTreeMap::new();
    for (h, rec) in reader.project_landings()? {
        if reader.snapshot_delta_of(&h)?.is_none() {
            out.insert(h, Need::of(&rec));
        }
    }
    Ok(out)
}

/// The refusal of a legacy SNAPSHOT `state` of `landing` that is not stored as the canonical
/// encoding of its map.
fn not_canonical_store(landing: &Hash, state: &Hash) -> StoreError {
    StoreError::Decode(
        FoldRefusal::NotCanonicalSnapshot {
            landing: *landing,
            state: *state,
        }
        .detail(),
    )
}

/// The store upgrade's action: the frames of one delta for each held landing that has none
/// ([`needs`]), in chain order. Every landing's legacy SNAPSHOT is checked first, in landing
/// order, before any delta is built: a SNAPSHOT that is not stored canonically refuses with
/// `landing {landing}: snapshot {state} is not canonical` (`StoreError::Decode`), and nothing is
/// prepared. Reads only: at most two full maps are held at a time.
pub(crate) fn prepare(reader: &StoreReader) -> Result<Vec<(u8, Vec<u8>)>, StoreError> {
    let need = needs(reader)?;
    for (l, n) in &need {
        let (raw, payload) = reader.get_kind_raw(kinds::SNAPSHOT, &n.state)?;
        if !canonical_snapshot(raw, &payload) {
            return Err(not_canonical_store(l, &n.state));
        }
    }
    let mut out: Vec<(u8, Vec<u8>)> = Vec::new();
    build(
        &need,
        &mut |prev: &Hash| match reader.snapshot_delta_of(prev)? {
            Some((delta, _)) => Ok(Outside::Base {
                delta,
                map: reader.snapshot_at(prev)?.blocks,
            }),
            None => Err(StoreError::Decode(
                FoldRefusal::MissingDelta { landing: *prev }.detail(),
            )),
        },
        &mut |_, state: &Hash| {
            Ok(reader
                .get_typed::<StateSnapshot>(kinds::SNAPSHOT, state)?
                .blocks)
        },
        &not_canonical_store,
        &mut |d: SnapshotDelta| {
            out.push(codec::item_new(kinds::SNAPSHOT_DELTA, &d));
            Ok(())
        },
    )?;
    Ok(out)
}

fn refused(detail: String) -> StoreError {
    StoreError::Refused(detail)
}

/// The upgrade batch's object rule, before anything is appended: `objects` are exactly what the
/// registered snapshot-delta step produces, when the receipt's `steps` name it, and nothing
/// otherwise. Each object is a canonical SNAPSHOT_DELTA of a held landing that has none
/// ([`needs`]), with that landing's `state` as its `full`, its `set` and `drop` in canonical
/// order; each such landing has exactly one, and its `base` is the previous landing's delta (in
/// the batch or held). Another kind, or any object without the step, is [`UPGRADE_BATCH`]; a
/// delta rule names the delta or the landing with the closed detail texts. The folds (each
/// delta against its previous map, and the result against `full`) are the store check's, which
/// runs after the batch and before the upgrade completes.
pub(crate) fn check_batch(
    reader: &StoreReader,
    steps: &[String],
    objects: &[(u8, Vec<u8>)],
) -> Result<(), StoreError> {
    if !steps.iter().any(|s| s == SNAPSHOT_DELTA_STEP) {
        if objects.is_empty() {
            return Ok(());
        }
        return Err(refused(UPGRADE_BATCH.into()));
    }
    let need = needs(reader)?;
    let mut by_landing: BTreeMap<Hash, (Hash, SnapshotDelta)> = BTreeMap::new();
    for (raw, payload) in objects {
        if codec::logical_kind(*raw) != kinds::SNAPSHOT_DELTA {
            return Err(refused(UPGRADE_BATCH.into()));
        }
        let id = Hash::of(payload);
        let not_canonical =
            || refused(format!("snapshot delta {id}: set or drop is not canonical"));
        let d: SnapshotDelta =
            codec::decode_canonical(*raw, payload).map_err(|_| not_canonical())?;
        // the form a delta has whatever its previous map: `set` and `drop` strictly ascending,
        // no id in both. Checked against a map of exactly the dropped ids, which no `set` pair
        // can match once they share no id; the rest of the form needs the previous map, and the
        // store check folds every delta before the upgrade completes
        let dropped: BlockMap = d.drop.iter().map(|k| (k.clone(), Hash([0; 32]))).collect();
        if d.set.iter().any(|(k, _)| dropped.contains_key(k))
            || rdy_model::validate_snapshot_delta_form(&dropped, &d).is_err()
        {
            return Err(not_canonical());
        }
        match need.get(&d.landing) {
            Some(n) if n.state == d.full => {}
            _ => return Err(refused(FoldRefusal::Identity { delta: id }.detail())),
        }
        let landing = d.landing;
        if by_landing.insert(landing, (id, d)).is_some() {
            return Err(refused(FoldRefusal::MissingDelta { landing }.detail()));
        }
    }
    for (l, n) in &need {
        let Some((id, d)) = by_landing.get(l) else {
            return Err(refused(FoldRefusal::MissingDelta { landing: *l }.detail()));
        };
        let want = match n.prev {
            None => None,
            Some(p) => match by_landing.get(&p) {
                Some((pid, _)) => Some(*pid),
                None => reader.snapshot_delta_of(&p)?.map(|(h, _)| h),
            },
        };
        if d.base != want || (n.prev.is_some() && want.is_none()) {
            return Err(refused(FoldRefusal::Base { delta: *id }.detail()));
        }
    }
    Ok(())
}

/// The take's action (`crate::take_batch`): one canonical delta for each new landing of a legacy
/// upload (`landings`, read through `source`, the union over the holder), in chain order. A
/// landing's base is its `prev`'s delta: made here for a new `prev`, else the holder's own, whose
/// map the holder reads. Each landing's map is its SNAPSHOT, from the union or the holder; the
/// take's record rules have checked that its bytes are canonical. Returns each delta with its
/// frame.
pub(crate) fn take_deltas(
    reader: &StoreReader,
    source: &mut dyn ObjectSource,
    landings: &[Hash],
) -> Result<Vec<(SnapshotDelta, (u8, Vec<u8>))>, StreamError> {
    let held = |e: StoreError, landing: Hash| {
        crate::closure::store_error(
            e,
            Some(ObjectKey {
                kind: kinds::LANDING,
                hash: landing,
            }),
        )
    };
    let mut need: BTreeMap<Hash, Need> = BTreeMap::new();
    for l in landings {
        let key = ObjectKey {
            kind: kinds::LANDING,
            hash: *l,
        };
        let rec = crate::stream_source::typed::<LandingRecord>(source, key)?
            .ok_or_else(|| StreamError::unresolved(0, 0, key))?;
        need.insert(*l, Need::of(&rec));
    }
    let mut out = Vec::new();
    build(
        &need,
        &mut |prev: &Hash| match reader.snapshot_delta_of(prev).map_err(|e| held(e, *prev))? {
            Some((delta, _)) => Ok(Outside::Base {
                delta,
                map: reader.snapshot_at(prev).map_err(|e| held(e, *prev))?.blocks,
            }),
            None => Err(StreamError::broken_rule(
                0,
                0,
                &FoldRefusal::MissingDelta { landing: *prev }.detail(),
            )),
        },
        &mut |_, state: &Hash| {
            let key = ObjectKey {
                kind: kinds::SNAPSHOT,
                hash: *state,
            };
            Ok(crate::stream_source::typed::<StateSnapshot>(source, key)?
                .ok_or_else(|| StreamError::unresolved(0, 0, key))?
                .blocks)
        },
        &|l: &Hash, s: &Hash| {
            StreamError::broken_rule(
                0,
                0,
                &FoldRefusal::NotCanonicalSnapshot {
                    landing: *l,
                    state: *s,
                }
                .detail(),
            )
        },
        &mut |d: SnapshotDelta| {
            let item = codec::item_new(kinds::SNAPSHOT_DELTA, &d);
            out.push((d, item));
            Ok(())
        },
    )?;
    Ok(out)
}

/// What a load collects while it ingests a log, for its tail batch's deltas: each present
/// LANDING's `prev` and `state`, and each present SNAPSHOT's place in the new log.
#[derive(Default)]
pub(crate) struct LoadHistory {
    landings: BTreeMap<Hash, Need>,
    snapshots: HashMap<Hash, Location>,
}

impl LoadHistory {
    /// Notes the present frame `hash` of raw kind `raw` that the ingest wrote at `loc`.
    pub(crate) fn frame(&mut self, raw: u8, hash: Hash, loc: Location, payload: &[u8]) {
        match codec::logical_kind(raw) {
            kinds::LANDING => {
                // a landing that does not decode is the load's fsck's to refuse
                if let Ok(rec) = codec::decode_bounded::<LandingRecord>(raw, payload) {
                    self.landings.entry(hash).or_insert(Need::of(&rec));
                }
            }
            kinds::SNAPSHOT => {
                self.snapshots.entry(hash).or_insert(loc);
            }
            _ => {}
        }
    }

    /// The deltas of the collected landings that hold their SNAPSHOT, read back from `log`, in
    /// chain order. A landing whose SNAPSHOT the log does not hold (a foreign-removal proof's
    /// source landing), or whose `prev` gets no delta, gets none: the load's fsck judges it.
    /// The ingest has checked that each SNAPSHOT it wrote is canonical.
    pub(crate) fn deltas(&self, log: &SegmentLog) -> Result<Vec<(u8, Vec<u8>)>, StoreError> {
        let need: BTreeMap<Hash, Need> = self
            .landings
            .iter()
            .filter(|(_, n)| self.snapshots.contains_key(&n.state))
            .map(|(h, n)| (*h, *n))
            .collect();
        let mut out: Vec<(u8, Vec<u8>)> = Vec::new();
        let dump = |what: String| StoreError::Dump { at: 0, what };
        build(
            &need,
            &mut |_| Ok(Outside::Skip),
            &mut |_, state: &Hash| {
                let (raw, payload) = log.read(self.snapshots[state])?;
                codec::decode_canonical::<StateSnapshot>(raw, &payload)
                    .map(|s| s.blocks)
                    .map_err(|_| dump(crate::snapshot::noncanonical_snapshot(state)))
            },
            &|l: &Hash, s: &Hash| {
                dump(
                    FoldRefusal::NotCanonicalSnapshot {
                        landing: *l,
                        state: *s,
                    }
                    .detail(),
                )
            },
            &mut |d: SnapshotDelta| {
                out.push(codec::item_new(kinds::SNAPSHOT_DELTA, &d));
                Ok(())
            },
        )?;
        Ok(out)
    }
}

/// The links of a delta as write-order children: its landing, its base and each set version.
pub(crate) fn delta_children(d: &SnapshotDelta) -> Vec<ObjectKey> {
    let mut out = vec![ObjectKey {
        kind: kinds::LANDING,
        hash: d.landing,
    }];
    if let Some(b) = d.base {
        out.push(ObjectKey {
            kind: kinds::SNAPSHOT_DELTA,
            hash: b,
        });
    }
    out.extend(d.set.iter().map(|(_, v)| ObjectKey {
        kind: kinds::BLOCK,
        hash: *v,
    }));
    out.sort_unstable();
    out.dedup();
    out
}
