//! The portable proof of a removal (D-3M81W: rdy "drops their bytes and keeps their hashes";
//! D-CC90E: a removal lands only with a human approval). A sync batch carries no physical place
//! of a source log, so the receiver cannot check an apply by its spans' places or by its batch,
//! as the native open does (`crate::redaction_io`). It checks what holds wherever the records
//! are, by following object links only:
//!
//! - the apply decodes and is canonically encoded;
//! - its landing was approved by a person (`human_approval`, policy never passes) and names the
//!   request in `checks`, each entry of which is a REDACTION record the source holds: the
//!   approval commits to the exact removal set, so no other request of the same carrier can
//!   reuse it;
//! - the request's carrier is a zero-op carrier change, the landing lands it and the proposal
//!   version the approver approved lists it;
//! - the request keeps its bounds, canonical manifest order and selector expansion, and the
//!   apply binds its report hash (`redaction_io::request_agrees`, the native rule);
//! - each span is projected to its logical tuple `(kind, hash, len, residue)`: repeated equal
//!   tuples (two physical copies) collapse, unequal tuples of one key refuse. A block or change
//!   needs the residue it binds (`redaction_io::residue_fits`, the native rule), a chunk has
//!   none and is a chunk of a selected blob;
//! - every payload the source still holds matches its tuple: its length, and for a block or a
//!   change exactly what its residue records (`residue_matches`); a dropped record keeps the
//!   tuple's length and residue.
//!
//! The approved set is the manifest's, never wider: an all-versions target never adds a version
//! the receiver holds and the approved manifest does not list, and shared blobs and report prose
//! add nothing. The result names logical objects only; the take finds the receiver's own copies.
use crate::bounded::{self, RecordError};
use crate::dump::check_dropped;
use crate::kinds::{self, BlobManifest};
use crate::object_refs::ObjectKey;
use crate::redaction::{is_removable, residue_matches};
use crate::redaction_io::{human_approval, request_agrees, residue_fits};
use crate::store::encode;
use crate::stream_error::{Reason, StreamError};
use crate::sync_stream::StreamRecord;
use rdy_model::redaction::{CARRIER_MESSAGE, Redaction, RedactionApply, RedactionResidue};
use rdy_model::{BlockId, Change, Hash, LandingRecord, Proposal};
use serde::de::DeserializeOwned;
use std::collections::btree_map::Entry;
use std::collections::hash_map::Entry as HashEntry;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Where the proof check reads records: a source store, a batch union, or both.
pub trait ObjectSource {
    /// The record of `key`: its original bytes (bounded as a stream record is), or its dropped
    /// identity; None when the source does not hold it.
    fn record(&mut self, key: ObjectKey) -> Result<Option<StreamRecord>, StreamError>;
}

/// Records by key, as a batch reader returned them.
impl ObjectSource for BTreeMap<ObjectKey, StreamRecord> {
    fn record(&mut self, key: ObjectKey) -> Result<Option<StreamRecord>, StreamError> {
        Ok(self.get(&key).cloned())
    }
}

/// One object an approved removal drops: its original payload length and the residue that
/// replaces it for reads (none for a blob chunk).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemovalObject {
    pub len: u32,
    pub residue: Option<Hash>,
}

/// What one checked apply authorizes, by logical key: the objects of its request's manifest
/// spans, each once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortableRemoval {
    pub request: Hash,
    pub apply: Hash,
    pub landing: Hash,
    pub objects: BTreeMap<ObjectKey, RemovalObject>,
}

fn key(kind: u8, hash: Hash) -> ObjectKey {
    ObjectKey { kind, hash }
}

/// The refusal of a proof that fails at `object`.
fn invalid(object: ObjectKey) -> StreamError {
    StreamError::bad_record(0, 0, Reason::InvalidRemovalProof).with_object(Some(object))
}

/// The present body of `k` (its payload, less a versioned record's version, `crate::codec`): a
/// missing record is E_UNRESOLVED_REF; a dropped one, or one whose bytes are not its hash's, is
/// no proof.
fn payload(source: &mut dyn ObjectSource, k: ObjectKey) -> Result<Vec<u8>, StreamError> {
    versioned(source, k).map(|(body, _)| body)
}

/// The body of the record of `k` and the encoding its frame records.
fn versioned(source: &mut dyn ObjectSource, k: ObjectKey) -> Result<(Vec<u8>, u16), StreamError> {
    match source.record(k)? {
        None => Err(StreamError::unresolved(0, 0, k)),
        Some(r @ StreamRecord::Present { .. }) => {
            let StreamRecord::Present { payload, .. } = &r else {
                unreachable!("a present record")
            };
            if Hash::of(payload) != k.hash {
                return Err(invalid(k));
            }
            crate::codec::split(r.raw_kind(), payload)
                .map(|o| (o.body.to_vec(), o.version))
                .map_err(|e| StreamError::from_record(0, 0, &e).with_object(Some(k)))
        }
        Some(_) => Err(invalid(k)),
    }
}

/// The record of `k`, decoded within its bounds.
fn typed<T: DeserializeOwned>(
    source: &mut dyn ObjectSource,
    k: ObjectKey,
) -> Result<T, StreamError> {
    let p = payload(source, k)?;
    bounded::decode(&p)
        .map_err(|e: RecordError| StreamError::from_record(0, 0, &e).with_object(Some(k)))
}

/// Checks the removal that apply `apply` records against the records `source` holds, and returns
/// the logical objects it authorizes. A failure is E_BAD_RECORD `invalid removal proof` naming
/// the record that fails, E_UNRESOLVED_REF for a proof record the source does not hold, or the
/// bound a record breaks.
pub fn check(source: &mut dyn ObjectSource, apply: Hash) -> Result<PortableRemoval, StreamError> {
    let apply_key = key(kinds::REDACTION_APPLY, apply);
    let apply_bytes = payload(source, apply_key)?;
    let record: RedactionApply = bounded::decode(&apply_bytes)
        .map_err(|e| StreamError::from_record(0, 0, &e).with_object(Some(apply_key)))?;
    if encode(&record) != apply_bytes {
        return Err(invalid(apply_key));
    }
    // the human landing
    let landing_key = key(kinds::LANDING, record.landing);
    let landing: LandingRecord = typed(source, landing_key)?;
    if !human_approval(&landing) {
        return Err(invalid(landing_key));
    }
    // the approval names the request: the landing's `checks` holds it, and each other entry is
    // a REDACTION record the source holds (a removal landing names its requests, D-CC90E; the
    // request itself is checked below). A removal landing that names none, made before this
    // rule, is refused.
    if !landing.checks.contains(&record.request) {
        return Err(invalid(landing_key));
    }
    // each distinct entry is read once, so a repeated entry costs nothing more
    let others: BTreeSet<Hash> = landing
        .checks
        .iter()
        .filter(|c| **c != record.request)
        .copied()
        .collect();
    for c in others {
        let named = key(kinds::REDACTION, c);
        match typed::<Redaction>(source, named) {
            Ok(_) => {}
            Err(e) if e.code == "E_LIMIT" || e.code == "E_IO" => return Err(e),
            Err(_) => return Err(invalid(landing_key)),
        }
    }
    // the request, its carrier and the approved proposal
    let request_key = key(kinds::REDACTION, record.request);
    let request: Redaction = typed(source, request_key)?;
    let carrier_key = key(kinds::CHANGE, request.carrier);
    let carrier: Change = typed(source, carrier_key)?;
    if !carrier.ops.is_empty() || carrier.message != CARRIER_MESSAGE {
        return Err(invalid(carrier_key));
    }
    let approved_key = key(
        kinds::PROPOSAL,
        landing.approved_version.unwrap_or(landing.proposal),
    );
    let approved: Proposal = typed(source, approved_key)?;
    if !approved.changes.contains(&request.carrier) {
        return Err(invalid(approved_key));
    }
    let mut buf = None;
    match request_agrees(&request, &landing, &record, &mut buf) {
        Ok(true) => {}
        Ok(false) => return Err(invalid(request_key)),
        Err(_) => return Err(StreamError::allocation(None, 0).with_object(Some(request_key))),
    }
    drop(buf);
    // the logical tuples of the spans
    let m = &request.manifest;
    let versions: BTreeSet<(BlockId, Hash)> = m.versions.iter().cloned().collect();
    let version_hashes: HashSet<Hash> = versions.iter().map(|(_, v)| *v).collect();
    let mut chunks: HashSet<Hash> = HashSet::new();
    for b in &m.blobs {
        let manifest: BlobManifest = typed(source, key(kinds::BLOB_MANIFEST, *b))?;
        chunks.extend(manifest.chunks);
    }
    let mut residues: HashMap<Hash, (RedactionResidue, u16)> = HashMap::new();
    let mut objects: BTreeMap<ObjectKey, RemovalObject> = BTreeMap::new();
    for span in &m.spans {
        let k = key(span.kind, span.hash);
        if !is_removable(span.kind)
            || check_dropped(span.kind, span.residue.as_ref(), span.len).is_err()
        {
            return Err(invalid(k));
        }
        match span.residue {
            Some(r) => {
                if let HashEntry::Vacant(v) = residues.entry(r) {
                    let k = key(kinds::REDACTION_RESIDUE, r);
                    let (body, encoding) = versioned(source, k)?;
                    let res = bounded::decode(&body).map_err(|e: RecordError| {
                        StreamError::from_record(0, 0, &e).with_object(Some(k))
                    })?;
                    v.insert((res, encoding));
                }
                if !residue_fits(
                    span.kind,
                    &span.hash,
                    &residues[&r].0,
                    &versions,
                    &version_hashes,
                ) {
                    return Err(invalid(k));
                }
            }
            // a chunk: of a selected blob
            None => {
                if !chunks.contains(&span.hash) {
                    return Err(invalid(k));
                }
            }
        }
        let tuple = RemovalObject {
            len: span.len,
            residue: span.residue,
        };
        match objects.entry(k) {
            Entry::Vacant(v) => {
                v.insert(tuple);
            }
            Entry::Occupied(o) if *o.get() == tuple => {}
            Entry::Occupied(_) => return Err(invalid(k)),
        }
    }
    // every payload the source still holds is what its tuple says
    for (k, tuple) in &objects {
        match source.record(*k)? {
            None => {}
            Some(StreamRecord::Present { payload, .. }) => {
                let fits = Hash::of(&payload) == k.hash
                    && payload.len() == tuple.len as usize
                    && match tuple.residue {
                        Some(r) => {
                            let (res, encoding) = &residues[&r];
                            residue_matches(k.kind, &payload, res, *encoding)
                        }
                        None => true,
                    };
                if !fits {
                    return Err(invalid(*k));
                }
            }
            Some(StreamRecord::Dropped {
                original_len,
                residue,
                ..
            }) if original_len != tuple.len || residue != tuple.residue => {
                return Err(invalid(*k));
            }
            Some(StreamRecord::Dropped { .. }) => {}
        }
    }
    Ok(PortableRemoval {
        request: record.request,
        apply,
        landing: record.landing,
        objects,
    })
}
