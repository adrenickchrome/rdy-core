//! The state of one canon tip as a sync stream (format 5, purpose `canon_state`; D-GGTYA,
//! D-4PV25): what a receiver with no history needs to open the project at that tip.
//!
//! # The selection
//!
//! [`select_state`] reads one root LANDING up to a log end and selects, with no walk of the
//! history closure:
//!
//! - the root landing and its map, read through the store's one map read (`snapshot_at`: a held
//!   SNAPSHOT, the map cache, or the fold of the deltas). The map's canonical SNAPSHOT must hash
//!   to the landing's `state`; the stream carries that SNAPSHOT, made from the map for the stream
//!   only (no store need hold it, and nothing stores it);
//! - every BLOCK version the map names, with each id checked against the map;
//! - the root's SNAPSHOT_DELTA, when the store lists `snapshot_delta` (a missing one is the
//!   snapshot read's refusal), checked against the landing and the map;
//! - every TAG that names the root; every SIGNATURE whose subject is the root or such a tag; every
//!   ANSWER that targets the root or such a tag;
//! - the root's sealed PLAN;
//! - each CHECK_REQUEST whose subject names one of the root's changes or its proposal versions,
//!   and each REVIEW of such a request; each DISPUTE of the root and each DISPUTE_RESOLUTION of
//!   such a dispute (as a canon history attaches them);
//! - each active REDACTION_APPLY of a request that names a selected BLOCK, BLOB_MANIFEST or
//!   BLOB_CHUNK;
//! - and the targets of every never-absent link of a selected record ([`state_link_required`]):
//!   SNAPSHOT to BLOCK, a delta's `set` to BLOCK, BLOCK to BLOB_MANIFEST, BLOB_MANIFEST to
//!   BLOB_CHUNK, REDACTION_APPLY to REDACTION, REDACTION to its residues, a dropped record's
//!   residue and apply, a plan delta's `prev` (to the nearest whole plan version) and a seal's
//!   answers.
//!
//! Every other link of a selected record is a frontier edge (D-4PV25): its target, when the
//! selection does not hold it, is a prerequisite of the stream, which a later history fetch
//! brings. The source must hold it. The table is closed: nothing else is selected.
//!
//! # The check of a fetch
//!
//! One file of a canon state is checked as any batch is (`crate::sync_stream`): its records, its
//! dependencies and its prerequisites. [`StateUnion`] then checks the files of one fetch
//! together: the root landing, its canonical SNAPSHOT and (under `snapshot_delta`) its one
//! delta, the map's block ids, and every never-absent link resolved within the union. The same
//! [`state_link_required`] rule governs the export and this check.
use crate::closure::{StoreSource, dependency_order, store_error};
use crate::decoded_record::DecodedRecord;
use crate::format::{FEATURE_PARTIAL_HISTORY, FEATURE_SNAPSHOT_DELTA, StoreFeatures};
use crate::index::IndexRead;
use crate::kinds;
use crate::object_refs::{ObjectKey, ObjectRef};
use crate::plan_record::PlanRecord;
use crate::segment::Location;
use crate::snapshot::{FoldRefusal, SnapshotSource, StoreSnapshots, stream_refusal};
use crate::stream_error::{
    LIMIT_GRAPH_EDGES, LIMIT_OBJECT_RECORDS, LIMIT_PREREQUISITES, Reason, StreamError,
};
use crate::sync_proof::ObjectSource;
use crate::sync_stream::{RecordFeatures, StreamRecord, sync_kinds};
use crate::{StoreError, StoreReader};
use rdy_model::{BlockId, Hash, LandingRecord, SnapshotDelta, StateSnapshot};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// The most bytes of a canon state's SNAPSHOT payload: the record bound.
pub const MAX_SNAPSHOT_BYTES: u64 = crate::bounded::MAX_RECORD_BYTES as u64;

thread_local! {
    /// This thread's bound on the SNAPSHOT payload (a test hook lowers it).
    static SNAPSHOT_BYTES: Cell<u64> = const { Cell::new(MAX_SNAPSHOT_BYTES) };
}

/// While held, this thread's state selections keep the earlier SNAPSHOT bound again on drop.
#[doc(hidden)]
#[must_use]
pub struct SnapshotBytesGuard(u64);

impl Drop for SnapshotBytesGuard {
    fn drop(&mut self) {
        SNAPSHOT_BYTES.with(|c| c.set(self.0));
    }
}

/// Test hook: this thread's state selections refuse a SNAPSHOT payload over `bytes` until the
/// guard drops, with the released bound's text. Production never lowers it.
#[doc(hidden)]
pub fn lower_snapshot_bytes_for_tests(bytes: u64) -> SnapshotBytesGuard {
    SnapshotBytesGuard(SNAPSHOT_BYTES.with(|c| c.replace(bytes)))
}

/// The refusal of a SNAPSHOT payload over its bound: the record bound's E_LIMIT.
fn snapshot_limit(state: Hash) -> StreamError {
    StreamError::from_record(
        0,
        0,
        &crate::bounded::RecordError::Limit {
            name: crate::bounded::RECORD_BYTES,
            maximum: MAX_SNAPSHOT_BYTES,
        },
    )
    .with_object(Some(ObjectKey {
        kind: kinds::SNAPSHOT,
        hash: state,
    }))
}

/// One selected record: the keys its links name (resolved, sorted, no repeats), the removal
/// proof a placeholder names, and its frame's payload bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateNode {
    pub children: Vec<ObjectKey>,
    pub proof: Option<ObjectKey>,
    pub len: u64,
}

/// One canon tip's state (see the module docs): its records in dependency order, each with its
/// links, the SNAPSHOT made for the stream, the features the records need with
/// `partial_history`, and the frontier edges the selection omits (sorted, distinct).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateSelection {
    pub root: Hash,
    pub records: Vec<ObjectKey>,
    pub nodes: BTreeMap<ObjectKey, StateNode>,
    pub snapshot: Hash,
    snapshot_payload: Vec<u8>,
    pub features: StoreFeatures,
    pub prerequisites: Vec<ObjectKey>,
}

impl StateSelection {
    /// The root LANDING's key.
    pub fn root_key(&self) -> ObjectKey {
        ObjectKey {
            kind: kinds::LANDING,
            hash: self.root,
        }
    }

    /// The SNAPSHOT made from the root's map, as the stream carries it (encoding 0).
    pub fn snapshot_record(&self) -> StreamRecord {
        StreamRecord::Present {
            key: ObjectKey {
                kind: kinds::SNAPSHOT,
                hash: self.snapshot,
            },
            encoding: 0,
            payload: self.snapshot_payload.clone(),
        }
    }

    /// The records of the selection as a batch source: the SNAPSHOT from memory, every other
    /// record from `reader` before `end` (its original bytes, or its placeholder).
    pub fn source<'a>(&'a self, reader: &'a StoreReader, end: Location) -> StateSource<'a> {
        StateSource {
            store: StoreSource { reader, end },
            selection: self,
        }
    }
}

/// A canon state's records as a batch source ([`StateSelection::source`]).
pub struct StateSource<'a> {
    store: StoreSource<'a>,
    selection: &'a StateSelection,
}

impl ObjectSource for StateSource<'_> {
    fn record(&mut self, key: ObjectKey) -> Result<Option<StreamRecord>, StreamError> {
        if key.kind == kinds::SNAPSHOT && key.hash == self.selection.snapshot {
            return Ok(Some(self.selection.snapshot_record()));
        }
        self.store.record(key)
    }

    fn snapshot_delta_of(&mut self, landing: &Hash) -> Result<Option<Hash>, StreamError> {
        self.store.snapshot_delta_of(landing)
    }
}

/// Whether the link of record `from` (decoded as `d`) to `target` must resolve within a canon
/// state rooted at `root` (D-4PV25, the never-absent row, and every link to the root): SNAPSHOT
/// to BLOCK; a delta's `set` to BLOCK; BLOCK to BLOB_MANIFEST; BLOB_MANIFEST to BLOB_CHUNK;
/// REDACTION_APPLY to REDACTION; REDACTION to its residues; a dropped record's residue; a plan
/// delta's `prev`; a sealed plan's answers; any link to the root landing. Every other link is a
/// frontier edge. A dropped record's apply is a proof, required as well ([`StreamRecord::proof`]).
pub fn state_link_required(
    root: Hash,
    from: ObjectKey,
    d: &DecodedRecord,
    target: ObjectKey,
) -> bool {
    if target.kind == kinds::LANDING && target.hash == root {
        return true;
    }
    match (d, target.kind) {
        (DecodedRecord::Dropped, _) => true,
        (DecodedRecord::Snapshot(_), kinds::BLOCK) => true,
        (DecodedRecord::SnapshotDelta(_), kinds::BLOCK) => true,
        (DecodedRecord::Block(_), kinds::BLOB_MANIFEST) => true,
        (DecodedRecord::BlobManifest(_), kinds::BLOB_CHUNK) => true,
        (DecodedRecord::RedactionApply(_), kinds::REDACTION) => true,
        (DecodedRecord::Redaction(_), kinds::REDACTION_RESIDUE) => true,
        (DecodedRecord::Plan(_), kinds::ANSWER) => true,
        (DecodedRecord::Plan(p), kinds::PLAN) => {
            matches!(**p, PlanRecord::Delta(_)) && p.prev() == Some(target.hash)
        }
        _ => {
            let _ = from;
            false
        }
    }
}

/// The bounds of one selection: the batch check's, so a test lowers them with the check's hook
/// (`crate::sync_stream::lower_caps_for_tests`), and the SNAPSHOT payload's.
fn bounds() -> (crate::sync_stream::CheckCaps, u64) {
    (crate::sync_stream::caps(), SNAPSHOT_BYTES.with(Cell::get))
}

/// Selects the state of the root LANDING `root` from `reader` before `end` (see the module docs).
/// A root the source does not hold is E_UNRESOLVED_REF; a map that does not read is the map
/// read's refusal (a missing delta, a fold that does not match), a SNAPSHOT that is not the
/// canonical encoding of the root's map is E_BAD_RECORD; a selected record or a frontier target
/// the source does not hold is E_UNRESOLVED_REF; the bounds of one batch (records, edges,
/// prerequisites) and of one record (the SNAPSHOT) are E_LIMIT, checked before each grows.
pub fn select_state(
    reader: &StoreReader,
    root: Hash,
    end: Location,
) -> Result<StateSelection, StreamError> {
    let read = reader
        .read_for_closure()
        .map_err(|e| store_error(e, None))?;
    let features = crate::snapshot::held_features(reader).map_err(|e| store_error(e, None))?;
    let deltas = features.has(FEATURE_SNAPSHOT_DELTA);
    let (caps, snapshot_cap) = bounds();
    let root_key = ObjectKey {
        kind: kinds::LANDING,
        hash: root,
    };
    let mut sel = Select {
        reader,
        read,
        end,
        root,
        features,
        caps,
        nodes: BTreeMap::new(),
        decoded_kinds: HashMap::new(),
        queue: Vec::new(),
        queued: HashSet::new(),
        edges: 0,
        needs: RecordFeatures::default(),
        snapshot: None,
    };
    let landing: LandingRecord = match sel.store_record(root_key)? {
        Some(r) => match DecodedRecord::decode(&r)
            .map_err(|e| StreamError::from_record(0, 0, &e).with_object(Some(root_key)))?
        {
            DecodedRecord::Landing(l) => *l,
            _ => return Err(StreamError::unresolved(0, 0, root_key)),
        },
        None => return Err(StreamError::unresolved(0, 0, root_key)),
    };

    // the map, through the store's one map read, and its canonical SNAPSHOT
    let map = read_map(reader, &root, &landing)?;
    let size = snapshot_len(&map);
    if size > snapshot_cap {
        return Err(snapshot_limit(landing.state));
    }
    let mut payload = Vec::new();
    payload
        .try_reserve_exact(size as usize)
        .map_err(|_| StreamError::allocation(None, 0))?;
    payload = postcard::to_extend(&map, payload).map_err(|_| StreamError::allocation(None, 0))?;
    if Hash::of(&payload) != landing.state {
        return Err(stream_refusal(FoldRefusal::NotCanonicalSnapshot {
            landing: root,
            state: landing.state,
        }));
    }
    if let Some(held) = held_canonical(reader, &landing.state)?
        && !held
    {
        return Err(stream_refusal(FoldRefusal::NotCanonicalSnapshot {
            landing: root,
            state: landing.state,
        }));
    }
    let snapshot_key = ObjectKey {
        kind: kinds::SNAPSHOT,
        hash: landing.state,
    };
    sel.snapshot = Some((landing.state, payload));

    sel.queue_key(root_key)?;
    sel.queue_key(snapshot_key)?;
    // the root's delta, checked against the landing and the map
    if deltas {
        let Some(delta) = sel.delta_of(&root)? else {
            return Err(stream_refusal(FoldRefusal::MissingDelta { landing: root }));
        };
        let key = ObjectKey {
            kind: kinds::SNAPSHOT_DELTA,
            hash: delta,
        };
        let d: SnapshotDelta = crate::stream_source::typed(&mut sel.source(), key)?
            .ok_or_else(|| StreamError::unresolved(0, 0, key))?;
        if d.landing != root || d.full != landing.state {
            return Err(stream_refusal(FoldRefusal::Identity { delta }));
        }
        if !delta_agrees(&d, &map.blocks) {
            return Err(stream_refusal(FoldRefusal::Mismatch { delta }));
        }
        sel.queue_key(key)?;
    }
    sel.drain()?;
    // each block the map names is that block's version
    for (id, v) in &map.blocks {
        let key = ObjectKey {
            kind: kinds::BLOCK,
            hash: *v,
        };
        if let Some(found) = sel.block_ids(key)?
            && found != *id
        {
            return Err(wrong_block(landing.state, id, *v));
        }
    }

    // the tags of the root, their signatures and the answers to the root and its tags
    let tags = sel.pass(kinds::TAG, |d| match d {
        DecodedRecord::Tag(t) => t.landing == root,
        _ => false,
    })?;
    let mut subjects: HashSet<Hash> = tags.iter().map(|k| k.hash).collect();
    subjects.insert(root);
    let mut ordered: Vec<Hash> = subjects.iter().copied().collect();
    ordered.sort_unstable();
    for s in &ordered {
        let found = sel
            .read
            .signatures_before(s, crate::signature::MAX_PER_SUBJECT, end)
            .map_err(|e| store_error(e, Some(root_key)))?;
        for g in found {
            sel.queue_key(ObjectKey {
                kind: kinds::SIGNATURE,
                hash: g,
            })?;
        }
    }
    sel.pass(kinds::ANSWER, |d| match d {
        DecodedRecord::Answer(a) => a.targets.iter().any(|t| subjects.contains(t)),
        _ => false,
    })?;
    // the sealed plan: its chain and its answers follow by their links
    if let Some((plan, _)) = sel
        .read
        .plan_seal(&root)
        .map_err(|e| store_error(e, Some(root_key)))?
        .filter(|(_, at)| *at < end)
    {
        sel.queue_key(ObjectKey {
            kind: kinds::PLAN,
            hash: plan,
        })?;
    }
    // the carried shapes a canon history attaches to the root
    let changes: HashSet<Hash> = landing.changes.iter().copied().collect();
    let versions: HashSet<Hash> = std::iter::once(landing.proposal)
        .chain(landing.approved_version)
        .collect();
    let requests: HashSet<Hash> = sel
        .pass(kinds::CHECK_REQUEST, |d| match d {
            DecodedRecord::CheckRequest(c) => {
                c.subject.change.is_some_and(|h| changes.contains(&h))
                    || c.subject.version.is_some_and(|h| versions.contains(&h))
            }
            _ => false,
        })?
        .into_iter()
        .map(|k| k.hash)
        .collect();
    if !requests.is_empty() {
        sel.pass(kinds::REVIEW, |d| match d {
            DecodedRecord::Review(r) => matches!(
                r.subject,
                rdy_model::review::ReviewSubjectV1::Check { request } if requests.contains(&request)
            ),
            _ => false,
        })?;
    }
    let disputes: HashSet<Hash> = sel
        .pass(kinds::DISPUTE, |d| match d {
            DecodedRecord::Dispute(x) => x.landing == root,
            _ => false,
        })?
        .into_iter()
        .map(|k| k.hash)
        .collect();
    if !disputes.is_empty() {
        sel.pass(kinds::DISPUTE_RESOLUTION, |d| match d {
            DecodedRecord::DisputeResolution(r) => disputes.contains(&r.dispute),
            _ => false,
        })?;
    }
    sel.drain()?;
    // the active removal proofs of the selected content, until no new one joins
    let mut asked: HashSet<ObjectKey> = HashSet::new();
    loop {
        let content: Vec<ObjectKey> = sel
            .nodes
            .keys()
            .filter(|k| {
                matches!(
                    k.kind,
                    kinds::BLOCK | kinds::BLOB_MANIFEST | kinds::BLOB_CHUNK
                ) && !asked.contains(k)
            })
            .copied()
            .collect();
        if content.is_empty() {
            break;
        }
        for k in content {
            asked.insert(k);
            let err = |e| store_error(e, Some(k));
            for request in sel
                .read
                .redaction_requests_naming(k.kind, &k.hash)
                .map_err(err)?
            {
                if let Some((apply, at)) = sel.read.redaction_applied(&request).map_err(err)?
                    && at < end
                {
                    sel.queue_key(ObjectKey {
                        kind: kinds::REDACTION_APPLY,
                        hash: apply,
                    })?;
                }
            }
        }
        sel.drain()?;
    }
    sel.finish(landing)
}

/// The map of landing `root` (its record `landing`) through the store's map read and cache, its
/// refusals as stream errors.
fn read_map(
    reader: &StoreReader,
    root: &Hash,
    landing: &LandingRecord,
) -> Result<StateSnapshot, StreamError> {
    crate::snapshot::read_landing_snapshot(
        &mut StateMaps {
            inner: StoreSnapshots { reader },
        },
        Some(reader.map_cache()),
        root,
        landing,
    )
}

/// Whether the store holds SNAPSHOT `state` as the canonical encoding of its map; None when it
/// holds no SNAPSHOT of that hash.
fn held_canonical(reader: &StoreReader, state: &Hash) -> Result<Option<bool>, StreamError> {
    StoreSnapshots { reader }
        .held_snapshot_canonical(state)
        .map_err(|e| {
            store_error(
                e,
                Some(ObjectKey {
                    kind: kinds::SNAPSHOT,
                    hash: *state,
                }),
            )
        })
}

/// The bytes of the canonical encoding of `map`, counted without building it.
fn snapshot_len(map: &StateSnapshot) -> u64 {
    struct Count(u64);
    impl Extend<u8> for Count {
        fn extend<I: IntoIterator<Item = u8>>(&mut self, iter: I) {
            self.0 += iter.into_iter().count() as u64;
        }
    }
    postcard::to_extend(map, Count(0)).map_or(u64::MAX, |c| c.0)
}

/// Whether delta `d` agrees with the full map `map` it records: each set pair is the map's, and
/// no dropped id is in the map.
fn delta_agrees(d: &SnapshotDelta, map: &BTreeMap<BlockId, Hash>) -> bool {
    d.set.iter().all(|(id, v)| map.get(id) == Some(v))
        && d.drop.iter().all(|id| !map.contains_key(id))
}

/// The refusal of a map whose entry for block `id` names `version`, a version of another block.
fn wrong_block(state: Hash, id: &BlockId, version: Hash) -> StreamError {
    StreamError {
        code: "E_BAD_RECORD",
        message: format!("snapshot {state}: block {id} names version {version} of another block"),
        fix: "upload the history as its source project exported it".into(),
        record: None,
        offset: 0,
        object: Some(ObjectKey {
            kind: kinds::SNAPSHOT,
            hash: state,
        }),
    }
}

/// The store's map read with its refusals as stream errors ([`stream_refusal`]).
struct StateMaps<'a> {
    inner: StoreSnapshots<'a>,
}

fn map_error(e: StoreError) -> StreamError {
    store_error(e, None)
}

impl SnapshotSource for StateMaps<'_> {
    type Error = StreamError;

    fn landing(&mut self, landing: &Hash) -> Result<Option<LandingRecord>, StreamError> {
        self.inner.landing(landing).map_err(map_error)
    }

    fn held_snapshot(
        &mut self,
        landing: &Hash,
        state: &Hash,
    ) -> Result<Option<StateSnapshot>, StreamError> {
        self.inner.held_snapshot(landing, state).map_err(map_error)
    }

    fn held_snapshot_canonical(&mut self, state: &Hash) -> Result<Option<bool>, StreamError> {
        self.inner.held_snapshot_canonical(state).map_err(map_error)
    }

    fn delta_of(&mut self, landing: &Hash) -> Result<Option<Hash>, StreamError> {
        self.inner.delta_of(landing).map_err(map_error)
    }

    fn delta(&mut self, delta: &Hash) -> Result<SnapshotDelta, StreamError> {
        self.inner.delta(delta).map_err(map_error)
    }

    fn requires_delta(&mut self) -> Result<bool, StreamError> {
        self.inner.requires_delta().map_err(map_error)
    }

    fn refused(&mut self, refusal: FoldRefusal) -> StreamError {
        stream_refusal(refusal)
    }
}

/// One selection in progress.
struct Select<'a> {
    reader: &'a StoreReader,
    read: IndexRead,
    end: Location,
    root: Hash,
    /// The source store's features: the links a record names follow them.
    features: StoreFeatures,
    caps: crate::sync_stream::CheckCaps,
    nodes: BTreeMap<ObjectKey, StateNode>,
    /// The block id of each selected present BLOCK.
    decoded_kinds: HashMap<ObjectKey, BlockId>,
    queue: Vec<ObjectKey>,
    queued: HashSet<ObjectKey>,
    edges: u64,
    needs: RecordFeatures,
    snapshot: Option<(Hash, Vec<u8>)>,
}

impl Select<'_> {
    fn source(&self) -> StoreSource<'_> {
        StoreSource {
            reader: self.reader,
            end: self.end,
        }
    }

    /// The record of `key` from the store before the end, or the SNAPSHOT made for the stream.
    fn store_record(&self, key: ObjectKey) -> Result<Option<StreamRecord>, StreamError> {
        if let Some((h, payload)) = &self.snapshot
            && key.kind == kinds::SNAPSHOT
            && key.hash == *h
        {
            return Ok(Some(StreamRecord::Present {
                key,
                encoding: 0,
                payload: payload.clone(),
            }));
        }
        self.source().record(key)
    }

    fn delta_of(&self, landing: &Hash) -> Result<Option<Hash>, StreamError> {
        let key = ObjectKey {
            kind: kinds::LANDING,
            hash: *landing,
        };
        Ok(self
            .read
            .snapshot_delta(landing)
            .map_err(|e| store_error(e, Some(key)))?
            .filter(|(_, at)| *at < self.end)
            .map(|(h, _)| h))
    }

    /// Queues `key` once, within the record bound.
    fn queue_key(&mut self, key: ObjectKey) -> Result<(), StreamError> {
        if self.queued.contains(&key) {
            return Ok(());
        }
        if self.queued.len() as u64 >= self.caps.records {
            return Err(StreamError::limit(None, 0, LIMIT_OBJECT_RECORDS));
        }
        let grow = |_| StreamError::allocation(None, 0);
        self.queued.try_reserve(1).map_err(grow)?;
        self.queue.try_reserve(1).map_err(grow)?;
        self.queued.insert(key);
        self.queue.push(key);
        Ok(())
    }

    /// Reads every queued record, notes its links, and queues the targets of its never-absent
    /// links, until the queue is empty.
    fn drain(&mut self) -> Result<(), StreamError> {
        while let Some(k) = self.queue.pop() {
            self.expand(k)?;
        }
        Ok(())
    }

    fn expand(&mut self, k: ObjectKey) -> Result<(), StreamError> {
        if k.kind == kinds::FOREIGN {
            return Err(
                StreamError::bad_record(0, 0, Reason::OutsideTagClosure).with_object(Some(k))
            );
        }
        let Some(record) = self.store_record(k)? else {
            return Err(StreamError::unresolved(0, 0, k));
        };
        let bad = |e: crate::bounded::RecordError| {
            StreamError::from_record(0, 0, &e).with_object(Some(k))
        };
        let d = DecodedRecord::decode(&record).map_err(bad)?;
        let refs = record.refs_decoded(&self.features, &d).map_err(bad)?;
        let mut children = Vec::new();
        children
            .try_reserve_exact(refs.len())
            .map_err(|_| StreamError::allocation(None, 0))?;
        for r in refs {
            children.push(self.resolve(k, r)?);
        }
        children.sort_unstable();
        children.dedup();
        let proof = record.proof();
        self.edges += children.len() as u64 + u64::from(proof.is_some());
        if self.edges > self.caps.edges {
            return Err(StreamError::limit(None, 0, LIMIT_GRAPH_EDGES).with_object(Some(k)));
        }
        for c in &children {
            if state_link_required(self.root, k, &d, *c) {
                self.queue_key(*c)?;
            }
        }
        if let Some(p) = proof {
            self.queue_key(p)?;
        }
        if let DecodedRecord::Block(b) = &d {
            self.decoded_kinds.insert(k, b.id.clone());
        }
        self.needs.add(&record);
        self.nodes.insert(
            k,
            StateNode {
                children,
                proof,
                len: record.frame_len(),
            },
        );
        Ok(())
    }

    /// The key a link of record `from` names: its own kind, or for a kind-None link the one
    /// portable object of its hash the store holds before the end. None held is E_UNRESOLVED_REF,
    /// two are `ambiguous object kind`, and a foreign entry is outside the state.
    fn resolve(&self, from: ObjectKey, r: ObjectRef) -> Result<ObjectKey, StreamError> {
        if let Some(kind) = r.kind {
            return Ok(ObjectKey { kind, hash: r.hash });
        }
        let source = self.source();
        let mut found = None;
        for &kind in sync_kinds() {
            if source.holds(kind, &r.hash)? {
                if found.is_some() {
                    return Err(StreamError::bad_record(0, 0, Reason::AmbiguousObjectKind)
                        .with_object(Some(from)));
                }
                found = Some(ObjectKey { kind, hash: r.hash });
            }
        }
        if let Some(k) = found {
            return Ok(k);
        }
        if source.holds(kinds::FOREIGN, &r.hash)? {
            return Err(
                StreamError::bad_record(0, 0, Reason::OutsideTagClosure).with_object(Some(
                    ObjectKey {
                        kind: kinds::FOREIGN,
                        hash: r.hash,
                    },
                )),
            );
        }
        Err(StreamError::unresolved(
            0,
            0,
            ObjectKey {
                kind: 0,
                hash: r.hash,
            },
        )
        .with_object(Some(from)))
    }

    /// The block id of the selected BLOCK `key`, when it is present (a placeholder has none).
    fn block_ids(&self, key: ObjectKey) -> Result<Option<BlockId>, StreamError> {
        Ok(self.decoded_kinds.get(&key).cloned())
    }

    /// One pass over the records of `kind` before the end: each present record that `keep`
    /// accepts is queued. Returns the keys it queued.
    fn pass(
        &mut self,
        kind: u8,
        mut keep: impl FnMut(&DecodedRecord) -> bool,
    ) -> Result<Vec<ObjectKey>, StreamError> {
        let mut hashes: Vec<Hash> = Vec::new();
        let mut failed: Option<StreamError> = None;
        self.reader.note_kind_walk();
        let walked = self.read.for_each_object_of(kind, |h| {
            if hashes.try_reserve(1).is_err() {
                failed = Some(StreamError::allocation(None, 0));
                return Err(StoreError::Refused("the state's pass stopped".into()));
            }
            hashes.push(h);
            Ok(())
        });
        if let Some(e) = failed {
            return Err(e);
        }
        walked.map_err(|e| store_error(e, None))?;
        let mut found = Vec::new();
        for h in hashes {
            let key = ObjectKey { kind, hash: h };
            let Some(r @ StreamRecord::Present { .. }) = self.source().record(key)? else {
                continue;
            };
            let d = DecodedRecord::decode(&r)
                .map_err(|e| StreamError::from_record(0, 0, &e).with_object(Some(key)))?;
            if keep(&d) {
                found.push(key);
            }
        }
        for k in &found {
            self.queue_key(*k)?;
        }
        Ok(found)
    }

    /// The selection in dependency order, its frontier edges as prerequisites, and its features.
    fn finish(self, landing: LandingRecord) -> Result<StateSelection, StreamError> {
        let source = self.source();
        let mut prerequisites: BTreeSet<ObjectKey> = BTreeSet::new();
        for node in self.nodes.values() {
            for c in node.children.iter().chain(&node.proof) {
                if self.nodes.contains_key(c) || prerequisites.contains(c) {
                    continue;
                }
                // a frontier edge's target is the source's
                if !source.holds(c.kind, &c.hash)? {
                    return Err(StreamError::unresolved(0, 0, *c));
                }
                if prerequisites.len() >= self.caps.prerequisites {
                    return Err(StreamError::limit(None, 0, LIMIT_PREREQUISITES));
                }
                prerequisites.insert(*c);
            }
        }
        let root = ObjectKey {
            kind: kinds::LANDING,
            hash: self.root,
        };
        let children: BTreeMap<ObjectKey, Vec<ObjectKey>> = self
            .nodes
            .iter()
            .map(|(k, n)| (*k, n.children.clone()))
            .collect();
        let records = dependency_order(root, &children)?;
        let mut features = self.needs.finish();
        if let Some(f) = crate::format::registered_features()
            .into_iter()
            .find(|f| f.name == FEATURE_PARTIAL_HISTORY)
        {
            features = features.merged(&[f]);
        }
        let (snapshot, snapshot_payload) = self.snapshot.expect("the map was read");
        debug_assert_eq!(snapshot, landing.state);
        Ok(StateSelection {
            root: self.root,
            records,
            nodes: self.nodes,
            snapshot,
            snapshot_payload,
            features,
            prerequisites: prerequisites.into_iter().collect(),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// the check of one fetch's files together

/// The files of one canon-state fetch, checked together (see the module docs). Each file is
/// first checked alone (`crate::sync_stream::check_canon_state_prepared_with`), which hands each
/// record to [`StateUnion::add`]; [`StateUnion::finish`] then checks the union.
pub struct StateUnion {
    root: Hash,
    deltas: bool,
    keys: HashSet<ObjectKey>,
    /// Each never-absent link: the record that names it, its place, and its target.
    required: Vec<(ObjectKey, u64, u64, ObjectKey)>,
    landing: Option<LandingRecord>,
    snapshots: HashMap<Hash, BTreeMap<BlockId, Hash>>,
    root_deltas: Vec<(Hash, SnapshotDelta)>,
    block_ids: HashMap<Hash, BlockId>,
}

impl StateUnion {
    /// The union of a fetch rooted at the LANDING `root`, whose headers list `features`.
    pub fn new(root: Hash, features: &StoreFeatures) -> StateUnion {
        StateUnion {
            root,
            deltas: features.has(FEATURE_SNAPSHOT_DELTA),
            keys: HashSet::new(),
            required: Vec::new(),
            landing: None,
            snapshots: HashMap::new(),
            root_deltas: Vec::new(),
            block_ids: HashMap::new(),
        }
    }

    /// Notes record `r` (decoded as `d`), record `n` at `offset` of its file, whose links follow
    /// `features` (its header's).
    pub fn add(
        &mut self,
        n: u64,
        offset: u64,
        features: &StoreFeatures,
        r: &StreamRecord,
        d: &DecodedRecord,
    ) -> Result<(), StreamError> {
        let key = r.key();
        let grow = |_| StreamError::allocation(Some(n), offset);
        self.keys.try_reserve(1).map_err(grow)?;
        self.keys.insert(key);
        let refs = r
            .refs_decoded(features, d)
            .map_err(|e| StreamError::from_record(n, offset, &e).with_object(Some(key)))?;
        for e in refs {
            // a link of no fixed kind is required only when it names the root
            let target = match e.kind {
                Some(kind) => ObjectKey { kind, hash: e.hash },
                None if e.hash == self.root => ObjectKey {
                    kind: kinds::LANDING,
                    hash: e.hash,
                },
                None => continue,
            };
            if state_link_required(self.root, key, d, target) {
                self.required.try_reserve(1).map_err(grow)?;
                self.required.push((key, n, offset, target));
            }
        }
        if let Some(p) = r.proof() {
            self.required.try_reserve(1).map_err(grow)?;
            self.required.push((key, n, offset, p));
        }
        match d {
            DecodedRecord::Landing(l) if key.hash == self.root => {
                self.landing = Some((**l).clone());
            }
            DecodedRecord::Snapshot(s) => {
                self.snapshots.insert(key.hash, s.blocks.clone());
            }
            DecodedRecord::SnapshotDelta(x) if x.landing == self.root => {
                self.root_deltas.push((key.hash, (**x).clone()));
            }
            DecodedRecord::Block(b) => {
                self.block_ids.insert(key.hash, b.id.clone());
            }
            _ => {}
        }
        Ok(())
    }

    /// Checks the union: the root landing, its SNAPSHOT, its one delta (under `snapshot_delta`)
    /// and the map's block ids, and every never-absent link resolved within the union.
    pub fn finish(self) -> Result<(), StreamError> {
        let root = ObjectKey {
            kind: kinds::LANDING,
            hash: self.root,
        };
        let Some(landing) = &self.landing else {
            return Err(StreamError::unresolved(0, 0, root));
        };
        let Some(map) = self.snapshots.get(&landing.state) else {
            return Err(stream_refusal(FoldRefusal::MissingMap {
                landing: self.root,
                state: landing.state,
            }));
        };
        if self.deltas {
            let [(delta, d)] = &self.root_deltas[..] else {
                return Err(stream_refusal(FoldRefusal::MissingDelta {
                    landing: self.root,
                }));
            };
            if d.full != landing.state {
                return Err(stream_refusal(FoldRefusal::Identity { delta: *delta }));
            }
            if !delta_agrees(d, map) {
                return Err(stream_refusal(FoldRefusal::Mismatch { delta: *delta }));
            }
        }
        for (id, v) in map {
            if let Some(found) = self.block_ids.get(v)
                && found != id
            {
                return Err(wrong_block(landing.state, id, *v));
            }
        }
        for (from, n, offset, target) in &self.required {
            if !self.keys.contains(target) {
                let named = ObjectRef {
                    kind: Some(target.kind),
                    hash: target.hash,
                };
                return Err(StreamError::bad_record(
                    *n,
                    *offset,
                    Reason::UnlistedDependency(named),
                )
                .with_object(Some(*from)));
            }
        }
        Ok(())
    }
}
