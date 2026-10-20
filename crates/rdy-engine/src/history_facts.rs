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

/// Walks the histories of `heads` in order, each newest first by `prev` (read by `prev_of`), and
/// calls `visit` once for each distinct landing met, until `visit` returns true. A landing an
/// earlier head's walk passed is not walked again, so divergent histories that share a past cost
/// it once. One budget of [`MAX_LANDINGS`] covers the distinct landings met across all heads, the
/// one `visit` stops at included; the landing that would pass it is `E_LIMIT`, before it is
/// visited. A landing met twice in one walk is the dependency-cycle error. Returns whether
/// `visit` stopped the walk.
fn walk_heads(
    heads: &[Hash],
    mut prev_of: impl FnMut(&Hash) -> Result<Option<Hash>, RdyError>,
    mut visit: impl FnMut(Hash) -> bool,
) -> Result<bool, RdyError> {
    // landings met by the walks that ended; the walk in progress is `path`
    let mut done: HashSet<Hash> = HashSet::new();
    for head in heads {
        let mut path: HashSet<Hash> = HashSet::new();
        let mut at = Some(*head);
        while let Some(h) = at {
            if done.contains(&h) {
                break;
            }
            if path.contains(&h) {
                return Err(to_rdy(
                    StreamError::bad_record(0, 0, Reason::DependencyCycle)
                        .with_object(Some(landing_key(h))),
                ));
            }
            if done.len() + path.len() == MAX_LANDINGS {
                return Err(history_limit());
            }
            if visit(h) {
                return Ok(true);
            }
            path.insert(h);
            at = prev_of(&h)?;
        }
        done.extend(path);
    }
    Ok(false)
}

/// Whether landing `hash` lies in the history of one of `heads`. Each head's history is walked
/// once: a landing an earlier walk passed is not walked again, so divergent histories that share
/// a past cost it once. One budget of [`MAX_LANDINGS`] covers the distinct landings met across
/// all heads, a matched `hash` included; the landing that would pass it is `E_LIMIT`.
pub fn reachable(read: &ReadState, heads: &[Hash], hash: Hash) -> Result<bool, RdyError> {
    read.ensure_current_epoch()?;
    let reader = read.reader();
    let found = walk_heads(heads, |h| Ok(reader.landing(h)?.prev), |h| h == hash)?;
    // a hit is checked as a miss is: a removal may have landed during the walk
    before_last_check();
    read.ensure_current_epoch()?;
    Ok(found)
}

/// `E_LIMIT` for a landing facts query over [`MAX_KEYS`] hashes.
fn landings_limit() -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("landing facts query exceeds {MAX_KEYS} hashes"),
        format!("request at most {MAX_KEYS} landing hashes"),
    )
}

/// The recorded facts of each landing of `hashes` that lies in the history of one of `heads`:
/// one per distinct hash, in the order each first occurs. Each equals [`landing`] of that hash.
/// The input counts before its repeats collapse, and more than [`MAX_KEYS`] hashes is `E_LIMIT`
/// before anything is read. All the hashes are found in one walk of the heads' histories, as
/// [`reachable`] walks them and under its one budget; the walk ends when every hash is found. A
/// hash no history reaches (missing, later than the state, or stored and unreferenced) fails
/// the whole call with [`not_reachable`], naming the first such hash of the input; there is no
/// partial answer. A damaged record or a cycle fails the call with its own error.
pub fn landings(
    read: &ReadState,
    heads: &[Hash],
    hashes: &[Hash],
) -> Result<Vec<LandingFacts>, RdyError> {
    let reader = read.reader();
    landings_by(read, heads, hashes, |h| Ok(reader.landing(h)?.prev))
}

/// [`landings`], with each landing's `prev` read by `prev_of`.
fn landings_by(
    read: &ReadState,
    heads: &[Hash],
    hashes: &[Hash],
    prev_of: impl FnMut(&Hash) -> Result<Option<Hash>, RdyError>,
) -> Result<Vec<LandingFacts>, RdyError> {
    if hashes.len() > MAX_KEYS {
        return Err(landings_limit());
    }
    read.ensure_current_epoch()?;
    // the distinct hashes in first-occurrence order, and those the walk has not met yet
    let mut order = Vec::new();
    let mut missing = HashSet::new();
    for h in hashes {
        if missing.insert(*h) {
            order.push(*h);
        }
    }
    if !missing.is_empty() {
        walk_heads(heads, prev_of, |h| {
            missing.remove(&h);
            missing.is_empty()
        })?;
    }
    if let Some(first) = order.iter().find(|h| missing.contains(*h)) {
        before_last_check();
        read.ensure_current_epoch()?;
        return Err(not_reachable(*first));
    }
    let mut out = Vec::with_capacity(order.len());
    for h in order {
        out.push(facts(read, h)?);
    }
    before_last_check();
    read.ensure_current_epoch()?;
    Ok(out)
}

#[cfg(test)]
thread_local! {
    /// What runs once on this thread just before the next read's last epoch check.
    static BEFORE_LAST_CHECK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Sets what runs once on this thread just before the next read's last epoch check.
#[cfg(test)]
fn set_before_last_check(hook: Box<dyn FnOnce()>) {
    BEFORE_LAST_CHECK.with(|h| *h.borrow_mut() = Some(hook));
}

/// Runs the hook a test set, if any; nothing in any other build.
fn before_last_check() {
    #[cfg(test)]
    if let Some(hook) = BEFORE_LAST_CHECK.with(|h| h.borrow_mut().take()) {
        hook();
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{E_REDACTED_STATE, REDACTED_STATE_FIX};
    use crate::redaction::RedactInput;
    use crate::{Rdy, Session, WriteRequest};
    use rdy_model::{Assurance, Provenance, RepoId, Role};
    use rdy_store::LandRequest;
    use serde_json::json;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn writer() -> Session {
        Session::human("human:t".into(), Role::Writer)
    }

    fn approver() -> Session {
        Session::human("human:t".into(), Role::Approver)
    }

    /// An error as its code, message and fix.
    fn parts(e: RdyError) -> (String, String, String) {
        (e.code.to_string(), e.message, e.fix)
    }

    /// A project whose one landing holds `n` prose blocks with a secret each. Returns the
    /// project, the landing and the blocks.
    fn secrets(dir: &std::path::Path, n: usize) -> (Rdy, Hash, Vec<String>) {
        let mut r = Rdy::init(dir, "rdy-standard").unwrap();
        let mut ops = vec![
            json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}}),
        ];
        for i in 0..n {
            ops.push(json!({"op": "create", "type": "prose", "id": format!("$p{i}"), "parent": "$d", "body": format!("The launch code is ZEBRA-{i}.")}));
        }
        let w = r
            .write(
                &writer(),
                WriteRequest {
                    view: "work".into(),
                    ops: serde_json::from_value(json!(ops)).unwrap(),
                    justification: None,
                    message: String::new(),
                },
            )
            .unwrap();
        let p = r.propose(&writer(), "work", "Base", None).unwrap();
        let l: Hash = r.land(&approver(), &p.id).unwrap().parse().unwrap();
        let blocks = (0..n)
            .map(|i| w.created[&format!("$p{i}")].clone())
            .collect();
        (r, l, blocks)
    }

    /// Removes every version of block `target` through the store's own landing path.
    fn remove(r: &mut Rdy, target: String) {
        let req = r
            .redact(
                &writer(),
                RedactInput {
                    targets: vec![target],
                    reason_class: "secret".into(),
                    reason: "a leaked code".into(),
                    dry_run: false,
                },
            )
            .unwrap();
        assert!(req.redaction.is_some());
        let p = r
            .propose(&writer(), "work", "Remove the code", None)
            .unwrap();
        let version: Hash = p.id.parse().unwrap();
        r.seal_open().unwrap();
        r.store_mut().set_held(false).unwrap();
        r.store_mut()
            .land_atomic(LandRequest {
                proposal: version,
                accept: None,
                approver: Provenance {
                    actor: "human:t".into(),
                    run: None,
                    role: Role::Approver,
                    at_ms: 1_000_000,
                },
                alt: None,
                relay: None,
                evidence: None,
            })
            .unwrap();
        r.store_mut().set_held(true).unwrap();
        r.invalidate();
    }

    #[test]
    fn sdc2_reachable_checks_epoch_on_a_hit() {
        let _clock = crate::pin_clock(1_000_000);
        let dir = tempfile::tempdir().unwrap();
        let (r, l, blocks) = secrets(dir.path(), 4);
        let r = Rc::new(RefCell::new(r));
        // the next read's last epoch check runs after a removal of `target` has landed
        let removing = |target: &String| {
            let (r, target) = (r.clone(), target.clone());
            set_before_last_check(Box::new(move || remove(&mut r.borrow_mut(), target)));
        };
        let stale = |e: RdyError| {
            assert_eq!(
                (e.code, e.fix.as_str()),
                (E_REDACTED_STATE, REDACTED_STATE_FIX),
                "{e}"
            );
        };
        let read = r.borrow().read();
        assert!(reachable(&read, &[l], l).unwrap());
        // a hit is checked too
        removing(&blocks[0]);
        stale(reachable(&read, &[l], l).unwrap_err());
        // the batch: all found, unreachable after the completed walk, and empty
        let read = r.borrow().read();
        removing(&blocks[1]);
        stale(landings(&read, &[l], &[l]).unwrap_err());
        let read = r.borrow().read();
        removing(&blocks[2]);
        stale(landings(&read, &[l], &[Hash([9; 32])]).unwrap_err());
        let read = r.borrow().read();
        removing(&blocks[3]);
        stale(landings(&read, &[l], &[]).unwrap_err());
        // every hook ran: four removals landed after the first landing
        let read = r.borrow().read();
        assert_eq!(walk(&read, read.canon_head().unwrap()).unwrap().len(), 5);
        assert_eq!(landings(&read, &[l], &[l]).unwrap()[0].hash, l);
    }

    /// A stored landing that follows `prev`, distinct by `n`.
    fn link(n: u8, prev: Option<Hash>) -> LandingRecord {
        LandingRecord {
            proposal: Hash([n; 32]),
            prev,
            state: Hash([2; 32]),
            approved_by: Provenance {
                actor: "human:founder".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1,
            },
            changes: vec![],
            chosen: None,
            checks: vec![],
            assurance: Assurance::Local,
            relayed_by: None,
            relay_evidence: None,
            approved_version: None,
            rebased_from: vec![],
            evidence: None,
        }
    }

    #[test]
    fn sdc2_batch_propagates_damage_and_cycles() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = Rdy::create_holder(dir.path(), RepoId([1; 16])).unwrap();
        let missing = Hash([9; 32]);
        let ok = h.store_mut().put(kinds::LANDING, &link(0, None)).unwrap();
        let broken = h
            .store_mut()
            .put(kinds::LANDING, &link(1, Some(missing)))
            .unwrap();
        // a record that is no landing (the store refuses a landing frame it cannot decode)
        let junk = h
            .store_mut()
            .put(
                kinds::BLOCK,
                &rdy_model::Block {
                    id: BlockId::parse("d-00009").unwrap(),
                    block_type: "doc".into(),
                    parent: None,
                    order: "a0".into(),
                    fields: Default::default(),
                    body: "no landing".into(),
                    links: Default::default(),
                    pins: Default::default(),
                    read_identity: None,
                },
            )
            .unwrap();
        let over_junk = h
            .store_mut()
            .put(kinds::LANDING, &link(2, Some(junk)))
            .unwrap();
        h.seal_open().unwrap();
        h.close().unwrap();
        let h = Rdy::open_holder(dir.path()).unwrap();
        let read = h.read();
        let absent = parts(not_reachable(ok));

        // a missing prev is the missing record's own error, as a single read has it
        let want = (
            E_NOT_FOUND.to_string(),
            format!("object {} not found", missing.short()),
            "check the hash".to_string(),
        );
        assert_eq!(parts(reachable(&read, &[broken], ok).unwrap_err()), want);
        assert_eq!(parts(landings(&read, &[broken], &[ok]).unwrap_err()), want);
        assert_ne!(want, absent);
        // an invalid record, as a head and as a prev
        let want = parts(reachable(&read, &[junk], ok).unwrap_err());
        assert!(
            want.1
                .contains(&format!("{junk} is not a landing (object kind 1)")),
            "{want:?}"
        );
        assert_ne!(want.0, E_NOT_FOUND, "{want:?}");
        for head in [junk, over_junk] {
            assert_eq!(parts(landings(&read, &[head], &[ok]).unwrap_err()), want);
        }
        // a hash found before the damage is met ends the walk there
        assert_eq!(landings(&read, &[ok, broken], &[ok]).unwrap()[0].hash, ok);

        // a cycle: the walk both reads share refuses it, naming the landing met twice
        let (x, y) = (Hash([1; 32]), Hash([2; 32]));
        let ring = |h: &Hash| Ok(Some(if *h == x { y } else { x }));
        let want = parts(to_rdy(
            StreamError::bad_record(0, 0, Reason::DependencyCycle)
                .with_object(Some(landing_key(x))),
        ));
        assert_eq!(want.1, "stream record 0 at byte 0: dependency cycle");
        assert_eq!(
            parts(landings_by(&read, &[x], &[ok], ring).unwrap_err()),
            want
        );
        assert_eq!(
            parts(walk_heads(&[ok, x], ring, |_| false).unwrap_err()),
            want
        );
    }
}
