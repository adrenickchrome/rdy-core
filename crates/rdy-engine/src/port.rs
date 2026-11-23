//! Hub ports (D-A3H7S, D-1QT23, D-08M5W, D-3DG3R, D-3JN9N, D-7AT4S): landed changes of one line
//! of a family, carried into a new view of another line.
//!
//! - **Make.** [`make_bundle`] reads one holder snapshot. It takes landings of that snapshot's
//!   hub canon only, and emits them once each, oldest first, with their changes in landing order
//!   and the blobs they show. Tags, held chains, replaced marks and stored presence do not make
//!   a landing eligible. The bundle holds no epoch, removal set or expiry: the hub makes it again
//!   for every attempt.
//! - **Verify.** [`verify_bundle`] checks the whole value (`crate::port_wire`) before anything
//!   reads it.
//! - **Prepare.** [`prepare_port`] reads the target under its one writer and returns a value: the
//!   new changes, their blobs, their conflicts and their briefing. It writes nothing.
//!
//! The checks of a take, after the view-head compare, run in this order: the target removals,
//! then for each op its foreign pin and then its missing-base delete, then the briefing. The
//! parent rule and the parent loop follow, on the merged result. A holder target checks no
//! foreign pin: it holds no foreign entry and its checks read portable refs only (D-FK58A), so
//! the client checks each pin where a person lands (D-DAPR8). A project target checks each pin
//! against its own deliveries. A port never brings a delivery.
//!
//! A ported change copies its source's ops, justification and message. It records its source in
//! `links.ported_from` and the target's briefing in `links.briefing`. Its `replayed_from` stays
//! empty, and it has no intent or alternative. A held base stays. A create or update whose base
//! the target lacks becomes a create, so the ordinary merge decides it. A delete whose base the
//! target lacks is dropped when the target lacks the block, and refused when it holds it.
use crate::error::*;
use crate::materialize::Materialized;
use crate::port_wire::{
    self, BundleBlob, BundleChange, BundleChunk, BundleContent, BundleLanding, ByteCount,
};
use crate::read_state::ReadState;
use crate::views::replay::{Preview, Slot, view_writers};
use crate::{BriefRegistry, OpInput, Scope};
use rdy_model::{
    Block, BlockId, Change, ChangeLinks, Hash, Justification, Op, PortedFrom, Provenance, RepoId,
};
use rdy_store::BlobState;
use rdy_store::holder::StoreMode;
use rdy_store::kinds;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

pub use crate::port_wire::{
    MAX_PORT_BUNDLE_BYTES, PORT_TOO_LARGE_FIX, PORT_TOO_LARGE_MESSAGE, PORT_UNVERIFIED_FIX,
    PORT_UNVERIFIED_MESSAGE,
};

/// The most landing entries one make takes, counted before duplicates collapse.
pub const MAX_PORT_ENTRIES: usize = 1_000;
/// The most changes one bundle holds.
pub const MAX_PORT_CHANGES: usize = 1_000;

/// The message of a make with no landing.
pub const PORT_EMPTY_MESSAGE: &str = "name at least one landing to carry";
/// The fix of a make with no landing.
pub const PORT_EMPTY_FIX: &str = "name a landing from this line's hub canon";
/// The message of a missing-base delete against a block the target holds.
pub const PORT_DELETE_BASE_MESSAGE: &str = "the target line lacks the base of a ported delete";
/// The fix of a missing-base delete against a block the target holds.
pub const PORT_DELETE_BASE_FIX: &str = "carry the landing that wrote it first";
/// The message of a bundle from outside the target's family.
pub const PORT_FAMILY_MESSAGE: &str = "the port bundle comes from outside this line's family";
/// The fix of a bundle from outside the target's family.
pub const PORT_FAMILY_FIX: &str = "carry from a line of the same family";
/// The message of a bundle from the target store itself.
pub const PORT_SAME_STORE_MESSAGE: &str = "the port bundle comes from this store";
/// The fix of a bundle from the target store itself.
pub const PORT_SAME_STORE_FIX: &str = "replay the change inside this store";
/// The message of a landing outside the snapshot's hub canon; `<hash>` is the landing.
pub const PORT_NOT_CANON_MESSAGE: &str = "landing <hash> is not on this store's hub canon";
/// The fix of a landing outside the snapshot's hub canon.
pub const PORT_NOT_CANON_FIX: &str = "carry a landing from this line's hub canon";
/// The message of a make on a store that is not a holder.
pub const PORT_HOLDER_MESSAGE: &str = "a hub port needs a holder";
/// The fix of a make on a store that is not a holder.
pub const PORT_HOLDER_FIX: &str = "open the line's hub store";
/// The message of a take into a line with no hub canon.
pub const PORT_NO_CANON_MESSAGE: &str = "the target line has no hub canon";
/// The fix of a take into a line with no hub canon.
pub const PORT_NO_CANON_FIX: &str = "seed the line before carrying a landing";
/// The message of a carry whose author is not a person's actor.
pub const PORT_AUTHOR_MESSAGE: &str = "a hub carry needs a person's actor";
/// The fix of a carry whose author is not a person's actor.
pub const PORT_AUTHOR_FIX: &str = "pass the person who asked for the carry";
/// The message of a make whose change shows a removed blob.
pub const PORT_SOURCE_BLOB_MESSAGE: &str = "a source blob of the ported change was removed";
/// The fix of a make whose change shows a removed blob.
pub const PORT_SOURCE_BLOB_FIX: &str = "port the change after its figure is replaced";
/// The message of a take whose bundle holds content the target removed.
pub const PORT_TARGET_REMOVED_MESSAGE: &str =
    "the port bundle holds a version or blob removed in this line";
/// The fix of a take whose bundle holds content the target removed.
pub const PORT_TARGET_REMOVED_FIX: &str = "carry the change after the line's removal is replaced";
/// The message of a port that cites no target briefing.
pub const PORT_BRIEF_MESSAGE: &str = "a port needs a target briefing";
/// The fix of a port that cites no target briefing.
pub const PORT_BRIEF_FIX: &str = "brief the target blocks and retry with the token";
/// The message of a port whose create or move needs a parent that is missing or conflicted;
/// `<id>` is the parent.
pub const PORT_PARENT_MESSAGE: &str = "the port needs parent <id> present and clean in the target";
/// The fix of a port whose create or move needs a missing or conflicted parent.
pub const PORT_PARENT_FIX: &str =
    "carry the change that creates the parent first, or resolve its conflict";
/// The message of a port that would make two blocks each other's ancestors; `<a>` and `<b>`
/// are the blocks.
pub const PORT_CYCLE_MESSAGE: &str = "the port would make <a> and <b> each other's ancestors";
/// The fix of a port that would make a parent loop.
pub const PORT_CYCLE_FIX: &str = "move one block out of the other in the target, then carry again";

/// The briefing fix that `brief::check` gives.
const BRIEF_AGAIN_FIX: &str = "brief these blocks (rdy_brief) and retry with the token";

/// `E_BAD_INPUT`: a make with no landing.
pub fn empty_input() -> RdyError {
    RdyError::new(E_BAD_INPUT, PORT_EMPTY_MESSAGE, PORT_EMPTY_FIX)
}

/// `E_BAD_INPUT`: past the entry, change or byte bound on make.
pub fn too_large() -> RdyError {
    port_wire::too_large()
}

/// `E_BAD_INPUT`: the bundle does not verify.
pub fn unverified() -> RdyError {
    port_wire::unverified()
}

/// `E_BAD_INPUT`: a missing-base delete against a block the target holds.
pub fn delete_base() -> RdyError {
    RdyError::new(E_BAD_INPUT, PORT_DELETE_BASE_MESSAGE, PORT_DELETE_BASE_FIX)
}

/// `E_BAD_INPUT`: no counted repo id is shared.
pub fn outside_family() -> RdyError {
    RdyError::new(E_BAD_INPUT, PORT_FAMILY_MESSAGE, PORT_FAMILY_FIX)
}

/// `E_BAD_INPUT`: the source is the target store.
pub fn same_store() -> RdyError {
    RdyError::new(E_BAD_INPUT, PORT_SAME_STORE_MESSAGE, PORT_SAME_STORE_FIX)
}

/// `E_BAD_INPUT`: `landing` is not on the snapshot's hub canon.
pub fn not_on_canon(landing: &Hash) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        PORT_NOT_CANON_MESSAGE.replace("<hash>", &landing.to_string()),
        PORT_NOT_CANON_FIX,
    )
}

/// `E_BAD_INPUT`: a make on a store that is not a holder.
pub fn needs_holder() -> RdyError {
    RdyError::new(E_BAD_INPUT, PORT_HOLDER_MESSAGE, PORT_HOLDER_FIX)
}

/// `E_BAD_INPUT`: the target line has no hub canon.
pub fn no_hub_canon() -> RdyError {
    RdyError::new(E_BAD_INPUT, PORT_NO_CANON_MESSAGE, PORT_NO_CANON_FIX)
}

/// `E_BAD_INPUT`: the author is not a person's actor.
pub fn not_a_person() -> RdyError {
    RdyError::new(E_BAD_INPUT, PORT_AUTHOR_MESSAGE, PORT_AUTHOR_FIX)
}

/// `E_REDACTED_STATE`: a change of the make shows a removed blob.
pub fn source_blob_removed() -> RdyError {
    RdyError::new(
        E_REDACTED_STATE,
        PORT_SOURCE_BLOB_MESSAGE,
        PORT_SOURCE_BLOB_FIX,
    )
}

/// `E_REDACTED_STATE`: the bundle holds a version or blob the target removed.
pub fn target_removed() -> RdyError {
    RdyError::new(
        E_REDACTED_STATE,
        PORT_TARGET_REMOVED_MESSAGE,
        PORT_TARGET_REMOVED_FIX,
    )
}

/// `E_BRIEF_REQUIRED`: a port that cites no target briefing.
pub fn needs_briefing() -> RdyError {
    RdyError::new(
        crate::brief::E_BRIEF_REQUIRED,
        PORT_BRIEF_MESSAGE,
        PORT_BRIEF_FIX,
    )
}

/// `E_DEPS_MISSING`: parent `p` is missing or conflicted once the port applies.
pub fn parent_missing(p: &BlockId) -> RdyError {
    RdyError::new(
        E_DEPS_MISSING,
        PORT_PARENT_MESSAGE.replace("<id>", &p.to_string()),
        PORT_PARENT_FIX,
    )
}

/// `E_BAD_INPUT`: the port would make `a` and `b` each other's ancestors.
pub fn parent_cycle(a: &BlockId, b: &BlockId) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        PORT_CYCLE_MESSAGE
            .replace("<a>", &a.to_string())
            .replace("<b>", &b.to_string()),
        PORT_CYCLE_FIX,
    )
}

/// One landing of a bundle, with its changes in landing order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortLandingFacts {
    pub landing: Hash,
    pub changes: Vec<Hash>,
}

/// What a bundle holds, in its canonical order: the source line, its counted origins, the
/// landings, the number of changes, the whole value's bytes and its hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortBundleFacts {
    pub source: RepoId,
    pub fork_origins: Vec<RepoId>,
    pub landings: Vec<PortLandingFacts>,
    pub changes: usize,
    /// The length of [`PortBundle::to_bytes`].
    pub bytes: usize,
    pub bundle_hash: Hash,
}

/// A verified port bundle: its bytes and what they hold. It holds no handle and no snapshot.
#[derive(Clone)]
pub struct PortBundle {
    bytes: Arc<[u8]>,
    content: Arc<BundleContent>,
    facts: PortBundleFacts,
}

impl std::fmt::Debug for PortBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PortBundle")
            .field("facts", &self.facts)
            .finish_non_exhaustive()
    }
}

impl PortBundle {
    /// The bundle's bytes (`rdy-port-bundle-1`).
    pub fn to_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The bundle `bytes` hold, verified ([`verify_bundle`]).
    pub fn from_bytes(bytes: &[u8]) -> Result<PortBundle, RdyError> {
        verify_bundle(bytes)
    }

    pub fn facts(&self) -> &PortBundleFacts {
        &self.facts
    }

    /// The decoded content.
    pub fn content(&self) -> &BundleContent {
        &self.content
    }

    fn of(bytes: Vec<u8>, content: BundleContent) -> PortBundle {
        let facts = PortBundleFacts {
            source: content.source,
            fork_origins: content.fork_origins.clone(),
            landings: content
                .landings
                .iter()
                .map(|l| PortLandingFacts {
                    landing: l.landing,
                    changes: l.changes.iter().map(|c| c.hash).collect(),
                })
                .collect(),
            changes: content.landings.iter().map(|l| l.changes.len()).sum(),
            bytes: bytes.len(),
            bundle_hash: port_wire::bundle_hash(&bytes).expect("a bundle ends with its hash"),
        };
        PortBundle {
            bytes: Arc::from(bytes),
            content: Arc::new(content),
            facts,
        }
    }
}

/// Verifies `bytes` as a bundle ([`port_wire::decode_bundle`]): the length before anything is
/// read or copied, then the form, the hash and the structure.
pub fn verify_bundle(bytes: &[u8]) -> Result<PortBundle, RdyError> {
    let content = port_wire::decode_bundle(bytes)?;
    Ok(PortBundle::of(bytes.to_vec(), content))
}

#[cfg(feature = "test-support")]
thread_local! {
    /// Tests only: run after a make's walk, before its last epoch check.
    static AFTER_WALK: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Tests only: `hook` runs once after the next make's walk on this thread, before its last
/// epoch check.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub fn after_make_walk_for_tests(hook: Box<dyn FnMut()>) {
    AFTER_WALK.with(|h| *h.borrow_mut() = Some(hook));
}

fn after_walk() {
    #[cfg(feature = "test-support")]
    if let Some(mut hook) = AFTER_WALK.with(|h| h.borrow_mut().take()) {
        hook();
    }
}

/// The manifest of blob `h` as the store holds it, checked against its hash.
fn read_manifest(st: &ReadState, h: &Hash) -> Result<BundleBlob, RdyError> {
    let reader = st.reader();
    let (raw, payload) = reader.get_kind_raw(kinds::BLOB_MANIFEST, h)?;
    let o = rdy_store::codec::split(raw, &payload)
        .map_err(|_| port_wire::damaged("blob manifest", h))?;
    let m: kinds::BlobManifest = rdy_store::codec::decode_split(&o)?;
    if BundleBlob::hash_of(m.len, &m.chunks) != *h {
        return Err(port_wire::damaged("blob manifest", h));
    }
    Ok(BundleBlob {
        hash: *h,
        len: m.len,
        chunks: m.chunks,
    })
}

/// Makes a bundle of `landings` from the holder snapshot `st` (D-3DG3R). The first fault wins:
/// a store that is not a holder; a snapshot that predates a removal; no landing; more than
/// [`MAX_PORT_ENTRIES`] entries; then each landing, in input order, that is not on the
/// snapshot's hub canon. Then the walk, oldest first: more than [`MAX_PORT_CHANGES`] changes, a
/// removed blob, and more than [`MAX_PORT_BUNDLE_BYTES`] bytes. The epoch is checked again
/// before the bundle returns. A repeated landing counts against the entry bound, then appears
/// once.
pub fn make_bundle(st: &ReadState, landings: &[Hash]) -> Result<PortBundle, RdyError> {
    let reader = st.reader();
    if reader.mode()? != StoreMode::Holder {
        return Err(needs_holder());
    }
    st.ensure_current_epoch()?;
    if landings.is_empty() {
        return Err(empty_input());
    }
    if landings.len() > MAX_PORT_ENTRIES {
        return Err(too_large());
    }
    // the snapshot's hub canon, walked once, newest first
    let canon = match st.canon_head() {
        Some(tip) => crate::history_facts::walk(st, tip)?,
        None => vec![],
    };
    let on_canon: HashSet<Hash> = canon.iter().copied().collect();
    let mut selected: HashSet<Hash> = HashSet::with_capacity(landings.len());
    for l in landings {
        if !on_canon.contains(l) {
            return Err(not_on_canon(l));
        }
        selected.insert(*l);
    }
    let (source, fork_origins) = crate::signature_check::repo_ids(st)?;
    let mut count = ByteCount::new();
    count.add(port_wire::header_body(source, &fork_origins)?.len())?;
    let mut out: Vec<BundleLanding> = Vec::new();
    let mut blobs: BTreeMap<Hash, BundleBlob> = BTreeMap::new();
    let mut chunks: BTreeMap<Hash, Vec<u8>> = BTreeMap::new();
    let mut changes = 0usize;
    for l in canon.iter().rev().filter(|l| selected.contains(l)) {
        let record = reader.landing(l)?;
        count.add(port_wire::landing_body(*l)?.len())?;
        let mut landing = BundleLanding {
            landing: *l,
            changes: Vec::new(),
        };
        for c in &record.changes {
            changes += 1;
            if changes > MAX_PORT_CHANGES {
                return Err(too_large());
            }
            let change = reader.change(c)?;
            reader.refuse_copy_of(&change)?;
            // the blobs this change shows that the bundle does not hold yet
            let mut new: Vec<BundleBlob> = Vec::new();
            for op in &change.ops {
                let (Op::Create { block } | Op::Update { block, .. }) = op else {
                    continue;
                };
                for h in port_wire::block_blobs(block) {
                    if blobs.contains_key(&h) || new.iter().any(|b| b.hash == h) {
                        continue;
                    }
                    match reader.blob_state(&h)? {
                        BlobState::Missing => {
                            return Err(rdy_store::StoreError::NotFound(h).into());
                        }
                        BlobState::Removed(_) => return Err(source_blob_removed()),
                        BlobState::Readable => new.push(read_manifest(st, &h)?),
                    }
                }
            }
            let bc = BundleChange {
                hash: *c,
                ops: change.ops,
                justification: change.justification,
                message: change.message,
            };
            count.add(port_wire::change_body(&bc)?.len())?;
            for b in new {
                count.add(port_wire::manifest_body(&b)?.len())?;
                for k in &b.chunks {
                    if chunks.contains_key(k) {
                        continue;
                    }
                    let bytes = reader.get_kind(kinds::BLOB_CHUNK, k)?;
                    count.add(port_wire::chunk_body_len(bytes.len()))?;
                    if BundleChunk::hash_of(&bytes) != *k {
                        return Err(port_wire::damaged("blob chunk", k));
                    }
                    chunks.insert(*k, bytes);
                }
                blobs.insert(b.hash, b);
            }
            landing.changes.push(bc);
        }
        out.push(landing);
    }
    let content = BundleContent {
        source,
        fork_origins,
        landings: out,
        blobs: blobs.into_values().collect(),
        chunks: chunks
            .into_iter()
            .map(|(hash, bytes)| BundleChunk { hash, bytes })
            .collect(),
    };
    let bytes = port_wire::encode_bundle(&content)?;
    debug_assert_eq!(bytes.len(), count.bytes());
    after_walk();
    st.ensure_current_epoch()?;
    Ok(PortBundle::of(bytes, content))
}

/// The family check of a take on the target `st` (D-CDQF7, D-HX6BX): the bundle's source and
/// counted origins must share a repo id with the target's own and counted origins, then the
/// source must not be the target. Part 1's counted-origin read gives both sides' origins.
pub fn check_family(st: &ReadState, bundle: &PortBundle) -> Result<(), RdyError> {
    let (current, origins) = crate::signature_check::repo_ids(st)?;
    let ours: BTreeSet<RepoId> = std::iter::once(current).chain(origins).collect();
    let facts = bundle.facts();
    let shared = std::iter::once(&facts.source)
        .chain(&facts.fork_origins)
        .any(|r| ours.contains(r));
    if !shared {
        return Err(outside_family());
    }
    if facts.source == current {
        return Err(same_store());
    }
    Ok(())
}

/// The removal check of a take (D-3JN9N): every block version, base, manifest and chunk the
/// bundle holds, used by the prepared ops or not and held already or not, against the target's
/// own removal records. It runs before any op is changed or dropped.
pub fn check_target_removals(st: &ReadState, bundle: &PortBundle) -> Result<(), RdyError> {
    let reader = st.reader();
    if !reader.any_removal() {
        return Ok(());
    }
    let removed = |kind: u8, h: &Hash| -> Result<(), RdyError> {
        match reader.redaction_of(kind, h)? {
            Some(_) => Err(target_removed()),
            None => Ok(()),
        }
    };
    let content = bundle.content();
    for c in content.landings.iter().flat_map(|l| &l.changes) {
        for op in &c.ops {
            if let Op::Create { block } | Op::Update { block, .. } = op {
                removed(kinds::BLOCK, &block.version_hash())?;
            }
            if let Some(base) = op.base() {
                removed(kinds::BLOCK, &base)?;
            }
        }
    }
    for b in &content.blobs {
        removed(kinds::BLOB_MANIFEST, &b.hash)?;
    }
    for k in &content.chunks {
        removed(kinds::BLOB_CHUNK, &k.hash)?;
    }
    Ok(())
}

/// One source change a port takes: its source, ops, justification and message, and its deps
/// among the selected sources (none for a bundle).
pub(crate) struct SourceChange<'a> {
    pub from: PortedFrom,
    pub ops: &'a [Op],
    pub deps: &'a [Hash],
    pub justification: &'a Option<Justification>,
    pub message: &'a str,
}

/// Where a port goes: a new hub view on canon's tip, or an existing view of a local project.
#[derive(Clone, Copy, Debug)]
pub(crate) enum PortTarget<'a> {
    Root(Hash),
    /// A local port into an existing view; the local and dump forms pass it.
    #[allow(dead_code)]
    View(&'a str),
}

/// How a port gets its target briefing: core makes it (a hub port), or the caller cites one.
#[derive(Clone, Copy, Debug)]
pub enum PortBrief<'a> {
    Make,
    Cite(Option<&'a str>),
}

/// One ported change: its source and the new change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortedChange {
    pub source: PortedFrom,
    pub change: Hash,
}

/// A prepared port: everything its one write stores, as a value. Nothing is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedPort {
    /// The view the new changes are written to.
    pub view: String,
    /// The landing the new view starts on (canon's tip at the writer's turn), for a hub port.
    pub base: Option<Hash>,
    /// The new changes, in port order.
    pub changes: Vec<Change>,
    /// Each source with its new change, in port order.
    pub ported: Vec<PortedChange>,
    /// The blocks the merged view records a conflict on, sorted.
    pub conflicts: Vec<BlockId>,
    /// The target briefing every new change records.
    pub briefing: Hash,
    /// The record of a briefing core made, registered already.
    pub record: Option<Arc<crate::brief::BriefRecord>>,
    /// The blob chunks and manifests the target lacks, each once: chunks first, as
    /// `(raw kind, payload)` frames.
    pub blobs: Vec<(u8, Vec<u8>)>,
}

impl PreparedPort {
    /// The frames of the port's objects: its blobs, then for each change its blocks and the
    /// change, each block version once.
    pub fn object_frames(&self) -> Result<Vec<(u8, Vec<u8>)>, RdyError> {
        let mut out = self.blobs.clone();
        let mut seen: HashSet<Hash> = HashSet::new();
        for c in &self.changes {
            for op in &c.ops {
                if let Op::Create { block } | Op::Update { block, .. } = op
                    && seen.insert(block.version_hash())
                {
                    out.push(rdy_store::codec::item_new(kinds::BLOCK, block));
                }
            }
            out.push(rdy_store::codec::item(kinds::CHANGE, c)?);
        }
        Ok(out)
    }
}

/// Prepares the port of `bundle` into a new view `view` of the target `st` (a hub's store,
/// read under its writer), authored by `by`. The view starts on canon's tip. Core makes the
/// target briefing at that root and registers it in `reg`. The checks run in the take order:
/// the target removals ([`check_target_removals`]), then per op the foreign pin (a project
/// target only) and the missing-base delete, then the briefing, then the parent rule and the
/// parent loop. Nothing is written.
pub fn prepare_port(
    st: &ReadState,
    bundle: &PortBundle,
    view: &str,
    by: &Provenance,
    reg: &BriefRegistry,
) -> Result<PreparedPort, RdyError> {
    let tip = st.canon_head().ok_or_else(no_hub_canon)?;
    check_target_removals(st, bundle)?;
    let content = bundle.content();
    let sources: Vec<SourceChange<'_>> = content
        .landings
        .iter()
        .flat_map(|l| &l.changes)
        .map(|c| SourceChange {
            from: PortedFrom {
                repo_id: content.source,
                change: c.hash,
            },
            ops: &c.ops,
            deps: &[],
            justification: &c.justification,
            message: &c.message,
        })
        .collect();
    let mut p = prepare_changes(
        st,
        PortTarget::Root(tip),
        view,
        &sources,
        by,
        PortBrief::Make,
        reg,
    )?;
    p.blobs = missing_blobs(st, content)?;
    Ok(p)
}

/// The frames of the bundle's blobs that the target lacks: each chunk it lacks once, then each
/// manifest it lacks.
fn missing_blobs(st: &ReadState, content: &BundleContent) -> Result<Vec<(u8, Vec<u8>)>, RdyError> {
    let reader = st.reader();
    let by_hash: HashMap<Hash, &BundleChunk> = content.chunks.iter().map(|k| (k.hash, k)).collect();
    let mut chunks: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut manifests: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut added: HashSet<Hash> = HashSet::new();
    for b in &content.blobs {
        if reader.has_blob(&b.hash)? {
            continue;
        }
        for k in &b.chunks {
            if added.contains(k) || reader.has_kind(kinds::BLOB_CHUNK, k)? {
                continue;
            }
            let chunk = by_hash.get(k).ok_or_else(unverified)?;
            chunks.push(rdy_store::codec::frame_latest(
                kinds::BLOB_CHUNK,
                chunk.bytes.clone(),
            ));
            added.insert(*k);
        }
        manifests.push(rdy_store::codec::item_new(
            kinds::BLOB_MANIFEST,
            &b.manifest(),
        ));
    }
    chunks.extend(manifests);
    Ok(chunks)
}

/// One change of a port before its briefing: its ops after the missing-base rule, the earlier
/// changes of this port it follows, and the target changes it depends on.
struct Draft {
    ops: Vec<Op>,
    after: BTreeSet<usize>,
    deps: BTreeSet<Hash>,
}

/// The port of `sources` into `target` on `st`, as the new changes of view `view` by `by`
/// (D-1QT23, D-08M5W, D-7AT4S). Each source becomes one new change, in order. Per op, the
/// foreign pin is checked first (in a project target; a holder checks none, D-FK58A), then the
/// missing-base rule: a held base stays; a create or
/// update whose base the target lacks becomes `Op::Create`; such a delete is dropped when the
/// target lacks the block and refused when it holds it. A change keeps its place with no op.
/// Then the briefing, then the parent rule and the parent loop on the merged view. Deps follow
/// replay's rules, except that a create gains no dep: a port never adds a target dep to settle
/// a conflict. Nothing is written.
pub(crate) fn prepare_changes(
    st: &ReadState,
    target: PortTarget<'_>,
    view: &str,
    sources: &[SourceChange<'_>],
    by: &Provenance,
    brief: PortBrief<'_>,
    reg: &BriefRegistry,
) -> Result<PreparedPort, RdyError> {
    let reader = st.reader();
    let (scope, base, anchor, deps_view, included): (Scope, Option<Hash>, Hash, Option<&str>, _) =
        match target {
            PortTarget::Root(tip) => (Scope::At(tip), Some(tip), tip, None, Arc::new(vec![])),
            PortTarget::View(v) => {
                let head = st
                    .view_head(v)
                    .ok_or_else(|| crate::views::no_view(v))?
                    .head;
                (
                    Scope::View(v.to_string()),
                    st.effective_base(v),
                    head,
                    Some(v),
                    st.included(v),
                )
            }
        };
    // a holder holds no foreign entry and checks portable refs only (D-FK58A): a hub carry
    // writes foreign links and pins as written, and the client checks each pin where a person
    // lands (D-DAPR8). A project target keeps the delivery check.
    let check_pins = reader.mode()? != StoreMode::Holder;
    let m = st.materialized(&scope)?;
    let conflicted: HashSet<BlockId> = match target {
        PortTarget::Root(_) => HashSet::new(),
        PortTarget::View(_) => st
            .conflicts(&scope)?
            .iter()
            .map(|c| c.block.clone())
            .collect(),
    };
    let versions = |id: &BlockId| m.version_of(id);
    let mut preview = Preview {
        versions: &versions,
        conflicted,
        slots: HashMap::new(),
    };
    let included_set: HashSet<Hash> = included.iter().copied().collect();
    let index_of: HashMap<Hash, usize> = sources
        .iter()
        .enumerate()
        .map(|(i, s)| (s.from.change, i))
        .collect();
    // the versions this port writes, with their blocks: a base an earlier op wrote is held
    let mut produced: HashMap<Hash, Block> = HashMap::new();
    let mut last_writer: HashMap<BlockId, usize> = HashMap::new();
    let mut drafts: Vec<Draft> = Vec::with_capacity(sources.len());
    for (n, source) in sources.iter().enumerate() {
        let mut ops: Vec<Op> = Vec::with_capacity(source.ops.len());
        let mut after: BTreeSet<usize> = source
            .deps
            .iter()
            .filter_map(|d| index_of.get(d).copied().filter(|i| *i < n))
            .collect();
        let mut deps: BTreeSet<Hash> = BTreeSet::new();
        let mut ordinary: Vec<Op> = Vec::new();
        let mut mine: HashSet<BlockId> = HashSet::new();
        for op in source.ops {
            // D-7AT4S: the foreign pin first, in a project target
            if check_pins && let Op::Create { block } | Op::Update { block, .. } = op {
                st.check_held_pins(&scope, std::iter::once(block))?;
            }
            // then the missing-base rule (D-1QT23)
            let held = |base: &Hash| -> Result<bool, RdyError> {
                Ok(produced.contains_key(base) || reader.has_kind(kinds::BLOCK, base)?)
            };
            let id = op.block_id().clone();
            let prepared = match op {
                Op::Create { .. } => op.clone(),
                Op::Update { base, block } => {
                    if held(base)? {
                        op.clone()
                    } else {
                        Op::Create {
                            block: block.clone(),
                        }
                    }
                }
                Op::Delete { base, .. } => {
                    if held(base)? {
                        op.clone()
                    } else {
                        match preview.get(&id) {
                            Slot::Clean(None) => continue,
                            _ => return Err(delete_base()),
                        }
                    }
                }
            };
            let earlier = last_writer.get(&id).copied();
            if let Some(e) = earlier {
                after.insert(e);
            }
            let new = match &prepared {
                Op::Create { block } | Op::Update { block, .. } => Some(block.version_hash()),
                Op::Delete { .. } => None,
            };
            let slot = match preview.get(&id) {
                Slot::Clean(v) if v == prepared.base() => {
                    // a create gains no target dep: a port never settles a conflict by a dep
                    if earlier.is_none()
                        && !mine.contains(&id)
                        && !matches!(prepared, Op::Create { .. })
                    {
                        ordinary.push(prepared.clone());
                    }
                    Slot::Clean(new)
                }
                Slot::Clean(v) if v == new => {
                    if preview.targets_own(&id) {
                        deps.extend(view_writers(
                            st,
                            &included_set,
                            &source.from.change,
                            &id,
                            v,
                        )?);
                    }
                    Slot::Clean(new)
                }
                _ => Slot::Conflicted,
            };
            preview.slots.insert(id.clone(), slot);
            mine.insert(id);
            if let Op::Create { block } | Op::Update { block, .. } = &prepared {
                produced.insert(block.version_hash(), block.clone());
            }
            ops.push(prepared);
        }
        // a parent an earlier change of this port wrote: the child follows it
        for op in &ops {
            if let Some(p) = placed_under(st, &produced, op)?
                && let Some(w) = last_writer.get(&p)
            {
                after.insert(*w);
            }
        }
        if !ordinary.is_empty() {
            let probe = Change {
                view: view.to_string(),
                ops: ordinary,
                deps: vec![],
                provenance: by.clone(),
                justification: None,
                message: String::new(),
                links: ChangeLinks::default(),
                read_identity: None,
            };
            deps.extend(reader.change_deps(&probe, deps_view)?);
        }
        for op in &ops {
            last_writer.insert(op.block_id().clone(), n);
        }
        drafts.push(Draft { ops, after, deps });
    }
    // the briefing, after every op check
    let all_ops: Vec<&Op> = drafts.iter().flat_map(|d| &d.ops).collect();
    let base_block = |v: &Hash| -> Result<Block, RdyError> {
        match produced.get(v) {
            Some(b) => Ok(b.clone()),
            None => Ok(reader.get_block(v)?),
        }
    };
    let (briefing, record) = port_brief(st, &scope, &m, &all_ops, &base_block, brief, reg)?;
    // the new changes, now that their briefing is known
    let mut changes: Vec<Change> = Vec::with_capacity(drafts.len());
    let mut ported: Vec<PortedChange> = Vec::with_capacity(drafts.len());
    for (n, (draft, source)) in drafts.into_iter().zip(sources).enumerate() {
        let mut deps = draft.deps;
        deps.extend(draft.after.iter().map(|i| ported[*i].change));
        let change = Change {
            view: view.to_string(),
            ops: draft.ops,
            deps: deps.into_iter().collect(),
            provenance: by.clone(),
            justification: source.justification.clone(),
            message: source.message.to_string(),
            links: ChangeLinks {
                intent: None,
                alternative: None,
                client_nonce: Some(format!("port:{anchor}:{n}")),
                imported_by: None,
                replayed_from: None,
                briefing: Some(briefing),
                ported_from: Some(source.from),
            },
            read_identity: None,
        };
        ported.push(PortedChange {
            source: source.from,
            change: change.id(),
        });
        changes.push(change);
    }
    // the merged view: the parent rule, then the parent loop
    let mut list: Vec<Change> = included
        .iter()
        .map(|h| reader.change(h))
        .collect::<Result<_, _>>()?;
    list.extend(changes.iter().cloned());
    let state = reader.state_of(base)?;
    let merged = reader.merge_onto(base, &state, &list)?;
    let in_conflict: BTreeSet<BlockId> = merged.conflicts.iter().map(|c| c.block.clone()).collect();
    let parent_of = |v: &Hash| -> Result<Option<BlockId>, RdyError> {
        Ok(match produced.get(v) {
            Some(b) => b.parent.clone(),
            None => reader.get_block(v)?.parent,
        })
    };
    let mut moved: Vec<BlockId> = Vec::new();
    for c in &changes {
        for op in &c.ops {
            if let Some(p) = placed_under(st, &produced, op)?
                && (!merged.state.blocks.contains_key(&p) || in_conflict.contains(&p))
            {
                return Err(parent_missing(&p));
            }
            if let Op::Update { base, block } = op
                && parent_of(base)? != block.parent
            {
                moved.push(block.id.clone());
            }
        }
    }
    if let Some((a, b)) =
        rdy_store::find_parent_cycle(moved, |id| match merged.state.blocks.get(id) {
            Some(v) => parent_of(v),
            None => Ok(None),
        })?
    {
        return Err(parent_cycle(&a, &b));
    }
    let conflicts: Vec<BlockId> = in_conflict.into_iter().collect();
    Ok(PreparedPort {
        view: view.to_string(),
        base,
        changes,
        ported,
        conflicts,
        briefing,
        record,
        blobs: vec![],
    })
}

/// Whether `op` puts its block under a parent it must find: a create under a parent, or an
/// update whose parent differs from its base version's. The base is read from the port's own
/// writes first, then from the store, which holds every base the missing-base rule kept.
fn placed_under(
    st: &ReadState,
    produced: &HashMap<Hash, Block>,
    op: &Op,
) -> Result<Option<BlockId>, RdyError> {
    Ok(match op {
        Op::Create { block } => block.parent.clone(),
        Op::Update { base, block } => {
            let before = match produced.get(base) {
                Some(b) => b.parent.clone(),
                None => st.reader().get_block(base)?.parent,
            };
            match &block.parent {
                Some(p) if before.as_ref() != Some(p) => Some(p.clone()),
                _ => None,
            }
        }
        Op::Delete { .. } => None,
    })
}

/// The write inputs `ops` stand for, as the briefing rule reads them: a create under its
/// parent, an update with its lifecycle edit and move, a delete. `base_block` reads an
/// update's base version.
fn op_inputs(
    ops: &[&Op],
    base_block: &dyn Fn(&Hash) -> Result<Block, RdyError>,
) -> Result<Vec<OpInput>, RdyError> {
    let mut out = Vec::with_capacity(ops.len());
    for op in ops {
        out.push(match op {
            Op::Create { block } => OpInput::Create {
                block_type: block.block_type.clone(),
                id: Some(block.id.to_string()),
                parent: block.parent.as_ref().map(ToString::to_string),
                after: None,
                before: None,
                fields: BTreeMap::new(),
                body: String::new(),
                links: BTreeMap::new(),
            },
            Op::Update { base, block } => {
                let before = base_block(base)?;
                let mut fields = BTreeMap::new();
                let mut unset = vec![];
                let now = block.field_str("lifecycle");
                if now != before.field_str("lifecycle") {
                    match now {
                        Some(l) => {
                            fields.insert("lifecycle".to_string(), serde_json::json!(l));
                        }
                        None => unset.push("lifecycle".to_string()),
                    }
                }
                let move_to = (before.parent != block.parent).then(|| crate::write::MoveTo {
                    parent: block.parent.as_ref().map(ToString::to_string),
                    after: None,
                    before: None,
                });
                OpInput::Update {
                    id: block.id.to_string(),
                    base: None,
                    fields,
                    unset,
                    body: None,
                    links: None,
                    block_type: None,
                    move_to,
                }
            }
            Op::Delete { id, .. } => OpInput::Delete {
                id: id.to_string(),
                base: None,
            },
        });
    }
    Ok(out)
}

/// The blocks a port's briefing covers, as the ordinary write rule reads `ops` against `m`
/// (the target before the port): each existing block an update or delete touches, and for a
/// create the nearest existing block it hangs from.
fn port_covers(m: &Materialized, ops: &[&Op]) -> BTreeSet<BlockId> {
    let mut anchors: HashMap<BlockId, Option<BlockId>> = HashMap::new();
    let mut out = BTreeSet::new();
    for op in ops {
        match op {
            Op::Create { block } => {
                let anchor = match &block.parent {
                    None => None,
                    Some(p) => match anchors.get(p) {
                        Some(a) => a.clone(),
                        None => m.get(p).map(|_| p.clone()),
                    },
                };
                if let Some(a) = &anchor {
                    out.insert(a.clone());
                }
                anchors.insert(block.id.clone(), anchor);
            }
            Op::Update { block, .. } => {
                if m.get(&block.id).is_some() {
                    out.insert(block.id.clone());
                }
            }
            Op::Delete { id, .. } => {
                if m.get(id).is_some() {
                    out.insert(id.clone());
                }
            }
        }
    }
    out
}

/// The target briefing of a port's `ops` on `m` (the target in `scope` before the port). With
/// [`PortBrief::Make`], core records a briefing of the blocks the write rule names
/// ([`port_covers`], with every stable subject) through `brief::brief_record_in`, at the target,
/// and registers it in `reg`. With [`PortBrief::Cite`], the cited token must be known and not
/// stale, for drafts too. Either way `brief::check` then checks stable coverage on the target.
/// No source token is ever used.
pub fn port_brief(
    st: &ReadState,
    scope: &Scope,
    m: &Materialized,
    ops: &[&Op],
    base_block: &dyn Fn(&Hash) -> Result<Block, RdyError>,
    brief: PortBrief<'_>,
    reg: &BriefRegistry,
) -> Result<(Hash, Option<Arc<crate::brief::BriefRecord>>), RdyError> {
    let inputs = op_inputs(ops, base_block)?;
    let pack = st.pack();
    let (token, record) = match brief {
        PortBrief::Make => {
            let mut covers = port_covers(m, ops);
            covers.extend(crate::brief::stable_subjects(pack, m, &inputs));
            let g = st.graph(scope)?;
            let rec = crate::brief::brief_record_in(m, &g, pack, &covers)?;
            reg.insert(Arc::clone(&rec));
            (rec.token, Some(rec))
        }
        PortBrief::Cite(None) => return Err(needs_briefing()),
        PortBrief::Cite(Some(t)) => {
            let short: String = t.chars().take(8).collect();
            let unknown = || {
                RdyError::new(
                    crate::brief::E_BRIEF_STALE,
                    format!(
                        "the cited briefing {short} is unknown: the daemon restarted, or the briefing was evicted"
                    ),
                    BRIEF_AGAIN_FIX,
                )
            };
            let rec = reg.get(t).ok_or_else(unknown)?;
            let g = st.graph(scope)?;
            let moved = rec.stale(pack, m, &g);
            if !moved.is_empty() {
                let ids: Vec<String> = moved
                    .iter()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .map(ToString::to_string)
                    .collect();
                return Err(RdyError::new(
                    crate::brief::E_BRIEF_STALE,
                    format!(
                        "the cited briefing is stale: {} moved since it was made",
                        ids.join(", ")
                    ),
                    BRIEF_AGAIN_FIX,
                ));
            }
            (rec.token, None)
        }
    };
    let hex = token.to_string();
    crate::brief::check(
        pack,
        reg,
        m,
        || st.graph(scope),
        &inputs,
        &crate::reaffirm::Selection::default(),
        Some(&hex),
    )?;
    Ok((token, record))
}
