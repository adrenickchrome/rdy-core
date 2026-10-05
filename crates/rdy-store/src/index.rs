use crate::error::{StoreError, db};
use crate::segment::Location;
use rdy_model::Hash;
use redb::{Database, Durability, ReadableDatabase, ReadableTable, TableDefinition};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// hash -> the location of the first frame written with that hash, of whatever kind (untyped reads).
const OBJECTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("objects");
/// hash (32 bytes) followed by the kind byte -> the location of that kind's object with that hash.
const TYPED: TableDefinition<&[u8], &[u8]> = TableDefinition::new("typed");
const PRODUCERS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("producers");
/// a view name's UTF-8 bytes, a 0x00 byte, then the op hash -> empty: the ops of each view.
const VIEW_OPS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("view_ops");
/// change hash -> the hash of the first landing that carried it into canon.
const LANDED: TableDefinition<&[u8], &[u8]> = TableDefinition::new("landed");
const REFS: TableDefinition<&str, &[u8]> = TableDefinition::new("refs");
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

pub enum IndexEntry {
    /// An object frame: indexed by (kind, hash), and by hash alone if it is the first frame
    /// written with that hash.
    Object {
        kind: u8,
        hash: Hash,
        loc: Location,
    },
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

/// The `typed` table's key: the hash's 32 bytes, then the kind byte.
fn typed_key(kind: u8, h: &Hash) -> [u8; 33] {
    let mut key = [0u8; 33];
    key[..32].copy_from_slice(&h.0);
    key[32] = kind;
    key
}

/// The `view_ops` key prefix of `view`: its UTF-8 bytes, then a 0x00 byte.
fn view_ops_prefix(view: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(view.len() + 33);
    key.extend_from_slice(view.as_bytes());
    key.push(0);
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

impl Index {
    pub fn open(path: &Path) -> Result<Index, StoreError> {
        Index::with_tables(Database::create(path).map_err(db)?)
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
            .map_err(db)?;
        let clean = database.check_integrity().map_err(db)?;
        Ok(CheckedIndex {
            index: Index::with_tables(database)?,
            clean,
            repaired_at_open: repaired.load(Ordering::Relaxed),
        })
    }

    fn with_tables(database: Database) -> Result<Index, StoreError> {
        let w = database.begin_write().map_err(db)?;
        {
            w.open_table(OBJECTS).map_err(db)?;
            w.open_table(TYPED).map_err(db)?;
            w.open_table(PRODUCERS).map_err(db)?;
            w.open_table(VIEW_OPS).map_err(db)?;
            w.open_table(LANDED).map_err(db)?;
            w.open_table(REFS).map_err(db)?;
            w.open_table(META).map_err(db)?;
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

    /// `apply`, durable when it returns. The M1 test writer's index commit.
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
            let mut refs = w.open_table(REFS).map_err(db)?;
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
                        producers
                            .insert(&version.0[..], &change.0[..])
                            .map_err(db)?;
                    }
                    IndexEntry::Ref(name, value) => {
                        refs.insert(name.as_str(), &value[..]).map_err(db)?;
                    }
                    IndexEntry::ViewOp { view, op } => {
                        let mut key = view_ops_prefix(view);
                        key.extend_from_slice(&op.0);
                        view_ops.insert(&key[..], &[][..]).map_err(db)?;
                    }
                    IndexEntry::Landed { change, landing } => {
                        // the first landing that carried a change keeps it
                        if landed.get(&change.0[..]).map_err(db)?.is_none() {
                            landed.insert(&change.0[..], &landing.0[..]).map_err(db)?;
                        }
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

    pub fn producer(&self, version: &Hash) -> Result<Option<Hash>, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(PRODUCERS).map_err(db)?;
        match t.get(&version.0[..]).map_err(db)? {
            Some(v) => Ok(Some(hash_from(v.value(), "producer entry")?)),
            None => Ok(None),
        }
    }

    /// The ops of `view`'s chain, in ascending hash order.
    pub fn view_ops(&self, view: &str) -> Result<Vec<Hash>, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(VIEW_OPS).map_err(db)?;
        let prefix = view_ops_prefix(view);
        let mut out = vec![];
        for item in t.range(&prefix[..]..).map_err(db)? {
            let (k, _) = item.map_err(db)?;
            let key = k.value();
            if !key.starts_with(&prefix) {
                break;
            }
            // a longer name that starts with `view` and a 0x00 byte sorts among these keys; only
            // a key of exactly prefix + hash is one of `view`'s
            if key.len() == prefix.len() + 32 {
                out.push(hash_from(&key[prefix.len()..], "view op entry")?);
            }
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
