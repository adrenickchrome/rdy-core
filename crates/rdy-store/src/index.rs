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
}

pub struct Index {
    db: Database,
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
        // an index written before the open-proposal table existed gets it filled from its refs,
        // once, in this commit
        let had_open = w
            .list_tables()
            .map_err(db)?
            .any(|t| t.name() == OPEN_PROPOSALS.name());
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
        Ok(Index { db: database })
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
        })
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
