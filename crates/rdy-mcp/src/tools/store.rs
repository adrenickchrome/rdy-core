//! dl7: `rdy_dump` and `rdy_load` (D-FR0C4 F4, F6 and F7). Any session may call them. Relative
//! paths resolve against the project directory.
use super::*;
use rdy_engine::RdyError;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DumpParams {
    /// The dump file to write: absolute, or relative to the project directory. It must not exist.
    pub out: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoadParams {
    /// The dump file: absolute, or relative to the project directory.
    pub file: String,
    /// The directory to load into: absolute, or relative to the project directory. It must have no rdy store.
    pub dir: String,
}

/// `p` as given when absolute, else under the project directory `root` (`join` replaces the
/// base with an absolute path).
fn under(root: &Path, p: &str) -> std::path::PathBuf {
    root.join(p)
}

/// `rdy_dump`: writes the whole project to `out`. A session is required, as for `rdy_site`.
pub fn rdy_dump(st: &mut McpState, p: DumpParams) -> Result<Value, RdyError> {
    session(st)?;
    let summary = st.engine.read().dump(&under(&st.engine.root(), &p.out))?;
    Ok(serde_json::to_value(summary).expect("a reply serializes"))
}

/// `rdy_load`: rebuilds a project from `file` in `dir`, a directory with no store. It runs in
/// the daemon's process and opens and closes the new project itself.
pub fn rdy_load(st: &mut McpState, p: LoadParams) -> Result<Value, RdyError> {
    session(st)?;
    let root = st.engine.root();
    let summary = rdy_engine::load::load(&under(&root, &p.file), &under(&root, &p.dir))?;
    Ok(serde_json::to_value(summary).expect("a reply serializes"))
}
