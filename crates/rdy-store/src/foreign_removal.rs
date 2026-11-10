//! A foreign removal (repo links, D-CN4QJ; D-3M81W: rdy "drops their bytes and keeps their
//! hashes"): a cited project's portable removal proof erases the matching versions in a citing
//! project's foreign copy, and nothing else.
//!
//! [`Store::take_foreign_removal`] checks the proof by object links alone (`sync_proof::check`,
//! as a holder's take checks one), against the repo the caller claims for it. Then it seals one
//! batch through the apply writer (`Store::seal_apply_batch`): the proof's records (its human
//! landing, approved proposal, carrier change, requests, residues and blob manifests), one
//! FOREIGN `Removal` entry that names the apply and the versions it erases, then the apply. Only
//! once that batch is durable does the erasure run, as a native removal's does.
//!
//! An apply is a foreign removal's proof exactly when a `Removal` entry of its own batch names
//! it ([`split`]): the native physical validation never reads it, since its request's spans name
//! places of the cited log. Each open, load, follow and live seal derives the authority again
//! from the log alone ([`authorize`]):
//!
//! - the proof passes `sync_proof::check` over the log's own records;
//! - each version the entry names is an approved BLOCK object of the proof, whose block residue
//!   has the entry's id;
//! - no local change before the entry produces the version: local content is never erased;
//! - each frame of the version before the entry that a FOREIGN batch wrote (a delivery's frame)
//!   is a span; at least one exists, and each keeps the approved length and, when intact, is
//!   exactly what its residue records.
//!
//! A failure is `RedactionCorrupt` at the apply (a proof that fails), at the entry (a version
//! the proof does not approve, local content, or no foreign frame) or at the frame. The proof's
//! records stay outside canon: the index keeps them as objects only, never as landed changes,
//! and only the versions the entry erases enter the active mask.
use crate::error::StoreError;
use crate::holder::StoreMode;
use crate::index::{IndexEntry, RedactedSpan};
use crate::kinds;
use crate::object_refs::ObjectKey;
use crate::redaction::residual_versions;
use crate::redaction_import::{PROOF_KINDS, copy_fits, proof_error};
use crate::redaction_io::{
    ApplyFrame, Authorization, Collected, FrameLog, RedactionCounts, Residues, SpanAuth,
    for_each_seen,
};
use crate::segment::{Hole, Holes, Location};
use crate::stream_error::StreamError;
use crate::sync_proof::{self, ObjectSource, RemovalObject};
use crate::sync_stream::StreamRecord;
use crate::{Store, codec};
use rdy_model::foreign::{ForeignEntry, ForeignEvent};
use rdy_model::redaction::{MAX_RECORD_BYTES, RedactionResidue};
use rdy_model::{BlockId, Change, Hash, Op, RepoId};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::fs::File;

/// One apply that a FOREIGN `Removal` entry of its own batch names: the apply frame, the
/// entry's place and versions, and where the batch starts.
pub(crate) struct ProofFrame {
    pub(crate) apply: ApplyFrame,
    pub(crate) entry_loc: Location,
    pub(crate) versions: Vec<(BlockId, Hash)>,
    pub(crate) batch_start: Location,
}

/// Moves each apply of one sealed batch (`applies`, whose first frame is at `start`) that a
/// `Removal` entry among the batch's FOREIGN frames (`foreign`: place, raw kind, hash and the
/// payload read) names out of the native applies, into `out` in log order. An entry that does
/// not decode, or that names no apply of its batch, moves nothing; an apply is moved once.
pub(crate) fn split(
    applies: &mut Vec<ApplyFrame>,
    foreign: &[(Location, u8, Hash, Option<Vec<u8>>)],
    start: Location,
    out: &mut Vec<ProofFrame>,
) {
    for (loc, raw, hash, body) in foreign {
        let Some(body) = body.as_deref().filter(|b| Hash::of(b) == *hash) else {
            continue;
        };
        let Ok(entry) = codec::decode_bounded::<ForeignEntry>(*raw, body) else {
            continue;
        };
        let ForeignEvent::Removal { proof, versions } = entry.event else {
            continue;
        };
        if let Some(i) = applies.iter().position(|a| a.hash == proof) {
            out.push(ProofFrame {
                apply: applies.remove(i),
                entry_loc: *loc,
                versions,
                batch_start: start,
            });
        }
    }
    out.sort_by_key(|p| p.apply.loc);
}

/// What one verified foreign removal authorizes: its apply and request, the versions its entry
/// erases with their residues (its objects in the active mask), and its spans.
#[derive(Clone, Debug)]
pub(crate) struct ForeignProven {
    pub(crate) apply_loc: Location,
    pub(crate) apply: Hash,
    pub(crate) request: Hash,
    objects: Vec<(u8, Hash, Option<Hash>)>,
    spans: Vec<RedactedSpan>,
}

/// What a project's verified foreign removals authorize, derived from its log alone.
#[derive(Clone, Debug, Default)]
pub(crate) struct ForeignAuthority {
    /// The verified removals, in log order of their applies.
    pub(crate) proofs: Vec<ForeignProven>,
    /// The places of the proofs' records (each apply excepted) that their own batches hold:
    /// the index keeps each as an object only.
    closure: BTreeSet<Location>,
    /// The requests a proof's landing approves beside the one it applies.
    closure_requests: HashSet<Hash>,
}

impl ForeignAuthority {
    /// Whether the apply frame at `loc` is a verified foreign removal's.
    pub(crate) fn holds_apply(&self, loc: Location) -> bool {
        self.proofs
            .binary_search_by_key(&loc, |p| p.apply_loc)
            .is_ok()
    }

    /// The request the verified foreign removal whose apply frame is at `loc` applies, if one
    /// is.
    pub(crate) fn request_of_apply(&self, loc: Location) -> Option<Hash> {
        self.proofs
            .binary_search_by_key(&loc, |p| p.apply_loc)
            .ok()
            .map(|i| self.proofs[i].request)
    }

    /// The index entry of a request frame whose own entry is `native`, when a foreign removal
    /// reads it: the request a proof applies keeps the versions the entry erases and the spans
    /// this log derives, never the cited log's places; a request the landing approves beside it
    /// keeps nothing. None for any other request.
    pub(crate) fn request_entry(&self, native: &IndexEntry) -> Option<IndexEntry> {
        let IndexEntry::RedactionRequest {
            request, carrier, ..
        } = native
        else {
            return None;
        };
        if let Some(p) = self.proofs.iter().find(|p| p.request == *request) {
            return Some(IndexEntry::RedactionRequest {
                request: *request,
                carrier: *carrier,
                objects: p.objects.clone(),
                spans: p.spans.clone(),
            });
        }
        self.closure_requests
            .contains(request)
            .then(|| IndexEntry::RedactionRequest {
                request: *request,
                carrier: *carrier,
                objects: vec![],
                spans: vec![],
            })
    }

    /// Whether the frame at `loc` (raw kind `kind`) is a proof's record other than a request
    /// or an apply.
    pub(crate) fn is_proof_record(&self, kind: u8, loc: Location) -> bool {
        let kind = codec::logical_kind(kind);
        self.closure.contains(&loc) && kind != kinds::REDACTION && kind != kinds::REDACTION_APPLY
    }

    /// The index entries of the frame at `loc` (raw kind `kind`, hash `hash`) when it is a
    /// proof's record other than a request or an apply: the object alone, so no foreign landing
    /// or change becomes local history. None for any other frame.
    pub(crate) fn frame_entries(
        &self,
        kind: u8,
        hash: Hash,
        loc: Location,
    ) -> Option<Vec<IndexEntry>> {
        self.is_proof_record(kind, loc).then(|| {
            vec![IndexEntry::Object {
                kind: codec::logical_kind(kind),
                hash,
                loc,
            }]
        })
    }
}

/// The holes the foreign removals before `below` would authorize, unchecked: every BLOCK frame
/// of a version an entry names. The marker rule's scan reads them as frames while the open
/// decides which batches the acknowledged log holds.
pub(crate) fn candidate_holes(
    c: &Collected,
    readers: &[File],
    below: Location,
    holes: &mut Holes,
) -> Result<(), StoreError> {
    let named: HashSet<Hash> = c
        .foreign_proofs
        .iter()
        .filter(|p| p.apply.loc < below)
        .flat_map(|p| p.versions.iter().map(|(_, v)| *v))
        .collect();
    if named.is_empty() {
        return Ok(());
    }
    for_each_seen(readers, below, &BTreeMap::new(), |f| {
        if codec::logical_kind(f.kind) == kinds::BLOCK && named.contains(&f.hash) {
            holes.entry(f.loc).or_insert(Hole {
                kind: kinds::BLOCK,
                hash: f.hash,
                len: f.len,
            });
        }
        Ok(())
    })
}

/// The log's records as a proof source: the first frame of each proof kind and hash (before
/// the walk's end, then a batch not yet written), served when its payload is intact. It notes
/// every key it serves.
struct LogRecords<'a> {
    log: &'a mut dyn FrameLog,
    first: &'a HashMap<ObjectKey, (Location, u8, u32)>,
    served: BTreeSet<ObjectKey>,
}

impl ObjectSource for LogRecords<'_> {
    fn record(&mut self, key: ObjectKey) -> Result<Option<StreamRecord>, StreamError> {
        let Some((loc, raw, len)) = self.first.get(&key).copied() else {
            return Ok(None);
        };
        if len as usize > MAX_RECORD_BYTES {
            return Ok(None);
        }
        let payload = self
            .log
            .record(loc, raw, &key.hash)
            .map_err(|e| StreamError::io(None, 0, &std::io::Error::other(e.to_string())))?;
        let Some(payload) = payload else {
            return Ok(None);
        };
        let Ok(o) = codec::split(raw, &payload) else {
            return Ok(None);
        };
        let encoding = o.version;
        self.served.insert(key);
        Ok(Some(StreamRecord::Present {
            key,
            encoding,
            payload,
        }))
    }
}

/// The residues the approved BLOCK objects of `objects` name, read from `source` and decoded.
/// One the source does not hold, or that does not decode, is left out.
fn block_residues(
    source: &mut dyn ObjectSource,
    objects: &BTreeMap<ObjectKey, RemovalObject>,
) -> Result<Residues, StreamError> {
    let mut out: Residues = HashMap::new();
    for (k, t) in objects {
        let Some(r) = t.residue.filter(|_| k.kind == kinds::BLOCK) else {
            continue;
        };
        if out.contains_key(&r) {
            continue;
        }
        let key = ObjectKey {
            kind: kinds::REDACTION_RESIDUE,
            hash: r,
        };
        if let Some(rec @ StreamRecord::Present { .. }) = source.record(key)? {
            let StreamRecord::Present { payload, .. } = &rec else {
                unreachable!("a present record")
            };
            let raw = rec.raw_kind();
            if let (Ok(res), Ok(o)) = (
                codec::decode_bounded::<RedactionResidue>(raw, payload),
                codec::split(raw, payload),
            ) {
                out.insert(r, (res, o.version));
            }
        }
    }
    Ok(out)
}

/// The id the approved object of `version` in `objects` has: its block residue's id, when that
/// residue (in `residues`) is a block residue of `version`. None when the proof does not approve
/// the version.
fn approved_id(
    objects: &BTreeMap<ObjectKey, RemovalObject>,
    residues: &Residues,
    version: &Hash,
) -> Option<(BlockId, RemovalObject)> {
    let t = objects.get(&ObjectKey {
        kind: kinds::BLOCK,
        hash: *version,
    })?;
    match residues.get(&t.residue?) {
        Some((RedactionResidue::Block(b), _)) if b.version == *version => Some((b.id.clone(), *t)),
        _ => None,
    }
}

/// One BLOCK frame of a named version: its place, raw kind, payload length and batch.
struct BlockFrame {
    loc: Location,
    raw: u8,
    hash: Hash,
    len: u32,
    batch: usize,
}

/// Adds what the foreign removals of `c` authorize to `auth` (see the module docs): those whose
/// apply lies before `upto`, or in a batch not yet written (`c.memory`). The walk reads `log`
/// before `upto`, then that batch. A failure is `RedactionCorrupt`.
pub(crate) fn authorize(
    c: &Collected,
    log: &mut dyn FrameLog,
    auth: &mut Authorization,
    upto: Location,
) -> Result<(), StoreError> {
    let proofs: Vec<&ProofFrame> = c
        .foreign_proofs
        .iter()
        .filter(|p| p.apply.loc < upto || c.memory.contains_key(&p.apply.loc))
        .collect();
    if proofs.is_empty() {
        return Ok(());
    }
    let named: HashSet<Hash> = proofs
        .iter()
        .flat_map(|p| p.versions.iter().map(|(_, v)| *v))
        .collect();
    // one walk: the first frame of each proof key, the frames of the named versions with their
    // batches, whether each batch holds a FOREIGN entry, and the first local producer of each
    // named version
    let mut first: HashMap<ObjectKey, (Location, u8, u32)> = HashMap::new();
    let mut blocks: Vec<BlockFrame> = vec![];
    let mut foreign_batch: Vec<bool> = vec![];
    let mut in_foreign = false;
    let mut producers: HashMap<Hash, Location> = HashMap::new();
    let spans = &auth.spans;
    let residues = &auth.residues;
    log.walk(upto, &c.memory, &mut |f| {
        if f.kind == kinds::BATCH_COMMIT {
            foreign_batch.push(std::mem::take(&mut in_foreign));
            return Ok(());
        }
        let kind = codec::logical_kind(f.kind);
        if PROOF_KINDS.contains(&kind) {
            first
                .try_reserve(1)
                .map_err(|_| crate::redaction_io::out_of_memory())?;
            first
                .entry(ObjectKey { kind, hash: f.hash })
                .or_insert((f.loc, f.kind, f.len));
        }
        match kind {
            kinds::FOREIGN => in_foreign = true,
            kinds::BLOCK if named.contains(&f.hash) => blocks.push(BlockFrame {
                loc: f.loc,
                raw: f.kind,
                hash: f.hash,
                len: f.len,
                batch: foreign_batch.len(),
            }),
            kinds::CHANGE => {
                let mut produced = |v: &Hash| {
                    if named.contains(v) {
                        producers.entry(*v).or_insert(f.loc);
                    }
                };
                let intact = if f.len as usize <= MAX_RECORD_BYTES {
                    f.intact()?
                } else {
                    None
                };
                match intact {
                    Some(p) => {
                        if let Ok(change) = codec::decode_record::<Change>(f.kind, &p) {
                            for op in &change.ops {
                                if let Op::Create { block } | Op::Update { block, .. } = op {
                                    produced(&block.version_hash());
                                }
                            }
                        }
                    }
                    // a removed local change keeps its versions in its change residue
                    None => {
                        if let Some(RedactionResidue::Change(ch)) = spans
                            .get(&f.loc)
                            .and_then(|s| s.residue)
                            .and_then(|r| residues.get(&r))
                        {
                            residual_versions(ch).for_each(&mut produced);
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    foreign_batch.push(in_foreign);
    let mut source = LogRecords {
        log: &mut *log,
        first: &first,
        served: BTreeSet::new(),
    };
    let mut found: Vec<ForeignProven> = Vec::with_capacity(proofs.len());
    let mut closure: BTreeSet<Location> = BTreeSet::new();
    let mut closure_requests: HashSet<Hash> = HashSet::new();
    let mut add: Vec<(Location, Hole, SpanAuth)> = vec![];
    let mut keep: Residues = HashMap::new();
    for p in &proofs {
        let at_apply = || StoreError::corrupt_at(p.apply.loc);
        let at_entry = || StoreError::corrupt_at(p.entry_loc);
        p.apply.record.as_ref().ok_or_else(at_apply)?;
        source.served.clear();
        let removal = sync_proof::check(&mut source, p.apply.hash)
            .map_err(|e| proof_error(e, p.apply.loc))?;
        let res = block_residues(&mut source, &removal.objects)
            .map_err(|e| proof_error(e, p.apply.loc))?;
        // the proof's records in its own batch, each apply excepted
        for k in source
            .served
            .iter()
            .filter(|k| !removal.objects.contains_key(k))
        {
            if let Some((loc, _, _)) = first.get(k)
                && *loc >= p.batch_start
                && *loc < p.apply.loc
            {
                closure.insert(*loc);
            }
            if k.kind == kinds::REDACTION && k.hash != removal.request {
                closure_requests.insert(k.hash);
            }
        }
        let mut objects = vec![];
        let mut spans = vec![];
        for (id, v) in &p.versions {
            let Some((approved, tuple)) = approved_id(&removal.objects, &res, v) else {
                return Err(at_entry());
            };
            if approved != *id || producers.get(v).is_some_and(|l| *l < p.entry_loc) {
                return Err(at_entry());
            }
            let r = tuple.residue.ok_or_else(at_entry)?;
            let mut any = false;
            for b in blocks
                .iter()
                .filter(|b| b.hash == *v && b.loc < p.entry_loc && foreign_batch[b.batch])
            {
                any = true;
                let payload = match c.memory.get(&b.loc) {
                    Some((_, p)) => p.clone(),
                    None => source.log.payload(b.loc, b.len)?,
                };
                let intact = (Hash::of(&payload) == *v).then_some(payload);
                if !copy_fits(b.raw, b.len, intact.as_deref(), &tuple, &res) {
                    return Err(StoreError::corrupt_at(b.loc));
                }
                // a frame a native apply, or an earlier foreign removal, holds keeps its proof
                if auth.holes.contains_key(&b.loc) || add.iter().any(|(l, _, _)| *l == b.loc) {
                    continue;
                }
                spans.push(RedactedSpan {
                    loc: b.loc,
                    kind: kinds::BLOCK,
                    hash: *v,
                    len: b.len,
                    residue: Some(r),
                });
                keep.entry(r).or_insert_with(|| res[&r].clone());
                add.push((
                    b.loc,
                    Hole {
                        kind: kinds::BLOCK,
                        hash: *v,
                        len: b.len,
                    },
                    SpanAuth {
                        residue: Some(r),
                        apply: p.apply.hash,
                        request: removal.request,
                        carried: false,
                    },
                ));
            }
            if !any {
                return Err(at_entry());
            }
            objects.push((kinds::BLOCK, *v, Some(r)));
        }
        found.push(ForeignProven {
            apply_loc: p.apply.loc,
            apply: p.apply.hash,
            request: removal.request,
            objects,
            spans,
        });
    }
    for (loc, hole, span) in add {
        auth.holes.insert(loc, hole);
        auth.spans.insert(loc, span);
    }
    auth.keep_residues(keep)?;
    auth.foreign = ForeignAuthority {
        proofs: found,
        closure,
        closure_requests,
    };
    Ok(())
}

/// A stored foreign removal proof as fsck checks it again: the `(id, version)` pairs it
/// approves (its BLOCK objects with their block residues' ids), and the keys of the proof's
/// records the check read.
pub(crate) struct StoredProof {
    pub(crate) approved: BTreeSet<(BlockId, Hash)>,
    pub(crate) keys: BTreeSet<ObjectKey>,
}

/// A proof source that notes each key it serves present.
struct Noted<'a> {
    inner: &'a mut dyn ObjectSource,
    keys: BTreeSet<ObjectKey>,
}

impl ObjectSource for Noted<'_> {
    fn record(&mut self, key: ObjectKey) -> Result<Option<StreamRecord>, StreamError> {
        let r = self.inner.record(key)?;
        if matches!(r, Some(StreamRecord::Present { .. })) {
            self.keys.insert(key);
        }
        Ok(r)
    }
}

/// Checks the stored proof of apply `apply` again over the store's own records
/// (`sync_proof::check` through `closure::StoreSource`), for fsck.
pub(crate) fn stored_proof(
    reader: &crate::StoreReader,
    apply: Hash,
) -> Result<StoredProof, StreamError> {
    let mut store = crate::closure::StoreSource {
        reader,
        end: crate::closure::END_OF_LOG,
    };
    let mut noted = Noted {
        inner: &mut store,
        keys: BTreeSet::new(),
    };
    let removal = sync_proof::check(&mut noted, apply)?;
    let res = block_residues(&mut noted, &removal.objects)?;
    let approved = removal
        .objects
        .keys()
        .filter(|k| k.kind == kinds::BLOCK)
        .filter_map(|k| approved_id(&removal.objects, &res, &k.hash).map(|(id, _)| (id, k.hash)))
        .collect();
    let keys = noted
        .keys
        .into_iter()
        .filter(|k| !removal.objects.contains_key(k))
        .collect();
    Ok(StoredProof { approved, keys })
}

/// The cited project's proof of a foreign removal: the repo it claims, its apply and where its
/// records are read (a stream's records, or a store's).
pub struct ForeignProof<'a> {
    pub repo: RepoId,
    pub apply: Hash,
    pub source: &'a mut dyn ObjectSource,
}

/// What [`Store::take_foreign_removal`] did with each version the proof approves, each list in
/// `(id, version)` order: `erased` (the `Removal` entry names them), `kept_local` (local content
/// of the same version; never erased), `skipped` (no delivery of the repo holds it, or no BLOCK
/// is stored) and `already_removed` (an earlier removal erased it). `entry` is the new entry,
/// None when nothing was written; `counts` are the active redaction's after the take (zero when
/// nothing was written).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignRemovalTaken {
    pub entry: Option<Hash>,
    pub erased: Vec<(BlockId, Hash)>,
    pub kept_local: Vec<(BlockId, Hash)>,
    pub skipped: Vec<(BlockId, Hash)>,
    pub already_removed: Vec<(BlockId, Hash)>,
    pub counts: RedactionCounts,
}

/// Why a foreign removal was not taken: the store refused it, the proof failed
/// (`E_BAD_RECORD` `invalid removal proof`, or `E_UNRESOLVED_REF` for a record its source lacks),
/// or the proof is another repo's.
#[derive(Debug)]
pub enum ForeignRemovalError {
    Store(StoreError),
    Proof(StreamError),
    RepoMismatch { source: RepoId, target: RepoId },
}

impl fmt::Display for ForeignRemovalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ForeignRemovalError::Store(e) => write!(f, "{e}"),
            ForeignRemovalError::Proof(e) => write!(f, "{e}"),
            ForeignRemovalError::RepoMismatch { source, target } => {
                write!(f, "stream repo {source} does not match repo {target}")
            }
        }
    }
}

impl std::error::Error for ForeignRemovalError {}

impl From<StoreError> for ForeignRemovalError {
    fn from(e: StoreError) -> Self {
        ForeignRemovalError::Store(e)
    }
}

/// A proof source that keeps a copy of each present proof record it serves.
struct Kept<'a> {
    inner: &'a mut dyn ObjectSource,
    records: BTreeMap<ObjectKey, StreamRecord>,
}

impl ObjectSource for Kept<'_> {
    fn record(&mut self, key: ObjectKey) -> Result<Option<StreamRecord>, StreamError> {
        let r = self.inner.record(key)?;
        if let Some(rec @ StreamRecord::Present { .. }) = &r
            && PROOF_KINDS.contains(&key.kind)
        {
            self.records.insert(key, rec.clone());
        }
        Ok(r)
    }
}

/// The order the proof's records take in the removal batch, by logical kind: what each names
/// comes first.
const RECORD_ORDER: [u8; 6] = [
    kinds::REDACTION_RESIDUE,
    kinds::BLOB_MANIFEST,
    kinds::CHANGE,
    kinds::PROPOSAL,
    kinds::REDACTION,
    kinds::LANDING,
];

impl Store {
    /// Takes the cited project's removal proof into this project's foreign copy of `repo`
    /// (D-CN4QJ). Checked first, and a refusal writes nothing:
    /// - the store is a project (`Refused`), the proof's repo is `repo` (`RepoMismatch`), and
    ///   `repo` is not the store's own (`Refused`);
    /// - the proof passes `sync_proof::check` over `proof.source` (`Proof`).
    ///
    /// Each version the proof approves is then erased when a delivery of `repo` (any source,
    /// live or revoked) holds it, no local change produces it and its BLOCK is stored intact;
    /// the rest are reported (see [`ForeignRemovalTaken`]). When some version is erased, one
    /// batch is sealed through the apply writer (the proof's records, one `Removal` entry naming
    /// the apply and the erased versions, then the apply), and the erasure runs after its sync
    /// (`Store::seal_apply_batch`: a failure after the sync is `RedactionPending`, and the next
    /// open finishes it). A proof already taken writes nothing again.
    pub fn take_foreign_removal(
        &mut self,
        repo: &RepoId,
        proof: ForeignProof<'_>,
    ) -> Result<ForeignRemovalTaken, ForeignRemovalError> {
        self.ensure_writable()?;
        if self.mode()? != StoreMode::Project {
            return Err(StoreError::Refused("a holder store holds no foreign copy".into()).into());
        }
        if proof.repo != *repo {
            return Err(ForeignRemovalError::RepoMismatch {
                source: proof.repo,
                target: *repo,
            });
        }
        if self.repo_id()? == Some(*repo) {
            return Err(StoreError::Refused(format!(
                "foreign repo {repo} is this store's own repo"
            ))
            .into());
        }
        let mut kept = Kept {
            inner: proof.source,
            records: BTreeMap::new(),
        };
        let removal =
            sync_proof::check(&mut kept, proof.apply).map_err(ForeignRemovalError::Proof)?;
        let res =
            block_residues(&mut kept, &removal.objects).map_err(ForeignRemovalError::Proof)?;
        let mut approved: Vec<(BlockId, Hash)> = removal
            .objects
            .keys()
            .filter(|k| k.kind == kinds::BLOCK)
            .filter_map(|k| {
                approved_id(&removal.objects, &res, &k.hash).map(|(id, _)| (id, k.hash))
            })
            .collect();
        approved.sort();
        // what the deliveries of `repo` hold, the source of its first chain, and the removals
        // that took this proof before
        let mut held: BTreeSet<(BlockId, Hash)> = BTreeSet::new();
        let mut earlier: Vec<(BlockId, Hash)> = vec![];
        let reader = self.reader();
        reader.for_each_object_of(kinds::FOREIGN, |h| {
            let e = reader.foreign_entry(&h)?;
            if e.repo != *repo {
                return Ok(());
            }
            match e.event {
                ForeignEvent::Delivery { added, changed, .. } => {
                    held.extend(added.into_iter().chain(changed).map(|v| (v.id, v.version)));
                }
                ForeignEvent::Removal { proof: p, versions } if p == proof.apply => {
                    earlier.extend(versions);
                }
                _ => {}
            }
            Ok(())
        })?;
        let mut taken = ForeignRemovalTaken {
            entry: None,
            erased: vec![],
            kept_local: vec![],
            skipped: vec![],
            already_removed: vec![],
            counts: RedactionCounts::default(),
        };
        let applied = reader.index().redaction_applied(&removal.request)?;
        if applied.is_some_and(|(a, _)| a == proof.apply) {
            // taken before: the versions its entries erased, and nothing written
            earlier.sort();
            earlier.dedup();
            taken.already_removed = earlier;
            return Ok(taken);
        }
        if applied.is_some() || reader.has_kind(kinds::REDACTION_APPLY, &proof.apply)? {
            return Err(StoreError::Refused(format!(
                "removal apply {} is stored here already, not as a foreign removal",
                proof.apply
            ))
            .into());
        }
        for (id, v) in approved {
            if !held.contains(&(id.clone(), v)) {
                taken.skipped.push((id, v));
            } else if !reader.producers(&v)?.is_empty() {
                taken.kept_local.push((id, v));
            } else {
                match reader.get_kind_raw(kinds::BLOCK, &v) {
                    Ok(_) => taken.erased.push((id, v)),
                    Err(StoreError::Redacted { .. }) => taken.already_removed.push((id, v)),
                    Err(StoreError::NotFound(_)) => taken.skipped.push((id, v)),
                    Err(e) => return Err(e.into()),
                }
            }
        }
        if taken.erased.is_empty() {
            return Ok(taken);
        }
        let source = reader
            .foreign_sources()?
            .into_iter()
            .find_map(|(_, head)| {
                reader
                    .foreign_entry(&head)
                    .ok()
                    .filter(|e| e.repo == *repo)
                    .map(|e| e.source)
            })
            .ok_or_else(|| StoreError::Refused(format!("no source delivers repo {repo}")))?;
        drop(reader);
        let entry = ForeignEntry {
            repo: *repo,
            source,
            event: ForeignEvent::Removal {
                proof: proof.apply,
                versions: taken.erased.clone(),
            },
        };
        let item = codec::item_new(kinds::FOREIGN, &entry);
        if let Err(e) = codec::decode_bounded::<ForeignEntry>(item.0, &item.1) {
            return Err(StoreError::Refused(format!(
                "the foreign entry exceeds a record bound: {e}"
            ))
            .into());
        }
        let mut items: Vec<(u8, Vec<u8>)> = vec![];
        for kind in RECORD_ORDER {
            for (k, r) in kept.records.range(
                ObjectKey {
                    kind,
                    hash: Hash([0; 32]),
                }..=ObjectKey {
                    kind,
                    hash: Hash([0xff; 32]),
                },
            ) {
                if removal.objects.contains_key(k) {
                    continue;
                }
                if let StreamRecord::Present { payload, .. } = r {
                    items.push((r.raw_kind(), payload.clone()));
                }
            }
        }
        taken.entry = Some(Hash::of(&item.1));
        items.push(item);
        let apply = kept
            .records
            .get(&ObjectKey {
                kind: kinds::REDACTION_APPLY,
                hash: proof.apply,
            })
            .ok_or_else(|| StoreError::Refused("the proof's apply was not read".into()))?;
        if let StreamRecord::Present { payload, .. } = apply {
            items.push((apply.raw_kind(), payload.clone()));
        }
        taken.counts = self.seal_apply_batch(items)?;
        Ok(taken)
    }
}
