//! The errors of dump chains and load stops. Each constructor takes paths, ids and numbers,
//! never a free-form message, so a text lives in one place.
use crate::dump_stop::{MAX_CHAIN_FILES, MAX_STOP_BYTES};
use crate::error::{E_BAD_INPUT, E_DUMP_CHAIN, E_LIMIT, E_LOAD_STOP, RdyError};
use rdy_model::{Hash, RepoId};
use std::path::Path;

const LOAD_CHAIN_FIX: &str = "pass the full dump, then each increment in chain order";
const EXPORT_CHAIN_FIX: &str =
    "follow the newest file of this store's chain, or start a new chain with rdy dump";
const STOP_FIX: &str = "name a landing, a tag or a batch end that the dump chain holds";

/// The name an export error gives the previous dump: its path, or `previous dump` where the
/// caller (the adapter) has none.
fn named(file: Option<&Path>) -> String {
    file.map_or_else(|| "previous dump".to_string(), |f| f.display().to_string())
}

fn load(message: String) -> RdyError {
    RdyError::new(E_DUMP_CHAIN, message, LOAD_CHAIN_FIX)
}

fn export(message: String) -> RdyError {
    RdyError::new(E_DUMP_CHAIN, message, EXPORT_CHAIN_FIX)
}

fn stop(message: String) -> RdyError {
    RdyError::new(E_LOAD_STOP, message, STOP_FIX)
}

/// A stop selector that is not spelled `landing:<hash>`, `tag:<name>` or `pos:<segment>.<offset>`.
pub fn stop_syntax(value: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("Invalid load stop {value:?}."),
        "Use landing:<hash>, tag:<name>, or pos:<segment>.<offset>.",
    )
}

/// A stop selector over [`MAX_STOP_BYTES`].
pub fn stop_limit() -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("The load stop exceeds the limit of {MAX_STOP_BYTES} bytes."),
        "Use a shorter landing, tag, or position selector.",
    )
}

/// More than [`MAX_CHAIN_FILES`] increments after the full dump.
pub fn chain_too_long() -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("The dump chain exceeds the limit of {MAX_CHAIN_FILES} increments."),
        "Load from a newer full dump of the store.",
    )
}

/// The first file of a chain is an increment.
pub fn first_is_increment(file: &Path) -> RdyError {
    load(format!(
        "{}: it is an increment; a dump chain starts with a full dump",
        file.display()
    ))
}

/// A later file of a chain is a full dump.
pub fn later_is_full(file: &Path) -> RdyError {
    load(format!(
        "{}: it is a full dump; only the first file of a dump chain is one",
        file.display()
    ))
}

/// A file of dump format `format`, which holds no chain facts, is in a chain.
pub fn load_legacy_file(file: &Path, format: u32) -> RdyError {
    load(format!(
        "{}: dump format {format} holds no chain facts; a dump chain starts with a full dump of format 3",
        file.display()
    ))
}

/// A file holds another repo than the chain.
pub fn load_repo_mismatch(file: &Path, file_repo: &RepoId, chain_repo: &RepoId) -> RdyError {
    load(format!(
        "{} holds repo {file_repo}, but the chain holds repo {chain_repo}",
        file.display()
    ))
}

/// A file is of another store (a scalar format, or feature manifest) than the chain.
pub fn load_store_mismatch(
    file: &Path,
    file_store: &rdy_store::format::DumpStore,
    chain_store: &rdy_store::format::DumpStore,
) -> RdyError {
    load(format!(
        "{} is of {file_store}, but the chain is of {chain_store}",
        file.display()
    ))
}

/// A file starts at `start`, but the file before it ends at `prev_end` (segment, offset).
pub fn load_position_mismatch(
    file: &Path,
    start_segment: u32,
    start_offset: u64,
    prev_segment: u32,
    prev_offset: u64,
) -> RdyError {
    load(format!(
        "{} starts at {start_segment}.{start_offset}, but the file before it ends at {prev_segment}.{prev_offset}",
        file.display()
    ))
}

/// A file follows a batch other than the last batch of the file before it.
pub fn load_marker_mismatch(file: &Path) -> RdyError {
    load(format!(
        "{} follows a batch that differs from the last batch of the file before it",
        file.display()
    ))
}

/// A file names `named` as the digest of its predecessor, which has digest `actual`.
pub fn load_digest_mismatch(file: &Path, named: &Hash, actual: &Hash) -> RdyError {
    load(format!(
        "{} follows the file with digest {named}, but the file before it has digest {actual}",
        file.display()
    ))
}

/// The previous dump holds another repo than this project.
pub fn export_repo_mismatch(file: Option<&Path>, file_repo: &RepoId, repo: &RepoId) -> RdyError {
    export(format!(
        "{} holds repo {file_repo}, but this project is repo {repo}",
        named(file)
    ))
}

/// The previous dump is of another store (a scalar format, or feature manifest) than this
/// store.
pub fn export_store_mismatch(
    file: Option<&Path>,
    file_store: &rdy_store::format::DumpStore,
    store: &rdy_store::format::DumpStore,
) -> RdyError {
    export(format!(
        "{} is of {file_store}, but this store is of {store}",
        named(file)
    ))
}

/// The previous dump ends after the end of this store.
pub fn export_end_past_state(file: Option<&Path>, segment: u32, offset: u64) -> RdyError {
    export(format!(
        "{} ends at {segment}.{offset}, after the end of this store",
        named(file)
    ))
}

/// The previous dump ends inside a batch of this store.
pub fn export_end_inside_batch(file: Option<&Path>, segment: u32, offset: u64) -> RdyError {
    export(format!(
        "{} ends at {segment}.{offset}, which is not the end of a batch in this store",
        named(file)
    ))
}

/// The batch of this store that ends at the previous dump's end differs from that dump's last
/// batch. The position is the previous dump's end.
pub fn export_marker_mismatch(file: Option<&Path>, segment: u32, offset: u64) -> RdyError {
    export(format!(
        "the batch of this store at {segment}.{offset} differs from the last batch of {}",
        named(file)
    ))
}

/// The previous dump is of a format that holds no chain facts.
pub fn export_legacy_predecessor(file: Option<&Path>, format: u32) -> RdyError {
    export(format!(
        "{}: dump format {format} holds no chain facts; dump the project again",
        named(file)
    ))
}

/// No landing in the chain matches `spelling` (lowercase hex, a full hash or a prefix).
pub fn stop_no_landing(spelling: &str) -> RdyError {
    stop(format!("the dump chain holds no landing {spelling}"))
}

/// No tag in the chain has this name.
pub fn stop_no_tag(name: &str) -> RdyError {
    stop(format!("the dump chain holds no tag {name}"))
}

/// A landing prefix that matches more than one landing in the chain.
pub fn stop_ambiguous_landing(prefix: &str) -> RdyError {
    stop(format!(
        "{prefix} names more than one landing in the dump chain"
    ))
}

/// A position that is not the end of a batch in the chain.
pub fn stop_not_batch_end(segment: u32, offset: u64) -> RdyError {
    stop(format!(
        "{segment}.{offset} is not the end of a batch in the dump chain"
    ))
}

/// A file of dump format `format` cannot carry a stop.
pub fn stop_legacy_file(file: &Path, format: u32) -> RdyError {
    stop(format!(
        "{}: dump format {format} holds no chain facts; a stop needs dump format 3",
        file.display()
    ))
}
