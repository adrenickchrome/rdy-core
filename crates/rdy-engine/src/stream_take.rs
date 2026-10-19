//! The take of one upload into a holder (D-D2NPN "verify-on-receive", D-BPYN8, D-CC90E; hub
//! p-97JVE, f-NE0Z4, r-DR5Z9, r-M1GA9). [`take`] hands every batch file of the upload to the
//! store's one take (`Store::take_batches`), which checks the union of the files over the holder
//! and writes the closure, its approved removals and the tag in one sealed batch. The engine adds
//! the checks that need its rules, in the store's validation callback, under the writer:
//!
//! 1. The root tag's name is a version by the existing parser (`parse_version`: one optional
//!    leading `v`, no `+build` metadata). A held tag of the same version (SemVer precedence, so
//!    `v1.2.0` equals `1.2.0`) at the same landing survives with its original name, hash and
//!    metadata (`TagChoice::Existing`); at another landing it is `E_TAG_EXISTS`, before anything
//!    is appended. The imported version need not exceed the held ones. Two held tags of that
//!    version are damage: the take returns fsck's tag report instead of picking one.
//! 2. Every block and blob manifest the holder lacks keeps the local limits the batch check
//!    keeps for one record (`stream_check::check_batch`), so a take keeps no record that a batch
//!    check refuses. Each such blob keeps the chunk lengths a blob read keeps, over the union
//!    and the holder; a blob with a removed chunk, or a removed manifest, has none to judge.
//! 3. Every landing of the upload that the holder does not hold keeps the landing rules the
//!    holder's fsck keeps (`rdy_store::landing_field_rules`: relay, evidence and the objects it
//!    names), and the root tag's chain keeps the chain rules (`check_holder::chain_rules`: each
//!    change lands once, after its dependencies). These are structural rules, not the current
//!    gate policy.
//! 4. Every landing of the upload that the holder does not hold has its snapshot read from the
//!    union and the holder: each entry must name a version of its own block, and the whole
//!    snapshot must keep the Publications resource limits (`check_snapshot_limits`). A removed
//!    block reads as its stub. No block is checked against a pack, and no approval is judged by
//!    the current gate: a block of a type no pack knows is taken when these checks pass.
//!
//! A record that breaks a structural rule is `E_BAD_RECORD` with the holder fsck's own text, so
//! no take acknowledges an upload that the holder's fsck would then refuse.
//!
//! The closure, the prerequisites, the removal proofs and the upload target are the store's
//! checks (`rdy_store::take_batch`), not repeated here. After a take that wrote something, the
//! holder's read state is published again from the store (a full publish; a holder logs no
//! moves, so no other batch is written). A take that sealed a removal but could not finish its
//! erasure returns `E_REDACTION_PENDING` and the reopen finishes it, as a native removal does.
//! A caller whose reply was lost repairs from the holder's tags and object-key presence: the
//! take keeps no receipt, transaction id or timer.
use crate::error::{E_BAD_INPUT, E_REDACTION_STALE, E_REPO_MISMATCH, E_TAG_EXISTS, RdyError};
use crate::publication::{self, check_snapshot_limits};
use crate::stream_error::{Reason, StreamError, to_rdy};
use crate::version::parse_version;
use crate::{Delta, Rdy};
use rdy_model::redaction::{RedactionApply, RedactionResidue};
use rdy_model::{Block, BlockId, Change, Hash, LandingRecord, StateSnapshot, Tag};
use rdy_schema::{Issue, Severity};
use rdy_store::bounded;
use rdy_store::check_holder;
use rdy_store::kinds::{self, BlobManifest};
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::StreamRecord;
use rdy_store::take_batch::{
    BatchTaken, TagChoice, TakeError, UploadFacts, UploadTarget, check_input_bounds,
};
use rdy_store::{StoreError, StoreReader};
use serde::{Serialize, Serializer};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::sync::Arc;

/// Where the holder's log ends after a take.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct StorePosition {
    pub segment: u32,
    pub offset: u64,
}

/// What one take did. The counts are the store's: the objects it wrote that the holder lacked,
/// its data frames (the marker not counted) and the objects it removed. `tag` and `tag_hash`
/// name the tag that survives (the held one when the same version already named the landing),
/// `landing` its landing. A holder has no canon: `canon` is always None. `unchanged` is true
/// when the take appended no frame and no marker (a complete retry).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TakeOutcome {
    pub records_taken: u64,
    pub frames_written: u64,
    pub redacted_objects: u64,
    pub tag: String,
    #[serde(serialize_with = "hex")]
    pub tag_hash: Hash,
    #[serde(serialize_with = "hex")]
    pub landing: Hash,
    #[serde(serialize_with = "hex_opt")]
    pub canon: Option<Hash>,
    pub end: StorePosition,
    pub unchanged: bool,
}

fn hex<S: Serializer>(h: &Hash, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(h)
}

fn hex_opt<S: Serializer>(h: &Option<Hash>, s: S) -> Result<S::Ok, S::Error> {
    match h {
        Some(h) => s.collect_str(h),
        None => s.serialize_none(),
    }
}

/// A tag the holder holds, by its ref: its parsed version, its record and its hash.
struct Held {
    version: semver::Version,
    hash: Hash,
    tag: Tag,
}

/// Takes the batch files `batches` of one upload for the publication `target` into the holder
/// `rdy` (see the module docs). The files may come in any order and repeat each other; together
/// they must hold the root tag's whole closure less what the holder holds. Every refusal before
/// the append leaves the holder unchanged.
pub fn take(
    rdy: &mut Rdy,
    batches: &[File],
    target: UploadTarget,
) -> Result<TakeOutcome, RdyError> {
    check_input_bounds(batches.len(), &target).map_err(to_rdy)?;
    // a project is refused before anything of it is sealed (the store refuses it too)
    if !rdy.is_holder() {
        return Err(take_error(TakeError::ModeMismatch));
    }
    // a take is a batch of its own: one a raw write left open is sealed and published first
    rdy.seal_open()?;
    // the writer is held for the whole call, so the tags read here are the ones the take sees
    let held = held_tags(&rdy.store)?;
    let damage = if same_version_pair(&held) {
        rdy.check_holder_tags().err()
    } else {
        None
    };
    let epoch = rdy.store.redaction_epoch();
    let reader = rdy.store.reader();
    let (held, damage) = (&held, &damage);
    // the writer's held batches are off for the take, which seals its own batch, as for the
    // store's other self-sealing writes
    rdy.store.set_held(false)?;
    let taken = rdy
        .store
        .take_batches(batches, &target, move |source, facts| {
            validate(&reader, source, facts, held, damage.as_ref())
        });
    rdy.store.set_held(true)?;
    match taken {
        Ok(t) => {
            if t.frames > 0 {
                republish(rdy, epoch)?;
            }
            Ok(outcome(t))
        }
        Err(e) => {
            if matches!(e, TakeError::Store(StoreError::RedactionPending)) {
                // the removal is sealed and its erasure unfinished: no state built before it is
                // served again (the reopen finishes the erasure); the take's error is returned
                let _ = republish(rdy, epoch);
            }
            Err(take_error(e))
        }
    }
}

/// Publishes the holder's state again from its store after a take wrote a batch. A take that
/// advanced the redaction epoch (or sealed a removal it could not finish) starts the shared
/// caches over, as a native removal does, so no read of the new state reuses an old build.
fn republish(rdy: &mut Rdy, epoch: u64) -> Result<(), RdyError> {
    let pending = rdy.store.reader().check_redaction_epoch().is_err();
    if pending || rdy.store.redaction_epoch() != epoch {
        rdy.renew_epoch_caches();
        rdy.logged = None;
    }
    rdy.publish(Delta::Full)
}

fn outcome(t: BatchTaken) -> TakeOutcome {
    TakeOutcome {
        records_taken: t.new_objects,
        frames_written: t.frames,
        redacted_objects: t.redacted_objects,
        tag: t.tag.name.clone(),
        tag_hash: t.tag_hash,
        landing: t.tag.landing,
        canon: None,
        end: StorePosition {
            segment: t.end.segment,
            offset: t.end.offset,
        },
        unchanged: t.frames == 0,
    }
}

/// A take's error as the engine's: the stream conversion for a stream refusal, the existing
/// mapping for a store error, the validation's own error, and the take's three structured
/// refusals.
fn take_error(e: TakeError<RdyError>) -> RdyError {
    match e {
        TakeError::Store(e) => e.into(),
        TakeError::Stream(e) => to_rdy(e),
        TakeError::Validation(e) => e,
        TakeError::UncoveredChange(h) => RdyError::new(
            E_REDACTION_STALE,
            format!("the imported removal does not cover receiver change {h}"),
            format!(
                "redact this content in the project that made change {h}, upload that removal, then upload again"
            ),
        ),
        TakeError::ModeMismatch => RdyError::new(
            E_BAD_INPUT,
            "this store is a project",
            "open it through the project adapter",
        ),
        TakeError::RepoMismatch { source, target } => RdyError::new(
            E_REPO_MISMATCH,
            format!("stream repo {source} does not match holder repo {target}"),
            "use the holder for the stream's repo",
        ),
    }
}

/// Every `tag/` ref whose record names a version, read as the holder's fsck reads them. A ref
/// that does not read as a version tag is fsck's to report; it names no version here.
fn held_tags(reader: &StoreReader) -> Result<Vec<Held>, RdyError> {
    let mut out = Vec::new();
    for (_, value) in reader.refs_with_prefix(rdy_store::holder::TAG_REF)? {
        let Ok(bytes) = <[u8; 32]>::try_from(value.as_slice()) else {
            continue;
        };
        let hash = Hash(bytes);
        let Ok(tag) = reader.read_tag(&hash) else {
            continue;
        };
        if let Ok(version) = parse_version(&tag.name) {
            out.push(Held { version, hash, tag });
        }
    }
    Ok(out)
}

/// Whether two held tags denote the same version.
fn same_version_pair(held: &[Held]) -> bool {
    let mut versions: Vec<&semver::Version> = held.iter().map(|h| &h.version).collect();
    versions.sort_by(|a, b| a.cmp_precedence(b));
    versions
        .windows(2)
        .any(|w| w[0].cmp_precedence(w[1]) == Ordering::Equal)
}

/// The store take's validation callback: the tag rule, each new record's local limits, then
/// every new landing's snapshot.
fn validate(
    reader: &StoreReader,
    source: &mut dyn ObjectSource,
    facts: &UploadFacts,
    held: &[Held],
    damage: Option<&RdyError>,
) -> Result<TagChoice, RdyError> {
    let choice = choose_tag(&facts.root, held, damage)?;
    check_records(reader, source, facts)?;
    check_landing_rules(reader, source, facts)?;
    check_landings(reader, source, facts)?;
    Ok(choice)
}

/// The local limits of every block and blob manifest of the upload that the holder does not
/// hold, as the batch check keeps them for each record (`stream_check::check_batch`), so a take
/// never keeps a record a batch check refuses: a block's own text, reference, depth and graph
/// limits; a manifest's canonical encoding, its claimed length (the publication blob limit) and
/// its chunk count. A removed record has no bytes to judge.
fn check_records(
    reader: &StoreReader,
    source: &mut dyn ObjectSource,
    facts: &UploadFacts,
) -> Result<(), RdyError> {
    let wanted = |k: &&ObjectKey| matches!(k.kind, kinds::BLOCK | kinds::BLOB_MANIFEST);
    for key in facts.objects.iter().filter(wanted) {
        if reader.has_kind(key.kind, &key.hash)? {
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
/// worth, read over the union and the holder. A blob with a removed chunk is unavailable to
/// reads, as a removal leaves it, and has no lengths to judge.
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

/// The landing rules of every landing of the upload that the holder does not hold, and the
/// chain rules of the root tag's chain over the union and the holder (module docs, step 3). A
/// landing's fields are judged whether or not its snapshot is shared with another; a held
/// landing was judged when it was taken, so its changes' dependencies are not read again.
fn check_landing_rules(
    reader: &StoreReader,
    source: &mut dyn ObjectSource,
    facts: &UploadFacts,
) -> Result<(), RdyError> {
    let in_union = |k: &ObjectKey| facts.objects.binary_search(k).is_ok();
    for key in facts.objects.iter().filter(|k| k.kind == kinds::LANDING) {
        if reader.has_kind(kinds::LANDING, &key.hash)? {
            continue;
        }
        let Some(landing) = typed::<LandingRecord>(source, *key)? else {
            continue;
        };
        rdy_store::landing_field_rules(&key.hash, &landing, &mut |kind, h| {
            let k = ObjectKey { kind, hash: *h };
            Ok::<_, Rule>(in_union(&k) || reader.has_kind(kind, h)?)
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
        if reader.has_kind(kinds::LANDING, l)? {
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
/// fsck's own text, refused before the take appends anything.
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

/// The tag rule (module docs, step 1).
fn choose_tag(root: &Tag, held: &[Held], damage: Option<&RdyError>) -> Result<TagChoice, RdyError> {
    let version = parse_version(&root.name)?;
    let same: Vec<&Held> = held
        .iter()
        .filter(|h| h.version.cmp_precedence(&version) == Ordering::Equal)
        .collect();
    match same[..] {
        [] => Ok(TagChoice::Incoming),
        [h] if h.tag.landing == root.landing => Ok(TagChoice::Existing {
            name: h.tag.name.clone(),
            hash: h.hash,
        }),
        [h] => Err(tag_exists(&root.name, &h.tag.landing)),
        // two held tags of one version: fsck's report, never a choice by iteration order
        _ => Err(damage.cloned().unwrap_or_else(|| {
            RdyError::new(
                crate::E_IO,
                format!("two tags denote version {}", root.name),
                "a tag ref is damaged or was written outside rdy tag: restore the store from a backup",
            )
        })),
    }
}

/// E_TAG_EXISTS: the incoming version `name` already names landing `existing` in the holder.
fn tag_exists(name: &str, existing: &Hash) -> RdyError {
    const FIX: &str = "use another version name";
    let mut e = RdyError::new(
        E_TAG_EXISTS,
        format!("version {name} already names landing {existing}"),
        FIX,
    );
    e.issues.push(Issue {
        severity: Severity::Error,
        code: E_TAG_EXISTS,
        block: None,
        message: format!("existing_landing: {existing}"),
        fix: FIX.into(),
    });
    e
}

/// Checks the snapshot of every landing of the upload that the holder does not hold yet (module
/// docs, step 2). A snapshot several landings share is checked once. The blocks of the last
/// snapshot are kept for the next one, so a history whose landings share most blocks reads each
/// shared version about once; nothing older is kept.
fn check_landings(
    reader: &StoreReader,
    source: &mut dyn ObjectSource,
    facts: &UploadFacts,
) -> Result<(), RdyError> {
    let mut checked: HashSet<Hash> = HashSet::new();
    let mut last: BTreeMap<Hash, Block> = BTreeMap::new();
    for key in facts.objects.iter().filter(|k| k.kind == kinds::LANDING) {
        if reader.has_kind(kinds::LANDING, &key.hash)? {
            continue;
        }
        let Some(landing) = typed::<LandingRecord>(source, *key)? else {
            continue;
        };
        if !checked.insert(landing.state) {
            continue;
        }
        let snapshot_key = ObjectKey {
            kind: kinds::SNAPSHOT,
            hash: landing.state,
        };
        let snapshot = typed::<StateSnapshot>(source, snapshot_key)?
            .ok_or_else(|| to_rdy(StreamError::unresolved(0, 0, snapshot_key)))?;
        let mut blocks: BTreeMap<Hash, Block> = BTreeMap::new();
        for (id, version) in &snapshot.blocks {
            let block = match last.remove(version) {
                Some(b) => b,
                None => match read_block(source, *version)? {
                    Some(b) => b,
                    // a removed version no residue describes reads as nothing
                    None => continue,
                },
            };
            if block.id != *id {
                return Err(foreign_version(&landing.state, id, version, &block.id));
            }
            blocks.insert(*version, block);
        }
        check_snapshot_limits(blocks.values(), |h| manifest(source, *h))?;
        last = blocks;
    }
    Ok(())
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
