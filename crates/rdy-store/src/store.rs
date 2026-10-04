use crate::error::StoreError;
use crate::index::{Index, IndexEntry};
use crate::kinds::{self, BlobManifest, RefUpdate};
use crate::segment::{Location, SegmentLog};
use rdy_model::{Block, Change, Hash, Op};
use serde::{de::DeserializeOwned, Serialize};
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

const SEGMENT_MAX: u64 = 64 * 1024 * 1024;
const CHUNK: usize = 1024 * 1024;

pub struct Store {
    dir: PathBuf,
    log: SegmentLog,
    index: Index,
    _lock: File,
}

fn encode<T: Serialize>(v: &T) -> Vec<u8> {
    postcard::to_allocvec(v).expect("postcard encoding of an in-memory value cannot fail")
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, StoreError> {
    postcard::from_bytes(bytes).map_err(|e| StoreError::Decode(e.to_string()))
}

/// The single code path that turns a log frame into index entries, used both live and when rebuilding.
fn entries_for_frame(kind: u8, hash: Hash, loc: Location, payload: &[u8]) -> Result<Vec<IndexEntry>, StoreError> {
    let mut out = vec![IndexEntry::Object(hash, loc)];
    match kind {
        kinds::CHANGE => {
            let change: Change = decode(payload)?;
            for op in &change.ops {
                if let Op::Create { block } | Op::Update { block, .. } = op {
                    out.push(IndexEntry::Producer { version: block.version_hash(), change: hash });
                }
            }
        }
        kinds::REF_UPDATE => {
            let r: RefUpdate = decode(payload)?;
            out.push(IndexEntry::Ref(r.name, r.value));
        }
        _ => {}
    }
    Ok(out)
}

impl Store {
    pub fn open(dir: &Path) -> Result<Store, StoreError> {
        std::fs::create_dir_all(dir)?;
        let lock = OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("lock"))?;
        if lock.try_lock().is_err() {
            return Err(StoreError::Locked(dir.to_path_buf()));
        }
        let log = SegmentLog::open(&dir.join("segments"), SEGMENT_MAX)?;
        let index = Index::open(&dir.join("index.redb"))?;
        let mut store = Store { dir: dir.to_path_buf(), log, index, _lock: lock };
        if store.index.indexed_upto()? > store.log.end() {
            store.reindex()?;
        } else {
            store.catch_up()?;
        }
        Ok(store)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn catch_up(&mut self) -> Result<(), StoreError> {
        let from = self.index.indexed_upto()?;
        let mut entries = vec![];
        for (loc, kind, hash) in self.log.scan_from(from)? {
            let (_, payload) = self.log.read(loc)?;
            entries.extend(entries_for_frame(kind, hash, loc, &payload)?);
        }
        self.index.apply(&entries, self.log.end())
    }

    pub fn reindex(&mut self) -> Result<(), StoreError> {
        let path = self.dir.join("index.redb");
        let tmp = self.dir.join("index.redb.tmp");
        // drop the old database before deleting its file
        self.index = Index::open(&tmp)?;
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        self.index = Index::open(&path)?;
        let _ = std::fs::remove_file(&tmp);
        self.catch_up()
    }

    pub fn write(&mut self, items: Vec<(u8, Vec<u8>)>) -> Result<Vec<Hash>, StoreError> {
        let mut hashes = Vec::with_capacity(items.len());
        let mut entries = vec![];
        let mut seen = BTreeSet::new();
        for (kind, payload) in items {
            let hash = Hash::of(&payload);
            hashes.push(hash);
            if kind != kinds::REF_UPDATE && (seen.contains(&hash) || self.index.object(&hash)?.is_some()) {
                continue;
            }
            seen.insert(hash);
            let (loc, _) = self.log.append(kind, &payload)?;
            entries.extend(entries_for_frame(kind, hash, loc, &payload)?);
        }
        if !entries.is_empty() {
            self.log.sync()?;
            self.index.apply(&entries, self.log.end())?;
        }
        Ok(hashes)
    }

    pub fn put<T: Serialize>(&mut self, kind: u8, value: &T) -> Result<Hash, StoreError> {
        Ok(self.write(vec![(kind, encode(value))])?[0])
    }

    pub fn get_raw(&self, h: &Hash) -> Result<(u8, Vec<u8>), StoreError> {
        let loc = self.index.object(h)?.ok_or(StoreError::NotFound(*h))?;
        Ok(self.log.read(loc)?)
    }

    pub fn get<T: DeserializeOwned>(&self, h: &Hash) -> Result<T, StoreError> {
        decode(&self.get_raw(h)?.1)
    }

    pub fn has(&self, h: &Hash) -> Result<bool, StoreError> {
        Ok(self.index.object(h)?.is_some())
    }

    pub fn get_block(&self, version: &Hash) -> Result<Block, StoreError> {
        self.get(version)
    }

    fn ref_items(&self, refs: Vec<(String, Vec<u8>)>) -> Vec<(u8, Vec<u8>)> {
        let at = self.log.end();
        refs.into_iter()
            .enumerate()
            .map(|(i, (name, value))| {
                let at = Location { segment: at.segment, offset: at.offset + i as u64 };
                (kinds::REF_UPDATE, encode(&RefUpdate { name, value, at }))
            })
            .collect()
    }

    pub fn set_refs(&mut self, refs: Vec<(String, Vec<u8>)>) -> Result<(), StoreError> {
        let items = self.ref_items(refs);
        self.write(items)?;
        Ok(())
    }

    pub fn get_ref(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.index.get_ref(name)
    }

    pub fn refs_with_prefix(&self, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        self.index.refs_with_prefix(prefix)
    }

    pub fn put_blob(&mut self, bytes: &[u8]) -> Result<Hash, StoreError> {
        let mut items: Vec<(u8, Vec<u8>)> = bytes.chunks(CHUNK).map(|c| (kinds::BLOB_CHUNK, c.to_vec())).collect();
        let chunks: Vec<Hash> = items.iter().map(|(_, c)| Hash::of(c)).collect();
        items.push((kinds::BLOB_MANIFEST, encode(&BlobManifest { len: bytes.len() as u64, chunks })));
        Ok(*self.write(items)?.last().expect("manifest hash"))
    }

    pub fn get_blob(&self, h: &Hash) -> Result<Vec<u8>, StoreError> {
        let m: BlobManifest = self.get(h)?;
        let mut out = Vec::with_capacity(m.len as usize);
        for c in &m.chunks {
            out.extend(self.get_raw(c)?.1);
        }
        Ok(out)
    }

    pub fn producer(&self, version: &Hash) -> Result<Option<Hash>, StoreError> {
        self.index.producer(version)
    }

    pub fn commit_change(&mut self, mut change: Change, refs: Vec<(String, Vec<u8>)>) -> Result<Hash, StoreError> {
        let mut deps = BTreeSet::new();
        for op in &change.ops {
            if let Some(base) = op.base() {
                deps.insert(self.index.producer(&base)?.ok_or(StoreError::NotFound(base))?);
            }
        }
        change.deps = deps.into_iter().collect();
        let mut items = vec![];
        for op in &change.ops {
            if let Op::Create { block } | Op::Update { block, .. } = op {
                items.push((kinds::BLOCK, encode(block)));
            }
        }
        items.push((kinds::CHANGE, encode(&change)));
        let change_pos = items.len() - 1;
        items.extend(self.ref_items(refs));
        Ok(self.write(items)?[change_pos])
    }
}
