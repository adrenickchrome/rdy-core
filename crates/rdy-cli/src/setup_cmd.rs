//! `rdy setup` (ain5, D-8X04F): only the stub. It writes three lines into `CLAUDE.md` and
//! `AGENTS.md` of the chosen project root, or of the working directory when there is no
//! project. The stub goes first, before the person's own text, which stays byte for byte; a file
//! that already holds the stub is left alone. A target that is not a regular file is refused
//! before either file is written, and a link is never followed. No harness detection, MCP
//! registration, skill, hook, download or rule import is part of it.
use crate::conn::require_project;
use crate::instructions_cmd::discover;
use rdy_daemon::ErrorReply;
use rdy_engine::{E_IO, RdyError};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The stub, with its final newline.
pub(crate) const STUB: &str = "This project keeps its agent instructions in rdy.\nAt the start of every task, call rdy_instructions and load the rulesets your task needs.\nRequired rules arrive without asking.\n";

/// The files that take the stub, in the order they are written.
pub(crate) const TARGETS: [&str; 2] = ["CLAUDE.md", "AGENTS.md"];

/// The fix of a target that is not a regular file.
const NOT_REGULAR_FIX: &str = "replace it with a regular file, then run rdy setup again";

/// The directory `rdy setup` writes in: the explicit project, else the nearest project's root
/// (a worktree's own directory), else `cwd`. A project that is found must be a project; it is
/// never the meta project merely because meta supplies the rules.
pub(crate) fn root(explicit: Option<&Path>, cwd: &Path) -> Result<PathBuf, ErrorReply> {
    if let Some(p) = explicit {
        require_project(p)?;
        return Ok(p.to_path_buf());
    }
    match discover(cwd) {
        Some(f) => {
            require_project(&f.project)?;
            Ok(f.root)
        }
        None => Ok(cwd.to_path_buf()),
    }
}

/// What one target needs.
enum Plan {
    /// No file: it is created with the stub.
    Create,
    /// A regular file without the stub: the stub goes before its bytes, and the file keeps its
    /// permissions.
    Prepend(Vec<u8>, std::fs::Permissions),
    /// A regular file that holds the stub: left alone.
    Keep,
}

fn io(path: &Path, e: std::io::Error) -> RdyError {
    RdyError::new(
        E_IO,
        format!("{}: {e}", path.display()),
        "check the file and its directory, then run rdy setup again",
    )
}

/// What `path` needs, read without following a link.
fn preflight(path: &Path) -> Result<Plan, RdyError> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Plan::Create),
        Err(e) => return Err(io(path, e)),
    };
    if !meta.file_type().is_file() {
        return Err(RdyError::new(
            E_IO,
            format!("{} is not a regular instruction file", path.display()),
            NOT_REGULAR_FIX,
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| io(path, e))?;
    let has = bytes.windows(STUB.len()).any(|w| w == STUB.as_bytes());
    Ok(if has {
        Plan::Keep
    } else {
        Plan::Prepend(bytes, meta.permissions())
    })
}

/// `rdy setup` in `root`: both targets are checked first, then each that needs it is written
/// durably. A failure after a write names what was written; a repeat completes the rest once.
pub(crate) fn setup(root: &Path) -> Result<(Value, String), ErrorReply> {
    let paths: Vec<PathBuf> = TARGETS.iter().map(|t| root.join(t)).collect();
    let mut plans = Vec::with_capacity(paths.len());
    for p in &paths {
        plans.push(preflight(p)?);
    }
    let mut written: Vec<&Path> = vec![];
    let mut files = vec![];
    let mut text = vec![];
    for (path, plan) in paths.iter().zip(plans) {
        let (bytes, perms) = match plan {
            Plan::Keep => {
                files.push(json!({"path": path.to_string_lossy(), "result": "kept"}));
                text.push(format!("{} already has the stub", path.display()));
                continue;
            }
            Plan::Create => (STUB.as_bytes().to_vec(), None),
            Plan::Prepend(old, perms) => ([STUB.as_bytes(), &old].concat(), Some(perms)),
        };
        let wrote = rdy_engine::write_durable(path, &bytes)
            .and_then(|()| perms.map_or(Ok(()), |p| std::fs::set_permissions(path, p)));
        if let Err(e) = wrote {
            let done: Vec<String> = written
                .iter()
                .map(|p| format!("; {} was written", p.display()))
                .collect();
            return Err(RdyError::new(
                E_IO,
                format!("{}: {e}{}", path.display(), done.concat()),
                "fix the cause, then run rdy setup again; it leaves a file that has the stub alone",
            )
            .into());
        }
        written.push(path);
        files.push(json!({"path": path.to_string_lossy(), "result": "written"}));
        text.push(format!("wrote the stub to {}", path.display()));
    }
    Ok((json!({"files": files}), text.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ain5_the_stub_is_three_lines() {
        assert_eq!(STUB.lines().count(), 3);
        assert!(STUB.ends_with('\n'));
        assert_eq!(
            STUB.lines().collect::<Vec<_>>(),
            [
                "This project keeps its agent instructions in rdy.",
                "At the start of every task, call rdy_instructions and load the rulesets your task needs.",
                "Required rules arrive without asking.",
            ]
        );
    }
}
