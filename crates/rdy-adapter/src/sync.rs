//! A hub's canon and view moves through the adapter (D-DZDB7, D-R80JP, D-47HVF, D-4XHPH,
//! D-4E32E, D-WDKWQ).
//!
//! A hub takes a push in two steps, as the publication take does:
//!
//! 1. It checks the push's batch files on one snapshot ([`Snapshot::check_quarantine`]), or
//!    names a head the holder already stores ([`Snapshot::check_stored_head`]). The result is a
//!    [`CheckedQuarantine`], bound to that snapshot. It is no raw store handle: it holds the
//!    checked objects and their facts ([`QuarantineFacts`]): the repo, the purpose, the pushed
//!    head, the pushed view and its root, the objects, and the minimum rdy before and after a
//!    move (D-4XHPH). [`Snapshot::canon_chain`] reads the chain facts ([`ChainFacts`]) on the
//!    same snapshot, and [`Snapshot::check_pushed_landing`] checks each pushed landing
//!    (D-0WWYR). No such call writes to the holder.
//! 2. It chooses a move ([`CanonMove`]) and the tip or head it expects, and calls
//!    [`StoreHandle::move_canon`]. Under the store's one writer, core checks that no removal
//!    has run since the snapshot, checks the objects again when the store moved since, compares
//!    the expected tip or head, checks the move's chain rule and writes the objects and the
//!    move's refs in one batch. A compare that fails is [`Moved::Behind`] with the tip or head
//!    it found: nothing is written and it is no error. A chain rule that fails is `E_BAD_INPUT`
//!    with D-47HVF's reason. No call writes without that compare.
//!
//! The batch files stay the hub's: core reads them and never unlinks them. The hub chooses the
//! move; core checks the move it is given and never chooses between them. The view reads
//! ([`Snapshot::view_heads`], [`Snapshot::view_head`]) give the heads the snapshot captured
//! with its other refs: a later move never changes them, and a holder rebases no view.

use crate::store::{Snapshot, StoreHandle};
use rdy_engine::engine::with_writer_result;
use rdy_engine::stream_error::to_rdy;
use rdy_engine::{E_BAD_INPUT, E_INTERNAL, E_IO, RdyError, ReadState, stream_check};
use rdy_model::{Hash, LandingRecord, RepoId};
use rdy_store::format::{FEATURE_HUB_CANON, StoreFeatures, registered_features, required_features};
use rdy_store::hub_canon::MoveBatch;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::stream_error::{Reason, StreamError};
use rdy_store::sync_stream::{
    self, MAX_BATCH_FILES, MAX_HISTORY_DEPTH, StreamHeader, StreamPurpose, StreamReader,
    StreamRecord,
};
use rdy_store::view::ViewOp;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub use rdy_store::format::RdyVersion;
pub use rdy_store::hub_canon::{CanonMove, Moved};
pub use rdy_store::view::Owner;

/// The view a view push names: its name and the root of its chain, the first op, which guards
/// the name on the hub (D-4E32E).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushedView {
    pub name: String,
    pub root: Hash,
}

/// What a checked quarantine holds, read before the hub's verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuarantineFacts {
    /// The repo of the push: the holder's.
    pub repo_id: RepoId,
    /// `canon_history` for a landing head, `view_history` for a view op head.
    pub purpose: StreamPurpose,
    /// The head the push names.
    pub head: Hash,
    /// For a view head, the view and its root.
    pub view: Option<PushedView>,
    /// The distinct keys of the push's objects, sorted. Empty for a stored head.
    pub objects: Vec<ObjectKey>,
    /// The holder's minimum rdy on the snapshot (D-4XHPH).
    pub min_rdy_before: Option<RdyVersion>,
    /// The holder's minimum rdy after a move of this push: the features its objects need and
    /// `hub_canon`, which every move lists, merged into the snapshot's features.
    pub min_rdy_after: Option<RdyVersion>,
}

/// A push checked on one snapshot (see the module docs). Built only by
/// [`Snapshot::check_quarantine`] and [`Snapshot::check_stored_head`]; used by
/// [`Snapshot::canon_chain`] on that snapshot and by [`StoreHandle::move_canon`].
pub struct CheckedQuarantine {
    state: Arc<ReadState>,
    batch: Arc<MoveBatch>,
    /// The pushed landings by hash, with their `prev`, for the chain facts.
    landings: BTreeMap<Hash, Option<Hash>>,
    facts: QuarantineFacts,
    /// The push's batch files (cloned handles; none for a stored head), for the scratch holder
    /// of [`Snapshot::check_pushed_landing`].
    files: Vec<File>,
    /// That scratch holder, made at the first check and kept with the quarantine.
    scratch: Mutex<Option<Arc<Scratch>>>,
}

impl std::fmt::Debug for CheckedQuarantine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckedQuarantine")
            .field("facts", &self.facts)
            .finish_non_exhaustive()
    }
}

impl CheckedQuarantine {
    /// The facts the hub reads before its verdict.
    pub fn facts(&self) -> &QuarantineFacts {
        &self.facts
    }

    /// Whether `snap` is the snapshot this quarantine was checked on.
    pub(crate) fn bound_to(&self, snap: &Snapshot) -> bool {
        Arc::ptr_eq(&self.state, snap.shared_state())
    }

    /// Whether the push's own records hold landing `landing`.
    pub(crate) fn pushes(&self, landing: &Hash) -> bool {
        self.landings.contains_key(landing)
    }

    /// The scratch holder of this quarantine on `snap`, its snapshot: made at the first call
    /// ([`Scratch::make`]) and kept.
    pub(crate) fn scratch(&self, snap: &Snapshot) -> Result<Arc<Scratch>, RdyError> {
        let mut slot = self.scratch.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = slot.as_ref() {
            return Ok(Arc::clone(s));
        }
        let s = Arc::new(Scratch::make(snap, &self.files)?);
        *slot = Some(Arc::clone(&s));
        Ok(s)
    }
}

/// A holder in a private temporary directory that holds what the snapshot's holder holds for a
/// landing check (its canon history and its held chains, exported from the snapshot) and the
/// push of a checked quarantine, taken by the same checks and moves as on the holder. The
/// landing check reads a quarantined landing there, so the holder itself stores nothing before
/// the hub's move. The directory is removed when the scratch drops.
pub(crate) struct Scratch {
    dir: PathBuf,
    handle: Option<StoreHandle>,
    snap: Option<Snapshot>,
}

impl Scratch {
    /// The scratch of `files`, a push checked on `snap`.
    fn make(snap: &Snapshot, files: &[File]) -> Result<Scratch, RdyError> {
        let dir = scratch_dir()?;
        let mut out = Scratch {
            dir,
            handle: None,
            snap: None,
        };
        let h = StoreHandle::create_holder(&out.dir.join("holder"), snap.identity().repo_id)?;
        let h = out.handle.insert(h);
        let empty = BTreeSet::new();
        let export = |root: Hash, base: Option<Hash>, name: &str| -> Result<File, RdyError> {
            let path = out.dir.join(name);
            let f = File::create(&path).map_err(io)?;
            snap.write_history(StreamPurpose::CanonHistory, root, base, &empty, f)?;
            File::open(&path).map_err(io)
        };
        let take = |files: &[File], mv: CanonMove| -> Result<(), RdyError> {
            let s = h.snapshot()?;
            let q = s.check_quarantine(files)?;
            let mv = match (mv, s.canon()) {
                (CanonMove::Force, Some(_)) if s.canon_chain(&q)?.holds_expected => {
                    CanonMove::Extend
                }
                (CanonMove::Force, None) => CanonMove::Seed,
                (mv, _) => mv,
            };
            match h.move_canon(&q, mv, s.canon())? {
                Moved::Moved { .. } => Ok(()),
                Moved::Behind { tip } => Err(RdyError::new(
                    E_INTERNAL,
                    format!("the scratch holder of a landing check moved to {tip:?}"),
                    "report this as a bug in rdy",
                )),
            }
        };
        if let Some(tip) = snap.canon() {
            take(&[export(tip, None, "canon.rdysync")?], CanonMove::Seed)?;
            for (i, held) in snap.captured_held().iter().enumerate() {
                let s = h.snapshot()?;
                let key = ObjectKey {
                    kind: kinds::LANDING,
                    hash: *held,
                };
                if s.has_objects(&[key])?.iter().any(|p| p.present) {
                    continue;
                }
                take(
                    &[export(*held, Some(tip), &format!("held-{i}.rdysync"))?],
                    CanonMove::Hold,
                )?;
            }
        }
        // the push: an extend when it holds the scratch tip, else a force or the seed
        take(files, CanonMove::Force)?;
        out.snap = Some(h.snapshot()?);
        Ok(out)
    }

    /// The scratch holder's snapshot, with the push taken.
    pub(crate) fn snapshot(&self) -> &Snapshot {
        self.snap.as_ref().expect("a made scratch has its snapshot")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        self.snap = None;
        if let Some(h) = self.handle.take() {
            let _ = h.close();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A new private directory for a scratch holder, under the system's temporary directory.
fn scratch_dir() -> Result<PathBuf, RdyError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir();
    loop {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = base.join(format!("rdy-landing-check-{}-{n}", std::process::id()));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io(e)),
        }
    }
}

/// The chain facts of a pushed landing head over the snapshot's canon (ruling 2026-11-15). A
/// landing reads as landed here when canon's chain holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainFacts {
    /// The snapshot's canon tip; None before the seed.
    pub head: Option<Hash>,
    /// The landings that canon's chain would gain if the pushed head became the tip, oldest
    /// first.
    pub entered: Vec<Hash>,
    /// Whether the pushed chain holds `head`. False when there is no canon.
    pub holds_expected: bool,
    /// The landings that canon's chain would lose under a force, oldest first.
    pub replaced: Vec<Hash>,
}

/// One view head as the snapshot captured it (D-4E32E): the view's name, the root of its chain,
/// the head op, the base landing, the size of the included set, the owner and the pin of this
/// store (a holder's heads have neither), whether the view is closed, and its last write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewRead {
    pub name: String,
    pub root: Hash,
    pub head: Hash,
    pub base: Option<Hash>,
    pub len: u64,
    pub owner: Option<Owner>,
    pub pinned: bool,
    pub closed: bool,
    pub last_write_ms: i64,
}

/// `E_BAD_INPUT`: a checked quarantine read on another snapshot than the one that checked it.
fn other_snapshot() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "the checked quarantine is bound to another snapshot",
        "read its chain facts on the snapshot that checked it",
    )
}

/// `E_BAD_INPUT`: a checked quarantine of another store.
fn other_store() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "the checked quarantine belongs to another store",
        "check the batch files on a snapshot of this store",
    )
}

/// `E_BAD_INPUT`: a dropped record in a push.
fn dropped(key: &ObjectKey) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("a move batch cannot hold the dropped object {key}"),
        "push the history from a store that holds the object",
    )
}

fn io(e: std::io::Error) -> RdyError {
    RdyError::new(
        E_IO,
        format!("stream input: {e}"),
        "check that the stream file can be read",
    )
}

/// A push read: its header, its present records as (kind, bytes), and their keys.
type PushRead = (StreamHeader, Vec<(u8, Vec<u8>)>, Vec<ObjectKey>);

/// The checked header and present records of the push `batches` for the holder of `repo`: each
/// file passes core's batch check, the headers agree, and every record is a canon or view
/// history's present object. Repeated records are one.
fn read_push(batches: &[File], repo: RepoId) -> Result<PushRead, RdyError> {
    if batches.is_empty() {
        return Err(to_rdy(StreamError::bad_record(0, 0, Reason::NoBatchFiles)));
    }
    if batches.len() > MAX_BATCH_FILES {
        return Err(to_rdy(StreamError::limit(
            None,
            0,
            sync_stream::LIMIT_BATCH_FILES,
        )));
    }
    let mut first: Option<StreamHeader> = None;
    let mut records: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut keys: BTreeSet<ObjectKey> = BTreeSet::new();
    for f in batches {
        let mut f = f.try_clone().map_err(io)?;
        f.seek(SeekFrom::Start(0)).map_err(io)?;
        let report = stream_check::check_batch(&mut f, Some(repo))?;
        let header = report.header;
        if !matches!(
            header.purpose,
            StreamPurpose::CanonHistory | StreamPurpose::ViewHistory
        ) {
            return Err(to_rdy(StreamError::purpose_for(
                None,
                0,
                header.purpose.byte(),
                StreamPurpose::CanonHistory.name(),
            )));
        }
        match &first {
            None => first = Some(header.clone()),
            Some(h) => {
                if (h.repo_id, h.purpose, h.tag, &h.features)
                    != (header.repo_id, header.purpose, header.tag, &header.features)
                {
                    return Err(to_rdy(StreamError::bad_record(
                        0,
                        sync_stream::MAGIC.len() as u64 + 4,
                        Reason::BatchHeaderDiffers,
                    )));
                }
            }
        }
        // the file passed: read its records again, each checked again by the reader
        f.seek(SeekFrom::Start(0)).map_err(io)?;
        let (mut reader, _) = StreamReader::open_history(&mut f).map_err(to_rdy)?;
        while let Some(r) = reader.next().map_err(to_rdy)? {
            let key = r.key();
            let raw = r.raw_kind();
            match r {
                StreamRecord::Present { payload, .. } => {
                    if keys.insert(key) {
                        if records.len() as u64 >= sync_stream::MAX_RECORDS {
                            return Err(to_rdy(StreamError::limit(
                                None,
                                0,
                                rdy_store::stream_error::LIMIT_OBJECT_RECORDS,
                            )));
                        }
                        records.push((raw, payload));
                    }
                }
                StreamRecord::Dropped { .. } => return Err(dropped(&key)),
            }
        }
    }
    let header = first.expect("at least one batch file");
    Ok((header, records, keys.into_iter().collect()))
}

impl Snapshot {
    /// Checks the push `batches` (one canon or view history of format 4, in one or more files of
    /// one header) as one move batch over this snapshot's store, a holder (see the module docs).
    /// Each file passes core's batch check for the holder's repo (`E_REPO_MISMATCH` otherwise);
    /// the files name one header (`batch header differs`); a dropped record is refused; every
    /// object's links resolve in the push or the holder; and each pushed landing has its one
    /// snapshot delta. The handles are cloned and read from their start; the files stay the
    /// caller's. Nothing is written.
    pub fn check_quarantine(&self, batches: &[File]) -> Result<CheckedQuarantine, RdyError> {
        let read = self.read_state();
        read.ensure_current_epoch()?;
        let repo = self.identity().repo_id;
        let (header, records, objects) = read_push(batches, repo)?;
        let mut need: Vec<rdy_store::format::StoreFeature> = Vec::new();
        let mut landings = BTreeMap::new();
        let mut ops: BTreeMap<Hash, ViewOp> = BTreeMap::new();
        for (raw, payload) in &records {
            need.extend(required_features(*raw, payload));
            let kind = rdy_store::codec::logical_kind(*raw);
            let hash = Hash::of(payload);
            if kind == kinds::LANDING {
                let l: LandingRecord = rdy_store::codec::decode_record(*raw, payload)?;
                landings.insert(hash, l.prev);
            } else if kind == kinds::VIEW_OP {
                let op: ViewOp = rdy_store::codec::decode_record(*raw, payload)?;
                ops.insert(hash, op);
            }
        }
        let batch = read.reader().check_move_batch(records, header.tag)?;
        let files = batches
            .iter()
            .map(File::try_clone)
            .collect::<Result<Vec<_>, _>>()
            .map_err(io)?;
        self.checked(batch, header.purpose, landings, &ops, objects, need, files)
    }

    /// A checked quarantine with no new batch: the head `head` that this snapshot's holder
    /// stores, a landing or a view op (for example the head a hold kept under `held/`). A move
    /// of it runs every check a pushed chain gets. A head the holder lacks is `E_BAD_INPUT`.
    pub fn check_stored_head(&self, head: Hash) -> Result<CheckedQuarantine, RdyError> {
        let read = self.read_state();
        read.ensure_current_epoch()?;
        let batch = read.reader().check_move_batch(Vec::new(), head)?;
        let purpose = if batch.head_kind() == kinds::VIEW_OP {
            StreamPurpose::ViewHistory
        } else {
            StreamPurpose::CanonHistory
        };
        self.checked(
            batch,
            purpose,
            BTreeMap::new(),
            &BTreeMap::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    /// The checked quarantine of `batch` on this snapshot, with its facts.
    fn checked(
        &self,
        batch: MoveBatch,
        purpose: StreamPurpose,
        landings: BTreeMap<Hash, Option<Hash>>,
        ops: &BTreeMap<Hash, ViewOp>,
        objects: Vec<ObjectKey>,
        mut need: Vec<rdy_store::format::StoreFeature>,
        files: Vec<File>,
    ) -> Result<CheckedQuarantine, RdyError> {
        let read = self.read_state();
        let head = batch.head();
        let head_purpose = if batch.head_kind() == kinds::VIEW_OP {
            StreamPurpose::ViewHistory
        } else {
            StreamPurpose::CanonHistory
        };
        if head_purpose != purpose {
            return Err(to_rdy(StreamError::purpose_for(
                None,
                0,
                purpose.byte(),
                head_purpose.name(),
            )));
        }
        let view = if purpose == StreamPurpose::ViewHistory {
            Some(pushed_view(read, ops, head)?)
        } else {
            None
        };
        // every move writes a hub ref, so it lists `hub_canon` (`Store::first_use`)
        need.extend(
            registered_features()
                .into_iter()
                .filter(|f| f.name == FEATURE_HUB_CANON),
        );
        let after = self
            .captured_features()
            .merged_min_rdy(&StoreFeatures::of(dedup(need)));
        let facts = QuarantineFacts {
            repo_id: self.identity().repo_id,
            purpose,
            head,
            view,
            objects,
            min_rdy_before: self.min_rdy(),
            min_rdy_after: after,
        };
        read.ensure_current_epoch()?;
        Ok(CheckedQuarantine {
            state: Arc::clone(self.shared_state()),
            batch: Arc::new(batch),
            landings,
            facts,
            files,
            scratch: Mutex::new(None),
        })
    }

    /// The chain facts of `checked`, a landing push checked on this snapshot (ruling
    /// 2026-11-15): the snapshot's canon tip, the landings the pushed chain would add to canon's
    /// chain, whether it holds the tip, and the landings a force would take off it. A read only:
    /// [`StoreHandle::move_canon`] checks the move again under the writer. A quarantine checked
    /// on another snapshot, or a view push, is `E_BAD_INPUT`.
    pub fn canon_chain(&self, checked: &CheckedQuarantine) -> Result<ChainFacts, RdyError> {
        if !Arc::ptr_eq(&checked.state, self.shared_state()) {
            return Err(other_snapshot());
        }
        let read = self.read_state();
        read.ensure_current_epoch()?;
        if checked.facts.purpose != StreamPurpose::CanonHistory {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("the pushed head {} is not a landing", checked.facts.head),
                "read the chain facts of a canon push",
            ));
        }
        let reader = read.reader();
        let prev_of = |h: &Hash| -> Result<Option<Option<Hash>>, RdyError> {
            if let Some(p) = checked.landings.get(h) {
                return Ok(Some(*p));
            }
            match reader.landing(h) {
                Ok(l) => Ok(Some(l.prev)),
                Err(rdy_store::StoreError::NotFound(x)) if x == *h => Ok(None),
                Err(e) => Err(e.into()),
            }
        };
        let pushed = chain(Some(checked.facts.head), &prev_of)?;
        let head = self.canon();
        let canon = chain(head, &prev_of)?;
        let in_pushed: HashSet<Hash> = pushed.iter().copied().collect();
        let in_canon: HashSet<Hash> = canon.iter().copied().collect();
        let entered = pushed
            .iter()
            .rev()
            .filter(|l| !in_canon.contains(l))
            .copied()
            .collect();
        let replaced = canon
            .iter()
            .rev()
            .filter(|l| !in_pushed.contains(l))
            .copied()
            .collect();
        let holds_expected = head.is_some_and(|t| in_pushed.contains(&t));
        read.ensure_current_epoch()?;
        Ok(ChainFacts {
            head,
            entered,
            holds_expected,
            replaced,
        })
    }

    /// The view heads the snapshot captured, in name order (see [`ViewRead`]). A later write
    /// never changes them. A removal since the snapshot is `E_REDACTED_STATE`.
    pub fn view_heads(&self) -> Result<Vec<ViewRead>, RdyError> {
        let read = self.read_state();
        read.ensure_current_epoch()?;
        let mut out = Vec::with_capacity(self.captured_views().len());
        for (name, head) in self.captured_views() {
            out.push(view_read(read, name, head)?);
        }
        read.ensure_current_epoch()?;
        Ok(out)
    }

    /// The captured head of view `name`; None when the snapshot has no such view.
    pub fn view_head(&self, name: &str) -> Result<Option<ViewRead>, RdyError> {
        let read = self.read_state();
        read.ensure_current_epoch()?;
        let out = match self.captured_views().get(name) {
            Some(head) => Some(view_read(read, name, head)?),
            None => None,
        };
        read.ensure_current_epoch()?;
        Ok(out)
    }
}

impl StoreHandle {
    /// Move `mv` of `checked`, expecting the tip or head `expected` (see the module docs), under
    /// the store's one writer: the store must be the one `checked` was made on, with no removal
    /// since its snapshot (`E_REDACTED_STATE`); then core's compare, chain rule and one batch.
    /// Returns [`Moved::Moved`] with the ref value written, or [`Moved::Behind`] with the tip or
    /// head found when the compare failed and nothing was written. A force's `replaced/` mark
    /// takes the time of the calling thread's clock.
    pub fn move_canon(
        &self,
        checked: &CheckedQuarantine,
        mv: CanonMove,
        expected: Option<Hash>,
    ) -> Result<Moved, RdyError> {
        let at_ms = rdy_engine::now_ms();
        let state = Arc::clone(&checked.state);
        let batch = Arc::clone(&checked.batch);
        with_writer_result(self.local(), move |rdy| {
            if state.root() != rdy.root() {
                return Err(other_store());
            }
            state.ensure_current_epoch()?;
            // a move is a batch of its own: one a raw write left open is sealed first
            rdy.seal_open()?;
            let out = rdy.store_mut().move_canon(&batch, &mv, expected, at_ms)?;
            if matches!(out, Moved::Moved { .. }) {
                rdy.invalidate();
            }
            Ok(out)
        })
    }
}

/// A landing's `prev` (outer none: the landing is not found).
type PrevOf<'a> = &'a dyn Fn(&Hash) -> Result<Option<Option<Hash>>, RdyError>;

/// The landings of the chain from `tip` back by `prev`, newest first, as far as `prev_of` finds
/// them, and at most [`MAX_HISTORY_DEPTH`].
fn chain(tip: Option<Hash>, prev_of: PrevOf<'_>) -> Result<Vec<Hash>, RdyError> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut at = tip;
    while let Some(h) = at {
        if out.len() as u64 >= MAX_HISTORY_DEPTH || !seen.insert(h) {
            break;
        }
        let Some(prev) = prev_of(&h)? else {
            break;
        };
        out.push(h);
        at = prev;
    }
    Ok(out)
}

/// The view and root of a pushed view op `head`, through the pushed ops `ops` and the store.
fn pushed_view(
    read: &ReadState,
    ops: &BTreeMap<Hash, ViewOp>,
    head: Hash,
) -> Result<PushedView, RdyError> {
    let op_of = |h: &Hash| -> Result<ViewOp, RdyError> {
        match ops.get(h) {
            Some(op) => Ok(op.clone()),
            None => Ok(read.reader().view_op(h)?),
        }
    };
    let name = op_of(&head)?.view;
    let mut root = head;
    let mut n = 0u64;
    while let Some(prev) = op_of(&root)?.prev {
        n += 1;
        if n >= MAX_HISTORY_DEPTH {
            return Err(to_rdy(StreamError::limit(
                None,
                0,
                rdy_store::stream_error::LIMIT_HISTORY_DEPTH,
            )));
        }
        root = prev;
    }
    Ok(PushedView { name, root })
}

/// The read of view `name`'s captured `head`, with its chain's root.
fn view_read(
    read: &ReadState,
    name: &str,
    head: &rdy_store::view::ViewHead,
) -> Result<ViewRead, RdyError> {
    let root = read.reader().view_chain_root(name, &head.head)?;
    Ok(ViewRead {
        name: name.to_string(),
        root,
        head: head.head,
        base: head.base,
        len: head.len,
        owner: head.owner.clone(),
        pinned: head.pinned,
        closed: head.closed,
        last_write_ms: head.last_write_ms,
    })
}

/// `features` with one entry per name, the first kept.
fn dedup(features: Vec<rdy_store::format::StoreFeature>) -> Vec<rdy_store::format::StoreFeature> {
    let mut seen = HashSet::new();
    features
        .into_iter()
        .filter(|f| seen.insert(f.name.clone()))
        .collect()
}
