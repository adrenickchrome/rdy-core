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
    faults: Faults,
    /// Set when a failed write could not be rolled back; every later write fails until reopen.
    poisoned: bool,
}

/// Opens the index, verifies its integrity and reads its acknowledged boundary (None if it has
/// none yet). An index that fails to open, fails the check, or makes redb panic is renamed to
/// `index.redb.damaged-<n>` and replaced by an empty one, which the caller rebuilds from the
/// log; the boundary is then unknown.
fn open_index(dir: &Path) -> Result<(Index, Option<Location>), StoreError> {
    let path = dir.join("index.redb");
    // redb can panic on a corrupt page instead of returning an error; that is damage too
    let opened = std::panic::catch_unwind(|| {
        Index::open_checked(&path).and_then(|i| {
            let acked = i.acknowledged()?;
            Ok((i, acked))
        })
    })
    .unwrap_or_else(|_| Err(StoreError::Db("index panicked while opening".into())));
    match opened {
        Ok(ok) => Ok(ok),
        Err(_) => {
            set_aside(&path)?;
            Ok((Index::open(&path)?, None))
        }
    }
}

/// Moves `path` to the first free `<path>.damaged-<n>`, never overwriting an existing file.
fn set_aside(path: &Path) -> Result<(), StoreError> {
    for n in 0u32.. {
        let mut name = path.file_name().expect("index file name").to_os_string();
        name.push(format!(".damaged-{n}"));
        match std::fs::hard_link(path, path.with_file_name(name)) {
            Ok(()) => {
                std::fs::remove_file(path)?;
                File::open(path.parent().expect("store dir"))?.sync_all()?;
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    unreachable!("u32 suffixes exhausted")
}

/// Test-only fault injection (see `fail_next_index_apply`).
#[derive(Default)]
struct Faults {
    index_apply: bool,
    rollback: bool,
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
        // the index opens first: its indexed_upto is the acknowledged end of the log, which
        // tells the log which torn frames are an unacknowledged tail
        let (index, acked) = open_index(dir)?;
        let log = SegmentLog::open_acked(&dir.join("segments"), SEGMENT_MAX, acked)?;
        let mut store = Store { dir: dir.to_path_buf(), log, index, _lock: lock, faults: Faults::default(), poisoned: false };
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

    /// Reads and hash-checks every frame in every segment; returns the frame count.
    pub fn verify(&self) -> Result<usize, StoreError> {
        let frames = self.log.scan_from(Location { segment: 0, offset: 0 })?;
        for (loc, _, _) in &frames {
            self.log.read(*loc)?;
        }
        Ok(frames.len())
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
        if self.poisoned {
            return Err(StoreError::Io(std::io::Error::other("an earlier failed write could not be rolled back; reopen the store")));
        }
        let start = self.log.end();
        let result = self.append_and_index(items);
        if result.is_err() && self.log.end() != start {
            // nothing past `start` was acknowledged: discard it so a later write or reindex
            // cannot resurrect it
            let rollback = if std::mem::take(&mut self.faults.rollback) { Err(std::io::Error::other("injected rollback failure")) } else { self.log.truncate_to(start) };
            if rollback.is_err() {
                self.poisoned = true;
            }
        }
        result
    }

    fn append_and_index(&mut self, items: Vec<(u8, Vec<u8>)>) -> Result<Vec<Hash>, StoreError> {
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
            if std::mem::take(&mut self.faults.index_apply) {
                return Err(StoreError::Db("injected index failure".into()));
            }
            self.index.apply(&entries, self.log.end())?;
        }
        Ok(hashes)
    }

    /// Test hook: the next write's index update fails after its frames are appended.
    #[doc(hidden)]
    pub fn fail_next_index_apply(&mut self) {
        self.faults.index_apply = true;
    }

    /// Test hook: the next rollback of a failed write fails, poisoning the store.
    #[doc(hidden)]
    pub fn fail_next_rollback(&mut self) {
        self.faults.rollback = true;
    }

    pub fn put<T: Serialize>(&mut self, kind: u8, value: &T) -> Result<Hash, StoreError> {
        Ok(self.write(vec![(kind, encode(value))])?[0])
    }

    pub fn get_raw(&self, h: &Hash) -> Result<(u8, Vec<u8>), StoreError> {
        let loc = self.index.object(h)?.ok_or(StoreError::NotFound(*h))?;
        let (kind, found, payload) = self.log.read_hashed(loc)?;
        if found != *h {
            return Err(StoreError::Decode(format!("the index places {h} at {loc:?}, which holds {found}; run rdy reindex")));
        }
        Ok((kind, payload))
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

    /// The hash `put_blob` would return for these bytes, without storing anything.
    pub fn blob_hash(bytes: &[u8]) -> Hash {
        let chunks: Vec<Hash> = bytes.chunks(CHUNK).map(Hash::of).collect();
        Hash::of(&encode(&BlobManifest { len: bytes.len() as u64, chunks }))
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
        change.deps = self.change_deps(&change)?;
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

impl Store {
    pub(crate) fn change_deps(&self, change: &Change) -> Result<Vec<Hash>, StoreError> {
        let mut deps = BTreeSet::new();
        for op in &change.ops {
            if let Some(base) = op.base() {
                deps.insert(self.index.producer(&base)?.ok_or(StoreError::NotFound(base))?);
            }
        }
        Ok(deps.into_iter().collect())
    }

    pub(crate) fn ref_items_pub(&self, refs: Vec<(String, Vec<u8>)>) -> Vec<(u8, Vec<u8>)> {
        self.ref_items(refs)
    }
}
