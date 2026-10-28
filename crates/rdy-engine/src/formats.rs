//! Format facts (D-69AY3, D-WWAK7): the formats this rdy writes and reads, and the formats of one
//! dump file. Neither call opens a store, a config or a daemon.
use crate::dump::ids;
use crate::error::RdyError;
use crate::load::{open_dump, read_error};
use rdy_model::{Hash, RepoId};
use rdy_store::dump::{DUMP_FORMAT, DumpReader, READS, READS_FEATURES};
use rdy_store::dump_chain::{BatchMarker, DumpKind};
use rdy_store::format::{DumpStore, StoreFeatures, baseline_features};
use rdy_store::segment::Location;
use rdy_store::transition::hex;
use serde::Serialize;
use std::path::Path;

/// The version of the sync stream format this rdy writes and reads.
const STREAM_FORMAT: u32 = rdy_store::sync_stream::STREAM_FORMAT;

/// One `(dump format, store format)` pair a dump may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct DumpPair {
    pub dump: u32,
    pub store: u32,
}

/// What this rdy writes and reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FormatFacts {
    /// This rdy's version.
    pub rdy: String,
    /// The features of a store this rdy creates.
    pub store: StoreFeatures,
    /// The dump format this rdy writes.
    pub dump: u32,
    /// The released pairs of scalar stores a dump may hold that this rdy reads, in release
    /// order.
    pub reads: Vec<DumpPair>,
    /// The manifest grammars of a feature store's dump this rdy reads.
    pub reads_features: Vec<u32>,
    /// The sync stream format.
    pub stream: u32,
}

/// The formats of one dump file, and the chain facts its header states, from its prelude and
/// header only. None of them says that the file is complete or valid. A header of dump format 1
/// or 2 states no chain facts: each is null, except `end`, which is its log end. In JSON every
/// hash is 64 lowercase hex digits and a repo id 32.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DumpFormatFacts {
    pub dump: u32,
    /// The dump's store: `{"legacy": <n>}` for a dump of a scalar store, `{"features": {...}}`
    /// for dump format 4.
    pub store: DumpStore,
    /// The version of the rdy that wrote the dump.
    pub rdy: String,
    /// Full or increment.
    pub kind: Option<DumpKind>,
    #[serde(serialize_with = "ids::opt_repo")]
    pub repo_id: Option<RepoId>,
    /// Where the file starts in the source log, and the marker of the batch that ends there.
    pub base: Option<Location>,
    pub base_batch: Option<BatchMarker>,
    /// The trailer digest of the file before it (zero for a full dump).
    #[serde(serialize_with = "hex::opt::serialize")]
    pub prev: Option<Hash>,
    /// Where the file ends in the source log, and the marker of its last batch.
    pub end: Option<Location>,
    pub end_batch: Option<BatchMarker>,
}

/// The formats this rdy writes and reads.
pub fn formats() -> FormatFacts {
    FormatFacts {
        rdy: env!("CARGO_PKG_VERSION").to_string(),
        store: baseline_features(),
        dump: DUMP_FORMAT,
        reads: READS
            .iter()
            .map(|&(dump, store)| DumpPair { dump, store })
            .collect(),
        reads_features: READS_FEATURES.to_vec(),
        stream: STREAM_FORMAT,
    }
}

/// The formats of the dump `file`. This reads the prelude and the header only: it is format
/// inspection, not a check that the whole dump is valid, and it reads no trailer. A full dump and
/// an increment alike are inspected. File and header errors are the load's.
pub fn inspect_dump(file: &Path) -> Result<DumpFormatFacts, RdyError> {
    let f = open_dump(file)?;
    // unbuffered: the reader takes the prelude and the header record, and nothing after them
    let (_, dump, header) = DumpReader::open(f).map_err(|e| read_error(file, e))?;
    let chain = header.chain;
    Ok(DumpFormatFacts {
        dump,
        store: header.store,
        rdy: header.rdy,
        kind: chain.map(|c| c.kind),
        repo_id: chain.map(|c| c.repo_id),
        base: chain.map(|c| c.base),
        base_batch: chain.map(|c| c.base_batch),
        prev: chain.map(|c| c.prev),
        end: Some(header.log_end),
        end_batch: chain.map(|c| c.end_batch),
    })
}
