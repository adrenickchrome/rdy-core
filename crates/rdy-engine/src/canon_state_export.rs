//! The export of one canon tip's state as format-5 batches (D-GGTYA, D-4PV25): the read side of
//! a fast first pull. [`PreparedCanonState`] selects the state once
//! (`rdy_store::canon_state::select_state`): the root landing, its map read through the store's
//! one map read (never a walk of the history closure), the SNAPSHOT made from that map, the
//! blocks it names, the tip's delta and the tip's attachments. Its known pages and every get
//! read that selection, as a prepared pull's read its closure (`crate::stream_export`).
//!
//! It reads up to the captured state's log end and checks the redaction epoch before and after
//! each call: a removal applied since refuses the next call (`E_REDACTED_STATE`). The caller owns
//! it for one operation; nothing is cached in the process or on disk, and the SNAPSHOT made for
//! the stream is never stored.
use crate::error::{E_INTERNAL, RdyError};
use crate::history_facts::{MAX_KEYS, key_limit};
use crate::read_state::ReadState;
use crate::stream_error::to_rdy;
use crate::stream_export::{
    EncodedHistory, FRAME_HEADER, Features, KeyPage, batch_bytes, encode_batch,
};
use rdy_model::{Hash, RepoId};
use rdy_store::canon_state::{StateSelection, select_state};
use rdy_store::closure::{dependency_order, write_batch};
use rdy_store::format::StoreFeatures;
use rdy_store::object_refs::ObjectKey;
use rdy_store::stream_error::{LIMIT_PREREQUISITES, LIMIT_TOTAL_BYTES, StreamError};
use rdy_store::sync_stream::{
    BatchReport, MAX_PREREQUISITES, MAX_STREAM_BYTES, RecordFeatures, StreamHeader,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::io::Write;
use std::sync::Arc;

/// One caller-owned read of a canon tip's state (see the module docs): the captured state, its
/// root, log end and redaction epoch, the selected keys in dependency order with their links,
/// lengths and needed features, and the one canonical SNAPSHOT payload.
pub struct PreparedCanonState {
    read: Arc<ReadState>,
    repo_id: RepoId,
    epoch: u64,
    selection: StateSelection,
    index: HashMap<ObjectKey, usize>,
}

impl fmt::Debug for PreparedCanonState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedCanonState")
            .field("root", &self.selection.root)
            .field("end", &self.read.seq())
            .field("epoch", &self.epoch)
            .field("records", &self.selection.records.len())
            .finish_non_exhaustive()
    }
}

impl PreparedCanonState {
    /// Selects the state of the root landing `root` that `read` holds, once. A root `read` does
    /// not hold is `E_UNRESOLVED_REF`; a map that does not read, or a SNAPSHOT that is not the
    /// canonical encoding of the root's map, keeps the snapshot read's refusal; a selection past
    /// the bounds of one batch, or a SNAPSHOT over the record bound, is `E_LIMIT`.
    pub fn new(read: Arc<ReadState>, root: Hash) -> Result<PreparedCanonState, RdyError> {
        read.ensure_current_epoch()?;
        let reader = read.reader();
        let Some(repo_id) = reader.repo_id()? else {
            return Err(RdyError::new(
                E_INTERNAL,
                "the store has no repo id",
                "report this as a bug in rdy",
            ));
        };
        let epoch = reader.captured_redaction_epoch();
        let selection = select_state(reader, root, read.seq()).map_err(to_rdy)?;
        let payload: u64 = selection.nodes.values().map(|n| n.len).sum();
        let bytes = batch_bytes(
            selection.features.to_json().len(),
            selection.prerequisites.len(),
            selection.records.len() as u64,
            payload,
        );
        if bytes > MAX_STREAM_BYTES {
            return Err(to_rdy(StreamError::limit(None, 0, LIMIT_TOTAL_BYTES)));
        }
        let grow = |_| to_rdy(StreamError::allocation(None, 0));
        let mut index = HashMap::new();
        index.try_reserve(selection.records.len()).map_err(grow)?;
        for (i, k) in selection.records.iter().enumerate() {
            index.insert(*k, i);
        }
        read.ensure_current_epoch()?;
        Ok(PreparedCanonState {
            read,
            repo_id,
            epoch,
            selection,
            index,
        })
    }

    /// The root landing.
    pub fn root(&self) -> Hash {
        self.selection.root
    }

    /// Every selected key, in the order a whole batch writes them.
    pub fn keys(&self) -> &[ObjectKey] {
        &self.selection.records
    }

    /// The features every batch of this state lists: the records' needs and `partial_history`.
    pub fn features(&self) -> &StoreFeatures {
        &self.selection.features
    }

    /// The frontier edges the whole state omits, sorted.
    pub fn prerequisites(&self) -> &[ObjectKey] {
        &self.selection.prerequisites
    }

    /// The SNAPSHOT made from the root's map: its hash, which is the root's `state`.
    pub fn snapshot(&self) -> Hash {
        self.selection.snapshot
    }

    /// The exact raw bytes of the state's object frames (each frame's header and payload, the
    /// SNAPSHOT made for the stream included): what the gets of the whole state send as object
    /// frames, however they split it.
    pub fn frame_bytes(&self) -> u64 {
        self.selection
            .nodes
            .values()
            .map(|n| FRAME_HEADER + n.len)
            .sum()
    }

    /// A page of at most `limit` keys (1 to [`MAX_KEYS`]) after `after` (from the start when
    /// None), in the order a whole batch writes them; `next` is the page's last key when more
    /// follow. An `after` outside the state is `E_UNRESOLVED_REF`.
    pub fn page(&self, after: Option<ObjectKey>, limit: usize) -> Result<KeyPage, RdyError> {
        if limit == 0 || limit > MAX_KEYS {
            return Err(key_limit());
        }
        self.read.ensure_current_epoch()?;
        let start = match after {
            None => 0,
            Some(k) => match self.index.get(&k) {
                Some(i) => i + 1,
                None => return Err(to_rdy(StreamError::unresolved(0, 0, k))),
            },
        };
        let rest = &self.selection.records[start..];
        let keys: Vec<ObjectKey> = rest.iter().take(limit).copied().collect();
        let next = if rest.len() > keys.len() {
            keys.last().copied()
        } else {
            None
        };
        self.read.ensure_current_epoch()?;
        Ok(KeyPage { keys, next })
    }

    /// Writes exactly the records that `keys` name as one complete format-5 batch: in the
    /// dependency order of that set alone, with the root (when the set leaves it out) and every
    /// link or proof a record needs and the set leaves out as prerequisites, and the features of
    /// the whole state. At most [`MAX_KEYS`] distinct keys count; a repeated key counts once. A key
    /// outside the state is `E_UNRESOLVED_REF` and nothing is written.
    pub fn write_keys(&self, keys: &[ObjectKey], out: impl Write) -> Result<BatchReport, RdyError> {
        let wanted: BTreeSet<ObjectKey> = keys.iter().copied().collect();
        if wanted.len() > MAX_KEYS {
            return Err(key_limit());
        }
        self.read.ensure_current_epoch()?;
        if let Some(k) = wanted.iter().find(|k| !self.index.contains_key(k)) {
            return Err(to_rdy(StreamError::unresolved(0, 0, *k)));
        }
        let (records, prerequisites, bytes) = self.subset(&wanted)?;
        let header = StreamHeader::canon_state(
            self.selection.features.clone(),
            self.repo_id,
            self.selection.root,
            prerequisites,
        );
        let reader = self.read.reader();
        let mut source = Features {
            inner: self.selection.source(reader, self.read.seq()),
            seen: RecordFeatures::default(),
        };
        let (_, totals) = write_batch(&mut source, &header, &records, out).map_err(to_rdy)?;
        let record_features = source.seen.finish();
        self.read.ensure_current_epoch()?;
        debug_assert_eq!(
            bytes,
            totals.bytes + FRAME_HEADER + rdy_store::sync_stream::TRAILER_LEN as u64,
            "the size check measures the batch"
        );
        Ok(BatchReport {
            header,
            keys: records,
            record_features,
            records: totals.records,
            bytes: totals.bytes,
            digest: totals.digest,
        })
    }

    /// [`PreparedCanonState::write_keys`] for a get reply, as a prepared pull's
    /// (`crate::stream_export::PreparedPull::write_keys_encoded`): one zstd frame when
    /// `accept_zstd` and the frame is smaller, else the batch's own bytes.
    pub fn write_keys_encoded(
        &self,
        keys: &[ObjectKey],
        accept_zstd: bool,
    ) -> Result<EncodedHistory, RdyError> {
        encode_batch(|out| self.write_keys(keys, out), accept_zstd)
    }

    /// The records of `wanted` in their own dependency order, the batch's prerequisites, and
    /// its size, checked against the stream bound.
    fn subset(
        &self,
        wanted: &BTreeSet<ObjectKey>,
    ) -> Result<(Vec<ObjectKey>, Vec<ObjectKey>, u64), RdyError> {
        let mut children: BTreeMap<ObjectKey, Vec<ObjectKey>> = BTreeMap::new();
        let mut payload = 0u64;
        let mut prerequisites: BTreeSet<ObjectKey> = BTreeSet::new();
        let limit = || to_rdy(StreamError::limit(None, 0, LIMIT_PREREQUISITES));
        for k in wanted {
            let node = &self.selection.nodes[k];
            payload += node.len;
            children.insert(*k, node.children.clone());
        }
        for k in wanted {
            let node = &self.selection.nodes[k];
            for c in node.children.iter().chain(&node.proof) {
                if !wanted.contains(c) {
                    prerequisites.insert(*c);
                    if prerequisites.len() > MAX_PREREQUISITES {
                        return Err(limit());
                    }
                }
            }
        }
        let root = self.selection.root_key();
        if !wanted.contains(&root) {
            prerequisites.insert(root);
        }
        if prerequisites.len() > MAX_PREREQUISITES {
            return Err(limit());
        }
        let records = dependency_order(root, &children).map_err(to_rdy)?;
        let bytes = batch_bytes(
            self.selection.features.to_json().len(),
            prerequisites.len(),
            wanted.len() as u64,
            payload,
        );
        if bytes > MAX_STREAM_BYTES {
            return Err(to_rdy(StreamError::limit(None, 0, LIMIT_TOTAL_BYTES)));
        }
        Ok((records, prerequisites.into_iter().collect(), bytes))
    }
}
