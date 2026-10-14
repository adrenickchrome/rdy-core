//! dl7: `rdy dump` and `rdy load`, the CLI twins of `rdy_dump` and `rdy_load` (D-KQ4CA, D-FR0C4
//! F3, F4 and F7). A dump is the whole project: there is no scope option.
use crate::conn::{Env, call, path_arg};
use crate::output::{num, text};
use rdy_daemon::ErrorReply;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(clap::Args)]
pub(crate) struct DumpArgs {
    /// The dump file to write; it must not exist.
    #[arg(long)]
    out: PathBuf,
}

#[derive(clap::Args)]
pub(crate) struct LoadArgs {
    /// The dump file.
    file: PathBuf,
    /// The directory to load into (default: --project, else the current directory). It must have no rdy store.
    dir: Option<PathBuf>,
}

/// `rdy dump`: through the daemon, as `rdy export` goes.
pub(crate) fn dump(env: &Env, a: DumpArgs) -> Result<(Value, String), ErrorReply> {
    let v = call(env, "dump", json!({"out": path_arg(&a.out)?}))?;
    let files = v["files"].as_array().map_or(0, Vec::len);
    let t = format!(
        "dumped {} batches and {} frames, with {files} project files, to {} ({} bytes)",
        num(&v["batches"]),
        num(&v["frames"]),
        text(&v["out"]),
        num(&v["bytes"])
    );
    Ok((v, t))
}

/// `rdy load`: in this process, as `rdy init` runs. The directory has no store, so there is no
/// daemon to ask.
pub(crate) fn load(
    env: &Env,
    a: LoadArgs,
    project: Option<PathBuf>,
) -> Result<(Value, String), ErrorReply> {
    let LoadArgs { file, dir } = a;
    let dir = dir.or(project).unwrap_or_else(|| env.cwd.clone());
    let s = rdy_engine::load::load(&file, &dir)?;
    let t = format!(
        "loaded {} batches and {} frames into {}: {} landings, {} versions, {} views; fsck ok\nnext: {}",
        s.batches, s.frames, s.project, s.landings, s.tags, s.views, s.next
    );
    let v = serde_json::to_value(&s).expect("a reply serializes");
    Ok((v, t))
}
