//! dl7: `rdy dump` and `rdy load`, the CLI twins of `rdy_dump` and `rdy_load` (D-KQ4CA, D-FR0C4
//! F3, F4 and F7). A dump is the whole project: there is no scope option. ec12: `rdy clone`, the
//! twin of `rdy_clone` (D-9QV85, D-D2NPN). `rdy dump --after` writes an increment and `rdy load
//! --then --stop-at` loads a chain to a stop (D-2FMRR).
use crate::conn::{Env, call, path_arg};
use crate::output::{num, text};
use crate::read_cmd::repo_lines;
use rdy_daemon::ErrorReply;
use rdy_engine::clone::{CloneSource, SourceKind, source_kind};
use rdy_engine::dump_error;
use rdy_engine::dump_stop::{DumpStop, MAX_CHAIN_FILES};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(clap::Args)]
#[command(group(clap::ArgGroup::new("dump_output")
    .args(["file", "out"]).required(true).multiple(false)))]
pub(crate) struct DumpArgs {
    /// The dump file to write; it must not exist.
    #[arg(value_name = "FILE")]
    file: Option<PathBuf>,
    /// The dump file to write; it must not exist.
    #[arg(long, value_name = "OUT")]
    out: Option<PathBuf>,
    /// An earlier dump or increment of this store: write only the batches after its end.
    #[arg(long, value_name = "FILE")]
    after: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct LoadArgs {
    /// The dump file.
    file: PathBuf,
    /// The directory to load into (default: --project, else the current directory). It must have no rdy store.
    dir: Option<PathBuf>,
    /// An increment of the dump, in chain order; repeat the flag for each one.
    #[arg(long, value_name = "FILE")]
    then: Vec<PathBuf>,
    /// Stop at the end of one batch of the chain: landing:<hash>, tag:<name> or pos:<segment>.<offset>.
    #[arg(long, value_name = "SELECTOR")]
    stop_at: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct CloneArgs {
    /// A project directory, a dump file, or an https hub repo URL.
    source: PathBuf,
    /// The directory to clone into (default: --project, else the current directory, as rdy load). It must have no rdy store.
    dir: Option<PathBuf>,
    /// Give the clone a new repo id, and record forked_from.
    #[arg(long)]
    fork: bool,
    /// With --fork: fork a project at this version (such as v1.2.0), with only the history up to that tag.
    #[arg(long, value_name = "TAG")]
    at: Option<String>,
}

/// `rdy dump`: through the daemon, as `rdy export` goes. With `--after` the daemon writes the
/// increment after that file; when nothing is newer, no file is made.
pub(crate) fn dump(env: &Env, a: DumpArgs) -> Result<(Value, String), ErrorReply> {
    let out = a
        .file
        .as_ref()
        .or(a.out.as_ref())
        .expect("clap requires one output");
    let mut args = json!({"out": path_arg(out)?});
    if let Some(after) = &a.after {
        args["after"] = json!(path_arg(after)?);
    }
    let v = call(env, "dump", args)?;
    let files = v["files"].as_array().map_or(0, Vec::len);
    let t = match (&a.after, v["out"].is_null()) {
        (Some(after), true) => format!(
            "nothing after {}: the store ends at {}.{}",
            after.display(),
            num(&v["end"]["segment"]),
            num(&v["end"]["offset"])
        ),
        (Some(after), false) => format!(
            "dumped {} batches and {} frames after {}, with {files} project files, to {} ({} bytes)",
            num(&v["batches"]),
            num(&v["frames"]),
            after.display(),
            text(&v["out"]),
            num(&v["bytes"])
        ),
        (None, _) => format!(
            "dumped {} batches and {} frames, with {files} project files, to {} ({} bytes)",
            num(&v["batches"]),
            num(&v["frames"]),
            text(&v["out"]),
            num(&v["bytes"])
        ),
    };
    Ok((v, t))
}

/// `rdy load`: in this process, as `rdy init` runs. The directory has no store, so there is no
/// daemon to ask. The files are `file`, then each `--then` in the order given; their count is
/// checked before any is opened, then the stop is parsed.
pub(crate) fn load(
    env: &Env,
    a: LoadArgs,
    project: Option<PathBuf>,
) -> Result<(Value, String), ErrorReply> {
    let LoadArgs {
        file,
        dir,
        then,
        stop_at,
    } = a;
    let dir = dir.or(project).unwrap_or_else(|| env.cwd.clone());
    if then.len() > MAX_CHAIN_FILES {
        return Err(dump_error::chain_too_long().into());
    }
    let stop = stop_at.as_deref().map(DumpStop::parse).transpose()?;
    let mut files = vec![file];
    files.extend(then);
    let s = rdy_engine::load::load_chain(&files, &dir, stop.as_ref())?;
    let stopped = s.stopped_at.as_ref().map_or_else(String::new, |at| {
        format!(
            ", stopped at {}.{}",
            at.position.segment, at.position.offset
        )
    });
    let mut t = format!(
        "loaded {} batches and {} frames into {}: {} landings, {} versions, {} views{stopped}; fsck ok\nnext: {}",
        s.batches, s.frames, s.project, s.landings, s.tags, s.views, s.next
    );
    for w in &s.warnings {
        t.push_str(&format!("\n{}: {}\nfix: {}", w.code, w.message, w.fix));
        for issue in &w.issues {
            t.push_str(&format!("\n  target: {}", issue.target));
        }
    }
    let v = serde_json::to_value(&s).expect("a reply serializes");
    Ok((v, t))
}

/// `rdy clone`: the source decides (O7, D-47HVF). A dump file clones in this process, as `rdy
/// load` runs. A project directory clones through its own daemon's op `clone`, or in process
/// with `--no-daemon`. An `https://` hub URL clones in this process with the user config's trust
/// and sign-in (D-D5J18); it never forks. The target is `dir`, else `--project`, else the
/// current directory (O38).
pub(crate) fn clone(
    env: &Env,
    a: CloneArgs,
    project: Option<PathBuf>,
) -> Result<(Value, String), ErrorReply> {
    let CloneArgs {
        source,
        dir,
        fork,
        at,
    } = a;
    // a fork at a tag (D-99CWC): the fork first, then the source form, then the tag and target
    if at.is_some() && !fork {
        return Err(rdy_engine::clone::at_needs_fork().into());
    }
    let dir = dir.or(project).unwrap_or_else(|| env.cwd.clone());
    let v = match source_kind(&source)? {
        SourceKind::Url => return clone_url(&source, &dir, fork),
        SourceKind::File if at.is_some() => {
            return Err(rdy_engine::clone::at_needs_project().into());
        }
        SourceKind::File => {
            let s = rdy_engine::clone::clone(CloneSource::File(&source), &dir, fork)?;
            serde_json::to_value(&s).expect("a reply serializes")
        }
        SourceKind::Project => {
            let project_env = Env {
                project: env.cwd.join(&source),
                cwd: env.cwd.clone(),
                no_daemon: env.no_daemon,
                place: None,
            };
            // the op resolves a relative dir against this session's working directory
            let mut args = json!({"dir": path_arg(&dir)?, "fork": fork});
            if let Some(tag) = &at {
                args["at"] = json!(tag);
            }
            call(&project_env, "clone", args)?
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

/// `rdy clone <url>`: the hub repo at `url`, cloned in this process as the configured person,
/// with `RDY_ACTOR` and `RDY_RUN` (D-D5J18, D-47HVF).
fn clone_url(
    source: &std::path::Path,
    dir: &std::path::Path,
    fork: bool,
) -> Result<(Value, String), ErrorReply> {
    if fork {
        return Err(rdy_engine::clone::url_fork().into());
    }
    let user_file = rdy_engine::hub_credentials::require_user_file(
        rdy_engine::config_levels::user_file_from_env(),
    )?;
    let person = rdy_mcp::tools::auth_person(None, &user_file)?;
    let (actor, run) = crate::conn::session_env()?;
    let session = rdy_engine::classify(
        &person,
        actor.as_deref(),
        run.as_deref(),
        rdy_model::Role::Reader,
    )?;
    use std::io::Write as _;
    let url = source.to_string_lossy();
    // O17: the CLI fetches the history in the foreground once the clone is ready; the ready
    // line and each step's progress go to stderr, flushed before the first history get, and
    // the reply stays one final object
    let mut ready = |s: &rdy_engine::clone::UrlCloneSummary| {
        if let Some(line) = s.ready_text() {
            let mut err = std::io::stderr().lock();
            let _ = writeln!(err, "{line}");
            let _ = err.flush();
        }
    };
    let mut step = |p: &rdy_engine::partial_history::HistoryProgress| {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err, "history: {} of {} records", p.held, p.total);
        let _ = err.flush();
    };
    let s = rdy_engine::clone::clone_url_with_steps(
        &session, &url, dir, &user_file, &mut ready, &mut step,
    )?;
    let mut t = format!(
        "cloned hub repo {} into {}: {} landings, {} versions, canon at {}; fsck ok\nremote {}: share, {}\nrepo id: {}\nsettings from the hub: {}\nsettings at their defaults: {}",
        s.url,
        s.project,
        s.landings,
        s.tags,
        s.canon,
        s.remote,
        s.url,
        s.repo_id,
        list(&s.settings.hub),
        list(&s.settings.defaults),
    );
    if let Some(h) = &s.history {
        t.push_str(&format!(
            "\nhistory: {} of {} records{}",
            h.held,
            h.total,
            if h.complete { ", complete" } else { "" }
        ));
    }
    for n in &s.notes {
        t.push_str(&format!("\nnote: {n}"));
    }
    for w in &s.warnings {
        t.push_str(&format!("\n{}: {}\nfix: {}", w.code, w.message, w.fix));
    }
    t.push_str(&format!("\nnext: {}", s.next));
    let v = serde_json::to_value(&s).expect("a reply serializes");
    Ok((v, t))
}

/// `names` joined with commas, or `none`.
fn list(names: &[String]) -> String {
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use clap::{CommandFactory, Parser};

    #[derive(Parser)]
    struct Wrap {
        #[command(flatten)]
        a: DumpArgs,
    }

    fn parse(args: &[&str]) -> Result<Wrap, clap::Error> {
        Wrap::try_parse_from(std::iter::once("dump").chain(args.iter().copied()))
    }

    fn kind(args: &[&str]) -> ErrorKind {
        match parse(args) {
            Ok(_) => panic!("{args:?} parsed"),
            Err(e) => e.kind(),
        }
    }

    #[test]
    fn sdc4_dump_requires_exactly_one_output() {
        assert_eq!(kind(&[]), ErrorKind::MissingRequiredArgument);
        assert_eq!(
            kind(&["a.rdydump", "--out", "a.rdydump"]),
            ErrorKind::ArgumentConflict
        );
        assert_eq!(
            kind(&["a.rdydump", "--out", "b.rdydump"]),
            ErrorKind::ArgumentConflict
        );
        assert_eq!(
            kind(&["a.rdydump", "b.rdydump"]),
            ErrorKind::UnknownArgument
        );
        // either single form parses
        assert!(parse(&["a.rdydump"]).is_ok());
        assert!(parse(&["--out", "a.rdydump"]).is_ok());
    }

    #[test]
    fn sdc4_dump_accepts_a_hyphen_path_after_separator() {
        let w = parse(&["--", "-copy.rdydump"]).unwrap();
        assert_eq!(w.a.file, Some(PathBuf::from("-copy.rdydump")));
        assert_eq!(w.a.out, None);
    }

    #[test]
    fn sdc4_dump_help_names_both_output_forms() {
        let help = Wrap::command().render_long_help().to_string();
        let one: String = help.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(one.contains("[FILE]"), "{help}");
        assert!(one.contains("--out <OUT>"), "{help}");
        // clap drops the final full stop of a doc comment, as it did for the old `--out`
        assert_eq!(
            one.matches("The dump file to write; it must not exist")
                .count(),
            2,
            "{help}"
        );
    }
}
