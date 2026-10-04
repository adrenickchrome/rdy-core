use crate::error::{StoreError, db};
use crate::segment::Location;
use rdy_model::Hash;
use redb::{Database, ReadableDatabase, TableDefinition};
use std::path::Path;

const OBJECTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("objects");
const PRODUCERS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("producers");
const REFS: TableDefinition<&str, &[u8]> = TableDefinition::new("refs");
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

pub enum IndexEntry {
    Object(Hash, Location),
    Producer { version: Hash, change: Hash },
    Ref(String, Vec<u8>),
}

pub struct Index {
    db: Database,
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

    /// Opens the index and verifies every page checksum (about 30 ms per 300k objects). A file
    /// that fails the check, or that redb had to repair, is an error: the caller sets it aside
    /// and rebuilds it from the log rather than trusting it.
    pub fn open_checked(path: &Path) -> Result<Index, StoreError> {
        let mut database = Database::create(path).map_err(db)?;
        if !database.check_integrity().map_err(db)? {
            return Err(StoreError::Db("index failed its integrity check".into()));
        }
        Index::with_tables(database)
    }

    fn with_tables(database: Database) -> Result<Index, StoreError> {
        let w = database.begin_write().map_err(db)?;
        {
            w.open_table(OBJECTS).map_err(db)?;
            w.open_table(PRODUCERS).map_err(db)?;
            w.open_table(REFS).map_err(db)?;
            w.open_table(META).map_err(db)?;
        }
        w.commit().map_err(db)?;
        Ok(Index { db: database })
    }

    pub fn apply(&self, entries: &[IndexEntry], upto: Location) -> Result<(), StoreError> {
        let w = self.db.begin_write().map_err(db)?;
        {
            let mut objects = w.open_table(OBJECTS).map_err(db)?;
            let mut producers = w.open_table(PRODUCERS).map_err(db)?;
            let mut refs = w.open_table(REFS).map_err(db)?;
            let mut meta = w.open_table(META).map_err(db)?;
            for e in entries {
                match e {
                    IndexEntry::Object(h, loc) => {
                        objects.insert(&h.0[..], &enc(loc)[..]).map_err(db)?;
                    }
                    IndexEntry::Producer { version, change } => {
                        producers
                            .insert(&version.0[..], &change.0[..])
                            .map_err(db)?;
                    }
                    IndexEntry::Ref(name, value) => {
                        refs.insert(name.as_str(), &value[..]).map_err(db)?;
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

    pub fn producer(&self, version: &Hash) -> Result<Option<Hash>, StoreError> {
        let r = self.db.begin_read().map_err(db)?;
        let t = r.open_table(PRODUCERS).map_err(db)?;
        match t.get(&version.0[..]).map_err(db)? {
            Some(v) => Ok(Some(hash_from(v.value(), "producer entry")?)),
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
