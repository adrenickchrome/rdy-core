//! A checked render source without a store (hub r-7GS51: a stream render "with no store open";
//! D-D2NPN: "verify-on-receive"; D-48NYM, D-0VCQ5, D-KDFZ3). [`RenderSource::read`] copies one
//! tag history stream to one anonymous scratch file, checks it as a take checks an upload with
//! nothing held, and then answers the reads a publication makes ([`PublicationSource`]) from
//! that file: the tagged state, the profile fields of the supplied effective pack, and the
//! blobs of the selected figures. It reads no working directory, config or project, and creates
//! no `.rdy`, FORMAT, segment or index file. Nothing outlives it: the scratch file closes when
//! the source drops, on success, on a refusal and on an early output failure alike.
//!
//! # What the read checks before anything selects
//!
//! 1. The render inputs (`render_inputs::validate_inputs`).
//! 2. The stream's bytes as they are copied: at most `MAX_STREAM_BYTES`; one byte more is
//!    refused once it is read, and never kept.
//! 3. The stream whole (`rdy_store::stream_source::CheckedStream::open`): the batch rules of
//!    `sync_stream::check_batch` (the purpose `tag_history`, one root TAG, framing, hashes,
//!    bounded records, record rules, trailer, end of file, order, the 1,000,000 records and
//!    edges and the 100,000 history depth), then the take's completeness checks with nothing
//!    held: no prerequisite (a valid nonempty list is E_UNRESOLVED_REF for its first key), every
//!    record in the root's closure, every removal proof valid and every dropped record proved;
//!    and the take's step 7: a present change that embeds an approved block version and that
//!    no approval covers is the take's own E_REDACTION_STALE.
//! 4. The root's name: the take's 1,048,576-byte bound, then the version parser.
//! 5. The take's structural and snapshot checks ([`check_snapshots`]): each record's local
//!    limits and chunk lengths, the landing and chain rules, every signature, and every
//!    landing's snapshot within the Publications limits. No historical approval is judged
//!    again, and no block is checked against the current pack or gate.
//!
//! Then the root's state is materialized (a removed block reads as its approved residue's stub,
//! with its original version hash) and the tag's redaction marker is read ([`tag_is_redacted`]).
//! A removed blob yields its removal request, never stale bytes; a present blob's bytes are read
//! from the scratch file only when a caller asks for them, checked chunk by chunk.
use crate::Materialized;
use crate::error::{E_IO, RdyError};
use crate::publication::{self, PublicationSource, SnapshotBudget, check_snapshot_limits};
use crate::read_state::ReadState;
use crate::render_inputs::{RenderInputs, validate_inputs};
use crate::signature_check::{
    CheckContext, STATEMENT_FOREIGN_REPO, STATEMENT_MATCH, SignatureSubject, check_signature,
};
use crate::stream_error::{Reason, StreamError, to_rdy};
use crate::version::parse_version;
use rdy_model::redaction::{RedactionApply, RedactionResidue};
use rdy_model::signature::Signature;
use rdy_model::{Answer, Block, BlockId, Change, Hash, LandingRecord, Op, StateSnapshot};
use rdy_schema::Pack;
use rdy_store::check_holder;
use rdy_store::kinds::{self, BlobManifest};
use rdy_store::object_refs::ObjectKey;
use rdy_store::stream_error::LIMIT_TOTAL_BYTES;
use rdy_store::stream_source::CheckedStream;
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::{MAX_STREAM_BYTES, StreamRecord};
use rdy_store::take_batch::{UploadFacts, UploadTarget, check_input_bounds};
use rdy_store::{StoreError, StoreReader, bounded};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::ControlFlow;
use std::sync::Arc;

/// The scratch file a render copies its stream to: any readable, writable, seekable file that
/// may move to another thread with its source.
pub trait ScratchFile: Read + Write + Seek + Send {}

impl<T: Read + Write + Seek + Send> ScratchFile for T {}

/// The bytes one copy step moves.
const COPY_BUFFER: usize = 64 * 1024;

/// E_IO: the render scratch file could not be made, written or read.
fn scratch_error() -> RdyError {
    RdyError::new(
        E_IO,
        "the render scratch file could not be used",
        "check the temporary directory and available disk space",
    )
}

/// A stream error as the engine's, a read of the scratch file that failed as the scratch error.
fn from_stream(e: StreamError) -> RdyError {
    if e.code == E_IO {
        return scratch_error();
    }
    to_rdy(e)
}

/// An engine error of a check that read the scratch file: an I/O failure (the only I/O a
/// render does after the copy is a scratch read) is the scratch error.
fn scratch_io(e: RdyError) -> RdyError {
    if e.code == E_IO { scratch_error() } else { e }
}

/// One checked tag history stream, read from its scratch file, with the render inputs the
/// caller supplied: the tagged state, its redaction marker and the publication reads.
pub struct RenderSource {
    stream: CheckedStream<Box<dyn ScratchFile>>,
    materialized: Materialized,
    inputs: RenderInputs,
    tag_redacted: bool,
}

impl std::fmt::Debug for RenderSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderSource")
            .field("root", &self.facts().root_hash)
            .field("blocks", &self.materialized.len())
            .field("tag_redacted", &self.tag_redacted)
            .finish_non_exhaustive()
    }
}

impl RenderSource {
    /// The stream `input`, rendered with `inputs`, through one anonymous temporary file (see the
    /// module docs).
    pub fn read(input: impl Read, inputs: RenderInputs) -> Result<RenderSource, RdyError> {
        RenderSource::read_with(input, inputs, &mut || {
            Ok(Box::new(tempfile::tempfile()?) as Box<dyn ScratchFile>)
        })
    }

    /// [`RenderSource::read`] through the scratch file `scratch` makes, once, after the inputs
    /// are checked. The source owns that file and drops it with itself, or before it returns a
    /// refusal.
    pub fn read_with(
        mut input: impl Read,
        inputs: RenderInputs,
        scratch: &mut dyn FnMut() -> io::Result<Box<dyn ScratchFile>>,
    ) -> Result<RenderSource, RdyError> {
        validate_inputs(&inputs)?;
        let mut file = scratch().map_err(|_| scratch_error())?;
        copy(&mut input, &mut *file)?;
        let stream = CheckedStream::open(file).map_err(from_stream)?;
        // the take's step 7: a change that keeps removed content readable needs its approval
        if let Some(h) = stream.uncovered_change() {
            return Err(crate::stream_take::uncovered_change(h));
        }
        let facts = stream.facts();
        // the root's name: the take's bound, then the version parser
        check_input_bounds(
            1,
            &UploadTarget {
                tag: facts.root.name.clone(),
                landing: facts.root.landing,
            },
        )
        .map_err(to_rdy)?;
        parse_version(&facts.root.name)?;
        // a record read again from the scratch file that fails is the scratch error
        check_snapshots(&NothingHeld, &mut &stream, facts).map_err(scratch_io)?;
        let (snapshot, blocks) = stream.materialize_tag().map_err(from_stream)?;
        let landing = facts.root.landing;
        let tag_redacted = tag_is_redacted(&mut StreamHistory(&stream), landing, &inputs.pack)?;
        Ok(RenderSource {
            stream,
            materialized: Materialized::build(snapshot, blocks),
            inputs,
            tag_redacted,
        })
    }

    /// What the checks found: the repo, the root tag, every key and the relay answers.
    pub fn facts(&self) -> &UploadFacts {
        self.stream.facts()
    }

    /// The root tag's state: its snapshot and blocks, a removed block as its stub.
    pub fn materialized(&self) -> &Materialized {
        &self.materialized
    }

    /// The render inputs the caller supplied, checked.
    pub fn inputs(&self) -> &RenderInputs {
        &self.inputs
    }

    /// Whether the tag's history holds content an approved removal removed
    /// ([`tag_is_redacted`] over the stream, with the supplied effective pack).
    pub fn tag_redacted(&self) -> bool {
        self.tag_redacted
    }

    /// The one removal walk of blob `h`: the manifest's mark, then each chunk's. `step` answers
    /// each request and may break the walk.
    fn walk(
        &self,
        h: &Hash,
        step: &mut dyn FnMut(Hash) -> Result<ControlFlow<()>, RdyError>,
    ) -> Result<(), RdyError> {
        let manifest_key = ObjectKey {
            kind: kinds::BLOB_MANIFEST,
            hash: *h,
        };
        if !self.stream.holds(&manifest_key) {
            return Ok(());
        }
        if let Some(request) = self.stream.redaction_of(&manifest_key)
            && step(request)?.is_break()
        {
            return Ok(());
        }
        let Some(manifest) = self.stored_manifest(h)? else {
            return Ok(());
        };
        for c in &manifest.chunks {
            let chunk = ObjectKey {
                kind: kinds::BLOB_CHUNK,
                hash: *c,
            };
            if let Some(request) = self.stream.redaction_of(&chunk)
                && step(request)?.is_break()
            {
                break;
            }
        }
        Ok(())
    }

    /// The manifest of blob `h` the stream holds, whatever removal marks it.
    fn stored_manifest(&self, h: &Hash) -> Result<Option<BlobManifest>, RdyError> {
        self.stream
            .typed(ObjectKey {
                kind: kinds::BLOB_MANIFEST,
                hash: *h,
            })
            .map_err(from_stream)
    }
}

/// Copies `input` to `out` while counting its bytes: one byte past `MAX_STREAM_BYTES` is
/// refused once it is read, and never written. Then `out` is rewound.
fn copy(input: &mut dyn Read, out: &mut dyn ScratchFile) -> Result<(), RdyError> {
    let mut buf = vec![0u8; COPY_BUFFER];
    let mut total: u64 = 0;
    loop {
        // at most one byte past the bound is ever read
        let room = (MAX_STREAM_BYTES + 1 - total).min(COPY_BUFFER as u64) as usize;
        let n = match input.read(&mut buf[..room]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(to_rdy(StreamError::io(None, total, &e))),
        };
        total += n as u64;
        if total > MAX_STREAM_BYTES {
            return Err(to_rdy(StreamError::limit(None, 0, LIMIT_TOTAL_BYTES)));
        }
        out.write_all(&buf[..n]).map_err(|_| scratch_error())?;
    }
    out.flush().map_err(|_| scratch_error())?;
    out.seek(SeekFrom::Start(0)).map_err(|_| scratch_error())?;
    Ok(())
}

impl PublicationSource for RenderSource {
    fn pack(&self) -> &Pack {
        &self.inputs.pack
    }

    fn title(&self) -> &str {
        &self.inputs.title
    }

    fn description(&self) -> Option<&str> {
        self.inputs.description.as_deref()
    }

    fn manifest(&self, blob: &Hash) -> Result<Option<BlobManifest>, RdyError> {
        self.stored_manifest(blob)
    }

    fn blob_available(&self, blob: &Hash) -> Result<bool, RdyError> {
        if self.stored_manifest(blob)?.is_none() {
            return Ok(false);
        }
        Ok(self.removed_blob(blob)?.is_none())
    }

    fn blob(&self, blob: &Hash) -> Result<Vec<u8>, RdyError> {
        if let Some(request) = self.removed_blob(blob)? {
            return Err(StoreError::Redacted { request }.into());
        }
        let m = self
            .stored_manifest(blob)?
            .ok_or(StoreError::NotFound(*blob))?;
        let l = publication::limits();
        if m.len > l.blob {
            return Err(publication::limit("blob", l.blob, "bytes"));
        }
        let mut out = Vec::new();
        out.try_reserve_exact(m.len as usize)
            .map_err(|_| to_rdy(StreamError::allocation(None, 0)))?;
        let mut left = m.len;
        for c in &m.chunks {
            let key = ObjectKey {
                kind: kinds::BLOB_CHUNK,
                hash: *c,
            };
            let bytes = match self.stream.read(key).map_err(from_stream)? {
                Some(StreamRecord::Present { payload, .. }) => payload,
                _ => return Err(StoreError::NotFound(*c).into()),
            };
            left = rdy_store::check_chunk_len(blob, left, bytes.len() as u64)?;
            out.extend_from_slice(&bytes);
        }
        Ok(out)
    }

    fn removed_blob(&self, blob: &Hash) -> Result<Option<Hash>, RdyError> {
        let mut first = None;
        self.walk(blob, &mut |request| {
            first = Some(request);
            Ok(ControlFlow::Break(()))
        })?;
        Ok(first)
    }

    fn visit_blob_redactions(
        &self,
        blob: &Hash,
        visit: &mut dyn FnMut(Hash) -> Result<(), RdyError>,
    ) -> Result<(), RdyError> {
        self.walk(blob, &mut |request| {
            visit(request)?;
            Ok(ControlFlow::Continue(()))
        })
    }
}

// ---------------------------------------------------------------------------------------------
// the tag's redaction marker

/// What the redaction marker of one tag reads: its landings, the original records of the
/// changes they carry, which objects an approved removal removed, and blob manifests.
pub trait TagHistorySource {
    /// Landing `h`.
    fn landing(&mut self, h: Hash) -> Result<LandingRecord, RdyError>;
    /// Change `h` as it was recorded, not a read projection. A caller asks
    /// [`TagHistorySource::removed`] first: a removed change has no record to give.
    fn change(&mut self, h: Hash) -> Result<Change, RdyError>;
    /// Whether an approved removal removed the object of `kind` with hash `h`, or names it (a
    /// block version, a blob manifest).
    fn removed(&mut self, kind: u8, h: Hash) -> Result<bool, RdyError>;
    /// The manifest of blob `h`, None for a blob the source lacks.
    fn manifest(&mut self, h: Hash) -> Result<Option<BlobManifest>, RdyError>;
}

/// Whether the history of the tag at landing `landing` holds content an approved removal
/// removed: the rule of a project's version marker (`VersionInfo::redacted`), read from one tag
/// with no canon. The walk follows only `landing` and its `prev` chain, within the holder
/// check's bounds (a landing met twice, or more than 100,000 landings, refuses). It is true when
/// a landing of the chain carries a removed change; or a change whose Create or Update writes a
/// block whose original version hash is removed; or such a block of a type with the asset trait
/// of `pack` whose `blob` field parses as a hash that names a removed manifest, or a manifest
/// with a removed chunk. A Delete writes no version; a `blob` field of any other type is plain
/// text; a manifest the source lacks marks nothing. False only after the whole chain is read.
pub fn tag_is_redacted(
    source: &mut dyn TagHistorySource,
    landing: Hash,
    pack: &Pack,
) -> Result<bool, RdyError> {
    let head = ObjectKey {
        kind: kinds::LANDING,
        hash: landing,
    };
    let chain = check_holder::tag_chain_with(&landing, &mut |h| {
        source.landing(*h).map(Arc::new).map_err(Rule::Other)
    })
    .map_err(|e| e.into_rdy(head))?;
    let mut blobs: HashSet<Hash> = HashSet::new();
    for (_, rec) in &chain {
        for c in &rec.changes {
            if source.removed(kinds::CHANGE, *c)? {
                return Ok(true);
            }
            let change = source.change(*c)?;
            for op in &change.ops {
                let (Op::Create { block } | Op::Update { block, .. }) = op else {
                    continue;
                };
                if source.removed(kinds::BLOCK, block.version_hash())? {
                    return Ok(true);
                }
                // only an asset's `blob` field names a blob; on any other type it is plain text
                if !pack.is_asset(&block.block_type) {
                    continue;
                }
                let Some(h) = block.field_str("blob").and_then(|s| s.parse::<Hash>().ok()) else {
                    continue;
                };
                if !blobs.insert(h) {
                    continue;
                }
                if source.removed(kinds::BLOB_MANIFEST, h)? {
                    return Ok(true);
                }
                let Some(m) = source.manifest(h)? else {
                    continue;
                };
                for chunk in &m.chunks {
                    if source.removed(kinds::BLOB_CHUNK, *chunk)? {
                        return Ok(true);
                    }
                }
            }
        }
    }
    Ok(false)
}

/// The tag history of one checked stream.
struct StreamHistory<'a, R: Read + Seek>(&'a CheckedStream<R>);

impl<R: Read + Seek> TagHistorySource for StreamHistory<'_, R> {
    fn landing(&mut self, h: Hash) -> Result<LandingRecord, RdyError> {
        let key = ObjectKey {
            kind: kinds::LANDING,
            hash: h,
        };
        self.0
            .typed(key)
            .map_err(from_stream)?
            .ok_or_else(|| to_rdy(StreamError::unresolved(0, 0, key)))
    }

    fn change(&mut self, h: Hash) -> Result<Change, RdyError> {
        let key = ObjectKey {
            kind: kinds::CHANGE,
            hash: h,
        };
        match self.0.read(key).map_err(from_stream)? {
            Some(StreamRecord::Present { payload, .. }) => decode(&payload, key),
            Some(StreamRecord::Dropped { .. }) => match self.0.redaction_of(&key) {
                Some(request) => Err(StoreError::Redacted { request }.into()),
                None => Err(to_rdy(StreamError::unresolved(0, 0, key))),
            },
            None => Err(to_rdy(StreamError::unresolved(0, 0, key))),
        }
    }

    fn removed(&mut self, kind: u8, h: Hash) -> Result<bool, RdyError> {
        Ok(self.0.redaction_of(&ObjectKey { kind, hash: h }).is_some())
    }

    fn manifest(&mut self, h: Hash) -> Result<Option<BlobManifest>, RdyError> {
        self.0
            .typed(ObjectKey {
                kind: kinds::BLOB_MANIFEST,
                hash: h,
            })
            .map_err(from_stream)
    }
}

impl TagHistorySource for RenderSource {
    fn landing(&mut self, h: Hash) -> Result<LandingRecord, RdyError> {
        StreamHistory(&self.stream).landing(h)
    }

    fn change(&mut self, h: Hash) -> Result<Change, RdyError> {
        StreamHistory(&self.stream).change(h)
    }

    fn removed(&mut self, kind: u8, h: Hash) -> Result<bool, RdyError> {
        StreamHistory(&self.stream).removed(kind, h)
    }

    fn manifest(&mut self, h: Hash) -> Result<Option<BlobManifest>, RdyError> {
        StreamHistory(&self.stream).manifest(h)
    }
}

/// The tag history of one published store state, read through its captured reader: a project's
/// or a holder's (which has no canon).
pub struct ReadTagHistorySource<'a> {
    read: &'a ReadState,
}

impl<'a> ReadTagHistorySource<'a> {
    pub fn new(read: &'a ReadState) -> ReadTagHistorySource<'a> {
        ReadTagHistorySource { read }
    }
}

impl TagHistorySource for ReadTagHistorySource<'_> {
    fn landing(&mut self, h: Hash) -> Result<LandingRecord, RdyError> {
        Ok(self.read.reader().landing(&h)?)
    }

    fn change(&mut self, h: Hash) -> Result<Change, RdyError> {
        Ok(self.read.reader().get_typed(kinds::CHANGE, &h)?)
    }

    fn removed(&mut self, kind: u8, h: Hash) -> Result<bool, RdyError> {
        Ok(self.read.reader().redaction_of(kind, &h)?.is_some())
    }

    fn manifest(&mut self, h: Hash) -> Result<Option<BlobManifest>, RdyError> {
        match self.read.reader().get_typed(kinds::BLOB_MANIFEST, &h) {
            Ok(m) => Ok(Some(m)),
            Err(StoreError::NotFound(_) | StoreError::Redacted { .. }) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// the structural and snapshot checks a take and a render share

/// What a receiver holds already: a holder's store for a take, nothing for a render. A held
/// record was checked when it was taken, so the checks skip it.
pub(crate) trait Held {
    /// Whether the receiver holds an object of `key`.
    fn holds(&self, key: &ObjectKey) -> Result<bool, StoreError>;
    /// The value of the receiver's `answer/tag-object:<tag>` ref, if any.
    fn tag_answer(&self, tag: &Hash) -> Result<Option<Vec<u8>>, StoreError>;
    /// The approved removal request of the receiver that removes or names the object `key`
    /// (`StoreReader::redaction_of`), if any.
    fn redaction_of(&self, key: &ObjectKey) -> Result<Option<Hash>, StoreError>;
}

impl Held for StoreReader {
    fn holds(&self, key: &ObjectKey) -> Result<bool, StoreError> {
        self.has_kind(key.kind, &key.hash)
    }

    fn tag_answer(&self, tag: &Hash) -> Result<Option<Vec<u8>>, StoreError> {
        self.get_ref(&rdy_store::holder::answer_tag_object_ref(tag))
    }

    fn redaction_of(&self, key: &ObjectKey) -> Result<Option<Hash>, StoreError> {
        StoreReader::redaction_of(self, key.kind, &key.hash)
    }
}

/// A receiver that holds nothing: a render.
pub(crate) struct NothingHeld;

impl Held for NothingHeld {
    fn holds(&self, _: &ObjectKey) -> Result<bool, StoreError> {
        Ok(false)
    }

    fn tag_answer(&self, _: &Hash) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(None)
    }

    fn redaction_of(&self, _: &ObjectKey) -> Result<Option<Hash>, StoreError> {
        Ok(None)
    }
}

/// The structural and snapshot checks of every record of `facts` that `held` does not hold,
/// read through `source` (a take's union and holder, or a checked stream), in the take's order:
/// each new block's and blob manifest's local limits and chunk lengths, each new landing's
/// fields and the root tag's chain rules, each new signature, then each new landing's snapshot
/// within the Publications limits. A record that breaks a structural rule is E_BAD_RECORD with
/// the holder fsck's own text.
pub(crate) fn check_snapshots(
    held: &dyn Held,
    source: &mut dyn ObjectSource,
    facts: &UploadFacts,
) -> Result<(), RdyError> {
    check_records(held, source, facts)?;
    check_landing_rules(held, source, facts)?;
    check_signatures(held, source, facts)?;
    check_landings(held, source, facts)
}

/// SIG's check of every signature of the upload that the receiver does not hold, as the
/// holder's fsck checks it once the take is sealed: against its subject read from the source,
/// and a relayed tag's relay answer as the take names it (`UploadFacts::relay_answers`, else the
/// one the receiver names), with no fork lineage and no trust file. A valid signature by a key
/// no trust file names is accepted (trust is not judged here), and so is one that names another
/// project (`foreign_repo`, ruling task-13). An invalid one is E_SIG_INVALID, and a valid one
/// that does not cover its subject E_SIG_OBJECT_MISMATCH.
fn check_signatures(
    held: &dyn Held,
    source: &mut dyn ObjectSource,
    facts: &UploadFacts,
) -> Result<(), RdyError> {
    let in_union = |k: &ObjectKey| facts.objects.binary_search(k).is_ok();
    let has = |k: &ObjectKey| -> Result<bool, RdyError> { Ok(in_union(k) || held.holds(k)?) };
    for key in facts.objects.iter().filter(|k| k.kind == kinds::SIGNATURE) {
        if held.holds(key)? {
            continue;
        }
        let Some(g) = typed::<Signature>(source, *key)? else {
            continue;
        };
        let landing = ObjectKey {
            kind: kinds::LANDING,
            hash: g.subject,
        };
        let tag = ObjectKey {
            kind: kinds::TAG,
            hash: g.subject,
        };
        let subject = match () {
            _ if has(&landing)? => match typed::<LandingRecord>(source, landing)? {
                Some(record) => SignatureSubject::Landing {
                    hash: g.subject,
                    record,
                },
                None => SignatureSubject::Missing { hash: g.subject },
            },
            _ if has(&tag)? => match typed::<rdy_model::Tag>(source, tag)? {
                Some(record) => SignatureSubject::Tag {
                    hash: g.subject,
                    record,
                    answer: relay_answer(held, source, facts, &g.subject)?,
                },
                None => SignatureSubject::Missing { hash: g.subject },
            },
            _ => SignatureSubject::Missing { hash: g.subject },
        };
        let context = CheckContext {
            subject,
            current_repo: facts.repo_id,
            source_repos: vec![],
            holder: true,
        };
        let check = check_signature(&g, &context, None);
        if check.statement == STATEMENT_MATCH || check.statement == STATEMENT_FOREIGN_REPO {
            continue;
        }
        let hash = key.hash.to_string();
        let e = if check.code.as_deref() == Some(crate::E_SIG_INVALID) {
            crate::sig_invalid(&hash)
        } else {
            crate::sig_object_mismatch(&hash)
        };
        return Err(at_object(e, *key));
    }
    Ok(())
}

/// The relay answer of TAG object `tag` once the take is sealed: the one the upload names for
/// it, else the one the receiver's `answer/tag-object:<tag>` names, if any.
fn relay_answer(
    held: &dyn Held,
    source: &mut dyn ObjectSource,
    facts: &UploadFacts,
    tag: &Hash,
) -> Result<Option<(Hash, Answer)>, RdyError> {
    let hash = match facts.relay_answers.get(tag) {
        Some(h) => *h,
        None => match held.tag_answer(tag)? {
            Some(v) => match <[u8; 32]>::try_from(v.as_slice()) {
                Ok(b) => Hash(b),
                // a ref that is no hash is the holder fsck's to report: it names no answer
                Err(_) => return Ok(None),
            },
            None => return Ok(None),
        },
    };
    let key = ObjectKey {
        kind: kinds::ANSWER,
        hash,
    };
    Ok(typed::<Answer>(source, key)?.map(|a| (hash, a)))
}

/// The local limits of every block and blob manifest of the upload that the receiver does not
/// hold, as the batch check keeps them for each record (`stream_check::check_batch`): a block's
/// own text, reference, depth and graph limits; a manifest's canonical encoding, its claimed
/// length (the publication blob limit), its chunk count and its chunks' lengths. A removed
/// record has no bytes to judge.
fn check_records(
    held: &dyn Held,
    source: &mut dyn ObjectSource,
    facts: &UploadFacts,
) -> Result<(), RdyError> {
    let wanted = |k: &&ObjectKey| matches!(k.kind, kinds::BLOCK | kinds::BLOB_MANIFEST);
    for key in facts.objects.iter().filter(wanted) {
        if held.holds(key)? {
            continue;
        }
        let Some(StreamRecord::Present { payload, .. }) = source.record(*key).map_err(to_rdy)?
        else {
            continue;
        };
        let at = |e: RdyError| at_object(e, *key);
        if key.kind == kinds::BLOCK {
            let b: Block = decode(&payload, *key)?;
            publication::check_block_limits(&b).map_err(at)?;
            continue;
        }
        let m: BlobManifest = decode(&payload, *key)?;
        let malformed = || {
            to_rdy(
                StreamError::bad_record(0, 0, Reason::RecordDoesNotDecode).with_object(Some(*key)),
            )
        };
        if !m.is_encoding_of(&payload) {
            return Err(malformed());
        }
        let l = publication::limits();
        if m.len > l.blob {
            return Err(at(publication::limit("blob", l.blob, "bytes")));
        }
        if !m.chunks_fit() {
            return Err(malformed());
        }
        check_chunk_lengths(source, key.hash, &m)?;
    }
    Ok(())
}

/// The chunk lengths of blob `blob` (manifest `m`), as a blob read keeps them
/// (`rdy_store::check_chunk_len`): each chunk is the bytes still due, at most one chunk's
/// worth, read through the source. A blob with a removed chunk is unavailable to reads, as a
/// removal leaves it, and has no lengths to judge.
fn check_chunk_lengths(
    source: &mut dyn ObjectSource,
    blob: Hash,
    m: &BlobManifest,
) -> Result<(), RdyError> {
    let mut lens = Vec::with_capacity(m.chunks.len());
    for c in &m.chunks {
        let key = ObjectKey {
            kind: kinds::BLOB_CHUNK,
            hash: *c,
        };
        match source.record(key).map_err(to_rdy)? {
            Some(StreamRecord::Present { payload, .. }) => lens.push(payload.len() as u64),
            Some(StreamRecord::Dropped { .. }) => return Ok(()),
            None => return Err(to_rdy(StreamError::unresolved(0, 0, key))),
        }
    }
    let mut left = m.len;
    for len in lens {
        left = rdy_store::check_chunk_len(&blob, left, len).map_err(|e| {
            structural(
                e,
                ObjectKey {
                    kind: kinds::BLOB_MANIFEST,
                    hash: blob,
                },
            )
        })?;
    }
    Ok(())
}

/// The landing rules of every landing of the upload that the receiver does not hold, and the
/// chain rules of the root tag's chain over the source. A landing's fields are judged whether or
/// not its snapshot is shared with another; a held landing was judged when it was taken, so its
/// changes' dependencies are not read again.
fn check_landing_rules(
    held: &dyn Held,
    source: &mut dyn ObjectSource,
    facts: &UploadFacts,
) -> Result<(), RdyError> {
    let in_union = |k: &ObjectKey| facts.objects.binary_search(k).is_ok();
    for key in facts.objects.iter().filter(|k| k.kind == kinds::LANDING) {
        if held.holds(key)? {
            continue;
        }
        let Some(landing) = typed::<LandingRecord>(source, *key)? else {
            continue;
        };
        rdy_store::landing_field_rules(&key.hash, &landing, &mut |kind, h| {
            let k = ObjectKey { kind, hash: *h };
            Ok::<_, Rule>(in_union(&k) || held.holds(&k)?)
        })
        .map_err(|e| e.into_rdy(*key))?;
    }
    let root = &facts.root;
    let head = ObjectKey {
        kind: kinds::LANDING,
        hash: root.landing,
    };
    let chain = check_holder::tag_chain_with(&root.landing, &mut |h| {
        let key = ObjectKey {
            kind: kinds::LANDING,
            hash: *h,
        };
        match typed::<LandingRecord>(source, key) {
            Ok(Some(l)) => Ok(Arc::new(l)),
            Ok(None) => Err(Rule::Other(to_rdy(StreamError::unresolved(0, 0, key)))),
            Err(e) => Err(Rule::Other(e)),
        }
    })
    .map_err(|e| e.into_rdy(head))?;
    let refname = format!("{}{}", rdy_store::holder::TAG_REF, root.name);
    check_holder::chain_rules(&refname, &chain, &mut |l, c| {
        let landing = ObjectKey {
            kind: kinds::LANDING,
            hash: *l,
        };
        if held.holds(&landing)? {
            return Ok(None);
        }
        let key = ObjectKey {
            kind: kinds::CHANGE,
            hash: *c,
        };
        // a removed change has no dependencies to read; its approval's checks cover it
        typed::<Change>(source, key)
            .map(|c| c.map(Arc::new))
            .map_err(Rule::Other)
    })
    .map_err(|e| e.into_rdy(head))
}

/// A structural check's failure: a rule the record breaks (the store's `Decode` text), or any
/// other error, which passes through.
enum Rule {
    Broken(StoreError),
    Other(RdyError),
}

impl From<StoreError> for Rule {
    fn from(e: StoreError) -> Rule {
        match e {
            StoreError::Decode(_) => Rule::Broken(e),
            // a bound of the walk (`holder check exceeds ...`) is the holder's E_LIMIT
            StoreError::Refused(_) => Rule::Other(crate::holder::holder_error(e)),
            e => Rule::Other(e.into()),
        }
    }
}

impl Rule {
    /// The engine error of this failure, a broken rule naming the object `key`.
    fn into_rdy(self, key: ObjectKey) -> RdyError {
        match self {
            Rule::Broken(e) => structural(e, key),
            Rule::Other(e) => e,
        }
    }
}

/// E_BAD_RECORD for the object `key`, which breaks the structural rule `e` names: the holder
/// fsck's own text, refused before anything is taken or selected.
fn structural(e: StoreError, key: ObjectKey) -> RdyError {
    let message = match e {
        StoreError::Decode(m) => m,
        e => e.to_string(),
    };
    to_rdy(StreamError {
        code: "E_BAD_RECORD",
        message,
        fix: STRUCTURE_FIX.into(),
        record: None,
        offset: 0,
        object: Some(key),
    })
}

/// The fix of a structural refusal.
const STRUCTURE_FIX: &str = "upload the history as its source project exported it";

/// `e`, an engine limit error of the record `key`, with the one issue the stream conversion
/// gives: the same code, message and fix, naming the object.
fn at_object(e: RdyError, key: ObjectKey) -> RdyError {
    to_rdy(StreamError {
        code: e.code,
        message: e.message,
        fix: e.fix,
        record: None,
        offset: 0,
        object: Some(key),
    })
}

/// Checks the snapshot of every landing of the upload that the receiver does not hold. A
/// snapshot several landings share is checked once. The blocks of the last snapshot that the
/// next one names are kept for it, so a history whose landings share most blocks reads each
/// shared version about once; nothing older is kept.
///
/// Every block is charged to the snapshot's resource budget (`SnapshotBudget`: selection, text,
/// references, graph) before it is kept, the kept ones from the last snapshot first, so the
/// blocks held at any time are within the Publications limits of one snapshot, plus the one
/// block being read: a snapshot past the limits is refused before it is materialized, and the
/// memory a check holds follows the limits, not the upload.
fn check_landings(
    held: &dyn Held,
    source: &mut dyn ObjectSource,
    facts: &UploadFacts,
) -> Result<(), RdyError> {
    let mut checked: HashSet<Hash> = HashSet::new();
    let mut last: BTreeMap<Hash, Block> = BTreeMap::new();
    for key in facts.objects.iter().filter(|k| k.kind == kinds::LANDING) {
        if held.holds(key)? {
            continue;
        }
        let Some(landing) = typed::<LandingRecord>(source, *key)? else {
            continue;
        };
        if !checked.insert(landing.state) {
            continue;
        }
        let (_, blocks) = snapshot_blocks(source, landing.state, &mut last)?;
        last = blocks;
    }
    Ok(())
}

/// The blocks of snapshot `state`, by version, read through `source` within the Publications
/// limits of one snapshot (see [`check_landings`]): each entry must name a version of its own
/// block, a removed version reads as its approved residue's stub (and as nothing when no residue
/// describes it), every block is charged to the snapshot's budget before it is kept, and the
/// whole snapshot then keeps [`check_snapshot_limits`]. The blocks of `last` that the snapshot
/// names are reused, charged first; `last` is left empty. Returns the snapshot record too.
fn snapshot_blocks(
    source: &mut dyn ObjectSource,
    state: Hash,
    last: &mut BTreeMap<Hash, Block>,
) -> Result<(StateSnapshot, BTreeMap<Hash, Block>), RdyError> {
    let snapshot_key = ObjectKey {
        kind: kinds::SNAPSHOT,
        hash: state,
    };
    let snapshot = typed::<StateSnapshot>(source, snapshot_key)?
        .ok_or_else(|| to_rdy(StreamError::unresolved(0, 0, snapshot_key)))?;
    // the kept blocks are this snapshot's only, charged before any other is read
    let named: HashSet<&Hash> = snapshot.blocks.values().collect();
    last.retain(|v, _| named.contains(v));
    drop(named);
    let mut budget = SnapshotBudget::new();
    for b in last.values() {
        budget.charge(b)?;
    }
    let mut blocks: BTreeMap<Hash, Block> = BTreeMap::new();
    for (id, version) in &snapshot.blocks {
        let (block, charged) = match last.remove(version) {
            Some(b) => (b, true),
            None => match read_block(source, *version)? {
                Some(b) => (b, false),
                // a removed version no residue describes reads as nothing
                None => continue,
            },
        };
        if block.id != *id {
            return Err(foreign_version(&state, id, version, &block.id));
        }
        if !charged {
            budget.charge(&block)?;
        }
        blocks.insert(*version, block);
    }
    last.clear();
    check_snapshot_limits(blocks.values(), |h| manifest(source, *h))?;
    Ok((snapshot, blocks))
}

/// The state of landing `landing`, read through `source` (a take's upload over its holder, with
/// approved removals as their drops) and materialized within the Publications limits of one
/// snapshot: the bounded read [`check_landings`] makes of each new landing, with the same
/// errors. A removed block reads as its approved residue's stub, with its original version
/// hash; one no residue describes reads as nothing.
pub(crate) fn materialize_landing(
    source: &mut dyn ObjectSource,
    landing: Hash,
) -> Result<Materialized, RdyError> {
    let key = ObjectKey {
        kind: kinds::LANDING,
        hash: landing,
    };
    let record = typed::<LandingRecord>(source, key)?
        .ok_or_else(|| to_rdy(StreamError::unresolved(0, 0, key)))?;
    let (snapshot, blocks) = snapshot_blocks(source, record.state, &mut BTreeMap::new())?;
    let by_id = blocks.into_values().map(|b| (b.id.clone(), b)).collect();
    Ok(Materialized::build(snapshot, by_id))
}

// ---------------------------------------------------------------------------------------------
// the publication reads of a take's checked source

/// The settings a publication take resolves its profile with: the holder's pack and the title
/// and description its published state captured.
pub(crate) struct SourceSettings<'a> {
    pub(crate) pack: &'a Pack,
    pub(crate) title: &'a str,
    pub(crate) description: Option<&'a str>,
}

/// A [`PublicationSource`] over a take's checked source (the upload over the holder, approved
/// removals as their drops), borrowed for one resolution, with the holder's captured settings.
/// Nothing of the upload is collected: each read goes to the source, through a `RefCell` that
/// lends its cursor to the trait's shared reads. A blob is masked as the holder will mask it once
/// the take is sealed: by a removal that names its manifest or drops one of its chunks (the
/// holder's own, then one of the upload's applies). The masks come from the holder's index and
/// the upload's request records: no chunk payload is read to report facts.
pub(crate) struct ObjectPublicationSource<'a, 's> {
    source: RefCell<&'a mut (dyn ObjectSource + 's)>,
    held: &'a dyn Held,
    facts: &'a UploadFacts,
    settings: SourceSettings<'a>,
    /// The manifests the upload's own applies name and the chunks their spans drop, to their
    /// requests; read once, when a blob is first asked about.
    marks: RefCell<Option<BTreeMap<ObjectKey, Hash>>>,
}

impl<'a, 's> ObjectPublicationSource<'a, 's> {
    pub(crate) fn new(
        source: &'a mut (dyn ObjectSource + 's),
        held: &'a dyn Held,
        facts: &'a UploadFacts,
        settings: SourceSettings<'a>,
    ) -> ObjectPublicationSource<'a, 's> {
        ObjectPublicationSource {
            source: RefCell::new(source),
            held,
            facts,
            settings,
            marks: RefCell::new(None),
        }
    }

    /// The present record `key`, decoded within its bounds; None when the source does not hold
    /// it intact.
    fn typed<T: serde::de::DeserializeOwned>(&self, key: ObjectKey) -> Result<Option<T>, RdyError> {
        match self.record(key)? {
            Some(StreamRecord::Present { payload, .. }) => decode(&payload, key).map(Some),
            _ => Ok(None),
        }
    }

    /// The record `key` as the source gives it: every read of this source goes through here, so
    /// a chunk payload it returns is counted (a take reads none to report its facts).
    fn record(&self, key: ObjectKey) -> Result<Option<StreamRecord>, RdyError> {
        let record = self.source.borrow_mut().record(key).map_err(to_rdy)?;
        if key.kind == kinds::BLOB_CHUNK && matches!(record, Some(StreamRecord::Present { .. })) {
            crate::publication_take::count_blob_payload_read();
        }
        Ok(record)
    }

    /// The request of the approved apply `apply`.
    fn request_of(&self, apply: Hash) -> Result<Hash, RdyError> {
        let key = ObjectKey {
            kind: kinds::REDACTION_APPLY,
            hash: apply,
        };
        self.typed::<RedactionApply>(key)?
            .map(|a| a.request)
            .ok_or_else(|| to_rdy(StreamError::unresolved(0, 0, key)))
    }

    /// The request that removes or names object `key` once the take is sealed: the holder's,
    /// else the first of the upload's new applies (in key order, as the take masks) whose
    /// request names it. Reads the holder's index and the upload's request records only, never a
    /// chunk payload.
    fn mark(&self, key: ObjectKey) -> Result<Option<Hash>, RdyError> {
        if let Some(request) = self.held.redaction_of(&key)? {
            return Ok(Some(request));
        }
        if self.marks.borrow().is_none() {
            let mut marks = BTreeMap::new();
            let applies = self
                .facts
                .objects
                .iter()
                .filter(|k| k.kind == kinds::REDACTION_APPLY);
            for apply in applies {
                if self.held.holds(apply)? {
                    continue;
                }
                let request = self.request_of(apply.hash)?;
                let request_key = ObjectKey {
                    kind: kinds::REDACTION,
                    hash: request,
                };
                let Some(r) = self.typed::<rdy_model::redaction::Redaction>(request_key)? else {
                    continue;
                };
                // the manifests it names, and the chunks its spans drop (the objects
                // `sync_proof::check` authorizes)
                let manifests = r.manifest.blobs.iter().map(|b| ObjectKey {
                    kind: kinds::BLOB_MANIFEST,
                    hash: *b,
                });
                let chunks = r
                    .manifest
                    .spans
                    .iter()
                    .filter(|s| s.kind == kinds::BLOB_CHUNK)
                    .map(|s| ObjectKey {
                        kind: kinds::BLOB_CHUNK,
                        hash: s.hash,
                    });
                for k in manifests.chain(chunks) {
                    marks.entry(k).or_insert(request);
                }
            }
            *self.marks.borrow_mut() = Some(marks);
        }
        Ok(self
            .marks
            .borrow()
            .as_ref()
            .and_then(|m| m.get(&key).copied()))
    }

    /// The one removal walk of blob `h`: the manifest's mark, then each chunk's. `step` answers
    /// each request and may break the walk. A blob the source lacks has none. A chunk's mark is
    /// the request that drops it ([`Self::mark`]): every dropped chunk of the take fits an
    /// approval, so no chunk record is read.
    fn walk(
        &self,
        h: &Hash,
        step: &mut dyn FnMut(Hash) -> Result<ControlFlow<()>, RdyError>,
    ) -> Result<(), RdyError> {
        let Some(manifest) = PublicationSource::manifest(self, h)? else {
            return Ok(());
        };
        let manifest_key = ObjectKey {
            kind: kinds::BLOB_MANIFEST,
            hash: *h,
        };
        if let Some(request) = self.mark(manifest_key)?
            && step(request)?.is_break()
        {
            return Ok(());
        }
        for c in &manifest.chunks {
            let key = ObjectKey {
                kind: kinds::BLOB_CHUNK,
                hash: *c,
            };
            if let Some(request) = self.mark(key)?
                && step(request)?.is_break()
            {
                break;
            }
        }
        Ok(())
    }
}

impl PublicationSource for ObjectPublicationSource<'_, '_> {
    fn pack(&self) -> &Pack {
        self.settings.pack
    }

    fn title(&self) -> &str {
        self.settings.title
    }

    fn description(&self) -> Option<&str> {
        self.settings.description
    }

    fn manifest(&self, blob: &Hash) -> Result<Option<BlobManifest>, RdyError> {
        self.typed(ObjectKey {
            kind: kinds::BLOB_MANIFEST,
            hash: *blob,
        })
    }

    fn blob_available(&self, blob: &Hash) -> Result<bool, RdyError> {
        if PublicationSource::manifest(self, blob)?.is_none() {
            return Ok(false);
        }
        Ok(self.removed_blob(blob)?.is_none())
    }

    fn blob(&self, blob: &Hash) -> Result<Vec<u8>, RdyError> {
        if let Some(request) = self.removed_blob(blob)? {
            return Err(StoreError::Redacted { request }.into());
        }
        let m = PublicationSource::manifest(self, blob)?.ok_or(StoreError::NotFound(*blob))?;
        let l = publication::limits();
        if m.len > l.blob {
            return Err(publication::limit("blob", l.blob, "bytes"));
        }
        let mut out = Vec::new();
        out.try_reserve_exact(m.len as usize)
            .map_err(|_| to_rdy(StreamError::allocation(None, 0)))?;
        let mut left = m.len;
        for c in &m.chunks {
            let key = ObjectKey {
                kind: kinds::BLOB_CHUNK,
                hash: *c,
            };
            let bytes = match self.record(key)? {
                Some(StreamRecord::Present { payload, .. }) => payload,
                _ => return Err(StoreError::NotFound(*c).into()),
            };
            left = rdy_store::check_chunk_len(blob, left, bytes.len() as u64)?;
            out.extend_from_slice(&bytes);
        }
        Ok(out)
    }

    fn removed_blob(&self, blob: &Hash) -> Result<Option<Hash>, RdyError> {
        let mut first = None;
        self.walk(blob, &mut |request| {
            first = Some(request);
            Ok(ControlFlow::Break(()))
        })?;
        Ok(first)
    }

    fn visit_blob_redactions(
        &self,
        blob: &Hash,
        visit: &mut dyn FnMut(Hash) -> Result<(), RdyError>,
    ) -> Result<(), RdyError> {
        self.walk(blob, &mut |request| {
            visit(request)?;
            Ok(ControlFlow::Continue(()))
        })
    }
}

/// E_BAD_RECORD: snapshot `s` names `version`, a version of another block, for block `id` (the
/// holder's fsck rule, refused before it is taken).
fn foreign_version(s: &Hash, id: &BlockId, version: &Hash, other: &BlockId) -> RdyError {
    structural(
        StoreError::Decode(format!(
            "snapshot {s}: block {id} names version {version}, which is a version of {other}"
        )),
        ObjectKey {
            kind: kinds::SNAPSHOT,
            hash: *s,
        },
    )
}

/// Block version `version` as a read sees it: its record, or for a removed version the stub its
/// residue gives (None when no residue describes it).
fn read_block(source: &mut dyn ObjectSource, version: Hash) -> Result<Option<Block>, RdyError> {
    let key = ObjectKey {
        kind: kinds::BLOCK,
        hash: version,
    };
    match source.record(key).map_err(to_rdy)? {
        Some(StreamRecord::Present { payload, .. }) => decode(&payload, key).map(Some),
        Some(StreamRecord::Dropped {
            apply,
            residue: Some(residue),
            ..
        }) => {
            let apply_key = ObjectKey {
                kind: kinds::REDACTION_APPLY,
                hash: apply,
            };
            let residue_key = ObjectKey {
                kind: kinds::REDACTION_RESIDUE,
                hash: residue,
            };
            let apply = typed::<RedactionApply>(source, apply_key)?
                .ok_or_else(|| to_rdy(StreamError::unresolved(0, 0, apply_key)))?;
            match typed::<RedactionResidue>(source, residue_key)? {
                Some(RedactionResidue::Block(b)) if b.version == version => {
                    Ok(Some(b.stub(apply.request)))
                }
                Some(_) => Err(to_rdy(
                    StreamError::bad_record(0, 0, Reason::InvalidRemovalProof)
                        .with_object(Some(key)),
                )),
                None => Err(to_rdy(StreamError::unresolved(0, 0, residue_key))),
            }
        }
        Some(StreamRecord::Dropped { residue: None, .. }) => Ok(None),
        None => Err(to_rdy(StreamError::unresolved(0, 0, key))),
    }
}

/// Blob manifest `h`, if the source holds it intact.
fn manifest(source: &mut dyn ObjectSource, h: Hash) -> Result<Option<BlobManifest>, RdyError> {
    typed(
        source,
        ObjectKey {
            kind: kinds::BLOB_MANIFEST,
            hash: h,
        },
    )
}

/// The present record `key` of `source`, decoded within its bounds; None when the source does
/// not hold it intact.
fn typed<T: serde::de::DeserializeOwned>(
    source: &mut dyn ObjectSource,
    key: ObjectKey,
) -> Result<Option<T>, RdyError> {
    match source.record(key).map_err(to_rdy)? {
        Some(StreamRecord::Present { payload, .. }) => decode(&payload, key).map(Some),
        _ => Ok(None),
    }
}

fn decode<T: serde::de::DeserializeOwned>(payload: &[u8], key: ObjectKey) -> Result<T, RdyError> {
    bounded::decode(payload)
        .map_err(|e| to_rdy(StreamError::from_record(0, 0, &e).with_object(Some(key))))
}
