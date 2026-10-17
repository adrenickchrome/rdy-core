//! dl7: `rdy dump` and `rdy load`, the CLI twins of `rdy_dump` and `rdy_load` (D-KQ4CA, D-FR0C4
//! F3, F4 and F7). A dump is the whole project: there is no scope option. ec12: `rdy clone`, the
//! twin of `rdy_clone` (D-9QV85, D-D2NPN).
use crate::conn::{Env, call, path_arg};
use crate::output::{num, text};
use crate::read_cmd::repo_lines;
use rdy_daemon::ErrorReply;
use rdy_engine::clone::{CloneSource, SourceKind, source_kind};
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

#[derive(clap::Args)]
pub(crate) struct CloneArgs {
    /// A project directory or a dump file.
    source: PathBuf,
    /// The directory to clone into (default: --project, else the current directory, as rdy load). It must have no rdy store.
    dir: Option<PathBuf>,
    /// Give the clone a new repo id, and record forked_from.
    #[arg(long)]
    fork: bool,
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

/// `rdy clone`: the source decides (O7). A dump file clones in this process, as `rdy load`
/// runs. A project directory clones through its own daemon's op `clone`, or in process with
/// `--no-daemon`. The target is `dir`, else `--project`, else the current directory (O38).
pub(crate) fn clone(
    env: &Env,
    a: CloneArgs,
    project: Option<PathBuf>,
) -> Result<(Value, String), ErrorReply> {
    let CloneArgs { source, dir, fork } = a;
    let dir = dir.or(project).unwrap_or_else(|| env.cwd.clone());
    let v = match source_kind(&source)? {
        SourceKind::File => {
            let s = rdy_engine::clone::clone(CloneSource::File(&source), &dir, fork)?;
            serde_json::to_value(&s).expect("a reply serializes")
        }
        SourceKind::Project => {
            let at = Env {
                project: env.cwd.join(&source),
                cwd: env.cwd.clone(),
                no_daemon: env.no_daemon,
                place: None,
            };
            // the op resolves a relative dir against this session's working directory
            call(&at, "clone", json!({"dir": path_arg(&dir)?, "fork": fork}))?
        }
    };
    let t = format!(
        "cloned {} batches and {} frames into {}: {} landings, {} versions, {} views; fsck ok\n{}next: {}",
        num(&v["batches"]),
        num(&v["frames"]),
        text(&v["project"]),
        num(&v["landings"]),
        num(&v["tags"]),
        num(&v["views"]),
        repo_lines(&v),
        text(&v["next"])
    );
    Ok((v, t))
}
