//! The take of an upload into a holder (D-D2NPN: "verify-on-receive"; D-BPYN8: closure and tag in
//! one batch; D-3M81W and D-CC90E: a removal keeps its hashes and needs its approval; hub
//! p-97JVE, f-NE0Z4). `Store::take_batches` reads every batch file of one upload, checks the
//! union of their records against the holder, and writes what the holder lacks, the approved
//! drops and the tag in one sealed batch.
//!
//! # What the take checks before anything is written
//!
//! 1. Each file alone (`sync_stream::check_batch`): framing, hashes, bounded typed records and
//!    their kinds' record rules (an ANSWER's relay and evidence rules), trailer and end of file,
//!    the order inside the batch and its exact prerequisites. Every header names the same
//!    format, repo, purpose and root; the repo is the holder's. The upload-wide bytes, records,
//!    edges and pending drops are counted over every file, repeats included.
//! 2. The union of the files' records over the holder: every prerequisite of every file is a
//!    record of the union or of the holder (the smallest missing key is E_UNRESOLVED_REF).
//!    Equal records of several files are one; a key's dropped representations must agree.
//! 3. The root TAG names exactly the upload target: its name byte for byte and its landing.
//! 4. Each SIGNATURE meets SIG's rule (`check_new_signatures`): its subject is a landing or a
//!    tag of the union or of the holder.
//! 5. Every record of the union is in the root's closure: reached from the root by forward links
//!    (through the holder's records too), or attached as the closure attaches it
//!    (`crate::closure`): answers of reached landings and tags, signatures of reached landings
//!    and of the root, the root's relay answer (O11: act `tag`, item `tag:<root name>`, and
//!    targets exactly the root TAG), requests of reached carriers, and applies whose request
//!    names a reached object. Each new PLAN keeps its chain, bound proposal and seal, and each
//!    new ANSWER its typed targets, as the holder's fsck checks them, against the union and the
//!    holder (`stream_source::check_typed_context`): a second seal of one landing, in the upload
//!    or against the holder's seal, is `duplicate landing seal`. A hash the holder holds already
//!    is not checked again.
//! 6. Each new apply passes its portable proof (`sync_proof::check`, over the union and the
//!    holder). Each dropped record names a validated apply that approves its exact tuple, and
//!    each key the upload approves or drops has one tuple over every approval of the holder and
//!    of the upload, whether the holder holds it or not (an object it removed already keeps
//!    its identity). A present record that an approval covers is written as a drop (its bytes
//!    checked against the tuple first); a covered key the holder holds intact is a local copy
//!    to erase, each of its frames found by the holder's own log headers, never by a source
//!    place.
//! 7. An intact CHANGE that embeds an approved block version and that no approval covers, in the
//!    holder or in the union, is `UncoveredChange`.
//!    Then the holder's capacity (TASK-2340, `check_capacity`): the take refuses a batch after
//!    which the holder would hold more than the holder check reads (more objects or object
//!    links than its bounds, over the whole holder, from the index's derived counts, or a new
//!    landing deeper than its chain bound), E_LIMIT with the holder check's own text. It runs
//!    whether the closure walk of step 5 ended early or not.
//! 8. The caller's check (`check`) runs once, with the union and the holder as its source; its
//!    choice selects which tag survives. With `Incoming`, the take writes `tag/<name>`. Either
//!    way the root TAG is stored, and for every TAG object of the union whose relay answer the
//!    union holds (one that targets exactly that TAG; `crate::holder::is_relay_answer_of`), the
//!    root or any other tag the closure reaches, the take writes `answer/tag-object:<tag hash>`
//!    (O11, ruling task-13): at most one relay answer per tag object, and never another one than
//!    the holder holds for it. The check gets these answers before it runs
//!    (`UploadFacts::relay_answers`), so it judges each signature of a relayed tag against the
//!    answer the holder will name.
//! 9. A holder with an approval keeps within its receiver caps (`crate::redaction_import`),
//!    the frames, payload bytes and spans of the batch included; the marker counts only when
//!    there is a frame to write (a complete retry writes none).
//!
//! Steps 2, 3 (the root), 5 (with the typed context) and 6 (the portable proofs, the dropped
//! records and the one tuple of each key) and the relay answers of step 8 are `crate::stream_source`'s checks, which a checked
//! stream runs with nothing held; the take runs them over the union and the holder. The
//! receiver's own steps stay here: the repo, the upload target, SIG's rule over the holder, the
//! scan of the holder's local copies, the receiver caps, the tag choice and the append.
//!
//! The take keeps one record payload buffer at a time: it indexes each record's file, number,
//! offset and frame, and reads a record again when a check or the append needs it. Each re-read
//! checks the frame's kind, length and hash (and a drop's descriptor), so input that changed
//! after the checks is refused (`input changed during take`) and its unacknowledged bytes are
//! rolled back. No private spool is needed: the append's re-read closes the race.
use crate::closure::{END_OF_LOG, StoreSource, dependency_order, store_error};
use crate::dump::{ENCODED_DROPPED_LEN, Frame, encoded_dropped_payload, parse_encoded_dropped};
use crate::error::StoreError;
use crate::format::{RdyVersion, StoreFeature, StoreFeatures};
use crate::kinds;
use crate::object_refs::{ObjectKey, ObjectRef, portable_refs_of};
use crate::redaction::residue_matches;
use crate::redaction_import::{Imported, ReceiverCaps, embeds};
use crate::redaction_io::{Authorization, for_each_seen};
use crate::segment::{HEADER, Location, MARKER_MAX};
use crate::store::StoreReader;
use crate::stream_error::{
    LIMIT_GRAPH_EDGES, LIMIT_OBJECT_RECORDS, LIMIT_PENDING_DROPPED_BYTES, LIMIT_PENDING_DROPS,
    LIMIT_TOTAL_BYTES, Reason, StreamError,
};
use crate::stream_source::{self, Drop, Records, bad};
use crate::sync_proof::{ObjectSource, PortableRemoval, RemovalObject};
use crate::sync_stream::{
    LIMIT_BATCH_FILES, MAX_BATCH_FILES, MAX_EDGES, MAX_PENDING_DROPPED_BYTES, MAX_PENDING_DROPS,
    MAX_RECORDS, MAX_STREAM_BYTES, StreamHeader, StreamReader, StreamRecord, check_batch, rec,
};
use rdy_model::redaction::RedactionResidue;
use rdy_model::signature::Signature;
use rdy_model::{Change, Hash, RepoId, Tag};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::fs::File;
use std::io::{Seek, SeekFrom};

/// The most bytes of an upload target's tag name (inclusive).
pub const MAX_UPLOAD_TAG_BYTES: usize = 1_048_576;

/// The publication an upload is for: the root tag's exact name and its landing. A strict input:
/// an unknown key is refused when it is deserialized; the landing is full lowercase hex.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadTarget {
    pub tag: String,
    #[serde(serialize_with = "hex_hash", deserialize_with = "parse_hash")]
    pub landing: Hash,
}

fn hex_hash<S: Serializer>(h: &Hash, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(h)
}

fn parse_hash<'de, D: Deserializer<'de>>(d: D) -> Result<Hash, D::Error> {
    let s = String::deserialize(d)?;
    s.parse().map_err(serde::de::Error::custom)
}

/// What the take validated before it asks the caller: the stream's repo, the root TAG's hash
/// and record, every key of the union, sorted, and the relay answer of each TAG object (of the
/// union or the holder) that has one in the union (tag hash to answer hash): the answer the take
/// names by `answer/tag-object:<tag>` (O11, ruling task-13). A tag with no relay answer in the
/// union keeps the one the holder names for it, if any. Temporary metadata, never stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadFacts {
    pub repo_id: RepoId,
    pub root_hash: Hash,
    pub root: Tag,
    pub objects: Vec<ObjectKey>,
    pub relay_answers: BTreeMap<Hash, Hash>,
    /// The features the upload's headers declare: its records' links follow them.
    pub features: StoreFeatures,
}

/// What a project fetch takes (D-3RSCW): a canon or view history from remote `remote` at `url`,
/// rooted at `root` (a landing, or a view op of view `view`). The fetch moves no canon or view
/// ref: it writes the records, the free tag refs, and the remote's sync state with `root` as
/// its fetched tip or head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchTarget {
    pub remote: String,
    pub url: String,
    pub purpose: crate::sync_stream::StreamPurpose,
    pub root: Hash,
    pub view: Option<String>,
}

/// What a fetch validated before it asks the caller: the stream's repo and purpose, its root,
/// every key of the union, sorted, and the features its headers declare.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchFacts {
    pub repo_id: RepoId,
    pub purpose: crate::sync_stream::StreamPurpose,
    pub root: Hash,
    pub objects: Vec<ObjectKey>,
    pub features: StoreFeatures,
}

/// What a fetch sealed: the distinct keys it wrote that the project did not hold, its data
/// frames (the marker not counted), the log's end after it, the tag names it wrote, the tag
/// names it did not take because the project's tag of that name names another landing, and the
/// store's minimum rdy before and after the batch (D-4XHPH).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchTaken {
    pub new_objects: u64,
    pub frames: u64,
    pub end: Location,
    pub tags_taken: Vec<String>,
    pub tags_kept: Vec<String>,
    pub min_rdy_before: Option<RdyVersion>,
    pub min_rdy_after: Option<RdyVersion>,
}

/// Which immutable tag ref survives the take: the upload's root (`tag/<root.name>`), or a tag
/// the holder holds already for the same landing, by its stored name and hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TagChoice {
    Incoming,
    Existing { name: String, hash: Hash },
}

/// What a take sealed: the distinct keys it wrote that the holder did not hold, its data frames
/// (the marker not counted), the objects it removed (written as drops, or local copies its
/// approvals erase), the log's end after it, and the tag that survives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchTaken {
    pub new_objects: u64,
    pub frames: u64,
    pub redacted_objects: u64,
    pub end: Location,
    pub tag_hash: Hash,
    pub tag: Tag,
    /// The holder's minimum rdy before and after the batch (D-4XHPH).
    pub min_rdy_before: Option<RdyVersion>,
    pub min_rdy_after: Option<RdyVersion>,
}

/// Why a take refused or failed. `Validation` is the caller's check's own error; the other
/// variants are the store's. The engine supplies the public texts of the structured ones.
#[derive(Debug)]
pub enum TakeError<E> {
    Store(StoreError),
    Stream(StreamError),
    Validation(E),
    /// An intact receiver change embeds an approved removed block version, and no approval of
    /// the upload or of the holder covers it.
    UncoveredChange(Hash),
    /// The store is a project, not a holder (an upload), or a holder, not a project (a fetch).
    ModeMismatch,
    /// The stream's repo is not the holder's.
    RepoMismatch {
        source: RepoId,
        target: RepoId,
    },
}

impl<E: fmt::Display> fmt::Display for TakeError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TakeError::Store(e) => write!(f, "{e}"),
            TakeError::Stream(e) => write!(f, "{e}"),
            TakeError::Validation(e) => write!(f, "{e}"),
            TakeError::UncoveredChange(h) => {
                write!(f, "the imported removal does not cover receiver change {h}")
            }
            TakeError::ModeMismatch => write!(f, "this store is a project"),
            TakeError::RepoMismatch { source, target } => {
                write!(
                    f,
                    "stream repo {source} does not match holder repo {target}"
                )
            }
        }
    }
}

impl<E> From<StoreError> for TakeError<E> {
    fn from(e: StoreError) -> Self {
        TakeError::Store(e)
    }
}

impl<E> From<std::io::Error> for TakeError<E> {
    fn from(e: std::io::Error) -> Self {
        TakeError::Store(e.into())
    }
}

impl<E> From<StreamError> for TakeError<E> {
    fn from(e: StreamError) -> Self {
        TakeError::Stream(e)
    }
}

/// The bounds of a take's inputs, checked before anything is allocated or any handle cloned: 1
/// to 1,024 batch files, and a target tag name of at most 1,048,576 bytes. Zero files is
/// E_BAD_RECORD `no batch files`; more is E_LIMIT `stream exceeds 1024 batch files`; a longer
/// tag is E_LIMIT `upload tag exceeds 1048576 bytes`.
pub fn check_input_bounds(file_count: usize, target: &UploadTarget) -> Result<(), StreamError> {
    if file_count == 0 {
        return Err(StreamError::bad_record(0, 0, Reason::NoBatchFiles));
    }
    if file_count > MAX_BATCH_FILES {
        return Err(StreamError::limit(None, 0, LIMIT_BATCH_FILES));
    }
    if target.tag.len() > MAX_UPLOAD_TAG_BYTES {
        return Err(StreamError {
            code: "E_LIMIT",
            message: format!("upload tag exceeds {MAX_UPLOAD_TAG_BYTES} bytes"),
            fix: "reduce the tag name to the stated limit".into(),
            record: None,
            offset: 0,
            object: None,
        });
    }
    Ok(())
}

/// The refusal of a fetched stream whose purpose or root is not the fetch target's.
fn fetch_mismatch(root: ObjectKey) -> StreamError {
    StreamError {
        code: "E_BAD_RECORD",
        message: "fetch target does not match the batch root".into(),
        fix: "fetch the history the remote named".into(),
        record: Some(0),
        offset: 0,
        object: Some(root),
    }
}

/// The refusal of a fetched stream that holds a removal: the removal intake takes it.
pub const FETCH_REMOVAL: &str = "a fetched history that holds a removal is not taken yet";

/// The refusal of a root TAG that does not name the upload target.
fn target_mismatch(root: ObjectKey) -> StreamError {
    StreamError {
        code: "E_BAD_RECORD",
        message: "upload target does not match the batch root tag".into(),
        fix: "upload the history for the publication tag and landing".into(),
        record: Some(0),
        offset: 0,
        object: Some(root),
    }
}

/// The placeholder of the union whose proof is `proof`, when `proof` is an apply key whose hash
/// names a carried record (kind REDACTION_CARRIED) the holder holds: such a record is never a
/// removal proof.
fn names_carried(
    rx: &Receiver,
    union: &BTreeMap<ObjectKey, Entry>,
    proof: &ObjectKey,
) -> Result<Option<ObjectKey>, StreamError> {
    if proof.kind != kinds::REDACTION_APPLY {
        return Ok(None);
    }
    let carried = ObjectKey {
        kind: kinds::REDACTION_CARRIED,
        hash: proof.hash,
    };
    if !rx.holds(&carried)? {
        return Ok(None);
    }
    Ok(union
        .iter()
        .find(|(_, e)| e.dropped.iter().any(|(_, d)| d.apply == proof.hash))
        .map(|(k, _)| *k))
}

/// Where a record of a batch file is, and the frame it must still be there: its file, its
/// record number and frame offset, and its frame's kind, payload length and hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct At {
    file: usize,
    record: u64,
    offset: u64,
    /// The record's raw kind on the wire, as its source stored it.
    pub(crate) kind: u8,
    len: u32,
    hash: Hash,
}

/// One key of the union: its present record, if any file holds one, each distinct dropped
/// representation, and the links of its present record.
#[derive(Default)]
struct Entry {
    present: Option<At>,
    refs: Vec<ObjectRef>,
    dropped: Vec<(At, Drop)>,
    /// The features its present record uses (`format::required_features`).
    needs: Vec<StoreFeature>,
}

/// How a planned write is checked again at the append.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Verify {
    /// A present record of a file.
    Present(At),
    /// A dropped record of a file, with its descriptor: its key, the removed frame's raw kind,
    /// its original length, its apply and its residue.
    Dropped(At, ObjectKey, u8, u32, Hash, Option<Hash>),
}

/// One frame the take writes, in canonical order.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Write {
    /// The original bytes of a present record.
    Present { key: ObjectKey, at: At },
    /// The original header and zeros of an approved drop: the removed frame's raw kind (its
    /// residue's source encoding, `crate::codec`), never the logical kind of its key.
    Dropped {
        key: ObjectKey,
        raw: u8,
        len: u32,
        verify: Verify,
    },
    /// A snapshot delta the take made for a new legacy landing of an upload into a holder that
    /// holds `snapshot_delta` (`crate::snapshot_upgrade`): its raw kind and length; its bytes
    /// are the plan's (`Plan::generated`).
    Generated { key: ObjectKey, raw: u8, len: u32 },
}

impl Write {
    /// The object the frame writes.
    pub(crate) fn key(&self) -> ObjectKey {
        match self {
            Write::Present { key, .. }
            | Write::Dropped { key, .. }
            | Write::Generated { key, .. } => *key,
        }
    }

    /// The frame's payload bytes in the log.
    pub(crate) fn len(&self) -> u32 {
        match self {
            Write::Present { at, .. } => at.len,
            Write::Dropped { len, .. } | Write::Generated { len, .. } => *len,
        }
    }
}

/// What the store writes once every check passed.
pub(crate) struct Plan {
    /// The new records, in canonical order.
    pub(crate) writes: Vec<Write>,
    /// The refs to write (`tag/<name>`, `answer/tag-object:<hash>`).
    pub(crate) refs: Vec<(String, Vec<u8>)>,
    /// The batch changes the holder's authorization: it writes a redaction record or a drop.
    pub(crate) redaction: bool,
    /// The batch holds a new apply (the redaction epoch advances).
    pub(crate) new_applies: bool,
    pub(crate) redacted_objects: u64,
    pub(crate) tag_hash: Hash,
    /// The root TAG of an upload; None for a fetch.
    pub(crate) tag: Option<Tag>,
    /// The tag names a fetch writes and those it does not take.
    pub(crate) tags_taken: Vec<String>,
    pub(crate) tags_kept: Vec<String>,
    /// The store's minimum rdy before and after the batch (D-4XHPH).
    pub(crate) min_rdy: (Option<RdyVersion>, Option<RdyVersion>),
    /// The holder's merged features when this batch activates any (its last ref sets them).
    pub(crate) features: Option<StoreFeatures>,
    /// The bytes of each [`Write::Generated`] delta, by its key.
    pub(crate) generated: BTreeMap<ObjectKey, Vec<u8>>,
}

/// The holder as the take reads it: its reader, its log's segment files and authorization, the
/// log's end, and its repo id.
pub(crate) struct Receiver<'a> {
    pub(crate) reader: &'a StoreReader,
    pub(crate) log: &'a [File],
    pub(crate) auth: &'a Authorization,
    pub(crate) end: Location,
    pub(crate) repo: RepoId,
    /// The holder's features: its log's `meta/store_features`.
    pub(crate) features: &'a StoreFeatures,
}

impl Receiver<'_> {
    fn holds(&self, k: &ObjectKey) -> Result<bool, StreamError> {
        self.reader
            .has_kind(k.kind, &k.hash)
            .map_err(|e| store_error(e, Some(*k)))
    }
}

/// Reads the frame `at` names again into `buf`: its header must still be the frame indexed and
/// its payload must match its hash. Anything else (a change, a cut, a read failure) is
/// `input changed during take` at that record.
pub(crate) fn reread(files: &[File], at: &At, buf: &mut Vec<u8>) -> Result<(), StreamError> {
    let changed = || StreamError::bad_record(at.record, at.offset, Reason::InputChangedDuringTake);
    let f = &files[at.file];
    let mut head = [0u8; HEADER as usize];
    crate::pio::read_exact_at(f, &mut head, at.offset).map_err(|_| changed())?;
    let len = u32::from_le_bytes(head[0..4].try_into().expect("4 bytes"));
    if len != at.len || head[4] != at.kind || head[5..37] != at.hash.0 {
        return Err(changed());
    }
    buf.clear();
    buf.try_reserve_exact(len as usize)
        .map_err(|_| StreamError::allocation(Some(at.record), at.offset))?;
    buf.resize(len as usize, 0);
    crate::pio::read_exact_at(f, buf, at.offset + HEADER).map_err(|_| changed())?;
    if Hash::of(buf) != at.hash {
        return Err(changed());
    }
    Ok(())
}

/// Checks a planned write's input again (see [`reread`]); a dropped record's descriptor must
/// still be the one indexed.
pub(crate) fn verify(files: &[File], v: &Verify, buf: &mut Vec<u8>) -> Result<(), StreamError> {
    match v {
        Verify::Present(at) => reread(files, at, buf),
        Verify::Dropped(at, key, raw, len, apply, residue) => {
            reread(files, at, buf)?;
            match parse_encoded_dropped(buf) {
                Ok(Frame::Dropped {
                    kind,
                    hash,
                    len: l,
                    apply: a,
                    residue: r,
                }) if (kind, hash, l, a, r) == (*raw, key.hash, *len, *apply, *residue)
                    && crate::codec::logical_kind(kind) == key.kind =>
                {
                    Ok(())
                }
                _ => Err(StreamError::bad_record(
                    at.record,
                    at.offset,
                    Reason::InputChangedDuringTake,
                )),
            }
        }
    }
}

/// The union and the holder as one record source: a key of the union is read again from its
/// file, any other from the holder. Keys in `masked` read as their dropped identity (an approved
/// removal's tuple), whatever bytes a file or the holder still has.
struct UploadSource<'a> {
    files: &'a [File],
    union: &'a BTreeMap<ObjectKey, Entry>,
    store: StoreSource<'a>,
    masked: BTreeMap<ObjectKey, (Hash, RemovalObject, u16)>,
    /// Each landing's delta among the union's records, built once from their typed deltas
    /// (`crate::snapshot`); a landing the union has no delta of asks the holder.
    deltas: Option<BTreeMap<Hash, Hash>>,
    /// The holder's features: a held record's links follow them.
    features: &'a StoreFeatures,
}

impl ObjectSource for UploadSource<'_> {
    fn record(&mut self, key: ObjectKey) -> Result<Option<StreamRecord>, StreamError> {
        if let Some((apply, t, encoding)) = self.masked.get(&key) {
            return Ok(Some(StreamRecord::Dropped {
                key,
                encoding: *encoding,
                original_len: t.len,
                apply: *apply,
                residue: t.residue,
            }));
        }
        if let Some(e) = self.union.get(&key) {
            if let Some(at) = &e.present {
                let mut payload = Vec::new();
                reread(self.files, at, &mut payload).map_err(|e| e.with_object(Some(key)))?;
                let encoding = crate::codec::split(at.kind, &payload)
                    .map_err(|e| StreamError::from_record(at.record, at.offset, &e))?
                    .version;
                return Ok(Some(StreamRecord::Present {
                    key,
                    encoding,
                    payload,
                }));
            }
            if let Some((_, d)) = e.dropped.first() {
                return Ok(Some(StreamRecord::Dropped {
                    key,
                    encoding: d.encoding,
                    original_len: d.original_len,
                    apply: d.apply,
                    residue: d.residue,
                }));
            }
        }
        self.store.record(key)
    }

    fn snapshot_delta_of(&mut self, landing: &Hash) -> Result<Option<Hash>, StreamError> {
        if self.deltas.is_none() {
            let keys: Vec<ObjectKey> = self
                .union
                .keys()
                .filter(|k| k.kind == kinds::SNAPSHOT_DELTA)
                .copied()
                .collect();
            let by =
                crate::snapshot::associate_deltas(&keys, |k| crate::stream_source::typed(self, k))?;
            self.deltas = Some(by);
        }
        match self.deltas.as_ref().and_then(|by| by.get(landing)) {
            Some(h) => Ok(Some(*h)),
            None => self.store.snapshot_delta_of(landing),
        }
    }
}

/// The take's lookups for the shared checks (`crate::stream_source`): a union key from the
/// union's index (its links as indexed, with no read), any other from the holder.
impl Records for UploadSource<'_> {
    fn object_source(&mut self) -> &mut dyn ObjectSource {
        self
    }

    fn has(&mut self, k: &ObjectKey) -> Result<bool, StreamError> {
        Ok(self.union.contains_key(k)
            || self
                .store
                .reader
                .has_kind(k.kind, &k.hash)
                .map_err(|e| store_error(e, Some(*k)))?)
    }

    fn position(&self, k: &ObjectKey) -> Option<(u64, u64)> {
        let at = self.union.get(k)?.present?;
        Some((at.record, at.offset))
    }

    /// A union key's links are indexed; any other key's are read from the holder.
    fn in_memory(&self, k: &ObjectKey) -> bool {
        self.union.contains_key(k)
    }

    fn links(&mut self, k: ObjectKey) -> Result<Option<Vec<ObjectRef>>, StreamError> {
        if let Some(e) = self.union.get(&k) {
            let mut links = e.refs.clone();
            for (_, d) in &e.dropped {
                links.extend(d.residue.map(|r| ObjectRef {
                    kind: Some(kinds::REDACTION_RESIDUE),
                    hash: r,
                }));
                links.push(ObjectRef {
                    kind: Some(kinds::REDACTION_APPLY),
                    hash: d.apply,
                });
            }
            return Ok(Some(links));
        }
        match self.store.record(k)? {
            Some(record) => stream_source::record_links(k, &record, self.features).map(Some),
            None => Ok(None),
        }
    }

    fn held_delta_of(&mut self, landing: &Hash) -> Result<Option<Hash>, StreamError> {
        self.store.snapshot_delta_of(landing)
    }
}

impl UploadSource<'_> {
    /// The present bytes of union key `key`.
    /// The union's present record of `key`, read again from its file, with the encoding its
    /// raw frame kind gives (`crate::codec`).
    fn union_record(&self, key: &ObjectKey) -> Result<Option<StreamRecord>, StreamError> {
        match self.union.get(key).and_then(|e| e.present.as_ref()) {
            Some(at) => {
                let mut payload = Vec::new();
                reread(self.files, at, &mut payload).map_err(|e| e.with_object(Some(*key)))?;
                let encoding = crate::codec::split(at.kind, &payload)
                    .map_err(|e| StreamError::from_record(at.record, at.offset, &e))?
                    .version;
                Ok(Some(StreamRecord::Present {
                    key: *key,
                    encoding,
                    payload,
                }))
            }
            None => Ok(None),
        }
    }
}

/// The upload-wide counts over every file, repeats included.
#[derive(Default)]
struct Counts {
    bytes: u64,
    records: u64,
    edges: u64,
    drops: u64,
    dropped_bytes: u64,
}

/// Why indexing the files stopped: a stream refusal, or the first header's repo, which is not
/// the holder's.
enum IndexError {
    Stream(StreamError),
    Repo(RepoId),
}

impl From<StreamError> for IndexError {
    fn from(e: StreamError) -> Self {
        IndexError::Stream(e)
    }
}

/// Checks every file and indexes the union: its records by key, and every file's
/// prerequisites. Returns the first header.
fn index_files(
    rx: &Receiver,
    files: &[File],
    union: &mut BTreeMap<ObjectKey, Entry>,
    prerequisites: &mut BTreeSet<ObjectKey>,
) -> Result<StreamHeader, IndexError> {
    let mut counts = Counts::default();
    let mut first: Option<StreamHeader> = None;
    for (i, f) in files.iter().enumerate() {
        let io = |e: std::io::Error| StreamError::io(Some(0), 0, &e);
        let len = f.metadata().map_err(io)?.len();
        counts.bytes = counts.bytes.saturating_add(len);
        if counts.bytes > MAX_STREAM_BYTES {
            return Err(StreamError::limit(None, 0, LIMIT_TOTAL_BYTES).into());
        }
        let mut handle = f;
        handle.seek(SeekFrom::Start(0)).map_err(io)?;
        let report = check_batch(handle)?;
        match &first {
            None => {
                if report.header.repo_id != rx.repo {
                    return Err(IndexError::Repo(report.header.repo_id));
                }
                first = Some(report.header.clone());
            }
            Some(h) => {
                if (h.repo_id, h.purpose, h.tag, &h.features)
                    != (
                        report.header.repo_id,
                        report.header.purpose,
                        report.header.tag,
                        &report.header.features,
                    )
                {
                    return Err(StreamError::bad_record(
                        0,
                        crate::sync_stream::MAGIC.len() as u64 + 4,
                        Reason::BatchHeaderDiffers,
                    )
                    .into());
                }
            }
        }
        counts.records += report.records;
        if counts.records > MAX_RECORDS {
            return Err(StreamError::limit(None, 0, LIMIT_OBJECT_RECORDS).into());
        }
        // the index pass: each record's place and frame, its links and its descriptor
        handle.seek(SeekFrom::Start(0)).map_err(io)?;
        let (mut reader, header) = StreamReader::open(handle)?;
        while let Some(r) = reader.next()? {
            let (n, offset) = reader.position();
            let key = r.key();
            let proof = usize::from(r.proof().is_some());
            let entry = match union.get_mut(&key) {
                Some(e) => e,
                None => union.entry(key).or_default(),
            };
            match r {
                StreamRecord::Present { ref payload, .. } => {
                    let raw = r.raw_kind();
                    // an incoming record's links follow the features its stream declares
                    let refs = portable_refs_of(&header.features, raw, payload)
                        .map_err(|e| StreamError::from_record(n, offset, &e))?;
                    // the reader checked the frame splits
                    let needs = crate::codec::split(raw, payload)
                        .map(|_| crate::format::required_features(raw, payload))
                        .map_err(|e| StreamError::from_record(n, offset, &e))?;
                    counts.edges += refs.len() as u64;
                    if entry.present.is_none() {
                        entry.present = Some(At {
                            file: i,
                            record: n,
                            offset,
                            kind: raw,
                            len: payload.len() as u32,
                            hash: key.hash,
                        });
                        entry.refs = refs;
                        entry.needs = needs;
                    }
                }
                StreamRecord::Dropped {
                    encoding,
                    original_len,
                    apply,
                    residue,
                    ..
                } => {
                    counts.edges += u64::from(residue.is_some()) + proof as u64;
                    counts.drops += 1;
                    counts.dropped_bytes += u64::from(original_len);
                    if counts.drops > MAX_PENDING_DROPS {
                        return Err(StreamError::limit(Some(n), offset, LIMIT_PENDING_DROPS).into());
                    }
                    if counts.dropped_bytes > MAX_PENDING_DROPPED_BYTES {
                        return Err(StreamError::limit(
                            Some(n),
                            offset,
                            LIMIT_PENDING_DROPPED_BYTES,
                        )
                        .into());
                    }
                    let body = encoded_dropped_payload(
                        key.kind,
                        encoding,
                        &key.hash,
                        original_len,
                        &apply,
                        residue.as_ref(),
                    );
                    let d = Drop {
                        encoding,
                        original_len,
                        apply,
                        residue,
                    };
                    if !entry.dropped.iter().any(|(_, x)| *x == d) {
                        entry.dropped.push((
                            At {
                                file: i,
                                record: n,
                                offset,
                                kind: rec::DROPPED,
                                len: ENCODED_DROPPED_LEN as u32,
                                hash: Hash::of(&body),
                            },
                            d,
                        ));
                    }
                }
            }
            if counts.edges > MAX_EDGES {
                return Err(StreamError::limit(Some(n), offset, LIMIT_GRAPH_EDGES).into());
            }
        }
        reader.finish()?;
        prerequisites.extend(header.prerequisites.iter().copied());
    }
    Ok(first.expect("at least one file"))
}

/// The residue `r`, from the union's present bytes or the holder's, and the encoding its frame
/// records.
fn residue(source: &mut UploadSource, r: Hash) -> Result<(RedactionResidue, u16), StreamError> {
    let key = ObjectKey {
        kind: kinds::REDACTION_RESIDUE,
        hash: r,
    };
    let rec = match source.union_record(&key)? {
        Some(rec) => rec,
        None => match source.store.record(key)? {
            Some(rec @ StreamRecord::Present { .. }) => rec,
            _ => return Err(StreamError::unresolved(0, 0, key)),
        },
    };
    let StreamRecord::Present { encoding, .. } = rec else {
        unreachable!("a present record")
    };
    let res = rec
        .typed()
        .expect("a present record")
        .map_err(|e| StreamError::from_record(0, 0, &e).with_object(Some(key)))?;
    Ok((res, encoding))
}

/// The bounds a take keeps its holder within (TASK-2340): the holder check's own
/// (`crate::check_holder`), so a holder that takes a batch can still pass its check.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HolderCapacity {
    /// The deepest landing chain.
    pub landing_depth: u64,
    /// The most distinct objects.
    pub objects: u64,
    /// The most object links.
    pub edges: u64,
}

impl HolderCapacity {
    /// The holder check's bounds.
    pub const DEFAULT: HolderCapacity = HolderCapacity {
        landing_depth: crate::check_holder::MAX_LANDING_DEPTH as u64,
        objects: crate::check_holder::MAX_OBJECTS as u64,
        edges: crate::check_holder::MAX_EDGES as u64,
    };

    fn current() -> HolderCapacity {
        CAPACITY
            .with(|c| c.get())
            .unwrap_or(HolderCapacity::DEFAULT)
    }
}

thread_local! {
    /// Lowered capacity bounds for this thread (test hook); None is the default.
    static CAPACITY: std::cell::Cell<Option<HolderCapacity>> = const { std::cell::Cell::new(None) };
}

/// Test hook: lowers the take's capacity bounds on this thread (None puts the holder check's
/// back), so a test pins N and N+1 without a holder of a million objects. A take on this thread
/// uses them; pair it with `StoreReader::check_holder_bounded_for_tests` at the same bounds.
#[doc(hidden)]
pub fn set_holder_capacity_for_tests(bounds: Option<HolderCapacity>) {
    CAPACITY.with(|c| c.set(bounds));
}

/// The take's capacity guard (TASK-2340; D-BPYN8, p-7QDPC). The holder check reads the whole
/// holder within its bounds, and every upgrade, load and fsck of a holder runs it, so a take
/// refuses a batch after which the check could not: more than [`HolderCapacity::objects`]
/// objects or [`HolderCapacity::edges`] object links over the whole holder (the index's derived
/// counts, `StoreReader::holder_counts`, upper bounds of what the check reads, plus what this
/// batch writes), or a new landing deeper than [`HolderCapacity::landing_depth`] in its chain
/// (its `prev`'s depth, `StoreReader::landing_depth` for a held one, plus one). The edge count
/// follows the holder check's counting under `after`, the manifest the holder holds after the
/// batch (`portable_refs_of` of each present record; under `snapshot_delta` a SNAPSHOT counts
/// no object and no link, and a landing's links omit its `state`: `Index::holder_counts_under`
/// for what is held). It costs index reads and the
/// batch's own records, never a walk of the holder, and it runs whether the closure walk ended
/// early or not. The refusal is E_LIMIT with the holder check's own text.
fn check_capacity(
    reader: &StoreReader,
    source: &mut UploadSource<'_>,
    writes: &BTreeMap<ObjectKey, (Write, Vec<ObjectKey>)>,
    generated: &BTreeMap<ObjectKey, Vec<u8>>,
    after: &StoreFeatures,
) -> Result<(), StreamError> {
    use crate::check_holder::{
        CAPACITY_FIX, depth_limit, edges_limit, exceeds_text, objects_limit,
    };
    let bounds = HolderCapacity::current();
    let refuse = |limit: String| StreamError::capacity(exceeds_text(&limit), CAPACITY_FIX);
    let deltas = after.has(crate::format::FEATURE_SNAPSHOT_DELTA);
    let (held_objects, held_edges) = reader
        .index()
        .holder_counts_under(after)
        .map_err(|e| store_error(e, None))?;
    let (mut objects, mut edges) = (held_objects, held_edges);
    for (key, (w, _)) in writes {
        if !crate::object_refs::is_portable(key.kind) || (deltas && key.kind == kinds::SNAPSHOT) {
            continue;
        }
        objects += 1;
        if let Write::Present { .. } = w
            && key.kind != kinds::BLOB_CHUNK
            && let Some(e) = source.union.get(key)
        {
            // a landing the stream linked to its `state` links it no more under the feature
            edges += e
                .refs
                .iter()
                .filter(|r| !(deltas && r.kind == Some(kinds::SNAPSHOT)))
                .count() as u64;
        }
        // a delta the take made counts its own links under the manifest after the batch, as
        // the index counts them
        if let Write::Generated { raw, .. } = w
            && let Some(payload) = generated.get(key)
        {
            edges += portable_refs_of(after, *raw, payload)
                .map_err(|e| StreamError::from_record(0, 0, &e).with_object(Some(*key)))?
                .len() as u64;
        }
    }
    if objects > bounds.objects {
        return Err(refuse(objects_limit(bounds.objects as usize)));
    }
    if edges > bounds.edges {
        return Err(refuse(edges_limit(bounds.edges as usize)));
    }
    // each new landing's depth: its chain through the batch's new landings down to a held one
    let too_deep = || refuse(depth_limit(bounds.landing_depth as usize));
    let mut depth: std::collections::HashMap<Hash, u64> = std::collections::HashMap::new();
    for key in writes.keys().filter(|k| k.kind == kinds::LANDING) {
        let mut chain: Vec<Hash> = Vec::new();
        let mut cur = Some(key.hash);
        let mut base = 0u64;
        while let Some(h) = cur {
            if let Some(d) = depth.get(&h) {
                base = *d;
                break;
            }
            let k = ObjectKey {
                kind: kinds::LANDING,
                hash: h,
            };
            if !writes.contains_key(&k) {
                base = reader
                    .landing_depth(&h)
                    .map_err(|e| store_error(e, Some(k)))?
                    .unwrap_or(0);
                break;
            }
            chain.push(h);
            if chain.len() as u64 > bounds.landing_depth {
                return Err(too_deep());
            }
            cur = stream_source::typed::<rdy_model::LandingRecord>(source, k)?.and_then(|l| l.prev);
        }
        for (i, h) in chain.iter().rev().enumerate() {
            let d = base + i as u64 + 1;
            if d > bounds.landing_depth {
                return Err(too_deep());
            }
            depth.insert(*h, d);
        }
    }
    Ok(())
}

/// Step 8 of an upload: the relay answers, the caller's check and the surviving tag. `facts`
/// comes without its relay answers, which this finds. Pushes the tag ref (when it is new) and
/// then the answer refs onto `refs`; returns the surviving tag's hash and record.
fn upload_refs<E>(
    reader: &StoreReader,
    source: &mut UploadSource,
    mut facts: UploadFacts,
    check: UploadCheck<'_, E>,
    refs: &mut Vec<(String, Vec<u8>)>,
) -> Result<(Hash, Tag), TakeError<E>> {
    let root_key = ObjectKey {
        kind: kinds::TAG,
        hash: facts.root_hash,
    };
    // the relay answer of every tag object of the union (O11, ruling task-13), each named by
    // its tag object whichever tag stays the version: at most one per tag object, and never
    // another one than the holder holds for it. The caller's check judges signatures against
    // them, so they are found before it runs
    let relayed = stream_source::relay_answers(source, &facts.objects)?;
    let mut answer_refs: Vec<(String, Vec<u8>)> = Vec::new();
    for (tag, answer) in &relayed {
        let name = crate::holder::answer_tag_object_ref(tag);
        match reader.get_ref(&name)? {
            Some(v) if v == answer.0 => {}
            // the tag object holds another relay answer already: one per tag object
            Some(_) => {
                let key = ObjectKey {
                    kind: kinds::ANSWER,
                    hash: *answer,
                };
                return Err(bad(Reason::OutsideTagClosure, key).into());
            }
            None => answer_refs.push((name, answer.0.to_vec())),
        }
    }
    facts.relay_answers = relayed;
    let choice = check(source, &facts).map_err(TakeError::Validation)?;
    // each answer is read again after the check (a reread checks its frame and hash): input
    // changed since then refuses here, before any append, as an answer the holder holds is not
    // read again by the append
    if stream_source::relay_answers(source, &facts.objects)? != facts.relay_answers {
        return Err(StreamError::bad_record(0, 0, Reason::InputChangedDuringTake).into());
    }
    let root = facts.root;
    let surviving = match choice {
        TagChoice::Incoming => {
            let name = format!("{}{}", crate::holder::TAG_REF, root.name);
            match reader.get_ref(&name)? {
                Some(v) if v == root_key.hash.0 => {}
                Some(_) => return Err(bad(Reason::InvalidHeader, root_key).into()),
                None => refs.push((name, root_key.hash.0.to_vec())),
            }
            (root_key.hash, root)
        }
        TagChoice::Existing { name, hash } => {
            let held = reader.get_ref(&format!("{}{name}", crate::holder::TAG_REF))?;
            let tag_key = ObjectKey {
                kind: kinds::TAG,
                hash,
            };
            let stored: Option<Tag> = match held {
                Some(v) if v == hash.0 => match reader.get_kind_raw(kinds::TAG, &hash) {
                    Ok((raw, p)) => crate::codec::decode_bounded(raw, &p).ok(),
                    Err(_) => None,
                },
                _ => None,
            };
            match stored {
                Some(t) if t.name == name && t.landing == root.landing => (hash, t),
                _ => return Err(bad(Reason::InvalidHeader, tag_key).into()),
            }
        }
    };
    refs.extend(answer_refs);
    Ok(surviving)
}

/// The tag refs of a fetch: for each TAG of the union, `tag/<name>` when the project has no tag
/// of that name (taken), nothing when its tag of that name names the same landing, and nothing
/// when it names another landing (kept: the project's own tag stays, and the name is listed).
///
/// A tag that the project names by `tag/<name>` after the fetch (taken, or held already) keeps
/// its relay answer (`relayed`, tag to answer): `answer/tag:<name>` names that answer, as
/// `tag_as` writes it, when the project has no such ref. A project ref that names another
/// answer stays, and so does every ref of a kept name. Project signature checks find a tag's
/// relay answer by that ref (`rdy_engine::signature_check`).
fn fetch_tag_refs(
    reader: &StoreReader,
    source: &mut UploadSource,
    objects: &[ObjectKey],
    relayed: &BTreeMap<Hash, Hash>,
    refs: &mut Vec<(String, Vec<u8>)>,
    taken: &mut Vec<String>,
    kept: &mut Vec<String>,
) -> Result<(), StreamError> {
    let store = |e: StoreError| store_error(e, None);
    for k in objects.iter().filter(|k| k.kind == kinds::TAG) {
        let Some(t) = stream_source::typed::<Tag>(source, *k)? else {
            continue;
        };
        let name = format!("{}{}", crate::holder::TAG_REF, t.name);
        if refs.iter().any(|(n, _)| *n == name) {
            continue;
        }
        let held = match reader.get_ref(&name).map_err(store)? {
            Some(v) => Some(crate::index::hash_from(&v, "tag ref").map_err(store)?),
            None => None,
        };
        // the relay answer of a tag the project names after the fetch, when its ref is free
        let answer_ref = |refs: &mut Vec<(String, Vec<u8>)>| -> Result<(), StreamError> {
            if let Some(a) = relayed.get(&k.hash) {
                let item = crate::answer::answer_ref(&format!("tag:{}", t.name));
                if reader.get_ref(&item).map_err(store)?.is_none()
                    && !refs.iter().any(|(n, _)| *n == item)
                {
                    refs.push((item, a.0.to_vec()));
                }
            }
            Ok(())
        };
        match held {
            None => {
                refs.push((name, k.hash.0.to_vec()));
                answer_ref(refs)?;
                taken.push(t.name);
            }
            Some(h) if h == k.hash => answer_ref(refs)?,
            Some(h) => {
                let same = reader
                    .read_tag(&h)
                    .map(|own| own.landing == t.landing)
                    .unwrap_or(false);
                if !same {
                    kept.push(t.name);
                }
            }
        }
    }
    Ok(())
}

/// The sync state ref a fetch writes (D-3RSCW): the remote's canon state, or the view's, with
/// `root` as its fetched tip or head and the target's URL. A state of the same hub repo keeps
/// its applied tip and held request, whatever URL it named; a state of another repo starts
/// again. A view state's root is the first op of the
/// fetched head's chain. None when the state holds that value already (a retry).
fn fetch_state_ref(
    reader: &StoreReader,
    source: &mut UploadSource,
    target: &FetchTarget,
    repo_id: RepoId,
    root: ObjectKey,
) -> Result<Option<(String, Vec<u8>)>, StreamError> {
    use crate::sync_state::{SyncCanon, SyncValue, SyncView};
    use crate::sync_stream::StreamPurpose;
    let store = |e: StoreError| store_error(e, None);
    // the hub repo decides; a changed URL keeps the state and records the new one
    let same = |r: &RepoId| *r == repo_id;
    let (name, value) = match target.purpose {
        StreamPurpose::CanonHistory => {
            let old = reader.sync_canon(&target.remote).map_err(store)?;
            let old = old.filter(|o| same(&o.repo_id));
            let v = SyncCanon {
                repo_id,
                url: target.url.clone(),
                fetched: Some(root.hash),
                applied: old.as_ref().and_then(|o| o.applied),
                held: old.and_then(|o| o.held),
            };
            (
                crate::sync_state::canon_ref(&target.remote),
                SyncValue::Canon(v),
            )
        }
        StreamPurpose::ViewHistory => {
            let Some(view) = &target.view else {
                return Err(fetch_mismatch(root));
            };
            let old = reader.sync_view(&target.remote, view).map_err(store)?;
            let old = old.filter(|o| same(&o.repo_id));
            // the chain's first op: the walk ends where `prev` is null
            let mut first = root.hash;
            let mut steps = 0u64;
            loop {
                let key = ObjectKey {
                    kind: kinds::VIEW_OP,
                    hash: first,
                };
                let Some(op) = stream_source::typed::<crate::view::ViewOp>(source, key)? else {
                    return Err(stream_source::missing(key));
                };
                match op.prev {
                    Some(p) => first = p,
                    None => break,
                }
                steps += 1;
                if steps > crate::sync_stream::MAX_HISTORY_DEPTH {
                    return Err(StreamError::limit(
                        None,
                        0,
                        crate::stream_error::LIMIT_HISTORY_DEPTH,
                    ));
                }
            }
            let v = SyncView {
                repo_id,
                url: target.url.clone(),
                root: first,
                fetched: Some(root.hash),
                applied: old.and_then(|o| o.applied),
            };
            (
                crate::sync_state::view_ref(&target.remote, view),
                SyncValue::View(v),
            )
        }
        _ => return Err(fetch_mismatch(root)),
    };
    let bytes = value.encode();
    if reader.get_ref(&name).map_err(store)?.as_deref() == Some(&bytes[..]) {
        return Ok(None);
    }
    Ok(Some((name, bytes)))
}

/// The caller's check of an upload.
type UploadCheck<'a, E> =
    Box<dyn FnOnce(&mut dyn ObjectSource, &UploadFacts) -> Result<TagChoice, E> + 'a>;
/// The caller's check of a fetch.
type FetchCheck<'a, E> = Box<dyn FnOnce(&mut dyn ObjectSource, &FetchFacts) -> Result<(), E> + 'a>;

/// What a take takes: a holder's upload for a publication tag, or a project's fetched canon or
/// view history (D-3RSCW), each with the caller's check.
pub(crate) enum Intake<'a, E> {
    Upload(&'a UploadTarget, UploadCheck<'a, E>),
    Fetch(&'a FetchTarget, FetchCheck<'a, E>),
}

/// Prepares a take (module docs, steps 1 to 9): every check before anything is written, the
/// caller's check, and the frames to write in canonical order.
pub(crate) fn prepare<'a, E>(
    rx: &Receiver,
    files: &[File],
    target: &'a UploadTarget,
    check: impl FnOnce(&mut dyn ObjectSource, &UploadFacts) -> Result<TagChoice, E> + 'a,
) -> Result<Plan, TakeError<E>> {
    prepare_intake(rx, files, Intake::Upload(target, Box::new(check)))
}

/// Prepares a project's fetch: the take's steps 1, 2 and 4 to 7 over the project, then the
/// fetch's own: the header's purpose and root are the target's, the caller's check, each TAG
/// of the union written as `tag/<name>` when that name is free (one that names the same
/// landing stays; one that names another landing is kept and listed), and the remote's sync
/// state with the root as its fetched tip or head, in the same batch. A tag the project names
/// after the fetch keeps its relay answer by `answer/tag:<name>` when that ref is free. No canon
/// or view ref moves. A removal in the stream is refused
/// ([`FETCH_REMOVAL`]).
pub(crate) fn prepare_fetch<'a, E>(
    rx: &Receiver,
    files: &[File],
    target: &'a FetchTarget,
    check: impl FnOnce(&mut dyn ObjectSource, &FetchFacts) -> Result<(), E> + 'a,
) -> Result<Plan, TakeError<E>> {
    prepare_intake(rx, files, Intake::Fetch(target, Box::new(check)))
}

/// [`prepare`] and [`prepare_fetch`].
fn prepare_intake<E>(
    rx: &Receiver,
    files: &[File],
    intake: Intake<'_, E>,
) -> Result<Plan, TakeError<E>> {
    let fetch = matches!(intake, Intake::Fetch(..));
    // 1. every file alone, and the union's index
    let mut union: BTreeMap<ObjectKey, Entry> = BTreeMap::new();
    let mut prerequisites: BTreeSet<ObjectKey> = BTreeSet::new();
    let header = match index_files(rx, files, &mut union, &mut prerequisites) {
        Ok(h) => h,
        Err(IndexError::Repo(source)) => {
            return Err(TakeError::RepoMismatch {
                source,
                target: rx.repo,
            });
        }
        Err(IndexError::Stream(e)) => return Err(e.into()),
    };
    // a holder takes tag histories only, and a project fetches canon and view histories
    let expected = match &intake {
        Intake::Upload(..) => header.purpose == crate::sync_stream::StreamPurpose::TagHistory,
        Intake::Fetch(t, _) => header.purpose == t.purpose,
    };
    if !expected {
        let want = match &intake {
            Intake::Upload(..) => crate::sync_stream::StreamPurpose::TagHistory,
            Intake::Fetch(t, _) => t.purpose,
        };
        return Err(
            StreamError::purpose_for(Some(0), 0, header.purpose.byte(), want.name()).into(),
        );
    }
    // the stream's features: this holder writes what it takes, so a feature it may only read,
    // or not open, refuses the take before anything is read further
    crate::format::check_writable(&header.features)
        .map_err(|e| StreamError::store_format(Some(0), 0, &e))?;
    // the snapshot deltas (D-918YB, D-8T7PA): a holder without the feature takes no upload that
    // holds it, empty or not, while this rdy has the step that upgrades the holder: the host
    // upgrades it first. Refused before anything is read further
    let sd = crate::format::FEATURE_SNAPSHOT_DELTA;
    let upload_deltas =
        header.features.has(sd) || union.values().any(|e| e.needs.iter().any(|f| f.name == sd));
    let holder_deltas = rx.features.has(sd);
    if upload_deltas && !holder_deltas && crate::upgrade::converts_deltas() {
        return Err(TakeError::Store(StoreError::UpgradeRequired));
    }
    let objects: Vec<ObjectKey> = union.keys().copied().collect();
    let reader = rx.reader;
    let mut source = UploadSource {
        files,
        union: &union,
        store: StoreSource {
            reader,
            end: END_OF_LOG,
        },
        masked: BTreeMap::new(),
        deltas: None,
        features: rx.features,
    };
    // 2. every prerequisite, from the union or the holder. A placeholder's proof that names a
    // carried record of the holder is no removal proof (D-0VG72): the placeholder is refused
    if let Err(e) = stream_source::check_prerequisites(&mut source, &prerequisites) {
        if let Some(p) = e.object.filter(|_| e.code == "E_UNRESOLVED_REF")
            && let Some(placeholder) = names_carried(rx, &union, &p)?
        {
            return Err(bad(Reason::InvalidRemovalProof, placeholder).into());
        }
        return Err(e.into());
    }
    // 3. the root names the target
    let root_key = header.root();
    let root: Option<Tag> = match &intake {
        Intake::Upload(target, _) => {
            let root: Tag = stream_source::root_tag(&mut source, root_key)?;
            if root.name.as_bytes() != target.tag.as_bytes() || root.landing != target.landing {
                return Err(target_mismatch(root_key).into());
            }
            Some(root)
        }
        Intake::Fetch(target, _) => {
            if header.purpose != target.purpose
                || !header.purpose.is_history()
                || root_key.hash != target.root
            {
                return Err(fetch_mismatch(root_key).into());
            }
            None
        }
    };
    // 4. SIG's rule on every new signature
    for k in union.keys().filter(|k| k.kind == kinds::SIGNATURE) {
        if rx.holds(k)? {
            continue;
        }
        let Some(r) = source.union_record(k)? else {
            continue;
        };
        let mut items = Vec::new();
        if let Some(Ok(g)) = r.typed::<Signature>() {
            for kind in [kinds::LANDING, kinds::TAG] {
                let subject = ObjectKey {
                    kind,
                    hash: g.subject,
                };
                if let Some(s @ StreamRecord::Present { .. }) = source.union_record(&subject)? {
                    let raw = s.raw_kind();
                    let StreamRecord::Present { payload, .. } = s else {
                        unreachable!("a present record")
                    };
                    items.push((raw, payload));
                }
            }
        }
        let raw = r.raw_kind();
        let StreamRecord::Present { payload, .. } = r else {
            unreachable!("a present record")
        };
        items.push((raw, payload));
        reader.check_new_signatures(&items)?;
    }
    // 5. the closure
    stream_source::check_closure(&mut source, root_key, root.as_ref(), &objects)?;
    // 5. and each landing's delta across every file, which an early closure end never skips
    stream_source::check_snapshot_deltas(&mut source, &header.features, &objects)?;
    // 5. and the typed context of the new plans and plan answers, over the union and the holder
    // (with the new landings, which a landing's plans look up by the version they land), and of
    // the new records that may hold a carried shape (`crate::carried_records`)
    let mut new_keys: Vec<ObjectKey> = Vec::new();
    for k in union.keys().filter(|k| {
        matches!(k.kind, kinds::PLAN | kinds::ANSWER | kinds::LANDING)
            || crate::carried_records::may_carry(k.kind)
    }) {
        if !rx.holds(k)? {
            new_keys.push(*k);
        }
    }
    stream_source::check_typed_context(&mut source, Some(reader), &new_keys)?;
    drop(new_keys);
    // 6. the approvals: every new apply's proof, and the holder's active ones
    let empty = Imported::default();
    let active: &Imported = rx.auth.imported.as_ref().unwrap_or(&empty);
    let mut new_applies: Vec<Hash> = Vec::new();
    for k in union.keys().filter(|k| k.kind == kinds::REDACTION_APPLY) {
        if !rx.holds(k)? {
            new_applies.push(k.hash);
        }
    }
    let incoming: BTreeMap<Hash, PortableRemoval> =
        stream_source::check_applies(&mut source, new_applies)?;
    let removal_of = |apply: &Hash| incoming.get(apply).or_else(|| active.removals.get(apply));
    // the approved tuples of each key: the holder's applies in log order, then the upload's
    let held = rx
        .auth
        .applies
        .iter()
        .filter_map(|a| active.removals.get(&a.hash).map(|r| (a.hash, r)));
    let approved = stream_source::approved_tuples(held, &incoming);
    let incoming_keys: BTreeSet<ObjectKey> = incoming
        .values()
        .flat_map(|r| r.objects.keys().copied())
        .collect();
    // every dropped record names a validated apply that approves its exact tuple, and keeps
    // the encoding its residue records
    for (key, e) in &union {
        for (_, d) in &e.dropped {
            if !stream_source::drop_fits(removal_of(&d.apply), key, d) {
                return Err(bad(Reason::InvalidRemovalProof, *key).into());
            }
            let res = match d.residue {
                Some(r) => Some(residue(&mut source, r)?.0),
                None => None,
            };
            if !stream_source::drop_encoding_agrees(d.encoding, res.as_ref()) {
                return Err(bad(Reason::InvalidRemovalProof, *key).into());
            }
        }
    }
    // one tuple per key this take touches
    let one_tuple = |key: &ObjectKey, e: Option<&Entry>| -> Result<RemovalObject, StreamError> {
        stream_source::one_tuple(
            &approved,
            key,
            e.into_iter().flat_map(|e| e.dropped.iter().map(|(_, d)| d)),
        )
    };
    // every key the upload approves or drops has one tuple across the holder's approvals, the
    // upload's and its dropped records, held or not: a key the holder removed already keeps the
    // identity its approval gave it
    for key in incoming_keys.iter().chain(
        union
            .iter()
            .filter(|(_, e)| !e.dropped.is_empty())
            .map(|(k, _)| k),
    ) {
        one_tuple(key, union.get(key))?;
    }
    let block_versions: HashSet<Hash> = stream_source::approved_block_versions(&approved);
    let mut writes: BTreeMap<ObjectKey, (Write, Vec<ObjectKey>)> = BTreeMap::new();
    let mut redacted: BTreeSet<ObjectKey> = BTreeSet::new();
    let mut new_payload_bytes = 0u64;
    for (key, e) in &union {
        if rx.holds(key)? {
            continue;
        }
        let covered = approved.contains_key(key) || e.present.is_none();
        if covered {
            let t = one_tuple(key, Some(e))?;
            let (raw, verify) = match (&e.present, e.dropped.first()) {
                (Some(at), _) => {
                    // a present record an approval covers: its bytes are the tuple's
                    let r = source.union_record(key)?.expect("a present record");
                    let StreamRecord::Present { payload, .. } = &r else {
                        unreachable!("a present record")
                    };
                    // the frame's raw kind and whole payload: the residue derives from its body
                    // and names its encoding (`residue_matches`)
                    let fits = payload.len() == t.len as usize
                        && match t.residue {
                            Some(res) => {
                                let (res, encoding) = residue(&mut source, res)?;
                                residue_matches(r.raw_kind(), payload, &res, encoding)
                            }
                            None => r.raw_kind() == key.kind,
                        };
                    if !fits {
                        return Err(bad(Reason::InvalidRemovalProof, *key).into());
                    }
                    (at.kind, Verify::Present(*at))
                }
                (None, Some((at, d))) => {
                    let raw = crate::codec::raw_kind(key.kind, d.encoding);
                    (
                        raw,
                        Verify::Dropped(*at, *key, raw, d.original_len, d.apply, d.residue),
                    )
                }
                (None, None) => unreachable!("a union key has a representation"),
            };
            let children: Vec<ObjectKey> = t
                .residue
                .map(|r| ObjectKey {
                    kind: kinds::REDACTION_RESIDUE,
                    hash: r,
                })
                .into_iter()
                .collect();
            new_payload_bytes += u64::from(t.len);
            redacted.insert(*key);
            writes.insert(
                *key,
                (
                    Write::Dropped {
                        key: *key,
                        raw,
                        len: t.len,
                        verify,
                    },
                    children,
                ),
            );
        } else {
            let at = e.present.expect("an uncovered key is present");
            // 7. a new change that embeds an approved version needs its own approval (the rule
            // a checked stream applies with nothing held: `stream_source::uncovered_change`)
            if key.kind == kinds::CHANGE && !block_versions.is_empty() {
                let r = source.union_record(key)?.expect("a present record");
                if stream_source::embeds_approved(&r, &block_versions) {
                    return Err(TakeError::UncoveredChange(key.hash));
                }
            }
            let mut children = Vec::with_capacity(e.refs.len());
            for r in &e.refs {
                children.push(stream_source::resolve(&mut source, *key, *r)?);
            }
            children.sort_unstable();
            children.dedup();
            new_payload_bytes += u64::from(at.len);
            writes.insert(*key, (Write::Present { key: *key, at }, children));
        }
    }
    // the holder's own copies of what the upload approves, by its log headers; and its intact
    // changes that embed a newly approved version
    let caps = ReceiverCaps::current();
    let mut scanned = crate::redaction_import::Scanned::default();
    let mut local_spans = 0u64;
    let redaction_after = rx.auth.is_active() || !incoming.is_empty();
    if redaction_after {
        let new_versions: HashSet<Hash> = incoming_keys
            .iter()
            .filter(|k| k.kind == kinds::BLOCK)
            .map(|k| k.hash)
            .collect();
        let mut failed: Option<TakeError<E>> = None;
        let scan = for_each_seen(rx.log, rx.end, &BTreeMap::new(), |f| {
            scanned.frames += 1;
            scanned.bytes += u64::from(f.len);
            if scanned.frames > caps.frames {
                failed = Some(StreamError::limit(None, 0, &caps.frames_label()).into());
                return Err(StoreError::Refused(String::new()));
            }
            if scanned.bytes > caps.bytes {
                failed = Some(StreamError::limit(None, 0, &caps.bytes_label()).into());
                return Err(StoreError::Refused(String::new()));
            }
            if rx.auth.holes.contains_key(&f.loc) {
                return Ok(());
            }
            let key = ObjectKey {
                kind: crate::codec::logical_kind(f.kind),
                hash: f.hash,
            };
            if incoming_keys.contains(&key) {
                let t = match one_tuple(&key, None) {
                    Ok(t) => t,
                    Err(e) => {
                        failed = Some(e.into());
                        return Err(StoreError::Refused(String::new()));
                    }
                };
                let intact = f.intact()?;
                let fits = f.len == t.len
                    && match (intact, t.residue) {
                        (Some(p), Some(r)) => match residue(&mut source, r) {
                            Ok((res, encoding)) => residue_matches(f.kind, &p, &res, encoding),
                            Err(e) => {
                                failed = Some(e.into());
                                return Err(StoreError::Refused(String::new()));
                            }
                        },
                        _ => true,
                    };
                if !fits {
                    failed = Some(bad(Reason::InvalidRemovalProof, key).into());
                    return Err(StoreError::Refused(String::new()));
                }
                local_spans += 1;
                redacted.insert(key);
            } else if crate::codec::logical_kind(f.kind) == kinds::CHANGE
                && !new_versions.is_empty()
                && !approved.contains_key(&key)
                && f.len as usize <= rdy_model::redaction::MAX_RECORD_BYTES
                && let Some(p) = f.intact()?
                && let Ok(c) = crate::codec::decode_record::<Change>(f.kind, &p)
                && embeds(&c, &new_versions)
            {
                failed = Some(TakeError::UncoveredChange(f.hash));
                return Err(StoreError::Refused(String::new()));
            }
            Ok(())
        });
        if let Some(e) = failed {
            return Err(e);
        }
        scan?;
    }
    // a legacy upload into a holder that holds `snapshot_delta`: the take converts it in this
    // same batch, one canonical delta for each new landing, in chain order, based on the
    // holder's own delta where the chain meets the holder (`crate::snapshot_upgrade`). The
    // upload's SNAPSHOT records are written as they came. A delta the holder holds already is
    // not written again
    let mut generated: BTreeMap<ObjectKey, Vec<u8>> = BTreeMap::new();
    if holder_deltas && !upload_deltas {
        let landings: Vec<Hash> = writes
            .iter()
            .filter(|(k, (w, _))| k.kind == kinds::LANDING && matches!(w, Write::Present { .. }))
            .map(|(k, _)| k.hash)
            .collect();
        for (d, (raw, payload)) in
            crate::snapshot_upgrade::take_deltas(reader, &mut source, &landings)?
        {
            let key = ObjectKey {
                kind: kinds::SNAPSHOT_DELTA,
                hash: Hash::of(&payload),
            };
            if rx.holds(&key)? || writes.contains_key(&key) {
                continue;
            }
            let len = u32::try_from(payload.len())
                .map_err(|_| StreamError::limit(None, 0, LIMIT_TOTAL_BYTES))?;
            new_payload_bytes += u64::from(len);
            writes.insert(
                key,
                (
                    Write::Generated { key, raw, len },
                    crate::snapshot_upgrade::delta_children(&d),
                ),
            );
            generated.insert(key, payload);
        }
        // the batch keeps the bounds of one batch with the deltas it makes
        let records = writes.len() as u64;
        if records > MAX_RECORDS {
            return Err(StreamError::limit(None, 0, LIMIT_OBJECT_RECORDS).into());
        }
    }
    // the holder keeps the stream's features (unknown compatible ones included) and activates
    // those its new records first use, with the features ref in this same batch (D-WWAK7)
    let mut needs: Vec<StoreFeature> = header.features.features.clone();
    for (w, _) in writes.values() {
        if let Write::Present { key, .. } = w {
            needs.extend(union[key].needs.iter().cloned());
        }
    }
    if fetch {
        needs.push(crate::format::registered(crate::format::FEATURE_SYNC_STATE));
    }
    let merged = rx.features.merged(&needs);
    drop(needs);
    // the holder's capacity: after this batch the holder check (under the manifest the holder
    // holds after it) can still read the whole holder
    check_capacity(reader, &mut source, &writes, &generated, &merged)?;
    // 8. the caller's check, with removed objects as their drops
    for (k, by) in &approved {
        if let Some((apply, t)) = by.first() {
            let encoding = match t.residue {
                Some(r) => crate::redaction::recorded_source_encoding(&residue(&mut source, r)?.0),
                None => 0,
            };
            source.masked.insert(*k, (*apply, *t, encoding));
        }
    }
    let mut refs: Vec<(String, Vec<u8>)> = Vec::new();
    let mut tags_taken: Vec<String> = Vec::new();
    let mut tags_kept: Vec<String> = Vec::new();
    let (tag_hash, tag) = match intake {
        Intake::Upload(_, check) => {
            let facts = UploadFacts {
                repo_id: header.repo_id,
                root_hash: root_key.hash,
                root: root.expect("an upload's root tag"),
                objects,
                relay_answers: BTreeMap::new(),
                features: header.features.clone(),
            };
            let (tag_hash, tag) = upload_refs(reader, &mut source, facts, check, &mut refs)?;
            (tag_hash, Some(tag))
        }
        Intake::Fetch(target, check) => {
            let facts = FetchFacts {
                repo_id: header.repo_id,
                purpose: header.purpose,
                root: root_key.hash,
                objects: objects.clone(),
                features: header.features.clone(),
            };
            check(&mut source, &facts).map_err(TakeError::Validation)?;
            // the relay answer of each fetched tag object, by the rule a holder take keeps
            let relayed = stream_source::relay_answers(&mut source, &objects)?;
            fetch_tag_refs(
                reader,
                &mut source,
                &objects,
                &relayed,
                &mut refs,
                &mut tags_taken,
                &mut tags_kept,
            )?;
            if let Some(r) = fetch_state_ref(reader, &mut source, target, header.repo_id, root_key)?
            {
                refs.push(r);
            }
            (root_key.hash, None)
        }
    };
    // the manifest the holder would hold keeps the manifest's bounds, whichever parts make it up
    merged
        .validate_written()
        .map_err(|e| StreamError::store_format(None, 0, &e.store_error()))?;
    let features = (merged != *rx.features).then_some(merged);
    if let Some(f) = &features {
        refs.push((crate::format::REF_STORE_FEATURES.into(), f.to_json()));
    }
    // 9. the receiver caps of a holder that holds an approval after this batch
    if redaction_after {
        // the ref frames exactly as the store salts them (`Store::ref_items`); the marker at its
        // bound
        let ref_bytes: u64 = refs
            .iter()
            .enumerate()
            .map(|(i, (name, value))| {
                crate::store::encode(&crate::kinds::RefUpdate {
                    name: name.clone(),
                    value: value.clone(),
                    at: Location {
                        segment: rx.end.segment,
                        offset: rx.end.offset + i as u64,
                    },
                })
                .len() as u64
            })
            .sum();
        // a batch, and so its marker, only when there is a frame to write: a complete retry
        // writes none (`Store::seal_take`), and its log is checked as it stands
        let batch = !(writes.is_empty() && refs.is_empty());
        let frames = scanned.frames + writes.len() as u64 + refs.len() as u64 + u64::from(batch);
        let bytes =
            scanned.bytes + new_payload_bytes + ref_bytes + if batch { MARKER_MAX } else { 0 };
        let dropped = writes
            .values()
            .filter(|(w, _)| matches!(w, Write::Dropped { .. }))
            .count() as u64;
        let spans = rx.auth.holes.len() as u64 + local_spans + dropped;
        if frames > caps.frames {
            return Err(StreamError::limit(None, 0, &caps.frames_label()).into());
        }
        if bytes > caps.bytes {
            return Err(StreamError::limit(None, 0, &caps.bytes_label()).into());
        }
        if spans > caps.spans {
            return Err(StreamError::limit(None, 0, &caps.spans_label()).into());
        }
    }
    // the canonical order: the dependency walk over the new records
    let children: BTreeMap<ObjectKey, Vec<ObjectKey>> =
        writes.iter().map(|(k, (_, c))| (*k, c.clone())).collect();
    let order = dependency_order(root_key, &children)?;
    let redaction = writes.iter().any(|(k, (w, _))| {
        matches!(w, Write::Dropped { .. })
            || matches!(
                k.kind,
                kinds::REDACTION | kinds::REDACTION_RESIDUE | kinds::REDACTION_APPLY
            )
    });
    // a fetched removal is the removal intake's (a later step): the fetch takes none
    if fetch && (redaction || !incoming.is_empty() || !redacted.is_empty()) {
        return Err(TakeError::Store(StoreError::Refused(FETCH_REMOVAL.into())));
    }
    let min_rdy = (
        rx.features.min_rdy(),
        features.as_ref().unwrap_or(rx.features).min_rdy(),
    );
    Ok(Plan {
        tags_taken,
        tags_kept,
        min_rdy,
        writes: order
            .iter()
            .map(|k| writes.remove(k).expect("an ordered write").0)
            .collect(),
        refs,
        redaction,
        new_applies: !incoming.is_empty(),
        redacted_objects: redacted.len() as u64,
        tag_hash,
        tag,
        features,
        generated,
    })
}
