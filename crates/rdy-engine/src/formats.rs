//! Format facts (D-69AY3, D-WWAK7): the formats this rdy writes and reads, and the formats of one
//! dump file. Neither call opens a store, a config or a daemon.
use crate::error::RdyError;
use crate::load::{open_dump, open_stream};
use rdy_store::dump::{DUMP_FORMAT, READS};
use rdy_store::format::STORE_FORMAT;
use serde::Serialize;
use std::io::BufReader;
use std::path::Path;

/// The version of the sync stream format this rdy writes and reads.
const STREAM_FORMAT: u32 = 1;

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
    /// The store format this rdy writes.
    pub store: u32,
    /// The dump format this rdy writes.
    pub dump: u32,
    /// The pairs a dump may hold that this rdy reads, in release order.
    pub reads: Vec<DumpPair>,
    /// The sync stream format.
    pub stream: u32,
}

/// The formats of one dump file, from its prelude and header.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DumpFormatFacts {
    pub dump: u32,
    pub store: u32,
    /// The version of the rdy that wrote the dump.
    pub rdy: String,
}

/// The formats this rdy writes and reads.
pub fn formats() -> FormatFacts {
    FormatFacts {
        rdy: env!("CARGO_PKG_VERSION").to_string(),
        store: STORE_FORMAT,
        dump: DUMP_FORMAT,
        reads: READS
            .iter()
            .map(|&(dump, store)| DumpPair { dump, store })
            .collect(),
        stream: STREAM_FORMAT,
    }
}

/// The formats of the dump `file`. This reads the prelude and the header only: it is format
/// inspection, not a check that the whole dump is valid. File and header errors are the load's.
pub fn inspect_dump(file: &Path) -> Result<DumpFormatFacts, RdyError> {
    let f = open_dump(file)?;
    let opened = open_stream(file, BufReader::new(f))?;
    Ok(DumpFormatFacts {
        dump: opened.dump_format,
        store: opened.header.store_format,
        rdy: opened.header.rdy,
    })
}
