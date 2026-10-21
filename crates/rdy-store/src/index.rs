use crate::error::{StoreError, db};
use crate::segment::Location;
use rdy_model::{BlockId, Hash};
use redb::{Database, Durability, ReadableDatabase, ReadableTable, TableDefinition, TableHandle};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// hash -> the location of the first frame written with that hash, of whatever kind (untyped reads).
const OBJECTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("objects");
/// hash (32 bytes) followed by the kind byte -> the location of that kind's object with that hash.
const TYPED: TableDefinition<&[u8], &[u8]> = TableDefinition::new("typed");
/// a version hash, then the hash of a change that produced it -> empty: every producer of every
/// version, since two changes (in two views, or a view and a landing) can write identical bytes.
const PRODUCERS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("producers");
/// a view name's UTF-8 bytes, a 0x00 byte, then the op hash -> empty: the ops of each view.
const VIEW_OPS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("view_ops");
/// change hash -> the hash of the first landing that carried it into canon.
const LANDED: TableDefinition<&[u8], &[u8]> = TableDefinition::new("landed");
/// a block id's string bytes, a 0x00 byte, then the hash of a change that deletes it -> empty.
const DELETERS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("deleters");
const REFS: TableDefinition<&str, &[u8]> = TableDefinition::new("refs");
/// An open tension's id (32 bytes) -> the hash of the TRANSITION record that opened it: what the
/// log's tension moves fold to (plan M2e2, D-Z0EMP).
const OPEN_TENSIONS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("open_tensions");
/// An open suspect's key (`transition::suspect_key`, 32 bytes) -> the hash of the TRANSITION
/// record that made it new: what the log's suspect moves fold to.
const OPEN_SUSPECTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("open_suspects");
/// An `open/<view>` ref that names an open proposal -> its value (the proposal's stable hash):
/// the refs of that prefix whose value is not empty. A landing empties its view's ref, which
/// `refs` keeps (a dump lists it) and this table drops (final review fix round 1, R5), so the
/// open proposals are read without reading every view that ever proposed. Derived from the
/// refs, so a replay of the log rebuilds it.
const OPEN_PROPOSALS: TableDefinition<&str, &[u8]> = TableDefinition::new("open_proposals");
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
/// A carrier change's hash -> the hash of the first redaction request logged for it.
const REDACTION_CARRIERS: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("redaction_carriers");
/// Every request's items, derived when the request is logged and before any apply. Key: the
/// request's hash, then 0x00, the kind byte and the object's hash -> the residue's hash (or empty):
/// every object it removes (block versions, changes, chunks) and every blob manifest it names. Key:
/// the request's hash, then 0x01 and a frame location (`loc_key`) -> the kind byte, the object's
/// hash, the payload length (u32 little-endian) and the residue's hash (or nothing): every frame
/// payload it removes. No value holds a payload.
const REDACTION_ITEMS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("redaction_items");
/// An object's hash, its kind byte, then the hash of a request that names it -> empty: the
/// requests of each object, applied or not.
const REDACTION_OBJECTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("redaction_objects");
/// A request's hash -> the hash of the first apply record that activated it, then that record's
/// location (`loc_key`).
const REDACTION_APPLIED: TableDefinition<&[u8], &[u8]> = TableDefinition::new("redaction_applied");
/// The active mask by object: an object's hash and kind byte (as `typed`) -> the request, the apply
/// record and its location, then the residue's hash (or nothing). The earliest apply in log order
/// keeps an object. A carried record's `held: false` span's object is a row too, with the carried
/// record and its location in the apply's place, until an apply holds the object.
const REDACTION_ACTIVE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("redaction_active");
/// The span index, and the active mask by frame: a frame location (`loc_key`) -> the apply record,
/// the request, the kind byte, the object's hash, the payload length (u32 little-endian) and the
/// residue's hash (or nothing). The earliest apply in log order keeps a frame. While no apply
/// covers a frame that a carried record names, its row is the carried record's: the carried
/// record in the apply's place, its source request, the span, then one more byte, the span's
/// `held` flag (0 or 1). The first carried record in log order keeps the frame, except that a
/// `held: false` span takes over a `held: true` row; an apply that covers the frame replaces any
/// carried row. An apply's row and a `held: false` row mask the frame; a `held: true` row is
/// provenance only, and masks nothing.
const REDACTION_SPANS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("redaction_spans");
/// The places of the validated carried records (kind REDACTION_CARRIED): a record frame's
/// location (`loc_key`) -> its hash. One row per physical record, so a reader finds every
/// carried record in physical order without a walk of the log.
const REDACTION_CARRIED_AT: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("redaction_carried_at");
/// A signature's subject (32 bytes), then the signature's hash (32 bytes) -> empty: every
/// signature of every subject (kind SIGNATURE, D-1Z951), derived from the frames alone.
const SIGNATURES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("signatures");
/// The prefix of the refs [`OPEN_PROPOSALS`] mirrors.
const OPEN_PREFIX: &str = "open/";

pub enum IndexEntry {
    /// An object frame: indexed by (kind, hash), and by hash alone if it is the first frame
    /// written with that hash.
    Object {
        kind: u8,
        hash: Hash,
        loc: Location,
    },
    /// `change` has an op that produces `version`; a version keeps every producer.
    Producer {
        version: Hash,
        change: Hash,
    },
    Ref(String, Vec<u8>),
    /// `op` is one of `view`'s chain ops.
    ViewOp {
        view: String,
        op: Hash,
    },
    /// `landing` carried `change` into canon; only the first such landing is kept.
    Landed {
        change: Hash,
        landing: Hash,
    },
    /// `change` has an op that deletes `block`.
    Deleted {
        block: BlockId,
        change: Hash,
    },
    /// TRANSITION record `record` opened tension `id`.
    TensionOpen {
        id: Hash,
        record: Hash,
    },
    /// A TRANSITION record resolved tension `id`.
    TensionShut {
        id: Hash,
    },
    /// TRANSITION record `record` made the suspect with key `key` new.
    SuspectOpen {
        key: Hash,
        record: Hash,
    },
    /// A TRANSITION record made the suspect with key `key` gone.
    SuspectShut {
        key: Hash,
    },
    /// Redaction request `request` (kind REDACTION), carried by change `carrier`: the objects it
    /// removes or names (kind, hash, residue) and the frame payloads it removes.
    RedactionRequest {
        request: Hash,
        carrier: Hash,
        objects: Vec<(u8, Hash, Option<Hash>)>,
        spans: Vec<RedactedSpan>,
    },
    /// Apply record `apply`, at `loc`, activates request `request`. A request it does not follow
    /// in the log is damage: `RedactionCorrupt` at `loc`.
    RedactionApply {
        apply: Hash,
        request: Hash,
        loc: Location,
    },
    /// SIGNATURE record `signature` names `subject`; every signature of a subject is kept.
    Signature {
        subject: Hash,
        signature: Hash,
    },
    /// Carried record `carried` (kind REDACTION_CARRIED), at `loc`, of source request `request`:
    /// each span it names, with its `held` flag. Every span gets a `redaction_spans` row with
    /// the record, its source request and its flag where no apply holds the frame; only its
    /// `held: false` spans (and their objects) mask, with the record as their proof. A later
    /// apply that covers a span takes its row over.
    RedactionCarried {
        carried: Hash,
        request: Hash,
        loc: Location,
        spans: Vec<(RedactedSpan, bool)>,
    },
}

/// One frame payload a request removes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RedactedSpan {
    pub loc: Location,
    pub kind: u8,
    pub hash: Hash,
    pub len: u32,
    pub residue: Option<Hash>,
}

/// One frame of the active mask: the span, and the apply record and request that authorize it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveSpan {
    pub span: RedactedSpan,
    pub apply: Hash,
    pub request: Hash,
}

/// One object of the active mask: its request, the apply record and that record's location, and
/// its residue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveObject {
    pub request: Hash,
    pub apply: Hash,
    pub at: Location,
    pub residue: Option<Hash>,
}

pub struct Index {
    db: Database,
    /// Whether the active redaction mask may hold a row: false only while it is empty, so the
    /// reads of a store with no applied redaction never open its tables.
    redacted: AtomicBool,
}

/// An index opened with its integrity checked (see `Index::open_checked`).
pub struct CheckedIndex {
    pub index: Index,
    /// `check_integrity` found nothing to repair.
    pub clean: bool,
    /// redb fired its repair callback while opening the file (it was not closed cleanly).
    pub repaired_at_open: bool,
}

/// Every table of an index as raw keys and values, for comparing two indexes (fsck2). `meta` is
/// left out: it records how far the index has read, which a rebuild need not match.
#[doc(hidden)]
#[derive(Debug, PartialEq, Eq)]
pub struct IndexDump {
    pub objects: BTreeMap<Vec<u8>, Vec<u8>>,
    pub typed: BTreeMap<Vec<u8>, Vec<u8>>,
    pub producers: BTreeMap<Vec<u8>, Vec<u8>>,
    pub refs: BTreeMap<Vec<u8>, Vec<u8>>,
    pub view_ops: BTreeMap<Vec<u8>, Vec<u8>>,
    pub landed: BTreeMap<Vec<u8>, Vec<u8>>,
    pub deleters: BTreeMap<Vec<u8>, Vec<u8>>,
    pub open_tensions: BTreeMap<Vec<u8>, Vec<u8>>,
    pub open_suspects: BTreeMap<Vec<u8>, Vec<u8>>,
    pub open_proposals: BTreeMap<Vec<u8>, Vec<u8>>,
    pub redaction_carriers: BTreeMap<Vec<u8>, Vec<u8>>,
    pub redaction_items: BTreeMap<Vec<u8>, Vec<u8>>,
    pub redaction_objects: BTreeMap<Vec<u8>, Vec<u8>>,
    pub redaction_applied: BTreeMap<Vec<u8>, Vec<u8>>,
    pub redaction_active: BTreeMap<Vec<u8>, Vec<u8>>,
    pub redaction_spans: BTreeMap<Vec<u8>, Vec<u8>>,
    pub redaction_carried_at: BTreeMap<Vec<u8>, Vec<u8>>,
    pub signatures: BTreeMap<Vec<u8>, Vec<u8>>,
}

fn dump_table(
    r: &redb::ReadTransaction,
    def: TableDefinition<&'static [u8], &'static [u8]>,
) -> Result<BTreeMap<Vec<u8>, Vec<u8>>, StoreError> {
    let t = r.open_table(def).map_err(db)?;
    let mut out = BTreeMap::new();
    for item in t.iter().map_err(db)? {
        let (k, v) = item.map_err(db)?;
        out.insert(k.value().to_vec(), v.value().to_vec());
    }
    Ok(out)
}

/// The `typed` table's key: the hash's 32 bytes, then the kind byte.
fn typed_key(kind: u8, h: &Hash) -> [u8; 33] {
    let mut key = [0u8; 33];
    key[..32].copy_from_slice(&h.0);
    key[32] = kind;
    key
}

/// The key prefix of `name` in a name-then-hash table (`view_ops`, `deleters`): its UTF-8 bytes,
/// then a 0x00 byte.
fn name_prefix(name: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(name.len() + 33);
    key.extend_from_slice(name.as_bytes());
    key.push(0);
    key
}

/// The hashes keyed under `name` in a name-then-hash table, in ascending hash order.
fn hashes_under(
    t: &redb::ReadOnlyTable<&'static [u8], &'static [u8]>,
    name: &str,
    what: &str,
) -> Result<Vec<Hash>, StoreError> {
    let prefix = name_prefix(name);
    let mut out = vec![];
    for item in t.range(&prefix[..]..).map_err(db)? {
        let (k, _) = item.map_err(db)?;
        let key = k.value();
        if !key.starts_with(&prefix) {
            break;
        }
        // a longer name that starts with `name` and a 0x00 byte sorts among these keys; only a
        // key of exactly prefix + hash is one of `name`'s
        if key.len() == prefix.len() + 32 {
            out.push(hash_from(&key[prefix.len()..], what)?);
        }
    }
    Ok(out)
}

/// The `signatures` table's key: the subject's 32 bytes, then the signature's.
fn signature_key(subject: &Hash, signature: &Hash) -> [u8; 64] {
    let mut key = [0u8; 64];
    key[..32].copy_from_slice(&subject.0);
    key[32..].copy_from_slice(&signature.0);
    key
}

/// The subject and signature of a `signatures` key; any other length is damage.
fn signature_from(key: &[u8]) -> Result<(Hash, Hash), StoreError> {
    if key.len() != 64 {
        return Err(StoreError::Decode(format!(
            "signature entry is {} bytes, expected 64",
            key.len()
        )));
    }
    Ok((
        hash_from(&key[..32], "signature entry")?,
        hash_from(&key[32..], "signature entry")?,
    ))
}

/// A frame location as a key: the segment (u32) then the offset (u64), big-endian, so keys sort
/// in log order.
fn loc_key(loc: &Location) -> [u8; 12] {
    let mut key = [0u8; 12];
    key[..4].copy_from_slice(&loc.segment.to_be_bytes());
    key[4..].copy_from_slice(&loc.offset.to_be_bytes());
    key
}

fn loc_from(bytes: &[u8], what: &str) -> Result<Location, StoreError> {
    let bad = || StoreError::Decode(format!("{what} holds a damaged frame location"));
    let segment = u32::from_be_bytes(
        bytes
            .get(..4)
            .ok_or_else(bad)?
            .try_into()
            .map_err(|_| bad())?,
    );
    let offset = u64::from_be_bytes(
        bytes
            .get(4..12)
            .ok_or_else(bad)?
            .try_into()
            .map_err(|_| bad())?,
    );
    Ok(Location { segment, offset })
}

/// A residue hash as a value suffix: nothing for none.
fn residue_bytes(r: &Option<Hash>) -> &[u8] {
    match r {
        Some(h) => &h.0,
        None => &[],
    }
}

fn residue_from(bytes: &[u8], what: &str) -> Result<Option<Hash>, StoreError> {
    match bytes.len() {
        0 => Ok(None),
        _ => hash_from(bytes, what).map(Some),
    }
}

/// A span's value in `redaction_items` and the tail of one in `redaction_spans`: kind, hash,
/// length, residue.
fn span_value(span: &RedactedSpan) -> Vec<u8> {
    let mut v = Vec::with_capacity(1 + 32 + 4 + 32);
    v.push(span.kind);
    v.extend_from_slice(&span.hash.0);
    v.extend_from_slice(&span.len.to_le_bytes());
    v.extend_from_slice(residue_bytes(&span.residue));
    v
}

fn span_from(loc: Location, v: &[u8], what: &str) -> Result<RedactedSpan, StoreError> {
    if v.len() < 37 {
        return Err(StoreError::Decode(format!("{what} is {} bytes", v.len())));
    }
    Ok(RedactedSpan {
        loc,
        kind: v[0],
        hash: hash_from(&v[1..33], what)?,
        len: u32::from_le_bytes(v[33..37].try_into().expect("four bytes")),
        residue: residue_from(&v[37..], what)?,
    })
}

fn active_object_from(v: &[u8]) -> Result<ActiveObject, StoreError> {
    let what = "redaction mask entry";
    if v.len() < 76 {
        return Err(StoreError::Decode(format!("{what} is {} bytes", v.len())));
    }
    Ok(ActiveObject {
        request: hash_from(&v[..32], what)?,
        apply: hash_from(&v[32..64], what)?,
        at: loc_from(&v[64..76], what)?,
        residue: residue_from(&v[76..], what)?,
    })
}

/// The `held` byte that ends a carried record's row in `redaction_spans`.
fn held_byte(held: bool) -> u8 {
    u8::from(held)
}

/// One row of `redaction_spans`: the span with its proof, and for a carried record's row its
/// `held` flag (None for an apply's row).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SpanRow {
    pub(crate) active: ActiveSpan,
    pub(crate) held: Option<bool>,
}

impl SpanRow {
    /// Whether the row masks its frame: an apply's row, or a carried `held: false` one.
    pub(crate) fn masks(&self) -> bool {
        self.held != Some(true)
    }
}

/// A row of `redaction_spans`: an apply's is 101 or 133 bytes (with no residue, or with one); a
/// carried record's has one more, its `held` byte.
fn span_row_from(loc: Location, v: &[u8]) -> Result<SpanRow, StoreError> {
    let what = "redaction span entry";
    let (body, held) = match v.len() {
        101 | 133 => (v, None),
        102 | 134 => {
            let (body, last) = v.split_at(v.len() - 1);
            let held = match last[0] {
                0 => false,
                1 => true,
                b => return Err(StoreError::Decode(format!("{what} has held byte {b}"))),
            };
            (body, Some(held))
        }
        n => return Err(StoreError::Decode(format!("{what} is {n} bytes"))),
    };
    Ok(SpanRow {
        active: ActiveSpan {
            apply: hash_from(&body[..32], what)?,
            request: hash_from(&body[32..64], what)?,
            span: span_from(loc, &body[64..], what)?,
        },
        held,
    })
}

/// The key of an object item of `request` in `redaction_items`.
fn item_object_key(request: &Hash, kind: u8, h: &Hash) -> [u8; 66] {
    let mut key = [0u8; 66];
    key[..32].copy_from_slice(&request.0);
    key[32] = 0;
    key[33] = kind;
    key[34..].copy_from_slice(&h.0);
    key
}

/// The key of a span item of `request` in `redaction_items`.
fn item_span_key(request: &Hash, loc: &Location) -> [u8; 45] {
    let mut key = [0u8; 45];
    key[..32].copy_from_slice(&request.0);
    key[32] = 1;
    key[33..].copy_from_slice(&loc_key(loc));
    key
}

/// The prefix of `request`'s items of one class (0 objects, 1 spans) in `redaction_items`.
fn item_prefix(request: &Hash, class: u8) -> [u8; 33] {
    let mut key = [0u8; 33];
    key[..32].copy_from_slice(&request.0);
    key[32] = class;
    key
}

fn enc(loc: &Location) -> Vec<u8> {
    postcard::to_allocvec(loc).expect("encode location")
}

fn dec(bytes: &[u8]) -> Result<Location, StoreError> {
    postcard::from_bytes(bytes).map_err(|e| StoreError::Decode(e.to_string()))
}

/// A 32-byte hash stored as an index value; anything else is damage, reported as a decode error.
pub(crate) fn hash_from(bytes: &[u8], what: &str) -> Result<Hash, StoreError> {
    <[u8; 32]>::try_from(bytes).map(Hash).map_err(|_| {
        StoreError::Decode(format!(
            "{what} is {} bytes, expected a 32-byte hash",
            bytes.len()
        ))
    })
}

/// A redb open error: `HeldByReader(path)` when another live `Database` (in this process, e.g. a
/// reader that outlived its store, or another one) holds the file's lock, which is not damage;
/// anything else is `Db`.
fn open_error(path: &Path, e: redb::DatabaseError) -> StoreError {
    match e {
        redb::DatabaseError::DatabaseAlreadyOpen => StoreError::HeldByReader(path.to_path_buf()),
        e => db(e),
    }
}

/// The raw rows of the seven redaction tables: carriers, items, objects, applied, active, spans,
/// carried places.
pub(crate) type RedactionRows = [BTreeMap<Vec<u8>, Vec<u8>>; 7];

/// The seven redaction tables of one write transaction.
struct RedactionTables<'t> {
    carriers: redb::Table<'t, &'static [u8], &'static [u8]>,
    items: redb::Table<'t, &'static [u8], &'static [u8]>,
    objects: redb::Table<'t, &'static [u8], &'static [u8]>,
    applied: redb::Table<'t, &'static [u8], &'static [u8]>,
    active: redb::Table<'t, &'static [u8], &'static [u8]>,
    spans: redb::Table<'t, &'static [u8], &'static [u8]>,
    carried_at: redb::Table<'t, &'static [u8], &'static [u8]>,
}

impl<'t> RedactionTables<'t> {
    fn open(w: &'t redb::WriteTransaction) -> Result<RedactionTables<'t>, StoreError> {
        Ok(RedactionTables {
            carriers: w.open_table(REDACTION_CARRIERS).map_err(db)?,
            items: w.open_table(REDACTION_ITEMS).map_err(db)?,
            objects: w.open_table(REDACTION_OBJECTS).map_err(db)?,
            applied: w.open_table(REDACTION_APPLIED).map_err(db)?,
            active: w.open_table(REDACTION_ACTIVE).map_err(db)?,
            spans: w.open_table(REDACTION_SPANS).map_err(db)?,
            carried_at: w.open_table(REDACTION_CARRIED_AT).map_err(db)?,
        })
    }

    /// A request's rows: its carrier (the first request logged for a carrier keeps it), its
    /// objects both ways, and its spans.
    fn request(
        &mut self,
        request: &Hash,
        carrier: &Hash,
        objects: &[(u8, Hash, Option<Hash>)],
        spans: &[RedactedSpan],
    ) -> Result<(), StoreError> {
        if self.carriers.get(&carrier.0[..]).map_err(db)?.is_none() {
            self.carriers
                .insert(&carrier.0[..], &request.0[..])
                .map_err(db)?;
        }
        for (kind, h, residue) in objects {
            self.items
                .insert(
                    &item_object_key(request, *kind, h)[..],
                    residue_bytes(residue),
                )
                .map_err(db)?;
            let mut key = Vec::with_capacity(65);
            key.extend_from_slice(&typed_key(*kind, h));
            key.extend_from_slice(&request.0);
            self.objects.insert(&key[..], &[][..]).map_err(db)?;
        }
        for span in spans {
            self.items
                .insert(
                    &item_span_key(request, &span.loc)[..],
                    &span_value(span)[..],
                )
                .map_err(db)?;
        }
        Ok(())
    }

    /// An apply's rows: the request's objects and spans enter the active mask, where an earlier
    /// apply keeps what it holds. A second apply of a request changes nothing; an apply of a
    /// request the log does not hold before it is `RedactionCorrupt` at the apply.
    fn apply(
        &mut self,
        flag: &AtomicBool,
        apply: &Hash,
        request: &Hash,
        loc: &Location,
    ) -> Result<(), StoreError> {
        if self.applied.get(&request.0[..]).map_err(db)?.is_some() {
            return Ok(());
        }
        let mut objects = vec![];
        let (lo, mid, hi) = (
            item_prefix(request, 0),
            item_prefix(request, 1),
            item_prefix(request, 2),
        );
        for item in self.items.range(&lo[..]..&mid[..]).map_err(db)? {
            let (k, v) = item.map_err(db)?;
            let k = k.value();
            objects.push((
                k[33],
                hash_from(&k[34..], "redaction item")?,
                residue_from(v.value(), "redaction item")?,
            ));
        }
        let mut spans = vec![];
        for item in self.items.range(&mid[..]..&hi[..]).map_err(db)? {
            let (k, v) = item.map_err(db)?;
            let at = loc_from(&k.value()[33..], "redaction item")?;
            spans.push(span_from(at, v.value(), "redaction item")?);
        }
        if objects.is_empty() && spans.is_empty() {
            return Err(StoreError::RedactionCorrupt {
                segment: loc.segment,
                offset: loc.offset,
            });
        }
        let mut a = Vec::with_capacity(44);
        a.extend_from_slice(&apply.0);
        a.extend_from_slice(&loc_key(loc));
        self.applied.insert(&request.0[..], &a[..]).map_err(db)?;
        flag.store(true, Ordering::Release);
        for (kind, h, residue) in objects {
            let key = typed_key(kind, &h);
            let held = self
                .active
                .get(&key[..])
                .map_err(db)?
                .map(|v| v.value().to_vec());
            let mut residue = residue;
            if let Some(v) = held {
                let row = active_object_from(&v)?;
                if self.is_apply_row(&row.request, &row.apply)? {
                    continue;
                }
                // a carried record's row has the residue that reads the object; the apply takes
                // the row over (it covers the object) but lists no span of it when the frame
                // was removed before, so the residue stays
                residue = residue.or(row.residue);
            }
            let mut v = Vec::with_capacity(108);
            v.extend_from_slice(&request.0);
            v.extend_from_slice(&a);
            v.extend_from_slice(residue_bytes(&residue));
            self.active.insert(&key[..], &v[..]).map_err(db)?;
        }
        for span in spans {
            let key = loc_key(&span.loc);
            let held = self
                .spans
                .get(&key[..])
                .map_err(db)?
                .map(|v| v.value().to_vec());
            if let Some(v) = held
                && span_row_from(span.loc, &v)?.held.is_none()
            {
                // an earlier apply keeps the frame
                continue;
            }
            let mut v = Vec::with_capacity(64 + 69);
            v.extend_from_slice(&apply.0);
            v.extend_from_slice(&request.0);
            v.extend_from_slice(&span_value(&span));
            self.spans.insert(&key[..], &v[..]).map_err(db)?;
        }
        Ok(())
    }

    /// Whether a mask row of `request` and `apply` is an apply's: `redaction_applied` names that
    /// apply for that request. Any other row is a carried record's.
    fn is_apply_row(&self, request: &Hash, apply: &Hash) -> Result<bool, StoreError> {
        Ok(self
            .applied
            .get(&request.0[..])
            .map_err(db)?
            .is_some_and(|v| v.value().get(..32) == Some(&apply.0[..])))
    }

    /// A carried record's rows. Every span gets a `redaction_spans` row with the record, its
    /// source request and its `held` flag where no row holds the frame yet; a `held: false` span
    /// also takes over a `held: true` row (it accounts for the zeros the earlier record holds).
    /// An apply's row, and an earlier `held: false` row, keep the frame. Only a `held: false`
    /// span's object enters the active mask, where no row holds it yet.
    fn carried(
        &mut self,
        flag: &AtomicBool,
        carried: &Hash,
        request: &Hash,
        loc: &Location,
        spans: &[(RedactedSpan, bool)],
    ) -> Result<(), StoreError> {
        self.carried_at
            .insert(&loc_key(loc)[..], &carried.0[..])
            .map_err(db)?;
        for (span, held) in spans {
            let key = loc_key(&span.loc);
            let row = self
                .spans
                .get(&key[..])
                .map_err(db)?
                .map(|v| span_row_from(span.loc, v.value()))
                .transpose()?;
            let take = match row {
                None => true,
                Some(r) => !*held && r.held == Some(true),
            };
            if take {
                let mut v = Vec::with_capacity(64 + 69 + 1);
                v.extend_from_slice(&carried.0);
                v.extend_from_slice(&request.0);
                v.extend_from_slice(&span_value(span));
                v.push(held_byte(*held));
                self.spans.insert(&key[..], &v[..]).map_err(db)?;
            }
            if *held {
                continue;
            }
            flag.store(true, Ordering::Release);
            let key = typed_key(span.kind, &span.hash);
            if self.active.get(&key[..]).map_err(db)?.is_none() {
                let mut v = Vec::with_capacity(108);
                v.extend_from_slice(&request.0);
                v.extend_from_slice(&carried.0);
                v.extend_from_slice(&loc_key(loc));
                v.extend_from_slice(residue_bytes(&span.residue));
                self.active.insert(&key[..], &v[..]).map_err(db)?;
            }
        }
        Ok(())
    }
}

impl Index {
    pub fn open(path: &Path) -> Result<Index, StoreError> {
        Index::with_tables(Database::create(path).map_err(|e| open_error(path, e))?)
    }

    /// Opens the index and verifies every page checksum (about 30 ms per 300k objects). Whether
    /// the file was clean is reported, not enforced: the caller decides whether a file redb had to
    /// repair is damage (set aside and rebuilt) or a rollback after a kill (trusted, then caught
    /// up). An error from redb is still an error.
    pub fn open_checked(path: &Path) -> Result<CheckedIndex, StoreError> {
        let repaired = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&repaired);
        let mut database = redb::Builder::new()
            .set_repair_callback(move |_| seen.store(true, Ordering::Relaxed))
            .create(path)
            .map_err(|e| open_error(path, e))?;
        let clean = database.check_integrity().map_err(db)?;
        Ok(CheckedIndex {
            index: Index::with_tables(database)?,
            clean,
            repaired_at_open: repaired.load(Ordering::Relaxed),
        })
    }

    fn with_tables(database: Database) -> Result<Index, StoreError> {
        let w = database.begin_write().map_err(db)?;
        let names: Vec<String> = w
            .list_tables()
            .map_err(db)?
            .map(|t| t.name().to_string())
            .collect();
        let has = |name: &str| names.iter().any(|n| n == name);
        // an index that holds objects but no signature table cannot say a subject is unsigned:
        // it is refused, and the open sets it aside and rebuilds it from the log
        if has(OBJECTS.name()) && !has(SIGNATURES.name()) {
            return Err(StoreError::Db(
                "the index has no signatures table; it is rebuilt from the log".into(),
            ));
        }
        // an index written before the open-proposal table existed gets it filled from its refs,
        // once, in this commit
        let had_open = has(OPEN_PROPOSALS.name());
        {
            w.open_table(OBJECTS).map_err(db)?;
            w.open_table(TYPED).map_err(db)?;
            w.open_table(PRODUCERS).map_err(db)?;
            w.open_table(VIEW_OPS).map_err(db)?;
            w.open_table(LANDED).map_err(db)?;
            w.open_table(DELETERS).map_err(db)?;
            w.open_table(REFS).map_err(db)?;
            w.open_table(OPEN_TENSIONS).map_err(db)?;
            w.open_table(OPEN_SUSPECTS).map_err(db)?;
            w.open_table(META).map_err(db)?;
            w.open_table(REDACTION_CARRIERS).map_err(db)?;
            w.open_table(REDACTION_ITEMS).map_err(db)?;
            w.open_table(REDACTION_OBJECTS).map_err(db)?;
            w.open_table(REDACTION_APPLIED).map_err(db)?;
            w.open_table(REDACTION_ACTIVE).map_err(db)?;
            w.open_table(REDACTION_SPANS).map_err(db)?;
            w.open_table(REDACTION_CARRIED_AT).map_err(db)?;
            w.open_table(SIGNATURES).map_err(db)?;
            let mut open = w.open_table(OPEN_PROPOSALS).map_err(db)?;
            if !had_open {
                let refs = w.open_table(REFS).map_err(db)?;
                for item in refs.range(OPEN_PREFIX..).map_err(db)? {
                    let (k, v) = item.map_err(db)?;
                    if !k.value().starts_with(OPEN_PREFIX) {
                        break;
                    }
                    if !v.value().is_empty() {
                        open.insert(k.value(), v.value()).map_err(db)?;
                    }
                }
            }
        }
        w.commit().map_err(db)?;
        let redacted = {
            let r = database.begin_read().map_err(db)?;
            let t = r.open_table(REDACTION_ACTIVE).map_err(db)?;
            t.first().map_err(db)?.is_some()
        };
        Ok(Index {
            db: database,
            redacted: AtomicBool::new(redacted),
        })
    }

    /// Applies `entries` and moves `indexed_upto` to `upto` in one commit that is not durable: a
    /// kill or power loss can lose it, so it must be reproducible from the log. The next durable
    /// commit (`checkpoint`, `apply_durable`) makes it and every earlier one durable.
    pub fn apply(&self, entries: &[IndexEntry], upto: Location) -> Result<(), StoreError> {
        self.apply_with(entries, upto, Durability::None)
    }

    /// `apply`, durable when it returns. No store path calls it: it is the test hook that builds a
    /// durable index lagging the log.
    pub fn apply_durable(&self, entries: &[IndexEntry], upto: Location) -> Result<(), StoreError> {
        self.apply_with(entries, upto, Durability::Immediate)
    }

    /// A durable commit that records the current `indexed_upto` as `checkpointed_upto`, which
    /// makes every earlier non-durable commit durable. It always writes, so redb never skips it
    /// as an empty commit.
    pub fn checkpoint(&self) -> Result<(), StoreError> {
        let w = self.db.begin_write().map_err(db)?;
        {
            let mut meta = w.open_table(META).map_err(db)?;
            let upto = match meta.get("indexed_upto").map_err(db)? {
                Some(v) => v.value().to_vec(),
                None => enc(&Location {
                    segment: 0,
                    offset: 0,
                }),
            };
            meta.insert("checkpointed_upto", &upto[..]).map_err(db)?;
        }
        w.commit().map_err(db)?;
        Ok(())
    }

    fn apply_with(
        &self,
        entries: &[IndexEntry],
        upto: Location,
        durability: Durability,
    ) -> Result<(), StoreError> {
        let mut w = self.db.begin_write().map_err(db)?;
        w.set_durability(durability).map_err(db)?;
        {
            let mut objects = w.open_table(OBJECTS).map_err(db)?;
            let mut typed = w.open_table(TYPED).map_err(db)?;
            let mut producers = w.open_table(PRODUCERS).map_err(db)?;
            let mut view_ops = w.open_table(VIEW_OPS).map_err(db)?;
            let mut landed = w.open_table(LANDED).map_err(db)?;
            let mut deleters = w.open_table(DELETERS).map_err(db)?;
            let mut refs = w.open_table(REFS).map_err(db)?;
            let mut open_proposals = w.open_table(OPEN_PROPOSALS).map_err(db)?;
            let mut open_tensions = w.open_table(OPEN_TENSIONS).map_err(db)?;
            let mut open_suspects = w.open_table(OPEN_SUSPECTS).map_err(db)?;
            let mut meta = w.open_table(META).map_err(db)?;
            // the redaction tables are opened only for a batch with a redaction record, and the
            // signature table only for a batch with a signature
            let mut redaction: Option<RedactionTables<'_>> = None;
            let mut signatures: Option<redb::Table<'_, &'static [u8], &'static [u8]>> = None;
            for e in entries {
                match e {
                    IndexEntry::Object { kind, hash, loc } => {
                        let at = enc(loc);
                        typed
                            .insert(&typed_key(*kind, hash)[..], &at[..])
                            .map_err(db)?;
                        // the untyped entry keeps the first frame written with this hash
                        if objects.get(&hash.0[..]).map_err(db)?.is_none() {
                            objects.insert(&hash.0[..], &at[..]).map_err(db)?;
                        }
                    }
                    IndexEntry::Producer { version, change } => {
                        let mut key = [0u8; 64];
                        key[..32].copy_from_slice(&version.0);
                        key[32..].copy_from_slice(&change.0);
                        producers.insert(&key[..], &[][..]).map_err(db)?;
                    }
                    IndexEntry::Ref(name, value) => {
                        refs.insert(name.as_str(), &value[..]).map_err(db)?;
                        if name.starts_with(OPEN_PREFIX) {
                            if value.is_empty() {
                                open_proposals.remove(name.as_str()).map_err(db)?;
                            } else {
                                open_proposals
                                    .insert(name.as_str(), &value[..])
                                    .map_err(db)?;
                            }
                        }
                    }
                    IndexEntry::ViewOp { view, op } => {
                        let mut key = name_prefix(view);
                        key.extend_from_slice(&op.0);
                        view_ops.insert(&key[..], &[][..]).map_err(db)?;
                    }
                    IndexEntry::Landed { change, landing } => {
                        // the first landing that carried a change keeps it
                        if landed.get(&change.0[..]).map_err(db)?.is_none() {
                            landed.insert(&change.0[..], &landing.0[..]).map_err(db)?;
                        }
                    }
                    IndexEntry::Deleted { block, change } => {
                        let mut key = name_prefix(block.as_str());
                        key.extend_from_slice(&change.0);
                        deleters.insert(&key[..], &[][..]).map_err(db)?;
                    }
                    IndexEntry::TensionOpen { id, record } => {
                        open_tensions.insert(&id.0[..], &record.0[..]).map_err(db)?;
                    }
                    IndexEntry::TensionShut { id } => {
                        open_tensions.remove(&id.0[..]).map_err(db)?;
                    }
                    IndexEntry::SuspectOpen { key, record } => {
                        open_suspects
                            .insert(&key.0[..], &record.0[..])
                            .map_err(db)?;
                    }
                    IndexEntry::SuspectShut { key } => {
                        open_suspects.remove(&key.0[..]).map_err(db)?;
                    }
                    IndexEntry::RedactionRequest {
                        request,
                        carrier,
                        objects,
                        spans,
                    } => {
                        let t = match &mut redaction {
                            Some(t) => t,
                            None => redaction.insert(RedactionTables::open(&w)?),
                        };
                        t.request(request, carrier, objects, spans)?;
                    }
                    IndexEntry::RedactionApply {
                        apply,
                        request,
                        loc,
                    } => {
                        let t = match &mut redaction {
                            Some(t) => t,
                            None => redaction.insert(RedactionTables::open(&w)?),
                        };
                        t.apply(&self.redacted, apply, request, loc)?;
                    }
                    IndexEntry::RedactionCarried {
                        carried,
                        request,
                        loc,
                        spans,
                    } => {
                        let t = match &mut redaction {
                            Some(t) => t,
                            None => redaction.insert(RedactionTables::open(&w)?),
                        };
                        t.carried(&self.redacted, carried, request, loc, spans)?;
                    }
                    IndexEntry::Signature { subject, signature } => {
                        let t = match &mut signatures {
                            Some(t) => t,
                            None => signatures.insert(w.open_table(SIGNATURES).map_err(db)?),
                        };
                        t.insert(&signature_key(subject, signature)[..], &[][..])
                            .map_err(db)?;
                    }
                }
            }
            meta.insert("indexed_upto", &enc(&upto)[..]).map_err(db)?;
        }
        w.commit().map_err(db)?;
        Ok(())
    }

    pub fn object(&self, h: &Hash) -> Result<Option<Location>, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(OBJECTS).map_err(db)?;
        match t.get(&h.0[..]).map_err(db)? {
            Some(v) => Ok(Some(dec(v.value())?)),
            None => Ok(None),
        }
    }

    /// The location of the object of `kind` with hash `h`.
    pub fn typed(&self, kind: u8, h: &Hash) -> Result<Option<Location>, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(TYPED).map_err(db)?;
        match t.get(&typed_key(kind, h)[..]).map_err(db)? {
            Some(v) => Ok(Some(dec(v.value())?)),
            None => Ok(None),
        }
    }

    /// The changes with an op that produces `version`, in ascending hash order.
    pub fn producers(&self, version: &Hash) -> Result<Vec<Hash>, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(PRODUCERS).map_err(db)?;
        let mut out = vec![];
        for item in t.range(&version.0[..]..).map_err(db)? {
            let (k, _) = item.map_err(db)?;
            let key = k.value();
            if !key.starts_with(&version.0) {
                break;
            }
            // every key is a version and a change, 64 bytes; anything else is damage
            out.push(hash_from(&key[32..], "producer entry")?);
        }
        Ok(out)
    }

    /// Calls `f` with the hash of every stored object of `kind`, once each, in ascending hash
    /// order: one read of the typed table, no log walk. An object whose payload a redaction
    /// removed is still stored (its frame and hash remain).
    pub fn for_each_object_of(
        &self,
        kind: u8,
        mut f: impl FnMut(Hash) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(TYPED).map_err(db)?;
        for item in t.iter().map_err(db)? {
            let (k, _) = item.map_err(db)?;
            let key = k.value();
            // every key is a hash and a kind byte, 33 bytes; anything else is damage
            if key.len() != 33 {
                return Err(StoreError::Decode(format!(
                    "typed entry is {} bytes, expected 33",
                    key.len()
                )));
            }
            if key[32] == kind {
                f(hash_from(&key[..32], "typed entry")?)?;
            }
        }
        Ok(())
    }

    /// The ops of `view`'s chain, in ascending hash order.
    pub fn view_ops(&self, view: &str) -> Result<Vec<Hash>, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(VIEW_OPS).map_err(db)?;
        hashes_under(&t, view, "view op entry")
    }

    /// The changes with an op that deletes `block`, in ascending hash order.
    pub fn deleters(&self, block: &BlockId) -> Result<Vec<Hash>, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(DELETERS).map_err(db)?;
        hashes_under(&t, block.as_str(), "deleter entry")
    }

    /// For each of `blocks` that has a deleter, its deleters in ascending hash order, each with
    /// the landing that carried it into canon and that landing's location, if one has: one read
    /// transaction for all of them, and none when `blocks` is empty.
    #[allow(clippy::type_complexity)]
    pub fn deleter_rows<'a>(
        &self,
        blocks: impl IntoIterator<Item = &'a BlockId>,
    ) -> Result<BTreeMap<BlockId, Vec<(Hash, Option<(Hash, Location)>)>>, StoreError> {
        let mut blocks = blocks.into_iter().peekable();
        if blocks.peek().is_none() {
            return Ok(BTreeMap::new());
        }
        let r = self.db.begin_read().map_err(db)?;
        let deleters = r.open_table(DELETERS).map_err(db)?;
        let mut out = BTreeMap::new();
        let mut tables = None;
        for block in blocks {
            if out.contains_key(block) {
                continue;
            }
            let changes = hashes_under(&deleters, block.as_str(), "deleter entry")?;
            if changes.is_empty() {
                continue;
            }
            if tables.is_none() {
                tables = Some((
                    r.open_table(LANDED).map_err(db)?,
                    r.open_table(TYPED).map_err(db)?,
                ));
            }
            let (landed, typed) = tables.as_ref().expect("opened above");
            let mut rows = Vec::with_capacity(changes.len());
            for c in changes {
                let at = match landed.get(&c.0[..]).map_err(db)? {
                    Some(v) => {
                        let l = hash_from(v.value(), "landed entry")?;
                        let loc = typed
                            .get(&typed_key(crate::kinds::LANDING, &l)[..])
                            .map_err(db)?
                            .ok_or(StoreError::NotFound(l))?;
                        Some((l, dec(loc.value())?))
                    }
                    None => None,
                };
                rows.push((c, at));
            }
            out.insert(block.clone(), rows);
        }
        Ok(out)
    }

    /// The landing that carried `change` into canon, if one has.
    pub fn landing_of(&self, change: &Hash) -> Result<Option<Hash>, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(LANDED).map_err(db)?;
        match t.get(&change.0[..]).map_err(db)? {
            Some(v) => Ok(Some(hash_from(v.value(), "landed entry")?)),
            None => Ok(None),
        }
    }

    /// Test hook: overwrites the landed entry of `change` with raw bytes, so a test can damage it.
    #[doc(hidden)]
    pub fn set_landed_raw_for_tests(&self, change: &Hash, raw: &[u8]) -> Result<(), StoreError> {
        let w = self.db.begin_write().map_err(db)?;
        {
            let mut t = w.open_table(LANDED).map_err(db)?;
            t.insert(&change.0[..], raw).map_err(db)?;
        }
        w.commit().map_err(db)
    }

    /// Every table's rows, from one read transaction.
    #[doc(hidden)]
    pub fn dump(&self) -> Result<IndexDump, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let named = |def: TableDefinition<&'static str, &'static [u8]>| {
            let mut out = BTreeMap::new();
            for item in r.open_table(def).map_err(db)?.iter().map_err(db)? {
                let (k, v) = item.map_err(db)?;
                out.insert(k.value().as_bytes().to_vec(), v.value().to_vec());
            }
            Ok::<_, StoreError>(out)
        };
        let refs = named(REFS)?;
        let open_proposals = named(OPEN_PROPOSALS)?;
        Ok(IndexDump {
            objects: dump_table(&r, OBJECTS)?,
            typed: dump_table(&r, TYPED)?,
            producers: dump_table(&r, PRODUCERS)?,
            refs,
            view_ops: dump_table(&r, VIEW_OPS)?,
            landed: dump_table(&r, LANDED)?,
            deleters: dump_table(&r, DELETERS)?,
            open_tensions: dump_table(&r, OPEN_TENSIONS)?,
            open_suspects: dump_table(&r, OPEN_SUSPECTS)?,
            open_proposals,
            redaction_carriers: dump_table(&r, REDACTION_CARRIERS)?,
            redaction_items: dump_table(&r, REDACTION_ITEMS)?,
            redaction_objects: dump_table(&r, REDACTION_OBJECTS)?,
            redaction_applied: dump_table(&r, REDACTION_APPLIED)?,
            redaction_active: dump_table(&r, REDACTION_ACTIVE)?,
            redaction_spans: dump_table(&r, REDACTION_SPANS)?,
            redaction_carried_at: dump_table(&r, REDACTION_CARRIED_AT)?,
            signatures: dump_table(&r, SIGNATURES)?,
        })
    }

    /// The signatures of `subject`, in ascending hash order: at most `cap`. Reading one more
    /// entry stops the read with `signing input exceeds {cap} signatures per subject`.
    pub fn signatures_of(&self, subject: &Hash, cap: usize) -> Result<Vec<Hash>, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(SIGNATURES).map_err(db)?;
        let mut out = vec![];
        for item in t.range(&subject.0[..]..).map_err(db)? {
            let (k, _) = item.map_err(db)?;
            let key = k.value();
            if !key.starts_with(&subject.0) {
                break;
            }
            if out.len() == cap {
                return Err(crate::signature::over_per_subject());
            }
            out.push(signature_from(key)?.1);
        }
        Ok(out)
    }

    /// The signatures of `subject` whose frame lies before `end`, in ascending hash order: at
    /// most `cap`, counted among those only. A frame at or after `end` (a batch that is still
    /// open, or one sealed after the state that asks) is left out. Both tables are read in one
    /// transaction. A signature entry with no typed frame entry is `NotFound`, as reading the
    /// record would be.
    pub fn signatures_before(
        &self,
        subject: &Hash,
        cap: usize,
        end: Location,
    ) -> Result<Vec<Hash>, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(SIGNATURES).map_err(db)?;
        let typed = r.open_table(TYPED).map_err(db)?;
        let mut out = vec![];
        for item in t.range(&subject.0[..]..).map_err(db)? {
            let (k, _) = item.map_err(db)?;
            let key = k.value();
            if !key.starts_with(&subject.0) {
                break;
            }
            let id = signature_from(key)?.1;
            let at = typed
                .get(&typed_key(crate::kinds::SIGNATURE, &id)[..])
                .map_err(db)?
                .ok_or(StoreError::NotFound(id))?;
            if dec(at.value())? >= end {
                continue;
            }
            if out.len() == cap {
                return Err(crate::signature::over_per_subject());
            }
            out.push(id);
        }
        Ok(out)
    }

    /// Calls `f` with the hash of every signature, in subject then hash order, from one read
    /// transaction; nothing is collected.
    pub fn for_each_signature(
        &self,
        f: &mut dyn FnMut(Hash) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(SIGNATURES).map_err(db)?;
        for item in t.iter().map_err(db)? {
            let (k, _) = item.map_err(db)?;
            f(signature_from(k.value())?.1)?;
        }
        Ok(())
    }

    /// Whether the signature table holds exactly one row per stored SIGNATURE object, read in one
    /// read transaction, so a live writer's new signature is not a false failure (rdr1).
    /// `subject_of` reads each object's subject; its error stops the check.
    pub fn signature_rows_match(
        &self,
        subject_of: &mut dyn FnMut(Hash) -> Result<Hash, StoreError>,
    ) -> Result<bool, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let typed = r.open_table(TYPED).map_err(db)?;
        let rows = r.open_table(SIGNATURES).map_err(db)?;
        let mut n: u64 = 0;
        for item in typed.iter().map_err(db)? {
            let (k, _) = item.map_err(db)?;
            let key = k.value();
            if key.len() != 33 || key[32] != crate::kinds::SIGNATURE {
                continue;
            }
            let h = hash_from(&key[..32], "typed entry")?;
            let subject = subject_of(h)?;
            if rows
                .get(&signature_key(&subject, &h)[..])
                .map_err(db)?
                .is_none()
            {
                return Ok(false);
            }
            n += 1;
        }
        Ok(redb::ReadableTableMetadata::len(&rows).map_err(db)? == n)
    }

    /// Drops the signature table in a durable commit (`Store::drop_signature_table_for_tests`).
    pub(crate) fn drop_signature_table_for_tests(&self) -> Result<(), StoreError> {
        let w = self.db.begin_write().map_err(db)?;
        w.delete_table(SIGNATURES).map_err(db)?;
        w.commit().map_err(db)?;
        Ok(())
    }

    /// The open tensions: (id, the record that opened it), in id order.
    pub fn open_tensions(&self) -> Result<Vec<(Hash, Hash)>, StoreError> {
        self.open_rows(OPEN_TENSIONS, "open tension entry")
    }

    /// The open suspects: (suspect key, the record that made it new), in key order.
    pub fn open_suspects(&self) -> Result<Vec<(Hash, Hash)>, StoreError> {
        self.open_rows(OPEN_SUSPECTS, "open suspect entry")
    }

    fn open_rows(
        &self,
        def: TableDefinition<&'static [u8], &'static [u8]>,
        what: &str,
    ) -> Result<Vec<(Hash, Hash)>, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(def).map_err(db)?;
        let mut out = vec![];
        for item in t.iter().map_err(db)? {
            let (k, v) = item.map_err(db)?;
            out.push((hash_from(k.value(), what)?, hash_from(v.value(), what)?));
        }
        Ok(out)
    }

    /// Writes one raw `producers` row in a durable commit. No store path calls it: it is the test
    /// hook that damages the index (a key that is not a version and a change).
    #[doc(hidden)]
    pub fn put_raw_producer_for_tests(&self, key: &[u8]) -> Result<(), StoreError> {
        let w = self.db.begin_write().map_err(db)?;
        {
            let mut t = w.open_table(PRODUCERS).map_err(db)?;
            t.insert(key, &[][..]).map_err(db)?;
        }
        w.commit().map_err(db)?;
        Ok(())
    }

    /// An empty index in memory, for deriving the redaction rows a log's records give
    /// (`Store::redaction_index_matches`). Nothing is written to disk.
    pub(crate) fn in_memory() -> Result<Index, StoreError> {
        let database = redb::Builder::new()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .map_err(db)?;
        Index::with_tables(database)
    }

    /// The rows of the seven redaction tables, from one read transaction: carriers, items,
    /// objects, applied, active, spans, carried places.
    pub(crate) fn redaction_rows(&self) -> Result<RedactionRows, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        Ok([
            dump_table(&r, REDACTION_CARRIERS)?,
            dump_table(&r, REDACTION_ITEMS)?,
            dump_table(&r, REDACTION_OBJECTS)?,
            dump_table(&r, REDACTION_APPLIED)?,
            dump_table(&r, REDACTION_ACTIVE)?,
            dump_table(&r, REDACTION_SPANS)?,
            dump_table(&r, REDACTION_CARRIED_AT)?,
        ])
    }

    /// Writes a forged active span and mask row (an apply the log does not hold) in a durable
    /// commit. No store path calls it: it is the test hook that forges an authorization in the
    /// index, which the next open must not trust.
    #[doc(hidden)]
    pub fn forge_active_span_for_tests(
        &self,
        span: &RedactedSpan,
        apply: &Hash,
        request: &Hash,
    ) -> Result<(), StoreError> {
        let w = self.db.begin_write().map_err(db)?;
        {
            let mut spans = w.open_table(REDACTION_SPANS).map_err(db)?;
            let mut v = Vec::with_capacity(64 + 69);
            v.extend_from_slice(&apply.0);
            v.extend_from_slice(&request.0);
            v.extend_from_slice(&span_value(span));
            spans.insert(&loc_key(&span.loc)[..], &v[..]).map_err(db)?;
            let mut active = w.open_table(REDACTION_ACTIVE).map_err(db)?;
            let mut v = Vec::with_capacity(76);
            v.extend_from_slice(&request.0);
            v.extend_from_slice(&apply.0);
            v.extend_from_slice(&loc_key(&span.loc));
            active
                .insert(&typed_key(span.kind, &span.hash)[..], &v[..])
                .map_err(db)?;
        }
        w.commit().map_err(db)?;
        self.redacted.store(true, Ordering::Release);
        Ok(())
    }

    /// Empties the applied, active, span and carried-place tables in a durable commit. No store
    /// path calls it: it is the test hook of an index that forgot the log's authorization.
    #[doc(hidden)]
    pub fn clear_redaction_rows_for_tests(&self) -> Result<(), StoreError> {
        let w = self.db.begin_write().map_err(db)?;
        for def in [
            REDACTION_APPLIED,
            REDACTION_ACTIVE,
            REDACTION_SPANS,
            REDACTION_CARRIED_AT,
        ] {
            w.delete_table(def).map_err(db)?;
            w.open_table(def).map_err(db)?;
        }
        w.commit().map_err(db)?;
        self.redacted.store(false, Ordering::Release);
        Ok(())
    }

    /// Whether the active redaction mask may hold a row (see `Index::redacted`).
    pub fn any_redaction_active(&self) -> bool {
        self.redacted.load(Ordering::Acquire)
    }

    /// The request logged for carrier change `carrier`, if one was.
    pub fn redaction_carrier(&self, carrier: &Hash) -> Result<Option<Hash>, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(REDACTION_CARRIERS).map_err(db)?;
        match t.get(&carrier.0[..]).map_err(db)? {
            Some(v) => Ok(Some(hash_from(v.value(), "redaction carrier entry")?)),
            None => Ok(None),
        }
    }

    /// The active mask's row for the object of `kind` with hash `h`.
    pub fn redaction_active(&self, kind: u8, h: &Hash) -> Result<Option<ActiveObject>, StoreError> {
        if !self.any_redaction_active() {
            return Ok(None);
        }
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(REDACTION_ACTIVE).map_err(db)?;
        match t.get(&typed_key(kind, h)[..]).map_err(db)? {
            Some(v) => Ok(Some(active_object_from(v.value())?)),
            None => Ok(None),
        }
    }

    /// Of `hashes`, the first that the active mask holds under any kind, with the request that
    /// removed it (of several kinds, the lowest kind byte's): one read transaction, and none when
    /// the mask is empty.
    pub fn redaction_active_hit(
        &self,
        hashes: &[Hash],
    ) -> Result<Option<(Hash, Hash)>, StoreError> {
        if hashes.is_empty() || !self.any_redaction_active() {
            return Ok(None);
        }
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(REDACTION_ACTIVE).map_err(db)?;
        for h in hashes {
            let lo = typed_key(0, h);
            let hi = typed_key(u8::MAX, h);
            if let Some(item) = t.range(&lo[..]..=&hi[..]).map_err(db)?.next() {
                let (_, v) = item.map_err(db)?;
                return Ok(Some((*h, active_object_from(v.value())?.request)));
            }
        }
        Ok(None)
    }

    /// The active spans of frames in `[from, upto)`, by location.
    pub fn redaction_spans_in(
        &self,
        from: Location,
        upto: Location,
    ) -> Result<BTreeMap<Location, ActiveSpan>, StoreError> {
        let mut out = BTreeMap::new();
        if from >= upto || !self.any_redaction_active() {
            return Ok(out);
        }
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(REDACTION_SPANS).map_err(db)?;
        let (lo, hi) = (loc_key(&from), loc_key(&upto));
        for item in t.range(&lo[..]..&hi[..]).map_err(db)? {
            let (k, v) = item.map_err(db)?;
            let at = loc_from(k.value(), "redaction span entry")?;
            let row = span_row_from(at, v.value())?;
            if row.masks() {
                out.insert(at, row.active);
            }
        }
        Ok(out)
    }

    /// The active span of the frame at `loc`, if the mask holds it (a `held: true` carried row
    /// masks nothing).
    pub fn redaction_span_at(&self, loc: &Location) -> Result<Option<ActiveSpan>, StoreError> {
        if !self.any_redaction_active() {
            return Ok(None);
        }
        Ok(self
            .redaction_span_row_at(loc)?
            .filter(SpanRow::masks)
            .map(|r| r.active))
    }

    /// Calls `f` with the place and hash of every carried record frame that starts before
    /// `before`, in physical order: one range read of the carried-place rows, no log walk.
    pub(crate) fn for_each_carried_place(
        &self,
        before: Location,
        mut f: impl FnMut(Location, Hash) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(REDACTION_CARRIED_AT).map_err(db)?;
        for item in t.range(..&loc_key(&before)[..]).map_err(db)? {
            let (k, v) = item.map_err(db)?;
            let loc = loc_from(k.value(), "carried place entry")?;
            f(loc, hash_from(v.value(), "carried place entry")?)?;
        }
        Ok(())
    }

    /// The `redaction_spans` row of the frame at `loc`, masking or not.
    pub(crate) fn redaction_span_row_at(
        &self,
        loc: &Location,
    ) -> Result<Option<SpanRow>, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(REDACTION_SPANS).map_err(db)?;
        match t.get(&loc_key(loc)[..]).map_err(db)? {
            Some(v) => Ok(Some(span_row_from(*loc, v.value())?)),
            None => Ok(None),
        }
    }

    /// The requests that name the object of `kind` with hash `h`, applied or not, ascending.
    pub fn redaction_requests_naming(&self, kind: u8, h: &Hash) -> Result<Vec<Hash>, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(REDACTION_OBJECTS).map_err(db)?;
        let prefix = typed_key(kind, h);
        let mut out = vec![];
        for item in t.range(&prefix[..]..).map_err(db)? {
            let (k, _) = item.map_err(db)?;
            let key = k.value();
            if !key.starts_with(&prefix) {
                break;
            }
            out.push(hash_from(&key[33..], "redaction object entry")?);
        }
        Ok(out)
    }

    /// Request `request`'s items: the objects it removes or names (kind, hash, residue), and the
    /// frame payloads it removes, in log order.
    #[allow(clippy::type_complexity)]
    pub fn redaction_request_items(
        &self,
        request: &Hash,
    ) -> Result<(Vec<(u8, Hash, Option<Hash>)>, Vec<RedactedSpan>), StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(REDACTION_ITEMS).map_err(db)?;
        let (lo, mid, hi) = (
            item_prefix(request, 0),
            item_prefix(request, 1),
            item_prefix(request, 2),
        );
        let mut objects = vec![];
        for item in t.range(&lo[..]..&mid[..]).map_err(db)? {
            let (k, v) = item.map_err(db)?;
            let k = k.value();
            objects.push((
                k[33],
                hash_from(&k[34..], "redaction item")?,
                residue_from(v.value(), "redaction item")?,
            ));
        }
        let mut spans = vec![];
        for item in t.range(&mid[..]..&hi[..]).map_err(db)? {
            let (k, v) = item.map_err(db)?;
            let at = loc_from(&k.value()[33..], "redaction item")?;
            spans.push(span_from(at, v.value(), "redaction item")?);
        }
        Ok((objects, spans))
    }

    /// The apply record that activated request `request`, and its location, if one has.
    pub fn redaction_applied(
        &self,
        request: &Hash,
    ) -> Result<Option<(Hash, Location)>, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(REDACTION_APPLIED).map_err(db)?;
        match t.get(&request.0[..]).map_err(db)? {
            Some(v) => {
                let v = v.value();
                if v.len() != 44 {
                    return Err(StoreError::Decode(format!(
                        "redaction apply entry is {} bytes",
                        v.len()
                    )));
                }
                Ok(Some((
                    hash_from(&v[..32], "redaction apply entry")?,
                    loc_from(&v[32..], "redaction apply entry")?,
                )))
            }
            None => Ok(None),
        }
    }

    pub fn get_ref(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(REFS).map_err(db)?;
        Ok(t.get(name).map_err(db)?.map(|v| v.value().to_vec()))
    }

    pub fn refs_with_prefix(&self, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(REFS).map_err(db)?;
        let mut out = vec![];
        for item in t.range(prefix..).map_err(db)? {
            let (k, v) = item.map_err(db)?;
            if !k.value().starts_with(prefix) {
                break;
            }
            out.push((k.value().to_string(), v.value().to_vec()));
        }
        Ok(out)
    }

    /// The `open/<view>` refs that name an open proposal, with their values, by name: the refs
    /// `refs_with_prefix("open/")` gives less the empty ones, read from [`OPEN_PROPOSALS`], so
    /// the cost follows the open proposals, not every view that ever proposed.
    pub fn open_proposal_refs(&self) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(OPEN_PROPOSALS).map_err(db)?;
        let mut out = vec![];
        for item in t.iter().map_err(db)? {
            let (k, v) = item.map_err(db)?;
            out.push((k.value().to_string(), v.value().to_vec()));
        }
        Ok(out)
    }

    /// Whether [`OPEN_PROPOSALS`] holds exactly the `open/` refs whose value is not empty, both
    /// read in one read transaction: a writer that proposes or lands between two reads would
    /// make a sound index look damaged (rdr1).
    pub fn open_proposals_match_refs(&self) -> Result<bool, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let refs = r.open_table(REFS).map_err(db)?;
        let mut named = vec![];
        for item in refs.range(OPEN_PREFIX..).map_err(db)? {
            let (k, v) = item.map_err(db)?;
            if !k.value().starts_with(OPEN_PREFIX) {
                break;
            }
            if !v.value().is_empty() {
                named.push((k.value().to_string(), v.value().to_vec()));
            }
        }
        let open = r.open_table(OPEN_PROPOSALS).map_err(db)?;
        let mut table = vec![];
        for item in open.iter().map_err(db)? {
            let (k, v) = item.map_err(db)?;
            table.push((k.value().to_string(), v.value().to_vec()));
        }
        Ok(named == table)
    }

    /// How many refs start with `prefix`, counted without reading their values.
    pub fn count_refs(&self, prefix: &str) -> Result<usize, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(REFS).map_err(db)?;
        let mut n = 0;
        for item in t.range(prefix..).map_err(db)? {
            let (k, _) = item.map_err(db)?;
            if !k.value().starts_with(prefix) {
                break;
            }
            n += 1;
        }
        Ok(n)
    }

    /// The acknowledged end of the log, or None if this index has never applied a write.
    pub fn acknowledged(&self) -> Result<Option<Location>, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(META).map_err(db)?;
        t.get("indexed_upto")
            .map_err(db)?
            .map(|v| dec(v.value()))
            .transpose()
    }

    pub fn indexed_upto(&self) -> Result<Location, StoreError> {
        Ok(self.acknowledged()?.unwrap_or(Location {
            segment: 0,
            offset: 0,
        }))
    }
}

impl crate::Store {
    /// Test hook: overwrites the landed entry of `change` with raw bytes (a damaged entry when
    /// `raw` is not a hash).
    #[doc(hidden)]
    pub fn set_landed_raw_for_tests(
        &mut self,
        change: &Hash,
        raw: &[u8],
    ) -> Result<(), StoreError> {
        self.index().set_landed_raw_for_tests(change, raw)
    }
}
