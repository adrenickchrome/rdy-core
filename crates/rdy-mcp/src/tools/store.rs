//! dl7: `rdy_dump` and `rdy_load` (D-FR0C4 F4, F6 and F7); ec12: `rdy_clone` (D-9QV85, D-D2NPN;
//! O6); chained dumps and stopped loads (D-2FMRR, D-QF2HQ). Any session may call them. Relative paths resolve against the project directory.
use super::*;
use rdy_engine::clone::{CloneSource, SourceKind, clone, source_kind};
use rdy_engine::dump_error;
use rdy_engine::dump_stop::{DumpStop, MAX_CHAIN_FILES};
use rdy_engine::stream_check::check_batch;
use rdy_engine::{E_BAD_INPUT, E_IO, RdyError};
use rdy_model::RepoId;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DumpParams {
    /// The dump file to write: absolute, or relative to the project directory. It must not exist.
    pub out: String,
    /// An earlier dump or increment of this project: absolute, or relative to the project directory. Give it to write only the batches after its end (an increment). When nothing is newer, no file is written and out is null.
    pub after: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoadParams {
    /// The dump file: absolute, or relative to the project directory.
    pub file: String,
    /// The directory to load into: absolute, or relative to the project directory. It must have no rdy store.
    pub dir: String,
    /// Increments of the dump in file, in chain order: each absolute, or relative to the project directory. Leave it out, or pass [], to load file alone.
    pub then: Option<Vec<String>>,
    /// Stop the load at the end of one batch of the chain: landing:<hash or prefix>, tag:<name> or pos:<segment>.<offset>. Leave it out to load the whole chain.
    pub stop_at: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloneParams {
    /// The directory to clone into: absolute, or relative to the project directory. It must have no rdy store.
    pub dir: String,
    /// A dump file to clone, absolute or relative to the project directory. Leave it out to clone this project.
    pub file: Option<String>,
    /// true: the clone gets a new repo id and records forked_from.
    pub fork: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StreamCheckParams {
    /// The batch file: absolute, or relative to the project directory. A name of - is an ordinary file name.
    pub file: String,
    /// Refuse a batch of another repo: 32 hex digits.
    pub repo_id: Option<String>,
}

/// `p` as given when absolute, else under the project directory `root` (`join` replaces the
/// base with an absolute path).
fn under(root: &Path, p: &str) -> std::path::PathBuf {
    root.join(p)
}

/// `rdy_dump`: writes the whole project to `out`, or with `after` the batches since that dump's
/// end. A session is required, as for `rdy_site`.
pub fn rdy_dump(st: &mut McpState, p: DumpParams) -> Result<Value, RdyError> {
    session(st)?;
    let root = st.engine.root();
    let out = under(&root, &p.out);
    let state = st.engine.read();
    let summary = match &p.after {
        Some(after) => state.dump_after_file(&under(&root, after), &out)?,
        None => state.dump(&out)?,
    };
    Ok(serde_json::to_value(summary).expect("a reply serializes"))
}

/// `rdy_load`: rebuilds a project from `file` and its `then` increments in `dir`, a directory
/// with no store, up to `stop_at` when given. The increment count is checked before any path is
/// opened, then the stop is parsed. It runs in
/// the daemon's process and opens and closes the new project itself.
pub fn rdy_load(st: &mut McpState, p: LoadParams) -> Result<Value, RdyError> {
    session(st)?;
    let root = st.engine.root();
    let then = p.then.unwrap_or_default();
    if then.len() > MAX_CHAIN_FILES {
        return Err(dump_error::chain_too_long());
    }
    let stop = p.stop_at.as_deref().map(DumpStop::parse).transpose()?;
    let mut files = vec![under(&root, &p.file)];
    files.extend(then.iter().map(|f| under(&root, f)));
    let summary = rdy_engine::load::load_chain(&files, &under(&root, &p.dir), stop.as_ref())?;
    Ok(serde_json::to_value(summary).expect("a reply serializes"))
}

/// `rdy_clone`: clones this project, or the dump `file`, into `dir`, a directory with no store.
/// It runs in this process (the daemon's, or the server's with `--no-daemon`) and opens and
/// closes the new project itself. A session is required, as for `rdy_dump`.
pub fn rdy_clone(st: &mut McpState, p: CloneParams) -> Result<Value, RdyError> {
    session(st)?;
    let root = st.engine.root();
    let dir = under(&root, &p.dir);
    let fork = p.fork.unwrap_or(false);
    let summary = match p.file {
        None => clone(CloneSource::State(&st.engine.read()), &dir, fork)?,
        Some(file) => {
            let file = dump_file(&root, &file)?;
            clone(CloneSource::File(&file), &dir, fork)?
        }
    };
    Ok(serde_json::to_value(summary).expect("a reply serializes"))
}

/// The dump file `rdy_clone {file}` names (O39): a regular file. A directory is refused, since
/// this tool clones only this project or a dump; any other path follows the clone sources (O7).
/// A URL is checked as given, so its text names it without the project directory.
fn dump_file(root: &Path, given: &str) -> Result<std::path::PathBuf, RdyError> {
    let path = if given.contains("://") {
        std::path::PathBuf::from(given)
    } else {
        under(root, given)
    };
    if path.is_dir() {
        return Err(not_a_dump(&path));
    }
    match source_kind(&path)? {
        SourceKind::File => Ok(path),
        // `source_kind` names only a directory a project, and a directory is refused above
        SourceKind::Project => Err(not_a_dump(&path)),
    }
}

fn not_a_dump(path: &Path) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("{} is not a dump file", path.display()),
        "to clone another project, call rdy_dump on its own server, then pass the file here",
    )
}

/// `rdy_stream_check` (ha15; D-D2NPN, D-KQ4CA): checks one upload batch file without opening a
/// store, through the same pure check as `rdy stream check`. A session is required, as for
/// `rdy_dump`. The report comes only after the whole batch passes.
pub fn rdy_stream_check(st: &mut McpState, p: StreamCheckParams) -> Result<Value, RdyError> {
    session(st)?;
    let repo = p
        .repo_id
        .as_deref()
        .map(|text| {
            text.parse::<RepoId>().map_err(|e| {
                RdyError::new(
                    E_BAD_INPUT,
                    e.to_string(),
                    "pass a repo id with 32 hex digits",
                )
            })
        })
        .transpose()?;
    let path = under(&st.engine.root(), &p.file);
    let file = std::fs::File::open(&path).map_err(|e| {
        RdyError::new(
            E_IO,
            format!("{}: {e}", path.display()),
            "check the path and its permissions",
        )
    })?;
    let report = check_batch(std::io::BufReader::new(file), repo)?;
    Ok(serde_json::to_value(report).expect("a report serializes"))
}
