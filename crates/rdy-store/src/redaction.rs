//! Redaction requests, residues and the active mask (plan Redaction; D-600CW: "Every change is
//! an entry in the log, a redaction included"; D-3M81W: a removed version reads as a stub that
//! names its redaction).
//!
//! A request (kind `REDACTION`) is recorded with its residues (kind `REDACTION_RESIDUE`), its
//! zero-op carrier change and the carrier's view commit in one batch, by
//! [`Store::prepare_redaction`] then [`Store::commit_redaction`]. That batch removes nothing.
//! The derived tables map the carrier to its request and each request to the objects and frame
//! payloads it removes, whether or not it is applied. Only an apply record (kind
//! `REDACTION_APPLY`) puts a request's objects and spans into the active mask; the earliest apply
//! in log order keeps an object or a frame.
//!
//! Reads go through the mask: [`StoreReader::get_raw`] and the raw walker refuse a masked payload
//! with [`StoreError::Redacted`], [`StoreReader::get_block`] and [`StoreReader::change`] return
//! projections built from residues, and a blob with a masked manifest or chunk is unavailable.
//! A write never brings a masked hash back, and never serializes a projection.
use crate::error::StoreError;
use crate::index::{IndexEntry, RedactedSpan};
use crate::kinds::{self, BlobManifest};
use crate::segment::{HEADER, Location};
use crate::store::decode;
use crate::{Store, StoreReader};
use rdy_model::redaction::{
    BlockResidue, CARRIER_MESSAGE, ChangeResidue, RedactTarget, Redaction, RedactionApply,
    RedactionResidue, ResidualOp, encode_into, record_buffer,
};
use rdy_model::{Block, BlockId, Change, ChangeLinks, Hash};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

/// The kinds whose payloads a redaction may remove: block versions, the changes that embed them
/// and blob chunks. Every other kind is metadata or history and stays intact.
pub const REMOVABLE_KINDS: [u8; 3] = [kinds::BLOCK, kinds::CHANGE, kinds::BLOB_CHUNK];

/// Whether a payload of `kind` may be removed (see [`REMOVABLE_KINDS`]).
pub fn is_removable(kind: u8) -> bool {
    REMOVABLE_KINDS.contains(&kind)
}

/// Whether `kind` is one of the redaction record kinds, which only the store's checked redaction
/// writers write.
pub fn is_redaction_kind(kind: u8) -> bool {
    matches!(
        kind,
        kinds::REDACTION | kinds::REDACTION_RESIDUE | kinds::REDACTION_APPLY
    )
}

/// The `limit` texts of [`StoreError::RedactionLimit`]: exactly these, in this order.
pub mod limit {
    pub const TARGETS: &str = "256 targets";
    pub const TARGET_BYTES: &str = "256 bytes per target";
    pub const SELECTED: &str = "100000 selected objects";
    pub const FRAMES: &str = "1000000 scanned frames";
    pub const BYTES: &str = "2147483648 scanned bytes";
    pub const ROWS: &str = "100000 report rows";
    pub const RECORD: &str = "16777216 bytes per record";
    pub const STRING: &str = "1048576 bytes per string";
    pub const LEVELS: &str = "64 field levels";
    pub const SOURCES: &str = "4096 source strings";
    pub const ALL: [&str; 10] = [
        TARGETS,
        TARGET_BYTES,
        SELECTED,
        FRAMES,
        BYTES,
        ROWS,
        RECORD,
        STRING,
        LEVELS,
        SOURCES,
    ];
}

/// The most targets a request names (inclusive).
pub const MAX_TARGETS: usize = 256;
/// The most UTF-8 bytes of one target's selector (inclusive).
pub const MAX_TARGET_BYTES: usize = 256;
/// The most selected versions and manifests of one request (inclusive).
pub const MAX_SELECTED: usize = 100_000;
/// The most frames one scan reads (inclusive).
pub const MAX_SCANNED_FRAMES: u64 = 1_000_000;
/// The most frame bytes one scan reads (inclusive).
pub const MAX_SCANNED_BYTES: u64 = 2_147_483_648;

/// One frame of the log as the record walker hands it out. `Present` holds the original payload.
/// `Dropped` is a frame whose payload an applied redaction removed: its header facts, the apply
/// record that authorized it and the residue that replaces it for reads. Not a persisted kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoredFrame<'a> {
    Present {
        kind: u8,
        hash: Hash,
        payload: &'a [u8],
    },
    Dropped {
        kind: u8,
        hash: Hash,
        len: u32,
        apply: Hash,
        residue: Option<Hash>,
    },
}

/// A request checked against the store, ready for [`Store::commit_redaction`]. Only
/// [`Store::prepare_redaction`] builds one; preparing wrote nothing.
#[derive(Clone, Debug)]
pub struct PreparedRedaction {
    checked: Checked,
}

impl PreparedRedaction {
    /// The request's hash.
    pub fn request(&self) -> Hash {
        self.checked.request_id
    }

    /// The carrier change's id, as the commit writes it.
    pub fn carrier(&self) -> Hash {
        self.checked.carrier_id
    }

    /// The view the carrier is committed to.
    pub fn view(&self) -> &str {
        &self.checked.carrier.view
    }
}

/// What a check of a request produced: the inputs, with the carrier's deps as the commit sets
/// them, and the encoded records.
#[derive(Clone, Debug)]
struct Checked {
    carrier: Change,
    carrier_id: Hash,
    request: Redaction,
    request_id: Hash,
    request_bytes: Vec<u8>,
    /// The supplied residues, as given (for the recheck at commit).
    residues: Vec<RedactionResidue>,
    /// Each distinct residue's encoding, in the order supplied.
    residue_frames: Vec<Vec<u8>>,
}

/// A read projection overlay: the masks of named, recorded requests that no apply activated.
/// Readers holding one read those objects as projections; the store and the index are unchanged.
#[derive(Debug, Default)]
pub(crate) struct Preview {
    objects: HashMap<(u8, Hash), (Hash, Option<Hash>)>,
    spans: BTreeMap<Location, Hash>,
}

/// How a reader sees one masked object: the request that removed it, its residue, and its order
/// among masks (applied ones by apply location first, then preview ones).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Mask {
    pub(crate) request: Hash,
    pub(crate) residue: Option<Hash>,
    order: (u8, Location),
}

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

fn location(place: rdy_model::redaction::FramePlace) -> Location {
    Location {
        segment: place.segment,
        offset: place.offset,
    }
}

fn refused(m: String) -> StoreError {
    StoreError::Refused(m)
}

/// The UTF-8 length of a target's selector in the input grammar (`id`, `id@hash`, `blob:hash`).
fn selector_len(t: &RedactTarget) -> usize {
    match t {
        RedactTarget::AllVersions(id) => id.as_str().len(),
        RedactTarget::Version { id, .. } => id.as_str().len() + 1 + 64,
        RedactTarget::Blob(_) => "blob:".len() + 64,
    }
}

/// The index entry of a `REDACTION` frame with hash `hash`: its carrier, every object it removes
/// or names (with the residue of its first span) and every span.
pub(crate) fn request_entry(hash: Hash, payload: &[u8]) -> Result<IndexEntry, StoreError> {
    let r: Redaction = decode(payload)?;
    let mut objects: BTreeMap<(u8, Hash), Option<Hash>> = BTreeMap::new();
    let mut spans = Vec::with_capacity(r.manifest.spans.len());
    for s in &r.manifest.spans {
        objects.entry((s.kind, s.hash)).or_insert(s.residue);
        spans.push(RedactedSpan {
            loc: location(s.place),
            kind: s.kind,
            hash: s.hash,
            len: s.len,
            residue: s.residue,
        });
    }
    for (_, v) in &r.manifest.versions {
        objects.entry((kinds::BLOCK, *v)).or_insert(None);
    }
    for b in &r.manifest.blobs {
        objects.entry((kinds::BLOB_MANIFEST, *b)).or_insert(None);
    }
    Ok(IndexEntry::RedactionRequest {
        request: hash,
        carrier: r.carrier,
        objects: objects
            .into_iter()
            .map(|((kind, h), residue)| (kind, h, residue))
            .collect(),
        spans,
    })
}

/// The index entry of a `REDACTION_APPLY` frame with hash `hash` at `loc`.
pub(crate) fn apply_entry(
    hash: Hash,
    loc: Location,
    payload: &[u8],
) -> Result<IndexEntry, StoreError> {
    let a: RedactionApply = decode(payload)?;
    Ok(IndexEntry::RedactionApply {
        apply: hash,
        request: a.request,
        loc,
    })
}

impl StoreReader {
    /// The mask of the object of `kind` with hash `h` as this reader reads: the active mask,
    /// then a preview overlay. None for an object no applied (or previewed) request removed.
    pub(crate) fn mask_of(&self, kind: u8, h: &Hash) -> Result<Option<Mask>, StoreError> {
        if let Some(a) = self.index().redaction_active(kind, h)? {
            return Ok(Some(Mask {
                request: a.request,
                residue: a.residue,
                order: (0, a.at),
            }));
        }
        Ok(self.preview().and_then(|p| {
            p.objects.get(&(kind, *h)).map(|(request, residue)| Mask {
                request: *request,
                residue: *residue,
                order: (1, ORIGIN),
            })
        }))
    }

    /// The request whose mask holds the frame at `loc`, as this reader reads.
    pub(crate) fn mask_at(&self, loc: &Location) -> Result<Option<Hash>, StoreError> {
        if let Some(s) = self.index().redaction_span_at(loc)? {
            return Ok(Some(s.request));
        }
        Ok(self.preview().and_then(|p| p.spans.get(loc).copied()))
    }

    /// The request that removed the object of `kind` with hash `h`, as this reader reads; None
    /// when no applied request did. `has_kind` stays true for such an object.
    pub fn redaction_of(&self, kind: u8, h: &Hash) -> Result<Option<Hash>, StoreError> {
        Ok(self.mask_of(kind, h)?.map(|m| m.request))
    }

    /// The redaction request with hash `h`. Requests are never removed.
    pub fn redaction_request(&self, h: &Hash) -> Result<Redaction, StoreError> {
        self.get_named(kinds::REDACTION, "redaction request", h)
    }

    /// The request that change `h` carries, if it is a carrier.
    pub fn redaction_for_carrier(&self, h: &Hash) -> Result<Option<Hash>, StoreError> {
        self.index().redaction_carrier(h)
    }

    /// The residue with hash `h`.
    fn residue(&self, h: &Hash) -> Result<RedactionResidue, StoreError> {
        self.get_typed(kinds::REDACTION_RESIDUE, h)
    }

    /// The stub of removed block version `version`; `Redacted { request }` (the refusal that
    /// led here) when no residue reads it.
    pub(crate) fn block_projection(
        &self,
        version: &Hash,
        request: Hash,
    ) -> Result<Block, StoreError> {
        let Some(m) = self.mask_of(kinds::BLOCK, version)? else {
            return Err(StoreError::Redacted { request });
        };
        let Some(r) = m.residue else {
            return Err(StoreError::Redacted { request: m.request });
        };
        match self.residue(&r)? {
            RedactionResidue::Block(b) if b.version == *version => Ok(b.stub(m.request)),
            _ => Err(StoreError::Decode(format!(
                "the residue {r} of block version {version} is not that version's"
            ))),
        }
    }

    /// The projection of removed change `h`: its residue with each op's block read by its
    /// original version (the block, or its stub); `Redacted { request }` when no residue reads it.
    pub(crate) fn change_projection(&self, h: &Hash, request: Hash) -> Result<Change, StoreError> {
        let Some(m) = self.mask_of(kinds::CHANGE, h)? else {
            return Err(StoreError::Redacted { request });
        };
        let Some(r) = m.residue else {
            return Err(StoreError::Redacted { request: m.request });
        };
        let c = match self.residue(&r)? {
            RedactionResidue::Change(c) if c.change == *h => c,
            _ => {
                return Err(StoreError::Decode(format!(
                    "the residue {r} of change {h} is not that change's"
                )));
            }
        };
        c.project(|id: &BlockId, version| {
            let b = self.get_block(&version)?;
            if b.id != *id {
                return Err(StoreError::Decode(format!(
                    "change {h}: version {version} is a version of {}, not {id}",
                    b.id
                )));
            }
            Ok(b)
        })
    }

    /// The refusal of a blob read when the blob's manifest or any of its chunks is masked: the
    /// request of the earliest authorizing apply in log order.
    pub(crate) fn blob_mask(
        &self,
        h: &Hash,
        m: &BlobManifest,
    ) -> Result<Option<StoreError>, StoreError> {
        if !self.index().any_redaction_active() && self.preview().is_none() {
            return Ok(None);
        }
        let mut first = self.mask_of(kinds::BLOB_MANIFEST, h)?;
        for c in &m.chunks {
            if let Some(mask) = self.mask_of(kinds::BLOB_CHUNK, c)?
                && first.is_none_or(|f| mask.order < f.order)
            {
                first = Some(mask);
            }
        }
        Ok(first.map(|m| StoreError::Redacted { request: m.request }))
    }

    /// Refuses a change that holds a read projection, before anything is serialized: a projected
    /// change or block whose marker names a request that removed it is `Redacted`; a forged or
    /// unmatched marker is `ProjectedWrite`.
    pub(crate) fn refuse_projection(&self, change: &Change) -> Result<(), StoreError> {
        if let Some(orig) = change.read_identity {
            let request = match self.mask_of(kinds::CHANGE, &orig)? {
                Some(m) => Some(m.request),
                None => self
                    .index()
                    .redaction_requests_naming(kinds::CHANGE, &orig)?
                    .first()
                    .copied(),
            };
            return Err(match request {
                Some(request) => StoreError::Redacted { request },
                None => StoreError::ProjectedWrite,
            });
        }
        for op in &change.ops {
            if let rdy_model::Op::Create { block } | rdy_model::Op::Update { block, .. } = op
                && let Some(id) = &block.read_identity
            {
                let named = self
                    .index()
                    .redaction_requests_naming(kinds::BLOCK, &id.version)?
                    .contains(&id.request);
                return Err(if named {
                    StoreError::Redacted {
                        request: id.request,
                    }
                } else {
                    StoreError::ProjectedWrite
                });
            }
        }
        Ok(())
    }

    /// Calls `callback(location, frame, next)` for every frame in `[from, upto)`, batch markers
    /// included, continuing through segment rolls: a frame the active mask holds is
    /// [`StoredFrame::Dropped`] and never hands out its payload; every other frame is
    /// [`StoredFrame::Present`]. A masked frame whose header disagrees with its span is
    /// `RedactionCorrupt` at its location. `upto` comes from the writer, as for
    /// `for_each_frame_from`. A preview overlay plays no part: the walk reads the log's own
    /// authorization.
    pub fn for_each_record_from(
        &self,
        from: Location,
        upto: Location,
        mut callback: impl for<'a> FnMut(Location, StoredFrame<'a>, Location) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let spans = self.index().redaction_spans_in(from, upto)?;
        self.segments()
            .for_each_from(from, upto, |loc, kind, hash, payload, next| {
                match spans.get(&loc) {
                    None => callback(
                        loc,
                        StoredFrame::Present {
                            kind,
                            hash,
                            payload,
                        },
                        next,
                    ),
                    Some(s) => {
                        if s.span.kind != kind
                            || s.span.hash != hash
                            || s.span.len as usize != payload.len()
                        {
                            return Err(StoreError::corrupt_at(loc));
                        }
                        callback(
                            loc,
                            StoredFrame::Dropped {
                                kind,
                                hash,
                                len: s.span.len,
                                apply: s.apply,
                                residue: s.span.residue,
                            },
                            next,
                        )
                    }
                }
            })
    }
}

impl Store {
    /// Checks a redaction request against the store and returns it ready to commit. Writes
    /// nothing. The checks:
    ///
    /// - the carrier is a new, zero-op change with no justification, the message
    ///   `Request content removal.` and default links, by the request's actor; the request names
    ///   it by the id the commit gives it; no request names it yet;
    /// - the bounds: 1 to 256 targets, 256 bytes per selector, 100000 selected objects, 16777216
    ///   bytes per record, 1000000 scanned frames and 2147483648 scanned bytes;
    /// - every manifest version is a stored block version of its id, every blob a stored blob
    ///   manifest, and the targets and the manifest agree;
    /// - every span, in strictly ascending location order, names an intact frame of a removable
    ///   kind with its kind, hash and length, of a selected version, a change producing one or a
    ///   chunk of a selected blob; a block or change span names the residue the store derives
    ///   from that frame, and a change residue's untargeted versions are stored blocks; a frame an
    ///   earlier apply removed keeps its recorded span;
    /// - every intact frame of a selected object is a span, and every supplied residue is named.
    ///
    /// A span that fails is `RedactionCorrupt` at the span's claimed location (a missing span at
    /// the unlisted frame's); a request that fails otherwise is `Refused`, a bound
    /// `RedactionLimit`, a projected carrier `ProjectedWrite`.
    pub fn prepare_redaction(
        &self,
        carrier: &Change,
        request: &Redaction,
        residues: &[RedactionResidue],
    ) -> Result<PreparedRedaction, StoreError> {
        let checked = self.check_redaction(carrier, request, residues)?;
        // the view takes the commit (a closed view refuses it), checked now as at commit
        let c = &checked.carrier;
        self.commit_parts(
            &c.view,
            vec![c.clone()],
            &|_| None,
            &[],
            None,
            &c.provenance,
        )?;
        Ok(PreparedRedaction { checked })
    }

    /// Writes a prepared request as one batch: its residues, the carrier, the request, then the
    /// carrier's view commit (through the view commit writer). It first checks the request again:
    /// a source version or span that changed since preparation is `RedactionStale`, and nothing
    /// is written. Removes nothing. Returns the request's hash.
    pub fn commit_redaction(&mut self, prepared: PreparedRedaction) -> Result<Hash, StoreError> {
        let p = prepared.checked;
        let checked = match self.check_redaction(&p.carrier, &p.request, &p.residues) {
            Err(StoreError::RedactionCorrupt { .. }) => return Err(StoreError::RedactionStale),
            r => r?,
        };
        if checked.carrier_id != p.carrier_id || checked.request_id != p.request_id {
            return Err(StoreError::RedactionStale);
        }
        let committer = checked.carrier.provenance.clone();
        let view = checked.carrier.view.clone();
        let parts = self.commit_parts(
            &view,
            vec![checked.carrier.clone()],
            &|_| None,
            &[],
            None,
            &committer,
        )?;
        let mut items: Vec<(u8, Vec<u8>)> = checked
            .residue_frames
            .into_iter()
            .map(|b| (kinds::REDACTION_RESIDUE, b))
            .collect();
        let carrier_pos = items.len() + parts.placed[0].1;
        items.extend(parts.changes);
        items.push((kinds::REDACTION, checked.request_bytes));
        items.extend(parts.views);
        let written = self.write_guarded(items)?;
        if written[carrier_pos] != checked.carrier_id {
            return Err(StoreError::Decode(format!(
                "carrier id mismatch: the request names {}, written {}",
                checked.carrier_id, written[carrier_pos]
            )));
        }
        Ok(checked.request_id)
    }

    /// A reader that reads the named recorded requests as if they were applied: their checked
    /// masks overlay the active mask for block, change and blob reads. The hashes are sorted and
    /// deduplicated; an unknown request is `NotFound`, a span whose frame no longer matches is
    /// `RedactionCorrupt` at it. It changes neither the index nor the redaction epoch, and it
    /// cannot write, seal or erase.
    pub fn redaction_preview_reader(&self, requests: &[Hash]) -> Result<StoreReader, StoreError> {
        let mut requests = requests.to_vec();
        requests.sort();
        requests.dedup();
        let mut preview = Preview::default();
        for r in &requests {
            if !self.has_kind(kinds::REDACTION, r)? {
                return Err(StoreError::NotFound(*r));
            }
            let (objects, spans) = self.index().redaction_request_items(r)?;
            for s in spans {
                if self.index().redaction_span_at(&s.loc)?.is_none() {
                    let (kind, hash, payload) = self
                        .segments()
                        .read_hashed(s.loc)
                        .map_err(|_| StoreError::corrupt_at(s.loc))?;
                    if kind != s.kind || hash != s.hash || payload.len() != s.len as usize {
                        return Err(StoreError::corrupt_at(s.loc));
                    }
                }
                preview.spans.entry(s.loc).or_insert(*r);
            }
            for (kind, h, residue) in objects {
                preview.objects.entry((kind, h)).or_insert((*r, residue));
            }
        }
        Ok(self.reader_with_preview(Arc::new(preview)))
    }

    /// The checks of `prepare_redaction`, and the encoded records.
    fn check_redaction(
        &self,
        carrier: &Change,
        request: &Redaction,
        residues: &[RedactionResidue],
    ) -> Result<Checked, StoreError> {
        // 1. the carrier
        if carrier.is_projection() {
            return Err(StoreError::ProjectedWrite);
        }
        if !carrier.ops.is_empty()
            || carrier.justification.is_some()
            || carrier.message != CARRIER_MESSAGE
            || carrier.links != ChangeLinks::default()
        {
            return Err(refused(format!(
                "a redaction carrier is a change with no ops, no justification, the message {CARRIER_MESSAGE:?} and default links"
            )));
        }
        if request.actor != carrier.provenance {
            return Err(refused(
                "the redaction request's actor is not its carrier's provenance".into(),
            ));
        }
        let mut carrier = carrier.clone();
        let view = carrier.view.clone();
        let (frames, pos) = self.change_frames(&mut carrier, Some(&view), &[])?;
        let carrier_id = Hash::of(&frames[pos].1);
        if request.carrier != carrier_id {
            return Err(refused(format!(
                "the redaction request names carrier {}, but the carrier's id is {carrier_id}",
                request.carrier
            )));
        }
        if let Some(r) = self.redaction_for_carrier(&carrier_id)? {
            return Err(refused(format!(
                "change {carrier_id} already carries redaction request {r}"
            )));
        }
        if self.has_kind(kinds::CHANGE, &carrier_id)? {
            return Err(refused(format!(
                "change {carrier_id} is already stored: a carrier is a new change"
            )));
        }
        // 2. the bounds, and the records' encodings
        if request.targets.is_empty() {
            return Err(refused(
                "a redaction request names at least one target".into(),
            ));
        }
        if request.targets.len() > MAX_TARGETS {
            return Err(StoreError::RedactionLimit {
                limit: limit::TARGETS,
            });
        }
        if request
            .targets
            .iter()
            .any(|t| selector_len(t) > MAX_TARGET_BYTES)
        {
            return Err(StoreError::RedactionLimit {
                limit: limit::TARGET_BYTES,
            });
        }
        let m = &request.manifest;
        if m.versions
            .len()
            .checked_add(m.blobs.len())
            .is_none_or(|n| n > MAX_SELECTED)
        {
            return Err(StoreError::RedactionLimit {
                limit: limit::SELECTED,
            });
        }
        let too_big = || StoreError::RedactionLimit {
            limit: limit::RECORD,
        };
        let mut buf = record_buffer().map_err(|_| too_big())?;
        let request_bytes = encode_into(request, &mut buf)
            .map_err(|_| too_big())?
            .to_vec();
        let request_id = Hash::of(&request_bytes);
        let mut supplied: HashMap<Hash, &RedactionResidue> = HashMap::new();
        let mut residue_frames = vec![];
        for r in residues {
            let bytes = encode_into(r, &mut buf).map_err(|_| too_big())?;
            let h = Hash::of(bytes);
            if supplied.insert(h, r).is_none() {
                residue_frames.push(bytes.to_vec());
            }
        }
        drop(buf);
        // 3. the manifest's selection, and the targets
        if m.versions.windows(2).any(|w| w[0] >= w[1]) || m.blobs.windows(2).any(|w| w[0] >= w[1]) {
            return Err(refused(
                "the manifest's versions and blobs are not in canonical order".into(),
            ));
        }
        let mut versions: HashSet<Hash> = HashSet::new();
        for (id, v) in &m.versions {
            if !self.has_kind(kinds::BLOCK, v)? {
                return Err(refused(format!("{v} is not a stored block version")));
            }
            let b = self.get_block(v)?;
            if b.id != *id {
                return Err(refused(format!(
                    "version {v} belongs to another block, not {id}"
                )));
            }
            versions.insert(*v);
        }
        let mut chunks: HashSet<Hash> = HashSet::new();
        for b in &m.blobs {
            if !self.has_kind(kinds::BLOB_MANIFEST, b)? {
                return Err(refused(format!("{b} is not a blob manifest")));
            }
            let manifest: BlobManifest = self.get_typed(kinds::BLOB_MANIFEST, b)?;
            chunks.extend(manifest.chunks);
        }
        for t in &request.targets {
            let found = match t {
                RedactTarget::AllVersions(id) => m.versions.iter().any(|(i, _)| i == id),
                RedactTarget::Version { id, version } => {
                    m.versions.binary_search(&(id.clone(), *version)).is_ok()
                }
                RedactTarget::Blob(h) => m.blobs.binary_search(h).is_ok(),
            };
            if !found {
                return Err(refused(format!(
                    "a redaction target selects nothing the manifest lists: {t:?}"
                )));
            }
        }
        for (id, v) in &m.versions {
            let covered = request.targets.iter().any(|t| match t {
                RedactTarget::AllVersions(i) => i == id,
                RedactTarget::Version { id: i, version } => i == id && version == v,
                RedactTarget::Blob(_) => false,
            });
            if !covered {
                return Err(refused(format!(
                    "the manifest lists version {id}@{v}, which no target selects"
                )));
            }
        }
        for b in &m.blobs {
            if !request
                .targets
                .iter()
                .any(|t| matches!(t, RedactTarget::Blob(h) if h == b))
            {
                return Err(refused(format!(
                    "the manifest lists blob {b}, which no target selects"
                )));
            }
        }
        let mut changes: HashSet<Hash> = HashSet::new();
        for v in &versions {
            changes.extend(self.producers(v)?);
        }
        let selected = |kind: u8, h: &Hash| match kind {
            kinds::BLOCK => versions.contains(h),
            kinds::CHANGE => changes.contains(h),
            kinds::BLOB_CHUNK => chunks.contains(h),
            _ => false,
        };
        // 4. each span
        let mut listed: BTreeSet<Location> = BTreeSet::new();
        let mut named: HashSet<Hash> = HashSet::new();
        let mut prev: Option<Location> = None;
        for span in &m.spans {
            let at = location(span.place);
            let corrupt = || StoreError::corrupt_at(at);
            if prev.is_some_and(|p| p >= at) {
                return Err(corrupt());
            }
            prev = Some(at);
            if !is_removable(span.kind) || !selected(span.kind, &span.hash) {
                return Err(corrupt());
            }
            match self.index().redaction_span_at(&at)? {
                // removed by an earlier apply: its recorded span
                Some(active) => {
                    let s = active.span;
                    if (s.kind, s.hash, s.len, s.residue)
                        != (span.kind, span.hash, span.len, span.residue)
                    {
                        return Err(corrupt());
                    }
                    if let Some(r) = span.residue
                        && !supplied.contains_key(&r)
                        && !self.has_kind(kinds::REDACTION_RESIDUE, &r)?
                    {
                        return Err(corrupt());
                    }
                }
                None => {
                    let (kind, hash, payload) =
                        self.segments().read_hashed(at).map_err(|_| corrupt())?;
                    if kind != span.kind || hash != span.hash || payload.len() != span.len as usize
                    {
                        return Err(corrupt());
                    }
                    let residue = span.residue.and_then(|r| supplied.get(&r).copied());
                    match (span.kind, residue) {
                        (kinds::BLOCK, Some(RedactionResidue::Block(res))) => {
                            let block: Block = decode(&payload).map_err(|_| corrupt())?;
                            if *res != BlockResidue::of(&block) {
                                return Err(corrupt());
                            }
                        }
                        (kinds::CHANGE, Some(RedactionResidue::Change(res))) => {
                            let change: Change = decode(&payload).map_err(|_| corrupt())?;
                            if *res != ChangeResidue::of(&change) {
                                return Err(corrupt());
                            }
                            for op in &res.ops {
                                if let ResidualOp::Create { version, .. }
                                | ResidualOp::Update { version, .. } = op
                                    && !versions.contains(version)
                                    && !self.has_kind(kinds::BLOCK, version)?
                                {
                                    return Err(corrupt());
                                }
                            }
                        }
                        (kinds::BLOB_CHUNK, None) if span.residue.is_none() => {}
                        _ => return Err(corrupt()),
                    }
                }
            }
            if let Some(r) = span.residue {
                named.insert(r);
            }
            listed.insert(at);
        }
        if let Some(h) = supplied.keys().find(|h| !named.contains(h)) {
            return Err(refused(format!("residue {h} is named by no span")));
        }
        // 5. every intact frame of a selected object is listed, and every chunk has a frame
        let mut frames = 0u64;
        let mut bytes = 0u64;
        let mut seen_chunks: HashSet<Hash> = HashSet::new();
        self.for_each_record_from(ORIGIN, self.log_end(), |loc, frame, _| {
            frames += 1;
            if frames > MAX_SCANNED_FRAMES {
                return Err(StoreError::RedactionLimit {
                    limit: limit::FRAMES,
                });
            }
            let (kind, hash, len, present) = match frame {
                StoredFrame::Present {
                    kind,
                    hash,
                    payload,
                } => (kind, hash, payload.len() as u64, true),
                StoredFrame::Dropped {
                    kind, hash, len, ..
                } => (kind, hash, u64::from(len), false),
            };
            bytes = bytes
                .checked_add(HEADER + len)
                .filter(|b| *b <= MAX_SCANNED_BYTES)
                .ok_or(StoreError::RedactionLimit {
                    limit: limit::BYTES,
                })?;
            if kind == kinds::BLOB_CHUNK && chunks.contains(&hash) {
                seen_chunks.insert(hash);
            }
            if present && selected(kind, &hash) && !listed.contains(&loc) {
                return Err(StoreError::corrupt_at(loc));
            }
            Ok(())
        })?;
        if let Some(c) = chunks.iter().find(|c| !seen_chunks.contains(c)) {
            return Err(refused(format!(
                "chunk {c} of a selected blob has no chunk frame"
            )));
        }
        Ok(Checked {
            carrier,
            carrier_id,
            request: request.clone(),
            request_id,
            request_bytes,
            residues: residues.to_vec(),
            residue_frames,
        })
    }
}
