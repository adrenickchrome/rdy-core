//! Recorded landing facts, history walks and object-key presence of one read state (D-D2NPN,
//! D-DAGNB, D-WDKWQ, D-3M81W).
//!
//! A host that keeps tag histories needs what the log recorded, not what rdy would decide now:
//! each landing as it was stored (approval, assurance, relay, evidence, checks, chosen
//! alternative, rebase), the block writes of its changes, and the signatures that state holds.
//! Nothing here recomputes an assurance, a gate or a signature verdict.
//!
//! Every call reads one [`ReadState`] and checks its redaction epoch before and after, so a
//! removal that lands during a read is refused (`E_REDACTED_STATE`) and the caller discards the
//! result. A history walk is iterative, follows `prev` only, and is bounded at
//! [`MAX_LANDINGS`] landings.
use crate::error::{E_BAD_INPUT, E_LIMIT, E_NOT_FOUND, RdyError};
use crate::read_state::ReadState;
use crate::signature_check::state_signatures;
use crate::stream_error::{Reason, StreamError, to_rdy};
use rdy_model::signature::{Signature, parse_statement};
use rdy_model::{BlockId, Hash, LandingRecord, Op};
use rdy_store::StoreError;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::Location;
use rdy_store::sync_stream::is_sync_kind;
use std::collections::{BTreeMap, HashSet};

/// The most landings one history walk visits.
pub const MAX_LANDINGS: usize = 100_000;
/// The most object keys one presence query or one `known` set holds.
pub const MAX_KEYS: usize = 100_000;

/// What a change did to one block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockOp {
    Create,
    Update,
    Delete,
}

impl BlockOp {
    pub fn as_str(self) -> &'static str {
        match self {
            BlockOp::Create => "create",
            BlockOp::Update => "update",
            BlockOp::Delete => "delete",
        }
    }
}

/// One op of one change of a landing. `change` is the change's recorded hash (a change read
/// through its removal residue keeps it), `base` the version an update or delete started from,
/// and `version` the version a create or update wrote (none for a delete). `redacted` says the
/// version's content was removed; its identity is still the recorded one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockWrite {
    pub change: Hash,
    pub id: BlockId,
    pub op: BlockOp,
    pub base: Option<Hash>,
    pub version: Option<Hash>,
    pub redacted: bool,
}

/// One signature of a subject, as stored. `kind` is the statement kind (`approval`, `relay`,
/// `tag` or `keybind`), or none when the statement bytes do not parse. No trust, key or validity
/// verdict is computed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureFact {
    pub hash: Hash,
    pub kind: Option<String>,
    pub record: Signature,
}

/// A landing as recorded: its full record, the block writes of its changes (in `changes` order,
/// then op order) and the signatures of the landing that the read state holds (signature hash
/// order).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LandingFacts {
    pub hash: Hash,
    pub record: LandingRecord,
    pub blocks: Vec<BlockWrite>,
    pub signatures: Vec<SignatureFact>,
}

/// Whether the log holds one stored object, by exact kind and hash. A removed object counts as
/// present and redacted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectPresence {
    pub key: ObjectKey,
    pub present: bool,
    pub redacted: bool,
}

/// `E_LIMIT` for a history over [`MAX_LANDINGS`] landings.
fn history_limit() -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("history exceeds {MAX_LANDINGS} landings"),
        "request a shorter history",
    )
}

/// `E_LIMIT` for a key set over [`MAX_KEYS`] keys.
pub(crate) fn key_limit() -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("object presence query exceeds {MAX_KEYS} keys"),
        format!("request at most {MAX_KEYS} object keys"),
    )
}

fn landing_key(hash: Hash) -> ObjectKey {
    ObjectKey {
        kind: kinds::LANDING,
        hash,
    }
}

/// The landings of the history that starts at `start`, newest first: `start`, then each `prev`.
/// More than [`MAX_LANDINGS`] is `E_LIMIT`; a landing met twice is the dependency-cycle error.
fn walk(read: &ReadState, start: Hash) -> Result<Vec<Hash>, RdyError> {
    let reader = read.reader();
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut at = Some(start);
    while let Some(h) = at {
        if !seen.insert(h) {
            return Err(to_rdy(
                StreamError::bad_record(0, 0, Reason::DependencyCycle)
                    .with_object(Some(landing_key(h))),
            ));
        }
        if out.len() == MAX_LANDINGS {
            return Err(history_limit());
        }
        out.push(h);
        at = reader.landing(&h)?.prev;
    }
    Ok(out)
}

/// The signatures `read` holds for `subject` (a landing or a tag), in signature hash order, each
/// decoded. More than 4096 is the signing `E_LIMIT`.
pub fn signatures(read: &ReadState, subject: Hash) -> Result<Vec<SignatureFact>, RdyError> {
    read.ensure_current_epoch()?;
    let out = signature_facts(read, &subject)?;
    read.ensure_current_epoch()?;
    Ok(out)
}

fn signature_facts(read: &ReadState, subject: &Hash) -> Result<Vec<SignatureFact>, RdyError> {
    let reader = read.reader();
    let hashes = state_signatures(read, subject)?;
    let mut out = Vec::with_capacity(hashes.len());
    for hash in hashes {
        let record = reader.signature(&hash)?;
        let kind = parse_statement(&record.statement)
            .ok()
            .map(|p| p.kind().as_str().to_string());
        out.push(SignatureFact { hash, kind, record });
    }
    Ok(out)
}

/// The facts of landing `hash` in `read`, without an epoch check (the callers check it once).
fn facts(read: &ReadState, hash: Hash) -> Result<LandingFacts, RdyError> {
    let reader = read.reader();
    let record = reader.landing(&hash)?;
    let mut blocks = Vec::new();
    for change_hash in &record.changes {
        let change = reader.change(change_hash)?;
        for op in &change.ops {
            blocks.push(match op {
                Op::Create { block } => BlockWrite {
                    change: *change_hash,
                    id: block.id.clone(),
                    op: BlockOp::Create,
                    base: None,
                    version: Some(block.version_hash()),
                    redacted: block.is_redacted(),
                },
                Op::Update { base, block } => BlockWrite {
                    change: *change_hash,
                    id: block.id.clone(),
                    op: BlockOp::Update,
                    base: Some(*base),
                    version: Some(block.version_hash()),
                    redacted: block.is_redacted(),
                },
                Op::Delete { base, id } => BlockWrite {
                    change: *change_hash,
                    id: id.clone(),
                    op: BlockOp::Delete,
                    base: Some(*base),
                    version: None,
                    redacted: false,
                },
            });
        }
    }
    let signatures = signature_facts(read, &hash)?;
    Ok(LandingFacts {
        hash,
        record,
        blocks,
        signatures,
    })
}

/// The recorded facts of landing `hash`. The signatures are those `read` holds, so a signature
/// stored after the state was published is not in them.
pub fn landing(read: &ReadState, hash: Hash) -> Result<LandingFacts, RdyError> {
    read.ensure_current_epoch()?;
    let out = facts(read, hash)?;
    read.ensure_current_epoch()?;
    Ok(out)
}

/// The facts of the history that starts at landing `start`, newest first. At most
/// [`MAX_LANDINGS`] landings.
pub fn chain(read: &ReadState, start: Hash) -> Result<Vec<LandingFacts>, RdyError> {
    read.ensure_current_epoch()?;
    let hashes = walk(read, start)?;
    let mut out = Vec::with_capacity(hashes.len());
    for h in hashes {
        out.push(facts(read, h)?);
    }
    read.ensure_current_epoch()?;
    Ok(out)
}

/// Whether landing `hash` lies in the history of one of `heads`. Each head's history is walked
/// once: a landing an earlier walk passed is not walked again, so divergent histories that share
/// a past cost it once.
pub fn reachable(read: &ReadState, heads: &[Hash], hash: Hash) -> Result<bool, RdyError> {
    read.ensure_current_epoch()?;
    let reader = read.reader();
    let mut done: HashSet<Hash> = HashSet::new();
    for head in heads {
        let mut path: HashSet<Hash> = HashSet::new();
        let mut at = Some(*head);
        while let Some(h) = at {
            if done.contains(&h) {
                break;
            }
            if h == hash {
                return Ok(true);
            }
            if !path.insert(h) {
                return Err(to_rdy(
                    StreamError::bad_record(0, 0, Reason::DependencyCycle)
                        .with_object(Some(landing_key(h))),
                ));
            }
            if path.len() > MAX_LANDINGS {
                return Err(history_limit());
            }
            at = reader.landing(&h)?.prev;
        }
        done.extend(path);
    }
    read.ensure_current_epoch()?;
    Ok(false)
}

/// The refusal of a landing outside the captured histories.
pub fn not_reachable(hash: Hash) -> RdyError {
    RdyError::new(
        E_NOT_FOUND,
        format!("no landing {hash} in the captured history"),
        "read landings from canon_chain or tag_history",
    )
}

/// Whether the sealed log of `read` (up to its captured end) holds each of `keys`, by exact kind
/// and hash, in key order without repeats. Only an assigned portable object kind may be asked
/// (`E_BAD_INPUT` otherwise). A ref update, a marker or a transition is no object. A frame an
/// applied removal dropped counts as present and redacted; a frame in a batch whose marker the
/// log has not reached by the captured end counts as nothing. At most [`MAX_KEYS`] keys.
pub fn presence(read: &ReadState, keys: &[ObjectKey]) -> Result<Vec<ObjectPresence>, RdyError> {
    if keys.len() > MAX_KEYS {
        return Err(key_limit());
    }
    let mut seen: BTreeMap<ObjectKey, (bool, bool)> =
        keys.iter().map(|k| (*k, (false, false))).collect();
    if let Some(k) = seen.keys().find(|k| !is_sync_kind(k.kind)) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("object presence kind {} is not portable", k.kind),
            "request stored object kinds only",
        ));
    }
    read.ensure_current_epoch()?;
    let origin = Location {
        segment: 0,
        offset: 0,
    };
    // a batch's frames count when its marker closes it
    let mut open: Vec<(ObjectKey, bool)> = Vec::new();
    read.reader()
        .for_each_record_from(origin, read.seq(), |_, frame, _| {
            match frame {
                StoredFrame::Present {
                    kind: kinds::BATCH_COMMIT,
                    ..
                } => {
                    for (key, redacted) in open.drain(..) {
                        if let Some(row) = seen.get_mut(&key) {
                            row.0 = true;
                            row.1 |= redacted;
                        }
                    }
                }
                StoredFrame::Present { kind, hash, .. } => {
                    let key = ObjectKey { kind, hash };
                    if seen.contains_key(&key) {
                        open.push((key, false));
                    }
                }
                StoredFrame::Dropped { kind, hash, .. } => {
                    let key = ObjectKey { kind, hash };
                    if seen.contains_key(&key) {
                        open.push((key, true));
                    }
                }
            }
            Ok::<(), StoreError>(())
        })?;
    read.ensure_current_epoch()?;
    Ok(seen
        .into_iter()
        .map(|(key, (present, redacted))| ObjectPresence {
            key,
            present,
            redacted,
        })
        .collect())
}
