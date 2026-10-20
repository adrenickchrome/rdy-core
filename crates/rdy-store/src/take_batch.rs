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
//!    names a reached object.
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
//! The take keeps one record payload buffer at a time: it indexes each record's file, number,
//! offset and frame, and reads a record again when a check or the append needs it. Each re-read
//! checks the frame's kind, length and hash (and a drop's descriptor), so input that changed
//! after the checks is refused (`input changed during take`) and its unacknowledged bytes are
//! rolled back. No private spool is needed: the append's re-read closes the race.
use crate::closure::{END_OF_LOG, StoreSource, dependency_order, store_error};
use crate::dump::{DROPPED_LEN, Frame, dropped_payload, parse_dropped};
use crate::error::StoreError;
use crate::kinds;
use crate::object_refs::{ObjectKey, ObjectRef, refs_of};
use crate::redaction::residue_matches;
use crate::redaction_import::{Imported, ReceiverCaps, embeds};
use crate::redaction_io::{Authorization, for_each_seen};
use crate::segment::{HEADER, Location, MARKER_MAX};
use crate::store::StoreReader;
use crate::stream_error::{
    LIMIT_GRAPH_EDGES, LIMIT_OBJECT_RECORDS, LIMIT_PENDING_DROPPED_BYTES, LIMIT_PENDING_DROPS,
    LIMIT_TOTAL_BYTES, Reason, StreamError,
};
use crate::sync_proof::{self, ObjectSource, PortableRemoval, RemovalObject};
use crate::sync_stream::{
    LIMIT_BATCH_FILES, MAX_BATCH_FILES, MAX_EDGES, MAX_PENDING_DROPPED_BYTES, MAX_PENDING_DROPS,
    MAX_RECORDS, MAX_STREAM_BYTES, SYNC_KINDS, StreamHeader, StreamReader, StreamRecord,
    check_batch, rec,
};
use rdy_model::redaction::{Redaction, RedactionApply, RedactionResidue};
use rdy_model::signature::Signature;
use rdy_model::{Answer, Change, Hash, RepoId, Tag};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
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
    /// The store is a project, not a holder.
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

/// The refusal of a prerequisite that neither the upload nor the holder holds.
fn missing(key: ObjectKey) -> StreamError {
    StreamError {
        code: "E_UNRESOLVED_REF",
        message: format!("stream prerequisite {key} is missing"),
        fix: "send the missing object with its dependencies".into(),
        record: Some(0),
        offset: 0,
        object: Some(key),
    }
}

fn bad(reason: Reason, object: ObjectKey) -> StreamError {
    StreamError::bad_record(0, 0, reason).with_object(Some(object))
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
    kind: u8,
    len: u32,
    hash: Hash,
}

/// A dropped record's descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Drop {
    original_len: u32,
    apply: Hash,
    residue: Option<Hash>,
}

/// One key of the union: its present record, if any file holds one, each distinct dropped
/// representation, and the links of its present record.
#[derive(Default)]
struct Entry {
    present: Option<At>,
    refs: Vec<ObjectRef>,
    dropped: Vec<(At, Drop)>,
}

/// How a planned write is checked again at the append.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Verify {
    /// A present record of a file.
    Present(At),
    /// A dropped record of a file, with its descriptor.
    Dropped(At, ObjectKey, u32, Hash, Option<Hash>),
}

/// One frame the take writes, in canonical order.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Write {
    /// The original bytes of a present record.
    Present { key: ObjectKey, at: At },
    /// The original header and zeros of an approved drop.
    Dropped {
        key: ObjectKey,
        len: u32,
        verify: Verify,
    },
}

impl Write {
    /// The object the frame writes.
    pub(crate) fn key(&self) -> ObjectKey {
        match self {
            Write::Present { key, .. } | Write::Dropped { key, .. } => *key,
        }
    }

    /// The frame's payload bytes in the log.
    pub(crate) fn len(&self) -> u32 {
        match self {
            Write::Present { at, .. } => at.len,
            Write::Dropped { len, .. } => *len,
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
    pub(crate) tag: Tag,
}

/// The holder as the take reads it: its reader, its log's segment files and authorization, the
/// log's end, and its repo id.
pub(crate) struct Receiver<'a> {
    pub(crate) reader: &'a StoreReader,
    pub(crate) log: &'a [File],
    pub(crate) auth: &'a Authorization,
    pub(crate) end: Location,
    pub(crate) repo: RepoId,
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
        Verify::Dropped(at, key, len, apply, residue) => {
            reread(files, at, buf)?;
            match parse_dropped(buf) {
                Ok(Frame::Dropped {
                    kind,
                    hash,
                    len: l,
                    apply: a,
                    residue: r,
                }) if (kind, hash, l, a, r) == (key.kind, key.hash, *len, *apply, *residue) => {
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
    masked: BTreeMap<ObjectKey, (Hash, RemovalObject)>,
}

impl ObjectSource for UploadSource<'_> {
    fn record(&mut self, key: ObjectKey) -> Result<Option<StreamRecord>, StreamError> {
        if let Some((apply, t)) = self.masked.get(&key) {
            return Ok(Some(StreamRecord::Dropped {
                key,
                original_len: t.len,
                apply: *apply,
                residue: t.residue,
            }));
        }
        if let Some(e) = self.union.get(&key) {
            if let Some(at) = &e.present {
                let mut payload = Vec::new();
                reread(self.files, at, &mut payload).map_err(|e| e.with_object(Some(key)))?;
                return Ok(Some(StreamRecord::Present { key, payload }));
            }
            if let Some((_, d)) = e.dropped.first() {
                return Ok(Some(StreamRecord::Dropped {
                    key,
                    original_len: d.original_len,
                    apply: d.apply,
                    residue: d.residue,
                }));
            }
        }
        self.store.record(key)
    }
}

impl UploadSource<'_> {
    /// The present bytes of union key `key`.
    fn union_payload(&self, key: &ObjectKey) -> Result<Option<Vec<u8>>, StreamError> {
        match self.union.get(key).and_then(|e| e.present.as_ref()) {
            Some(at) => {
                let mut payload = Vec::new();
                reread(self.files, at, &mut payload).map_err(|e| e.with_object(Some(*key)))?;
                Ok(Some(payload))
            }
            None => Ok(None),
        }
    }

    /// The record `key` decoded within its bounds, from the union's present bytes or the
    /// holder's.
    fn typed<T: serde::de::DeserializeOwned>(
        &mut self,
        key: ObjectKey,
    ) -> Result<Option<T>, StreamError> {
        let payload = match self.union_payload(&key)? {
            Some(p) => p,
            None => match self.store.record(key)? {
                Some(StreamRecord::Present { payload, .. }) => payload,
                _ => return Ok(None),
            },
        };
        crate::bounded::decode(&payload)
            .map(Some)
            .map_err(|e| StreamError::from_record(0, 0, &e).with_object(Some(key)))
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
                if (h.repo_id, h.purpose, h.tag, h.store_format)
                    != (
                        report.header.repo_id,
                        report.header.purpose,
                        report.header.tag,
                        report.header.store_format,
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
                StreamRecord::Present { payload, .. } => {
                    let refs = refs_of(key.kind, &payload)
                        .map_err(|e| StreamError::from_record(n, offset, &e))?;
                    counts.edges += refs.len() as u64;
                    if entry.present.is_none() {
                        entry.present = Some(At {
                            file: i,
                            record: n,
                            offset,
                            kind: key.kind,
                            len: payload.len() as u32,
                            hash: key.hash,
                        });
                        entry.refs = refs;
                    }
                }
                StreamRecord::Dropped {
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
                    let body = dropped_payload(
                        key.kind,
                        &key.hash,
                        original_len,
                        &apply,
                        residue.as_ref(),
                    );
                    let d = Drop {
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
                                len: DROPPED_LEN as u32,
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

/// The key a link names: its own kind, or for a kind-None link the one record of the union or
/// the holder of its hash (none: kind 0; two or more: `ambiguous object kind`).
fn resolve(
    rx: &Receiver,
    union: &BTreeMap<ObjectKey, Entry>,
    from: ObjectKey,
    r: ObjectRef,
) -> Result<ObjectKey, StreamError> {
    if let Some(kind) = r.kind {
        return Ok(ObjectKey { kind, hash: r.hash });
    }
    let mut found = None;
    for &kind in &SYNC_KINDS {
        let k = ObjectKey { kind, hash: r.hash };
        if union.contains_key(&k) || rx.holds(&k)? {
            if found.is_some() {
                return Err(bad(Reason::AmbiguousObjectKind, from));
            }
            found = Some(k);
        }
    }
    Ok(found.unwrap_or(ObjectKey {
        kind: 0,
        hash: r.hash,
    }))
}

/// The reverse attachments of the union's records, by what triggers them.
#[derive(Default)]
struct Attachments {
    /// Answers by each hash they target.
    answers: HashMap<Hash, Vec<ObjectKey>>,
    /// Signatures by subject.
    signatures: HashMap<Hash, Vec<ObjectKey>>,
    /// Requests by carrier.
    requests: HashMap<Hash, Vec<ObjectKey>>,
    /// Applies by each object their request names.
    applies: HashMap<ObjectKey, Vec<ObjectKey>>,
    /// The relay answers of the root (O11).
    relay: Vec<ObjectKey>,
}

/// Collects the union's attachments: every answer, signature, request and apply it holds. The
/// root's relay answer is one that targets exactly the root TAG (`is_relay_answer_of`): an
/// answer to a tag of the same name that targets another tag object is not attached by name.
fn attachments(
    source: &mut UploadSource,
    root_key: ObjectKey,
    root: &Tag,
) -> Result<Attachments, StreamError> {
    let mut a = Attachments::default();
    let keys: Vec<ObjectKey> = source
        .union
        .keys()
        .filter(|k| {
            matches!(
                k.kind,
                kinds::ANSWER | kinds::SIGNATURE | kinds::REDACTION | kinds::REDACTION_APPLY
            )
        })
        .copied()
        .collect();
    for k in keys {
        match k.kind {
            kinds::ANSWER => {
                let Some(answer) = source.typed::<Answer>(k)? else {
                    continue;
                };
                for t in &answer.targets {
                    a.answers.entry(*t).or_default().push(k);
                }
                if crate::holder::is_relay_answer_of(&answer, &root_key.hash, &root.name) {
                    a.relay.push(k);
                }
            }
            kinds::SIGNATURE => {
                if let Some(g) = source.typed::<Signature>(k)? {
                    a.signatures.entry(g.subject).or_default().push(k);
                }
            }
            kinds::REDACTION => {
                if let Some(r) = source.typed::<Redaction>(k)? {
                    a.requests.entry(r.carrier).or_default().push(k);
                }
            }
            _ => {
                let Some(apply) = source.typed::<RedactionApply>(k)? else {
                    continue;
                };
                let request = ObjectKey {
                    kind: kinds::REDACTION,
                    hash: apply.request,
                };
                if let Some(r) = source.typed::<Redaction>(request)? {
                    for named in named_objects(&r) {
                        a.applies.entry(named).or_default().push(k);
                    }
                }
            }
        }
    }
    Ok(a)
}

/// The objects a request names, as the index's request entry names them: its spans' objects,
/// its versions and its blobs.
fn named_objects(r: &Redaction) -> BTreeSet<ObjectKey> {
    let m = &r.manifest;
    m.spans
        .iter()
        .map(|s| ObjectKey {
            kind: s.kind,
            hash: s.hash,
        })
        .chain(m.versions.iter().map(|(_, v)| ObjectKey {
            kind: kinds::BLOCK,
            hash: *v,
        }))
        .chain(m.blobs.iter().map(|b| ObjectKey {
            kind: kinds::BLOB_MANIFEST,
            hash: *b,
        }))
        .collect()
}

/// Checks that every union key is in the root's closure (module docs, step 5), walking forward
/// links through the union and the holder and the union's attachments.
fn check_closure(
    rx: &Receiver,
    source: &mut UploadSource,
    root_key: ObjectKey,
    root: &Tag,
) -> Result<(), StreamError> {
    let att = attachments(source, root_key, root)?;
    let mut reached: HashSet<ObjectKey> = HashSet::new();
    let mut queue: Vec<ObjectKey> = Vec::new();
    let mut edges = 0u64;
    fn reach(
        k: ObjectKey,
        reached: &mut HashSet<ObjectKey>,
        queue: &mut Vec<ObjectKey>,
    ) -> Result<(), StreamError> {
        if reached.contains(&k) {
            return Ok(());
        }
        if reached.len() as u64 >= MAX_RECORDS {
            return Err(StreamError::limit(None, 0, LIMIT_OBJECT_RECORDS));
        }
        reached
            .try_reserve(1)
            .map_err(|_| StreamError::allocation(None, 0))?;
        queue
            .try_reserve(1)
            .map_err(|_| StreamError::allocation(None, 0))?;
        reached.insert(k);
        queue.push(k);
        Ok(())
    }
    reach(root_key, &mut reached, &mut queue)?;
    for r in &att.relay {
        reach(*r, &mut reached, &mut queue)?;
    }
    while let Some(k) = queue.pop() {
        // forward links
        let mut links: Vec<ObjectKey> = Vec::new();
        if let Some(e) = source.union.get(&k) {
            for r in &e.refs {
                links.push(resolve(rx, source.union, k, *r)?);
            }
            for (_, d) in &e.dropped {
                links.extend(d.residue.map(|r| ObjectKey {
                    kind: kinds::REDACTION_RESIDUE,
                    hash: r,
                }));
                links.push(ObjectKey {
                    kind: kinds::REDACTION_APPLY,
                    hash: d.apply,
                });
            }
        } else if let Some(record) = source.store.record(k)? {
            for r in record
                .refs()
                .map_err(|e| StreamError::from_record(0, 0, &e).with_object(Some(k)))?
            {
                links.push(resolve(rx, source.union, k, r)?);
            }
            links.extend(record.proof());
        }
        // attachments
        if matches!(k.kind, kinds::LANDING | kinds::TAG) {
            links.extend(att.answers.get(&k.hash).into_iter().flatten());
        }
        if k.kind == kinds::LANDING || k == root_key {
            links.extend(att.signatures.get(&k.hash).into_iter().flatten());
        }
        if k.kind == kinds::CHANGE {
            links.extend(att.requests.get(&k.hash).into_iter().flatten());
        }
        links.extend(att.applies.get(&k).into_iter().flatten());
        edges += links.len() as u64;
        if edges > MAX_EDGES {
            return Err(StreamError::limit(None, 0, LIMIT_GRAPH_EDGES));
        }
        for l in links {
            if l.kind != 0 {
                reach(l, &mut reached, &mut queue)?;
            }
        }
    }
    match source.union.keys().find(|k| !reached.contains(k)) {
        Some(k) => Err(bad(Reason::OutsideTagClosure, *k)),
        None => Ok(()),
    }
}

/// The relay answer of each TAG object, of the union or the holder, that has one in the union
/// (tag hash to answer hash): an answer of the union of act `tag`, item `tag:<name>` and targets
/// exactly that TAG object, whose name is `<name>` (`crate::holder::is_relay_answer_of`). An
/// answer to a tag of the same name that targets another tag object is not that tag's. Two
/// relay answers of one tag object refuse the upload at the second.
fn relay_answers(
    source: &mut UploadSource,
    union: &BTreeMap<ObjectKey, Entry>,
) -> Result<BTreeMap<Hash, Hash>, StreamError> {
    let mut out: BTreeMap<Hash, Hash> = BTreeMap::new();
    for k in union.keys().filter(|k| k.kind == kinds::ANSWER) {
        let Some(a) = source.typed::<Answer>(*k)? else {
            continue;
        };
        let [tag] = a.targets[..] else {
            continue;
        };
        if a.act != rdy_model::AnswerAct::Tag {
            continue;
        }
        // the tag object of the union, or one the holder holds already (a retry's root)
        let tag_key = ObjectKey {
            kind: kinds::TAG,
            hash: tag,
        };
        let Some(t) = source.typed::<Tag>(tag_key)? else {
            continue;
        };
        if !crate::holder::is_relay_answer_of(&a, &tag, &t.name) {
            continue;
        }
        if out.contains_key(&tag) {
            return Err(bad(Reason::OutsideTagClosure, *k));
        }
        out.insert(tag, k.hash);
    }
    Ok(out)
}

/// The residue `r`, decoded, from the union or the holder.
fn residue(source: &mut UploadSource, r: Hash) -> Result<RedactionResidue, StreamError> {
    let key = ObjectKey {
        kind: kinds::REDACTION_RESIDUE,
        hash: r,
    };
    source
        .typed(key)?
        .ok_or_else(|| StreamError::unresolved(0, 0, key))
}

/// Prepares a take (module docs, steps 1 to 9): every check before anything is written, the
/// caller's check, and the frames to write in canonical order.
pub(crate) fn prepare<E>(
    rx: &Receiver,
    files: &[File],
    target: &UploadTarget,
    check: impl FnOnce(&mut dyn ObjectSource, &UploadFacts) -> Result<TagChoice, E>,
) -> Result<Plan, TakeError<E>> {
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
    // 2. every prerequisite, from the union or the holder. A placeholder's proof that names a
    // carried record of the holder is no removal proof (D-0VG72): the placeholder is refused
    for p in &prerequisites {
        if !union.contains_key(p) && !rx.holds(p)? {
            if let Some(placeholder) = names_carried(rx, &union, p)? {
                return Err(bad(Reason::InvalidRemovalProof, placeholder).into());
            }
            return Err(missing(*p).into());
        }
    }
    let reader = rx.reader;
    let mut source = UploadSource {
        files,
        union: &union,
        store: StoreSource {
            reader,
            end: END_OF_LOG,
        },
        masked: BTreeMap::new(),
    };
    // 3. the root names the target
    let root_key = header.root();
    let root: Tag = source.typed(root_key)?.ok_or_else(|| missing(root_key))?;
    if root.name.as_bytes() != target.tag.as_bytes() || root.landing != target.landing {
        return Err(target_mismatch(root_key).into());
    }
    // 4. SIG's rule on every new signature
    for k in union.keys().filter(|k| k.kind == kinds::SIGNATURE) {
        if rx.holds(k)? {
            continue;
        }
        let Some(payload) = source.union_payload(k)? else {
            continue;
        };
        let mut items = Vec::new();
        if let Ok(g) = crate::bounded::decode::<Signature>(&payload) {
            for kind in [kinds::LANDING, kinds::TAG] {
                let subject = ObjectKey {
                    kind,
                    hash: g.subject,
                };
                if let Some(p) = source.union_payload(&subject)? {
                    items.push((kind, p));
                }
            }
        }
        items.push((kinds::SIGNATURE, payload));
        reader.check_new_signatures(&items)?;
    }
    // 5. the closure
    check_closure(rx, &mut source, root_key, &root)?;
    // 6. the approvals: every new apply's proof, and the holder's active ones
    let empty = Imported::default();
    let active: &Imported = rx.auth.imported.as_ref().unwrap_or(&empty);
    let mut incoming: BTreeMap<Hash, PortableRemoval> = BTreeMap::new();
    for k in union.keys().filter(|k| k.kind == kinds::REDACTION_APPLY) {
        if rx.holds(k)? {
            continue;
        }
        incoming.insert(k.hash, sync_proof::check(&mut source, k.hash)?);
    }
    let removal_of = |apply: &Hash| incoming.get(apply).or_else(|| active.removals.get(apply));
    // the approved tuples of each key: the holder's applies in log order, then the upload's
    let mut approved: BTreeMap<ObjectKey, Vec<(Hash, RemovalObject)>> = BTreeMap::new();
    for a in &rx.auth.applies {
        if let Some(r) = active.removals.get(&a.hash) {
            for (k, t) in &r.objects {
                approved.entry(*k).or_default().push((a.hash, *t));
            }
        }
    }
    for (h, r) in &incoming {
        for (k, t) in &r.objects {
            approved.entry(*k).or_default().push((*h, *t));
        }
    }
    let incoming_keys: BTreeSet<ObjectKey> = incoming
        .values()
        .flat_map(|r| r.objects.keys().copied())
        .collect();
    // every dropped record names a validated apply that approves its exact tuple
    for (key, e) in &union {
        for (_, d) in &e.dropped {
            let fits = removal_of(&d.apply)
                .and_then(|r| r.objects.get(key))
                .is_some_and(|t| t.len == d.original_len && t.residue == d.residue);
            if !fits {
                return Err(bad(Reason::InvalidRemovalProof, *key).into());
            }
        }
    }
    // one tuple per key this take touches
    let one_tuple = |key: &ObjectKey, e: Option<&Entry>| -> Result<RemovalObject, StreamError> {
        let mut tuples: BTreeSet<(u32, Option<Hash>)> = BTreeSet::new();
        for (_, t) in approved.get(key).into_iter().flatten() {
            tuples.insert((t.len, t.residue));
        }
        for (_, d) in e.into_iter().flat_map(|e| &e.dropped) {
            tuples.insert((d.original_len, d.residue));
        }
        match tuples.into_iter().collect::<Vec<_>>()[..] {
            [(len, residue)] => Ok(RemovalObject { len, residue }),
            _ => Err(bad(Reason::InvalidRemovalProof, *key)),
        }
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
    let block_versions: HashSet<Hash> = approved
        .keys()
        .filter(|k| k.kind == kinds::BLOCK)
        .map(|k| k.hash)
        .collect();
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
            let verify = match (&e.present, e.dropped.first()) {
                (Some(at), _) => {
                    // a present record an approval covers: its bytes are the tuple's
                    let payload = source.union_payload(key)?.expect("a present record");
                    let fits = payload.len() == t.len as usize
                        && match t.residue {
                            Some(r) => {
                                residue_matches(key.kind, &payload, &residue(&mut source, r)?)
                            }
                            None => true,
                        };
                    if !fits {
                        return Err(bad(Reason::InvalidRemovalProof, *key).into());
                    }
                    Verify::Present(*at)
                }
                (None, Some((at, d))) => {
                    Verify::Dropped(*at, *key, d.original_len, d.apply, d.residue)
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
                        len: t.len,
                        verify,
                    },
                    children,
                ),
            );
        } else {
            let at = e.present.expect("an uncovered key is present");
            // 7. a new change that embeds an approved version needs its own approval
            if key.kind == kinds::CHANGE && !block_versions.is_empty() {
                let payload = source.union_payload(key)?.expect("a present record");
                if let Ok(c) = crate::store::decode::<Change>(&payload)
                    && embeds(&c, &block_versions)
                {
                    return Err(TakeError::UncoveredChange(key.hash));
                }
            }
            let mut children = Vec::with_capacity(e.refs.len());
            for r in &e.refs {
                children.push(resolve(rx, &union, *key, *r)?);
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
                kind: f.kind,
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
                            Ok(res) => residue_matches(f.kind, &p, &res),
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
            } else if f.kind == kinds::CHANGE
                && !new_versions.is_empty()
                && !approved.contains_key(&key)
                && f.len as usize <= rdy_model::redaction::MAX_RECORD_BYTES
                && let Some(p) = f.intact()?
                && let Ok(c) = crate::store::decode::<Change>(&p)
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
    // 8. the caller's check, with removed objects as their drops
    for (k, by) in &approved {
        if let Some((apply, t)) = by.first() {
            source.masked.insert(*k, (*apply, *t));
        }
    }
    // the relay answer of every tag object of the union (O11, ruling task-13), each named by
    // its tag object whichever tag stays the version: at most one per tag object, and never
    // another one than the holder holds for it. The caller's check judges signatures against
    // them, so they are found before it runs
    let relayed = relay_answers(&mut source, &union)?;
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
    let facts = UploadFacts {
        repo_id: header.repo_id,
        root_hash: root_key.hash,
        root: root.clone(),
        objects: union.keys().copied().collect(),
        relay_answers: relayed,
    };
    let choice = check(&mut source, &facts).map_err(TakeError::Validation)?;
    // each answer is read again after the check (a reread checks its frame and hash): input
    // changed since then refuses here, before any append, as an answer the holder holds is not
    // read again by the append
    if relay_answers(&mut source, &union)? != facts.relay_answers {
        return Err(StreamError::bad_record(0, 0, Reason::InputChangedDuringTake).into());
    }
    let mut refs: Vec<(String, Vec<u8>)> = Vec::new();
    let (tag_hash, tag) = match choice {
        TagChoice::Incoming => {
            let name = format!("{}{}", crate::holder::TAG_REF, root.name);
            match reader.get_ref(&name)? {
                Some(v) if v == root_key.hash.0 => {}
                Some(_) => return Err(bad(Reason::InvalidHeader, root_key).into()),
                None => refs.push((name, root_key.hash.0.to_vec())),
            }
            (root_key.hash, root.clone())
        }
        TagChoice::Existing { name, hash } => {
            let held = reader.get_ref(&format!("{}{name}", crate::holder::TAG_REF))?;
            let tag_key = ObjectKey {
                kind: kinds::TAG,
                hash,
            };
            let stored: Option<Tag> = match held {
                Some(v) if v == hash.0 => match reader.get_kind(kinds::TAG, &hash) {
                    Ok(p) => crate::bounded::decode(&p).ok(),
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
    Ok(Plan {
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
    })
}
