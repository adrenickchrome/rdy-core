//! A holder's canon (D-DZDB7, D-R80JP, D-BPYN8): the store feature `hub_canon` opens four ref
//! families in a holder, and five moves write them. A holder without the feature keeps its
//! closed refs (`crate::holder`).
//!
//! | Ref | Value |
//! | --- | --- |
//! | `canon` | the 32-byte hash of a stored landing: hub canon's tip |
//! | `view/<name>` | a [`ViewHead`] with `owner: None` and `pinned: false`, whose head is a stored view op of `<name>` |
//! | `held/<landing>` | empty: the head of a chain of landings that the hub keeps outside canon |
//! | `replaced/<hash>` | a [`Replaced`] mark: the landing or view op `<hash>` left canon's chain or a view's chain |
//!
//! `<landing>` and `<hash>` are 64 lowercase hex digits. A `replaced/` value is the postcard
//! encoding of `{kind: u8, by: Hash, at_ms: i64}`, as a `view/` value is the postcard encoding
//! of its `ViewHead` (D-QCE8M, D-3RSCW). `kind` is 0 for a landing and 1 for a view op; any
//! other byte is damage. `by` is the tip or head that replaced it, of the same kind. `at_ms`
//! comes from the writer's clock. The `held/` and `replaced/` rules hold in projects too.
//!
//! The first holder batch that writes one of the four families lists `hub_canon` in that batch
//! (`Store::first_use`), so no older rdy opens the holder after it. A write, the replay of the
//! log, an ingest and the fsck judge every one of these refs by the same rules: its name, its
//! value, the kind of each object it names, and, in a holder, the feature in force by the end
//! of its batch. Nothing removes a `held/` or `replaced/` ref, and their targets stay roots of
//! the holder check, so a dump, a load, a compaction and a reindex keep every chain they name.
//!
//! # Moves
//!
//! A move takes a checked batch plan ([`MoveBatch`], from [`StoreReader::check_move_batch`]):
//! the objects the push brings and the head it names. [`Store::move_canon`] compares the
//! expected tip or head under the writer, checks the move's chain rule over the plan and the
//! holder, and writes the plan's objects and the move's refs in one batch:
//!
//! - **Seed:** `canon` is absent. The head's chain reaches its first landing.
//! - **Extend:** `canon` is the expected tip `T`, and the head's chain holds `T`.
//! - **Force:** `canon` is `T`, and the head's chain does not hold `T`. Writes `replaced/T`.
//! - **Hold:** `canon` is `T`, and the head's chain holds a landing of canon; or `canon` is
//!   absent, expected as none, and the head's chain reaches its first landing (as a seed's
//!   does), so a later seed of the stored head lands it. Writes only `held/<head>`.
//! - **Set view head:** `view/<name>` is the expected head, or absent. The pushed chain of view
//!   `<name>` holds that head, and its root is the hub view's root.
//!
//! A seed, an extend or a force also names the tags of the canon history it moves to that no
//! `tag/` ref names yet ([`CarriedTag`]), in the same batch, by its caller's tag rule: the tags
//! of every landing of that history's closure, as an export walks it (the chain, each replay
//! source and each attachment landing). A hold names none until a later move makes its chain
//! canon. A plan whose pushed history carries a removal takes it through the store's checked
//! take, under the writer, with the move's refs in the take's one batch
//! ([`Store::move_canon_with`], D-FNRXT).
//!
//! A compare that fails is no error: the move writes nothing and returns
//! [`Moved::Behind`] with the tip or head it found. A chain rule that fails is
//! `StoreError::Refused` (E_BAD_INPUT) with D-47HVF's one reason for the move. The hub chooses
//! the move; core checks the move it is given and never chooses between them.
use crate::error::StoreError;
use crate::format::StoreFeatures;
use crate::holder::StoreMode;
use crate::kinds;
use crate::object_refs::{ObjectKey, portable_kinds, portable_refs_of};
use crate::segment::Location;
use crate::snapshot::{FoldRefusal, SnapshotSource};
use crate::store::{Store, StoreReader};
use crate::take_batch::TakeError;
use crate::view::{ResolvedOp, ViewHead, ViewOp, ViewOpKind};
use rdy_model::{Answer, Block, Hash, LandingRecord, SnapshotDelta, StateSnapshot, Tag};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::sync::Arc;

/// Hub canon's tip: a landing hash.
pub const CANON_REF: &str = "canon";
/// The prefix of a view's head ref.
pub const VIEW_REF: &str = "view/";
/// The prefix of a held chain's ref: `held/<landing>`.
pub const HELD_REF: &str = "held/";
/// The prefix of a replaced mark's ref: `replaced/<hash>`.
pub const REPLACED_REF: &str = "replaced/";

/// The most records one move batch holds (the stream's record bound).
pub const MAX_MOVE_RECORDS: usize = crate::sync_stream::MAX_RECORDS as usize;

/// D-47HVF's reason of a seed whose chain does not reach its first landing.
pub const SEED_REASON: &str = "the pushed chain does not reach its first landing";
/// D-47HVF's reason of a hold whose chain holds no landing of canon.
pub const HOLD_REASON: &str = "the pushed chain holds no landing of canon";

/// D-47HVF's reason of an extend whose chain does not hold canon's tip `tip`.
pub fn extend_reason(tip: &Hash) -> String {
    format!("the pushed chain does not hold canon tip {tip}")
}

/// D-47HVF's reason of a force whose chain holds canon's tip `tip`.
pub fn force_reason(tip: &Hash) -> String {
    format!("the pushed chain holds canon tip {tip}")
}

/// D-47HVF's reason of a view push whose chain does not hold the hub's head `head`.
pub fn view_head_reason(head: &Hash) -> String {
    format!("the pushed chain does not hold view head {head}")
}

/// D-47HVF's reason of a view push whose root `root` is not the root `hub` of view `name`.
pub fn view_root_reason(root: &Hash, hub: &Hash, name: &str) -> String {
    format!("the pushed root {root} is not the root {hub} of view {name}")
}

/// The ref of the held chain whose head is `landing`.
pub fn held_ref(landing: &Hash) -> String {
    format!("{HELD_REF}{landing}")
}

/// The ref of the replaced mark of the landing or view op `hash`.
pub fn replaced_ref(hash: &Hash) -> String {
    format!("{REPLACED_REF}{hash}")
}

/// What a replaced mark names: a landing (0) or a view op (1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplacedKind {
    Landing,
    ViewOp,
}

impl ReplacedKind {
    /// The mark's byte.
    pub fn byte(self) -> u8 {
        match self {
            ReplacedKind::Landing => 0,
            ReplacedKind::ViewOp => 1,
        }
    }

    /// The kind of byte `b`; None for any byte but 0 and 1.
    pub fn of(b: u8) -> Option<ReplacedKind> {
        match b {
            0 => Some(ReplacedKind::Landing),
            1 => Some(ReplacedKind::ViewOp),
            _ => None,
        }
    }

    /// The object kind the mark and its `by` name.
    pub fn object_kind(self) -> u8 {
        match self {
            ReplacedKind::Landing => kinds::LANDING,
            ReplacedKind::ViewOp => kinds::VIEW_OP,
        }
    }
}

/// The value of `replaced/<hash>`: what `<hash>` is, the tip or head that replaced it, and when.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Replaced {
    pub kind: ReplacedKind,
    pub by: Hash,
    pub at_ms: i64,
}

/// The encoded form of [`Replaced`]. Field order is format.
#[derive(Serialize, Deserialize)]
struct ReplacedWire {
    kind: u8,
    by: Hash,
    at_ms: i64,
}

impl Replaced {
    /// The postcard bytes of `{kind: u8, by: Hash, at_ms: i64}`.
    pub fn encode(&self) -> Vec<u8> {
        crate::store::encode(&ReplacedWire {
            kind: self.kind.byte(),
            by: self.by,
            at_ms: self.at_ms,
        })
    }

    /// The mark `bytes` encode: exactly one postcard `{kind, by, at_ms}`, with `kind` 0 or 1.
    /// Anything else is refused with its reason.
    pub fn decode(bytes: &[u8]) -> Result<Replaced, String> {
        let (w, rest): (ReplacedWire, &[u8]) =
            postcard::take_from_bytes(bytes).map_err(|e| format!("not a replaced mark: {e}"))?;
        if !rest.is_empty() {
            return Err(format!(
                "{} trailing bytes after the replaced mark",
                rest.len()
            ));
        }
        let kind = ReplacedKind::of(w.kind)
            .ok_or_else(|| format!("kind {} is not landing (0) or view op (1)", w.kind))?;
        Ok(Replaced {
            kind,
            by: w.by,
            at_ms: w.at_ms,
        })
    }
}

/// The hub family of ref `name`, with the text after its prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HubRef<'a> {
    Canon,
    View(&'a str),
    Held(&'a str),
    Replaced(&'a str),
}

/// The hub family `name` belongs to; None for every other ref.
pub(crate) fn hub_family(name: &str) -> Option<HubRef<'_>> {
    if name == CANON_REF {
        Some(HubRef::Canon)
    } else if let Some(n) = name.strip_prefix(VIEW_REF) {
        Some(HubRef::View(n))
    } else if let Some(h) = name.strip_prefix(HELD_REF) {
        Some(HubRef::Held(h))
    } else {
        name.strip_prefix(REPLACED_REF).map(HubRef::Replaced)
    }
}

/// Whether `name` is in one of the four families `hub_canon` opens in a holder.
pub fn is_hub_ref(name: &str) -> bool {
    hub_family(name).is_some()
}

/// `hex` as a hash: exactly 64 lowercase hex digits.
fn hex_hash(hex: &str) -> Option<Hash> {
    if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    hex.parse().ok()
}

/// The refusal of ref `name` whose name or value breaks its family's rule (`Decode`): damage in
/// the log or the index, or a write the rule refuses.
pub(crate) fn bad_ref(name: &str, why: impl std::fmt::Display) -> StoreError {
    StoreError::Decode(format!("ref {name}: {why}"))
}

/// The refusal of ref `name` that names object `hash` of kind `kind`, which is not stored.
pub(crate) fn missing_target(name: &str, kind: u8, hash: &Hash) -> StoreError {
    let what = match kind {
        kinds::LANDING => "landing".to_string(),
        kinds::VIEW_OP => "view op".to_string(),
        k => format!("object of kind {k}"),
    };
    bad_ref(name, format!("it names {what} {hash}, which is not stored"))
}

/// A view head value of ref `name`, read whole: one postcard `ViewHead` and nothing after it.
fn decode_head_strict(name: &str, value: &[u8]) -> Result<ViewHead, StoreError> {
    let (head, rest): (ViewHead, &[u8]) = postcard::take_from_bytes(value)
        .map_err(|e| bad_ref(name, format!("not a view head: {e}")))?;
    if !rest.is_empty() {
        return Err(bad_ref(
            name,
            format!("{} trailing bytes after the view head", rest.len()),
        ));
    }
    Ok(head)
}

/// The objects ref `name` with value `value` names, each `(kind, hash)`, after the rules of its
/// family that need no store: the name, the value's encoding and its fixed members. In a
/// project, only `held/` and `replaced/` have rules here (canon and views keep their own
/// checks); every other ref names nothing here.
pub(crate) fn ref_targets(
    mode: StoreMode,
    name: &str,
    value: &[u8],
) -> Result<Vec<(u8, Hash)>, StoreError> {
    let Some(family) = hub_family(name) else {
        return Ok(vec![]);
    };
    match family {
        HubRef::Canon | HubRef::View(_) if mode == StoreMode::Project => Ok(vec![]),
        HubRef::Canon => {
            let h = <[u8; 32]>::try_from(value)
                .map_err(|_| bad_ref(name, "the value is not a 32-byte hash"))?;
            Ok(vec![(kinds::LANDING, Hash(h))])
        }
        HubRef::View(view) => {
            if view.is_empty() {
                return Err(bad_ref(name, "the view has no name"));
            }
            let head = decode_head_strict(name, value)?;
            if head.owner.is_some() || head.pinned {
                return Err(bad_ref(
                    name,
                    "a holder's view head has no owner and no pin",
                ));
            }
            let mut out = vec![(kinds::VIEW_OP, head.head)];
            out.extend(head.base.map(|b| (kinds::LANDING, b)));
            Ok(out)
        }
        HubRef::Held(hex) => {
            let h = hex_hash(hex)
                .ok_or_else(|| bad_ref(name, "the name is not 64 lowercase hex digits"))?;
            if !value.is_empty() {
                return Err(bad_ref(
                    name,
                    format!(
                        "the value of a held ref is empty, not {} bytes",
                        value.len()
                    ),
                ));
            }
            Ok(vec![(kinds::LANDING, h)])
        }
        HubRef::Replaced(hex) => {
            let h = hex_hash(hex)
                .ok_or_else(|| bad_ref(name, "the name is not 64 lowercase hex digits"))?;
            let mark = Replaced::decode(value).map_err(|e| bad_ref(name, e))?;
            let k = mark.kind.object_kind();
            Ok(vec![(k, h), (k, mark.by)])
        }
    }
}

/// The move a hub chose (D-47HVF): seed, extend, force, hold, or set the head of view `name`,
/// whose root is `root`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CanonMove {
    Seed,
    Extend,
    Force,
    Hold,
    SetViewHead { name: String, root: Hash },
}

/// What a move did: `Moved` with the ref value it wrote (canon's new tip, the held head, or the
/// view's new head), or `Behind` with the tip or head the compare found (None: absent), when
/// the compare failed and nothing was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moved {
    Moved { tip: Hash },
    Behind { tip: Option<Hash> },
}

/// A checked batch plan: the objects a push brings, each checked against the holder it was
/// checked on, and the head the push names. Built only by [`StoreReader::check_move_batch`];
/// [`Store::move_canon`] checks it again when the holder moved since.
#[derive(Clone, Debug)]
pub struct MoveBatch {
    records: Vec<(u8, Vec<u8>)>,
    head: Hash,
    head_kind: u8,
    checked_at: Location,
    /// A pushed history that carries a removal (D-FNRXT): its batch files and target. The move
    /// takes it through the store's checked take, with the move's refs in the same batch
    /// ([`Store::move_canon_with`]); `records` then holds its present records only.
    history: Option<Arc<HistoryFiles>>,
}

/// The batch files of a pushed history that carries a removal, and its target.
#[derive(Debug)]
struct HistoryFiles {
    files: Vec<File>,
    target: crate::take_batch::HistoryTarget,
}

impl MoveBatch {
    /// The head the push names: a landing or a view op.
    pub fn head(&self) -> Hash {
        self.head
    }

    /// The kind of the head: `LANDING` or `VIEW_OP`.
    pub fn head_kind(&self) -> u8 {
        self.head_kind
    }

    /// How many objects the plan brings.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether the plan brings no object (a head the holder stores).
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The index point the plan was checked at.
    pub fn checked_at(&self) -> Location {
        self.checked_at
    }

    /// Whether the plan is a pushed history that carries a removal, which the move takes
    /// through the store's checked take.
    pub fn carries_removal(&self) -> bool {
        self.history.is_some()
    }
}

/// A tag a canon move carries: a TAG object of the plan or the holder whose landing is in the
/// canon history the move makes canon, and that no `tag/` ref names (D-KE83X). That history is
/// its closure, as an export walks it (`crate::closure`): the chain, each replay source
/// (`rebased_from`) with its chain, and each attachment landing (a removal proof's human
/// landing, a dispute resolution's correction landing), so the hub names what a push carries.
/// The tag rule of the move's caller ([`Store::move_canon_with`]) chooses which of them the
/// move names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CarriedTag {
    pub hash: Hash,
    pub tag: Tag,
}

/// The refusal of input that a move batch cannot hold (E_BAD_INPUT).
fn refused(m: impl Into<String>) -> StoreError {
    StoreError::Refused(m.into())
}

/// The plan's objects over the holder: what a move reads before it writes.
struct Overlay<'a> {
    reader: &'a StoreReader,
    records: &'a [(u8, Vec<u8>)],
    by_key: HashMap<ObjectKey, usize>,
}

impl<'a> Overlay<'a> {
    fn new(reader: &'a StoreReader, records: &'a [(u8, Vec<u8>)]) -> Overlay<'a> {
        let by_key = records
            .iter()
            .enumerate()
            .map(|(i, (raw, p))| {
                (
                    ObjectKey {
                        kind: crate::codec::logical_kind(*raw),
                        hash: Hash::of(p),
                    },
                    i,
                )
            })
            .collect();
        Overlay {
            reader,
            records,
            by_key,
        }
    }

    fn has(&self, kind: u8, hash: &Hash) -> Result<bool, StoreError> {
        Ok(self.by_key.contains_key(&ObjectKey { kind, hash: *hash })
            || self.reader.has_kind(kind, hash)?)
    }

    /// The raw kind and payload of `kind`/`hash` in the plan, if it is there.
    fn local(&self, kind: u8, hash: &Hash) -> Option<(u8, &[u8])> {
        self.by_key
            .get(&ObjectKey { kind, hash: *hash })
            .map(|i| (self.records[*i].0, &self.records[*i].1[..]))
    }

    fn typed<T: serde::de::DeserializeOwned + 'static>(
        &self,
        kind: u8,
        hash: &Hash,
    ) -> Result<Option<T>, StoreError> {
        if let Some((raw, payload)) = self.local(kind, hash) {
            return crate::codec::decode_record(raw, payload).map(Some);
        }
        match self.reader.get_typed(kind, hash) {
            Ok(v) => Ok(Some(v)),
            Err(StoreError::NotFound(h)) if h == *hash => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn landing(&self, h: &Hash) -> Result<Option<LandingRecord>, StoreError> {
        self.typed(kinds::LANDING, h)
    }

    fn view_op(&self, h: &Hash) -> Result<Option<ViewOp>, StoreError> {
        self.typed(kinds::VIEW_OP, h)
    }
}

/// The landings of the chain from `head` back, newest first, read through `landing`: it ends at
/// the first landing (true), or at a landing it cannot read (false). More than
/// [`crate::check_holder::MAX_LANDING_DEPTH`] landings, or one met twice, ends it as
/// incomplete too.
fn walk_landings(
    head: &Hash,
    landing: &mut dyn FnMut(&Hash) -> Result<Option<LandingRecord>, StoreError>,
) -> Result<(Vec<Hash>, bool), StoreError> {
    let mut out = vec![];
    let mut on = HashSet::new();
    let mut cur = Some(*head);
    while let Some(h) = cur {
        if out.len() >= crate::check_holder::MAX_LANDING_DEPTH || !on.insert(h) {
            return Ok((out, false));
        }
        let Some(rec) = landing(&h)? else {
            return Ok((out, false));
        };
        out.push(h);
        cur = rec.prev;
    }
    Ok((out, true))
}

/// The ops of view `name`'s chain from `head` back to its root, newest first: each op must be
/// an op of `name`, and the root a `Create` or a `Fork`. A broken chain is `Decode`, with the
/// view chain's own texts.
fn walk_view(
    name: &str,
    head: &Hash,
    op: &mut dyn FnMut(&Hash) -> Result<Option<ViewOp>, StoreError>,
) -> Result<Vec<(Hash, ViewOp)>, StoreError> {
    let broken = |at: &Hash, why: String| {
        StoreError::Decode(format!("view chain of {name:?} is broken at {at}: {why}"))
    };
    let mut out: Vec<(Hash, ViewOp)> = vec![];
    let mut on = HashSet::new();
    let mut cur = Some(*head);
    while let Some(h) = cur {
        if out.len() >= MAX_MOVE_RECORDS || !on.insert(h) {
            return Err(broken(&h, "the chain does not end".into()));
        }
        let Some(o) = op(&h)? else {
            return Err(broken(&h, "the op is missing".into()));
        };
        if o.view != name {
            return Err(broken(&h, format!("the op belongs to view {:?}", o.view)));
        }
        cur = o.prev;
        if cur.is_none() && !matches!(o.op, ViewOpKind::Create { .. } | ViewOpKind::Fork { .. }) {
            return Err(broken(&h, "it ends without a checkpoint".into()));
        }
        out.push((h, o));
    }
    Ok(out)
}

/// The hub's head of view `name` at the newest op of `ops` (newest first): its base, its
/// included changes, whether it is closed and its last write, with no owner and no pin. With
/// `from`, the ops follow that head, and its set is `set`; an undo of an op before them reads
/// that op's effect from the holder.
fn hub_head(
    reader: &StoreReader,
    name: &str,
    ops: &[(Hash, ViewOp)],
    from: Option<(&ViewHead, Vec<Hash>)>,
) -> Result<ViewHead, StoreError> {
    let mut undo = |target: &Hash| reader.undo_effect(name, target);
    Ok(head_and_set(name, ops, from, &mut undo)?.0)
}

/// What a view history's root op says of its view (D-V9RF5): the view's name, and its base and
/// included changes at that op.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewAtOp {
    pub name: String,
    pub base: Option<Hash>,
    pub included: Vec<Hash>,
}

/// The view at op `op`, read from the ops that `op_of` gives alone: the chain from `op` back to
/// its first op, with the view's own texts for a broken chain ([`walk_view`]), and the base and
/// included changes the hub's head reads from that chain. An undo whose target the chain does not
/// hold is a broken chain at the target: `the op is missing`.
pub fn view_at_op(
    op: &Hash,
    op_of: &mut dyn FnMut(&Hash) -> Result<Option<ViewOp>, StoreError>,
) -> Result<ViewAtOp, StoreError> {
    let Some(first) = op_of(op)? else {
        return Err(StoreError::NotFound(*op));
    };
    let name = first.view;
    let chain = walk_view(&name, op, op_of)?;
    let mut undo = |target: &Hash| {
        Err(StoreError::Decode(format!(
            "view chain of {name:?} is broken at {target}: the op is missing"
        )))
    };
    let (head, included) = head_and_set(&name, &chain, None, &mut undo)?;
    Ok(ViewAtOp {
        base: head.base,
        name,
        included,
    })
}

/// [`hub_head`] with the included changes, an undo of an op before `ops` read through `undo`.
fn head_and_set(
    name: &str,
    ops: &[(Hash, ViewOp)],
    from: Option<(&ViewHead, Vec<Hash>)>,
    undo: &mut dyn FnMut(&Hash) -> Result<ResolvedOp, StoreError>,
) -> Result<(ViewHead, Vec<Hash>), StoreError> {
    let (mut base, mut set, mut closed, mut last) = match &from {
        Some((h, set)) => (h.base, set.clone(), h.closed, h.last_write_ms),
        None => (None, vec![], false, 0),
    };
    let mut members: HashSet<Hash> = set.iter().copied().collect();
    let mut effects: HashMap<Hash, ResolvedOp> = HashMap::new();
    let include = |set: &mut Vec<Hash>, members: &mut HashSet<Hash>, cs: &[Hash]| {
        for c in cs {
            if members.insert(*c) {
                set.push(*c);
            }
        }
    };
    let exclude = |set: &mut Vec<Hash>, members: &mut HashSet<Hash>, cs: &[Hash]| {
        let gone: HashSet<&Hash> = cs.iter().collect();
        set.retain(|c| !gone.contains(c));
        members.retain(|c| !gone.contains(c));
    };
    for (id, op) in ops.iter().rev() {
        let effect = match &op.op {
            ViewOpKind::Create { base: b, .. } => {
                base = *b;
                set.clear();
                members.clear();
                closed = false;
                ResolvedOp::Nothing
            }
            ViewOpKind::Fork {
                base: b, changes, ..
            } => {
                base = *b;
                set.clear();
                members.clear();
                include(&mut set, &mut members, changes);
                closed = false;
                ResolvedOp::Nothing
            }
            ViewOpKind::Commit { change } => ResolvedOp::Include(vec![*change]),
            ViewOpKind::Include { changes } => ResolvedOp::Include(changes.clone()),
            ViewOpKind::Exclude { changes } => ResolvedOp::Exclude(changes.clone()),
            ViewOpKind::Rebase {
                landing, remaining, ..
            } => {
                base = Some(*landing);
                set.clear();
                members.clear();
                include(&mut set, &mut members, remaining);
                ResolvedOp::Nothing
            }
            ViewOpKind::Close { .. } => ResolvedOp::Close,
            ViewOpKind::Own { owner } => ResolvedOp::SetOwner(owner.clone()),
            ViewOpKind::Undo { target } => match effects.get(target) {
                Some(e) => match e.clone() {
                    ResolvedOp::Include(x) => ResolvedOp::Exclude(x),
                    ResolvedOp::Exclude(x) => ResolvedOp::Include(x),
                    ResolvedOp::Close => ResolvedOp::Reopen,
                    ResolvedOp::Reopen => ResolvedOp::Close,
                    ResolvedOp::SetOwner(_) => ResolvedOp::SetOwner(None),
                    ResolvedOp::Nothing => match ops.iter().find(|(h, _)| h == target) {
                        Some((_, t)) if matches!(t.op, ViewOpKind::Rebase { .. }) => {
                            return Err(StoreError::Decode(format!(
                                "view {name:?}: op {target} is a rebase, which cannot be undone"
                            )));
                        }
                        _ => ResolvedOp::Close,
                    },
                },
                None => undo(target)?,
            },
        };
        match &effect {
            ResolvedOp::Include(x) => include(&mut set, &mut members, x),
            ResolvedOp::Exclude(x) => exclude(&mut set, &mut members, x),
            ResolvedOp::Close => closed = true,
            ResolvedOp::Reopen => closed = false,
            ResolvedOp::SetOwner(_) | ResolvedOp::Nothing => {}
        }
        if !matches!(op.op, ViewOpKind::Close { .. } | ViewOpKind::Own { .. }) {
            last = op.at_ms;
        }
        effects.insert(*id, effect);
    }
    let head = ops
        .first()
        .map(|(h, _)| *h)
        .or(from.as_ref().map(|(h, _)| h.head))
        .ok_or_else(|| StoreError::Decode(format!("view {name:?} has no op")))?;
    let head = ViewHead {
        head,
        base,
        len: set.len() as u64,
        owner: None,
        pinned: false,
        closed,
        last_write_ms: last,
    };
    Ok((head, set))
}

/// The overlay as a map source: the plan's landings and deltas first, then the holder's.
struct OverlaySnapshots<'o, 'a> {
    overlay: &'o Overlay<'a>,
    deltas: BTreeMap<Hash, Hash>,
}

impl SnapshotSource for OverlaySnapshots<'_, '_> {
    type Error = StoreError;

    fn landing(&mut self, landing: &Hash) -> Result<Option<LandingRecord>, StoreError> {
        self.overlay.landing(landing)
    }

    fn held_snapshot(
        &mut self,
        landing: &Hash,
        state: &Hash,
    ) -> Result<Option<StateSnapshot>, StoreError> {
        if self.deltas.contains_key(landing) {
            return Ok(None);
        }
        crate::snapshot::StoreSnapshots {
            reader: self.overlay.reader,
        }
        .held_snapshot(landing, state)
    }

    fn held_snapshot_canonical(&mut self, state: &Hash) -> Result<Option<bool>, StoreError> {
        crate::snapshot::StoreSnapshots {
            reader: self.overlay.reader,
        }
        .held_snapshot_canonical(state)
    }

    fn delta_of(&mut self, landing: &Hash) -> Result<Option<Hash>, StoreError> {
        if let Some(d) = self.deltas.get(landing) {
            return Ok(Some(*d));
        }
        Ok(self
            .overlay
            .reader
            .snapshot_delta_of(landing)?
            .map(|(h, _)| h))
    }

    fn delta(&mut self, delta: &Hash) -> Result<SnapshotDelta, StoreError> {
        self.overlay
            .typed(kinds::SNAPSHOT_DELTA, delta)?
            .ok_or(StoreError::NotFound(*delta))
    }

    fn requires_delta(&mut self) -> Result<bool, StoreError> {
        Ok(true)
    }

    fn refused(&mut self, refusal: FoldRefusal) -> StoreError {
        refused(refusal.detail())
    }
}

impl StoreReader {
    /// The checked batch plan of a push into this holder: `records` (raw kind and payload of
    /// each object, in any order) and the head the push names. Refused (`Refused`, E_BAD_INPUT)
    /// when the store is not a holder, when the plan holds more than [`MAX_MOVE_RECORDS`]
    /// records, a kind no sync batch carries, a removal record or an object whose links do not
    /// decode, when an object names one that neither the plan nor the holder holds, when the
    /// head is not exactly one landing or view op of the plan or the holder, and when a landing
    /// of the plan does not have exactly one snapshot delta that keeps the delta rules
    /// (`landing <landing>: expected exactly one snapshot delta`, D-8T7PA). A holder that does
    /// not list `snapshot_delta` takes no move until its upgrade (`UpgradeRequired`). The
    /// record rules of each kind are the take's; the chain rule is the move's.
    pub fn check_move_batch(
        &self,
        records: Vec<(u8, Vec<u8>)>,
        head: Hash,
    ) -> Result<MoveBatch, StoreError> {
        if self.mode()? != StoreMode::Holder {
            return Err(refused("a move needs a holder store"));
        }
        let features = crate::snapshot::held_features(self)?;
        if !features.has(crate::format::FEATURE_SNAPSHOT_DELTA) {
            return Err(StoreError::UpgradeRequired);
        }
        if records.len() > MAX_MOVE_RECORDS {
            return Err(refused(format!(
                "a move batch holds at most {MAX_MOVE_RECORDS} records"
            )));
        }
        let checked_at = self.index().indexed_upto()?;
        self.check_move_records(&features, &records)?;
        let overlay = Overlay::new(self, &records);
        let is_landing = overlay.has(kinds::LANDING, &head)?;
        let is_op = overlay.has(kinds::VIEW_OP, &head)?;
        let head_kind = match (is_landing, is_op) {
            (true, false) => kinds::LANDING,
            (false, true) => kinds::VIEW_OP,
            _ => {
                return Err(refused(format!(
                    "the pushed head {head} is not one landing or view op of the batch or the holder"
                )));
            }
        };
        Ok(MoveBatch {
            records,
            head,
            head_kind,
            checked_at,
            history: None,
        })
    }

    /// The checked batch plan of a pushed history that carries a removal (D-FNRXT): its
    /// present `records`, the head it names, and its batch `files` (cloned by the caller, read
    /// again from their start by the move) of history purpose `purpose`. Refused as
    /// [`StoreReader::check_move_batch`] refuses a store that is not a holder, a holder that
    /// does not list `snapshot_delta`, too many records, or a head that is not one landing or
    /// view op of the records or the holder. The records' links, the removal's proofs, the
    /// stale-content rule and the closure are the take's checks, which the move runs over the
    /// holder under the writer ([`Store::move_canon_with`]).
    pub fn check_history_move_batch(
        &self,
        records: Vec<(u8, Vec<u8>)>,
        head: Hash,
        files: Vec<File>,
        purpose: crate::sync_stream::StreamPurpose,
    ) -> Result<MoveBatch, StoreError> {
        if self.mode()? != StoreMode::Holder {
            return Err(refused("a move needs a holder store"));
        }
        let features = crate::snapshot::held_features(self)?;
        if !features.has(crate::format::FEATURE_SNAPSHOT_DELTA) {
            return Err(StoreError::UpgradeRequired);
        }
        if records.len() > MAX_MOVE_RECORDS {
            return Err(refused(format!(
                "a move batch holds at most {MAX_MOVE_RECORDS} records"
            )));
        }
        if !purpose.is_history() {
            return Err(refused("a move takes a canon or view history"));
        }
        let checked_at = self.index().indexed_upto()?;
        let overlay = Overlay::new(self, &records);
        let head_kind = match (
            overlay.has(kinds::LANDING, &head)?,
            overlay.has(kinds::VIEW_OP, &head)?,
        ) {
            (true, false) => kinds::LANDING,
            (false, true) => kinds::VIEW_OP,
            _ => {
                return Err(refused(format!(
                    "the pushed head {head} is not one landing or view op of the batch or the holder"
                )));
            }
        };
        drop(overlay);
        Ok(MoveBatch {
            records,
            head,
            head_kind,
            checked_at,
            history: Some(Arc::new(HistoryFiles {
                files,
                target: crate::take_batch::HistoryTarget {
                    purpose,
                    root: head,
                },
            })),
        })
    }

    /// The record checks of a move batch over this holder (see [`StoreReader::check_move_batch`]).
    fn check_move_records(
        &self,
        features: &StoreFeatures,
        records: &[(u8, Vec<u8>)],
    ) -> Result<(), StoreError> {
        // the links of each object follow every feature the batch uses
        let mut used = features.clone();
        for (raw, payload) in records {
            used = used.merged(&crate::format::required_features(*raw, payload));
        }
        let overlay = Overlay::new(self, records);
        let mut deltas: BTreeMap<Hash, Hash> = BTreeMap::new();
        let mut landings: Vec<(Hash, LandingRecord)> = vec![];
        for (raw, payload) in records {
            let kind = crate::codec::logical_kind(*raw);
            let key = ObjectKey {
                kind,
                hash: Hash::of(payload),
            };
            if !crate::sync_stream::is_sync_kind(kind)
                || matches!(
                    kind,
                    kinds::REDACTION | kinds::REDACTION_RESIDUE | kinds::REDACTION_APPLY
                )
            {
                return Err(refused(format!(
                    "a move batch cannot hold object {key}: its kind is not a pushed object"
                )));
            }
            let refs = portable_refs_of(&used, *raw, payload)
                .map_err(|e| refused(format!("object {key}: {e}")))?;
            for r in refs {
                match r.kind {
                    Some(k) => {
                        if !overlay.has(k, &r.hash)? {
                            let child = ObjectKey {
                                kind: k,
                                hash: r.hash,
                            };
                            return Err(refused(format!(
                                "object {key} names {child}, which is not stored"
                            )));
                        }
                    }
                    None => {
                        let mut found = 0;
                        for &k in portable_kinds() {
                            if overlay.has(k, &r.hash)? {
                                found += 1;
                            }
                        }
                        if found != 1 {
                            return Err(refused(format!(
                                "object {key} names {}, which is not stored as exactly one kind",
                                r.hash
                            )));
                        }
                    }
                }
            }
            match kind {
                kinds::LANDING => {
                    let rec: LandingRecord = crate::codec::decode_record(*raw, payload)?;
                    landings.push((key.hash, rec));
                }
                kinds::SNAPSHOT_DELTA => {
                    let d: SnapshotDelta = crate::codec::decode_record(*raw, payload)?;
                    match deltas.get(&d.landing) {
                        Some(h) if *h != key.hash => {
                            return Err(refused(
                                FoldRefusal::MissingDelta { landing: d.landing }.detail(),
                            ));
                        }
                        _ => {
                            deltas.insert(d.landing, key.hash);
                        }
                    }
                }
                _ => {}
            }
        }
        // each landing of the plan: exactly one delta, the plan's or the holder's, never two
        for (l, _) in &landings {
            let held = self.snapshot_delta_of(l)?.map(|(h, _)| h);
            match (deltas.get(l), held) {
                (Some(a), Some(b)) if *a != b => {
                    return Err(refused(FoldRefusal::MissingDelta { landing: *l }.detail()));
                }
                (None, None) => {
                    return Err(refused(FoldRefusal::MissingDelta { landing: *l }.detail()));
                }
                _ => {}
            }
        }
        // the six delta rules of each landing of the plan (D-7K4Z0), the holder's own deltas
        // standing in for the landings before them
        let refs: Vec<(Hash, &LandingRecord)> = landings.iter().map(|(h, r)| (*h, r)).collect();
        let mut source = OverlaySnapshots {
            overlay: &overlay,
            deltas,
        };
        crate::snapshot::check_snapshot_chains(
            &mut source,
            true,
            &refs,
            &mut |s: &mut OverlaySnapshots<'_, '_>, v: &Hash| match s.overlay.local(kinds::BLOCK, v)
            {
                Some((raw, payload)) => {
                    let b: Block = crate::codec::decode_record(raw, payload)?;
                    Ok(Some(b.id))
                }
                None => match s.overlay.reader.get_block(v) {
                    Ok(b) => Ok(Some(b.id)),
                    Err(StoreError::Redacted { .. }) => Ok(None),
                    Err(e) => Err(e),
                },
            },
            &mut |_, _, _, _| Ok(()),
        )
    }

    /// The root of view `name`'s chain at `head`, read from the holder.
    fn view_root(&self, name: &str, head: &Hash) -> Result<Hash, StoreError> {
        let chain = walk_view(name, head, &mut |h| self.typed_or_none(kinds::VIEW_OP, h))?;
        Ok(chain.last().expect("a chain has a root").0)
    }

    fn typed_or_none<T: serde::de::DeserializeOwned + 'static>(
        &self,
        kind: u8,
        h: &Hash,
    ) -> Result<Option<T>, StoreError> {
        match self.get_typed(kind, h) {
            Ok(v) => Ok(Some(v)),
            Err(StoreError::NotFound(x)) if x == *h => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The hub's head of view `name` at op `head`, recomputed from the holder's whole chain:
    /// what a set-view move writes. The fsck compares each `view/` ref of a holder with it.
    pub(crate) fn hub_view_head(&self, name: &str, head: &Hash) -> Result<ViewHead, StoreError> {
        let chain = walk_view(name, head, &mut |h| self.typed_or_none(kinds::VIEW_OP, h))?;
        hub_head(self, name, &chain, None)
    }

    /// The landings of canon's chain, newest first, read from the holder.
    fn canon_landings(&self, tip: &Hash) -> Result<HashSet<Hash>, StoreError> {
        let (chain, _) = walk_landings(tip, &mut |h| self.typed_or_none(kinds::LANDING, h))?;
        Ok(chain.into_iter().collect())
    }
}

/// A move's ref writes and the tags it carries, or the tip or head its failed compare found.
type Planned = Result<(Vec<(String, Vec<u8>)>, Vec<CarriedTag>), Option<Hash>>;

/// The tags a canon move to head `head` carries (see [`CarriedTag`]): each TAG object of the
/// plan or the holder that no `tag/` ref names and whose landing is in the canon history at
/// `head`, in name and hash order. The history's landings are first read cheaply
/// ([`replay_landings`]); only a tag they do not hold reads the whole history
/// ([`history_landings`]), once.
fn carried_tags(
    reader: &StoreReader,
    overlay: &Overlay<'_>,
    head: &Hash,
) -> Result<Vec<CarriedTag>, StoreError> {
    let mut named: HashSet<Hash> = HashSet::new();
    for (_, v) in reader.refs_with_prefix(crate::holder::TAG_REF)? {
        if let Ok(b) = <[u8; 32]>::try_from(v.as_slice()) {
            named.insert(Hash(b));
        }
    }
    let mut hashes: BTreeSet<Hash> = BTreeSet::new();
    for (raw, payload) in overlay.records {
        if crate::codec::logical_kind(*raw) == kinds::TAG {
            hashes.insert(Hash::of(payload));
        }
    }
    reader.for_each_object_of(kinds::TAG, |h| {
        hashes.insert(h);
        Ok(())
    })?;
    let mut candidates = vec![];
    for h in hashes {
        if named.contains(&h) {
            continue;
        }
        let Some(tag) = overlay.typed::<Tag>(kinds::TAG, &h)? else {
            continue;
        };
        candidates.push(CarriedTag { hash: h, tag });
    }
    if candidates.is_empty() {
        return Ok(vec![]);
    }
    let near = replay_landings(overlay, head)?;
    let mut far: Option<HashSet<Hash>> = None;
    let mut out = vec![];
    for c in candidates {
        let on = near.contains(&c.tag.landing) || {
            let f = match &far {
                Some(f) => f,
                None => far.insert(history_landings(reader, overlay, head)?),
            };
            f.contains(&c.tag.landing)
        };
        if on {
            out.push(c);
        }
    }
    out.sort_by(|a, b| (&a.tag.name, a.hash).cmp(&(&b.tag.name, b.hash)));
    Ok(out)
}

/// The landings of the canon history at `head` that its landings' own links reach, read through
/// the plan and the holder: the head's chain and each replay source (`rebased_from`), with
/// their chains, to any depth (D-KE83X: a clean replay keeps its sources, and their tags travel
/// with the history). A landing that neither holds ends its walk.
fn replay_landings(overlay: &Overlay<'_>, head: &Hash) -> Result<HashSet<Hash>, StoreError> {
    let mut seen: HashSet<Hash> = HashSet::new();
    let mut todo = vec![*head];
    while let Some(l) = todo.pop() {
        if seen.contains(&l) {
            continue;
        }
        let Some(rec) = overlay.landing(&l)? else {
            continue;
        };
        seen.insert(l);
        todo.extend(rec.prev);
        todo.extend(rec.rebased_from.iter().copied());
    }
    Ok(seen)
}

/// The kinds that join a canon history as a reverse attachment of a record it holds
/// (`crate::closure`): no forward link reaches them.
const ATTACHMENT_KINDS: [u8; 10] = [
    kinds::SIGNATURE,
    kinds::ANSWER,
    kinds::PLAN,
    kinds::SNAPSHOT_DELTA,
    kinds::REDACTION,
    kinds::REDACTION_APPLY,
    kinds::DISPUTE,
    kinds::DISPUTE_RESOLUTION,
    kinds::CHECK_REQUEST,
    kinds::REVIEW,
];

/// Every landing of the canon history at `head`, as the export walks it (`crate::closure`),
/// over the plan and the holder together, to one fixed point. The plan's part: the records of
/// the plan that the walk reaches through their object links, with each record of an attachment
/// kind that links a record the walk holds, the plan's or the holder's (a removal proof brings
/// its human landing, a dispute resolution its correction landing). The holder's part: the
/// canon-history closure of each held object that a plan record links
/// (`crate::closure::history_keys_before`), with its own attachments. A plan attachment of a
/// held record joins the walk as one of a plan record does: after a take, a pushed removal proof
/// whose request names a version the holder holds below the push brings its human landing and
/// that landing's tags. A holder record links no plan record, so the holder's part needs no
/// plan record.
fn history_landings(
    reader: &StoreReader,
    overlay: &Overlay<'_>,
    head: &Hash,
) -> Result<HashSet<Hash>, StoreError> {
    let mut features = crate::snapshot::held_features(reader)?;
    for (raw, payload) in overlay.records {
        features = features.merged(&crate::format::required_features(*raw, payload));
    }
    // each plan record's key and the links it names (a record whose links do not decode
    // names none: the move's own checks refuse it)
    let plan: Vec<(ObjectKey, Vec<crate::object_refs::ObjectRef>)> = overlay
        .records
        .iter()
        .map(|(raw, payload)| {
            let key = ObjectKey {
                kind: crate::codec::logical_kind(*raw),
                hash: Hash::of(payload),
            };
            (
                key,
                portable_refs_of(&features, *raw, payload).unwrap_or_default(),
            )
        })
        .collect();
    let in_plan: HashMap<ObjectKey, usize> =
        plan.iter().enumerate().map(|(i, (k, _))| (*k, i)).collect();
    let mut walked: HashSet<ObjectKey> = HashSet::new();
    let mut walked_hashes: HashSet<Hash> = HashSet::new();
    // the held objects the plan's records link, and the holder's closure of them
    let mut held_roots: BTreeSet<ObjectKey> = BTreeSet::new();
    let mut held_walked: BTreeSet<ObjectKey> = BTreeSet::new();
    let mut held: HashSet<ObjectKey> = HashSet::new();
    let mut held_hashes: HashSet<Hash> = HashSet::new();
    let mut todo: Vec<crate::object_refs::ObjectRef> = vec![crate::object_refs::ObjectRef {
        kind: Some(kinds::LANDING),
        hash: *head,
    }];
    loop {
        while let Some(r) = todo.pop() {
            let kinds_of: Vec<u8> = match r.kind {
                Some(k) => vec![k],
                None => portable_kinds().to_vec(),
            };
            let found = kinds_of
                .iter()
                .map(|&k| ObjectKey {
                    kind: k,
                    hash: r.hash,
                })
                .find(|key| in_plan.contains_key(key));
            match found {
                Some(key) => {
                    if walked.insert(key) {
                        walked_hashes.insert(key.hash);
                        todo.extend(plan[in_plan[&key]].1.iter().copied());
                    }
                }
                None => {
                    for k in kinds_of {
                        if reader.has_kind(k, &r.hash)? {
                            held_roots.insert(ObjectKey {
                                kind: k,
                                hash: r.hash,
                            });
                        }
                    }
                }
            }
        }
        // the holder's closure of the held objects met since the last pass
        let fresh: BTreeSet<ObjectKey> = held_roots
            .iter()
            .filter(|k| !held.contains(k) && !held_walked.contains(k))
            .copied()
            .collect();
        if !fresh.is_empty() {
            held_walked.extend(fresh.iter().copied());
            let keys =
                crate::closure::history_keys_before(reader, &fresh, crate::closure::END_OF_LOG)
                    .map_err(|e| refused(e.to_string()))?;
            for k in keys {
                held_hashes.insert(k.hash);
                held.insert(k);
            }
        }
        // the plan's attachments of what the walk holds, the plan's records or the holder's
        let names = |r: &crate::object_refs::ObjectRef| match r.kind {
            Some(k) => {
                let key = ObjectKey {
                    kind: k,
                    hash: r.hash,
                };
                walked.contains(&key) || held.contains(&key)
            }
            None => walked_hashes.contains(&r.hash) || held_hashes.contains(&r.hash),
        };
        let joined: Vec<ObjectKey> = plan
            .iter()
            .filter(|(k, refs)| {
                ATTACHMENT_KINDS.contains(&k.kind) && !walked.contains(k) && refs.iter().any(names)
            })
            .map(|(k, _)| *k)
            .collect();
        if joined.is_empty() {
            break;
        }
        for k in joined {
            todo.push(crate::object_refs::ObjectRef {
                kind: Some(k.kind),
                hash: k.hash,
            });
        }
    }
    let mut out: HashSet<Hash> = walked
        .iter()
        .chain(&held)
        .filter(|k| k.kind == kinds::LANDING)
        .map(|k| k.hash)
        .collect();
    out.extend(
        held_roots
            .iter()
            .filter(|k| k.kind == kinds::LANDING)
            .map(|k| k.hash),
    );
    Ok(out)
}

/// The refs that name the tags `chosen` of `carried`: `tag/<name>` for each, and
/// `answer/tag-object:<tag>` for each that has a relay answer in the plan or the holder and no
/// such ref yet (O11, ruling task-13). A `tag/<name>` that names another tag refuses the move.
fn tag_refs(
    reader: &StoreReader,
    overlay: &Overlay<'_>,
    carried: &[CarriedTag],
    chosen: &[Hash],
) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    let mut out: Vec<(String, Vec<u8>)> = vec![];
    let mut held_answers: Option<Vec<(Hash, Answer)>> = None;
    for c in carried.iter().filter(|c| chosen.contains(&c.hash)) {
        let name = format!("{}{}", crate::holder::TAG_REF, c.tag.name);
        if out.iter().any(|(n, _)| *n == name) {
            return Err(refused(format!("{name} names two tags of the move")));
        }
        match reader.get_ref(&name)? {
            None => out.push((name, c.hash.0.to_vec())),
            Some(v) if v == c.hash.0 => {}
            Some(_) => return Err(refused(format!("{name} names another tag"))),
        }
        let answer = crate::holder::answer_tag_object_ref(&c.hash);
        if reader.get_ref(&answer)?.is_some() {
            continue;
        }
        let relay = |a: &Answer| crate::holder::is_relay_answer_of(a, &c.hash, &c.tag.name);
        let mut found: Option<Hash> = None;
        for (raw, payload) in overlay.records {
            if crate::codec::logical_kind(*raw) == kinds::ANSWER
                && let Ok(a) = crate::codec::decode_record::<Answer>(*raw, payload)
                && relay(&a)
            {
                found = Some(Hash::of(payload));
                break;
            }
        }
        if found.is_none()
            && !overlay.by_key.contains_key(&ObjectKey {
                kind: kinds::TAG,
                hash: c.hash,
            })
        {
            let answers = match &held_answers {
                Some(a) => a,
                None => {
                    let mut all = vec![];
                    let mut hashes = vec![];
                    reader.for_each_object_of(kinds::ANSWER, |h| {
                        hashes.push(h);
                        Ok(())
                    })?;
                    for h in hashes {
                        if let Ok(a) = reader.get_typed::<Answer>(kinds::ANSWER, &h) {
                            all.push((h, a));
                        }
                    }
                    held_answers.insert(all)
                }
            };
            found = answers.iter().find(|(_, a)| relay(a)).map(|(h, _)| *h);
        }
        if let Some(a) = found {
            out.push((answer, a.0.to_vec()));
        }
    }
    Ok(out)
}

/// The store's own tag rule for [`Store::move_canon`]: a carried tag whose name is free is
/// named; one whose name names a tag of the same landing is not; one whose name names a tag of
/// another landing refuses the move.
fn store_tag_rule(reader: &StoreReader, carried: &[CarriedTag]) -> Result<Vec<Hash>, StoreError> {
    let mut out = vec![];
    let mut names: HashSet<&str> = HashSet::new();
    for c in carried {
        let name = format!("{}{}", crate::holder::TAG_REF, c.tag.name);
        match reader.get_ref(&name)? {
            None if names.insert(&c.tag.name) => out.push(c.hash),
            None => {}
            Some(v) => {
                let held = crate::index::hash_from(&v, "tag ref")?;
                let landing = reader.read_tag(&held)?.landing;
                if landing != c.tag.landing {
                    return Err(refused(format!(
                        "version {} already names landing {landing}",
                        c.tag.name
                    )));
                }
            }
        }
    }
    Ok(out)
}

/// A take's error as the store's own, for [`Store::move_canon`].
fn take_to_store(e: TakeError<StoreError>) -> StoreError {
    match e {
        TakeError::Store(e) | TakeError::Validation(e) => e,
        other => refused(other.to_string()),
    }
}

/// The ref writes of `mv` from plan `batch`, after its compare and chain rule, over `reader`.
/// Returns the refs and the value the move moves to, or the compare's observed tip or head.
fn plan_move(
    reader: &StoreReader,
    batch: &MoveBatch,
    mv: &CanonMove,
    expected: Option<Hash>,
    at_ms: i64,
) -> Result<Planned, StoreError> {
    let overlay = Overlay::new(reader, &batch.records);
    let head = batch.head;
    let want_kind = match mv {
        CanonMove::SetViewHead { .. } => kinds::VIEW_OP,
        _ => kinds::LANDING,
    };
    if let CanonMove::SetViewHead { name, root } = mv {
        let current = reader.view_head(name)?;
        let current_head = current.as_ref().map(|h| h.head);
        if current_head != expected {
            return Ok(Err(current_head));
        }
        if batch.head_kind != want_kind {
            return Err(refused(format!("the pushed head {head} is not a view op")));
        }
        // a pushed chain that is not one view's chain is the push's fault
        let chain = walk_view(name, &head, &mut |h| overlay.view_op(h)).map_err(|e| match e {
            StoreError::Decode(m) => refused(m),
            e => e,
        })?;
        let pushed_root = chain.last().expect("a chain has a root").0;
        let hub_root = match &current {
            Some(h) => reader.view_root(name, &h.head)?,
            None => *root,
        };
        if *root != hub_root {
            return Err(refused(view_root_reason(root, &hub_root, name)));
        }
        if pushed_root != hub_root {
            return Err(refused(view_root_reason(&pushed_root, &hub_root, name)));
        }
        let value = match &current {
            Some(h) => {
                let Some(at) = chain.iter().position(|(op, _)| *op == h.head) else {
                    return Err(refused(view_head_reason(&h.head)));
                };
                let set = reader.included_at(name, h)?.as_ref().clone();
                hub_head(reader, name, &chain[..at], Some((h, set)))?
            }
            None => hub_head(reader, name, &chain, None)?,
        };
        return Ok(Ok((
            vec![(format!("{VIEW_REF}{name}"), crate::store::encode(&value))],
            vec![],
        )));
    }
    let current = reader.canon_head()?;
    let compare = match mv {
        CanonMove::Seed => current.is_none() && expected.is_none(),
        // a hold also takes an empty canon, expected as none
        CanonMove::Hold => current == expected,
        _ => current.is_some() && current == expected,
    };
    if !compare {
        return Ok(Err(current));
    }
    if batch.head_kind != want_kind {
        return Err(refused(format!("the pushed head {head} is not a landing")));
    }
    let (chain, complete) = walk_landings(&head, &mut |h| overlay.landing(h))?;
    let canon = (CANON_REF.to_string(), head.0.to_vec());
    // a move that makes the chain canon carries the tags of its history; a hold names none
    let carried = || carried_tags(reader, &overlay, &head);
    match (mv, current) {
        (CanonMove::Seed, _) => {
            if !complete {
                return Err(refused(SEED_REASON));
            }
            Ok(Ok((vec![canon], carried()?)))
        }
        (CanonMove::Extend, Some(tip)) => {
            if !chain.contains(&tip) {
                return Err(refused(extend_reason(&tip)));
            }
            Ok(Ok((vec![canon], carried()?)))
        }
        (CanonMove::Force, Some(tip)) => {
            if chain.contains(&tip) {
                return Err(refused(force_reason(&tip)));
            }
            if !complete {
                return Err(refused(SEED_REASON));
            }
            let mark = Replaced {
                kind: ReplacedKind::Landing,
                by: head,
                at_ms,
            };
            Ok(Ok((
                vec![canon, (replaced_ref(&tip), mark.encode())],
                carried()?,
            )))
        }
        (CanonMove::Hold, Some(tip)) => {
            let canon_set = reader.canon_landings(&tip)?;
            if !chain.iter().any(|l| canon_set.contains(l)) {
                return Err(refused(HOLD_REASON));
            }
            Ok(Ok((vec![(held_ref(&head), vec![])], vec![])))
        }
        (CanonMove::Hold, None) => {
            // an empty canon: the chain a later seed lands, so it reaches its first landing
            if !complete {
                return Err(refused(SEED_REASON));
            }
            Ok(Ok((vec![(held_ref(&head), vec![])], vec![])))
        }
        _ => unreachable!("the compare holds a tip for every move but seed and hold"),
    }
}

impl Store {
    /// Move `mv` of plan `batch` (see the module docs) with the store's own tag rule
    /// (`store_tag_rule`): [`Store::move_canon_with`], its errors as the store's.
    pub fn move_canon(
        &mut self,
        batch: &MoveBatch,
        mv: &CanonMove,
        expected: Option<Hash>,
        at_ms: i64,
    ) -> Result<Moved, StoreError> {
        self.move_canon_with(batch, mv, expected, at_ms, store_tag_rule)
            .map_err(take_to_store)
    }

    /// Move `mv` of plan `batch` (see the module docs): the compare of `expected` with the
    /// current tip or head under the writer, the move's chain rule, then the plan's objects and
    /// the move's refs in one batch, which lists `hub_canon` when it first writes one of its
    /// refs. A plan checked before the holder last moved is checked again first. `at_ms` is the
    /// writer's clock, for a `replaced/` mark. Returns `Moved::Behind` with nothing written when
    /// the compare fails; a chain rule that fails is `Refused` with D-47HVF's reason. A batch
    /// that fails to write leaves the objects, the refs, the features and the index as they
    /// were.
    ///
    /// A seed, an extend or a force carries the tags of the history it makes canon
    /// ([`CarriedTag`]). `tag_rule` gets them and returns the ones the move names, or its own
    /// refusal (`TakeError::Validation`), before anything is written. The batch then names each
    /// by `tag/<name>`, with its relay answer by `answer/tag-object:<tag>`.
    ///
    /// A plan that carries a removal ([`MoveBatch::carries_removal`]) is taken through the
    /// store's checked take of a pushed history (`crate::take_batch::prepare_history`), whose
    /// checks run here, under the writer: each file, the union over the holder, the closure,
    /// each new removal's portable proof and the stale-content rule. The batch holds the
    /// records the holder lacks, the approved drops, the move's refs and the tag refs; the
    /// holder's own copies of what the removal approves are erased once it is durable, and the
    /// removal epoch advances.
    pub fn move_canon_with<E>(
        &mut self,
        batch: &MoveBatch,
        mv: &CanonMove,
        expected: Option<Hash>,
        at_ms: i64,
        tag_rule: impl FnOnce(&StoreReader, &[CarriedTag]) -> Result<Vec<Hash>, E>,
    ) -> Result<Moved, TakeError<E>> {
        self.ensure_writable()?;
        if self.open_batch().is_some() {
            return Err(crate::store::batch_open("before a move").into());
        }
        let reader = self.reader();
        let rechecked;
        let batch = if batch.history.is_some() || reader.index().indexed_upto()? == batch.checked_at
        {
            batch
        } else {
            rechecked = reader.check_move_batch(batch.records.clone(), batch.head)?;
            &rechecked
        };
        let (mut refs, carried) = match plan_move(&reader, batch, mv, expected, at_ms)? {
            Ok(planned) => planned,
            Err(tip) => return Ok(Moved::Behind { tip }),
        };
        if !carried.is_empty() {
            let chosen = tag_rule(&reader, &carried).map_err(TakeError::Validation)?;
            let overlay = Overlay::new(&reader, &batch.records);
            refs.extend(tag_refs(&reader, &overlay, &carried, &chosen)?);
        }
        drop(reader);
        let tip = batch.head;
        match &batch.history {
            None => {
                let mut items = batch.records.clone();
                items.extend(self.ref_items_pub(refs));
                self.write_move(items)?;
            }
            Some(h) => {
                self.take_history_move(&h.files, &h.target, &batch.records, refs)?;
            }
        }
        Ok(Moved::Moved { tip })
    }
}
