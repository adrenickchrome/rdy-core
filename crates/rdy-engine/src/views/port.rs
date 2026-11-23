//! Local and dump ports (D-G4PZX, n-E258F, D-1QT23, D-08M5W, D-7AT4S): landed changes of another
//! project, or of a dump of one, replayed into an open view of this project as new changes.
//!
//! - **Source.** [`read_source`] opens the source for reading only, outside the target's writer.
//!   A project opens through [`Rdy::open_source_read_only`]: nothing in it changes, and it is
//!   never upgraded. A dump loads into a private staging directory, which opens the same way.
//!   Each selection is a landing on the source's canon (its changes, in order) or a change that
//!   canon carries. The changes, their ops, justifications, messages and blobs become an opaque
//!   [`PortSource`]. Every source handle is closed, and staging is removed, before it returns,
//!   on success and on failure.
//! - **Port.** [`Rdy::view_port`] runs under the target's one writer. It checks the view as a
//!   replay does, then the target's removals, then each op and the briefing through the port
//!   preparation (`crate::port::prepare_changes`). It writes the new changes, the blobs the
//!   target lacks and one `Include`, in one batch: one undo removes the whole port.
//!
//! A local source need not share the target's lineage. No source change, landing or briefing
//! enters the target. The replay limits apply: 1,000 changes, 10,000 ops and 16,777,216 encoded
//! bytes of new changes.
use super::replay::{
    MAX_REPLAY_BYTES, MAX_REPLAY_CHANGES, MAX_REPLAY_OPS, ReplayResult, ReplayedChange,
};
use super::{
    Action, Eligibility, check_view_op, conflict_infos, no_change, no_view, owner_of, parse_change,
    strings,
};
use crate::error::*;
use crate::port::{self, PortBrief, PortTarget, SourceChange};
use crate::port_wire::{BundleBlob, BundleChunk};
use crate::read_state::ReadState;
use crate::{Delta, Rdy, Scope, Session};
use rdy_model::{Hash, Justification, Op, PortedFrom, RepoId};
use rdy_store::BlobState;
use rdy_store::kinds;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

/// The name of the project a dump loads into, inside its private staging directory.
const STAGED_PROJECT: &str = "source";

/// One source change of a port: its hash, ops, deps, justification and message.
#[derive(Clone, Debug)]
struct SourceEntry {
    hash: Hash,
    ops: Vec<Op>,
    deps: Vec<Hash>,
    justification: Option<Justification>,
    message: String,
}

/// The selected source data of a local or dump port ([`read_source`]): the source project's
/// repo id, the selected changes in canon order (oldest landing first), and the blobs their ops
/// show. It holds no handle, path or briefing of the source.
#[derive(Clone, Debug)]
pub struct PortSource {
    repo_id: RepoId,
    changes: Vec<SourceEntry>,
    blobs: Vec<BundleBlob>,
    chunks: Vec<BundleChunk>,
}

impl PortSource {
    /// The source project's repo id.
    pub fn repo_id(&self) -> RepoId {
        self.repo_id
    }

    /// The selected source changes, in port order.
    pub fn changes(&self) -> Vec<Hash> {
        self.changes.iter().map(|c| c.hash).collect()
    }
}

/// `E_LIMIT`: the replay limits, which a local or dump port keeps.
fn limit() -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!(
            "replay accepts at most {MAX_REPLAY_CHANGES} changes, {MAX_REPLAY_OPS} ops and \
             {MAX_REPLAY_BYTES} encoded bytes"
        ),
        "replay a smaller set of changes",
    )
}

/// `E_BAD_INPUT`: a source change that the source's canon does not carry.
fn not_landed(c: &Hash, project: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("change {c} has not landed in {project}"),
        "land it there first, or replay it inside that project",
    )
}

/// `E_VIEW_OWNER`: the view is not this session's, as a replay refuses it.
fn not_owned() -> RdyError {
    RdyError::new(
        E_VIEW_OWNER,
        "replay needs a view owned by this session",
        "use your own view, or ask the owner to hand it to you",
    )
}

#[cfg(feature = "test-support")]
thread_local! {
    /// Tests only: called with each staging directory a dump port makes on this thread.
    static ON_STAGING: std::cell::RefCell<Option<Box<dyn FnMut(&Path)>>> =
        const { std::cell::RefCell::new(None) };
}

/// Tests only: `hook` runs with the path of each staging directory a dump port makes on this
/// thread, once the dump has loaded there.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub fn on_staging_for_tests(hook: Box<dyn FnMut(&Path)>) {
    ON_STAGING.with(|h| *h.borrow_mut() = Some(hook));
}

fn staged(_dir: &Path) {
    #[cfg(feature = "test-support")]
    ON_STAGING.with(|h| {
        if let Some(hook) = h.borrow_mut().as_mut() {
            hook(_dir);
        }
    });
}

/// Opens the source at `path` for reading only: a project directory through
/// [`Rdy::open_source_read_only`], any other path as a dump loaded into a new private staging
/// directory, which then opens the same way. Returns the handle and the staging directory. A
/// failure removes the staging directory before it returns.
pub fn open_port_source(path: &Path) -> Result<(Rdy, Option<tempfile::TempDir>), RdyError> {
    if path.is_dir() {
        return Ok((Rdy::open_source_read_only(path)?, None));
    }
    let staging = tempfile::Builder::new()
        .prefix(".rdy-port-")
        .tempdir()
        .map_err(crate::handle::io)?;
    let project = staging.path().join(STAGED_PROJECT);
    let opened =
        crate::load::load(path, &project).and_then(|_| Rdy::open_source_read_only(&project));
    staged(staging.path());
    match opened {
        Ok(r) => Ok((r, Some(staging))),
        Err(e) => {
            // the load closed what it opened; the directory goes now
            let _ = staging.close();
            Err(e)
        }
    }
}

/// Reads the port source at `path` (a project or a dump): the landed changes `selections`
/// name, each a landing on its canon or a change that canon carries. The input is checked
/// before the source opens: at least one entry, at most [`MAX_REPLAY_CHANGES`] counted before
/// duplicates collapse, each a full hash. The source is closed, and a dump's staging removed,
/// before this returns, on success and on failure.
pub fn read_source(path: &Path, selections: &[String]) -> Result<PortSource, RdyError> {
    if selections.is_empty() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "name at least one change to replay",
            "call rdy_view with op replay and changes, or rdy view replay <view> <change>...",
        ));
    }
    if selections.len() > MAX_REPLAY_CHANGES {
        return Err(limit());
    }
    let mut wanted: Vec<Hash> = Vec::new();
    wanted
        .try_reserve_exact(selections.len())
        .map_err(|_| limit())?;
    let mut seen = HashSet::new();
    for text in selections {
        let h = parse_change(text)?;
        if seen.insert(h) {
            wanted.push(h);
        }
    }
    let label = path.display().to_string();
    let (r, staging) = open_port_source(path)?;
    let read = {
        let st = r.read();
        prepare_project_port(&st, &label, &wanted)
    };
    let closed = r.close();
    let removed = match staging {
        Some(dir) => dir.close().map_err(crate::handle::io),
        None => Ok(()),
    };
    let source = read?;
    closed?;
    removed?;
    Ok(source)
}

/// The port source of `wanted` (distinct, in input order) in the closed source's state `st`,
/// named `project` in refusals. A landing on canon gives its changes. Any other entry must be a
/// stored change (else `E_NOT_FOUND`) that the source's own history reaches (D-99CWC: else
/// `E_NOT_FOUND`, as a replay refuses evidence) and that canon carries (else `E_BAD_INPUT`,
/// "has not landed"). Then, oldest first: the change count, each change's stored bytes, a
/// removed projection, the op count, and each blob an op shows (a removed blob refuses).
pub fn prepare_project_port(
    st: &ReadState,
    project: &str,
    wanted: &[Hash],
) -> Result<PortSource, RdyError> {
    st.ensure_current_epoch()?;
    let reader = st.reader();
    // canon's landings, oldest first, and where each landed change sits
    let mut canon = match st.canon_head() {
        Some(tip) => crate::history_facts::walk(st, tip)?,
        None => vec![],
    };
    canon.reverse();
    let mut landings: HashMap<Hash, Vec<Hash>> = HashMap::new();
    let mut place: HashMap<Hash, (usize, usize)> = HashMap::new();
    for (i, l) in canon.iter().enumerate() {
        let changes = reader.landing(l)?.changes;
        for (j, c) in changes.iter().enumerate() {
            place.entry(*c).or_insert((i, j));
        }
        landings.insert(*l, changes);
    }
    let mut sources_ok = Eligibility::lazy(st);
    let mut selected: HashSet<Hash> = HashSet::new();
    for h in wanted {
        if let Some(changes) = landings.get(h) {
            selected.extend(changes.iter().copied());
            continue;
        }
        if !reader.has_kind(kinds::CHANGE, h)? {
            return Err(no_change(h));
        }
        sources_ok.check(reader, h)?;
        if !place.contains_key(h) {
            return Err(not_landed(h, project));
        }
        selected.insert(*h);
    }
    if selected.len() > MAX_REPLAY_CHANGES {
        return Err(limit());
    }
    let mut order: Vec<Hash> = selected.into_iter().collect();
    order.sort_by_key(|c| place[c]);
    let mut changes: Vec<SourceEntry> = Vec::with_capacity(order.len());
    let mut stored = 0u64;
    let mut ops = 0usize;
    let mut blobs: BTreeMap<Hash, BundleBlob> = BTreeMap::new();
    let mut chunks: BTreeMap<Hash, Vec<u8>> = BTreeMap::new();
    for c in order {
        stored = stored
            .checked_add(reader.stored_len(kinds::CHANGE, &c)?)
            .filter(|b| *b <= MAX_REPLAY_BYTES as u64)
            .ok_or_else(limit)?;
        let change = reader.change(&c)?;
        reader.refuse_copy_of(&change)?;
        ops = ops
            .checked_add(change.ops.len())
            .filter(|n| *n <= MAX_REPLAY_OPS)
            .ok_or_else(limit)?;
        for op in &change.ops {
            let (Op::Create { block } | Op::Update { block, .. }) = op else {
                continue;
            };
            for h in crate::port_wire::block_blobs(block) {
                if blobs.contains_key(&h) {
                    continue;
                }
                let blob = match reader.blob_state(&h)? {
                    BlobState::Missing => return Err(rdy_store::StoreError::NotFound(h).into()),
                    BlobState::Removed(_) => return Err(port::source_blob_removed()),
                    BlobState::Readable => read_manifest(st, &h)?,
                };
                for k in &blob.chunks {
                    if chunks.contains_key(k) {
                        continue;
                    }
                    let bytes = reader.get_kind(kinds::BLOB_CHUNK, k)?;
                    if BundleChunk::hash_of(&bytes) != *k {
                        return Err(crate::port_wire::damaged("blob chunk", k));
                    }
                    chunks.insert(*k, bytes);
                }
                blobs.insert(h, blob);
            }
        }
        changes.push(SourceEntry {
            hash: c,
            ops: change.ops,
            deps: change.deps,
            justification: change.justification,
            message: change.message,
        });
    }
    Ok(PortSource {
        repo_id: st.repo_id()?,
        changes,
        blobs: blobs.into_values().collect(),
        chunks: chunks
            .into_iter()
            .map(|(hash, bytes)| BundleChunk { hash, bytes })
            .collect(),
    })
}

/// The manifest of blob `h` as the source holds it, checked against its hash.
fn read_manifest(st: &ReadState, h: &Hash) -> Result<BundleBlob, RdyError> {
    let (raw, payload) = st.reader().get_kind_raw(kinds::BLOB_MANIFEST, h)?;
    let o = rdy_store::codec::split(raw, &payload)
        .map_err(|_| crate::port_wire::damaged("blob manifest", h))?;
    let m: kinds::BlobManifest = rdy_store::codec::decode_split(&o)?;
    if BundleBlob::hash_of(m.len, &m.chunks) != *h {
        return Err(crate::port_wire::damaged("blob manifest", h));
    }
    Ok(BundleBlob {
        hash: *h,
        len: m.len,
        chunks: m.chunks,
    })
}

/// The removal check of a local port (D-3JN9N), as a hub take checks a bundle: every block
/// version, base, manifest and chunk the source holds, used by the prepared ops or not and
/// held already or not, against the target's own removal records.
fn check_removals(st: &ReadState, source: &PortSource) -> Result<(), RdyError> {
    let reader = st.reader();
    if !reader.any_removal() {
        return Ok(());
    }
    let removed = |kind: u8, h: &Hash| -> Result<(), RdyError> {
        match reader.redaction_of(kind, h)? {
            Some(_) => Err(port::target_removed()),
            None => Ok(()),
        }
    };
    for c in &source.changes {
        for op in &c.ops {
            if let Op::Create { block } | Op::Update { block, .. } = op {
                removed(kinds::BLOCK, &block.version_hash())?;
            }
            if let Some(base) = op.base() {
                removed(kinds::BLOCK, &base)?;
            }
        }
    }
    for b in &source.blobs {
        removed(kinds::BLOB_MANIFEST, &b.hash)?;
    }
    for k in &source.chunks {
        removed(kinds::BLOB_CHUNK, &k.hash)?;
    }
    Ok(())
}

/// The frames of the source's blobs that the target lacks: each chunk it lacks once, then each
/// manifest it lacks.
fn missing_blobs(st: &ReadState, source: &PortSource) -> Result<Vec<(u8, Vec<u8>)>, RdyError> {
    let reader = st.reader();
    let by_hash: HashMap<Hash, &BundleChunk> = source.chunks.iter().map(|k| (k.hash, k)).collect();
    let mut chunks: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut manifests: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut added: HashSet<Hash> = HashSet::new();
    for b in &source.blobs {
        if reader.has_blob(&b.hash)? {
            continue;
        }
        for k in &b.chunks {
            if added.contains(k) || reader.has_kind(kinds::BLOB_CHUNK, k)? {
                continue;
            }
            let chunk = by_hash
                .get(k)
                .ok_or_else(|| crate::port_wire::damaged("blob chunk", k))?;
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

impl Rdy {
    /// Ports `source` ([`read_source`]) into `view`, an existing open view the session owns, as
    /// new changes authored by the session, each citing the target briefing `brief` (see the
    /// module docs). The checks, in order: the view rules a replay keeps, the read epoch, the
    /// target's removals, then per op the foreign pin and the missing-base rule, the briefing,
    /// the parent rule and the parent loop (`crate::port::prepare_changes`), then the new
    /// changes' encoded bytes. One batch stores the new changes, the blobs the target lacks and
    /// one `Include`. A refusal writes nothing.
    pub fn view_port(
        &mut self,
        s: &Session,
        view: &str,
        source: &PortSource,
        brief: Option<&str>,
    ) -> Result<ReplayResult, RdyError> {
        let st = self.read();
        check_view_op(&st, s, view, Action::Replay)?;
        let target = st.view_head(view).ok_or_else(|| no_view(view))?;
        if target.owner.as_ref() != Some(&owner_of(s)) {
            return Err(not_owned());
        }
        st.ensure_current_epoch()?;
        check_removals(&st, source)?;
        let by = s.provenance();
        let sources: Vec<SourceChange<'_>> = source
            .changes
            .iter()
            .map(|c| SourceChange {
                from: PortedFrom {
                    repo_id: source.repo_id,
                    change: c.hash,
                },
                ops: &c.ops,
                deps: &c.deps,
                justification: &c.justification,
                message: &c.message,
            })
            .collect();
        let prepared = port::prepare_changes(
            &st,
            PortTarget::View(view),
            view,
            &sources,
            &by,
            PortBrief::Cite(brief),
            &self.briefs(),
        )?;
        // the new changes' exact bytes, as a replay counts them
        let mut bytes = 0usize;
        for c in &prepared.changes {
            let (_, payload) = rdy_store::codec::item(kinds::CHANGE, c)?;
            bytes = bytes
                .checked_add(payload.len())
                .filter(|b| *b <= MAX_REPLAY_BYTES)
                .ok_or_else(limit)?;
        }
        let blobs = missing_blobs(&st, source)?;
        drop(st);
        let (include, ids) = self
            .store
            .view_port_prepared(view, &prepared.changes, &blobs, &by)?;
        debug_assert_eq!(
            ids,
            prepared.ported.iter().map(|p| p.change).collect::<Vec<_>>()
        );
        self.publish(Delta::View {
            view: view.to_string(),
        })?;
        let st = self.read();
        Ok(ReplayResult {
            view: view.to_string(),
            head: include.to_string(),
            changes: st.included(view).len(),
            replayed: prepared
                .ported
                .iter()
                .map(|p| ReplayedChange {
                    source: p.source.change.to_string(),
                    change: p.change.to_string(),
                })
                .collect(),
            added: strings(&ids),
            added_deps: vec![],
            removed: vec![],
            cascaded: vec![],
            conflicts: conflict_infos(&st, &st.conflicts(&Scope::View(view.to_string()))?)?,
        })
    }
}
